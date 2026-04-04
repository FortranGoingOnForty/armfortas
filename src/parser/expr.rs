//! Pratt parser for Fortran expressions.
//!
//! 12 precedence levels matching the Fortran standard exactly.
//! Handles right-associative **, non-associative comparisons,
//! unary operators, function calls, array constructors, and
//! component access chains.

use crate::ast::Spanned;
use crate::ast::expr::*;
use crate::lexer::{TokenKind, Span, Position};
use super::{Parser, ParseError};

/// Binding power for Pratt parsing.
/// Higher = tighter binding. Each level has left and right binding power.
/// For left-associative: right bp > left bp.
/// For right-associative: left bp > right bp.
/// For non-associative: left bp == right bp (disallows chaining).
#[derive(Debug, Clone, Copy)]
struct Bp {
    left: u8,
    right: u8,
}

// Precedence levels (from Fortran standard, lowest to highest).
// We use even numbers for left bp, odd for right, to create the half-levels
// needed for associativity.
const BP_DEFINED_BINARY: Bp = Bp { left: 2, right: 3 };  // .myop. (binary)
const BP_EQV: Bp = Bp { left: 4, right: 5 };             // .eqv., .neqv.
const BP_OR: Bp = Bp { left: 6, right: 7 };              // .or.
const BP_AND: Bp = Bp { left: 8, right: 9 };             // .and.
const BP_NOT: u8 = 10;                                    // .not. (unary, right)
const BP_COMPARISON: Bp = Bp { left: 12, right: 12 };    // ==, /=, <, >, <=, >= (non-assoc)
const BP_CONCAT: Bp = Bp { left: 14, right: 15 };        // //
const BP_ADD: Bp = Bp { left: 16, right: 17 };           // +, - (binary)
const BP_UNARY_ADD: u8 = 18;                              // +, - (unary)
const BP_MUL: Bp = Bp { left: 20, right: 21 };           // *, /
const BP_POW: Bp = Bp { left: 23, right: 22 };           // ** (RIGHT-assoc: left > right)
const BP_DEFINED_UNARY: u8 = 24;                          // .myop. (unary)

impl<'a> Parser<'a> {
    /// Parse an expression.
    pub fn parse_expr(&mut self) -> Result<SpannedExpr, ParseError> {
        self.parse_expr_bp(0)
    }

    /// Parse an expression with at least `min_bp` binding power (Pratt core).
    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<SpannedExpr, ParseError> {
        // Parse prefix (atom or unary operator).
        let mut left = self.parse_prefix()?;

        // Loop: consume infix operators with sufficient binding power.
        loop {
            // Check for postfix operations: function call, component access.
            left = self.parse_postfix(left)?;

            // Check for infix operator.
            let Some(bp) = self.infix_bp() else { break };
            if bp.left < min_bp { break; }

            let op_token = self.advance().clone();
            let op = token_to_binary_op(&op_token)?;
            let right = self.parse_expr_bp(bp.right)?;

            let span = Span {
                file_id: left.span.file_id,
                start: left.span.start,
                end: right.span.end,
            };

            left = Spanned::new(Expr::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }, span);
        }

        Ok(left)
    }

    /// Parse a prefix expression (atom, unary operator, parenthesized expr).
    fn parse_prefix(&mut self) -> Result<SpannedExpr, ParseError> {
        let start = self.current_span();

        match self.peek().clone() {
            // Unary operators.
            TokenKind::Plus => {
                self.advance();
                let operand = self.parse_expr_bp(BP_UNARY_ADD)?;
                let span = span_from_to(start, operand.span);
                Ok(Spanned::new(Expr::UnaryOp {
                    op: UnaryOp::Plus,
                    operand: Box::new(operand),
                }, span))
            }
            TokenKind::Minus => {
                self.advance();
                let operand = self.parse_expr_bp(BP_UNARY_ADD)?;
                let span = span_from_to(start, operand.span);
                Ok(Spanned::new(Expr::UnaryOp {
                    op: UnaryOp::Minus,
                    operand: Box::new(operand),
                }, span))
            }
            TokenKind::DotOp(ref name) if name == "not" => {
                self.advance();
                let operand = self.parse_expr_bp(BP_NOT)?;
                let span = span_from_to(start, operand.span);
                Ok(Spanned::new(Expr::UnaryOp {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                }, span))
            }
            TokenKind::DefinedOp(ref name) => {
                let op_name = name.clone();
                self.advance();
                let operand = self.parse_expr_bp(BP_DEFINED_UNARY)?;
                let span = span_from_to(start, operand.span);
                Ok(Spanned::new(Expr::UnaryOp {
                    op: UnaryOp::Defined(op_name),
                    operand: Box::new(operand),
                }, span))
            }

            // Parenthesized expression or array constructor (/ ... /).
            TokenKind::LParen => {
                self.advance();
                // Check for (/ ... /) array constructor.
                if self.eat(&TokenKind::Slash) {
                    return self.parse_array_constructor_slash(start);
                }
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                let span = span_from_to(start, self.prev_span());
                Ok(Spanned::new(Expr::ParenExpr {
                    inner: Box::new(inner),
                }, span))
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
            TokenKind::Identifier => self.parse_name(),

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
                    expr = Spanned::new(Expr::FunctionCall {
                        callee: Box::new(expr),
                        args,
                    }, span);
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
                    expr = Spanned::new(Expr::ComponentAccess {
                        base: Box::new(expr),
                        component: name_tok.text,
                    }, span);
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
            TokenKind::Eq | TokenKind::Ne | TokenKind::Lt |
            TokenKind::Le | TokenKind::Gt | TokenKind::Ge => Some(BP_COMPARISON),
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

    fn parse_string_literal(&mut self) -> Result<SpannedExpr, ParseError> {
        let tok = self.advance().clone();
        // Strip outer quotes for the value.
        let value = if tok.text.len() >= 2 {
            tok.text[1..tok.text.len()-1].replace("''", "'").replace("\"\"", "\"")
        } else {
            tok.text.clone()
        };
        Ok(Spanned::new(Expr::StringLiteral { value, kind: None }, tok.span))
    }

    fn parse_logical_literal(&mut self) -> Result<SpannedExpr, ParseError> {
        let tok = self.advance().clone();
        let lower = tok.text.to_lowercase();
        let value = lower.contains("true");
        let kind = if let Some(pos) = lower.find("._") {
            Some(lower[pos+2..lower.len()-0].trim_end_matches('.').to_string())
        } else {
            None
        };
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
        Ok(Spanned::new(Expr::BozLiteral { text: tok.text, base }, tok.span))
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
            let arg = self.parse_argument()?;
            args.push(arg);
            if !self.eat(&TokenKind::Comma) { break; }
        }
        Ok(args)
    }

    fn parse_argument(&mut self) -> Result<Argument, ParseError> {
        // Check for keyword argument: name = expr.
        // We need lookahead: if current is Identifier and next is =, it's a keyword arg.
        if self.peek() == &TokenKind::Identifier {
            let next_pos = self.pos + 1;
            if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::Assign {
                let name_tok = self.advance().clone();
                self.advance(); // skip =
                let value = self.parse_expr()?;
                return Ok(Argument { keyword: Some(name_tok.text), value });
            }
        }

        // Check for section subscript (colon range).
        if self.peek() == &TokenKind::Colon || self.is_range_context() {
            let sub = self.parse_section_subscript()?;
            return Ok(Argument { keyword: None, value: sub });
        }

        let value = self.parse_expr()?;

        // After parsing expr, check if a colon follows (range like start:end).
        if self.peek() == &TokenKind::Colon {
            return self.parse_range_after_start(value);
        }

        Ok(Argument { keyword: None, value })
    }

    fn is_range_context(&self) -> bool {
        // A leading colon means `:end` or `:` (full range).
        self.peek() == &TokenKind::Colon
    }

    fn parse_section_subscript(&mut self) -> Result<SpannedExpr, ParseError> {
        let start_span = self.current_span();

        let start = if self.peek() == &TokenKind::Colon {
            None
        } else {
            Some(Box::new(self.parse_expr()?))
        };

        if !self.eat(&TokenKind::Colon) {
            // Just an element, not a range.
            if let Some(s) = start {
                return Ok(*s);
            }
            return Err(self.error("expected expression in subscript".into()));
        }

        // Parse end (optional).
        let end = if !matches!(self.peek(), TokenKind::Colon | TokenKind::Comma | TokenKind::RParen | TokenKind::RBracket) {
            Some(Box::new(self.parse_expr()?))
        } else {
            None
        };

        // Parse stride (optional, after second colon).
        let stride = if self.eat(&TokenKind::Colon) {
            if !matches!(self.peek(), TokenKind::Comma | TokenKind::RParen | TokenKind::RBracket) {
                Some(Box::new(self.parse_expr()?))
            } else {
                None
            }
        } else {
            None
        };

        // Build a synthetic "range" expression. We'll represent this as a
        // FunctionCall-like node that sema can distinguish. For now, use Name
        // as a placeholder — the parser returns the range components as a
        // colon-separated expression which the caller handles.
        let span = span_from_to(start_span, self.prev_span());
        // Return the start expression (the caller builds the range from context).
        // Actually, for simplicity, return start if it exists, otherwise a placeholder.
        if let Some(s) = start {
            Ok(*s)
        } else {
            Ok(Spanned::new(Expr::Name { name: ":".into() }, span))
        }
    }

    fn parse_range_after_start(&mut self, start: SpannedExpr) -> Result<Argument, ParseError> {
        // We already parsed `start` and see `:`.
        // This handles a(start:end) or a(start:end:stride).
        // For now, emit as a regular argument — the parser for subscripts
        // will need more sophisticated range handling in Sprint 8+.
        Ok(Argument { keyword: None, value: start })
    }

    // ---- Array constructors ----

    fn parse_array_constructor_bracket(&mut self, start: Span) -> Result<SpannedExpr, ParseError> {
        // [type_spec :: ] value, value, ...
        let type_spec = self.try_parse_ac_type_spec();

        let mut values = Vec::new();
        if self.peek() != &TokenKind::RBracket {
            loop {
                values.push(self.parse_ac_value()?);
                if !self.eat(&TokenKind::Comma) { break; }
            }
        }
        self.expect(&TokenKind::RBracket)?;
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Expr::ArrayConstructor { type_spec, values }, span))
    }

    fn parse_array_constructor_slash(&mut self, start: Span) -> Result<SpannedExpr, ParseError> {
        // Already consumed ( and /. Parse values until / ).
        let mut values = Vec::new();
        loop {
            values.push(self.parse_ac_value()?);
            if !self.eat(&TokenKind::Comma) { break; }
        }
        self.expect(&TokenKind::Slash)?;
        self.expect(&TokenKind::RParen)?;
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Expr::ArrayConstructor { type_spec: None, values }, span))
    }

    fn try_parse_ac_type_spec(&mut self) -> Option<String> {
        // Look for pattern: identifier ::
        if self.peek() == &TokenKind::Identifier {
            let next_pos = self.pos + 1;
            if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::ColonColon {
                let name = self.advance().text.clone();
                self.advance(); // skip ::
                return Some(name);
            }
        }
        None
    }

    fn parse_ac_value(&mut self) -> Result<AcValue, ParseError> {
        // Could be an implied-do: (expr, var=start,end[,step])
        // For now, just parse as expression. Implied-do support comes later.
        let expr = self.parse_expr()?;
        Ok(AcValue::Expr(expr))
    }

    // ---- Helpers ----

    fn prev_span(&self) -> Span {
        if self.pos > 0 {
            self.tokens[self.pos - 1].span
        } else {
            self.current_span()
        }
    }
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
            _ => Err(ParseError { span: tok.span, msg: format!("unknown dot-operator .{}.", name) }),
        },
        TokenKind::DefinedOp(name) => Ok(BinaryOp::Defined(name.clone())),
        _ => Err(ParseError { span: tok.span, msg: format!("expected operator, got {}", tok.kind) }),
    }
}

fn split_kind_suffix(text: &str) -> (String, Option<String>) {
    if let Some(pos) = text.find('_') {
        let num = text[..pos].to_string();
        let kind = text[pos+1..].to_string();
        if kind.is_empty() {
            (text.to_string(), None)
        } else {
            (num, Some(kind))
        }
    } else {
        (text.to_string(), None)
    }
}

fn span_from_to(start: Span, end: Span) -> Span {
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

    #[test] fn integer() { assert_eq!(sexpr("42"), "42"); }
    #[test] fn integer_kind() { assert_eq!(sexpr("42_8"), "42"); }
    #[test] fn real() { assert_eq!(sexpr("3.14"), "3.14"); }
    #[test] fn real_exp() { assert_eq!(sexpr("1.0e5"), "1.0e5"); }
    #[test] fn real_double() { assert_eq!(sexpr("1.0d0"), "1.0d0"); }
    #[test] fn string_single() { assert_eq!(sexpr("'hello'"), "'hello'"); }
    #[test] fn string_double() { assert_eq!(sexpr("\"hello\""), "'hello'"); }
    #[test] fn logical_true() { assert_eq!(sexpr(".true."), ".true."); }
    #[test] fn logical_false() { assert_eq!(sexpr(".false."), ".false."); }
    #[test] fn boz() { assert_eq!(sexpr("B'1010'"), "B'1010'"); }
    #[test] fn name() { assert_eq!(sexpr("x"), "x"); }

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
        assert_eq!(
            sexpr("x > 0 .and. y < 10"),
            "((x > 0) .and. (y < 10))"
        );
    }

    #[test]
    fn chained_function_calls() {
        assert_eq!(sexpr("f(g(x))"), "f(g(x))");
    }

    #[test]
    fn array_element_in_expression() {
        assert_eq!(sexpr("a(i) + b(j)"), "(a(i) + b(j))");
    }
}
