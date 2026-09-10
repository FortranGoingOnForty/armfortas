//! Abstract syntax tree node definitions.
//!
//! AST nodes for expressions, statements, declarations,
//! program units, and all Fortran constructs. Every node
//! carries a Span for source location tracking.

pub mod decl;
pub mod expr;
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

/// Return the canonical spelling for a generic-spec name.
///
/// Fortran's dotted relational operators are synonyms for their symbolic
/// spellings. Store one spelling in the AST and module interfaces so that an
/// `operator(.eq.)` declaration is visible to an expression parsed from `==`
/// (and vice versa), including across `.amod` boundaries.
pub fn canonical_generic_spec_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let Some(operator) = lower
        .strip_prefix("operator(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return name.to_string();
    };
    let operator = match operator {
        ".eq." => "==",
        ".ne." => "/=",
        ".lt." => "<",
        ".le." => "<=",
        ".gt." => ">",
        ".ge." => ">=",
        other => other,
    };
    format!("operator({operator})")
}
