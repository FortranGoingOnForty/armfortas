//! Typed OpenMP directive syntax.
//!
//! These nodes describe source-level constructs only. Executable lowering is
//! deliberately separate so recognizing a directive can never imply that the
//! compiler already implements its parallel semantics.

use super::expr::{Argument, SpannedExpr};
use super::stmt::SpannedStmt;

#[derive(Debug, Clone, PartialEq)]
pub enum OpenMpConstruct {
    Parallel {
        clauses: Vec<OpenMpClause>,
        body: Vec<SpannedStmt>,
    },
    Do {
        clauses: Vec<OpenMpClause>,
        loop_stmt: Box<SpannedStmt>,
    },
    Loop {
        clauses: Vec<OpenMpClause>,
        loop_stmt: Box<SpannedStmt>,
    },
    ParallelDo {
        clauses: Vec<OpenMpClause>,
        loop_stmt: Box<SpannedStmt>,
    },
    Critical {
        name: Option<String>,
        body: Vec<SpannedStmt>,
    },
    Master {
        body: Vec<SpannedStmt>,
    },
    Single {
        clauses: Vec<OpenMpClause>,
        body: Vec<SpannedStmt>,
    },
    Sections {
        clauses: Vec<OpenMpClause>,
        body: Vec<SpannedStmt>,
        section_starts: Vec<usize>,
    },
    Ordered {
        body: Vec<SpannedStmt>,
    },
    OrderedDoacross {
        dependence: OpenMpDoacrossDependence,
    },
    Barrier,
    Flush {
        variables: Vec<String>,
    },
    AtomicRead {
        assignment: Box<SpannedStmt>,
    },
    AtomicWrite {
        assignment: Box<SpannedStmt>,
    },
    AtomicUpdate {
        assignment: Box<SpannedStmt>,
    },
}

impl OpenMpConstruct {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Parallel { .. } => "PARALLEL",
            Self::Do { .. } => "DO",
            Self::Loop { .. } => "LOOP",
            Self::ParallelDo { .. } => "PARALLEL DO",
            Self::Critical { .. } => "CRITICAL",
            Self::Master { .. } => "MASTER",
            Self::Single { .. } => "SINGLE",
            Self::Sections { .. } => "SECTIONS",
            Self::Ordered { .. } => "ORDERED",
            Self::OrderedDoacross { .. } => "ORDERED DOACROSS",
            Self::Barrier => "BARRIER",
            Self::Flush { .. } => "FLUSH",
            Self::AtomicRead { .. } => "ATOMIC READ",
            Self::AtomicWrite { .. } => "ATOMIC WRITE",
            Self::AtomicUpdate { .. } => "ATOMIC UPDATE",
        }
    }

    pub fn clauses(&self) -> &[OpenMpClause] {
        match self {
            Self::Parallel { clauses, .. }
            | Self::Do { clauses, .. }
            | Self::Loop { clauses, .. }
            | Self::ParallelDo { clauses, .. }
            | Self::Single { clauses, .. }
            | Self::Sections { clauses, .. } => clauses,
            Self::Critical { .. }
            | Self::Master { .. }
            | Self::Ordered { .. }
            | Self::OrderedDoacross { .. }
            | Self::Barrier
            | Self::Flush { .. }
            | Self::AtomicRead { .. }
            | Self::AtomicWrite { .. }
            | Self::AtomicUpdate { .. } => &[],
        }
    }

    pub fn region_body(&self) -> Option<&[SpannedStmt]> {
        match self {
            Self::Parallel { body, .. }
            | Self::Critical { body, .. }
            | Self::Master { body }
            | Self::Single { body, .. }
            | Self::Sections { body, .. }
            | Self::Ordered { body } => Some(body),
            Self::AtomicRead { assignment }
            | Self::AtomicWrite { assignment }
            | Self::AtomicUpdate { assignment } => Some(std::slice::from_ref(assignment.as_ref())),
            Self::Do { .. }
            | Self::Loop { .. }
            | Self::ParallelDo { .. }
            | Self::OrderedDoacross { .. }
            | Self::Barrier
            | Self::Flush { .. } => None,
        }
    }

    pub fn loop_stmt(&self) -> Option<&SpannedStmt> {
        match self {
            Self::Do { loop_stmt, .. }
            | Self::Loop { loop_stmt, .. }
            | Self::ParallelDo { loop_stmt, .. } => Some(loop_stmt),
            Self::Parallel { .. }
            | Self::Critical { .. }
            | Self::Master { .. }
            | Self::Single { .. }
            | Self::Sections { .. }
            | Self::Ordered { .. }
            | Self::OrderedDoacross { .. }
            | Self::Barrier
            | Self::Flush { .. }
            | Self::AtomicRead { .. }
            | Self::AtomicWrite { .. }
            | Self::AtomicUpdate { .. } => None,
        }
    }

    pub fn standalone_expressions(&self) -> Vec<&SpannedExpr> {
        match self {
            Self::OrderedDoacross { dependence } => dependence.vectors().iter().flatten().collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpenMpClause {
    Private(Vec<String>),
    FirstPrivate(Vec<String>),
    CopyIn(Vec<String>),
    LastPrivate {
        conditional: bool,
        variables: Vec<String>,
    },
    Shared(Vec<String>),
    Default(OpenMpDefault),
    If {
        modifier: Option<String>,
        condition: SpannedExpr,
    },
    NumThreads(SpannedExpr),
    Schedule {
        kind: OpenMpScheduleKind,
        chunk_size: Option<SpannedExpr>,
    },
    Collapse(SpannedExpr),
    Ordered(Option<SpannedExpr>),
    Nowait,
    Reduction {
        operator: OpenMpReductionOperator,
        items: Vec<OpenMpReductionItem>,
    },
}

impl OpenMpClause {
    pub fn expression(&self) -> Option<&SpannedExpr> {
        match self {
            Self::If { condition, .. }
            | Self::NumThreads(condition)
            | Self::Collapse(condition) => Some(condition),
            Self::Ordered(Some(parameter)) => Some(parameter),
            Self::Schedule { chunk_size, .. } => chunk_size.as_ref(),
            Self::Private(_)
            | Self::FirstPrivate(_)
            | Self::CopyIn(_)
            | Self::LastPrivate { .. }
            | Self::Shared(_)
            | Self::Default(_)
            | Self::Ordered(None)
            | Self::Nowait
            | Self::Reduction { .. } => None,
        }
    }

    pub fn listed_variable_names(&self) -> Vec<&str> {
        match self {
            Self::Private(names)
            | Self::FirstPrivate(names)
            | Self::CopyIn(names)
            | Self::Shared(names) => names.iter().map(String::as_str).collect(),
            Self::LastPrivate { variables, .. } => variables.iter().map(String::as_str).collect(),
            Self::Reduction { items, .. } => {
                items.iter().map(OpenMpReductionItem::base_name).collect()
            }
            Self::Default(_)
            | Self::If { .. }
            | Self::NumThreads(_)
            | Self::Schedule { .. }
            | Self::Collapse(_)
            | Self::Ordered(_)
            | Self::Nowait => Vec::new(),
        }
    }

    pub fn reduction_items(&self) -> Option<&[OpenMpReductionItem]> {
        match self {
            Self::Reduction { items, .. } => Some(items),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpenMpDoacrossDependence {
    Source,
    Sink { vectors: Vec<Vec<SpannedExpr>> },
}

impl OpenMpDoacrossDependence {
    pub fn vectors(&self) -> &[Vec<SpannedExpr>] {
        match self {
            Self::Source => &[],
            Self::Sink { vectors } => vectors,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpenMpReductionItem {
    Name(String),
    ArrayDesignator {
        name: String,
        subscripts: Vec<Argument>,
    },
}

impl OpenMpReductionItem {
    pub fn base_name(&self) -> &str {
        match self {
            Self::Name(name) | Self::ArrayDesignator { name, .. } => name,
        }
    }

    pub fn source_text(&self) -> String {
        match self {
            Self::Name(name) => name.clone(),
            Self::ArrayDesignator { name, subscripts } => format!(
                "{}({})",
                name,
                subscripts
                    .iter()
                    .map(|subscript| subscript.value.to_sexpr())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMpDefault {
    Shared,
    Private,
    FirstPrivate,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMpScheduleKind {
    Static,
    Dynamic,
    Guided,
    Runtime,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMpReductionOperator {
    Add,
    Multiply,
    Max,
    Min,
    And,
    Or,
    Eqv,
    Neqv,
    Iand,
    Ior,
    Ieor,
}
