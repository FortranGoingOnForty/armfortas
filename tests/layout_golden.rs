//! Sprint x02 golden layout test.
//!
//! Dumps every size/alignment the frontend computes — the `TypeInfo`
//! matrix, the `IrType` table, and derived-type layouts over a fixture
//! corpus — and byte-compares against a committed golden. The golden is
//! cut BEFORE the TargetLayout refactor; if the dump moves, the
//! refactor changed behavior. Both supported arches are LP64 with
//! identical natural alignments, so one golden serves every target —
//! that coincidence is the tested claim.
//!
//! Regenerate with `AFS_UPDATE_GOLDEN=1 cargo test --test layout_golden`.
//! CI never sets it. Runs on every host: nothing here links binaries,
//! so there is no x01 skip gate.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use armfortas::ir::types::{FloatWidth, FuncSig, IntWidth, IrType};
use armfortas::lexer::{tokenize, SourceForm};
use armfortas::parser::Parser;
use armfortas::sema::symtab::TypeInfo;
use armfortas::sema::type_layout::{compute_layout_with_attrs, size_of_type, TypeLayoutRegistry};

/// Derived-type corpus exercising every compute_layout path: padding,
/// EXTENDS, allocatable array component (array descriptor), deferred-
/// length character component (string descriptor), scalar pointer
/// component, procedure-pointer component, nesting, complex components.
const CORPUS: &str = r#"
module layout_corpus
  implicit none

  abstract interface
    subroutine cb_iface()
    end subroutine cb_iface
  end interface

  type :: simple
    integer :: a
    real(8) :: b
  end type simple

  type :: padded
    integer(1) :: a
    real(8) :: b
    integer(2) :: c
  end type padded

  type :: with_alloc_array
    real, allocatable :: arr(:)
    integer(1) :: tail
  end type with_alloc_array

  type :: with_defchar
    character(len=:), allocatable :: s
    integer(1) :: tail
  end type with_defchar

  type :: with_ptr_scalar
    integer, pointer :: p
    integer(1) :: tail
  end type with_ptr_scalar

  type :: with_procptr
    procedure(cb_iface), pointer, nopass :: f
    integer(1) :: tail
  end type with_procptr

  type :: base
    integer :: tagval
  end type base

  type, extends(base) :: child
    real :: extra
  end type child

  type :: nest_inner
    integer(2) :: small
    real(8) :: wide
  end type nest_inner

  type :: nest_outer
    type(nest_inner) :: inner
    integer(1) :: tail
  end type nest_outer

  type :: chars_and_complex
    character(len=12) :: name
    complex :: z4
    complex(8) :: z8
    logical(1) :: flag
  end type chars_and_complex
end module layout_corpus
"#;

fn type_info_matrix() -> Vec<(String, TypeInfo)> {
    let mut rows = Vec::new();
    for kind in [None, Some(1u8), Some(2), Some(4), Some(8), Some(16)] {
        rows.push((
            format!("integer(kind={:?})", kind),
            TypeInfo::Integer { kind },
        ));
    }
    for kind in [None, Some(4u8), Some(8)] {
        rows.push((format!("real(kind={:?})", kind), TypeInfo::Real { kind }));
    }
    rows.push(("double_precision".to_string(), TypeInfo::DoublePrecision));
    for kind in [None, Some(4u8), Some(8)] {
        rows.push((
            format!("complex(kind={:?})", kind),
            TypeInfo::Complex { kind },
        ));
    }
    for kind in [None, Some(1u8), Some(2), Some(4), Some(8)] {
        rows.push((
            format!("logical(kind={:?})", kind),
            TypeInfo::Logical { kind },
        ));
    }
    for len in [Some(1i64), Some(12), None] {
        rows.push((
            format!("character(len={:?})", len),
            TypeInfo::Character { len, kind: None },
        ));
    }
    rows.push((
        "derived(simple)".to_string(),
        TypeInfo::Derived("simple".to_string()),
    ));
    rows.push((
        "class(base)".to_string(),
        TypeInfo::Class("base".to_string()),
    ));
    rows.push(("class(*)".to_string(), TypeInfo::ClassStar));
    rows.push(("type(*)".to_string(), TypeInfo::TypeStar));
    rows
}

fn ir_type_table() -> Vec<(String, IrType)> {
    let mut rows: Vec<(String, IrType)> = Vec::new();
    rows.push(("void".into(), IrType::Void));
    rows.push(("bool".into(), IrType::Bool));
    for w in [
        IntWidth::I8,
        IntWidth::I16,
        IntWidth::I32,
        IntWidth::I64,
        IntWidth::I128,
    ] {
        rows.push((format!("i{}", w.bits()), IrType::Int(w)));
    }
    for w in [FloatWidth::F32, FloatWidth::F64] {
        rows.push((format!("f{}", w.bits()), IrType::Float(w)));
    }
    rows.push(("ptr(i64)".into(), IrType::Ptr(Box::new(IrType::Int(IntWidth::I64)))));
    rows.push((
        "funcptr".into(),
        IrType::FuncPtr(Box::new(FuncSig {
            params: vec![IrType::Int(IntWidth::I32)],
            ret: IrType::Void,
        })),
    ));
    rows.push((
        "array[i32;10]".into(),
        IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 10),
    ));
    rows.push((
        "array[f64;3]".into(),
        IrType::Array(Box::new(IrType::Float(FloatWidth::F64)), 3),
    ));
    // Every verifier-legal vector shape (all 128-bit).
    for (lanes, elem, name) in [
        (16u8, IrType::Int(IntWidth::I8), "v16i8"),
        (8, IrType::Int(IntWidth::I16), "v8i16"),
        (4, IrType::Int(IntWidth::I32), "v4i32"),
        (2, IrType::Int(IntWidth::I64), "v2i64"),
        (4, IrType::Float(FloatWidth::F32), "v4f32"),
        (2, IrType::Float(FloatWidth::F64), "v2f64"),
    ] {
        rows.push((
            name.to_string(),
            IrType::Vector {
                lanes,
                elem: Box::new(elem),
            },
        ));
    }
    rows
}

fn generate_dump() -> String {
    // The kind defaults are thread-local state consulted by
    // size_of_type; reset so a sibling test's -fdefault-integer-8 run
    // cannot poison the dump (x02 pitfall).
    armfortas::driver::defaults::reset();

    let mut out = String::new();

    out.push_str("[type_info]\n");
    for (name, ti) in type_info_matrix() {
        let (size, align) = size_of_type(&ti);
        writeln!(out, "{}\t{}\t{}", name, size, align).unwrap();
    }

    out.push_str("\n[ir_type]\n");
    for (name, ty) in ir_type_table() {
        writeln!(out, "{}\t{}", name, ty.size_bytes()).unwrap();
    }

    out.push_str("\n[derived]\n");
    let tokens = tokenize(CORPUS, 0, SourceForm::FreeForm).expect("corpus must lex");
    let mut parser = Parser::new(&tokens);
    let units = parser.parse_file().expect("corpus must parse");
    let mut registry = TypeLayoutRegistry::new();
    let empty_params: HashMap<String, i64> = HashMap::new();
    let empty_inits = HashMap::new();
    let mut dumped = 0usize;
    for unit in &units {
        let decls = match &unit.node {
            armfortas::ast::unit::ProgramUnit::Module { decls, .. } => decls,
            _ => continue,
        };
        for decl in decls {
            if let armfortas::ast::decl::Decl::DerivedTypeDef {
                name,
                extends,
                attrs,
                components,
                type_bound_procs,
                final_procs,
                ..
            } = &decl.node
            {
                let parent = extends.as_ref().and_then(|p| registry.get(p)).cloned();
                let is_abstract = attrs
                    .iter()
                    .any(|attr| matches!(attr, armfortas::ast::decl::TypeAttr::Abstract));
                let layout = compute_layout_with_attrs(
                    name,
                    Some("layout_corpus"),
                    type_bound_procs,
                    final_procs,
                    components,
                    parent.as_ref(),
                    is_abstract,
                    &registry,
                    &empty_params,
                    &empty_inits,
                );
                writeln!(out, "{}\t{}\t{}", layout.name, layout.size, layout.align).unwrap();
                for field in &layout.fields {
                    writeln!(out, "  {}\t{}\t{}", field.name, field.offset, field.size).unwrap();
                }
                registry.insert(layout);
                dumped += 1;
            }
        }
    }
    assert!(
        dumped >= 10,
        "derived corpus shrank ({} types) — fixture parse silently lost types",
        dumped
    );

    out
}

fn golden_path() -> PathBuf {
    for base in ["tests/fixtures/layout", "../tests/fixtures/layout"] {
        let dir = PathBuf::from(base);
        if dir.parent().map(|p| p.exists()).unwrap_or(false) {
            return dir.join("lp64.golden");
        }
    }
    PathBuf::from("tests/fixtures/layout/lp64.golden")
}

#[test]
fn layout_dump_matches_golden() {
    let dump = generate_dump();
    let path = golden_path();

    if std::env::var_os("AFS_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &dump).unwrap();
        eprintln!("layout golden regenerated at {}", path.display());
        return;
    }

    let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} ({}); run AFS_UPDATE_GOLDEN=1 cargo test --test layout_golden",
            path.display(),
            e
        )
    });
    assert_eq!(
        dump, golden,
        "layout dump diverged from the committed golden — a layout-affecting change escaped"
    );
}
