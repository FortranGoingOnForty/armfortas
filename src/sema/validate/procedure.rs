//! Procedure characteristics used by procedure-pointer assignment.
//!
//! F2018 10.2.2.4 requires an explicitly interfaced procedure pointer and
//! its target to have the same procedure characteristics, with two narrow
//! exceptions: an impure pointer may target a pure procedure, and a
//! nonelemental pointer may target an elemental intrinsic. Keep extraction
//! and comparison here so calls, declarations, and later interface checks can
//! share one deterministic representation instead of growing independent
//! approximations.

use std::collections::{HashMap, HashSet};

use crate::ast::decl::ArraySpec;
use crate::ast::expr::{Expr, SpannedExpr};
use crate::lexer::Span;
use crate::sema::symtab::{Intent, Scope, ScopeId, ScopeKind, Symbol, SymbolKind, TypeInfo};

use super::allocatable::{expr_selects_component, leaf_field_layout};
use super::core::{is_intrinsic_name, Ctx};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProcedureNature {
    Function,
    Subroutine,
}

struct ProcedureCharacteristics {
    nature: ProcedureNature,
    pure: bool,
    elemental: bool,
    bind_c: bool,
    dummies: Vec<DummyCharacteristics>,
    result: Option<ProcedureResultCharacteristics>,
}

struct DummyCharacteristics {
    optional: bool,
    pointer: bool,
    kind: DummyKind,
}

enum DummyKind {
    Data(DataCharacteristics),
    Procedure {
        explicit: bool,
        characteristics: Option<Box<ProcedureCharacteristics>>,
    },
}

struct DataCharacteristics {
    type_info: Option<TypeInfo>,
    declared_scope: ScopeId,
    shape: Vec<ShapeDimension>,
    intent: Option<Intent>,
    allocatable: bool,
    asynchronous: bool,
    contiguous: bool,
    value: bool,
    volatile: bool,
    pointer: bool,
    target: bool,
}

enum ProcedureResultCharacteristics {
    Data(DataCharacteristics),
    Procedure {
        explicit: bool,
        characteristics: Option<Box<ProcedureCharacteristics>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ShapeDimension {
    Explicit {
        lower: Option<String>,
        upper: String,
    },
    AssumedShape {
        lower: Option<String>,
    },
    AssumedSize {
        lower: Option<String>,
    },
    Deferred,
    AssumedRank,
}

struct PointerObject {
    name: String,
    characteristics: Option<ProcedureCharacteristics>,
}

struct ProcedureTarget {
    name: String,
    intrinsic: bool,
    pointer: bool,
    explicit: bool,
    characteristics: Option<ProcedureCharacteristics>,
}

enum TargetResolution {
    Null,
    Procedure(ProcedureTarget),
    DataName(String),
    DataFunctionResult(String),
    AbstractInterface(String),
    IneligibleIntrinsic(String),
    Unknown,
}

/// Validate the assignment when the left operand is a procedure pointer.
///
/// Returns true when the assignment was recognized as a procedure-pointer
/// assignment. The ordinary data-pointer validator must not run in that case.
pub(super) fn validate_procedure_pointer_assignment(
    ctx: &mut Ctx<'_>,
    pointer: &SpannedExpr,
    target: &SpannedExpr,
    span: Span,
) -> bool {
    let Some(pointer) = resolve_pointer_object(ctx, pointer) else {
        return false;
    };
    validate_pointer_target(ctx, &pointer, target, span);
    true
}

pub(super) fn validate_procedure_pointer_initializer(
    ctx: &mut Ctx<'_>,
    pointer_name: &str,
    owner_scope: ScopeId,
    interface_name: &str,
    target: &SpannedExpr,
    span: Span,
) {
    let target_expr = unparenthesized(target);
    let is_null = matches!(
        &target_expr.node,
        Expr::FunctionCall { callee, .. }
            if matches!(
                &unparenthesized(callee).node,
                Expr::Name { name } if name.eq_ignore_ascii_case("null")
            )
    );
    if !is_null {
        let Expr::Name { name } = &target_expr.node else {
            ctx.error(
                span,
                format!(
                    "procedure pointer '{}' initializer must be NULL() or a procedure name",
                    pointer_name
                ),
            );
            return;
        };
        if let Some(symbol) = ctx.lookup_lexical(name) {
            let owner_kind = &ctx.st.scope(symbol.scope).kind;
            let is_procedure = is_procedure_symbol(symbol);
            let forbidden = symbol.kind == SymbolKind::ProcedurePointer
                || (is_procedure && ctx.current_args.contains(&name.to_ascii_lowercase()))
                || (is_procedure
                    && matches!(
                        owner_kind,
                        ScopeKind::Program(_) | ScopeKind::Function(_) | ScopeKind::Subroutine(_)
                    )
                    && !symbol.attrs.external);
            if forbidden {
                ctx.error(
                    span,
                    format!(
                        "procedure pointer '{}' initializer '{}' must name an external, module, or unrestricted specific intrinsic procedure",
                        pointer_name, name
                    ),
                );
                return;
            }
        }
    }

    let pointer = PointerObject {
        name: pointer_name.to_string(),
        characteristics: characteristics_for_interface_name(
            ctx,
            owner_scope,
            interface_name,
            &mut HashSet::new(),
        ),
    };
    validate_pointer_target(ctx, &pointer, target, span);
}

/// Validate an actual argument associated with a procedure dummy.
///
/// F2018 15.5.2.9 requires every such actual to denote a procedure. When the
/// dummy has an explicit interface, the effective procedure characteristics
/// must also match. Procedure-pointer dummies additionally require a pointer
/// actual unless a nonpointer procedure is associated with INTENT(IN).
pub(super) fn validate_procedure_dummy_actual(
    ctx: &mut Ctx<'_>,
    dummy_name: &str,
    dummy_scope: ScopeId,
    dummy_has_explicit_interface: bool,
    dummy_pointer: bool,
    dummy_intent: Option<Intent>,
    actual: &SpannedExpr,
) {
    let actual = unparenthesized(actual);
    if matches!(actual.node, Expr::NilArgument) {
        return;
    }
    if let Expr::ConditionalExpr {
        then_val, else_val, ..
    } = &actual.node
    {
        validate_procedure_dummy_actual(
            ctx,
            dummy_name,
            dummy_scope,
            dummy_has_explicit_interface,
            dummy_pointer,
            dummy_intent,
            then_val,
        );
        validate_procedure_dummy_actual(
            ctx,
            dummy_name,
            dummy_scope,
            dummy_has_explicit_interface,
            dummy_pointer,
            dummy_intent,
            else_val,
        );
        return;
    }

    let Some(dummy) = ctx
        .st
        .scope(dummy_scope)
        .symbols
        .get(&dummy_name.to_ascii_lowercase())
    else {
        return;
    };
    let expected = direct_procedure_characteristics(ctx, dummy, &mut HashSet::new());
    if dummy_has_explicit_interface && expected.is_none() {
        ctx.error(
            actual.span,
            format!(
                "procedure dummy '{}' has an unresolved declared interface",
                dummy_name
            ),
        );
        return;
    }

    match resolve_procedure_target(ctx, actual) {
        TargetResolution::Unknown => {}
        TargetResolution::Null => {
            if !dummy_pointer {
                ctx.error(
                    actual.span,
                    format!(
                        "actual argument for nonpointer procedure dummy '{}' cannot be NULL()",
                        dummy_name
                    ),
                );
            }
        }
        TargetResolution::DataName(_) | TargetResolution::DataFunctionResult(_) => {
            ctx.error(
                actual.span,
                format!(
                    "actual argument for procedure dummy '{}' is not a procedure or procedure pointer",
                    dummy_name
                ),
            );
        }
        TargetResolution::AbstractInterface(name) => ctx.error(
            actual.span,
            format!(
                "actual procedure '{}' for dummy '{}' is an abstract interface and is not callable",
                name, dummy_name
            ),
        ),
        TargetResolution::IneligibleIntrinsic(name) => ctx.error(
            actual.span,
            format!(
                "actual procedure '{}' for dummy '{}' is not an unrestricted specific intrinsic procedure",
                name, dummy_name
            ),
        ),
        TargetResolution::Procedure(target) => {
            if dummy_pointer && !target.pointer && dummy_intent != Some(Intent::In) {
                ctx.error(
                    actual.span,
                    format!(
                        "nonpointer actual procedure '{}' requires procedure pointer dummy '{}' to have INTENT(IN)",
                        target.name, dummy_name
                    ),
                );
                return;
            }

            let Some(expected) = expected.as_ref() else {
                return;
            };
            let Some(actual_characteristics) = target.characteristics.as_ref() else {
                // An explicitly EXTERNAL actual may have only an implicit
                // interface. Its conformance remains a program requirement;
                // the caller has no characteristics available to compare.
                return;
            };
            if let Some(reason) = incompatible_characteristic(
                ctx,
                expected,
                actual_characteristics,
                target.intrinsic,
            ) {
                ctx.error(
                    actual.span,
                    format!(
                        "actual procedure '{}' for dummy '{}' has incompatible characteristics: {}",
                        target.name, dummy_name, reason
                    ),
                );
            }
        }
    }
}

fn validate_pointer_target(
    ctx: &mut Ctx<'_>,
    pointer: &PointerObject,
    target: &SpannedExpr,
    span: Span,
) {
    let Some(pointer_characteristics) = pointer.characteristics.as_ref() else {
        ctx.error(
            span,
            format!(
                "procedure pointer '{}' has an unresolved declared interface",
                pointer.name
            ),
        );
        return;
    };

    match resolve_procedure_target(ctx, target) {
        TargetResolution::Null | TargetResolution::Unknown => {}
        TargetResolution::DataName(name) => ctx.error(
            span,
            format!(
                "procedure pointer '{}' target '{}' is not a procedure or procedure pointer",
                pointer.name, name
            ),
        ),
        TargetResolution::DataFunctionResult(name) => ctx.error(
            span,
            format!(
                "procedure pointer '{}' target '{}' is not a procedure-pointer function result",
                pointer.name, name
            ),
        ),
        TargetResolution::AbstractInterface(name) => ctx.error(
            span,
            format!(
                "procedure pointer '{}' target '{}' is an abstract interface and cannot be a procedure target",
                pointer.name, name
            ),
        ),
        TargetResolution::IneligibleIntrinsic(name) => ctx.error(
            span,
            format!(
                "procedure pointer '{}' target '{}' is not an unrestricted specific intrinsic procedure",
                pointer.name, name
            ),
        ),
        TargetResolution::Procedure(target) => {
            if !target.intrinsic
                && target
                    .characteristics
                    .as_ref()
                    .is_some_and(|characteristics| characteristics.elemental)
            {
                ctx.error(
                    span,
                    format!(
                        "procedure pointer '{}' target '{}' is a nonintrinsic ELEMENTAL procedure",
                        pointer.name, target.name
                    ),
                );
                return;
            }

            let Some(target_characteristics) = target.characteristics.as_ref() else {
                if !target.explicit {
                    ctx.error(
                        span,
                        format!(
                            "procedure pointer '{}' target '{}' does not have the required explicit interface",
                            pointer.name, target.name
                        ),
                    );
                } else {
                    ctx.error(
                        span,
                        format!(
                            "procedure pointer '{}' target '{}' has an unresolved explicit interface",
                            pointer.name, target.name
                        ),
                    );
                }
                return;
            };

            if let Some(reason) = incompatible_characteristic(
                ctx,
                pointer_characteristics,
                target_characteristics,
                target.intrinsic,
            ) {
                ctx.error(
                    span,
                    format!(
                        "procedure pointer '{}' target '{}' has incompatible characteristics: {}",
                        pointer.name, target.name, reason
                    ),
                );
            }
        }
    }
}

fn resolve_pointer_object(ctx: &Ctx<'_>, expr: &SpannedExpr) -> Option<PointerObject> {
    let expr = unparenthesized(expr);
    if expr_selects_component(expr) {
        let leaf = leaf_field_layout(ctx, expr)?;
        if !leaf.field.procedure_pointer {
            return None;
        }
        let interface_name = match &leaf.field.type_info {
            TypeInfo::Derived(name) | TypeInfo::Class(name) => name,
            _ => {
                return Some(PointerObject {
                    name: leaf.field.name.clone(),
                    characteristics: None,
                });
            }
        };
        let owner_scope = leaf.owner_layout.owner_scope.unwrap_or(ctx.scope_id);
        let characteristics = characteristics_for_interface_name(
            ctx,
            owner_scope,
            interface_name,
            &mut HashSet::new(),
        );
        return Some(PointerObject {
            name: leaf.field.name.clone(),
            characteristics,
        });
    }

    let Expr::Name { name } = &expr.node else {
        return None;
    };
    let symbol = ctx.lookup_lexical(name)?;
    if symbol.kind != SymbolKind::ProcedurePointer {
        return None;
    }
    Some(PointerObject {
        name: name.clone(),
        characteristics: procedure_pointer_characteristics(ctx, symbol, &mut HashSet::new()),
    })
}

fn resolve_procedure_target(ctx: &Ctx<'_>, expr: &SpannedExpr) -> TargetResolution {
    let expr = unparenthesized(expr);
    match &expr.node {
        Expr::FunctionCall { callee, .. } => {
            if let Expr::Name { name } = &unparenthesized(callee).node {
                let symbol = ctx.lookup_lexical(name);
                if name.eq_ignore_ascii_case("null")
                    && symbol.is_none_or(|symbol| {
                        symbol.kind == SymbolKind::IntrinsicProc || symbol.attrs.intrinsic
                    })
                {
                    return TargetResolution::Null;
                }
                if symbol.is_none() {
                    return TargetResolution::Unknown;
                }
            }
            let Some(characteristics) = callable_characteristics(ctx, callee) else {
                return TargetResolution::DataFunctionResult(expr.to_sexpr());
            };
            let Some(ProcedureResultCharacteristics::Procedure {
                explicit,
                characteristics,
            }) = characteristics.result
            else {
                return TargetResolution::DataFunctionResult(expr.to_sexpr());
            };
            TargetResolution::Procedure(ProcedureTarget {
                name: expr.to_sexpr(),
                intrinsic: false,
                pointer: true,
                explicit,
                characteristics: characteristics.map(|characteristics| *characteristics),
            })
        }
        Expr::ComponentAccess { .. } if expr_selects_component(expr) => {
            let Some(leaf) = leaf_field_layout(ctx, expr) else {
                return TargetResolution::Unknown;
            };
            if !leaf.field.procedure_pointer {
                return TargetResolution::DataName(expr.to_sexpr());
            }
            let interface_name = match &leaf.field.type_info {
                TypeInfo::Derived(name) | TypeInfo::Class(name) => name,
                _ => {
                    return TargetResolution::Procedure(ProcedureTarget {
                        name: expr.to_sexpr(),
                        intrinsic: false,
                        pointer: true,
                        explicit: true,
                        characteristics: None,
                    });
                }
            };
            let owner_scope = leaf.owner_layout.owner_scope.unwrap_or(ctx.scope_id);
            TargetResolution::Procedure(ProcedureTarget {
                name: expr.to_sexpr(),
                intrinsic: false,
                pointer: true,
                explicit: true,
                characteristics: characteristics_for_interface_name(
                    ctx,
                    owner_scope,
                    interface_name,
                    &mut HashSet::new(),
                ),
            })
        }
        Expr::Name { name } => {
            let symbol = ctx.lookup_lexical(name);
            if symbol.is_none_or(|symbol| {
                symbol.kind == SymbolKind::IntrinsicProc || symbol.attrs.intrinsic
            }) {
                if let Some(characteristics) = specific_intrinsic_characteristics(name) {
                    return TargetResolution::Procedure(ProcedureTarget {
                        name: name.clone(),
                        intrinsic: true,
                        pointer: false,
                        explicit: true,
                        characteristics: Some(characteristics),
                    });
                }
                if is_intrinsic_name(name) {
                    return TargetResolution::IneligibleIntrinsic(name.clone());
                }
            }
            let Some(symbol) = symbol else {
                return TargetResolution::Unknown;
            };
            if symbol.attrs.abstract_interface {
                return TargetResolution::AbstractInterface(name.clone());
            }
            if is_procedure_pointer_symbol(symbol) {
                return TargetResolution::Procedure(ProcedureTarget {
                    name: name.clone(),
                    intrinsic: false,
                    pointer: true,
                    explicit: symbol.attrs.procedure_iface.is_some(),
                    characteristics: procedure_pointer_characteristics(
                        ctx,
                        symbol,
                        &mut HashSet::new(),
                    ),
                });
            }
            if is_procedure_symbol(symbol) {
                let intrinsic = symbol.kind == SymbolKind::IntrinsicProc || symbol.attrs.intrinsic;
                let characteristics =
                    direct_procedure_characteristics(ctx, symbol, &mut HashSet::new());
                return TargetResolution::Procedure(ProcedureTarget {
                    name: name.clone(),
                    intrinsic,
                    pointer: false,
                    explicit: intrinsic || characteristics.is_some(),
                    characteristics,
                });
            }
            TargetResolution::DataName(name.clone())
        }
        _ => TargetResolution::DataName(expr.to_sexpr()),
    }
}

fn callable_characteristics(
    ctx: &Ctx<'_>,
    callee: &SpannedExpr,
) -> Option<ProcedureCharacteristics> {
    let callee = unparenthesized(callee);
    match &callee.node {
        Expr::Name { name } => {
            let symbol = ctx.lookup_lexical(name)?;
            if is_procedure_pointer_symbol(symbol) {
                procedure_pointer_characteristics(ctx, symbol, &mut HashSet::new())
            } else if is_procedure_symbol(symbol) {
                direct_procedure_characteristics(ctx, symbol, &mut HashSet::new())
            } else {
                None
            }
        }
        Expr::ComponentAccess { .. } if expr_selects_component(callee) => {
            let leaf = leaf_field_layout(ctx, callee)?;
            if !leaf.field.procedure_pointer {
                return None;
            }
            let interface_name = match &leaf.field.type_info {
                TypeInfo::Derived(name) | TypeInfo::Class(name) => name,
                _ => return None,
            };
            characteristics_for_interface_name(
                ctx,
                leaf.owner_layout.owner_scope.unwrap_or(ctx.scope_id),
                interface_name,
                &mut HashSet::new(),
            )
        }
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum IntrinsicType {
    Integer,
    Real,
    DoublePrecision,
    Complex,
    Character,
}

fn specific_intrinsic_characteristics(name: &str) -> Option<ProcedureCharacteristics> {
    use IntrinsicType::{Character, Complex, DoublePrecision, Integer, Real};

    let key = name.to_ascii_lowercase();
    let (arguments, result): (&[IntrinsicType], IntrinsicType) = match key.as_str() {
        "abs" | "acos" | "aint" | "alog" | "alog10" | "anint" | "asin" | "atan" | "cos"
        | "cosh" | "exp" | "sin" | "sinh" | "sqrt" | "tan" | "tanh" => (&[Real], Real),
        "amod" | "atan2" | "dim" | "sign" => (&[Real, Real], Real),
        "aimag" | "cabs" => (&[Complex], Real),
        "ccos" | "cexp" | "clog" | "conjg" | "csin" | "csqrt" => (&[Complex], Complex),
        "dabs" | "dacos" | "dasin" | "datan" | "dcos" | "dcosh" | "dexp" | "dint" | "dlog"
        | "dlog10" | "dnint" | "dsin" | "dsinh" | "dsqrt" | "dtan" | "dtanh" => {
            (&[DoublePrecision], DoublePrecision)
        }
        "datan2" | "ddim" | "dmod" | "dsign" => {
            (&[DoublePrecision, DoublePrecision], DoublePrecision)
        }
        "dprod" => (&[Real, Real], DoublePrecision),
        "iabs" => (&[Integer], Integer),
        "idim" | "isign" | "mod" => (&[Integer, Integer], Integer),
        "idnint" => (&[DoublePrecision], Integer),
        "nint" => (&[Real], Integer),
        "index" => (&[Character, Character], Integer),
        "len" => (&[Character], Integer),
        _ => return None,
    };

    Some(ProcedureCharacteristics {
        nature: ProcedureNature::Function,
        pure: true,
        elemental: true,
        bind_c: false,
        dummies: arguments
            .iter()
            .copied()
            .map(|argument| DummyCharacteristics {
                optional: false,
                pointer: false,
                kind: DummyKind::Data(intrinsic_data_characteristics(argument, true)),
            })
            .collect(),
        result: Some(ProcedureResultCharacteristics::Data(
            intrinsic_data_characteristics(result, false),
        )),
    })
}

fn intrinsic_data_characteristics(
    intrinsic_type: IntrinsicType,
    dummy: bool,
) -> DataCharacteristics {
    let type_info = match intrinsic_type {
        IntrinsicType::Integer => TypeInfo::Integer { kind: None },
        IntrinsicType::Real => TypeInfo::Real { kind: None },
        IntrinsicType::DoublePrecision => TypeInfo::DoublePrecision,
        IntrinsicType::Complex => TypeInfo::Complex { kind: None },
        IntrinsicType::Character => TypeInfo::Character {
            len: None,
            kind: None,
        },
    };
    DataCharacteristics {
        type_info: Some(type_info),
        // Intrinsic types do not require a declaration scope for identity.
        declared_scope: 0,
        shape: Vec::new(),
        intent: dummy.then_some(Intent::In),
        allocatable: false,
        asynchronous: false,
        contiguous: false,
        value: false,
        volatile: false,
        pointer: false,
        target: false,
    }
}

fn unparenthesized(mut expr: &SpannedExpr) -> &SpannedExpr {
    while let Expr::ParenExpr { inner } = &expr.node {
        expr = inner;
    }
    expr
}

fn is_procedure_pointer_symbol(symbol: &Symbol) -> bool {
    symbol.kind == SymbolKind::ProcedurePointer
}

fn is_procedure_symbol(symbol: &Symbol) -> bool {
    matches!(
        symbol.kind,
        SymbolKind::Function
            | SymbolKind::Subroutine
            | SymbolKind::ExternalProc
            | SymbolKind::IntrinsicProc
    ) || symbol.attrs.external
        || symbol.attrs.procedure_iface.is_some()
}

fn procedure_scope<'a>(ctx: &'a Ctx<'_>, name: &str, owner_scope: ScopeId) -> Option<&'a Scope> {
    let matches_name = |scope: &&Scope| {
        matches!(
            &scope.kind,
            ScopeKind::Function(candidate) | ScopeKind::Subroutine(candidate)
                if candidate.eq_ignore_ascii_case(name)
        )
    };
    ctx.st
        .all_scopes()
        .iter()
        .filter(matches_name)
        .find(|scope| scope.parent == Some(owner_scope))
        .or_else(|| {
            ctx.st
                .all_scopes()
                .iter()
                .filter(matches_name)
                .find(|scope| {
                    scope.parent.is_some_and(|parent| {
                        matches!(ctx.st.scope(parent).kind, ScopeKind::Interface)
                            && ctx.st.scope(parent).parent == Some(owner_scope)
                    })
                })
        })
}

fn procedure_pointer_characteristics(
    ctx: &Ctx<'_>,
    pointer: &Symbol,
    visiting: &mut HashSet<ScopeId>,
) -> Option<ProcedureCharacteristics> {
    let interface_name =
        pointer
            .attrs
            .procedure_iface
            .as_deref()
            .or(match pointer.type_info.as_ref() {
                Some(TypeInfo::Derived(name)) | Some(TypeInfo::Class(name)) => Some(name.as_str()),
                _ => None,
            })?;
    characteristics_for_interface_name(ctx, pointer.scope, interface_name, visiting)
}

fn characteristics_for_interface_name(
    ctx: &Ctx<'_>,
    owner_scope: ScopeId,
    interface_name: &str,
    visiting: &mut HashSet<ScopeId>,
) -> Option<ProcedureCharacteristics> {
    let interface = ctx.st.lookup_in(owner_scope, interface_name)?;
    direct_procedure_characteristics(ctx, interface, visiting)
}

fn direct_procedure_characteristics(
    ctx: &Ctx<'_>,
    symbol: &Symbol,
    visiting: &mut HashSet<ScopeId>,
) -> Option<ProcedureCharacteristics> {
    if symbol.kind == SymbolKind::ProcedurePointer
        || (symbol.attrs.procedure_iface.is_some()
            && !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Subroutine))
    {
        return procedure_pointer_characteristics(ctx, symbol, visiting);
    }

    let scope = procedure_scope(ctx, &symbol.name, symbol.scope)?;
    if !visiting.insert(scope.id) {
        return None;
    }
    let characteristics = build_procedure_characteristics(ctx, symbol, scope, visiting);
    visiting.remove(&scope.id);
    characteristics
}

fn build_procedure_characteristics(
    ctx: &Ctx<'_>,
    symbol: &Symbol,
    scope: &Scope,
    visiting: &mut HashSet<ScopeId>,
) -> Option<ProcedureCharacteristics> {
    let nature = match scope.kind {
        ScopeKind::Function(_) => ProcedureNature::Function,
        ScopeKind::Subroutine(_) => ProcedureNature::Subroutine,
        _ => return None,
    };
    let declared_scope = scope.id;
    let dummy_positions: HashMap<String, usize> = scope
        .arg_order
        .iter()
        .enumerate()
        .map(|(index, name)| (name.to_ascii_lowercase(), index))
        .collect();
    let mut dummies = Vec::with_capacity(scope.arg_order.len());
    for dummy_name in &scope.arg_order {
        let dummy = scope.symbols.get(&dummy_name.to_ascii_lowercase())?;
        let kind = if is_procedure_symbol(dummy) || is_procedure_pointer_symbol(dummy) {
            let characteristics =
                direct_procedure_characteristics(ctx, dummy, visiting).map(Box::new);
            DummyKind::Procedure {
                explicit: dummy.attrs.procedure_iface.is_some() || characteristics.is_some(),
                characteristics,
            }
        } else {
            DummyKind::Data(data_characteristics(
                dummy,
                declared_scope,
                &dummy_positions,
            ))
        };
        dummies.push(DummyCharacteristics {
            optional: dummy.attrs.optional,
            pointer: dummy.attrs.pointer,
            kind,
        });
    }

    let result = if nature == ProcedureNature::Function {
        let result_symbol = scope.procedure_result_symbol();
        if let Some(result) = result_symbol.filter(|result| is_procedure_pointer_symbol(result)) {
            Some(ProcedureResultCharacteristics::Procedure {
                explicit: result.attrs.procedure_iface.is_some(),
                characteristics: procedure_pointer_characteristics(ctx, result, visiting)
                    .map(Box::new),
            })
        } else {
            let metadata = result_symbol.unwrap_or(symbol);
            let mut result_data = data_characteristics(metadata, declared_scope, &dummy_positions);
            result_data.type_info = symbol
                .type_info
                .clone()
                .or_else(|| metadata.type_info.clone());
            if result_data.shape.is_empty() && symbol.attrs.result_rank > 0 {
                result_data.shape = vec![
                    ShapeDimension::AssumedShape { lower: None };
                    symbol.attrs.result_rank as usize
                ];
            }
            Some(ProcedureResultCharacteristics::Data(result_data))
        }
    } else {
        None
    };

    Some(ProcedureCharacteristics {
        nature,
        pure: symbol.attrs.pure,
        elemental: symbol.attrs.elemental,
        bind_c: symbol.attrs.bind_c || scope.bind_c,
        dummies,
        result,
    })
}

fn data_characteristics(
    symbol: &Symbol,
    declared_scope: ScopeId,
    dummy_positions: &HashMap<String, usize>,
) -> DataCharacteristics {
    DataCharacteristics {
        type_info: symbol.type_info.clone(),
        declared_scope,
        shape: symbol
            .attrs
            .array_spec
            .iter()
            .map(|spec| {
                shape_dimension(
                    spec,
                    dummy_positions,
                    symbol.attrs.allocatable || symbol.attrs.pointer,
                )
            })
            .collect(),
        intent: symbol.attrs.intent,
        allocatable: symbol.attrs.allocatable,
        asynchronous: symbol.attrs.asynchronous,
        contiguous: symbol.attrs.contiguous,
        value: symbol.attrs.value,
        volatile: symbol.attrs.volatile,
        pointer: symbol.attrs.pointer,
        target: symbol.attrs.target,
    }
}

fn shape_dimension(
    spec: &ArraySpec,
    dummy_positions: &HashMap<String, usize>,
    deferred_shape: bool,
) -> ShapeDimension {
    match spec {
        ArraySpec::Explicit { lower, upper } => ShapeDimension::Explicit {
            lower: lower
                .as_ref()
                .map(|bound| canonical_bound(bound, dummy_positions)),
            upper: canonical_bound(upper, dummy_positions),
        },
        ArraySpec::AssumedShape { .. } if deferred_shape => ShapeDimension::Deferred,
        ArraySpec::AssumedShape { lower } => ShapeDimension::AssumedShape {
            lower: lower
                .as_ref()
                .map(|bound| canonical_bound(bound, dummy_positions)),
        },
        ArraySpec::AssumedSize { lower } => ShapeDimension::AssumedSize {
            lower: lower
                .as_ref()
                .map(|bound| canonical_bound(bound, dummy_positions)),
        },
        // The parser uses `Deferred` for a bare colon before declaration
        // attributes are known. For a nonpointer, nonallocatable dummy that
        // spelling is assumed-shape; `.amod` reconstruction already records
        // it as `AssumedShape`. Normalize both representations here.
        ArraySpec::Deferred if !deferred_shape => ShapeDimension::AssumedShape { lower: None },
        ArraySpec::Deferred => ShapeDimension::Deferred,
        ArraySpec::AssumedRank => ShapeDimension::AssumedRank,
    }
}

fn canonical_bound(bound: &SpannedExpr, dummy_positions: &HashMap<String, usize>) -> String {
    let rendered = bound.to_sexpr().to_ascii_lowercase();
    let text = strip_redundant_outer_parentheses(&rendered);
    let mut canonical = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut end = start + ch.len_utf8();
            while let Some((index, next)) = chars.peek().copied() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    chars.next();
                    end = index + next.len_utf8();
                } else {
                    break;
                }
            }
            let identifier = &text[start..end];
            if let Some(position) = dummy_positions.get(identifier) {
                canonical.push_str("$arg");
                canonical.push_str(&(position + 1).to_string());
            } else {
                canonical.push_str(identifier);
            }
        } else {
            canonical.push(ch);
        }
    }
    canonical
}

fn strip_redundant_outer_parentheses(mut text: &str) -> &str {
    loop {
        let bytes = text.as_bytes();
        if bytes.first() != Some(&b'(') || bytes.last() != Some(&b')') {
            return text;
        }
        let mut depth = 0i32;
        let mut encloses_all = true;
        for (index, byte) in bytes.iter().copied().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 && index + 1 != bytes.len() {
                        encloses_all = false;
                        break;
                    }
                }
                _ => {}
            }
            if depth < 0 {
                return text;
            }
        }
        if !encloses_all || depth != 0 {
            return text;
        }
        text = text[1..text.len() - 1].trim();
    }
}

fn incompatible_characteristic(
    ctx: &Ctx<'_>,
    pointer: &ProcedureCharacteristics,
    target: &ProcedureCharacteristics,
    target_is_intrinsic: bool,
) -> Option<String> {
    if pointer.nature != target.nature {
        return Some("procedure nature differs".into());
    }
    if pointer.pure && !target.pure {
        return Some("target is not PURE".into());
    }
    if pointer.elemental != target.elemental
        && !(target_is_intrinsic && target.elemental && !pointer.elemental)
    {
        return Some("ELEMENTAL attributes differ".into());
    }
    if pointer.bind_c != target.bind_c {
        return Some("BIND(C) attributes differ".into());
    }
    if pointer.dummies.len() != target.dummies.len() {
        return Some("dummy argument count differs".into());
    }
    for (index, (pointer_dummy, target_dummy)) in
        pointer.dummies.iter().zip(&target.dummies).enumerate()
    {
        if let Some(reason) = incompatible_dummy(ctx, pointer_dummy, target_dummy) {
            return Some(format!("dummy argument {} {}", index + 1, reason));
        }
    }
    match (&pointer.result, &target.result) {
        (Some(pointer), Some(target)) => incompatible_result(ctx, pointer, target),
        (None, None) => None,
        _ => Some("function result differs".into()),
    }
}

fn incompatible_dummy(
    ctx: &Ctx<'_>,
    pointer: &DummyCharacteristics,
    target: &DummyCharacteristics,
) -> Option<String> {
    if pointer.optional != target.optional {
        return Some("has different OPTIONAL attributes".into());
    }
    match (&pointer.kind, &target.kind) {
        (DummyKind::Data(pointer), DummyKind::Data(target)) => {
            if !same_type(
                ctx,
                pointer.declared_scope,
                pointer.type_info.as_ref(),
                target.declared_scope,
                target.type_info.as_ref(),
            ) {
                return Some("has a different type".into());
            }
            if pointer.shape != target.shape {
                return Some("has a different rank or shape".into());
            }
            if pointer.intent != target.intent {
                return Some("has a different INTENT".into());
            }
            if pointer.allocatable != target.allocatable {
                return Some("has different ALLOCATABLE attributes".into());
            }
            if pointer.asynchronous != target.asynchronous {
                return Some("has different ASYNCHRONOUS attributes".into());
            }
            if pointer.contiguous != target.contiguous {
                return Some("has different CONTIGUOUS attributes".into());
            }
            if pointer.value != target.value {
                return Some("has different VALUE attributes".into());
            }
            if pointer.volatile != target.volatile {
                return Some("has different VOLATILE attributes".into());
            }
            if pointer.pointer != target.pointer {
                return Some("has different POINTER attributes".into());
            }
            if pointer.target != target.target {
                return Some("has different TARGET attributes".into());
            }
        }
        (
            DummyKind::Procedure {
                explicit: pointer_explicit,
                characteristics: pointer_characteristics,
            },
            DummyKind::Procedure {
                explicit: target_explicit,
                characteristics: target_characteristics,
            },
        ) => {
            if pointer_explicit != target_explicit {
                return Some("has different interface explicitness".into());
            }
            if pointer.pointer != target.pointer {
                return Some("has different POINTER attributes".into());
            }
            if let (Some(pointer), Some(target)) = (
                pointer_characteristics.as_deref(),
                target_characteristics.as_deref(),
            ) {
                if let Some(reason) = incompatible_characteristic(ctx, pointer, target, false) {
                    return Some(format!(
                        "has incompatible procedure characteristics ({reason})"
                    ));
                }
            }
        }
        _ => return Some("has a different entity kind".into()),
    }
    None
}

fn incompatible_result(
    ctx: &Ctx<'_>,
    pointer: &ProcedureResultCharacteristics,
    target: &ProcedureResultCharacteristics,
) -> Option<String> {
    match (pointer, target) {
        (
            ProcedureResultCharacteristics::Data(pointer),
            ProcedureResultCharacteristics::Data(target),
        ) => {
            if !same_type(
                ctx,
                pointer.declared_scope,
                pointer.type_info.as_ref(),
                target.declared_scope,
                target.type_info.as_ref(),
            ) {
                return Some("function result has a different type".into());
            }
            if pointer.shape != target.shape {
                return Some("function result has a different rank or shape".into());
            }
            if pointer.allocatable != target.allocatable {
                return Some("function result has different ALLOCATABLE attributes".into());
            }
            if pointer.pointer != target.pointer {
                return Some("function result has different POINTER attributes".into());
            }
            if pointer.contiguous != target.contiguous {
                return Some("function result has different CONTIGUOUS attributes".into());
            }
            None
        }
        (
            ProcedureResultCharacteristics::Procedure {
                explicit: pointer_explicit,
                characteristics: pointer_characteristics,
            },
            ProcedureResultCharacteristics::Procedure {
                explicit: target_explicit,
                characteristics: target_characteristics,
            },
        ) => {
            if pointer_explicit != target_explicit {
                return Some(
                    "function procedure-pointer result has different interface explicitness".into(),
                );
            }
            match (
                pointer_characteristics.as_deref(),
                target_characteristics.as_deref(),
            ) {
                (Some(pointer), Some(target)) => incompatible_characteristic(ctx, pointer, target, false)
                    .map(|reason| {
                        format!(
                            "function procedure-pointer result has incompatible characteristics ({reason})"
                        )
                    }),
                _ => None,
            }
        }
        _ => Some("function result differs between data and procedure pointer".into()),
    }
}

fn same_type(
    ctx: &Ctx<'_>,
    left_scope: ScopeId,
    left: Option<&TypeInfo>,
    right_scope: ScopeId,
    right: Option<&TypeInfo>,
) -> bool {
    fn same_kind(left: Option<u8>, right: Option<u8>, default: u8) -> bool {
        left.unwrap_or(default) == right.unwrap_or(default)
    }

    let (Some(left), Some(right)) = (left, right) else {
        return left.is_none() && right.is_none();
    };
    match (left, right) {
        (TypeInfo::Integer { kind: left }, TypeInfo::Integer { kind: right }) => {
            same_kind(*left, *right, crate::driver::defaults::default_int_kind())
        }
        (TypeInfo::Real { kind: left }, TypeInfo::Real { kind: right }) => {
            same_kind(*left, *right, crate::driver::defaults::default_real_kind())
        }
        (TypeInfo::DoublePrecision, TypeInfo::DoublePrecision) => true,
        (TypeInfo::DoublePrecision, TypeInfo::Real { kind })
        | (TypeInfo::Real { kind }, TypeInfo::DoublePrecision) => {
            same_kind(Some(8), *kind, crate::driver::defaults::default_real_kind())
        }
        (TypeInfo::Complex { kind: left }, TypeInfo::Complex { kind: right }) => {
            same_kind(*left, *right, crate::driver::defaults::default_real_kind())
        }
        (TypeInfo::Logical { kind: left }, TypeInfo::Logical { kind: right }) => {
            same_kind(*left, *right, crate::driver::defaults::default_int_kind())
        }
        (
            TypeInfo::Character {
                len: left_len,
                kind: left_kind,
            },
            TypeInfo::Character {
                len: right_len,
                kind: right_kind,
            },
        ) => left_len == right_len && same_kind(*left_kind, *right_kind, 1),
        (TypeInfo::Derived(left), TypeInfo::Derived(right))
        | (TypeInfo::Class(left), TypeInfo::Class(right)) => {
            same_derived_type(ctx, left_scope, left, right_scope, right)
        }
        (TypeInfo::ClassStar, TypeInfo::ClassStar) | (TypeInfo::TypeStar, TypeInfo::TypeStar) => {
            true
        }
        (TypeInfo::Enumeration(left), TypeInfo::Enumeration(right)) => {
            left.eq_ignore_ascii_case(right)
        }
        _ => false,
    }
}

fn same_derived_type(
    ctx: &Ctx<'_>,
    left_scope: ScopeId,
    left: &str,
    right_scope: ScopeId,
    right: &str,
) -> bool {
    let Some(layouts) = ctx.type_layouts else {
        return left.eq_ignore_ascii_case(right);
    };
    let left_layout = layouts
        .get_for_scope(left_scope, left)
        .or_else(|| layouts.get(left));
    let right_layout = layouts
        .get_for_scope(right_scope, right)
        .or_else(|| layouts.get(right));
    match (left_layout, right_layout) {
        (Some(left), Some(right)) => {
            layouts.canonical_key_for_layout(left) == layouts.canonical_key_for_layout(right)
        }
        _ => left.eq_ignore_ascii_case(right),
    }
}
