//! Typed OpenMP directive syntax.
//!
//! These nodes describe source-level constructs only. Executable lowering is
//! deliberately separate so recognizing a directive can never imply that the
//! compiler already implements its parallel semantics.

use super::expr::SpannedExpr;
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
    ParallelDo {
        clauses: Vec<OpenMpClause>,
        loop_stmt: Box<SpannedStmt>,
    },
    Critical {
        name: Option<String>,
        body: Vec<SpannedStmt>,
    },
}

impl OpenMpConstruct {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Parallel { .. } => "PARALLEL",
            Self::Do { .. } => "DO",
            Self::ParallelDo { .. } => "PARALLEL DO",
            Self::Critical { .. } => "CRITICAL",
        }
    }

    pub fn clauses(&self) -> &[OpenMpClause] {
        match self {
            Self::Parallel { clauses, .. }
            | Self::Do { clauses, .. }
            | Self::ParallelDo { clauses, .. } => clauses,
            Self::Critical { .. } => &[],
        }
    }

    pub fn region_body(&self) -> Option<&[SpannedStmt]> {
        match self {
            Self::Parallel { body, .. } | Self::Critical { body, .. } => Some(body),
            Self::Do { .. } | Self::ParallelDo { .. } => None,
        }
    }

    pub fn loop_stmt(&self) -> Option<&SpannedStmt> {
        match self {
            Self::Do { loop_stmt, .. } | Self::ParallelDo { loop_stmt, .. } => Some(loop_stmt),
            Self::Parallel { .. } | Self::Critical { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpenMpClause {
    Private(Vec<String>),
    FirstPrivate(Vec<String>),
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
    Nowait,
    Reduction {
        operator: OpenMpReductionOperator,
        variables: Vec<String>,
    },
}

impl OpenMpClause {
    pub fn expression(&self) -> Option<&SpannedExpr> {
        match self {
            Self::If { condition, .. }
            | Self::NumThreads(condition)
            | Self::Collapse(condition) => Some(condition),
            Self::Schedule { chunk_size, .. } => chunk_size.as_ref(),
            Self::Private(_)
            | Self::FirstPrivate(_)
            | Self::Shared(_)
            | Self::Default(_)
            | Self::Nowait
            | Self::Reduction { .. } => None,
        }
    }

    pub fn listed_variables(&self) -> Option<&[String]> {
        match self {
            Self::Private(names) | Self::FirstPrivate(names) | Self::Shared(names) => Some(names),
            Self::Reduction { variables, .. } => Some(variables),
            Self::Default(_)
            | Self::If { .. }
            | Self::NumThreads(_)
            | Self::Schedule { .. }
            | Self::Collapse(_)
            | Self::Nowait => None,
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
}
