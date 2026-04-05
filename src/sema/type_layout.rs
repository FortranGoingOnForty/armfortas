//! Derived type memory layout computation.
//!
//! Computes field offsets, alignment, and total size for derived types
//! using natural alignment rules (same as C struct layout on ARM64).

use std::collections::HashMap;
use super::symtab::TypeInfo;

/// Layout of a single field in a derived type.
#[derive(Debug, Clone)]
pub struct FieldLayout {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub type_info: TypeInfo,
}

/// Complete layout of a derived type.
#[derive(Debug, Clone)]
pub struct TypeLayout {
    pub name: String,
    pub size: usize,
    pub align: usize,
    pub fields: Vec<FieldLayout>,
}

impl TypeLayout {
    /// Look up a field by name and return its layout.
    pub fn field(&self, name: &str) -> Option<&FieldLayout> {
        let key = name.to_lowercase();
        self.fields.iter().find(|f| f.name.to_lowercase() == key)
    }
}

/// Registry of all computed type layouts.
#[derive(Debug, Default)]
pub struct TypeLayoutRegistry {
    pub layouts: HashMap<String, TypeLayout>,
}

impl TypeLayoutRegistry {
    pub fn new() -> Self {
        Self { layouts: HashMap::new() }
    }

    pub fn get(&self, type_name: &str) -> Option<&TypeLayout> {
        self.layouts.get(&type_name.to_lowercase())
    }

    pub fn insert(&mut self, layout: TypeLayout) {
        self.layouts.insert(layout.name.to_lowercase(), layout);
    }
}

/// Compute the size and alignment of a Fortran type on ARM64.
pub fn size_of_type(ti: &TypeInfo) -> (usize, usize) {
    match ti {
        TypeInfo::Integer { kind } => {
            let k = kind.unwrap_or(4) as usize;
            (k, k)
        }
        TypeInfo::Real { kind } => {
            let k = kind.unwrap_or(4) as usize;
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

/// Convert a TypeSpec AST node to TypeInfo for layout computation.
fn type_spec_to_type_info(ts: &crate::ast::decl::TypeSpec) -> TypeInfo {
    use crate::ast::decl::{TypeSpec, KindSelector, LenSpec};

    fn kind_to_u8(ks: &Option<KindSelector>) -> Option<u8> {
        ks.as_ref().and_then(|k| match k {
            KindSelector::Expr(e) | KindSelector::Star(e) => {
                if let crate::ast::expr::Expr::IntegerLiteral { text, .. } = &e.node {
                    text.parse::<u8>().ok()
                } else { None }
            }
        })
    }

    match ts {
        TypeSpec::Integer(kind) => TypeInfo::Integer { kind: kind_to_u8(kind) },
        TypeSpec::Real(kind) => TypeInfo::Real { kind: kind_to_u8(kind) },
        TypeSpec::DoublePrecision => TypeInfo::DoublePrecision,
        TypeSpec::Complex(kind) => TypeInfo::Complex { kind: kind_to_u8(kind) },
        TypeSpec::DoubleComplex => TypeInfo::Complex { kind: Some(8) },
        TypeSpec::Logical(kind) => TypeInfo::Logical { kind: kind_to_u8(kind) },
        TypeSpec::Character(sel) => {
            let len = sel.as_ref().and_then(|s| s.len.as_ref()).and_then(|l| match l {
                LenSpec::Expr(e) => {
                    if let crate::ast::expr::Expr::IntegerLiteral { text, .. } = &e.node {
                        text.parse::<i64>().ok()
                    } else { None }
                }
                _ => None,
            });
            let kind = sel.as_ref().and_then(|s| s.kind.as_ref()).and_then(|e| {
                if let crate::ast::expr::Expr::IntegerLiteral { text, .. } = &e.node {
                    text.parse::<u8>().ok()
                } else { None }
            });
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
    components: &[crate::ast::decl::SpannedDecl],
    parent_layout: Option<&TypeLayout>,
    registry: &TypeLayoutRegistry,
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
        if let crate::ast::decl::Decl::TypeDecl { type_spec, entities, .. } = &decl.node {
            let ti = type_spec_to_type_info(type_spec);
            let (elem_size, elem_align) = if let TypeInfo::Derived(ref dname) = ti {
                registry.get(dname)
                    .map(|l| (l.size, l.align))
                    .unwrap_or((8, 8))
            } else {
                size_of_type(&ti)
            };

            for entity in entities {
                // Pad to alignment.
                let padding = (elem_align - (offset % elem_align)) % elem_align;
                offset += padding;
                max_align = max_align.max(elem_align);

                fields.push(FieldLayout {
                    name: entity.name.clone(),
                    offset,
                    size: elem_size,
                    type_info: ti.clone(),
                });
                offset += elem_size;
            }
        }
    }

    // Pad total size to struct alignment.
    if max_align > 0 {
        let padding = (max_align - (offset % max_align)) % max_align;
        offset += padding;
    }

    TypeLayout {
        name: type_name.to_string(),
        size: offset,
        align: max_align,
        fields,
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
        assert_eq!(size_of_type(&TypeInfo::Character { len: Some(10), kind: None }), (10, 1));
        assert_eq!(size_of_type(&TypeInfo::Complex { kind: Some(4) }), (8, 4));
        assert_eq!(size_of_type(&TypeInfo::Complex { kind: Some(8) }), (16, 8));
    }
}
