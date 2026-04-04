//! Abstract syntax tree node definitions.
//!
//! AST nodes for expressions, statements, declarations,
//! program units, and all Fortran constructs. Every node
//! carries a Span for source location tracking.

pub mod expr;
pub mod decl;
pub mod stmt;
pub mod unit;

use crate::lexer::Span;

/// A spanned AST node — wraps any node with its source location.
#[derive(Debug, Clone, PartialEq)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(node: T, span: Span) -> Self {
        Self { node, span }
    }
}
