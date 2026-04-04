//! Statement AST nodes.
//!
//! Control flow, assignments, calls, and all executable Fortran statements.

use super::Spanned;
use super::expr::SpannedExpr;
use super::decl::SpannedDecl;

/// A spanned statement.
pub type SpannedStmt = Spanned<Stmt>;

/// A Fortran executable statement.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
#[allow(clippy::enum_variant_names)]
pub enum Stmt {
    // ---- Assignment ----
    Assignment { target: SpannedExpr, value: SpannedExpr },
    PointerAssignment { target: SpannedExpr, value: SpannedExpr },

    // ---- IF ----
    IfConstruct {
        name: Option<String>,
        condition: SpannedExpr,
        then_body: Vec<SpannedStmt>,
        else_ifs: Vec<(SpannedExpr, Vec<SpannedStmt>)>,
        else_body: Option<Vec<SpannedStmt>>,
    },
    IfStmt { condition: SpannedExpr, action: Box<SpannedStmt> },

    // ---- DO loops ----
    DoLoop {
        name: Option<String>,
        var: Option<String>,
        start: Option<SpannedExpr>,
        end: Option<SpannedExpr>,
        step: Option<SpannedExpr>,
        body: Vec<SpannedStmt>,
    },
    DoWhile {
        name: Option<String>,
        condition: SpannedExpr,
        body: Vec<SpannedStmt>,
    },
    DoConcurrent {
        name: Option<String>,
        controls: Vec<ConcurrentControl>,
        mask: Option<SpannedExpr>,
        body: Vec<SpannedStmt>,
    },

    // ---- SELECT CASE ----
    SelectCase {
        name: Option<String>,
        selector: SpannedExpr,
        cases: Vec<CaseBlock>,
    },

    // ---- WHERE / FORALL ----
    WhereConstruct {
        name: Option<String>,
        mask: SpannedExpr,
        body: Vec<SpannedStmt>,
        elsewhere: Vec<(Option<SpannedExpr>, Vec<SpannedStmt>)>,
    },
    WhereStmt { mask: SpannedExpr, stmt: Box<SpannedStmt> },
    ForallConstruct {
        name: Option<String>,
        specs: Vec<ForallSpec>,
        mask: Option<SpannedExpr>,
        body: Vec<SpannedStmt>,
    },
    ForallStmt { specs: Vec<ForallSpec>, mask: Option<SpannedExpr>, stmt: Box<SpannedStmt> },

    // ---- BLOCK / ASSOCIATE ----
    Block { name: Option<String>, body: Vec<SpannedStmt> },
    Associate { name: Option<String>, assocs: Vec<(String, SpannedExpr)>, body: Vec<SpannedStmt> },

    // ---- Branch/transfer ----
    Exit { name: Option<String> },
    Cycle { name: Option<String> },
    Stop { code: Option<SpannedExpr>, quiet: bool },
    ErrorStop { code: Option<SpannedExpr>, quiet: bool },
    Return { value: Option<SpannedExpr> },
    Goto { label: u64 },
    ComputedGoto { labels: Vec<u64>, selector: SpannedExpr },
    ArithmeticIf { expr: SpannedExpr, neg: u64, zero: u64, pos: u64 },

    // ---- Other executable ----
    Continue { label: Option<u64> },
    Call { callee: SpannedExpr, args: Vec<crate::ast::expr::Argument> },
    Print { format: SpannedExpr, items: Vec<SpannedExpr> },

    // ---- Declaration (embedded in statement context) ----
    Declaration(SpannedDecl),
}

// ---- Supporting types ----

/// A CASE block: case selector + body.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseBlock {
    pub selectors: Vec<CaseSelector>,
    pub body: Vec<SpannedStmt>,
}

/// A CASE selector: single value, range, or default.
#[derive(Debug, Clone, PartialEq)]
pub enum CaseSelector {
    Value(SpannedExpr),
    Range { low: Option<SpannedExpr>, high: Option<SpannedExpr> },
    Default,
}

/// DO CONCURRENT control: `i = 1:n`
#[derive(Debug, Clone, PartialEq)]
pub struct ConcurrentControl {
    pub var: String,
    pub start: SpannedExpr,
    pub end: SpannedExpr,
    pub step: Option<SpannedExpr>,
}

/// FORALL specification: `i = 1:n:step`
#[derive(Debug, Clone, PartialEq)]
pub struct ForallSpec {
    pub var: String,
    pub start: SpannedExpr,
    pub end: SpannedExpr,
    pub step: Option<SpannedExpr>,
}
