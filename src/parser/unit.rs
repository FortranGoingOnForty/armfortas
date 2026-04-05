//! Program unit parser.
//!
//! Parses top-level Fortran compilation units: programs, modules,
//! subroutines, functions, and interface blocks.

use crate::ast::Spanned;
use crate::ast::unit::*;
use crate::ast::decl::SpannedDecl;
use crate::ast::stmt::SpannedStmt;
use crate::lexer::TokenKind;
use super::{Parser, ParseError};
use super::expr::span_from_to;

impl<'a> Parser<'a> {
    /// Parse a complete Fortran source file — one or more program units.
    pub fn parse_file(&mut self) -> Result<Vec<SpannedUnit>, ParseError> {
        let mut units = Vec::new();
        loop {
            self.skip_newlines();
            if self.peek() == &TokenKind::Eof { break; }
            units.push(self.parse_program_unit()?);
        }
        Ok(units)
    }

    /// Parse a single program unit.
    pub fn parse_program_unit(&mut self) -> Result<SpannedUnit, ParseError> {
        self.skip_newlines();
        let start = self.current_span();

        // Collect prefixes (pure, elemental, recursive, etc.).
        let mut prefixes = Vec::new();
        loop {
            let text = self.peek_text().to_lowercase();
            match text.as_str() {
                "pure" => { self.advance(); prefixes.push(Prefix::Pure); }
                "impure" => { self.advance(); prefixes.push(Prefix::Impure); }
                "elemental" => { self.advance(); prefixes.push(Prefix::Elemental); }
                "recursive" => { self.advance(); prefixes.push(Prefix::Recursive); }
                "non_recursive" => { self.advance(); prefixes.push(Prefix::NonRecursive); }
                "module" => {
                    // "module" could be prefix or the MODULE keyword itself.
                    let next = if self.pos + 1 < self.tokens.len() {
                        self.tokens[self.pos + 1].text.to_lowercase()
                    } else { String::new() };
                    if matches!(next.as_str(), "subroutine" | "function" | "procedure") {
                        self.advance();
                        prefixes.push(Prefix::Module);
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Check for return type before function keyword.
        let mut return_type = None;
        if let Some(ts_result) = self.try_parse_type_spec() {
            return_type = Some(ts_result?);
            // This must be followed by "function".
        }

        let text = self.peek_text().to_lowercase();
        match text.as_str() {
            "program" => self.parse_program(start),
            "module" => self.parse_module(start),
            "submodule" => self.parse_submodule(start),
            "subroutine" => self.parse_subroutine(start, prefixes),
            "function" => self.parse_function(start, prefixes, return_type),
            "blockdata" | "block" => {
                if text == "block" && self.pos + 1 < self.tokens.len()
                    && self.tokens[self.pos + 1].text.eq_ignore_ascii_case("data")
                {
                    self.parse_block_data(start)
                } else if !prefixes.is_empty() || return_type.is_some() {
                    Err(self.error("expected 'subroutine' or 'function' after prefixes".into()))
                } else {
                    Err(self.error(format!("expected program unit keyword, got '{}'", text)))
                }
            }
            "interface" | "abstract" => self.parse_interface_block(start),
            _ => {
                if return_type.is_some() {
                    // Had a type spec — must be a function.
                    if self.peek_text().eq_ignore_ascii_case("function") {
                        self.parse_function(start, prefixes, return_type)
                    } else {
                        Err(self.error("expected 'function' after type specifier".into()))
                    }
                } else {
                    // Implicit main program (no PROGRAM keyword).
                    self.parse_implicit_program(start)
                }
            }
        }
    }

    fn parse_program(&mut self, start: crate::lexer::Span) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'program'
        let name = if self.peek() == &TokenKind::Identifier {
            Some(self.advance().clone().text)
        } else { None };
        self.skip_newlines();

        let (uses, imports, implicit, decls, body) = self.parse_unit_body(&["program"])?;
        let contains = self.parse_contains_section()?;
        self.consume_end("program")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(ProgramUnit::Program { name, uses, imports, implicit, decls, body, contains }, span))
    }

    fn parse_implicit_program(&mut self, start: crate::lexer::Span) -> Result<SpannedUnit, ParseError> {
        // No PROGRAM keyword — implicit main program.
        let (uses, imports, implicit, decls, body) = self.parse_unit_body(&["program"])?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(ProgramUnit::Program {
            name: None, uses, imports, implicit, decls, body, contains: Vec::new(),
        }, span))
    }

    fn parse_module(&mut self, start: crate::lexer::Span) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'module'
        let name = self.advance().clone().text;
        self.skip_newlines();

        let (uses, imports, implicit, decls, _body) = self.parse_unit_body(&["module"])?;
        let contains = self.parse_contains_section()?;
        self.consume_end("module")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(ProgramUnit::Module { name, uses, imports, implicit, decls, contains }, span))
    }

    fn parse_submodule(&mut self, start: crate::lexer::Span) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'submodule'
        self.expect(&TokenKind::LParen)?;
        let parent = self.advance().clone().text;
        let ancestor = if self.eat(&TokenKind::Colon) {
            Some(self.advance().clone().text)
        } else { None };
        self.expect(&TokenKind::RParen)?;
        let name = self.advance().clone().text;
        self.skip_newlines();

        let (uses, _imports, _implicit, decls, _body) = self.parse_unit_body(&["submodule"])?;
        let contains = self.parse_contains_section()?;
        self.consume_end("submodule")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(ProgramUnit::Submodule { parent, ancestor, name, uses, decls, contains }, span))
    }

    fn parse_subroutine(&mut self, start: crate::lexer::Span, prefix: Vec<Prefix>) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'subroutine'
        let name = self.advance().clone().text;

        let args = if self.eat(&TokenKind::LParen) {
            let a = self.parse_dummy_arg_list()?;
            self.expect(&TokenKind::RParen)?;
            a
        } else { Vec::new() };

        let bind = self.try_parse_bind()?;
        self.skip_newlines();

        let (uses, imports, implicit, decls, body) = self.parse_unit_body(&["subroutine"])?;
        let contains = self.parse_contains_section()?;
        self.consume_end("subroutine")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(ProgramUnit::Subroutine {
            name, args, bind, prefix, uses, imports, implicit, decls, body, contains,
        }, span))
    }

    fn parse_function(
        &mut self,
        start: crate::lexer::Span,
        prefix: Vec<Prefix>,
        return_type: Option<crate::ast::decl::TypeSpec>,
    ) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'function'
        let name = self.advance().clone().text;

        self.expect(&TokenKind::LParen)?;
        let args = self.parse_dummy_arg_list()?;
        self.expect(&TokenKind::RParen)?;

        // RESULT clause.
        let result = if self.peek_text().eq_ignore_ascii_case("result") {
            self.advance();
            self.expect(&TokenKind::LParen)?;
            let r = self.advance().clone().text;
            self.expect(&TokenKind::RParen)?;
            Some(r)
        } else { None };

        let bind = self.try_parse_bind()?;
        self.skip_newlines();

        let (uses, imports, implicit, decls, body) = self.parse_unit_body(&["function"])?;
        let contains = self.parse_contains_section()?;
        self.consume_end("function")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(ProgramUnit::Function {
            name, args, result, return_type, bind, prefix, uses, imports, implicit, decls, body, contains,
        }, span))
    }

    fn parse_block_data(&mut self, start: crate::lexer::Span) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'block'
        self.advance(); // consume 'data'
        let name = if self.peek() == &TokenKind::Identifier {
            Some(self.advance().clone().text)
        } else { None };
        self.skip_newlines();

        let (uses, _imports, _implicit, decls, _body) = self.parse_unit_body(&["blockdata", "block"])?;
        // End block data.
        self.skip_newlines();
        let text = self.peek_text().to_lowercase();
        if text == "endblockdata" {
            self.advance();
        } else if text == "end" {
            self.advance();
            self.eat_ident("block");
            self.eat_ident("data");
        }

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(ProgramUnit::BlockData { name, uses, decls }, span))
    }

    fn parse_interface_block(&mut self, start: crate::lexer::Span) -> Result<SpannedUnit, ParseError> {
        let is_abstract = if self.peek_text().eq_ignore_ascii_case("abstract") {
            self.advance();
            true
        } else { false };
        self.advance(); // consume 'interface'

        // Optional name or operator/assignment interface.
        // Check operator/assignment BEFORE generic identifier — they lex as identifiers.
        let name = if self.peek_text().eq_ignore_ascii_case("operator") || self.peek_text().eq_ignore_ascii_case("assignment") {
            let op_kw = self.advance().clone().text;
            self.expect(&TokenKind::LParen)?;
            let op = self.advance().clone().text;
            self.expect(&TokenKind::RParen)?;
            Some(format!("{}({})", op_kw, op))
        } else if self.peek() == &TokenKind::Identifier {
            Some(self.advance().clone().text)
        } else { None };
        self.skip_newlines();

        let mut bodies = Vec::new();
        loop {
            self.skip_newlines();
            let text = self.peek_text().to_lowercase();
            if text == "endinterface" || text == "end" { break; }

            if text == "module" {
                let next = if self.pos + 1 < self.tokens.len() {
                    self.tokens[self.pos + 1].text.to_lowercase()
                } else { String::new() };
                if next == "procedure" {
                    self.advance(); // module
                    self.advance(); // procedure
                    self.eat(&TokenKind::ColonColon);
                    let mut names = Vec::new();
                    loop {
                        names.push(self.advance().clone().text);
                        if !self.eat(&TokenKind::Comma) { break; }
                    }
                    bodies.push(InterfaceBody::ModuleProcedure(names));
                    self.skip_newlines();
                    continue;
                }
            }

            // Try parsing as a subprogram.
            let sub = self.parse_program_unit()?;
            bodies.push(InterfaceBody::Subprogram(sub));
        }

        self.consume_end("interface")?;
        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(ProgramUnit::InterfaceBlock { name, is_abstract, bodies }, span))
    }

    // ---- Helpers ----

    /// Parse the body of a program unit: uses, implicit, declarations, then executable statements.
    #[allow(clippy::type_complexity)]
    #[allow(clippy::type_complexity)]
    fn parse_unit_body(&mut self, terminators: &[&str]) -> Result<(Vec<SpannedDecl>, Vec<ImportStmt>, Vec<SpannedDecl>, Vec<SpannedDecl>, Vec<SpannedStmt>), ParseError> {
        let mut uses = Vec::new();
        let mut imports = Vec::new();
        let mut implicit = Vec::new();
        let mut decls = Vec::new();
        let mut body = Vec::new();

        // Phase 1: USE statements.
        loop {
            self.skip_newlines();
            if self.peek_text().eq_ignore_ascii_case("use") {
                self.advance();
                uses.push(self.parse_use_stmt()?);
            } else { break; }
        }

        // Phase 1.5: IMPORT statements.
        loop {
            self.skip_newlines();
            if self.peek_text().eq_ignore_ascii_case("import") {
                self.advance();
                imports.push(self.parse_import()?);
            } else { break; }
        }

        // Phase 2: IMPLICIT statements.
        loop {
            self.skip_newlines();
            if self.peek_text().eq_ignore_ascii_case("implicit") {
                self.advance();
                implicit.push(self.parse_implicit()?);
            } else { break; }
        }

        // Phase 3: Declarations and executable statements.
        // In practice, declarations and statements can be intermixed in modern Fortran.
        // We'll parse everything as statements and let sema separate them.
        loop {
            self.skip_newlines();
            if self.peek() == &TokenKind::Eof { break; }
            let text = self.peek_text().to_lowercase();

            // Check for end of unit.
            if terminators.iter().any(|t| text == format!("end{}", t)) { break; }
            if text == "end" {
                let next = if self.pos + 1 < self.tokens.len() {
                    self.tokens[self.pos + 1].text.to_lowercase()
                } else { String::new() };
                if terminators.iter().any(|t| next == *t) || next.is_empty() || self.at_stmt_end() {
                    break;
                }
            }
            if text == "contains" { break; }

            // Check for derived type definition: type [, attrs] :: name
            if text == "type" {
                let next_pos = self.pos + 1;
                let next_text = if next_pos < self.tokens.len() {
                    self.tokens[next_pos].text.to_lowercase()
                } else { String::new() };
                // type :: or type , → derived type definition (not type(name) specifier).
                if next_text == "::" || self.tokens.get(next_pos).is_some_and(|t| t.kind == TokenKind::Comma || t.kind == TokenKind::ColonColon) {
                    self.advance(); // consume 'type'
                    decls.push(self.parse_derived_type_def()?);
                    continue;
                }
            }

            // Check for interface block (specification construct).
            // Interface blocks are valid in the specification section of any
            // program unit. Parse and discard — type information is captured
            // by semantic analysis, no IR generation needed.
            if text == "interface" || text == "abstract" {
                let istart = self.current_span();
                let _iface = self.parse_interface_block(istart)?;
                continue;
            }

            // Try as type declaration.
            if let Some(ts_result) = self.try_parse_type_spec() {
                let ts = ts_result?;
                decls.push(self.parse_type_decl(ts)?);
                continue;
            }

            // Try as executable statement.
            body.push(self.parse_stmt()?);
        }

        Ok((uses, imports, implicit, decls, body))
    }

    fn parse_contains_section(&mut self) -> Result<Vec<SpannedUnit>, ParseError> {
        self.skip_newlines();
        if !self.peek_text().eq_ignore_ascii_case("contains") {
            return Ok(Vec::new());
        }
        self.advance(); // consume 'contains'
        self.skip_newlines();

        let mut units = Vec::new();
        loop {
            self.skip_newlines();
            if self.peek() == &TokenKind::Eof { break; }
            let text = self.peek_text().to_lowercase();
            // Only break on END that closes the parent unit — not on inner subprograms'
            // END keywords (those are consumed by parse_program_unit).
            // Combined forms like "endprogram", "endmodule" etc. close the parent.
            if text == "end" {
                let next = if self.pos + 1 < self.tokens.len() {
                    self.tokens[self.pos + 1].text.to_lowercase()
                } else { String::new() };
                // Bare "end" or "end program/module/submodule" closes the parent.
                if next.is_empty() || self.at_stmt_end()
                    || matches!(next.as_str(), "program" | "module" | "submodule" | "subroutine" | "function")
                {
                    break;
                }
            }
            if matches!(text.as_str(), "endprogram" | "endmodule" | "endsubmodule" | "endsubroutine" | "endfunction") {
                break;
            }
            units.push(self.parse_program_unit()?);
        }
        Ok(units)
    }

    fn parse_dummy_arg_list(&mut self) -> Result<Vec<DummyArg>, ParseError> {
        let mut args = Vec::new();
        if self.peek() == &TokenKind::RParen { return Ok(args); }
        loop {
            if self.eat(&TokenKind::Star) {
                args.push(DummyArg::Star);
            } else {
                args.push(DummyArg::Name(self.advance().clone().text));
            }
            if !self.eat(&TokenKind::Comma) { break; }
        }
        Ok(args)
    }

    /// Parse an IMPORT statement.
    pub fn parse_import(&mut self) -> Result<ImportStmt, ParseError> {
        // Already consumed 'import'.
        if self.eat(&TokenKind::Comma) {
            let text = self.peek_text().to_lowercase();
            match text.as_str() {
                "all" => { self.advance(); return Ok(ImportStmt::All); }
                "none" => { self.advance(); return Ok(ImportStmt::None); }
                "only" => {
                    self.advance();
                    self.expect(&TokenKind::Colon)?;
                    let mut names = Vec::new();
                    loop {
                        names.push(self.advance().clone().text);
                        if !self.eat(&TokenKind::Comma) { break; }
                    }
                    return Ok(ImportStmt::Only(names));
                }
                _ => {}
            }
        }
        // import :: name1, name2
        self.eat(&TokenKind::ColonColon);
        let mut names = Vec::new();
        if !self.at_stmt_end() {
            loop {
                names.push(self.advance().clone().text);
                if !self.eat(&TokenKind::Comma) { break; }
            }
        }
        Ok(ImportStmt::Default(names))
    }

    /// Parse optional BIND(C [, NAME="..."]) clause.
    /// Returns `None` if no BIND, `Some(BindInfo)` if present.
    fn try_parse_bind(&mut self) -> Result<Option<BindInfo>, ParseError> {
        if !self.peek_text().eq_ignore_ascii_case("bind") {
            return Ok(None);
        }
        self.advance(); // bind
        self.expect(&TokenKind::LParen)?;
        self.advance(); // c
        let name = if self.eat(&TokenKind::Comma) {
            if self.peek_text().eq_ignore_ascii_case("name") {
                self.advance();
                self.expect(&TokenKind::Assign)?;
                Some(self.advance().clone().text)
            } else { None }
        } else { None };
        self.expect(&TokenKind::RParen)?;
        Ok(Some(BindInfo { name }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse_units(src: &str) -> Vec<SpannedUnit> {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        parser.parse_file().unwrap()
    }

    fn parse_unit(src: &str) -> SpannedUnit {
        let units = parse_units(src);
        assert_eq!(units.len(), 1, "expected 1 unit, got {}", units.len());
        units.into_iter().next().unwrap()
    }

    // ---- PROGRAM ----

    #[test]
    fn simple_program() {
        let u = parse_unit("program hello\n  implicit none\n  integer :: x\n  x = 42\nend program hello\n");
        if let ProgramUnit::Program { name, decls, body, .. } = &u.node {
            assert_eq!(name.as_deref(), Some("hello"));
            assert!(!decls.is_empty());
            assert!(!body.is_empty());
        } else { panic!("not Program"); }
    }

    #[test]
    fn program_with_contains() {
        let u = parse_unit("program main\n  x = 1\ncontains\n  subroutine sub()\n  end subroutine\nend program\n");
        if let ProgramUnit::Program { contains, .. } = &u.node {
            assert_eq!(contains.len(), 1);
        } else { panic!("not Program"); }
    }

    // ---- SUBROUTINE ----

    #[test]
    fn simple_subroutine() {
        let u = parse_unit("subroutine foo(x, y)\n  real :: x, y\nend subroutine\n");
        if let ProgramUnit::Subroutine { name, args, .. } = &u.node {
            assert_eq!(name, "foo");
            assert_eq!(args.len(), 2);
        } else { panic!("not Subroutine"); }
    }

    #[test]
    fn pure_elemental_subroutine() {
        let u = parse_unit("pure elemental subroutine bar(x)\n  real, intent(in) :: x\nend subroutine\n");
        if let ProgramUnit::Subroutine { prefix, .. } = &u.node {
            assert!(prefix.contains(&Prefix::Pure));
            assert!(prefix.contains(&Prefix::Elemental));
        } else { panic!("not Subroutine"); }
    }

    // ---- FUNCTION ----

    #[test]
    fn simple_function() {
        let u = parse_unit("function square(x) result(y)\n  real :: x, y\n  y = x * x\nend function\n");
        if let ProgramUnit::Function { name, result, .. } = &u.node {
            assert_eq!(name, "square");
            assert_eq!(result.as_deref(), Some("y"));
        } else { panic!("not Function"); }
    }

    #[test]
    fn typed_function() {
        let u = parse_unit("real function add(a, b)\n  real :: a, b\n  add = a + b\nend function\n");
        if let ProgramUnit::Function { return_type, .. } = &u.node {
            assert!(return_type.is_some());
        } else { panic!("not Function"); }
    }

    #[test]
    fn recursive_function() {
        let u = parse_unit("recursive function fact(n) result(f)\n  integer :: n, f\n  if (n <= 1) then\n    f = 1\n  else\n    f = n * fact(n - 1)\n  end if\nend function\n");
        if let ProgramUnit::Function { prefix, .. } = &u.node {
            assert!(prefix.contains(&Prefix::Recursive));
        } else { panic!("not Function"); }
    }

    // ---- MODULE ----

    #[test]
    fn simple_module() {
        let u = parse_unit("module my_mod\n  implicit none\n  integer :: x\ncontains\n  subroutine sub()\n  end subroutine\nend module\n");
        if let ProgramUnit::Module { name, contains, .. } = &u.node {
            assert_eq!(name, "my_mod");
            assert_eq!(contains.len(), 1);
        } else { panic!("not Module"); }
    }

    #[test]
    fn module_with_use() {
        let u = parse_unit("module b\n  use a\n  implicit none\nend module\n");
        if let ProgramUnit::Module { uses, .. } = &u.node {
            assert_eq!(uses.len(), 1);
        } else { panic!("not Module"); }
    }

    // ---- INTERFACE ----

    #[test]
    fn interface_explicit() {
        let u = parse_unit("interface\n  subroutine ext(x)\n    real :: x\n  end subroutine\nend interface\n");
        if let ProgramUnit::InterfaceBlock { bodies, .. } = &u.node {
            assert_eq!(bodies.len(), 1);
        } else { panic!("not InterfaceBlock"); }
    }

    #[test]
    fn interface_generic() {
        let u = parse_unit("interface sort\n  module procedure sort_int\n  module procedure sort_real\nend interface\n");
        if let ProgramUnit::InterfaceBlock { name, bodies, .. } = &u.node {
            assert_eq!(name.as_deref(), Some("sort"));
            assert_eq!(bodies.len(), 2);
        } else { panic!("not InterfaceBlock"); }
    }

    // ---- MULTI-UNIT FILES ----

    #[test]
    fn multi_unit_file() {
        let units = parse_units("module m1\nend module\n\nmodule m2\n  use m1\nend module\n\nprogram main\n  use m2\nend program\n");
        assert_eq!(units.len(), 3);
        assert!(matches!(units[0].node, ProgramUnit::Module { .. }));
        assert!(matches!(units[1].node, ProgramUnit::Module { .. }));
        assert!(matches!(units[2].node, ProgramUnit::Program { .. }));
    }

    // ---- BIND(C) ----

    #[test]
    fn subroutine_bind_c() {
        let u = parse_unit("subroutine cfunc(x) bind(c)\n  real :: x\nend subroutine\n");
        if let ProgramUnit::Subroutine { bind, .. } = &u.node {
            assert!(bind.is_some(), "should have BindInfo");
            assert!(bind.as_ref().unwrap().name.is_none(), "no name= specified");
        } else { panic!("not Subroutine"); }
    }

    #[test]
    fn subroutine_bind_c_with_name() {
        let u = parse_unit("subroutine foo(x) bind(c, name='c_foo')\n  real :: x\nend subroutine\n");
        if let ProgramUnit::Subroutine { bind, .. } = &u.node {
            assert!(bind.is_some());
            assert_eq!(bind.as_ref().unwrap().name.as_deref(), Some("'c_foo'"));
        } else { panic!("not Subroutine"); }
    }
}
