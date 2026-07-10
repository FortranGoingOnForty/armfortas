//! Statement AST nodes.
//!
//! Control flow, assignments, calls, and all executable Fortran statements.

use super::decl::SpannedDecl;
use super::expr::SpannedExpr;
use super::Spanned;

/// A spanned statement.
pub type SpannedStmt = Spanned<Stmt>;

/// A Fortran executable statement.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
#[allow(clippy::enum_variant_names)]
pub enum Stmt {
    // ---- Assignment ----
    Assignment {
        target: SpannedExpr,
        value: SpannedExpr,
    },
    PointerAssignment {
        target: SpannedExpr,
        value: SpannedExpr,
    },

    // ---- IF ----
    IfConstruct {
        name: Option<String>,
        condition: SpannedExpr,
        then_body: Vec<SpannedStmt>,
        else_ifs: Vec<(SpannedExpr, Vec<SpannedStmt>)>,
        else_body: Option<Vec<SpannedStmt>>,
    },
    IfStmt {
        condition: SpannedExpr,
        action: Box<SpannedStmt>,
    },

    // ---- DO loops ----
    DoLoop {
        name: Option<String>,
        var: Option<String>,
        start: Option<SpannedExpr>,
        end: Option<SpannedExpr>,
        step: Option<SpannedExpr>,
        body: Vec<SpannedStmt>,
        shared_terminating_label: bool,
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
        locality: Vec<LocalitySpec>,
        body: Vec<SpannedStmt>,
    },

    // ---- SELECT CASE ----
    SelectCase {
        name: Option<String>,
        selector: SpannedExpr,
        cases: Vec<CaseBlock>,
    },

    // ---- SELECT TYPE ----
    SelectType {
        name: Option<String>,
        selector: SpannedExpr,
        assoc_name: Option<String>, // SELECT TYPE (assoc => expr)
        guards: Vec<TypeGuard>,
    },

    // ---- SELECT RANK (F2018) ----
    SelectRank {
        name: Option<String>,
        selector: SpannedExpr,
        assoc_name: Option<String>, // SELECT RANK (assoc => expr)
        guards: Vec<RankGuard>,
    },

    // ---- WHERE / FORALL ----
    WhereConstruct {
        name: Option<String>,
        mask: SpannedExpr,
        body: Vec<SpannedStmt>,
        elsewhere: Vec<(Option<SpannedExpr>, Vec<SpannedStmt>)>,
    },
    WhereStmt {
        mask: SpannedExpr,
        stmt: Box<SpannedStmt>,
    },
    ForallConstruct {
        name: Option<String>,
        specs: Vec<ForallSpec>,
        mask: Option<SpannedExpr>,
        body: Vec<SpannedStmt>,
    },
    ForallStmt {
        specs: Vec<ForallSpec>,
        mask: Option<SpannedExpr>,
        stmt: Box<SpannedStmt>,
    },

    // ---- BLOCK / ASSOCIATE ----
    Block {
        name: Option<String>,
        /// USE statements declared inside the BLOCK specification part.
        /// These imports are scoped to the block body and do not leak
        /// into the enclosing procedure.
        uses: Vec<super::decl::SpannedDecl>,
        /// Interface blocks declared inside the BLOCK specification part.
        /// They are only visible within the block body.
        ifaces: Vec<super::unit::SpannedUnit>,
        /// IMPLICIT statements declared inside the BLOCK.  F2018 §11.1.4
        /// gives a BLOCK its own implicit-type rule environment that does
        /// not leak out and is independent of the enclosing scope's
        /// IMPLICIT NONE state.
        implicit: Vec<super::decl::SpannedDecl>,
        decls: Vec<super::decl::SpannedDecl>,
        body: Vec<SpannedStmt>,
    },
    Associate {
        name: Option<String>,
        assocs: Vec<(String, SpannedExpr)>,
        body: Vec<SpannedStmt>,
    },

    // ---- Branch/transfer ----
    Exit {
        name: Option<String>,
    },
    Cycle {
        name: Option<String>,
    },
    Stop {
        code: Option<SpannedExpr>,
        quiet: bool,
    },
    ErrorStop {
        code: Option<SpannedExpr>,
        quiet: bool,
    },
    Return {
        value: Option<SpannedExpr>,
    },
    Goto {
        label: u64,
    },
    ComputedGoto {
        labels: Vec<u64>,
        selector: SpannedExpr,
    },
    ArithmeticIf {
        expr: SpannedExpr,
        neg: u64,
        zero: u64,
        pos: u64,
    },
    /// Statement label on any executable statement: `10 i = i + 1`.
    /// The parser emits this when it sees an integer literal at statement start.
    Labeled {
        label: u64,
        stmt: Box<SpannedStmt>,
    },

    // ---- I/O ----
    Write {
        controls: Vec<IoControl>,
        items: Vec<SpannedExpr>,
    },
    Read {
        controls: Vec<IoControl>,
        items: Vec<SpannedExpr>,
    },
    Open {
        specs: Vec<IoControl>,
    },
    Close {
        specs: Vec<IoControl>,
    },
    Inquire {
        specs: Vec<IoControl>,
        items: Vec<SpannedExpr>,
    },
    Rewind {
        specs: Vec<IoControl>,
    },
    Backspace {
        specs: Vec<IoControl>,
    },
    Endfile {
        specs: Vec<IoControl>,
    },
    Flush {
        specs: Vec<IoControl>,
    },
    Wait {
        specs: Vec<IoControl>,
    },

    // ---- Memory ----
    Allocate {
        type_spec: Option<super::decl::TypeSpec>,
        items: Vec<SpannedExpr>,
        opts: Vec<IoControl>,
    },
    Deallocate {
        items: Vec<SpannedExpr>,
        opts: Vec<IoControl>,
    },
    Nullify {
        items: Vec<SpannedExpr>,
    },

    // ---- Other executable ----
    Continue {
        label: Option<u64>,
    },
    Format {
        spec: String,
    },
    Call {
        callee: SpannedExpr,
        args: Vec<crate::ast::expr::Argument>,
    },
    Print {
        format: SpannedExpr,
        items: Vec<SpannedExpr>,
    },
    Namelist {
        groups: Vec<(String, Vec<String>)>,
    },

    // ---- Declaration (embedded in statement context) ----
    Declaration(SpannedDecl),
}

/// I/O control specifier: either a positional value or keyword=value pair.
/// Used for WRITE/READ control lists, OPEN/CLOSE/INQUIRE specifiers, and
/// ALLOCATE options (stat=, errmsg=, source=, mold=).
#[derive(Debug, Clone, PartialEq)]
pub struct IoControl {
    pub keyword: Option<String>,
    pub value: SpannedExpr,
}

// ---- Supporting types ----

/// A type guard in SELECT TYPE.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeGuard {
    /// TYPE IS (type_name) — exact type match.
    TypeIs {
        type_name: String,
        body: Vec<SpannedStmt>,
    },
    /// CLASS IS (type_name) — matches type or any extension.
    ClassIs {
        type_name: String,
        body: Vec<SpannedStmt>,
    },
    /// CLASS DEFAULT — fallback.
    ClassDefault { body: Vec<SpannedStmt> },
}

/// SELECT RANK guard (F2018).
#[derive(Debug, Clone, PartialEq)]
pub enum RankGuard {
    /// RANK (N) — match a specific rank.
    Rank { rank: i64, body: Vec<SpannedStmt> },
    /// RANK (*) — match assumed-size.
    RankStar { body: Vec<SpannedStmt> },
    /// RANK DEFAULT — fallback.
    RankDefault { body: Vec<SpannedStmt> },
}

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
    Range {
        low: Option<SpannedExpr>,
        high: Option<SpannedExpr>,
    },
    Default,
}

/// DO CONCURRENT locality specification (F2018).
#[derive(Debug, Clone, PartialEq)]
pub enum LocalitySpec {
    /// LOCAL(var_list) — private to each iteration.
    Local(Vec<String>),
    /// LOCAL_INIT(var_list) — private, initialized from outer scope.
    LocalInit(Vec<String>),
    /// SHARED(var_list) — shared across iterations.
    Shared(Vec<String>),
    /// DEFAULT(NONE) — all variables must be explicitly specified.
    DefaultNone,
    /// REDUCE(op: var_list) — reduction variable.
    Reduce { op: String, vars: Vec<String> },
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
