//! Recursive descent Fortran parser.
//!
//! Produces a typed AST from a token stream. No parser generators.
//! Expression parsing uses a Pratt parser with 12 precedence levels
//! matching the Fortran standard exactly.

pub mod expr;

use crate::lexer::{Token, TokenKind, Span, Position};
use crate::ast::Spanned;
use crate::ast::expr::SpannedExpr;

use std::fmt;

/// Parser error.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub span: Span,
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: error: {}", self.span.start.line, self.span.start.col, self.msg)
    }
}

impl std::error::Error for ParseError {}

/// The parser state — holds the token stream and current position.
pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    /// Peek at the current token kind without consuming.
    pub fn peek(&self) -> &TokenKind {
        if self.pos < self.tokens.len() {
            &self.tokens[self.pos].kind
        } else {
            &TokenKind::Eof
        }
    }

    /// Peek at the current token (with span info).
    pub fn current(&self) -> &Token {
        if self.pos < self.tokens.len() {
            &self.tokens[self.pos]
        } else if let Some(last) = self.tokens.last() {
            last
        } else {
            // Should never happen — tokenize always produces at least Eof.
            panic!("parser created with empty token stream");
        }
    }

    /// Peek at the current token's text.
    pub fn peek_text(&self) -> &str {
        if self.pos < self.tokens.len() {
            &self.tokens[self.pos].text
        } else {
            ""
        }
    }

    /// Advance past the current token and return it.
    pub fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos.min(self.tokens.len() - 1)];
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    /// Advance if the current token matches the expected kind. Returns true if consumed.
    pub fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek() == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Advance if the current token is an identifier with the given text (case-insensitive).
    pub fn eat_ident(&mut self, name: &str) -> bool {
        if self.peek() == &TokenKind::Identifier && self.peek_text().eq_ignore_ascii_case(name) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Expect the current token to match, or return an error.
    pub fn expect(&mut self, kind: &TokenKind) -> Result<&Token, ParseError> {
        if self.peek() == kind {
            Ok(self.advance())
        } else {
            Err(self.error(format!("expected {}, got {}", kind, self.peek())))
        }
    }

    /// Skip newlines (which are statement terminators but not significant mid-parse).
    pub fn skip_newlines(&mut self) {
        while matches!(self.peek(), TokenKind::Newline | TokenKind::Comment) {
            self.advance();
        }
    }

    /// Create an error at the current position.
    pub fn error(&self, msg: String) -> ParseError {
        ParseError {
            span: self.current().span,
            msg,
        }
    }

    /// Get the span of the current token.
    pub fn current_span(&self) -> Span {
        self.current().span
    }

    /// Check if we're at a statement-ending token.
    pub fn at_stmt_end(&self) -> bool {
        matches!(self.peek(), TokenKind::Newline | TokenKind::Semicolon | TokenKind::Eof | TokenKind::Comment)
    }
}
