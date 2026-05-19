//! Lower a single program unit (program / module / subprogram) to IR.
//!
//! Extracted from `core.rs` in Sprint 11 Stage A. Pure mechanical
//! move — behavior unchanged. The function still consults all of
//! core's lowering helpers, which were widened to `pub(super)`
//! visibility in the same commit.

use std::collections::{HashMap, HashSet};

use crate::ast::decl::TypeSpec;
use crate::ast::unit::*;
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;
use crate::sema::symtab::SymbolTable;

use super::const_scalar::ConstScalar;
use super::core::*;
use super::ctx::{
    AmbiguousUseWarnings, CharKind, HiddenResultAbi, LocalInfo, LowerCtx, ProcScopeGuard,
    SmpExtraHostGuard,
};
use super::helpers::clamp_nonnegative_i64;

pub(crate) fn lower_unit(
    module: &mut Module,
    unit: &SpannedUnit,
    st: &SymbolTable,
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
    // Audit CRITICAL-4: USE imports from the host program unit
    // (and its hosts, transitively). Per F2018 §16.2, names
    // imported into a host are visible in its contained
    // subprograms via host association. Each lower_unit call
    // accumulates its own uses on top of host_uses and passes
    // the combined list down to any nested subprogram. The
    // top-level call from lower_file passes an empty slice.
    host_uses: &[crate::ast::decl::SpannedDecl],
    host_param_consts: &HashMap<String, ConstScalar>,
    // `host_decls`: decls of the immediate enclosing program unit.
    // Used by contained subprograms to resolve element type, dims,
    // and character-kind for each host-associated variable the
    // closure-passing ABI threads in as a hidden pointer param.
    host_decls: &[crate::ast::decl::SpannedDecl],
    host_link_name: Option<&str>,
    host_module: Option<&str>,
    alloc_return_funcs: &HashSet<String>,
    optional_params: &HashMap<String, Vec<bool>>,
    descriptor_params: &HashMap<String, Vec<bool>>,
    internal_funcs: &HashMap<String, u32>,
    elemental_funcs: &HashSet<String>,
    char_len_star_params: &HashMap<String, Vec<bool>>,
    // `contained_host_refs`: per-callee ordered list of host-local
    // names it reads or writes. Drives both callee signature
    // (hidden trailing pointer params) and call-site arg list.
    contained_host_refs: &HashMap<String, Vec<String>>,
    ambiguous_use_warnings: &AmbiguousUseWarnings,
    internal_only: bool,
    // Sema scope id of the immediate host program unit (when this unit
    // is a contained procedure). Used to disambiguate same-name +
    // same-signature contained procedures across hosts.
    host_scope_id: Option<crate::sema::symtab::ScopeId>,
) {
    match &unit.node {
        ProgramUnit::Program {
            name,
            decls,
            body,
            contains,
            uses,
            ..
        } => {
            let fname = name.clone().unwrap_or_else(|| "main".into());
            let visible_param_consts =
                collect_decl_param_consts_with_scope(decls, host_param_consts, st);
            let body_fname = format!("__prog_{}", fname);
            let mut func = Function::new(body_fname.clone(), vec![], IrType::Void);
            let mut ctx = LowerCtx::new(
                st,
                globals,
                type_layouts,
                alloc_return_funcs,
                optional_params,
                descriptor_params,
                internal_funcs,
                elemental_funcs,
                char_len_star_params,
                contained_host_refs,
                ambiguous_use_warnings.clone(),
            );
            ctx.proc_scope_id = {
                let raw_name = name.as_deref();
                st.all_scopes().iter().enumerate().find_map(|(idx, scope)| {
                    match (&scope.kind, raw_name) {
                        (crate::sema::symtab::ScopeKind::Program(scope_name), Some(n)) => {
                            scope_name.eq_ignore_ascii_case(n).then_some(idx)
                        }
                        (crate::sema::symtab::ScopeKind::Program(_), None) => Some(idx),
                        _ => None,
                    }
                })
            };
            let mut pending_globals: Vec<PendingGlobal> = Vec::new();

            let combined_uses: Vec<crate::ast::decl::SpannedDecl> =
                host_uses.iter().chain(uses.iter()).cloned().collect();
            let required_import_names = collect_required_import_names(decls, body);

            {
                let mut b = FuncBuilder::new(&mut func);
                let _setup_proc_scope_guard = ProcScopeGuard::enter(ctx.proc_scope_id);
                install_common_locals(&mut b, &mut ctx.locals, decls);
                install_equivalence_locals(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    st,
                );
                super::alloc::alloc_decls(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    type_layouts,
                    &mut pending_globals,
                    &fname,
                    st,
                );
                install_host_param_consts(&mut b, &mut ctx.locals, host_param_consts, st);
                install_globals_as_locals(
                    &mut b,
                    &mut ctx.locals,
                    globals,
                    &combined_uses,
                    Some(&required_import_names),
                    host_module,
                    ctx.st,
                    &ctx.ambiguous_use_warnings,
                );
                ctx.filtered_names = compute_filtered_names(globals, &combined_uses, decls);
                check_no_filtered_refs(body, &ctx.filtered_names);
                collect_implicit_locals(&mut b, &mut ctx, body, UnitScope::Program(&fname));
                super::init::init_decls(&mut b, &ctx.locals, decls, st, Some(type_layouts));
                collect_label_blocks(&mut b, body, &mut ctx.label_blocks);
                let _proc_scope_guard = ProcScopeGuard::enter(ctx.proc_scope_id);
                super::stmt::lower_stmts(&mut b, &mut ctx, body);
                drop(_proc_scope_guard);
                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(
                        &mut b,
                        &ctx.locals,
                        &ctx.locals,
                        type_layouts,
                        ctx.st,
                        ctx.internal_funcs,
                        Some(ctx.contained_host_refs),
                        None,
                    );
                }
                ensure_termination(&mut b, None);
            }

            module.add_function(func);
            for pg in pending_globals {
                module.add_global(pg.global);
            }

            // Lower CONTAINS subprograms. Their host_decls chain is
            // this unit's decls PLUS whatever we inherited from our
            // own host (via `host_decls`). That way a nested contained
            // proc can resolve names from any ancestor scope when
            // build_host_ref_params looks up types.
            let mut child_host_decls: Vec<crate::ast::decl::SpannedDecl> = decls.to_vec();
            child_host_decls.extend(host_decls.iter().cloned());
            for sub in contains {
                lower_unit(
                    module,
                    sub,
                    st,
                    globals,
                    type_layouts,
                    &combined_uses,
                    &visible_param_consts,
                    &child_host_decls,
                    Some(body_fname.as_str()),
                    host_module,
                    alloc_return_funcs,
                    optional_params,
                    descriptor_params,
                    internal_funcs,
                    elemental_funcs,
                    char_len_star_params,
                    contained_host_refs,
                    ambiguous_use_warnings,
                    true,
                    ctx.proc_scope_id,
                );
            }
        }
        ProgramUnit::Subroutine {
            name,
            decls,
            body,
            args,
            bind,
            uses,
            contains,
            prefix,
            ..
        } => {
            // Sprint35-SMP Phase 2: separate-module-procedure body form.
            // Parser emits args=[] and prefix=[Module]; sema injected the
            // inherited dummies into the body scope. Two cases:
            //
            //   1. Parent interface declares a Function — build a
            //      synthetic ProgramUnit::Function and recurse into
            //      lower_unit so the Function arm handles result-var
            //      allocation, sret ABI, etc.
            //   2. Parent declares a Subroutine — synthesize args+decls
            //      and continue down this Subroutine arm, which then
            //      walks them like a normal procedure.
            if let Some(body_scope_id) = smp_body_proc_scope(st, name, args, prefix) {
                if let Some(synth_unit) = try_synth_smp_function_unit(
                    st,
                    body_scope_id,
                    name,
                    bind,
                    prefix,
                    uses,
                    decls,
                    body,
                    contains,
                    unit.span,
                ) {
                    let synth_spanned = crate::ast::Spanned::new(synth_unit, unit.span);
                    lower_unit(
                        module,
                        &synth_spanned,
                        st,
                        globals,
                        type_layouts,
                        host_uses,
                        host_param_consts,
                        host_decls,
                        host_link_name,
                        host_module,
                        alloc_return_funcs,
                        optional_params,
                        descriptor_params,
                        internal_funcs,
                        elemental_funcs,
                        char_len_star_params,
                        contained_host_refs,
                        ambiguous_use_warnings,
                        internal_only,
                        host_scope_id,
                    );
                    return;
                }
            }
            let smp_synth = smp_body_proc_scope(st, name, args, prefix)
                .map(|sid| synthesize_smp_body_args_decls(st, sid, unit.span, decls));
            let args: &[crate::ast::unit::DummyArg] = match &smp_synth {
                Some((sa, _)) => sa.as_slice(),
                None => args.as_slice(),
            };
            let decls: &[crate::ast::decl::SpannedDecl] = match &smp_synth {
                Some((_, sd)) => sd.as_slice(),
                None => decls.as_slice(),
            };
            let func_name = lowered_procedure_symbol_name(
                name,
                bind.as_ref(),
                host_link_name,
                host_module,
                internal_only,
                internal_funcs,
            );
            let proc_scope_id =
                procedure_scope_for_dummy_args_with_host(st, name, args, host_scope_id);
            let visible_param_consts =
                collect_decl_param_consts_with_scope(decls, host_param_consts, st);
            let mut params: Vec<Param> = args
                .iter()
                .enumerate()
                .filter_map(|(i, arg)| {
                    if let DummyArg::Name(n) = arg {
                        let elem_ty = arg_type_from_decls(n, decls, Some(st));
                        let fortran_noalias = arg_is_fortran_noalias(n, decls);
                        let uses_descriptor =
                            arg_uses_descriptor_for_lowering(n, decls, st, proc_scope_id);
                        let uses_string_descriptor =
                            arg_uses_string_descriptor_from_decls(n, decls);
                        let is_derived = arg_derived_type_name(n, decls).is_some();
                        if arg_has_value_attr(n, decls) {
                            // VALUE: pass by value (raw type, not pointer).
                            Some(Param {
                                name: n.clone(),
                                ty: elem_ty,
                                id: ValueId(i as u32),
                                fortran_noalias: false,
                            })
                        } else {
                            Some(Param {
                                name: n.clone(),
                                ty: by_ref_storage_ir_type(
                                    &elem_ty,
                                    uses_descriptor,
                                    uses_string_descriptor,
                                    is_derived,
                                ),
                                id: ValueId(i as u32),
                                fortran_noalias,
                            })
                        }
                    } else {
                        None
                    }
                })
                .collect();
            // Append hidden-length i64 params for character(len=*) dummies.
            // Per the standard Fortran ABI, these trail the normal params.
            //
            // Compute flags from this procedure's own decls — a bare-name
            // lookup against `char_len_star_params` collides when several
            // contained procedures across different hosts share an arg
            // name (stdlib_sorting_sort has nine `helper`/`introsort`/etc
            // contained sets, only the character variant declares
            // `character(len=*)` actuals, but the map key is just
            // "introsort"; without this the integer variants of
            // `insertion_sort` would receive the char variant's flags and
            // their bodies would lower as character arrays, calling
            // `afs_compare_char` on integer data and silently failing to
            // sort).
            let mut hidden_len_params: Vec<(String, ValueId)> = Vec::new();
            let own_cls_flags = compute_char_len_star_flags(args, decls);
            if own_cls_flags.iter().any(|f| *f) {
                let normal_count = params.len();
                for (i, (flag, arg)) in own_cls_flags.iter().zip(args.iter()).enumerate() {
                    if *flag {
                        if let DummyArg::Name(n) = arg {
                            let hid_id = ValueId((normal_count + hidden_len_params.len()) as u32);
                            params.push(Param {
                                name: format!("__len_{}", n.to_lowercase()),
                                ty: IrType::Int(IntWidth::I64),
                                id: hid_id,
                                fortran_noalias: false,
                            });
                            hidden_len_params.push((n.to_lowercase(), hid_id));
                        }
                    }
                    let _ = i;
                }
            }

            // Host-association closure params. Trailing pointer params,
            // one per host-local variable this contained proc reads or
            // writes. Order matches contained_host_refs[name].
            let host_ref_infos = build_host_ref_params(
                name,
                host_decls,
                host_param_consts,
                contained_host_refs,
                params.len() as u32,
                st,
                &mut params,
            );

            let mut func = Function::new(func_name.clone(), params, IrType::Void);
            use crate::ast::unit::Prefix;
            func.is_pure = prefix.iter().any(|p| matches!(p, Prefix::Pure));
            func.is_elemental = prefix.iter().any(|p| matches!(p, Prefix::Elemental));
            func.internal_only = internal_only;
            let mut ctx = LowerCtx::new(
                st,
                globals,
                type_layouts,
                alloc_return_funcs,
                optional_params,
                descriptor_params,
                internal_funcs,
                elemental_funcs,
                char_len_star_params,
                contained_host_refs,
                ambiguous_use_warnings.clone(),
            );
            ctx.proc_scope_id = proc_scope_id;
            let mut pending_globals: Vec<PendingGlobal> = Vec::new();
            let combined_uses: Vec<crate::ast::decl::SpannedDecl> =
                host_uses.iter().chain(uses.iter()).cloned().collect();
            let required_import_names = collect_required_import_names(decls, body);

            // Collect param info: (name, param_id, elem_type, is_value).
            // Skip hidden params: __len_* (character-length) and __host_*
            // (host-association closure pointers) — they are installed
            // by separate paths below.
            let param_info: Vec<(String, ValueId, IrType, bool)> = func
                .params
                .iter()
                .filter(|p| !p.name.starts_with("__len_") && !p.name.starts_with("__host_"))
                .map(|p| {
                    let pname = p.name.to_lowercase();
                    let elem_ty = arg_type_from_decls(&pname, decls, Some(st));
                    let is_value = arg_has_value_attr(&pname, decls);
                    (pname, p.id, elem_ty, is_value)
                })
                .collect();

            {
                let mut b = FuncBuilder::new(&mut func);
                let _setup_proc_scope_guard = ProcScopeGuard::enter(ctx.proc_scope_id);

                // Set up hidden-length locals for assumed-len char dummies.
                let mut hidden_len_addrs: HashMap<String, ValueId> = HashMap::new();
                for (hname, hid) in &hidden_len_params {
                    let slot = b.alloca(IrType::Int(IntWidth::I64));
                    b.store(*hid, slot);
                    hidden_len_addrs.insert(hname.clone(), slot);
                }

                for (pname, pid, elem_ty, is_value) in &param_info {
                    if *is_value {
                        let slot = b.alloca(elem_ty.clone());
                        b.store(*pid, slot);
                        ctx.insert_scalar(pname.clone(), slot, elem_ty.clone());
                    } else {
                        let uses_descriptor =
                            arg_uses_descriptor_for_lowering(pname, decls, st, proc_scope_id);
                        let uses_string_descriptor =
                            arg_uses_string_descriptor_from_decls(pname, decls);
                        let dt_name = arg_derived_type_name(pname, decls);
                        let is_pointer = decl_is_pointer(pname, decls);
                        let local_elem_ty = dummy_local_ir_type(
                            elem_ty,
                            dt_name.as_deref(),
                            is_pointer,
                            type_layouts,
                        );
                        let slot = b.alloca(by_ref_storage_ir_type(
                            elem_ty,
                            uses_descriptor,
                            uses_string_descriptor,
                            dt_name.is_some(),
                        ));
                        b.store(*pid, slot);
                        // Check if this is a derived type parameter.
                        let ck = if let Some(&len_slot) = hidden_len_addrs.get(pname) {
                            CharKind::AssumedLen { len_addr: len_slot }
                        } else {
                            arg_char_kind_from_decls(pname, decls, st)
                        };
                        let info = LocalInfo {
                            addr: slot,
                            ty: local_elem_ty,
                            dims: arg_dims_from_decls(pname, decls, &visible_param_consts, st),
                            allocatable: false,
                            descriptor_arg: uses_descriptor,
                            by_ref: true,
                            char_kind: ck,
                            derived_type: dt_name,
                            inline_const: None,
                            is_pointer,
                            runtime_dim_upper: vec![],
                            is_class: decl_is_class(pname, decls),
                            logical_kind: arg_logical_kind_from_decls(
                                pname,
                                decls,
                                Some(&visible_param_consts),
                                st,
                            ),
                            last_dim_assumed_size: arg_last_dim_assumed_size_from_decls(
                                pname, decls,
                            ),
                        };
                        ctx.locals.insert(pname.clone(), info);
                        if decl_is_optional(pname, decls) {
                            ctx.optional_locals.insert(pname.clone());
                        }
                    }
                }

                for (pname, _, _, is_value) in &param_info {
                    if *is_value || hidden_len_addrs.contains_key(pname) {
                        continue;
                    }
                    let Some(len_expr) = arg_runtime_char_len_expr_from_decls(pname, decls, st)
                    else {
                        continue;
                    };
                    let len_raw = super::expr::lower_expr_with_optional_layouts(
                        &mut b,
                        &ctx.locals,
                        &len_expr,
                        ctx.st,
                        Some(type_layouts),
                    );
                    let len_addr = b.alloca(IrType::Int(IntWidth::I64));
                    let len_val = clamp_nonnegative_i64(&mut b, len_raw);
                    b.store(len_val, len_addr);
                    if let Some(info) = ctx.locals.get_mut(pname) {
                        info.char_kind = CharKind::FixedRuntime { len_addr };
                    }
                }
                // Explicit-shape dummies whose upper bound is itself
                // a (non-const) dummy argument — e.g. `xs(n)` — need
                // the bound evaluated at runtime on function entry.
                // arg_dims_from_decls falls back to (1, 1) when the
                // bound isn't const-foldable, which would produce
                // spurious bounds-check failures. Walk every by_ref
                // dummy, lower its bound expressions now (all other
                // dummies are already in ctx.locals), and stash the
                // i64 result into runtime_dim_upper.
                install_runtime_dim_bounds(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    ctx.st,
                    type_layouts,
                );
                install_assumed_shape_lower_overrides(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    ctx.st,
                    type_layouts,
                );
                install_explicit_shape_dummy_rebase(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    ctx.st,
                    type_layouts,
                );
                clear_intent_out_allocatable_array_params(&mut b, &param_info, &ctx.locals, decls);
                clear_intent_out_derived_params(
                    &mut b,
                    &param_info,
                    &ctx.locals,
                    decls,
                    type_layouts,
                );

                install_common_locals(&mut b, &mut ctx.locals, decls);
                install_equivalence_locals(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    st,
                );
                // Install host-association by_ref locals before alloc_decls
                // so any same-named callee local (shouldn't occur per F
                // scoping rules) is short-circuited, and so init_decls has
                // them available for initialization expressions that
                // reference host vars.
                install_host_ref_locals(&mut b, &mut ctx.locals, &host_ref_infos);
                super::alloc::alloc_decls(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    type_layouts,
                    &mut pending_globals,
                    &func_name,
                    st,
                );
                install_host_param_consts(&mut b, &mut ctx.locals, host_param_consts, st);
                install_globals_as_locals(
                    &mut b,
                    &mut ctx.locals,
                    globals,
                    &combined_uses,
                    Some(&required_import_names),
                    host_module,
                    ctx.st,
                    &ctx.ambiguous_use_warnings,
                );
                ctx.filtered_names = compute_filtered_names(globals, &combined_uses, decls);
                check_no_filtered_refs(body, &ctx.filtered_names);
                collect_implicit_locals(&mut b, &mut ctx, body, UnitScope::Subroutine(name));
                super::init::init_decls(&mut b, &ctx.locals, decls, st, Some(type_layouts));
                // Pre-create blocks for all statement labels so GOTO can branch forward.
                collect_label_blocks(&mut b, body, &mut ctx.label_blocks);
                let _proc_scope_guard = ProcScopeGuard::enter(ctx.proc_scope_id);
                super::stmt::lower_stmts(&mut b, &mut ctx, body);
                drop(_proc_scope_guard);
                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(
                        &mut b,
                        &ctx.locals,
                        &ctx.locals,
                        type_layouts,
                        ctx.st,
                        ctx.internal_funcs,
                        Some(ctx.contained_host_refs),
                        None,
                    );
                }
                ensure_termination(&mut b, None);
            }

            module.add_function(func);
            for pg in pending_globals {
                module.add_global(pg.global);
            }

            // Lower nested CONTAINS subprograms (this was a latent
            // bug — the previous code only walked Program::contains).
            // Each nested sub inherits this subroutine's combined
            // host_uses + own uses, and its host_decls chain is our
            // `decls` followed by whatever host_decls we inherited —
            // so a two-level-nested contained proc can look up host
            // variables that live two scopes above it.
            let mut child_host_decls: Vec<crate::ast::decl::SpannedDecl> = decls.to_vec();
            child_host_decls.extend(host_decls.iter().cloned());
            for sub in contains {
                lower_unit(
                    module,
                    sub,
                    st,
                    globals,
                    type_layouts,
                    &combined_uses,
                    &visible_param_consts,
                    &child_host_decls,
                    Some(func_name.as_str()),
                    host_module,
                    alloc_return_funcs,
                    optional_params,
                    descriptor_params,
                    internal_funcs,
                    elemental_funcs,
                    char_len_star_params,
                    contained_host_refs,
                    ambiguous_use_warnings,
                    true,
                    proc_scope_id,
                );
            }
        }
        ProgramUnit::Function {
            name,
            decls,
            body,
            args,
            result,
            return_type,
            bind,
            uses,
            contains,
            prefix,
            ..
        } => {
            let func_name = lowered_procedure_symbol_name(
                name,
                bind.as_ref(),
                host_link_name,
                host_module,
                internal_only,
                internal_funcs,
            );
            let proc_scope_id =
                procedure_scope_for_dummy_args_with_host(st, name, args, host_scope_id);
            let visible_param_consts =
                collect_decl_param_consts_with_scope(decls, host_param_consts, st);

            // Hidden-result ABI: allocatable arrays use a 384-byte array
            // descriptor, while scalar character results use a 32-byte
            // string descriptor. In both cases the caller provides the
            // descriptor storage as param 0 and the callee returns void.
            let hidden_result_abi = function_hidden_result_abi(
                name,
                result,
                return_type.as_ref(),
                decls,
                bind.as_ref(),
            );
            let uses_hidden_result = hidden_result_abi != HiddenResultAbi::None;

            let (func_params, ir_ret_ty) = if uses_hidden_result {
                let desc_size = match hidden_result_abi {
                    HiddenResultAbi::ArrayDescriptor => 384,
                    HiddenResultAbi::StringDescriptor => 32,
                    HiddenResultAbi::DerivedAggregate => {
                        let result_name = result.as_deref().unwrap_or(name.as_str()).to_lowercase();
                        derived_type_name_for_result_var(return_type, &result_name, decls)
                            .and_then(|dt_name| {
                                type_layouts
                                    .get(&dt_name)
                                    .map(|layout| layout.size.max(1) as u64)
                            })
                            .unwrap_or(8)
                    }
                    HiddenResultAbi::ComplexBuffer => {
                        // 8 bytes for complex(sp), 16 for complex(dp).
                        let kind = super::core::complex_result_kind(
                            name,
                            result,
                            return_type.as_ref(),
                            decls,
                            st,
                        );
                        if kind == 8 {
                            16
                        } else {
                            8
                        }
                    }
                    HiddenResultAbi::None => 0,
                };
                let desc_ptr_ty = IrType::Ptr(Box::new(IrType::Array(
                    Box::new(IrType::Int(IntWidth::I8)),
                    desc_size,
                )));
                let sret = Param {
                    name: "_sret".into(),
                    ty: desc_ptr_ty,
                    id: ValueId(0),
                    fortran_noalias: false,
                };
                // Real args shifted by 1 so _sret is param 0.
                let real: Vec<Param> = args
                    .iter()
                    .enumerate()
                    .filter_map(|(i, arg)| {
                        if let DummyArg::Name(n) = arg {
                            let elem_ty = arg_type_from_decls(n, decls, Some(st));
                            let fortran_noalias = arg_is_fortran_noalias(n, decls);
                            let uses_descriptor =
                                arg_uses_descriptor_for_lowering(n, decls, st, proc_scope_id);
                            let uses_string_descriptor =
                                arg_uses_string_descriptor_from_decls(n, decls);
                            let is_derived = arg_derived_type_name(n, decls).is_some();
                            if arg_has_value_attr(n, decls) {
                                Some(Param {
                                    name: n.clone(),
                                    ty: elem_ty,
                                    id: ValueId(i as u32 + 1),
                                    fortran_noalias: false,
                                })
                            } else {
                                Some(Param {
                                    name: n.clone(),
                                    ty: by_ref_storage_ir_type(
                                        &elem_ty,
                                        uses_descriptor,
                                        uses_string_descriptor,
                                        is_derived,
                                    ),
                                    id: ValueId(i as u32 + 1),
                                    fortran_noalias,
                                })
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                let mut params = vec![sret];
                params.extend(real);
                (params, IrType::Void)
            } else {
                let result_name = result.as_deref().unwrap_or(name.as_str());
                let result_is_pointer = decl_is_pointer(result_name, decls);
                let mut ret_ty = if bind.is_some()
                    && matches!(return_type.as_ref(), Some(TypeSpec::Character(_)))
                {
                    IrType::Int(IntWidth::I8)
                } else {
                    return_type
                        .as_ref()
                        .map(|ts| lower_type_spec_st(ts, Some(st)))
                        .unwrap_or_else(|| arg_type_from_decls(result_name, decls, Some(st)))
                };
                if result_is_pointer && !ret_ty.is_ptr() {
                    ret_ty = IrType::Ptr(Box::new(ret_ty));
                }
                let params: Vec<Param> = args
                    .iter()
                    .enumerate()
                    .filter_map(|(i, arg)| {
                        if let DummyArg::Name(n) = arg {
                            let elem_ty = arg_type_from_decls(n, decls, Some(st));
                            let fortran_noalias = arg_is_fortran_noalias(n, decls);
                            let uses_descriptor =
                                arg_uses_descriptor_for_lowering(n, decls, st, proc_scope_id);
                            let uses_string_descriptor =
                                arg_uses_string_descriptor_from_decls(n, decls);
                            let is_derived = arg_derived_type_name(n, decls).is_some();
                            if arg_has_value_attr(n, decls) {
                                Some(Param {
                                    name: n.clone(),
                                    ty: elem_ty,
                                    id: ValueId(i as u32),
                                    fortran_noalias: false,
                                })
                            } else {
                                Some(Param {
                                    name: n.clone(),
                                    ty: by_ref_storage_ir_type(
                                        &elem_ty,
                                        uses_descriptor,
                                        uses_string_descriptor,
                                        is_derived,
                                    ),
                                    id: ValueId(i as u32),
                                    fortran_noalias,
                                })
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                (params, ret_ty)
            };

            // Host-association closure params for contained functions.
            // Trailing pointer params, one per host-local variable the
            // body reads or writes. See `build_host_ref_params`.
            let mut func_params = func_params;
            let mut hidden_len_params: Vec<(String, ValueId)> = Vec::new();
            // See sister site above for why we compute from decls
            // instead of looking up the bare-name map.
            let own_cls_flags = compute_char_len_star_flags(args, decls);
            if own_cls_flags.iter().any(|f| *f) {
                let normal_count = func_params.len();
                for (flag, arg) in own_cls_flags.iter().zip(args.iter()) {
                    if *flag {
                        if let DummyArg::Name(n) = arg {
                            let hid_id = ValueId((normal_count + hidden_len_params.len()) as u32);
                            func_params.push(Param {
                                name: format!("__len_{}", n.to_lowercase()),
                                ty: IrType::Int(IntWidth::I64),
                                id: hid_id,
                                fortran_noalias: false,
                            });
                            hidden_len_params.push((n.to_lowercase(), hid_id));
                        }
                    }
                }
            }
            let host_ref_infos = build_host_ref_params(
                name,
                host_decls,
                host_param_consts,
                contained_host_refs,
                func_params.len() as u32,
                st,
                &mut func_params,
            );

            let mut func = Function::new(func_name.clone(), func_params, ir_ret_ty.clone());
            // Propagate PURE/ELEMENTAL from AST prefix.
            use crate::ast::unit::Prefix;
            func.is_pure = prefix.iter().any(|p| matches!(p, Prefix::Pure));
            func.is_elemental = prefix.iter().any(|p| matches!(p, Prefix::Elemental));
            func.internal_only = internal_only;
            let mut ctx = LowerCtx::new(
                st,
                globals,
                type_layouts,
                alloc_return_funcs,
                optional_params,
                descriptor_params,
                internal_funcs,
                elemental_funcs,
                char_len_star_params,
                contained_host_refs,
                ambiguous_use_warnings.clone(),
            );
            ctx.proc_scope_id = proc_scope_id;
            let mut pending_globals: Vec<PendingGlobal> = Vec::new();
            let combined_uses: Vec<crate::ast::decl::SpannedDecl> =
                host_uses.iter().chain(uses.iter()).cloned().collect();
            let required_import_names = collect_required_import_names(decls, body);

            // Build param_info skipping the sret param (not a Fortran
            // variable) and __host_* closure-passing pointers (installed
            // via install_host_ref_locals below).
            let param_info: Vec<(String, ValueId, IrType, bool)> = func
                .params
                .iter()
                .filter(|p| {
                    p.name != "_sret"
                        && !p.name.starts_with("__len_")
                        && !p.name.starts_with("__host_")
                })
                .map(|p| {
                    let pname = p.name.to_lowercase();
                    let elem_ty = arg_type_from_decls(&pname, decls, Some(st));
                    let is_value = arg_has_value_attr(&pname, decls);
                    (pname, p.id, elem_ty, is_value)
                })
                .collect();

            {
                let mut b = FuncBuilder::new(&mut func);
                let _setup_proc_scope_guard = ProcScopeGuard::enter(ctx.proc_scope_id);

                let mut hidden_len_addrs: HashMap<String, ValueId> = HashMap::new();
                for (hname, hid) in &hidden_len_params {
                    let slot = b.alloca(IrType::Int(IntWidth::I64));
                    b.store(*hid, slot);
                    hidden_len_addrs.insert(hname.clone(), slot);
                }

                for (pname, pid, elem_ty, is_value) in &param_info {
                    if *is_value {
                        let slot = b.alloca(elem_ty.clone());
                        b.store(*pid, slot);
                        ctx.insert_scalar(pname.clone(), slot, elem_ty.clone());
                    } else {
                        let uses_descriptor =
                            arg_uses_descriptor_for_lowering(pname, decls, st, proc_scope_id);
                        let uses_string_descriptor =
                            arg_uses_string_descriptor_from_decls(pname, decls);
                        let dt_name = arg_derived_type_name(pname, decls);
                        let is_pointer = decl_is_pointer(pname, decls);
                        let local_elem_ty = dummy_local_ir_type(
                            elem_ty,
                            dt_name.as_deref(),
                            is_pointer,
                            type_layouts,
                        );
                        let slot = b.alloca(by_ref_storage_ir_type(
                            elem_ty,
                            uses_descriptor,
                            uses_string_descriptor,
                            dt_name.is_some(),
                        ));
                        b.store(*pid, slot);
                        let ck = if let Some(&len_slot) = hidden_len_addrs.get(pname) {
                            CharKind::AssumedLen { len_addr: len_slot }
                        } else {
                            arg_char_kind_from_decls(pname, decls, st)
                        };
                        ctx.locals.insert(
                            pname.clone(),
                            LocalInfo {
                                addr: slot,
                                ty: local_elem_ty,
                                dims: arg_dims_from_decls(pname, decls, &visible_param_consts, st),
                                allocatable: false,
                                descriptor_arg: uses_descriptor,
                                by_ref: true,
                                char_kind: ck,
                                derived_type: dt_name,
                                inline_const: None,
                                is_pointer,
                                runtime_dim_upper: vec![],
                                is_class: decl_is_class(pname, decls),
                                logical_kind: arg_logical_kind_from_decls(
                                    pname,
                                    decls,
                                    Some(&visible_param_consts),
                                    st,
                                ),
                                last_dim_assumed_size: arg_last_dim_assumed_size_from_decls(
                                    pname, decls,
                                ),
                            },
                        );
                        if decl_is_optional(pname, decls) {
                            ctx.optional_locals.insert(pname.clone());
                        }
                    }
                }

                for (pname, _, _, is_value) in &param_info {
                    if *is_value || hidden_len_addrs.contains_key(pname) {
                        continue;
                    }
                    let Some(len_expr) = arg_runtime_char_len_expr_from_decls(pname, decls, st)
                    else {
                        continue;
                    };
                    let len_raw = super::expr::lower_expr(&mut b, &ctx.locals, &len_expr, ctx.st);
                    let len_addr = b.alloca(IrType::Int(IntWidth::I64));
                    let len_val = clamp_nonnegative_i64(&mut b, len_raw);
                    b.store(len_val, len_addr);
                    if let Some(info) = ctx.locals.get_mut(pname) {
                        info.char_kind = CharKind::FixedRuntime { len_addr };
                    }
                }
                install_runtime_dim_bounds(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    ctx.st,
                    type_layouts,
                );
                install_assumed_shape_lower_overrides(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    ctx.st,
                    type_layouts,
                );
                install_explicit_shape_dummy_rebase(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    ctx.st,
                    type_layouts,
                );
                clear_intent_out_allocatable_array_params(&mut b, &param_info, &ctx.locals, decls);
                clear_intent_out_derived_params(
                    &mut b,
                    &param_info,
                    &ctx.locals,
                    decls,
                    type_layouts,
                );

                let result_name = result.as_deref().unwrap_or(name.as_str()).to_lowercase();
                ctx.result_name = Some(result_name.clone());
                ctx.hidden_result_abi = hidden_result_abi;

                let result_is_pointer = decl_is_pointer(&result_name, decls);

                if hidden_result_abi == HiddenResultAbi::ArrayDescriptor {
                    // The hidden first param is the caller-provided array descriptor.
                    let result_char_kind = arg_char_kind_from_decls(&result_name, decls, st);
                    let elem_ty = match result_char_kind {
                        CharKind::Fixed(len) => fixed_char_storage_ir_type(len),
                        _ => arg_type_from_decls(&result_name, decls, Some(st)),
                    };
                    let result_derived_type = arg_derived_type_name(&result_name, decls);
                    let local_elem_ty = derived_local_storage_ir_type(
                        &elem_ty,
                        result_derived_type.as_deref(),
                        type_layouts,
                    );
                    let result_dims =
                        arg_dims_from_decls(&result_name, decls, &visible_param_consts, st);
                    ctx.locals.insert(
                        result_name.clone(),
                        LocalInfo {
                            addr: ValueId(0),
                            ty: local_elem_ty,
                            dims: result_dims,
                            allocatable: true,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: result_char_kind,
                            derived_type: result_derived_type,
                            inline_const: None,
                            is_pointer: false,
                            runtime_dim_upper: vec![],
                            is_class: false,
                            logical_kind: None,
                            last_dim_assumed_size: false,
                        },
                    );
                } else if hidden_result_abi == HiddenResultAbi::DerivedAggregate {
                    let dt_name =
                        derived_type_name_for_result_var(return_type, &result_name, decls)
                            .expect("derived hidden-result function missing result type");
                    if let Some(layout) = type_layouts.get(&dt_name) {
                        if derived_layout_needs_runtime_initialization(layout, type_layouts) {
                            initialize_derived_storage(&mut b, ValueId(0), layout, type_layouts);
                        }
                    }
                    ctx.locals.insert(
                        result_name.clone(),
                        LocalInfo {
                            addr: ValueId(0),
                            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            dims: vec![],
                            allocatable: false,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: CharKind::None,
                            derived_type: Some(dt_name),
                            inline_const: None,
                            is_pointer: false,
                            runtime_dim_upper: vec![],
                            is_class: false,
                            logical_kind: None,
                            last_dim_assumed_size: false,
                        },
                    );
                    ctx.result_addr = Some(ValueId(0));
                    ctx.result_type = Some(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                } else if hidden_result_abi == HiddenResultAbi::ComplexBuffer {
                    // The hidden first param is the caller-allocated complex
                    // buffer typed as `Ptr<[i8 x 8/16]>` so the call-site IR
                    // type matches the caller's byte-buffer alloca. The body
                    // needs a *typed* complex pointer (`Ptr<[Float x 2]>`)
                    // for two reasons:
                    //   1. The complex-assign path stores two Float lanes —
                    //      a typed pointer keeps load/store types consistent
                    //      with the IR verifier.
                    //   2. Generic dispatch on the result variable consults
                    //      `b.func().value_type(addr)` to match candidates;
                    //      a `Ptr<[Float x 2]>` is recognised as complex,
                    //      while `Ptr<[i8 x 8]>` matches no complex formal.
                    // GEP at byte offset 0 with a Float-array result type
                    // produces the typed view without changing the runtime
                    // address.
                    let kind = super::core::complex_result_kind(
                        name,
                        result,
                        return_type.as_ref(),
                        decls,
                        st,
                    );
                    let fw = if kind == 8 {
                        FloatWidth::F64
                    } else {
                        FloatWidth::F32
                    };
                    let cplx_ty = IrType::Array(Box::new(IrType::Float(fw)), 2);
                    let zero_off = b.const_i64(0);
                    let typed_addr = b.gep(ValueId(0), vec![zero_off], cplx_ty.clone());
                    ctx.locals.insert(
                        result_name.clone(),
                        LocalInfo {
                            addr: typed_addr,
                            ty: cplx_ty.clone(),
                            dims: vec![],
                            allocatable: false,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: CharKind::None,
                            derived_type: None,
                            inline_const: None,
                            is_pointer: false,
                            runtime_dim_upper: vec![],
                            is_class: false,
                            logical_kind: None,
                            last_dim_assumed_size: false,
                        },
                    );
                    ctx.result_addr = Some(typed_addr);
                    ctx.result_type = Some(IrType::Ptr(Box::new(cplx_ty)));
                } else if hidden_result_abi == HiddenResultAbi::StringDescriptor {
                    // Scalar character results use the hidden StringDescriptor
                    // ABI, but the body still writes to a normal local result
                    // variable. We materialize that local through alloc_decls
                    // / ensure_hidden_string_result_local below and copy it
                    // into the hidden descriptor right before return.
                } else if result_is_pointer {
                    let result_addr = b.alloca(ir_ret_ty.clone());
                    let zero_byte = b.const_i32(0);
                    let eight = b.const_i64(8);
                    b.call(
                        FuncRef::External("memset".into()),
                        vec![result_addr, zero_byte, eight],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    ctx.locals.insert(
                        result_name.clone(),
                        LocalInfo {
                            addr: result_addr,
                            ty: match &ir_ret_ty {
                                IrType::Ptr(elem) => (**elem).clone(),
                                other => other.clone(),
                            },
                            dims: vec![],
                            allocatable: false,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: CharKind::None,
                            derived_type: derived_type_name_for_result_var(
                                return_type,
                                &result_name,
                                decls,
                            ),
                            inline_const: None,
                            is_pointer: true,
                            runtime_dim_upper: vec![],
                            is_class: false,
                            logical_kind: None,
                            last_dim_assumed_size: false,
                        },
                    );
                    ctx.result_addr = Some(result_addr);
                    ctx.result_type = Some(ir_ret_ty.clone());
                } else if let Some(dt_name) =
                    derived_type_name_for_result_var(return_type, &result_name, decls)
                {
                    // Derived-type FUNCTION result: allocate a struct-shaped
                    // buffer ([i8 x layout.size]) and register the result
                    // variable with `derived_type = Some(name)` so component
                    // access (e.g. `vec_add%x = ...`) lands on the buffer.
                    // Without this, the generic `b.alloca(ir_ret_ty)` path
                    // allocates a `ptr<ptr<i8>>` slot, ComponentAccess can't
                    // resolve the type name, and every assignment to the
                    // result variable is silently dropped. derived_type_name_
                    // for_result_var accepts both header-level (`type(t)
                    // function f`) and body-level (`function f result(r);
                    // type(t) :: r`) declarations.
                    let layout = type_layouts.get(&dt_name);
                    let size = layout.map(|l| l.size as u64).unwrap_or(8);
                    let buf_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), size);
                    let result_addr = b.alloca(buf_ty);
                    if let Some(layout) = layout {
                        if derived_layout_needs_runtime_initialization(layout, type_layouts) {
                            initialize_derived_storage(&mut b, result_addr, layout, type_layouts);
                        }
                    }
                    ctx.locals.insert(
                        result_name.clone(),
                        LocalInfo {
                            addr: result_addr,
                            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            dims: vec![],
                            allocatable: false,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: CharKind::None,
                            derived_type: Some(dt_name),
                            inline_const: None,
                            is_pointer: false,
                            runtime_dim_upper: vec![],
                            is_class: false,
                            logical_kind: None,
                            last_dim_assumed_size: false,
                        },
                    );
                    ctx.result_addr = Some(result_addr);
                    ctx.result_type = Some(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                } else {
                    let result_addr = b.alloca(ir_ret_ty.clone());
                    ctx.insert_scalar(result_name.clone(), result_addr, ir_ret_ty.clone());
                    ctx.result_addr = Some(result_addr);
                    ctx.result_type = Some(ir_ret_ty.clone());
                }

                install_common_locals(&mut b, &mut ctx.locals, decls);
                install_equivalence_locals(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    st,
                );
                install_host_ref_locals(&mut b, &mut ctx.locals, &host_ref_infos);
                super::alloc::alloc_decls(
                    &mut b,
                    &mut ctx.locals,
                    decls,
                    &visible_param_consts,
                    type_layouts,
                    &mut pending_globals,
                    &func_name,
                    st,
                );
                if hidden_result_abi == HiddenResultAbi::StringDescriptor {
                    let _proc_scope_guard = ProcScopeGuard::enter(ctx.proc_scope_id);
                    ensure_hidden_string_result_local(
                        &mut b,
                        &mut ctx.locals,
                        &result_name,
                        return_type.as_ref(),
                        &visible_param_consts,
                        st,
                        type_layouts,
                    );
                }
                install_host_param_consts(&mut b, &mut ctx.locals, host_param_consts, st);
                install_globals_as_locals(
                    &mut b,
                    &mut ctx.locals,
                    globals,
                    &combined_uses,
                    Some(&required_import_names),
                    host_module,
                    ctx.st,
                    &ctx.ambiguous_use_warnings,
                );
                ctx.filtered_names = compute_filtered_names(globals, &combined_uses, decls);
                check_no_filtered_refs(body, &ctx.filtered_names);
                collect_implicit_locals(&mut b, &mut ctx, body, UnitScope::Function(name));
                super::init::init_decls(&mut b, &ctx.locals, decls, st, Some(type_layouts));
                if hidden_result_abi == HiddenResultAbi::ArrayDescriptor {
                    if let Some(info) = ctx.locals.get(&result_name).cloned() {
                        if !info.allocatable || info.is_pointer {
                            // Already handled above by attribute exclusion.
                        }
                        allocate_runtime_shape_array_result(
                            &mut b,
                            &ctx.locals,
                            &result_name,
                            ValueId(0),
                            &info.ty,
                            decls,
                            &visible_param_consts,
                            ctx.st,
                            type_layouts,
                        );
                    }
                }
                collect_label_blocks(&mut b, body, &mut ctx.label_blocks);
                let _proc_scope_guard = ProcScopeGuard::enter(ctx.proc_scope_id);
                super::stmt::lower_stmts(&mut b, &mut ctx, body);
                drop(_proc_scope_guard);

                if b.func().block(b.current_block()).terminator.is_none() {
                    if hidden_result_abi == HiddenResultAbi::StringDescriptor {
                        lower_hidden_string_result_copy(&mut b, &ctx);
                    }
                    let result_is_pointer = ctx
                        .locals
                        .get(&result_name)
                        .map(|info| info.is_pointer)
                        .unwrap_or(false);
                    let derived_result_type =
                        derived_type_name_for_result_var(return_type, &result_name, decls);
                    let skip = if matches!(
                        hidden_result_abi,
                        HiddenResultAbi::ArrayDescriptor | HiddenResultAbi::DerivedAggregate
                    ) {
                        Some(ValueId(0))
                    } else if !result_is_pointer && derived_result_type.is_some() {
                        ctx.result_addr
                    } else {
                        None
                    };
                    insert_implicit_dealloc(
                        &mut b,
                        &ctx.locals,
                        &ctx.locals,
                        type_layouts,
                        ctx.st,
                        ctx.internal_funcs,
                        Some(ctx.contained_host_refs),
                        skip,
                    );
                    if uses_hidden_result {
                        b.ret(None);
                    } else if !result_is_pointer && derived_result_type.is_some() {
                        // Derived-type result: return the buffer
                        // address as a Ptr(i8) (the declared return
                        // type). A zero-offset GEP through `i8`
                        // reshapes Ptr(Array(i8, N)) into Ptr(i8).
                        let result_addr = ctx
                            .result_addr
                            .expect("derived-return function has result_addr");
                        let zero = b.const_i64(0);
                        let byte_ptr = b.gep(result_addr, vec![zero], IrType::Int(IntWidth::I8));
                        b.ret(Some(byte_ptr));
                    } else {
                        let result_addr =
                            ctx.result_addr.expect("non-sret function has result_addr");
                        let rv = b.load(result_addr);
                        b.ret(Some(rv));
                    }
                }
            }

            module.add_function(func);
            for pg in pending_globals {
                module.add_global(pg.global);
            }

            // Lower nested CONTAINS subprograms with the accumulated
            // host_decls chain (our decls + inherited).
            let mut child_host_decls: Vec<crate::ast::decl::SpannedDecl> = decls.to_vec();
            child_host_decls.extend(host_decls.iter().cloned());
            for sub in contains {
                lower_unit(
                    module,
                    sub,
                    st,
                    globals,
                    type_layouts,
                    &combined_uses,
                    &visible_param_consts,
                    &child_host_decls,
                    Some(func_name.as_str()),
                    host_module,
                    alloc_return_funcs,
                    optional_params,
                    descriptor_params,
                    internal_funcs,
                    elemental_funcs,
                    char_len_star_params,
                    contained_host_refs,
                    ambiguous_use_warnings,
                    true,
                    proc_scope_id,
                );
            }
        }
        ProgramUnit::Module {
            decls,
            uses,
            contains,
            ..
        } => {
            // Module globals are installed in pass 1 (collect_module_globals).
            // The module body has no executable statements, but its CONTAINS
            // subprograms (module procedures) must be lowered as top-level
            // functions so they are emitted into the object file.
            let visible_param_consts =
                collect_decl_param_consts_with_scope(decls, host_param_consts, st);
            let combined_uses: Vec<crate::ast::decl::SpannedDecl> =
                host_uses.iter().chain(uses.iter()).cloned().collect();
            let module_name = match &unit.node {
                ProgramUnit::Module { name, .. } => Some(name.as_str()),
                _ => None,
            };
            // Module procedures don't have host-local closure association;
            // they resolve module-level names through globals. Pass an
            // empty host_decls slice.
            let no_host_decls: Vec<crate::ast::decl::SpannedDecl> = Vec::new();
            for sub in contains {
                let module_scope = module_name.and_then(|n| {
                    st.all_scopes()
                        .iter()
                        .enumerate()
                        .find_map(|(idx, scope)| match &scope.kind {
                            crate::sema::symtab::ScopeKind::Module(scope_name)
                                if scope_name.eq_ignore_ascii_case(n) =>
                            {
                                Some(idx)
                            }
                            _ => None,
                        })
                });
                lower_unit(
                    module,
                    sub,
                    st,
                    globals,
                    type_layouts,
                    &combined_uses,
                    &visible_param_consts,
                    &no_host_decls,
                    None,
                    module_name,
                    alloc_return_funcs,
                    optional_params,
                    descriptor_params,
                    internal_funcs,
                    elemental_funcs,
                    char_len_star_params,
                    contained_host_refs,
                    ambiguous_use_warnings,
                    false,
                    module_scope,
                );
            }
        }
        ProgramUnit::Submodule {
            parent,
            name: submodule_name,
            decls,
            uses,
            contains,
            ..
        } => {
            // F2018 §11.2.3: a submodule provides implementations for the
            // separate-module procedures declared in its parent module's
            // interface block.  The parent module already installed its
            // globals in pass 1; the submodule's own decls (if any) act
            // like additional private module-scope state.  We treat the
            // submodule's CONTAINS subprograms exactly like the parent
            // module's contains — emit them as top-level functions whose
            // host scope is the parent module — so the linker sees the
            // implementations the program later calls into.
            //
            // Caveat: only separate-module-procedure bodies (those with
            // a `module` prefix or matching a parent interface) link as
            // `afs_modproc_<parent>_<name>`; plain contained helpers
            // (`pure function anycolor(...)` declared only inside the
            // submodule) live in the submodule's own scope, and the call
            // site resolves them through the `Submodule(name)` scope —
            // so their definition must use the submodule name to match.
            // Use the scope-aware folder so initializers like
            // `integer, parameter :: ilp = int64` (where int64 is
            // imported from another module) can resolve via the
            // symbol table; otherwise the param falls through to a
            // zero-initialized module global.
            let visible_param_consts =
                collect_decl_param_consts_with_scope(decls, host_param_consts, st);
            let combined_uses: Vec<crate::ast::decl::SpannedDecl> =
                host_uses.iter().chain(uses.iter()).cloned().collect();
            let no_host_decls: Vec<crate::ast::decl::SpannedDecl> = Vec::new();
            for sub in contains {
                let sub_is_smp_body = match &sub.node {
                    ProgramUnit::Function { prefix, .. }
                    | ProgramUnit::Subroutine { prefix, .. } => prefix
                        .iter()
                        .any(|p| matches!(p, crate::ast::unit::Prefix::Module)),
                    _ => false,
                };
                // SMP bodies link under the parent module's name (per
                // F2018 §11.2.3 — the implementation slot belongs to
                // the parent's interface). Plain helpers contained in
                // the submodule live in the submodule's own scope and
                // link there. host_module drives the IR procedure link
                // name AND the install_globals_as_locals lookup; for
                // SMP bodies these two needs diverge — link name needs
                // parent, but globals lookup also needs the containing
                // submodule (since commit d770b77 mangles
                // submodule-local globals under the submodule name).
                // Stash that submodule via the extra_host thread-local
                // so install_globals_as_locals_in can pick it up.
                let host_module_name = if sub_is_smp_body {
                    parent.as_str()
                } else {
                    submodule_name.as_str()
                };
                let _smp_extra_host_guard = if sub_is_smp_body {
                    Some(SmpExtraHostGuard::set(submodule_name.clone()))
                } else {
                    None
                };
                let submod_scope =
                    st.all_scopes()
                        .iter()
                        .enumerate()
                        .find_map(|(idx, scope)| match &scope.kind {
                            crate::sema::symtab::ScopeKind::Submodule(scope_name)
                                if scope_name.eq_ignore_ascii_case(submodule_name) =>
                            {
                                Some(idx)
                            }
                            _ => None,
                        });
                lower_unit(
                    module,
                    sub,
                    st,
                    globals,
                    type_layouts,
                    &combined_uses,
                    &visible_param_consts,
                    &no_host_decls,
                    None,
                    Some(host_module_name),
                    alloc_return_funcs,
                    optional_params,
                    descriptor_params,
                    internal_funcs,
                    elemental_funcs,
                    char_len_star_params,
                    contained_host_refs,
                    ambiguous_use_warnings,
                    false,
                    submod_scope,
                );
            }
        }
        _ => {}
    }
}
