//! Statement parser.
//!
//! Parses executable statements: assignments, IF, DO, SELECT CASE,
//! WHERE, FORALL, BLOCK, ASSOCIATE, EXIT, CYCLE, STOP, RETURN, GOTO,
//! CALL, PRINT, and legacy control flow.

use crate::ast::Spanned;
use crate::ast::stmt::*;
use crate::ast::expr::{SpannedExpr, Expr};
use crate::lexer::TokenKind;
use super::{Parser, ParseError};
use super::expr::span_from_to;

impl<'a> Parser<'a> {
    /// Parse a single statement.
    pub fn parse_stmt(&mut self) -> Result<SpannedStmt, ParseError> {
        self.skip_newlines();
        let start = self.current_span();
        let text = self.peek_text().to_lowercase();

        match text.as_str() {
            "if" => self.parse_if(start),
            "do" => self.parse_do(start),
            "select" => self.parse_select(start),
            "where" => self.parse_where_construct(start),
            "forall" => self.parse_forall_construct(start),
            "block" => self.parse_block_construct(start),
            "associate" => self.parse_associate(start),
            "exit" => { self.advance(); self.parse_exit(start) }
            "cycle" => { self.advance(); self.parse_cycle(start) }
            "stop" => { self.advance(); self.parse_stop(start, false) }
            "error" => {
                self.advance();
                if self.peek_text().eq_ignore_ascii_case("stop") {
                    self.advance();
                    self.parse_stop(start, true)
                } else {
                    Err(self.error("expected 'stop' after 'error'".into()))
                }
            }
            "return" => { self.advance(); self.parse_return(start) }
            "goto" | "go" => self.parse_goto(start),
            "call" => { self.advance(); self.parse_call(start) }
            "print" => { self.advance(); self.parse_print(start) }
            "continue" => {
                self.advance();
                let span = span_from_to(start, self.prev_span());
                Ok(Spanned::new(Stmt::Continue { label: None }, span))
            }
            _ => self.parse_assignment_or_call(start),
        }
    }

    /// Parse a block of statements until a terminating keyword.
    pub fn parse_stmt_block(&mut self, terminators: &[&str]) -> Result<Vec<SpannedStmt>, ParseError> {
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.peek() == &TokenKind::Eof { break; }
            let text = self.peek_text().to_lowercase();
            if terminators.iter().any(|t| text == *t || text == format!("end{}", t)) {
                break;
            }
            // Also check for "end" followed by a terminator keyword.
            if text == "end" {
                let next = if self.pos + 1 < self.tokens.len() {
                    self.tokens[self.pos + 1].text.to_lowercase()
                } else { String::new() };
                if terminators.iter().any(|t| next == *t) || next.is_empty() {
                    break;
                }
            }
            // Check for "else", "elsewhere", "case", "contains" which terminate inner blocks.
            if matches!(text.as_str(), "else" | "elseif" | "elsewhere" | "case" | "contains" | "default") {
                break;
            }
            stmts.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        Ok(stmts)
    }

    // ---- IF ----

    fn parse_if(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        self.advance(); // consume 'if'
        self.expect(&TokenKind::LParen)?;
        let condition = self.parse_expr()?;
        self.expect(&TokenKind::RParen)?;

        // Check for THEN → block IF construct.
        if self.peek_text().eq_ignore_ascii_case("then") {
            self.advance();
            return self.parse_if_construct(start, None, condition);
        }

        // Single-line IF: if (cond) action
        let action = self.parse_stmt()?;
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::IfStmt {
            condition,
            action: Box::new(action),
        }, span))
    }

    fn parse_if_construct(
        &mut self,
        start: crate::lexer::Span,
        name: Option<String>,
        condition: SpannedExpr,
    ) -> Result<SpannedStmt, ParseError> {
        let then_body = self.parse_stmt_block(&["if"])?;
        let mut else_ifs = Vec::new();
        let mut else_body = None;

        loop {
            self.skip_newlines();
            let text = self.peek_text().to_lowercase();

            if text == "elseif" || text == "else" {
                if text == "elseif" || (text == "else" && {
                    let next = if self.pos + 1 < self.tokens.len() {
                        self.tokens[self.pos + 1].text.to_lowercase()
                    } else { String::new() };
                    next == "if"
                }) {
                    // ELSE IF
                    self.advance(); // else
                    if self.peek_text().eq_ignore_ascii_case("if") {
                        self.advance(); // if
                    }
                    self.expect(&TokenKind::LParen)?;
                    let ei_cond = self.parse_expr()?;
                    self.expect(&TokenKind::RParen)?;
                    if self.peek_text().eq_ignore_ascii_case("then") {
                        self.advance();
                    }
                    let ei_body = self.parse_stmt_block(&["if"])?;
                    else_ifs.push((ei_cond, ei_body));
                    continue;
                }

                // ELSE (no IF)
                self.advance(); // else
                let eb = self.parse_stmt_block(&["if"])?;
                else_body = Some(eb);
                continue;
            }

            break;
        }

        // Consume END IF / ENDIF
        self.skip_newlines();
        let end_text = self.peek_text().to_lowercase();
        if end_text == "endif" {
            self.advance();
        } else if end_text == "end" {
            self.advance();
            self.eat_ident("if");
        }
        // Optional name after end if.
        if self.peek() == &TokenKind::Identifier && name.is_some() {
            self.advance();
        }

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::IfConstruct {
            name, condition, then_body, else_ifs, else_body,
        }, span))
    }

    // ---- DO ----

    fn parse_do(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        self.advance(); // consume 'do'

        // DO WHILE
        if self.peek_text().eq_ignore_ascii_case("while") {
            self.advance();
            self.expect(&TokenKind::LParen)?;
            let condition = self.parse_expr()?;
            self.expect(&TokenKind::RParen)?;
            let body = self.parse_stmt_block(&["do"])?;
            self.consume_end("do");
            let span = span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Stmt::DoWhile { name: None, condition, body }, span));
        }

        // DO CONCURRENT
        if self.peek_text().eq_ignore_ascii_case("concurrent") {
            self.advance();
            self.expect(&TokenKind::LParen)?;
            let mut controls = Vec::new();
            loop {
                let var = self.advance().clone().text;
                self.expect(&TokenKind::Assign)?;
                let ctrl_start = self.parse_expr()?;
                self.expect(&TokenKind::Colon)?;
                let end = self.parse_expr()?;
                let step = if self.eat(&TokenKind::Colon) { Some(self.parse_expr()?) } else { None };
                controls.push(ConcurrentControl { var, start: ctrl_start, end, step });
                if !self.eat(&TokenKind::Comma) { break; }
                // Check for mask expression after controls.
                if self.peek() != &TokenKind::Identifier || {
                    let np = self.pos + 1;
                    np >= self.tokens.len() || self.tokens[np].kind != TokenKind::Assign
                } {
                    break;
                }
            }
            let mask = if self.peek() != &TokenKind::RParen {
                Some(self.parse_expr()?)
            } else { None };
            self.expect(&TokenKind::RParen)?;
            let body = self.parse_stmt_block(&["do"])?;
            self.consume_end("do");
            let span = span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Stmt::DoConcurrent { name: None, controls, mask, body }, span));
        }

        // Infinite DO (no variable).
        if self.at_stmt_end() {
            let body = self.parse_stmt_block(&["do"])?;
            self.consume_end("do");
            let span = span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Stmt::DoLoop {
                name: None, var: None, start: None, end: None, step: None, body,
            }, span));
        }

        // Counted DO: do var = start, end [, step]
        let var = self.advance().clone().text;
        self.expect(&TokenKind::Assign)?;
        let do_start = self.parse_expr()?;
        self.expect(&TokenKind::Comma)?;
        let do_end = self.parse_expr()?;
        let step = if self.eat(&TokenKind::Comma) { Some(self.parse_expr()?) } else { None };
        let body = self.parse_stmt_block(&["do"])?;
        self.consume_end("do");
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::DoLoop {
            name: None, var: Some(var), start: Some(do_start), end: Some(do_end), step, body,
        }, span))
    }

    // ---- SELECT CASE ----

    fn parse_select(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        self.advance(); // consume 'select'
        self.eat_ident("case");
        self.expect(&TokenKind::LParen)?;
        let selector = self.parse_expr()?;
        self.expect(&TokenKind::RParen)?;

        let mut cases = Vec::new();
        loop {
            self.skip_newlines();
            let text = self.peek_text().to_lowercase();
            if text == "case" {
                self.advance();
                let selectors = self.parse_case_selectors()?;
                let body = self.parse_stmt_block(&["select"])?;
                cases.push(CaseBlock { selectors, body });
            } else {
                break;
            }
        }
        self.consume_end("select");
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::SelectCase { name: None, selector, cases }, span))
    }

    fn parse_case_selectors(&mut self) -> Result<Vec<CaseSelector>, ParseError> {
        if self.peek_text().eq_ignore_ascii_case("default") {
            self.advance();
            return Ok(vec![CaseSelector::Default]);
        }
        self.expect(&TokenKind::LParen)?;
        let mut selectors = Vec::new();
        loop {
            // Check for range: low:high, :high, low:
            if self.peek() == &TokenKind::Colon {
                self.advance();
                let high = self.parse_expr()?;
                selectors.push(CaseSelector::Range { low: None, high: Some(high) });
            } else {
                let val = self.parse_expr()?;
                if self.eat(&TokenKind::Colon) {
                    if matches!(self.peek(), TokenKind::Comma | TokenKind::RParen) {
                        selectors.push(CaseSelector::Range { low: Some(val), high: None });
                    } else {
                        let high = self.parse_expr()?;
                        selectors.push(CaseSelector::Range { low: Some(val), high: Some(high) });
                    }
                } else {
                    selectors.push(CaseSelector::Value(val));
                }
            }
            if !self.eat(&TokenKind::Comma) { break; }
        }
        self.expect(&TokenKind::RParen)?;
        Ok(selectors)
    }

    // ---- Simple statements ----

    fn parse_exit(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        let name = if self.peek() == &TokenKind::Identifier && !self.at_stmt_end() {
            Some(self.advance().clone().text)
        } else { None };
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::Exit { name }, span))
    }

    fn parse_cycle(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        let name = if self.peek() == &TokenKind::Identifier && !self.at_stmt_end() {
            Some(self.advance().clone().text)
        } else { None };
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::Cycle { name }, span))
    }

    fn parse_stop(&mut self, start: crate::lexer::Span, is_error: bool) -> Result<SpannedStmt, ParseError> {
        let code = if !self.at_stmt_end() {
            Some(self.parse_expr()?)
        } else { None };
        let span = span_from_to(start, self.prev_span());
        if is_error {
            Ok(Spanned::new(Stmt::ErrorStop { code, quiet: false }, span))
        } else {
            Ok(Spanned::new(Stmt::Stop { code, quiet: false }, span))
        }
    }

    fn parse_return(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        let value = if !self.at_stmt_end() {
            Some(self.parse_expr()?)
        } else { None };
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::Return { value }, span))
    }

    fn parse_goto(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        self.advance(); // consume 'goto' or 'go'
        if self.peek_text().eq_ignore_ascii_case("to") {
            self.advance();
        }
        // Plain GOTO label.
        if self.peek() == &TokenKind::IntegerLiteral {
            let label: u64 = self.advance().clone().text.parse().unwrap_or(0);
            let span = span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Stmt::Goto { label }, span));
        }
        // Computed GOTO: (label-list), selector
        if self.peek() == &TokenKind::LParen {
            self.advance();
            let mut labels = Vec::new();
            loop {
                let l: u64 = self.advance().clone().text.parse().unwrap_or(0);
                labels.push(l);
                if !self.eat(&TokenKind::Comma) { break; }
            }
            self.expect(&TokenKind::RParen)?;
            self.eat(&TokenKind::Comma);
            let selector = self.parse_expr()?;
            let span = span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Stmt::ComputedGoto { labels, selector }, span));
        }
        Err(self.error("expected label or (label-list) after GOTO".into()))
    }

    fn parse_call(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        let callee = self.parse_expr()?;
        let span = span_from_to(start, self.prev_span());
        // The expression parser handles the (args) part as FunctionCall.
        // Extract the args from the FunctionCall if present.
        if let Expr::FunctionCall { callee: inner, args } = callee.node {
            Ok(Spanned::new(Stmt::Call { callee: *inner, args }, span))
        } else {
            // Call with no arguments: call sub
            Ok(Spanned::new(Stmt::Call { callee, args: Vec::new() }, span))
        }
    }

    fn parse_print(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        // Format can be * (list-directed), a label, or a format string.
        let format = if self.peek() == &TokenKind::Star {
            let tok = self.advance().clone();
            Spanned::new(Expr::Name { name: "*".into() }, tok.span)
        } else {
            self.parse_expr()?
        };
        let mut items = Vec::new();
        if self.eat(&TokenKind::Comma) {
            loop {
                items.push(self.parse_expr()?);
                if !self.eat(&TokenKind::Comma) { break; }
            }
        }
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::Print { format, items }, span))
    }

    fn parse_assignment_or_call(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        let target = self.parse_expr()?;

        if self.eat(&TokenKind::Assign) {
            let value = self.parse_expr()?;
            let span = span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Stmt::Assignment { target, value }, span));
        }

        if self.eat(&TokenKind::Arrow) {
            let value = self.parse_expr()?;
            let span = span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Stmt::PointerAssignment { target, value }, span));
        }

        // Bare expression as statement (e.g., function call without CALL keyword).
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::Call { callee: target, args: Vec::new() }, span))
    }

    // ---- WHERE / FORALL / BLOCK / ASSOCIATE stubs ----

    fn parse_where_construct(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        self.advance(); // consume 'where'
        self.expect(&TokenKind::LParen)?;
        let mask = self.parse_expr()?;
        self.expect(&TokenKind::RParen)?;

        // Single-line WHERE: where (mask) stmt
        if !self.at_stmt_end() && !self.peek_text().eq_ignore_ascii_case("then") {
            // Check if this looks like a statement, not a newline.
            let action = self.parse_stmt()?;
            let span = span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Stmt::WhereStmt { mask, stmt: Box::new(action) }, span));
        }

        let body = self.parse_stmt_block(&["where"])?;
        let mut elsewhere = Vec::new();
        while self.peek_text().eq_ignore_ascii_case("elsewhere") {
            self.advance();
            let ew_mask = if self.peek() == &TokenKind::LParen {
                self.advance();
                let m = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Some(m)
            } else { None };
            let ew_body = self.parse_stmt_block(&["where"])?;
            elsewhere.push((ew_mask, ew_body));
        }
        self.consume_end("where");
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::WhereConstruct { name: None, mask, body, elsewhere }, span))
    }

    fn parse_forall_construct(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        self.advance(); // consume 'forall'
        self.expect(&TokenKind::LParen)?;
        let mut specs = Vec::new();
        loop {
            let var = self.advance().clone().text;
            self.expect(&TokenKind::Assign)?;
            let fs_start = self.parse_expr()?;
            self.expect(&TokenKind::Colon)?;
            let end = self.parse_expr()?;
            let step = if self.eat(&TokenKind::Colon) { Some(self.parse_expr()?) } else { None };
            specs.push(ForallSpec { var, start: fs_start, end, step });
            if !self.eat(&TokenKind::Comma) { break; }
            // Check if next is a control or mask.
            if self.peek() != &TokenKind::Identifier || {
                let np = self.pos + 1;
                np >= self.tokens.len() || self.tokens[np].kind != TokenKind::Assign
            } { break; }
        }
        let mask = if self.peek() != &TokenKind::RParen { Some(self.parse_expr()?) } else { None };
        self.expect(&TokenKind::RParen)?;

        // Single-line FORALL or block.
        if !self.at_stmt_end() {
            let action = self.parse_stmt()?;
            let span = span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Stmt::ForallStmt { specs, mask, stmt: Box::new(action) }, span));
        }

        let body = self.parse_stmt_block(&["forall"])?;
        self.consume_end("forall");
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::ForallConstruct { name: None, specs, mask, body }, span))
    }

    fn parse_block_construct(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        self.advance(); // consume 'block'
        let body = self.parse_stmt_block(&["block"])?;
        self.consume_end("block");
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::Block { name: None, body }, span))
    }

    fn parse_associate(&mut self, start: crate::lexer::Span) -> Result<SpannedStmt, ParseError> {
        self.advance(); // consume 'associate'
        self.expect(&TokenKind::LParen)?;
        let mut assocs = Vec::new();
        loop {
            let name = self.advance().clone().text;
            self.expect(&TokenKind::Arrow)?;
            let expr = self.parse_expr()?;
            assocs.push((name, expr));
            if !self.eat(&TokenKind::Comma) { break; }
        }
        self.expect(&TokenKind::RParen)?;
        let body = self.parse_stmt_block(&["associate"])?;
        self.consume_end("associate");
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(Stmt::Associate { name: None, assocs, body }, span))
    }

    // ---- Helpers ----

    fn consume_end(&mut self, keyword: &str) {
        self.skip_newlines();
        let text = self.peek_text().to_lowercase();
        let combined = format!("end{}", keyword);
        if text == combined {
            self.advance();
        } else if text == "end" {
            self.advance();
            self.eat_ident(keyword);
        }
        // Skip optional construct name after end.
        if self.peek() == &TokenKind::Identifier {
            self.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse_one(src: &str) -> SpannedStmt {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        parser.parse_stmt().unwrap()
    }

    // ---- Assignment ----

    #[test]
    fn assignment() {
        let s = parse_one("x = 42\n");
        assert!(matches!(s.node, Stmt::Assignment { .. }));
    }

    #[test]
    fn pointer_assignment() {
        let s = parse_one("ptr => target\n");
        assert!(matches!(s.node, Stmt::PointerAssignment { .. }));
    }

    // ---- IF ----

    #[test]
    fn if_single_line() {
        let s = parse_one("if (x > 0) y = 1\n");
        assert!(matches!(s.node, Stmt::IfStmt { .. }));
    }

    #[test]
    fn if_construct() {
        let s = parse_one("if (x > 0) then\n  y = 1\nend if\n");
        if let Stmt::IfConstruct { then_body, else_ifs, else_body, .. } = &s.node {
            assert_eq!(then_body.len(), 1);
            assert!(else_ifs.is_empty());
            assert!(else_body.is_none());
        } else { panic!("not IfConstruct"); }
    }

    #[test]
    fn if_else() {
        let s = parse_one("if (x > 0) then\n  y = 1\nelse\n  y = 2\nend if\n");
        if let Stmt::IfConstruct { else_body, .. } = &s.node {
            assert!(else_body.is_some());
        } else { panic!("not IfConstruct"); }
    }

    #[test]
    fn if_elseif() {
        let s = parse_one("if (x > 0) then\n  y = 1\nelse if (x < 0) then\n  y = 2\nelse\n  y = 0\nend if\n");
        if let Stmt::IfConstruct { else_ifs, else_body, .. } = &s.node {
            assert_eq!(else_ifs.len(), 1);
            assert!(else_body.is_some());
        } else { panic!("not IfConstruct"); }
    }

    // ---- DO ----

    #[test]
    fn do_counted() {
        let s = parse_one("do i = 1, 10\n  x = i\nend do\n");
        if let Stmt::DoLoop { var, start, end, step, body, .. } = &s.node {
            assert_eq!(var.as_deref(), Some("i"));
            assert!(start.is_some());
            assert!(end.is_some());
            assert!(step.is_none());
            assert_eq!(body.len(), 1);
        } else { panic!("not DoLoop"); }
    }

    #[test]
    fn do_with_step() {
        let s = parse_one("do i = 10, 1, -1\n  x = i\nend do\n");
        if let Stmt::DoLoop { step, .. } = &s.node {
            assert!(step.is_some());
        } else { panic!("not DoLoop"); }
    }

    #[test]
    fn do_while() {
        let s = parse_one("do while (x > 0)\n  x = x - 1\nend do\n");
        assert!(matches!(s.node, Stmt::DoWhile { .. }));
    }

    #[test]
    fn do_infinite() {
        let s = parse_one("do\n  if (done) exit\nend do\n");
        if let Stmt::DoLoop { var, .. } = &s.node {
            assert!(var.is_none());
        } else { panic!("not DoLoop"); }
    }

    // ---- SELECT CASE ----

    #[test]
    fn select_case() {
        let s = parse_one("select case (x)\ncase (1)\n  y = 1\ncase (2)\n  y = 2\ncase default\n  y = 0\nend select\n");
        if let Stmt::SelectCase { cases, .. } = &s.node {
            assert_eq!(cases.len(), 3);
            assert!(matches!(cases[2].selectors[0], CaseSelector::Default));
        } else { panic!("not SelectCase"); }
    }

    #[test]
    fn select_case_range() {
        let s = parse_one("select case (x)\ncase (1:10)\n  y = 1\nend select\n");
        if let Stmt::SelectCase { cases, .. } = &s.node {
            assert!(matches!(cases[0].selectors[0], CaseSelector::Range { .. }));
        } else { panic!("not SelectCase"); }
    }

    // ---- Simple statements ----

    #[test]
    fn exit_stmt() {
        let s = parse_one("exit\n");
        assert!(matches!(s.node, Stmt::Exit { name: None }));
    }

    #[test]
    fn exit_named() {
        let s = parse_one("exit outer\n");
        if let Stmt::Exit { name } = &s.node {
            assert_eq!(name.as_deref(), Some("outer"));
        } else { panic!("not Exit"); }
    }

    #[test]
    fn cycle_stmt() {
        let s = parse_one("cycle\n");
        assert!(matches!(s.node, Stmt::Cycle { name: None }));
    }

    #[test]
    fn stop_stmt() {
        let s = parse_one("stop\n");
        assert!(matches!(s.node, Stmt::Stop { code: None, .. }));
    }

    #[test]
    fn stop_with_code() {
        let s = parse_one("stop 1\n");
        assert!(matches!(s.node, Stmt::Stop { code: Some(_), .. }));
    }

    #[test]
    fn error_stop() {
        let s = parse_one("error stop\n");
        assert!(matches!(s.node, Stmt::ErrorStop { .. }));
    }

    #[test]
    fn return_stmt() {
        let s = parse_one("return\n");
        assert!(matches!(s.node, Stmt::Return { value: None }));
    }

    #[test]
    fn goto_stmt() {
        let s = parse_one("goto 100\n");
        if let Stmt::Goto { label } = &s.node {
            assert_eq!(*label, 100);
        } else { panic!("not Goto"); }
    }

    #[test]
    fn call_stmt() {
        let s = parse_one("call sub(a, b)\n");
        assert!(matches!(s.node, Stmt::Call { .. }));
    }

    #[test]
    fn print_stmt() {
        let s = parse_one("print *, x, y\n");
        if let Stmt::Print { items, .. } = &s.node {
            assert_eq!(items.len(), 2);
        } else { panic!("not Print"); }
    }

    #[test]
    fn continue_stmt() {
        let s = parse_one("continue\n");
        assert!(matches!(s.node, Stmt::Continue { .. }));
    }
}
