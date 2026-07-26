//! Declaration AST nodes.
//!
//! Types, attributes, entity declarations, USE statements, IMPLICIT,
//! derived type definitions, and legacy declaration forms.

use super::expr::SpannedExpr;
use super::Spanned;

/// A spanned declaration.
pub type SpannedDecl = Spanned<Decl>;

/// A Fortran declaration.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum Decl {
    /// Type declaration: `integer, allocatable :: x(:), y`
    TypeDecl {
        type_spec: TypeSpec,
        attrs: Vec<Attribute>,
        entities: Vec<EntityDecl>,
    },

    /// Standalone access statement. At module scope this sets the module's
    /// default accessibility; inside a derived-type component stream a
    /// `PRIVATE` record is the type's private-components statement.
    AccessDefault { access: Attribute },

    /// `PUBLIC :: name1, name2` or `PRIVATE :: assignment(=)` — sets
    /// access on specific names or generic specs.
    AccessList {
        access: Attribute,
        names: Vec<String>,
    },

    /// `implicit none` or `implicit none(type, external)`
    ImplicitNone { external: bool, type_: bool },

    /// `implicit double precision (a-h, o-z)`
    ImplicitStmt { specs: Vec<ImplicitSpec> },

    /// Derived type definition
    DerivedTypeDef {
        name: String,
        extends: Option<String>,
        attrs: Vec<TypeAttr>,
        components: Vec<SpannedDecl>,
        type_bound_procs: Vec<TypeBoundProc>,
        final_procs: Vec<String>,
    },

    /// `use module_name [, only: ...]`
    UseStmt {
        module: String,
        nature: UseNature,
        renames: Vec<Rename>,
        only: Option<Vec<OnlyItem>>,
    },

    /// `parameter (pi = 3.14159, e = 2.71828)`
    ParameterStmt { pairs: Vec<(String, SpannedExpr)> },

    /// `common /block_name/ x, y, z`
    CommonBlock {
        name: Option<String>,
        vars: Vec<String>,
    },

    /// `equivalence (a, b), (c, d)`
    EquivalenceStmt { groups: Vec<Vec<SpannedExpr>> },

    /// `data x /1.0/, y /2.0/`
    DataStmt { sets: Vec<DataSet> },

    /// `enum, bind(c) [:: name]` — interoperable enumeration
    /// (F2023 R760 adds the optional enum-type-name).
    EnumDef {
        type_name: Option<String>,
        enumerators: Vec<(String, Option<SpannedExpr>)>,
    },

    /// F2023 `enumeration type :: name` (R766): a distinct
    /// nonintrinsic, non-interoperable type. Enumerators carry no
    /// values — declaration order defines 1-based ordinals.
    EnumerationTypeDef {
        name: String,
        enumerators: Vec<String>,
    },

    /// Standalone attribute statement: `allocatable :: x`, `dimension(10) :: a`
    AttributeStmt {
        attr: Attribute,
        entities: Vec<String>,
    },
}

// ---- Type specifiers ----

/// Fortran type specifier.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeSpec {
    Integer(Option<KindSelector>),
    Real(Option<KindSelector>),
    DoublePrecision,
    Complex(Option<KindSelector>),
    DoubleComplex,
    Logical(Option<KindSelector>),
    Character(Option<CharSelector>),
    /// `type(my_type)` — derived type reference
    Type(String),
    /// `class(my_type)` — polymorphic
    Class(String),
    /// `class(*)` — unlimited polymorphic
    ClassStar,
    /// `type(*)` — assumed type
    TypeStar,
    /// F2023 `typeof(entity)` — the declared type of a previously
    /// declared entity.
    TypeOf(String),
    /// F2023 `classof(entity)` — polymorphic with the entity's
    /// declared type as base.
    ClassOf(String),
}

/// Kind selector: `(4)`, `(kind=4)`, `*4`
#[derive(Debug, Clone, PartialEq)]
pub enum KindSelector {
    Expr(SpannedExpr),
    Star(SpannedExpr), // *4 (old-style)
}

/// Character selector: `(10)`, `(len=10, kind=1)`, `*10`
#[derive(Debug, Clone, PartialEq)]
pub struct CharSelector {
    pub len: Option<LenSpec>,
    pub kind: Option<SpannedExpr>,
}

/// Character length specification.
#[derive(Debug, Clone, PartialEq)]
pub enum LenSpec {
    Expr(SpannedExpr),
    Star,  // len=* (assumed length)
    Colon, // len=: (deferred length)
}

// ---- Attributes ----

/// Declaration attribute.
#[derive(Debug, Clone, PartialEq)]
pub enum Attribute {
    Dimension(Vec<ArraySpec>),
    /// `RANK(n)` (F2023 8.5.17). Kept as a conformance marker for
    /// sema (`--std` gating, C-constraint checks); the parser also
    /// appends the desugared `Dimension` so lowering sees a normal
    /// deferred/assumed-shape array.
    Rank(u8),
    Allocatable,
    Pointer,
    Target,
    Intent(Intent),
    Optional,
    Save,
    Parameter,
    Value,
    Volatile,
    Asynchronous,
    Protected,
    Contiguous,
    /// Internal marker for a `PROCEDURE(interface)` entity. The parser
    /// represents procedure entities as `TypeDecl` nodes so existing
    /// declaration/lowering paths can retain the interface name in
    /// `TypeSpec::Type`; this marker distinguishes that encoding from a
    /// genuine `TYPE(name), EXTERNAL` function declaration.
    Procedure,
    External,
    NoPass,
    Intrinsic,
    Bind(Option<String>), // bind(c, name="cfunc")
    Public,
    Private,
}

/// Intent specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    In,
    Out,
    InOut,
}

/// Array specification for dimension attribute.
#[derive(Debug, Clone, PartialEq)]
pub enum ArraySpec {
    /// `(10)` or `(1:10)` — explicit bounds
    Explicit {
        lower: Option<SpannedExpr>,
        upper: SpannedExpr,
    },
    /// `(:)` — assumed shape (for dummy arguments)
    AssumedShape { lower: Option<SpannedExpr> },
    /// `(*)` — assumed size (last dimension only)
    AssumedSize { lower: Option<SpannedExpr> },
    /// `(:)` with allocatable/pointer — deferred shape
    Deferred,
    /// `(..)` — assumed rank (F2018)
    AssumedRank,
}

// ---- Entity declarations ----

/// An entity in a type declaration: `x`, `x = 0`, `x(:,:)`, `ptr => null()`
#[derive(Debug, Clone, PartialEq)]
pub struct EntityDecl {
    pub name: String,
    pub array_spec: Option<Vec<ArraySpec>>,
    pub char_len: Option<LenSpec>, // character entity-specific length
    pub init: Option<SpannedExpr>, // = expr
    pub ptr_init: Option<SpannedExpr>, // => expr
}

// ---- Derived type parts ----

/// Derived type attribute.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeAttr {
    Public,
    Private,
    Abstract,
    Bind(Option<String>),
    Extends(String),
}

/// Type-bound procedure.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeBoundProc {
    pub name: String,
    pub interface: Option<String>, // procedure(iface) :: name
    pub binding: Option<String>,   // procedure :: name => binding
    pub bindings: Vec<String>,     // generic :: name => specific_a, specific_b
    pub attrs: Vec<String>,        // pass, nopass, deferred, etc.
    pub is_generic: bool,
}

// ---- USE statement parts ----

/// USE module nature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseNature {
    Normal,
    Intrinsic,
    NonIntrinsic,
}

/// USE rename: `local => remote`
#[derive(Debug, Clone, PartialEq)]
pub struct Rename {
    pub local: String,
    pub remote: String,
}

/// USE ONLY item.
#[derive(Debug, Clone, PartialEq)]
pub enum OnlyItem {
    Name(String),
    Rename(Rename),
    Generic(String),
}

// ---- IMPLICIT parts ----

/// Implicit type specification: `double precision (a-h, o-z)`
#[derive(Debug, Clone, PartialEq)]
pub struct ImplicitSpec {
    pub type_spec: TypeSpec,
    pub ranges: Vec<(char, char)>,
}

// ---- DATA statement parts ----

/// A data set: `x /1.0/` or `(a(i), i=1,10) /10*0.0/`
#[derive(Debug, Clone, PartialEq)]
pub struct DataSet {
    pub objects: Vec<SpannedExpr>,
    pub values: Vec<DataValue>,
}

/// DATA initializer item.
#[derive(Debug, Clone, PartialEq)]
pub enum DataValue {
    Expr(SpannedExpr),
    Repeat {
        count: SpannedExpr,
        value: SpannedExpr,
    },
}
