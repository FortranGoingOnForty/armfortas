//! Derived type memory layout computation.
//!
//! Computes field offsets, alignment, and total size for derived types
//! using natural alignment rules (same as C struct layout on ARM64).

use super::symtab::TypeInfo;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum FieldDefaultInit {
    Character(String),
    Integer(i128),
    Logical(bool),
    Derived(Vec<(String, FieldDefaultInit)>),
}

/// Layout of a single field in a derived type.
#[derive(Debug, Clone)]
pub struct FieldLayout {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub dims: Vec<(i64, i64)>,
    pub declared_array: bool,
    pub type_info: TypeInfo,
    /// F2018 §7.5.4.5 / §7.5.4.6 attributes on the component, carried
    /// per-field because validation of `obj%comp` as an ALLOCATE
    /// target or pointer-assignment LHS/RHS needs the leaf
    /// component's attributes, not the base variable's.
    pub allocatable: bool,
    pub pointer: bool,
    pub target: bool,
    pub default_init: Option<FieldDefaultInit>,
}

/// A type-bound procedure mapping.
#[derive(Debug, Clone)]
pub struct BoundProc {
    pub method_name: String,
    pub target_name: String,
    pub abi_name: String,
    pub nopass: bool,
}

/// Complete layout of a derived type.
#[derive(Debug, Clone)]
pub struct TypeLayout {
    pub name: String,
    pub size: usize,
    pub align: usize,
    pub fields: Vec<FieldLayout>,
    pub bound_procs: Vec<BoundProc>,
    pub final_procs: Vec<String>,
    /// Unique type tag for polymorphic dispatch. Assigned sequentially.
    pub type_tag: u64,
    /// Parent type name (from EXTENDS). None for base types.
    pub parent: Option<String>,
    /// Whether this type is ABSTRACT and therefore not a concrete dispatch target.
    pub is_abstract: bool,
}

impl TypeLayout {
    /// Look up a field by name and return its layout.
    pub fn field(&self, name: &str) -> Option<&FieldLayout> {
        let key = name.to_lowercase();
        self.fields.iter().find(|f| f.name.to_lowercase() == key)
    }

    /// Look up a type-bound procedure by method name.
    pub fn bound_proc(&self, name: &str) -> Option<&BoundProc> {
        let key = name.to_lowercase();
        self.bound_procs
            .iter()
            .find(|p| p.method_name.to_lowercase() == key)
    }
}

/// Registry of all computed type layouts.
#[derive(Debug, Default)]
pub struct TypeLayoutRegistry {
    pub layouts: HashMap<String, TypeLayout>,
    next_tag: u64,
}

impl TypeLayoutRegistry {
    pub fn new() -> Self {
        Self {
            layouts: HashMap::new(),
            next_tag: 1,
        }
    }

    pub fn alloc_tag(&mut self) -> u64 {
        let tag = self.next_tag;
        self.next_tag += 1;
        tag
    }

    pub fn get(&self, type_name: &str) -> Option<&TypeLayout> {
        self.layouts.get(&type_name.to_lowercase())
    }

    pub fn insert(&mut self, mut layout: TypeLayout) {
        if layout.type_tag == 0 {
            layout.type_tag = self.alloc_tag();
        } else if layout.type_tag >= self.next_tag {
            self.next_tag = layout.type_tag + 1;
        }
        self.layouts.insert(layout.name.to_lowercase(), layout);
    }
}

/// Compute the size and alignment of a Fortran type on ARM64.
pub fn size_of_type(ti: &TypeInfo) -> (usize, usize) {
    match ti {
        TypeInfo::Integer { kind } => {
            // No explicit kind selector → honour the driver's
            // -fdefault-integer-8 (sprint 32 #504).  Standard
            // processor default is 4.
            let k = kind
                .map(|k| k as usize)
                .unwrap_or_else(|| crate::driver::defaults::default_int_kind() as usize);
            (k, k)
        }
        TypeInfo::Real { kind } => {
            let k = kind
                .map(|k| k as usize)
                .unwrap_or_else(|| crate::driver::defaults::default_real_kind() as usize);
            (k, k)
        }
        TypeInfo::DoublePrecision => (8, 8),
        TypeInfo::Complex { kind } => {
            let k = kind.unwrap_or(4) as usize;
            (k * 2, k) // complex(4) = 8 bytes, aligned to 4
        }
        TypeInfo::Logical { kind } => {
            let k = kind.unwrap_or(4) as usize;
            (k, k)
        }
        TypeInfo::Character { len, kind: _ } => {
            let l = len.unwrap_or(1) as usize;
            (l, 1)
        }
        TypeInfo::Derived(_) => (8, 8), // resolved by caller via registry
        TypeInfo::Class(_) | TypeInfo::ClassStar | TypeInfo::TypeStar => (16, 8),
    }
}

fn eval_const_int_expr(
    expr: &crate::ast::expr::SpannedExpr,
    const_params: &HashMap<String, i64>,
) -> Option<i64> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::IntegerLiteral { text, .. } => {
            let clean = text.split('_').next().unwrap_or(text);
            clean.parse::<i64>().ok()
        }
        Expr::Name { name } => const_params.get(&name.to_lowercase()).copied(),
        Expr::UnaryOp { op, operand } => {
            let v = eval_const_int_expr(operand, const_params)?;
            match op {
                crate::ast::expr::UnaryOp::Minus => Some(-v),
                crate::ast::expr::UnaryOp::Plus => Some(v),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let l = eval_const_int_expr(left, const_params)?;
            let r = eval_const_int_expr(right, const_params)?;
            match op {
                crate::ast::expr::BinaryOp::Add => Some(l + r),
                crate::ast::expr::BinaryOp::Sub => Some(l - r),
                crate::ast::expr::BinaryOp::Mul => Some(l * r),
                crate::ast::expr::BinaryOp::Div if r != 0 => Some(l / r),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => eval_const_int_expr(inner, const_params),
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let first_arg_val = args.first().and_then(|a| {
                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                    eval_const_int_expr(e, const_params)
                } else {
                    None
                }
            });
            match name.to_lowercase().as_str() {
                "selected_int_kind" => {
                    let r = first_arg_val?;
                    Some(if r <= 2 {
                        1
                    } else if r <= 4 {
                        2
                    } else if r <= 9 {
                        4
                    } else if r <= 18 {
                        8
                    } else if r <= 38 {
                        16
                    } else {
                        -1
                    })
                }
                "selected_real_kind" => {
                    let p = first_arg_val?;
                    Some(if p <= 6 {
                        4
                    } else if p <= 15 {
                        8
                    } else {
                        -1
                    })
                }
                "kind" => {
                    let arg = args.first()?;
                    let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                        return None;
                    };
                    match &e.node {
                        Expr::RealLiteral { text, .. } => {
                            Some(if text.contains('d') || text.contains('D') {
                                8
                            } else {
                                4
                            })
                        }
                        Expr::IntegerLiteral { .. } => Some(4),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn eval_const_logical_expr(expr: &crate::ast::expr::SpannedExpr) -> Option<bool> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::LogicalLiteral { value, .. } => Some(*value),
        Expr::ParenExpr { inner } => eval_const_logical_expr(inner),
        Expr::UnaryOp {
            op: crate::ast::expr::UnaryOp::Not,
            operand,
        } => eval_const_logical_expr(operand).map(|v| !v),
        _ => None,
    }
}

fn eval_const_field_default_init(
    type_info: &TypeInfo,
    expr: &crate::ast::expr::SpannedExpr,
    registry: &TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
) -> Option<FieldDefaultInit> {
    match type_info {
        TypeInfo::Character { .. } => match &expr.node {
            crate::ast::expr::Expr::StringLiteral { value, .. } => {
                Some(FieldDefaultInit::Character(value.clone()))
            }
            _ => None,
        },
        TypeInfo::Integer { .. } => eval_const_int_expr(expr, const_params)
            .map(|value| FieldDefaultInit::Integer(value as i128)),
        TypeInfo::Logical { .. } => eval_const_logical_expr(expr).map(FieldDefaultInit::Logical),
        TypeInfo::Derived(type_name) | TypeInfo::Class(type_name) => {
            eval_const_derived_default_init(type_name, expr, registry, const_params)
        }
        _ => None,
    }
}

fn eval_const_derived_default_init(
    type_name: &str,
    expr: &crate::ast::expr::SpannedExpr,
    registry: &TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
) -> Option<FieldDefaultInit> {
    use crate::ast::expr::{Expr, SectionSubscript};

    let Expr::FunctionCall { callee, args } = &expr.node else {
        return None;
    };
    let Expr::Name { name: callee_name } = &callee.node else {
        return None;
    };
    if !callee_name.eq_ignore_ascii_case(type_name) {
        return None;
    }

    let layout = registry.get(type_name)?;
    let mut positional_idx = 0usize;
    let mut overrides = Vec::new();

    for arg in args {
        let field = if let Some(keyword) = &arg.keyword {
            layout.field(keyword)?
        } else {
            let field = layout.fields.get(positional_idx)?;
            positional_idx += 1;
            field
        };
        if !field.dims.is_empty() || field.allocatable || field.pointer {
            return None;
        }
        let SectionSubscript::Element(value_expr) = &arg.value else {
            return None;
        };
        let init =
            eval_const_field_default_init(&field.type_info, value_expr, registry, const_params)?;
        overrides.push((field.name.clone(), init));
    }

    Some(FieldDefaultInit::Derived(overrides))
}

fn eval_explicit_array_dims(
    specs: Option<&Vec<crate::ast::decl::ArraySpec>>,
    const_params: &HashMap<String, i64>,
) -> Vec<(i64, i64)> {
    let Some(specs) = specs else {
        return Vec::new();
    };
    let mut dims = Vec::new();
    for spec in specs {
        let crate::ast::decl::ArraySpec::Explicit { lower, upper } = spec else {
            return Vec::new();
        };
        let lower_val = lower
            .as_ref()
            .and_then(|expr| eval_const_int_expr(expr, const_params))
            .unwrap_or(1);
        let upper_val = match eval_const_int_expr(upper, const_params) {
            Some(value) => value,
            None => return Vec::new(),
        };
        let extent = (upper_val - lower_val + 1).max(0);
        dims.push((lower_val, extent));
    }
    dims
}

/// Convert a TypeSpec AST node to TypeInfo for layout computation.
fn type_spec_to_type_info(
    ts: &crate::ast::decl::TypeSpec,
    const_params: &HashMap<String, i64>,
) -> TypeInfo {
    use crate::ast::decl::{KindSelector, LenSpec, TypeSpec};

    match ts {
        TypeSpec::Integer(kind) => TypeInfo::Integer {
            kind: kind.as_ref().and_then(|k| match k {
                KindSelector::Expr(e) | KindSelector::Star(e) => {
                    eval_const_int_expr(e, const_params).and_then(|v| u8::try_from(v).ok())
                }
            }),
        },
        TypeSpec::Real(kind) => TypeInfo::Real {
            kind: kind.as_ref().and_then(|k| match k {
                KindSelector::Expr(e) | KindSelector::Star(e) => {
                    eval_const_int_expr(e, const_params).and_then(|v| u8::try_from(v).ok())
                }
            }),
        },
        TypeSpec::DoublePrecision => TypeInfo::DoublePrecision,
        TypeSpec::Complex(kind) => TypeInfo::Complex {
            kind: kind.as_ref().and_then(|k| match k {
                KindSelector::Expr(e) | KindSelector::Star(e) => {
                    eval_const_int_expr(e, const_params).and_then(|v| u8::try_from(v).ok())
                }
            }),
        },
        TypeSpec::DoubleComplex => TypeInfo::Complex { kind: Some(8) },
        TypeSpec::Logical(kind) => TypeInfo::Logical {
            kind: kind.as_ref().and_then(|k| match k {
                KindSelector::Expr(e) | KindSelector::Star(e) => {
                    eval_const_int_expr(e, const_params).and_then(|v| u8::try_from(v).ok())
                }
            }),
        },
        TypeSpec::Character(sel) => {
            let len = sel
                .as_ref()
                .and_then(|s| s.len.as_ref())
                .and_then(|l| match l {
                    LenSpec::Expr(e) => eval_const_int_expr(e, const_params),
                    _ => None,
                });
            let kind = sel
                .as_ref()
                .and_then(|s| s.kind.as_ref())
                .and_then(|e| eval_const_int_expr(e, const_params))
                .and_then(|v| u8::try_from(v).ok());
            TypeInfo::Character { len, kind }
        }
        TypeSpec::Type(name) => TypeInfo::Derived(name.clone()),
        TypeSpec::Class(name) => TypeInfo::Class(name.clone()),
        TypeSpec::ClassStar => TypeInfo::ClassStar,
        TypeSpec::TypeStar => TypeInfo::TypeStar,
    }
}

/// Compute the layout of a derived type from its component declarations.
pub fn compute_layout(
    type_name: &str,
    host_module: Option<&str>,
    type_bound_procs: &[crate::ast::decl::TypeBoundProc],
    final_proc_names: &[String],
    components: &[crate::ast::decl::SpannedDecl],
    parent_layout: Option<&TypeLayout>,
    registry: &TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
) -> TypeLayout {
    compute_layout_with_attrs(
        type_name,
        host_module,
        type_bound_procs,
        final_proc_names,
        components,
        parent_layout,
        false,
        registry,
        const_params,
    )
}

pub fn compute_layout_with_attrs(
    type_name: &str,
    host_module: Option<&str>,
    type_bound_procs: &[crate::ast::decl::TypeBoundProc],
    final_proc_names: &[String],
    components: &[crate::ast::decl::SpannedDecl],
    parent_layout: Option<&TypeLayout>,
    is_abstract: bool,
    registry: &TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
) -> TypeLayout {
    let mut offset: usize = 0;
    let mut max_align: usize = 1;
    let mut fields = Vec::new();

    // If this type extends a parent, start with parent's fields.
    if let Some(parent) = parent_layout {
        for f in &parent.fields {
            fields.push(f.clone());
        }
        offset = parent.size;
        max_align = parent.align;
    }

    // Process component declarations.
    for decl in components {
        if let crate::ast::decl::Decl::TypeDecl {
            type_spec,
            attrs,
            entities,
        } = &decl.node
        {
            let is_allocatable = attrs
                .iter()
                .any(|a| matches!(a, crate::ast::decl::Attribute::Allocatable));
            let is_pointer = attrs
                .iter()
                .any(|a| matches!(a, crate::ast::decl::Attribute::Pointer));
            let is_target = attrs
                .iter()
                .any(|a| matches!(a, crate::ast::decl::Attribute::Target));
            let has_dimension_attr = attrs
                .iter()
                .any(|a| matches!(a, crate::ast::decl::Attribute::Dimension(_)));

            let ti = type_spec_to_type_info(type_spec, const_params);
            for entity in entities {
                let declared_array = entity.array_spec.is_some() || has_dimension_attr;
                let dims = if is_allocatable || is_pointer {
                    Vec::new()
                } else {
                    eval_explicit_array_dims(entity.array_spec.as_ref(), const_params)
                };
                let (elem_size, elem_align) =
                    if matches!(&ti, TypeInfo::Character { len: None, .. })
                        && (is_allocatable || is_pointer)
                        && !declared_array
                    {
                        (32, 8) // StringDescriptor for deferred-length scalar char components
                } else if is_pointer
                    && !declared_array
                    && !matches!(ti, TypeInfo::Class(_))
                {
                    (8, 8) // Scalar POINTER components are pointer slots, not descriptors
                } else if is_allocatable || is_pointer {
                    (384, 8) // ArrayDescriptor size for allocatable/pointer array components
                    } else if let TypeInfo::Derived(ref dname) = ti {
                        registry
                            .get(dname)
                            .map(|l| (l.size, l.align))
                            .unwrap_or((8, 8))
                    } else {
                        size_of_type(&ti)
                    };
                // Pad to alignment.
                let padding = (elem_align - (offset % elem_align)) % elem_align;
                offset += padding;
                max_align = max_align.max(elem_align);
                let elem_count = if dims.is_empty() {
                    1usize
                } else {
                    dims.iter()
                        .map(|(_, extent)| (*extent).max(0) as usize)
                        .product::<usize>()
                };
                let field_size = elem_size.saturating_mul(elem_count.max(1));
                let default_init = if dims.is_empty() && !is_allocatable && !is_pointer {
                    entity.init.as_ref().and_then(|init| {
                        eval_const_field_default_init(&ti, init, registry, const_params)
                    })
                } else {
                    None
                };

                fields.push(FieldLayout {
                    name: entity.name.clone(),
                    offset,
                    size: field_size,
                    dims,
                    declared_array,
                    type_info: ti.clone(),
                    allocatable: is_allocatable,
                    pointer: is_pointer,
                    target: is_target,
                    default_init,
                });
                offset += field_size;
            }
        }
    }

    // Pad total size to struct alignment.
    if max_align > 0 {
        let padding = (max_align - (offset % max_align)) % max_align;
        offset += padding;
    }

    // Inherit parent's bindings, then let the local type override by method name.
    let mut bound_procs = parent_layout
        .map(|parent| parent.bound_procs.clone())
        .unwrap_or_default();
    for tbp in type_bound_procs {
            let target = tbp.binding.as_deref().unwrap_or(&tbp.name);
            let nopass = tbp.attrs.iter().any(|a| a.eq_ignore_ascii_case("nopass"));
            let target_name = if let Some(module_name) = host_module {
                format!(
                    "afs_modproc_{}_{}",
                    module_name.to_lowercase(),
                    target.to_lowercase()
                )
            } else {
                target.to_string()
            };
            let proc = BoundProc {
                method_name: tbp.name.clone(),
                target_name,
                abi_name: target.to_lowercase(),
                nopass,
            };
            if let Some(existing) = bound_procs
                .iter_mut()
                .find(|bp| bp.method_name.eq_ignore_ascii_case(&proc.method_name))
            {
                *existing = proc;
            } else {
                bound_procs.push(proc);
            }
    }

    let final_procs: Vec<String> = final_proc_names
        .iter()
        .map(|name| {
            if let Some(module_name) = host_module {
                format!(
                    "afs_modproc_{}_{}",
                    module_name.to_lowercase(),
                    name.to_lowercase()
                )
            } else {
                name.clone()
            }
        })
        .collect();

    TypeLayout {
        name: type_name.to_string(),
        size: offset,
        align: max_align,
        fields,
        bound_procs,
        final_procs,
        type_tag: 0, // assigned by registry after insertion
        parent: parent_layout.map(|p| p.name.clone()),
        is_abstract,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_of_basic_types() {
        assert_eq!(size_of_type(&TypeInfo::Integer { kind: Some(4) }), (4, 4));
        assert_eq!(size_of_type(&TypeInfo::Integer { kind: Some(8) }), (8, 8));
        assert_eq!(size_of_type(&TypeInfo::Real { kind: Some(4) }), (4, 4));
        assert_eq!(size_of_type(&TypeInfo::Real { kind: Some(8) }), (8, 8));
        assert_eq!(size_of_type(&TypeInfo::Logical { kind: Some(4) }), (4, 4));
        assert_eq!(
            size_of_type(&TypeInfo::Character {
                len: Some(10),
                kind: None
            }),
            (10, 1)
        );
        assert_eq!(size_of_type(&TypeInfo::Complex { kind: Some(4) }), (8, 4));
        assert_eq!(size_of_type(&TypeInfo::Complex { kind: Some(8) }), (16, 8));
    }

    #[test]
    fn layout_field_lookup() {
        let layout = TypeLayout {
            name: "point".into(),
            size: 8,
            align: 4,
            fields: vec![
                FieldLayout {
                    name: "x".into(),
                    offset: 0,
                    size: 4,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Real { kind: Some(4) },
                    allocatable: false,
                    pointer: false,
                    target: false,
                    default_init: None,
                },
                FieldLayout {
                    name: "y".into(),
                    offset: 4,
                    size: 4,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Real { kind: Some(4) },
                    allocatable: false,
                    pointer: false,
                    target: false,
                    default_init: None,
                },
            ],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        };
        assert_eq!(layout.field("x").unwrap().offset, 0);
        assert_eq!(layout.field("y").unwrap().offset, 4);
        assert_eq!(layout.field("X").unwrap().offset, 0); // case insensitive
        assert!(layout.field("z").is_none());
    }

    #[test]
    fn layout_mixed_types_padding() {
        // type :: mixed
        //   integer(1) :: a   ! 1 byte, offset 0
        //   real(8)    :: b   ! 8 bytes, offset 8 (padded to 8-byte alignment)
        //   integer(4) :: c   ! 4 bytes, offset 16
        // end type            ! total: 24 bytes (padded to 8-byte alignment)
        let layout = TypeLayout {
            name: "mixed".into(),
            size: 24,
            align: 8,
            fields: vec![
                FieldLayout {
                    name: "a".into(),
                    offset: 0,
                    size: 1,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Integer { kind: Some(1) },
                    allocatable: false,
                    pointer: false,
                    target: false,
                    default_init: None,
                },
                FieldLayout {
                    name: "b".into(),
                    offset: 8,
                    size: 8,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Real { kind: Some(8) },
                    allocatable: false,
                    pointer: false,
                    target: false,
                    default_init: None,
                },
                FieldLayout {
                    name: "c".into(),
                    offset: 16,
                    size: 4,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Integer { kind: Some(4) },
                    allocatable: false,
                    pointer: false,
                    target: false,
                    default_init: None,
                },
            ],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        };
        // Verify padding: a(1) + 7 pad + b(8) + c(4) + 4 pad = 24
        assert_eq!(layout.size, 24);
        assert_eq!(layout.align, 8);
        assert_eq!(layout.field("a").unwrap().offset, 0);
        assert_eq!(layout.field("b").unwrap().offset, 8);
        assert_eq!(layout.field("c").unwrap().offset, 16);
    }

    #[test]
    fn registry_lookup() {
        let mut reg = TypeLayoutRegistry::new();
        reg.insert(TypeLayout {
            name: "MyType".into(),
            size: 16,
            align: 8,
            fields: vec![],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        });
        assert!(reg.get("mytype").is_some()); // case insensitive
        assert!(reg.get("MYTYPE").is_some());
        assert!(reg.get("other").is_none());
    }

    /// Helper: build a component declaration for testing compute_layout.
    fn make_component(name: &str, ts: crate::ast::decl::TypeSpec) -> crate::ast::decl::SpannedDecl {
        use crate::ast::decl::*;
        use crate::ast::Spanned;
        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };
        Spanned::new(
            Decl::TypeDecl {
                type_spec: ts,
                attrs: vec![],
                entities: vec![EntityDecl {
                    name: name.to_string(),
                    array_spec: None,
                    char_len: None,
                    init: None,
                    ptr_init: None,
                }],
            },
            span,
        )
    }

    fn empty_params() -> std::collections::HashMap<String, i64> {
        std::collections::HashMap::new()
    }

    fn make_component_with_attrs(
        name: &str,
        ts: crate::ast::decl::TypeSpec,
        attrs: Vec<crate::ast::decl::Attribute>,
    ) -> crate::ast::decl::SpannedDecl {
        use crate::ast::decl::*;
        use crate::ast::Spanned;
        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };
        Spanned::new(
            Decl::TypeDecl {
                type_spec: ts,
                attrs,
                entities: vec![EntityDecl {
                    name: name.to_string(),
                    array_spec: None,
                    char_len: None,
                    init: None,
                    ptr_init: None,
                }],
            },
            span,
        )
    }

    fn make_component_with_init(
        name: &str,
        ts: crate::ast::decl::TypeSpec,
        init: crate::ast::expr::Expr,
    ) -> crate::ast::decl::SpannedDecl {
        use crate::ast::decl::*;
        use crate::ast::Spanned;
        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };
        Spanned::new(
            Decl::TypeDecl {
                type_spec: ts,
                attrs: vec![],
                entities: vec![EntityDecl {
                    name: name.to_string(),
                    array_spec: None,
                    char_len: None,
                    init: Some(Spanned::new(init, span)),
                    ptr_init: None,
                }],
            },
            span,
        )
    }

    #[test]
    fn compute_layout_simple_struct() {
        // type :: pair; integer :: x; real :: y; end type
        let reg = TypeLayoutRegistry::new();
        let components = vec![
            make_component("x", crate::ast::decl::TypeSpec::Integer(None)),
            make_component("y", crate::ast::decl::TypeSpec::Real(None)),
        ];
        let layout = compute_layout(
            "pair",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
        );
        assert_eq!(layout.name, "pair");
        assert_eq!(layout.size, 8); // 4 + 4, no padding needed
        assert_eq!(layout.align, 4);
        assert_eq!(layout.fields.len(), 2);
        assert_eq!(layout.field("x").unwrap().offset, 0);
        assert_eq!(layout.field("y").unwrap().offset, 4);
    }

    #[test]
    fn compute_layout_with_padding() {
        // type :: padded; integer(1) :: a; real(8) :: b; end type
        // a(1) + 7pad + b(8) = 16
        let reg = TypeLayoutRegistry::new();
        let components = vec![
            make_component(
                "a",
                crate::ast::decl::TypeSpec::Integer(Some(crate::ast::decl::KindSelector::Expr(
                    crate::ast::Spanned::new(
                        crate::ast::expr::Expr::IntegerLiteral {
                            text: "1".into(),
                            kind: None,
                        },
                        crate::lexer::Span {
                            start: crate::lexer::Position { line: 0, col: 0 },
                            end: crate::lexer::Position { line: 0, col: 0 },
                            file_id: 0,
                        },
                    ),
                ))),
            ),
            make_component("b", crate::ast::decl::TypeSpec::DoublePrecision),
        ];
        let layout = compute_layout(
            "padded",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
        );
        assert_eq!(layout.field("a").unwrap().offset, 0);
        assert_eq!(layout.field("a").unwrap().size, 1);
        assert_eq!(layout.field("b").unwrap().offset, 8); // padded to 8-byte alignment
        assert_eq!(layout.field("b").unwrap().size, 8);
        assert_eq!(layout.size, 16);
        assert_eq!(layout.align, 8);
    }

    #[test]
    fn compute_layout_with_extends() {
        // type :: base; integer :: x; end type
        // type, extends(base) :: child; real :: y; end type
        let reg = TypeLayoutRegistry::new();
        let base_comps = vec![make_component(
            "x",
            crate::ast::decl::TypeSpec::Integer(None),
        )];
        let base_layout = compute_layout(
            "base",
            None,
            &[],
            &[],
            &base_comps,
            None,
            &reg,
            &empty_params(),
        );
        assert_eq!(base_layout.size, 4);

        let child_comps = vec![make_component("y", crate::ast::decl::TypeSpec::Real(None))];
        let child_layout = compute_layout(
            "child",
            None,
            &[],
            &[],
            &child_comps,
            Some(&base_layout),
            &reg,
            &empty_params(),
        );
        assert_eq!(child_layout.fields.len(), 2); // x + y
        assert_eq!(child_layout.field("x").unwrap().offset, 0); // inherited
        assert_eq!(child_layout.field("y").unwrap().offset, 4); // appended
        assert_eq!(child_layout.size, 8);
    }

    #[test]
    fn compute_layout_scalar_derived_pointer_component_uses_pointer_slot() {
        let mut reg = TypeLayoutRegistry::new();
        reg.insert(TypeLayout {
            name: "node_t".into(),
            size: 16,
            align: 8,
            fields: vec![],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        });
        let components = vec![make_component_with_attrs(
            "left",
            crate::ast::decl::TypeSpec::Type("node_t".into()),
            vec![crate::ast::decl::Attribute::Pointer],
        )];

        let layout = compute_layout(
            "list_t",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
        );
        let field = layout.field("left").expect("missing left field");

        assert_eq!(field.size, 8);
        assert_eq!(field.offset, 0);
        assert!(field.pointer);
        assert_eq!(layout.size, 8);
    }

    #[test]
    fn compute_layout_deferred_char_array_component_with_dimension_attr_uses_array_descriptor() {
        use crate::ast::decl::{ArraySpec, Attribute, CharSelector, LenSpec, TypeSpec};

        let components = vec![make_component_with_attrs(
            "lines",
            TypeSpec::Character(Some(CharSelector {
                len: Some(LenSpec::Colon),
                kind: None,
            })),
            vec![
                Attribute::Allocatable,
                Attribute::Dimension(vec![ArraySpec::Deferred]),
            ],
        )];

        let reg = TypeLayoutRegistry::new();
        let layout = compute_layout(
            "item_t",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
        );
        let field = layout.field("lines").expect("missing lines field");

        assert_eq!(field.size, 384);
        assert!(field.allocatable);
        assert!(field.declared_array);
        assert!(matches!(
            field.type_info,
            TypeInfo::Character {
                len: None,
                kind: None
            }
        ));
    }

    #[test]
    fn compute_layout_captures_scalar_default_initializers() {
        let reg = TypeLayoutRegistry::new();
        let components = vec![
            make_component_with_init(
                "depth",
                crate::ast::decl::TypeSpec::Integer(None),
                crate::ast::expr::Expr::IntegerLiteral {
                    text: "7".into(),
                    kind: None,
                },
            ),
            make_component_with_init(
                "enabled",
                crate::ast::decl::TypeSpec::Logical(None),
                crate::ast::expr::Expr::LogicalLiteral {
                    value: true,
                    kind: None,
                },
            ),
            make_component_with_init(
                "tag",
                crate::ast::decl::TypeSpec::Character(Some(crate::ast::decl::CharSelector {
                    len: Some(crate::ast::decl::LenSpec::Expr(crate::ast::Spanned::new(
                        crate::ast::expr::Expr::IntegerLiteral {
                            text: "4".into(),
                            kind: None,
                        },
                        crate::lexer::Span {
                            start: crate::lexer::Position { line: 0, col: 0 },
                            end: crate::lexer::Position { line: 0, col: 0 },
                            file_id: 0,
                        },
                    ))),
                    kind: None,
                })),
                crate::ast::expr::Expr::StringLiteral {
                    value: "".into(),
                    kind: None,
                },
            ),
        ];

        let layout = compute_layout(
            "state_t",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
        );

        assert_eq!(
            layout
                .field("depth")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Integer(7))
        );
        assert_eq!(
            layout
                .field("enabled")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Logical(true))
        );
        assert_eq!(
            layout
                .field("tag")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Character(String::new()))
        );
    }

    #[test]
    fn compute_layout_captures_derived_component_constructor_defaults() {
        use crate::ast::decl::TypeSpec;
        use crate::ast::expr::{Argument, Expr, SectionSubscript};
        use crate::ast::Spanned;

        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };

        let nested_components = vec![
            make_component_with_init(
                "style",
                TypeSpec::Integer(None),
                Expr::IntegerLiteral {
                    text: "-1".into(),
                    kind: Some("i1".into()),
                },
            ),
            make_component_with_init(
                "bg",
                TypeSpec::Integer(None),
                Expr::IntegerLiteral {
                    text: "-1".into(),
                    kind: Some("i1".into()),
                },
            ),
            make_component_with_init(
                "fg",
                TypeSpec::Integer(None),
                Expr::IntegerLiteral {
                    text: "-1".into(),
                    kind: Some("i1".into()),
                },
            ),
        ];
        let mut reg = TypeLayoutRegistry::new();
        reg.insert(compute_layout(
            "color_code",
            None,
            &[],
            &[],
            &nested_components,
            None,
            &reg,
            &empty_params(),
        ));

        let components = vec![
            make_component_with_init(
                "bold",
                TypeSpec::Type("color_code".into()),
                Expr::FunctionCall {
                    callee: Box::new(Spanned::new(
                        Expr::Name {
                            name: "color_code".into(),
                        },
                        span,
                    )),
                    args: vec![Argument {
                        keyword: Some("style".into()),
                        value: SectionSubscript::Element(Spanned::new(
                            Expr::IntegerLiteral {
                                text: "1".into(),
                                kind: Some("i1".into()),
                            },
                            span,
                        )),
                    }],
                },
            ),
            make_component_with_init(
                "blue",
                TypeSpec::Type("color_code".into()),
                Expr::FunctionCall {
                    callee: Box::new(Spanned::new(
                        Expr::Name {
                            name: "color_code".into(),
                        },
                        span,
                    )),
                    args: vec![Argument {
                        keyword: Some("fg".into()),
                        value: SectionSubscript::Element(Spanned::new(
                            Expr::IntegerLiteral {
                                text: "4".into(),
                                kind: Some("i1".into()),
                            },
                            span,
                        )),
                    }],
                },
            ),
        ];

        let layout = compute_layout(
            "color_output",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
        );

        assert_eq!(
            layout
                .field("bold")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Derived(vec![(
                "style".into(),
                FieldDefaultInit::Integer(1),
            )]))
        );
        assert_eq!(
            layout
                .field("blue")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Derived(vec![(
                "fg".into(),
                FieldDefaultInit::Integer(4),
            )]))
        );
    }

    #[test]
    fn compute_layout_resolves_named_character_length_params() {
        use crate::ast::decl::{CharSelector, LenSpec, TypeSpec};
        use crate::ast::expr::Expr;
        use crate::ast::Spanned;

        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };
        let components = vec![make_component(
            "value",
            TypeSpec::Character(Some(CharSelector {
                len: Some(LenSpec::Expr(Spanned::new(
                    Expr::Name {
                        name: "MAX_TOKEN_LEN".into(),
                    },
                    span,
                ))),
                kind: None,
            })),
        )];
        let reg = TypeLayoutRegistry::new();
        let mut params = std::collections::HashMap::new();
        params.insert("max_token_len".into(), 8);

        let layout = compute_layout("token_t", None, &[], &[], &components, None, &reg, &params);
        let field = layout.field("value").expect("missing value field");

        assert_eq!(field.size, 8);
        assert!(matches!(
            field.type_info,
            TypeInfo::Character {
                len: Some(8),
                kind: None
            }
        ));
    }
}
