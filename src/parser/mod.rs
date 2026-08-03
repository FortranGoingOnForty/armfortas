//! Recursive descent Fortran parser.
//!
//! Produces a typed AST from a token stream. No parser generators.
//! Expression parsing uses a Pratt parser with 12 precedence levels
//! matching the Fortran standard exactly.

pub mod decl;
pub mod expr;
pub mod stmt;
pub mod unit;

use crate::lexer::{SourceForm, Span, Token, TokenKind};

use std::borrow::Cow;
use std::fmt;

/// Parser error.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub span: Span,
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: error: {}",
            self.span.start.line, self.span.start.col, self.msg
        )
    }
}

impl std::error::Error for ParseError {}

/// The parser state — holds the token stream and current position.
pub struct Parser<'a> {
    tokens: Cow<'a, [Token]>,
    pos: usize,
    expr_depth: usize,
    slash_array_ctor_depth: usize,
    source_view: bool,
    fixed_form: bool,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self::with_mode(tokens, false, false)
    }

    /// Construct a parser that can resolve source-form-specific lexical
    /// ambiguities retained by `tokenize`.
    pub fn new_for_form(tokens: &'a [Token], source_form: SourceForm) -> Self {
        Self::with_mode(tokens, false, matches!(source_form, SourceForm::FixedForm))
    }

    pub(crate) fn new_source_view(tokens: &'a [Token]) -> Self {
        Self::with_mode(tokens, true, false)
    }

    pub(crate) fn new_source_view_for_form(tokens: &'a [Token], source_form: SourceForm) -> Self {
        Self::with_mode(tokens, true, matches!(source_form, SourceForm::FixedForm))
    }

    fn with_mode(tokens: &'a [Token], source_view: bool, fixed_form: bool) -> Self {
        Self {
            tokens: Cow::Borrowed(tokens),
            pos: 0,
            expr_depth: 0,
            slash_array_ctor_depth: 0,
            source_view,
            fixed_form,
        }
    }

    /// Peek at the current token kind without consuming.
    pub fn peek(&self) -> &TokenKind {
        if self.pos < self.tokens.len() {
            &self.tokens[self.pos].kind
        } else {
            &TokenKind::Eof
        }
    }

    /// Peek at the kind of the token `n` positions ahead of the cursor.
    /// Returns `None` past the end of the stream.
    pub fn peek_kind_at(&self, n: usize) -> Option<&TokenKind> {
        self.tokens.get(self.pos + n).map(|t| &t.kind)
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

    /// Materialize one grammar-proven boundary in an otherwise ambiguous
    /// fixed-form identifier. The token buffer remains borrowed unless such a
    /// split is actually required.
    fn split_fixed_identifier_at(&mut self, token_index: usize, prefix_len: usize) -> bool {
        if !self.fixed_form {
            return false;
        }
        let Some(token) = self.tokens.get(token_index).cloned() else {
            return false;
        };
        if token.kind != TokenKind::Identifier
            || prefix_len == 0
            || prefix_len >= token.text.len()
            || !token.text.is_char_boundary(prefix_len)
        {
            return false;
        }

        let suffix_len = token.text.len() - prefix_len;
        let suffix_start = if token.span.end.col >= suffix_len as u32 {
            crate::lexer::Position {
                line: token.span.end.line,
                col: token.span.end.col - suffix_len as u32,
            }
        } else {
            token.span.start
        };
        let mut prefix = token.clone();
        prefix.text.truncate(prefix_len);
        prefix.span.end = suffix_start;

        let mut suffix = token;
        suffix.text = suffix.text[prefix_len..].to_string();
        suffix.span.start = suffix_start;

        self.tokens
            .to_mut()
            .splice(token_index..=token_index, [prefix, suffix]);
        true
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

    /// Skip statement boundaries (newlines, comments, and semicolons).
    /// F2018 §6.3.2: a semicolon separates statements on the same line
    /// and is semantically equivalent to a newline.
    pub fn skip_newlines(&mut self) {
        while matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::Comment | TokenKind::Semicolon
        ) {
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

    /// Get the span of the previous token.
    pub fn prev_span(&self) -> Span {
        if self.pos > 0 {
            self.tokens[self.pos - 1].span
        } else {
            self.current_span()
        }
    }

    /// Check if we're at a statement-ending token.
    pub fn at_stmt_end(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::Eof | TokenKind::Comment
        )
    }

    /// Check if the token at offset `n` from current position is a statement terminator.
    pub fn at_stmt_end_after(&self, n: usize) -> bool {
        let idx = self.pos + n;
        if idx >= self.tokens.len() {
            return true;
        }
        matches!(
            self.tokens[idx].kind,
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::Eof | TokenKind::Comment
        )
    }
}
