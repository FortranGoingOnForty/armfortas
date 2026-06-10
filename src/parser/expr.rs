//! Pratt parser for Fortran expressions.
//!
//! 12 precedence levels matching the Fortran standard exactly.
//! Handles right-associative **, non-associative comparisons,
//! unary operators, function calls, array constructors, and
//! component access chains.

use super::{ParseError, Parser};
use crate::ast::expr::*;
use crate::ast::Spanned;
use crate::lexer::{Span, TokenKind};

/// Binding power for Pratt parsing.
/// Higher = tighter binding. Each level has left and right binding power.
/// For left-associative: right bp > left bp.
/// For right-associative: left bp > right bp.
/// For non-associative: left bp == right bp (disallows chaining).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bp {
    pub(crate) left: u8,
    pub(crate) right: u8,
}

// Precedence levels (from Fortran standard, lowest to highest).
// We use even numbers for left bp, odd for right, to create the half-levels
// needed for associativity.
const BP_DEFINED_BINARY: Bp = Bp { left: 2, right: 3 }; // .myop. (binary)
const BP_EQV: Bp = Bp { left: 4, right: 5 }; // .eqv., .neqv.
const BP_OR: Bp = Bp { left: 6, right: 7 }; // .or.
const BP_AND: Bp = Bp { left: 8, right: 9 }; // .and.
const BP_NOT: u8 = 10; // .not. (unary, right)
const BP_COMPARISON: Bp = Bp {
    left: 12,
    right: 12,
}; // ==, /=, <, >, <=, >= (non-assoc)
const BP_CONCAT: Bp = Bp {
    left: 14,
    right: 15,
}; // //
const BP_ADD: Bp = Bp {
    left: 16,
    right: 17,
}; // +, - (binary)
const BP_UNARY_ADD: u8 = 18; // +, - (unary)
pub(crate) const BP_MUL: Bp = Bp {
    left: 20,
    right: 21,
}; // *, /
const BP_POW: Bp = Bp {
    left: 23,
    right: 22,
}; // ** (RIGHT-assoc: left > right)
const BP_DEFINED_UNARY: u8 = 24; // .myop. (unary)

impl<'a> Parser<'a> {
    /// Parse an expression.
    pub fn parse_expr(&mut self) -> Result<SpannedExpr, ParseError> {
        self.parse_expr_bp(0)
    }

    /// Parse an expression with at least `min_bp` binding power (Pratt core).
    pub fn parse_expr_bp(&mut self, min_bp: u8) -> Result<SpannedExpr, ParseError> {
        const EXPR_NESTING_LIMIT: usize = 1024;

        if self.expr_depth >= EXPR_NESTING_LIMIT {
            return Err(self.error("expression nesting exceeds parser limit".into()));
        }

        self.expr_depth += 1;
        let result = (|| {
            // Parse prefix (atom or unary operator).
            let mut left = self.parse_prefix()?;

            // Loop: consume infix operators with sufficient binding power.
            loop {
                // Check for postfix operations: function call, component access.
                left = self.parse_postfix(left)?;

                // Check for infix operator.
                let Some(bp) = self.infix_bp() else { break };
                if bp.left < min_bp {
                    break;
                }

                // Non-associative operators: if left_bp == right_bp and we're at the
                // same precedence level, reject chaining (e.g., a < b < c is illegal).
                if bp.left == bp.right && bp.left == min_bp {
                    return Err(self.error(
                        "chained comparison operators are not allowed in Fortran (non-associative)"
                            .into(),
                    ));
                }

                let op_token = self.advance().clone();
                let op = token_to_binary_op(&op_token)?;
                let right = self.parse_expr_bp(bp.right)?;

                let span = Span {
                    file_id: left.span.file_id,
                    start: left.span.start,
                    end: right.span.end,
                };

                left = Spanned::new(
                    Expr::BinaryOp {
                        op,
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    span,
                );
            }

            Ok(left)
        })();
        self.expr_depth -= 1;
        result
    }

    /// Parse a prefix expression (atom, unary operator, parenthesized expr).
    /// The `then : else` tail of a conditional expression, with the
    /// condition already parsed and `?` consumed. The else-part may
    /// itself be `cond ? then : else` — right-recursive, so chains
    /// like `( c1 ? v1 : c2 ? v2 : v3 )` group as
    /// `c1 ? v1 : (c2 ? v2 : v3)` without nested parentheses
    /// (F2023; gfortran conditional_1.f90 line 16 relies on this).
    fn parse_conditional_after_question(
        &mut self,
        cond: SpannedExpr,
    ) -> Result<SpannedExpr, ParseError> {
        let then_val = self.parse_expr()?;
        self.expect(&TokenKind::Colon)?;
        let mut else_val = self.parse_expr()?;
        if self.eat(&TokenKind::Question) {
            else_val = self.parse_conditional_after_question(else_val)?;
        }
        let span = span_from_to(cond.span, self.prev_span());
        Ok(Spanned::new(
            Expr::ConditionalExpr {
                cond: Box::new(cond),
                then_val: Box::new(then_val),
                else_val: Box::new(else_val),
            },
            span,
        ))
    }

    fn parse_prefix(&mut self) -> Result<SpannedExpr, ParseError> {
        let start = self.current_span();

        match self.peek().clone() {
            // Unary operators.
            TokenKind::Plus => {
                self.advance();
                let operand = self.parse_expr_bp(BP_UNARY_ADD)?;
                let span = span_from_to(start, operand.span);
                Ok(Spanned::new(
                    Expr::UnaryOp {
                        op: UnaryOp::Plus,
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }
            TokenKind::Minus => {
                self.advance();
                let operand = self.parse_expr_bp(BP_UNARY_ADD)?;
                let span = span_from_to(start, operand.span);
                Ok(Spanned::new(
                    Expr::UnaryOp {
                        op: UnaryOp::Minus,
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }
            TokenKind::DotOp(ref name) if name == "not" => {
                self.advance();
                let operand = self.parse_expr_bp(BP_NOT)?;
                let span = span_from_to(start, operand.span);
                Ok(Spanned::new(
                    Expr::UnaryOp {
                        op: UnaryOp::Not,
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }
            TokenKind::DefinedOp(ref name) => {
                let op_name = name.clone();
                self.advance();
                let operand = self.parse_expr_bp(BP_DEFINED_UNARY)?;
                let span = span_from_to(start, operand.span);
                Ok(Spanned::new(
                    Expr::UnaryOp {
                        op: UnaryOp::Defined(op_name),
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }

            // Parenthesized expression or array constructor (/ ... /).
            TokenKind::LParen => {
                self.advance();
                // Check for (/ ... /) array constructor.
                if self.eat(&TokenKind::Slash) {
                    return self.parse_array_constructor_slash(start);
                }
                let inner = self.parse_expr()?;
                // F2023 conditional expression: `( cond ? then : else )`.
                // Must be probed after the inner expression parse and
                // before the complex-literal comma check — the inner
                // expression may itself contain commas and parens.
                if self.eat(&TokenKind::Question) {
                    let conditional = self.parse_conditional_after_question(inner)?;
                    self.expect(&TokenKind::RParen)?;
                    let span = span_from_to(start, self.prev_span());
                    return Ok(Spanned::new(conditional.node, span));
                }
                // Check for complex literal: (expr, expr)
                if self.eat(&TokenKind::Comma) {
                    let imag = self.parse_expr()?;
                    self.expect(&TokenKind::RParen)?;
                    let span = span_from_to(start, self.prev_span());
                    return Ok(Spanned::new(
                        Expr::ComplexLiteral {
                            real: Box::new(inner),
                            imag: Box::new(imag),
                        },
                        span,
                    ));
                }
                self.expect(&TokenKind::RParen)?;
                let span = span_from_to(start, self.prev_span());
                Ok(Spanned::new(
                    Expr::ParenExpr {
                        inner: Box::new(inner),
                    },
                    span,
                ))
            }

            // Array constructor [...]
            TokenKind::LBracket => {
                self.advance();
                self.parse_array_constructor_bracket(start)
            }

            // Literals.
            TokenKind::IntegerLiteral => self.parse_integer_literal(),
            TokenKind::RealLiteral => self.parse_real_literal(),
            TokenKind::StringLiteral => self.parse_string_literal(),
            TokenKind::LogicalLiteral => self.parse_logical_literal(),
            TokenKind::BozLiteral => self.parse_boz_literal(),

            // Identifier (name, keyword used as name, potential function call).
            TokenKind::Identifier => {
                if self.peek_text().ends_with('_')
                    && self
                        .tokens
                        .get(self.pos + 1)
                        .is_some_and(|tok| tok.kind == TokenKind::StringLiteral)
                {
                    self.parse_kind_prefixed_string_literal()
                } else {
                    self.parse_name()
                }
            }

            _ => Err(self.error(format!("expected expression, got {}", self.peek()))),
        }
    }

    /// Parse postfix operations: function call (parens), component access (%).
    fn parse_postfix(&mut self, mut expr: SpannedExpr) -> Result<SpannedExpr, ParseError> {
        loop {
            match self.peek() {
                // Function call / array subscript: expr(...)
                TokenKind::LParen => {
                    self.advance();
                    let args = self.parse_argument_list()?;
                    self.expect(&TokenKind::RParen)?;
                    let span = span_from_to(expr.span, self.prev_span());
                    expr = Spanned::new(
                        Expr::FunctionCall {
                            callee: Box::new(expr),
                            args,
                        },
                        span,
                    );
                }
                // Component access: expr%name
                TokenKind::Percent => {
                    self.advance();
                    let name_tok = self.advance().clone();
                    if name_tok.kind != TokenKind::Identifier {
                        return Err(ParseError {
                            span: name_tok.span,
                            msg: format!("expected component name after %, got {}", name_tok.kind),
                        });
                    }
                    let span = span_from_to(expr.span, name_tok.span);
                    expr = Spanned::new(
                        Expr::ComponentAccess {
                            base: Box::new(expr),
                            component: name_tok.text,
                        },
                        span,
                    );
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Get the binding power of the current token if it's an infix operator.
    fn infix_bp(&self) -> Option<Bp> {
        match self.peek() {
            TokenKind::Power => Some(BP_POW),
            TokenKind::Star => Some(BP_MUL),
            TokenKind::Slash => Some(BP_MUL),
            TokenKind::Plus => Some(BP_ADD),
            TokenKind::Minus => Some(BP_ADD),
            TokenKind::Concat => Some(BP_CONCAT),
            TokenKind::Eq
            | TokenKind::Ne
            | TokenKind::Lt
            | TokenKind::Le
            | TokenKind::Gt
            | TokenKind::Ge => Some(BP_COMPARISON),
            TokenKind::DotOp(ref name) => match name.as_str() {
                "eq" | "ne" | "lt" | "le" | "gt" | "ge" => Some(BP_COMPARISON),
                "and" => Some(BP_AND),
                "or" => Some(BP_OR),
                "eqv" => Some(BP_EQV),
                "neqv" => Some(BP_EQV),
                _ => None,
            },
            TokenKind::DefinedOp(_) => Some(BP_DEFINED_BINARY),
            _ => None,
        }
    }

    // ---- Literal parsers ----

    fn parse_integer_literal(&mut self) -> Result<SpannedExpr, ParseError> {
        let tok = self.advance().clone();
        let (text, kind) = split_kind_suffix(&tok.text);
        Ok(Spanned::new(Expr::IntegerLiteral { text, kind }, tok.span))
    }

    fn parse_real_literal(&mut self) -> Result<SpannedExpr, ParseError> {
        let tok = self.advance().clone();
        let (text, kind) = split_kind_suffix(&tok.text);
        Ok(Spanned::new(Expr::RealLiteral { text, kind }, tok.span))
    }

    fn parse_string_literal_with_kind(
        &mut self,
        kind: Option<String>,
        prefix_span: Option<Span>,
    ) -> Result<SpannedExpr, ParseError> {
        let tok = self.advance().clone();
        // Strip outer quotes for the value.
        let value = if tok.text.len() >= 2 {
            let inner = &tok.text[1..tok.text.len() - 1];
            match tok.text.as_bytes().first().copied() {
                Some(b'\'') => inner.replace("''", "'"),
                Some(b'"') => inner.replace("\"\"", "\""),
                _ => inner.to_string(),
            }
        } else {
            tok.text.clone()
        };
        let span = prefix_span
            .map(|prefix| span_from_to(prefix, tok.span))
            .unwrap_or(tok.span);
        Ok(Spanned::new(Expr::StringLiteral { value, kind }, span))
    }

    fn parse_string_literal(&mut self) -> Result<SpannedExpr, ParseError> {
        self.parse_string_literal_with_kind(None, None)
    }

    fn parse_kind_prefixed_string_literal(&mut self) -> Result<SpannedExpr, ParseError> {
        let kind_tok = self.advance().clone();
        let Some(kind) = kind_tok
            .text
            .strip_suffix('_')
            .filter(|text| !text.is_empty())
            .map(|text| text.to_string())
        else {
            return Err(ParseError {
                span: kind_tok.span,
                msg: "expected string kind prefix before string literal".into(),
            });
        };
        if self.peek() != &TokenKind::StringLiteral {
            return Err(self.error(format!(
                "expected string literal after kind prefix, got {}",
                self.peek()
            )));
        }
        self.parse_string_literal_with_kind(Some(kind), Some(kind_tok.span))
    }

    fn parse_logical_literal(&mut self) -> Result<SpannedExpr, ParseError> {
        let tok = self.advance().clone();
        let lower = tok.text.to_lowercase();
        let value = lower.contains("true");
        let kind = lower
            .find("._")
            .map(|pos| lower[pos + 2..].trim_end_matches('.').to_string());
        Ok(Spanned::new(Expr::LogicalLiteral { value, kind }, tok.span))
    }

    fn parse_boz_literal(&mut self) -> Result<SpannedExpr, ParseError> {
        let tok = self.advance().clone();
        let base = match tok.text.as_bytes()[0] {
            b'B' | b'b' => BozBase::Binary,
            b'O' | b'o' => BozBase::Octal,
            b'Z' | b'z' => BozBase::Hex,
            _ => BozBase::Hex,
        };
        Ok(Spanned::new(
            Expr::BozLiteral {
                text: tok.text,
                base,
            },
            tok.span,
        ))
    }

    fn parse_name(&mut self) -> Result<SpannedExpr, ParseError> {
        let tok = self.advance().clone();
        Ok(Spanned::new(Expr::Name { name: tok.text }, tok.span))
    }

    // ---- Argument list ----

    fn parse_argument_list(&mut self) -> Result<Vec<Argument>, ParseError> {
        let mut args = Vec::new();
        if self.peek() == &TokenKind::RParen {
            return Ok(args);
        }

        loop {
            if let Some(tuple_args) = self.try_parse_repeated_name_tuple_argument() {
                args.extend(tuple_args);
            } else {
                let arg = self.parse_argument()?;
                args.push(arg);
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(args)
    }

    fn try_parse_repeated_name_tuple_argument(&mut self) -> Option<Vec<Argument>> {
        let checkpoint = self.pos;
        if !self.eat(&TokenKind::LParen) {
            return None;
        }

        let first_tok = if self.peek() == &TokenKind::Identifier {
            self.advance().clone()
        } else {
            self.pos = checkpoint;
            return None;
        };
        let first_name = first_tok.text.clone();
        let mut names = vec![first_tok];
        let mut saw_comma = false;

        while self.eat(&TokenKind::Comma) {
            saw_comma = true;
            let tok = if self.peek() == &TokenKind::Identifier {
                self.advance().clone()
            } else {
                self.pos = checkpoint;
                return None;
            };
            if !tok.text.eq_ignore_ascii_case(&first_name) {
                self.pos = checkpoint;
                return None;
            }
            names.push(tok);
        }

        if !saw_comma || !self.eat(&TokenKind::RParen) {
            self.pos = checkpoint;
            return None;
        }
        if !matches!(self.peek(), TokenKind::RParen | TokenKind::Comma) {
            self.pos = checkpoint;
            return None;
        }

        Some(
            names
                .into_iter()
                .map(|tok| Argument {
                    keyword: None,
                    value: SectionSubscript::Element(Spanned::new(
                        Expr::Name { name: tok.text },
                        tok.span,
                    )),
                })
                .collect(),
        )
    }

    fn parse_argument(&mut self) -> Result<Argument, ParseError> {
        // Check for keyword argument: name = expr.
        if self.peek() == &TokenKind::Identifier {
            let next_pos = self.pos + 1;
            if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::Assign {
                let name_tok = self.advance().clone();
                self.advance(); // skip =
                let value = self.parse_expr()?;
                return Ok(Argument {
                    keyword: Some(name_tok.text),
                    value: SectionSubscript::Element(value),
                });
            }
        }

        // Leading colon → range with no start: :end or : or ::stride
        if matches!(self.peek(), TokenKind::Colon | TokenKind::ColonColon) {
            let sub = self.parse_range(None)?;
            return Ok(Argument {
                keyword: None,
                value: sub,
            });
        }

        // Parse an expression.
        let expr = self.parse_expr()?;

        // If followed by colon, it's a range: start:end[:stride]
        if matches!(self.peek(), TokenKind::Colon | TokenKind::ColonColon) {
            // Both `start:end[:stride]` and `start::stride` (empty
            // end, common in `a(1::7)`) need to enter the range
            // parser; without this, `::` after `start` was leaking
            // through and tripping the closing-paren check.
            let sub = self.parse_range(Some(expr))?;
            return Ok(Argument {
                keyword: None,
                value: sub,
            });
        }

        // Plain element.
        Ok(Argument {
            keyword: None,
            value: SectionSubscript::Element(expr),
        })
    }

    /// Parse a range subscript: [start]:end[:stride] or [start]: or :
    /// `start` is already parsed if present.
    fn parse_range(&mut self, start: Option<SpannedExpr>) -> Result<SectionSubscript, ParseError> {
        // Handle :: (ColonColon token) as two colons — means start::stride with no end.
        if self.eat(&TokenKind::ColonColon) {
            let stride = if !matches!(
                self.peek(),
                TokenKind::Comma | TokenKind::RParen | TokenKind::RBracket
            ) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            return Ok(SectionSubscript::Range {
                start,
                end: None,
                stride,
            });
        }

        self.expect(&TokenKind::Colon)?; // consume first colon

        // Parse end (optional — absent if next is colon, comma, ), ], or ::).
        let end = if !matches!(
            self.peek(),
            TokenKind::Colon
                | TokenKind::ColonColon
                | TokenKind::Comma
                | TokenKind::RParen
                | TokenKind::RBracket
        ) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        // Parse stride (optional, after second colon).
        let stride = if self.eat(&TokenKind::Colon) {
            if !matches!(
                self.peek(),
                TokenKind::Comma | TokenKind::RParen | TokenKind::RBracket
            ) {
                Some(self.parse_expr()?)
            } else {
                None
            }
        } else {
            None
        };

        Ok(SectionSubscript::Range { start, end, stride })
    }

    // ---- Array constructors ----

    fn parse_array_constructor_bracket(&mut self, start: Span) -> Result<SpannedExpr, ParseError> {
        // [type_spec :: ] value, value, ...
        let type_spec = self.try_parse_ac_type_spec();

        let mut values = Vec::new();
        if self.peek() != &TokenKind::RBracket {
            loop {
                values.push(self.parse_ac_value()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RBracket)?;
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            Expr::ArrayConstructor { type_spec, values },
            span,
        ))
    }

    fn parse_array_constructor_slash(&mut self, start: Span) -> Result<SpannedExpr, ParseError> {
        // Already consumed ( and /. Parse values until /).
        // The closing /) is ambiguous with division. We handle this by
        // checking if / is immediately followed by ) — if so, it's the closer.
        // Division inside (/ /) (e.g., (/ a/b /) ) is allowed but the / before )
        // is always the constructor close.
        // Each value routes through parse_ac_value so implied-do
        // constructors like `(/ (i, i=1,5) /)` are recognised — the
        // previous path used parse_expr_bp, which couldn't parse the
        // parenthesised implied-do form and errored on `=`.
        let mut values = Vec::new();
        loop {
            if matches!(self.peek(), TokenKind::Slash) {
                break;
            }
            values.push(self.parse_ac_value_bracketed(BP_MUL.right)?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::Slash)?;
        self.expect(&TokenKind::RParen)?;
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            Expr::ArrayConstructor {
                type_spec: None,
                values,
            },
            span,
        ))
    }

    /// Variant of parse_ac_value that honours a minimum binding
    /// power. Needed inside the `(/ ... /)` form where plain
    /// `parse_expr` would greedily consume the closing `/` as
    /// integer division. Implied-do values nested inside a `(...)`
    /// still parse their inner expressions at full precedence — the
    /// minimum BP only applies at the top level of each AcValue.
    fn parse_ac_value_bracketed(&mut self, min_bp: u8) -> Result<AcValue, ParseError> {
        if self.peek() == &TokenKind::LParen {
            let save_pos = self.pos;
            if let Ok(implied) = self.try_parse_implied_do() {
                return Ok(implied);
            }
            self.pos = save_pos;
        }
        let expr = self.parse_expr_bp(min_bp)?;
        Ok(AcValue::Expr(expr))
    }

    fn try_parse_ac_type_spec(&mut self) -> Option<String> {
        let save_pos = self.pos;
        if let Some(ts_result) = self.try_parse_type_spec() {
            if ts_result.is_ok() && self.peek() == &TokenKind::ColonColon {
                let spec = render_token_slice(&self.tokens[save_pos..self.pos]);
                self.advance(); // skip ::
                return Some(spec);
            }
            self.pos = save_pos;
        }
        None
    }

    fn parse_ac_value(&mut self) -> Result<AcValue, ParseError> {
        // Check for implied-do: (value-list, var=start,end[,step])
        if self.peek() == &TokenKind::LParen {
            // Save position — we might need to backtrack if it's not an implied-do.
            let save_pos = self.pos;
            if let Ok(implied) = self.try_parse_implied_do() {
                return Ok(implied);
            }
            // Not an implied-do — restore and parse as expression.
            self.pos = save_pos;
        }

        let expr = self.parse_expr()?;
        Ok(AcValue::Expr(expr))
    }

    fn try_parse_implied_do(&mut self) -> Result<AcValue, ParseError> {
        self.expect(&TokenKind::LParen)?;

        // Parse value list (one or more expressions separated by commas).
        let mut values = vec![self.parse_ac_value()?];
        while self.eat(&TokenKind::Comma) {
            // Check if this is the var=start part.
            // Pattern: identifier = expr , expr [, expr]
            if self.peek() == &TokenKind::Identifier {
                let next_pos = self.pos + 1;
                if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::Assign {
                    let var_tok = self.advance().clone();
                    self.advance(); // skip =
                    let start = self.parse_expr()?;
                    self.expect(&TokenKind::Comma)?;
                    let end = self.parse_expr()?;
                    let step = if self.eat(&TokenKind::Comma) {
                        Some(self.parse_expr()?)
                    } else {
                        None
                    };
                    self.expect(&TokenKind::RParen)?;
                    return Ok(AcValue::ImpliedDo(Box::new(ImpliedDoLoop {
                        values,
                        var: var_tok.text,
                        start,
                        end,
                        step,
                    })));
                }
            }
            values.push(self.parse_ac_value()?);
        }

        // If we get here, it wasn't an implied-do — fail so caller backtracks.
        Err(self.error("expected implied-do variable assignment".into()))
    }

    // ---- Helpers ----
}

// ---- Token to operator conversion ----

fn token_to_binary_op(tok: &crate::lexer::Token) -> Result<BinaryOp, ParseError> {
    match &tok.kind {
        TokenKind::Plus => Ok(BinaryOp::Add),
        TokenKind::Minus => Ok(BinaryOp::Sub),
        TokenKind::Star => Ok(BinaryOp::Mul),
        TokenKind::Slash => Ok(BinaryOp::Div),
        TokenKind::Power => Ok(BinaryOp::Pow),
        TokenKind::Concat => Ok(BinaryOp::Concat),
        TokenKind::Eq => Ok(BinaryOp::Eq),
        TokenKind::Ne => Ok(BinaryOp::Ne),
        TokenKind::Lt => Ok(BinaryOp::Lt),
        TokenKind::Le => Ok(BinaryOp::Le),
        TokenKind::Gt => Ok(BinaryOp::Gt),
        TokenKind::Ge => Ok(BinaryOp::Ge),
        TokenKind::DotOp(name) => match name.as_str() {
            "eq" => Ok(BinaryOp::Eq),
            "ne" => Ok(BinaryOp::Ne),
            "lt" => Ok(BinaryOp::Lt),
            "le" => Ok(BinaryOp::Le),
            "gt" => Ok(BinaryOp::Gt),
            "ge" => Ok(BinaryOp::Ge),
            "and" => Ok(BinaryOp::And),
            "or" => Ok(BinaryOp::Or),
            "eqv" => Ok(BinaryOp::Eqv),
            "neqv" => Ok(BinaryOp::Neqv),
            _ => Err(ParseError {
                span: tok.span,
                msg: format!("unknown dot-operator .{}.", name),
            }),
        },
        TokenKind::DefinedOp(name) => Ok(BinaryOp::Defined(name.clone())),
        _ => Err(ParseError {
            span: tok.span,
            msg: format!("expected operator, got {}", tok.kind),
        }),
    }
}

fn split_kind_suffix(text: &str) -> (String, Option<String>) {
    if let Some(pos) = text.find('_') {
        let num = text[..pos].to_string();
        let kind = text[pos + 1..].to_string();
        if kind.is_empty() {
            (text.to_string(), None)
        } else {
            (num, Some(kind))
        }
    } else {
        (text.to_string(), None)
    }
}

fn render_token_slice(tokens: &[crate::lexer::Token]) -> String {
    fn is_word_like(kind: &TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Identifier
                | TokenKind::IntegerLiteral
                | TokenKind::RealLiteral
                | TokenKind::StringLiteral
                | TokenKind::LogicalLiteral
                | TokenKind::BozLiteral
        )
    }

    let mut out = String::new();
    let mut prev_kind: Option<&TokenKind> = None;
    for tok in tokens {
        if let Some(prev) = prev_kind {
            if is_word_like(prev) && is_word_like(&tok.kind) {
                out.push(' ');
            }
        }
        out.push_str(&tok.text);
        prev_kind = Some(&tok.kind);
    }
    out
}

pub(crate) fn span_from_to(start: Span, end: Span) -> Span {
    Span {
        file_id: start.file_id,
        start: start.start,
        end: end.end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse_expression(src: &str) -> SpannedExpr {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        parser.parse_expr().unwrap()
    }

    fn sexpr(src: &str) -> String {
        parse_expression(src).to_sexpr()
    }

    // ---- Literals ----

    #[test]
    fn integer() {
        assert_eq!(sexpr("42"), "42");
    }
    #[test]
    fn integer_kind() {
        assert_eq!(sexpr("42_8"), "42");
    }
    #[test]
    fn real() {
        assert_eq!(sexpr("3.14"), "3.14");
    }
    #[test]
    fn real_exp() {
        assert_eq!(sexpr("1.0e5"), "1.0e5");
    }
    #[test]
    fn real_double() {
        assert_eq!(sexpr("1.0d0"), "1.0d0");
    }
    #[test]
    fn string_single() {
        assert_eq!(sexpr("'hello'"), "'hello'");
    }
    #[test]
    fn string_double() {
        assert_eq!(sexpr("\"hello\""), "'hello'");
    }
    #[test]
    fn string_kind_prefix_named_constant() {
        let expr = parse_expression("tfc_\"'\"");
        match expr.node {
            Expr::StringLiteral { value, kind } => {
                assert_eq!(value, "'");
                assert_eq!(kind.as_deref(), Some("tfc"));
            }
            other => panic!("expected string literal, got {:?}", other),
        }
    }
    #[test]
    fn logical_true() {
        assert_eq!(sexpr(".true."), ".true.");
    }
    #[test]
    fn logical_false() {
        assert_eq!(sexpr(".false."), ".false.");
    }
    #[test]
    fn boz() {
        assert_eq!(sexpr("B'1010'"), "B'1010'");
    }
    #[test]
    fn complex_literal() {
        assert_eq!(sexpr("(1.0, 2.0)"), "(1.0, 2.0)");
    }
    #[test]
    fn complex_literal_exprs() {
        assert_eq!(sexpr("(a + b, c * d)"), "((a + b), (c * d))");
    }
    #[test]
    fn name() {
        assert_eq!(sexpr("x"), "x");
    }

    // ---- Arithmetic precedence ----

    #[test]
    fn add_mul_precedence() {
        // a + b * c → (a + (b * c))
        assert_eq!(sexpr("a + b * c"), "(a + (b * c))");
    }

    #[test]
    fn mul_add_precedence() {
        // a * b + c → ((a * b) + c)
        assert_eq!(sexpr("a * b + c"), "((a * b) + c)");
    }

    #[test]
    fn power_right_associative() {
        // a ** b ** c → (a ** (b ** c))
        assert_eq!(sexpr("a ** b ** c"), "(a ** (b ** c))");
    }

    #[test]
    fn unary_minus_below_power() {
        // -a ** b → (- (a ** b))
        assert_eq!(sexpr("-a ** b"), "(- (a ** b))");
    }

    #[test]
    fn unary_minus_simple() {
        assert_eq!(sexpr("-x"), "(- x)");
    }

    #[test]
    fn unary_plus() {
        assert_eq!(sexpr("+x"), "(+ x)");
    }

    // ---- Comparison operators ----

    #[test]
    fn comparison_eq() {
        assert_eq!(sexpr("a == b"), "(a == b)");
    }

    #[test]
    fn comparison_dot_eq() {
        assert_eq!(sexpr("a .eq. b"), "(a == b)");
    }

    #[test]
    fn comparison_ne() {
        assert_eq!(sexpr("a /= b"), "(a /= b)");
    }

    #[test]
    fn comparison_chained_is_error() {
        // a < b < c is illegal Fortran (non-associative).
        let tokens = Lexer::tokenize("a < b < c", 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let result = parser.parse_expr();
        assert!(result.is_err(), "chained comparisons should error");
    }

    // ---- Logical operators ----

    #[test]
    fn logical_and_or() {
        // a .and. b .or. c → ((a .and. b) .or. c)
        assert_eq!(sexpr("a .and. b .or. c"), "((a .and. b) .or. c)");
    }

    #[test]
    fn logical_not() {
        assert_eq!(sexpr(".not. x"), "(.not. x)");
    }

    #[test]
    fn logical_eqv() {
        assert_eq!(sexpr("a .eqv. b"), "(a .eqv. b)");
    }

    // ---- Concatenation ----

    #[test]
    fn concat() {
        assert_eq!(sexpr("a // b"), "(a // b)");
    }

    #[test]
    fn concat_left_assoc() {
        assert_eq!(sexpr("a // b // c"), "((a // b) // c)");
    }

    // ---- Parentheses ----

    #[test]
    fn parens_override_precedence() {
        // ParenExpr wraps inner, so (a + b) * c has an explicit paren node.
        assert_eq!(sexpr("(a + b) * c"), "(((a + b)) * c)");
    }

    // ---- Function calls ----

    #[test]
    fn function_call_no_args() {
        assert_eq!(sexpr("f()"), "f()");
    }

    #[test]
    fn function_call_one_arg() {
        assert_eq!(sexpr("sin(x)"), "sin(x)");
    }

    #[test]
    fn function_call_multiple_args() {
        assert_eq!(sexpr("max(a, b, c)"), "max(a, b, c)");
    }

    #[test]
    fn function_call_keyword_arg() {
        assert_eq!(sexpr("open(unit=10)"), "open(unit=10)");
    }

    // ---- Component access ----

    #[test]
    fn component_access() {
        assert_eq!(sexpr("x%field"), "x%field");
    }

    #[test]
    fn component_access_chain() {
        assert_eq!(sexpr("x%inner%deep"), "x%inner%deep");
    }

    #[test]
    fn component_access_with_call() {
        assert_eq!(sexpr("obj%method(a)"), "obj%method(a)");
    }

    // ---- Array constructors ----

    #[test]
    fn array_constructor_bracket() {
        assert_eq!(sexpr("[1, 2, 3]"), "[1, 2, 3]");
    }

    #[test]
    fn array_constructor_typed() {
        assert_eq!(sexpr("[integer :: 1, 2]"), "[integer :: 1, 2]");
    }

    #[test]
    fn array_constructor_typed_character_len() {
        assert_eq!(
            sexpr("[character(len=26) :: '%s', 'left']"),
            "[character(len=26) :: '%s', 'left']"
        );
    }

    #[test]
    fn implied_do_basic() {
        assert_eq!(sexpr("[(i, i=1,10)]"), "[(i, i=1, 10)]");
    }

    #[test]
    fn implied_do_with_step() {
        assert_eq!(sexpr("[(i, i=1,10,2)]"), "[(i, i=1, 10, 2)]");
    }

    #[test]
    fn implied_do_expression() {
        assert_eq!(sexpr("[(i*2, i=1,5)]"), "[((i * 2), i=1, 5)]");
    }

    // ---- Range subscripts ----

    #[test]
    fn range_start_end() {
        assert_eq!(sexpr("a(1:5)"), "a(1:5)");
    }

    #[test]
    fn range_start_end_stride() {
        assert_eq!(sexpr("a(1:10:2)"), "a(1:10:2)");
    }

    #[test]
    fn range_colon_only() {
        // a(:) — full range
        assert_eq!(sexpr("a(:)"), "a(:)");
    }

    #[test]
    fn range_no_start() {
        // a(:5) — range with no start
        assert_eq!(sexpr("a(:5)"), "a(:5)");
    }

    #[test]
    fn range_no_end() {
        // a(2:) — range with no end
        assert_eq!(sexpr("a(2:)"), "a(2:)");
    }

    #[test]
    fn range_stride_only() {
        // a(::2) — full range with stride
        assert_eq!(sexpr("a(::2)"), "a(::2)");
    }

    #[test]
    fn multi_dim_with_ranges() {
        assert_eq!(sexpr("a(1:5, :, 3)"), "a(1:5, :, 3)");
    }

    // ---- Complex expressions ----

    #[test]
    fn complex_arithmetic() {
        assert_eq!(
            sexpr("a + b * c ** d - e / f"),
            "((a + (b * (c ** d))) - (e / f))"
        );
    }

    #[test]
    fn mixed_comparison_and_logical() {
        assert_eq!(sexpr("x > 0 .and. y < 10"), "((x > 0) .and. (y < 10))");
    }

    #[test]
    fn chained_function_calls() {
        assert_eq!(sexpr("f(g(x))"), "f(g(x))");
    }

    #[test]
    fn array_element_in_expression() {
        assert_eq!(sexpr("a(i) + b(j)"), "(a(i) + b(j))");
    }

    // ======================================================================
    // Audit test gap coverage
    // ======================================================================

    // ---- (/ /) array constructor form ----
    #[test]
    fn array_constructor_slash_form() {
        // (/ ... /) form — the closing / before ) can conflict with division.
        // Our parser handles this by checking if / is followed by ).
        assert_eq!(sexpr("(/ 1, 2, 3 /)"), "[1, 2, 3]");
    }

    // ---- .not. vs .and. precedence ----
    #[test]
    fn not_binds_tighter_than_and() {
        // .not. a .and. b → ((.not. a) .and. b)
        assert_eq!(sexpr(".not. a .and. b"), "((.not. a) .and. b)");
    }

    #[test]
    fn not_binds_tighter_than_or() {
        assert_eq!(sexpr(".not. a .or. b"), "((.not. a) .or. b)");
    }

    // ---- .eqv./.neqv. precedence ----
    #[test]
    fn eqv_lower_than_or() {
        // a .or. b .eqv. c → ((a .or. b) .eqv. c)
        assert_eq!(sexpr("a .or. b .eqv. c"), "((a .or. b) .eqv. c)");
    }

    // ---- Defined operators ----
    #[test]
    fn defined_binary_op() {
        assert_eq!(sexpr("a .cross. b"), "(a .cross. b)");
    }

    #[test]
    fn defined_unary_op() {
        assert_eq!(sexpr(".inv. x"), "(.inv. x)");
    }

    #[test]
    fn defined_binary_lowest_precedence() {
        // defined binary is lowest — a + b .myop. c → ((a + b) .myop. c)
        assert_eq!(sexpr("a + b .myop. c"), "((a + b) .myop. c)");
    }

    // ---- BOZ variants ----
    #[test]
    fn boz_octal() {
        assert_eq!(sexpr("O'777'"), "O'777'");
    }
    #[test]
    fn boz_hex() {
        assert_eq!(sexpr("Z'FF'"), "Z'FF'");
    }

    // ---- Real literal edge cases ----
    #[test]
    fn real_leading_dot() {
        assert_eq!(sexpr(".5"), ".5");
    }
    #[test]
    fn real_trailing_dot() {
        assert_eq!(sexpr("5."), "5.");
    }

    // ---- Mixed postfix chains ----
    #[test]
    fn postfix_call_then_component() {
        assert_eq!(sexpr("a(i)%field"), "a(i)%field");
    }

    #[test]
    fn postfix_deep_chain() {
        assert_eq!(sexpr("a%b(i)%c"), "a%b(i)%c");
    }

    // ---- Error cases ----
    #[test]
    fn error_unexpected_operator() {
        let tokens = Lexer::tokenize("+ *", 0).unwrap();
        let mut parser = Parser::new(&tokens);
        assert!(parser.parse_expr().is_err());
    }

    #[test]
    fn error_unclosed_paren() {
        let tokens = Lexer::tokenize("(a + b", 0).unwrap();
        let mut parser = Parser::new(&tokens);
        assert!(parser.parse_expr().is_err());
    }

    #[test]
    fn error_trailing_operator() {
        let tokens = Lexer::tokenize("a +", 0).unwrap();
        let mut parser = Parser::new(&tokens);
        assert!(parser.parse_expr().is_err());
    }
}
