//! Program unit AST nodes.
//!
//! Top-level compilation units: programs, modules, submodules,
//! subroutines, functions, block data, and interface blocks.

use super::decl::{SpannedDecl, TypeSpec};
use super::stmt::SpannedStmt;
use super::Spanned;

/// A spanned program unit.
pub type SpannedUnit = Spanned<ProgramUnit>;

/// A Fortran program unit — the top-level organizational structure.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ProgramUnit {
    Program {
        name: Option<String>,
        uses: Vec<SpannedDecl>,
        imports: Vec<ImportStmt>,
        implicit: Vec<SpannedDecl>,
        decls: Vec<SpannedDecl>,
        body: Vec<SpannedStmt>,
        contains: Vec<SpannedUnit>,
    },

    Module {
        name: String,
        uses: Vec<SpannedDecl>,
        imports: Vec<ImportStmt>,
        implicit: Vec<SpannedDecl>,
        decls: Vec<SpannedDecl>,
        contains: Vec<SpannedUnit>,
    },

    Submodule {
        parent: String,
        ancestor: Option<String>,
        name: String,
        uses: Vec<SpannedDecl>,
        imports: Vec<ImportStmt>,
        implicit: Vec<SpannedDecl>,
        decls: Vec<SpannedDecl>,
        contains: Vec<SpannedUnit>,
    },

    Subroutine {
        name: String,
        args: Vec<DummyArg>,
        bind: Option<BindInfo>,
        prefix: Vec<Prefix>,
        uses: Vec<SpannedDecl>,
        imports: Vec<ImportStmt>,
        implicit: Vec<SpannedDecl>,
        decls: Vec<SpannedDecl>,
        body: Vec<SpannedStmt>,
        contains: Vec<SpannedUnit>,
    },

    Function {
        name: String,
        args: Vec<DummyArg>,
        result: Option<String>,
        return_type: Option<TypeSpec>,
        bind: Option<BindInfo>,
        prefix: Vec<Prefix>,
        uses: Vec<SpannedDecl>,
        imports: Vec<ImportStmt>,
        implicit: Vec<SpannedDecl>,
        decls: Vec<SpannedDecl>,
        body: Vec<SpannedStmt>,
        contains: Vec<SpannedUnit>,
    },

    BlockData {
        name: Option<String>,
        uses: Vec<SpannedDecl>,
        decls: Vec<SpannedDecl>,
    },

    /// An interface block (explicit, generic, abstract).
    InterfaceBlock {
        name: Option<String>,
        is_abstract: bool,
        bodies: Vec<InterfaceBody>,
    },
}

/// Subprogram prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    Pure,
    Impure,
    Elemental,
    Recursive,
    NonRecursive,
    Module,
}

/// A dummy argument in a subprogram.
#[derive(Debug, Clone, PartialEq)]
pub enum DummyArg {
    Name(String),
    Star, // alternate return: *
}

/// BIND(C) specification.
#[derive(Debug, Clone, PartialEq)]
pub struct BindInfo {
    pub name: Option<String>, // BIND(C, NAME="cname") — None if just BIND(C)
}

/// Body of an interface block — either a subprogram interface or a module procedure name.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum InterfaceBody {
    Subprogram(SpannedUnit),
    ModuleProcedure(Vec<String>),
}

/// IMPORT statement.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportStmt {
    Default(Vec<String>), // import :: name1, name2
    All,                  // import, all
    None,                 // import, none
    Only(Vec<String>),    // import, only: name1, name2
}
