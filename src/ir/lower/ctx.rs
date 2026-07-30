//! Lowering context — `LowerCtx`, `LocalInfo`, RAII guards for the
//! thread-local proc-scope/submodule state, loop-scope tracking, and
//! the small helper enums (`CharKind`, `HiddenResultAbi`) that
//! `LocalInfo` depends on.
//!
//! Extracted from `lower::core` in sprint 04 step 2. Visibility is
//! kept tight (`pub(super)` for the bulk of items, `pub(crate)` only
//! where the type already crossed a module boundary — namely
//! `CharKind`, which `sema::amod` constructs by name).

use crate::ir::inst::{BlockId, ValueId};
use crate::ir::types::IrType;
use crate::sema::symtab::SymbolTable;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::const_scalar::ConstScalar;
use super::core::{ModuleGlobalInfo, PendingGlobal};

pub(super) type AmbiguousUseWarnings = Rc<RefCell<HashSet<(String, String, String)>>>;

thread_local! {
    /// Sema scope id of the procedure currently being lowered. Set by
    /// `LowerCtx::with_proc_scope` around `lower_stmts` so the
    /// stateless `lower_expr_full` recursion can intercept F77
    /// statement-function call sites without threading an extra
    /// parameter through 60-plus call sites. `None` when lowering
    /// outside any procedure body (e.g. expression evaluation during
    /// `init_decls`).
    static CURRENT_PROC_SCOPE: RefCell<Option<crate::sema::symtab::ScopeId>> =
        const { RefCell::new(None) };

    /// For SMP body lowering: the submodule containing the body. The
    /// procedure's link name lives under the parent module, but the
    /// install_globals_as_locals path also needs the submodule so it
    /// can pull in the submodule's locally-declared parameters
    /// (mangled under the submodule name post-d770b77).
    static SMP_EXTRA_HOST: RefCell<Option<String>> = const { RefCell::new(None) };

    /// Active F2008 BLOCK-local USE declarations while lowering nested
    /// statements. Sema preloads these modules but the immutable symbol
    /// table does not enter a block scope during lowering, so generic
    /// dispatch needs this side channel to see names imported only inside
    /// the current BLOCK.
    static ACTIVE_BLOCK_USES: RefCell<Vec<Vec<crate::ast::decl::SpannedDecl>>> =
        const { RefCell::new(Vec::new()) };

    static ACTIVE_BLOCK_SCOPES: RefCell<Vec<crate::sema::symtab::ScopeId>> =
        const { RefCell::new(Vec::new()) };
}

pub(super) fn current_proc_scope() -> Option<crate::sema::symtab::ScopeId> {
    CURRENT_PROC_SCOPE.with(|c| *c.borrow())
}

pub(super) fn current_smp_extra_host() -> Option<String> {
    SMP_EXTRA_HOST.with(|c| c.borrow().clone())
}

pub(super) fn active_block_uses() -> Vec<Vec<crate::ast::decl::SpannedDecl>> {
    ACTIVE_BLOCK_USES.with(|uses| uses.borrow().clone())
}

pub(super) fn active_block_scopes() -> Vec<crate::sema::symtab::ScopeId> {
    ACTIVE_BLOCK_SCOPES.with(|scopes| scopes.borrow().clone())
}

/// RAII guard: install `scope` as the current procedure scope until
/// the guard is dropped, then restore the previous value. Lets nested
/// contained-subprogram lowering recover its outer scope cleanly.
pub(super) struct ProcScopeGuard(Option<crate::sema::symtab::ScopeId>);

impl ProcScopeGuard {
    pub(super) fn enter(scope: Option<crate::sema::symtab::ScopeId>) -> Self {
        let prev = CURRENT_PROC_SCOPE.with(|c| c.replace(scope));
        ProcScopeGuard(prev)
    }
}

impl Drop for ProcScopeGuard {
    fn drop(&mut self) {
        let prev = self.0;
        CURRENT_PROC_SCOPE.with(|c| *c.borrow_mut() = prev);
    }
}

pub(super) struct SmpExtraHostGuard(Option<String>);

impl SmpExtraHostGuard {
    pub(super) fn set(name: String) -> Self {
        let prev = SMP_EXTRA_HOST.with(|c| c.replace(Some(name)));
        SmpExtraHostGuard(prev)
    }
}

impl Drop for SmpExtraHostGuard {
    fn drop(&mut self) {
        let prev = self.0.take();
        SMP_EXTRA_HOST.with(|c| *c.borrow_mut() = prev);
    }
}

pub(super) struct BlockUseGuard;

impl BlockUseGuard {
    pub(super) fn enter(uses: &[crate::ast::decl::SpannedDecl]) -> Self {
        ACTIVE_BLOCK_USES.with(|active| active.borrow_mut().push(uses.to_vec()));
        BlockUseGuard
    }
}

impl Drop for BlockUseGuard {
    fn drop(&mut self) {
        ACTIVE_BLOCK_USES.with(|active| {
            active.borrow_mut().pop();
        });
    }
}

pub(super) struct BlockScopeGuard(bool);

impl BlockScopeGuard {
    pub(super) fn enter(scope: Option<crate::sema::symtab::ScopeId>) -> Self {
        if let Some(scope) = scope {
            ACTIVE_BLOCK_SCOPES.with(|active| active.borrow_mut().push(scope));
            return BlockScopeGuard(true);
        }
        BlockScopeGuard(false)
    }
}

impl Drop for BlockScopeGuard {
    fn drop(&mut self) {
        if self.0 {
            ACTIVE_BLOCK_SCOPES.with(|active| {
                active.borrow_mut().pop();
            });
        }
    }
}

/// Maximum array rank (Fortran allows up to 15).
#[allow(dead_code)]
pub(super) const MAX_RANK: usize = 15;

/// Loop context for EXIT/CYCLE targeting.
pub(super) struct LoopScope {
    pub(super) name: Option<String>,
    pub(super) header: BlockId, // CYCLE target
    pub(super) exit: BlockId,   // EXIT target
}

/// Non-loop construct target for named EXIT statements.
pub(super) struct ConstructExitScope {
    pub(super) name: String,
    pub(super) exit: BlockId,
    /// Number of active lexical cleanup scopes owned outside this construct.
    /// A named EXIT must clean every later scope before reaching `exit`.
    pub(super) cleanup_depth: usize,
}

/// Runtime cleanup owned by an active lexical construct.
pub(super) struct LexicalCleanupScope {
    pub(super) labels: HashSet<u64>,
    pub(super) owned_locals: HashMap<String, LocalInfo>,
}

/// Character variable kind: how string storage is managed.
#[derive(Clone, PartialEq)]
pub(crate) enum CharKind {
    /// Not a character variable.
    None,
    /// Fixed-length character(N): addr points to N-byte stack buffer.
    Fixed(i64),
    /// Fixed-length character whose length is only known at runtime.
    /// For locals, `addr` is a stack slot holding the heap buffer
    /// pointer. For by-ref dummies, `addr` is the usual slot holding
    /// the caller's pointer. `len_addr` stores the runtime length.
    FixedRuntime { len_addr: ValueId },
    /// Deferred-length character(:), allocatable: addr points to 32-byte StringDescriptor.
    Deferred,
    /// Assumed-length character(*) dummy parameter.  The runtime
    /// length is held in a hidden i64 parameter appended after the
    /// normal positional args.  `len_addr` is the alloca holding
    /// the hidden param's value so reads can load it at runtime.
    AssumedLen { len_addr: ValueId },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HiddenResultAbi {
    None,
    ArrayDescriptor,
    StringDescriptor,
    DerivedAggregate,
    /// Complex scalar function result. Caller allocates an 8-byte
    /// (real(sp)) or 16-byte (real(dp)) buffer, passes its address as
    /// the hidden first param; callee writes the two float lanes
    /// through that pointer and returns void. Without this, the
    /// IR-level return type was `[Float x 2]` aggregate which codegen
    /// packed into x0 as 8 bytes — the caller then memcpy'd from x0
    /// treating the value AS a pointer (SEGV on first complex-returning
    /// call to e.g. stdlib's `gamma_dist_pdf_csp`).
    ComplexBuffer,
}

/// Info about a local variable.
#[derive(Clone)]
pub(super) struct LocalInfo {
    pub(super) addr: ValueId,
    pub(super) ty: IrType,
    /// For arrays: (lower_bound, extent) per dimension. Empty for scalars.
    pub(super) dims: Vec<(i64, i64)>,
    /// Is this an allocatable variable?
    pub(super) allocatable: bool,
    /// Does this local carry runtime array metadata through a descriptor even
    /// though it is not allocatable (for example an assumed-shape dummy)?
    pub(super) descriptor_arg: bool,
    /// Is this a pass-by-reference parameter? If true, `addr` holds a pointer
    /// to the caller's storage. Reads/writes go through the pointer.
    pub(super) by_ref: bool,
    /// Character variable kind (fixed-length, deferred, or not character).
    pub(super) char_kind: CharKind,
    /// Derived type name (for component access resolution). Empty for non-derived.
    pub(super) derived_type: Option<String>,
    /// For PARAMETER-attributed locals whose initializer const-folds:
    /// the compile-time value to inline at every use. When `Some`,
    /// `Expr::Name` lookups should materialize this constant
    /// directly via `b.const_i32`/`b.const_i64`/etc., instead of
    /// loading through `addr`. Audit MAJOR-4: this lets parameters
    /// avoid wasting a `.data` slot per scope.
    pub(super) inline_const: Option<ConstScalar>,
    /// Fortran `POINTER` attribute on a scalar local.  When true,
    /// `addr` is an `alloca ptr<ty>` — a pointer slot that holds
    /// the address of the associated target (or null when
    /// unassociated).  Reads/writes go through the slot just like
    /// `by_ref`, but `by_ref` is reserved for dummy arguments that
    /// cannot carry a Fortran `POINTER` attribute's semantics
    /// (reassociation via `=>`, dereference on plain assignment).
    pub(super) is_pointer: bool,
    /// Per-dimension runtime upper bound (i64 value id) for arrays
    /// whose bounds are not compile-time constants — most commonly
    /// explicit-shape dummies like `xs(n)` where `n` is a dummy
    /// argument. When `Some`, bounds checks and cumulative-stride
    /// computation consult this value instead of `dims[i].1`.
    /// Empty vec (or an all-`None` vec) means the compile-time
    /// `dims` is authoritative. Parallel to `dims`.
    pub(super) runtime_dim_upper: Vec<Option<ValueId>>,
    /// True when the declaration used CLASS(...) rather than TYPE(...).
    pub(super) is_class: bool,
    /// Logical kind from the source declaration (1, 2, 4, or 8) when the
    /// variable is `logical(kind)`. Default-kind logical leaves this as
    /// Some(4); non-logical declarations leave it None. Needed because
    /// kind=1/2/8 logicals are stored as `IrType::Int(I8/I16/I64)` to get
    /// kind-correct storage size, which would otherwise be indistinguishable
    /// from real integer locals at later lookup points
    /// (semantic-type recovery, generic dispatch, print formatting, etc.).
    pub(super) logical_kind: Option<u8>,
    /// True for explicit-shape dummy arrays whose last dimension is
    /// `*` (assumed-size, F2018 §8.5.8.5). Such a dummy carries no
    /// upper bound on the last dim — accesses past `size(actual)` are
    /// legal as long as the underlying storage permits — so bounds
    /// checks must be skipped on that dim. The caller's descriptor
    /// (or the static `(1, 0)` sentinel emitted by extract_array_dims)
    /// would otherwise reject every legal access.
    pub(super) last_dim_assumed_size: bool,
}

/// Lowering context — tracks locals, loop scopes, and symbol table.
pub(super) struct LowerCtx<'a> {
    /// Target layout of the module under construction (x02).
    pub(super) layout: crate::target::TargetLayout,
    pub(super) locals: HashMap<String, LocalInfo>,
    /// Stable bindings owned by the current program unit.
    ///
    /// Lexical constructs temporarily replace entries in `locals` when a
    /// BLOCK declaration or associate name shadows an outer entity. Explicit
    /// RETURN still has to finalize and deallocate the program-unit owners,
    /// so procedure teardown must not depend on the currently visible map.
    pub(super) procedure_locals: HashMap<String, LocalInfo>,
    /// Lowercase names of OPTIONAL dummy arguments in the current subprogram.
    /// Hidden character-length forwarding must treat an absent optional
    /// character dummy as length zero instead of dereferencing its null slot.
    pub(super) optional_locals: HashSet<String>,
    pub(super) loops: Vec<LoopScope>,
    pub(super) construct_exits: Vec<ConstructExitScope>,
    pub(super) lexical_cleanups: Vec<LexicalCleanupScope>,
    /// SAVE-promoted locals collected while lowering this procedure,
    /// including declarations nested in BLOCK constructs. The owning
    /// program-unit lowerer flushes these into the IR module after the
    /// function body is complete.
    pub(super) pending_globals: Vec<PendingGlobal>,
    /// Stable procedure-specific prefix used for SAVE global symbols.
    pub(super) save_owner: String,
    /// Lexical BLOCK ordinal within this procedure. Ordinals are assigned
    /// by deterministic AST traversal, so symbol names do not depend on
    /// source paths, temporary directories, or compilation-unit order.
    next_block_save_scope: u64,
    pub(super) st: &'a SymbolTable,
    /// Module-scoped globals visible by (lowercase module name,
    /// lowercase variable name). Populated by the lower_file
    /// pre-pass over `ProgramUnit::Module` units so any subsequent
    /// function that USE-imports the module can resolve the name
    /// to a `GlobalAddr`. Keying by (module, var) is what lets
    /// install_globals_as_locals filter by the current function's
    /// USE statements, honor ONLY lists, and apply renames.
    pub(super) globals: &'a HashMap<(String, String), ModuleGlobalInfo>,
    pub(super) type_layouts: &'a crate::sema::type_layout::TypeLayoutRegistry,
    /// Names that a `use mod, only: ...` statement explicitly
    /// excluded. install_globals_as_locals populates this from the
    /// difference between a module's exported globals and the
    /// only-list. Audit MAJOR-1: a reference to a name in this
    /// set must produce a compile error rather than silently
    /// lowering to const_int 0.
    pub(super) filtered_names: HashSet<String>,
    /// For functions: address of the result variable (for RETURN).
    pub(super) result_addr: Option<ValueId>,
    /// For functions: the return type.
    pub(super) result_type: Option<IrType>,
    /// For functions: lowercase result variable name when one exists.
    pub(super) result_name: Option<String>,
    /// Hidden result ABI for this function, if any.
    pub(super) hidden_result_abi: HiddenResultAbi,
    /// Names of functions in the compilation unit that return allocatable
    /// arrays (sret convention). Used at call sites to detect when to
    /// pass a temp descriptor as the hidden first arg. Audit6 BLOCKING-1.
    pub(super) alloc_return_funcs: &'a HashSet<String>,
    /// Per-subroutine optional-parameter bitmap: maps lowercase callee name
    /// to a Vec<bool> (one entry per positional parameter, true = OPTIONAL).
    /// Pre-populated by `collect_optional_params` so call sites can pass
    /// null pointers for absent optional arguments (PRESENT support).
    pub(super) optional_params: &'a HashMap<String, Vec<bool>>,
    /// Per-subroutine/function descriptor-parameter bitmap: maps lowercase
    /// callee name to a Vec<bool> (one entry per positional parameter,
    /// true = lower this dummy through an ArrayDescriptor).
    pub(super) descriptor_params: &'a HashMap<String, Vec<bool>>,
    /// Lowercase same-module subprogram name → Module::functions index.
    /// Used so same-compilation-unit calls lower to FuncRef::Internal instead
    /// of pretending to be external references.
    pub(super) internal_funcs: &'a HashMap<String, u32>,
    /// Lowercase names of functions declared ELEMENTAL in this compilation unit.
    pub(super) elemental_funcs: &'a HashSet<String>,
    /// Per-callee bitmap of which params are character(len=*).
    /// Call sites append the string length as a hidden i64 arg for
    /// each flagged position.
    pub(super) char_len_star_params: &'a HashMap<String, Vec<bool>>,
    /// Per-callee list of host-associated variable names the callee
    /// needs threaded in as hidden trailing pointer args. See the
    /// closure-passing ABI documented on `lower_file::contained_host_refs`.
    pub(super) contained_host_refs: &'a HashMap<String, Vec<String>>,
    /// Map from Fortran statement label (u64) to the IR basic block that
    /// begins at that label. Pre-populated by `collect_label_blocks` before
    /// lowering so that GOTO can branch forward as well as backward.
    pub(super) label_blocks: HashMap<u64, BlockId>,
    /// Labeled FORMAT statements visible in the current scoping unit.
    pub(super) format_labels: HashMap<u64, String>,
    /// Cross-function dedupe for ambiguous USE-import warnings emitted by
    /// install_globals_as_locals. Large fortsh units can otherwise print the
    /// exact same ambiguity hundreds or thousands of times while lowering each
    /// contained procedure separately.
    pub(super) ambiguous_use_warnings: AmbiguousUseWarnings,
    /// Sema's `ScopeId` for the procedure currently being lowered.
    /// Set in the Program/Subroutine/Function arms of `lower_unit`.
    /// Statement-function lookup keys off this — without it we can't
    /// distinguish `cabs1` defined in one stdlib BLAS routine from a
    /// homonymous statement function in another.
    pub(super) proc_scope_id: Option<crate::sema::symtab::ScopeId>,
}

impl<'a> LowerCtx<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        st: &'a SymbolTable,
        globals: &'a HashMap<(String, String), ModuleGlobalInfo>,
        type_layouts: &'a crate::sema::type_layout::TypeLayoutRegistry,
        alloc_return_funcs: &'a HashSet<String>,
        optional_params: &'a HashMap<String, Vec<bool>>,
        descriptor_params: &'a HashMap<String, Vec<bool>>,
        internal_funcs: &'a HashMap<String, u32>,
        elemental_funcs: &'a HashSet<String>,
        char_len_star_params: &'a HashMap<String, Vec<bool>>,
        contained_host_refs: &'a HashMap<String, Vec<String>>,
        ambiguous_use_warnings: AmbiguousUseWarnings,
        save_owner: String,
        layout: crate::target::TargetLayout,
    ) -> Self {
        Self {
            layout,
            locals: HashMap::new(),
            procedure_locals: HashMap::new(),
            optional_locals: HashSet::new(),
            loops: Vec::new(),
            construct_exits: Vec::new(),
            lexical_cleanups: Vec::new(),
            pending_globals: Vec::new(),
            save_owner,
            next_block_save_scope: 0,
            st,
            globals,
            type_layouts,
            filtered_names: HashSet::new(),
            result_addr: None,
            result_type: None,
            result_name: None,
            hidden_result_abi: HiddenResultAbi::None,
            alloc_return_funcs,
            optional_params,
            descriptor_params,
            internal_funcs,
            elemental_funcs,
            char_len_star_params,
            contained_host_refs,
            label_blocks: HashMap::new(),
            format_labels: HashMap::new(),
            ambiguous_use_warnings,
            proc_scope_id: None,
        }
    }

    pub(super) fn next_block_save_owner(&mut self) -> String {
        let ordinal = self.next_block_save_scope;
        self.next_block_save_scope = self
            .next_block_save_scope
            .checked_add(1)
            .expect("BLOCK SAVE scope ordinal overflow");
        // Dots cannot appear in a Fortran identifier, so this scope
        // separator cannot collide with a procedure local such as
        // `block_0_value` when save_global_name appends the entity name.
        format!("{}.block.{}", self.save_owner, ordinal)
    }

    pub(super) fn capture_procedure_locals(&mut self) {
        self.procedure_locals.clone_from(&self.locals);
    }

    pub(super) fn insert_scalar(&mut self, name: String, addr: ValueId, ty: IrType) {
        self.locals.insert(
            name,
            LocalInfo {
                addr,
                ty,
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
    }

    pub(super) fn insert_param_by_ref(&mut self, name: String, addr: ValueId, ty: IrType) {
        self.locals.insert(
            name,
            LocalInfo {
                addr,
                ty,
                dims: vec![],
                allocatable: false,
                descriptor_arg: false,
                by_ref: true,
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
    }

    pub(super) fn push_loop(&mut self, name: Option<String>, header: BlockId, exit: BlockId) {
        self.loops.push(LoopScope { name, header, exit });
    }

    pub(super) fn pop_loop(&mut self) {
        self.loops.pop();
    }

    pub(super) fn push_construct_exit(&mut self, name: Option<String>, exit: BlockId) {
        if let Some(name) = name {
            self.construct_exits.push(ConstructExitScope {
                name: name.to_ascii_lowercase(),
                exit,
                cleanup_depth: self.lexical_cleanups.len(),
            });
        }
    }

    pub(super) fn pop_construct_exit(&mut self, name: &Option<String>) {
        if name.is_some() {
            self.construct_exits.pop();
        }
    }

    /// Look up an F77 statement function by name in the current
    /// procedure scope. Returns `None` when the name doesn't refer to
    /// a statement function (or no procedure scope is set).
    pub(super) fn lookup_statement_function(
        &self,
        name: &str,
    ) -> Option<&'a crate::sema::symtab::StatementFunctionDef> {
        let scope_id = self.proc_scope_id?;
        self.st.lookup_statement_function(scope_id, name)
    }

    /// Find loop by construct name (or innermost if None).
    pub(super) fn find_loop(&self, name: &Option<String>) -> Option<&LoopScope> {
        if let Some(n) = name {
            self.loops.iter().rev().find(|l| {
                l.name
                    .as_deref()
                    .map(|s| s.eq_ignore_ascii_case(n))
                    .unwrap_or(false)
            })
        } else {
            self.loops.last()
        }
    }

    pub(super) fn find_construct_exit(&self, name: &Option<String>) -> Option<(BlockId, usize)> {
        let name = name.as_ref()?;
        self.construct_exits
            .iter()
            .rev()
            .find(|scope| scope.name.eq_ignore_ascii_case(name))
            .map(|scope| (scope.exit, scope.cleanup_depth))
    }
}
