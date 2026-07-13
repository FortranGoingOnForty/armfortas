//! Program unit parser.
//!
//! Parses top-level Fortran compilation units: programs, modules,
//! subroutines, functions, and interface blocks.

use super::expr::span_from_to;
use super::{ParseError, Parser};
use crate::ast::decl::SpannedDecl;
use crate::ast::stmt::SpannedStmt;
use crate::ast::unit::*;
use crate::ast::Spanned;
use crate::lexer::TokenKind;

impl<'a> Parser<'a> {
    /// Parse a complete Fortran source file — one or more program units.
    pub fn parse_file(&mut self) -> Result<Vec<SpannedUnit>, ParseError> {
        let mut units = Vec::new();
        loop {
            self.skip_newlines();
            if self.peek() == &TokenKind::Eof {
                break;
            }
            // Progress guard: a unit parse that consumes no tokens
            // would loop here forever, allocating units until OOM
            // (seen with a stray attribute statement after a bare-main
            // CONTAINS function). Any zero-progress Ok is a parser
            // bug surfaced as a clean error, never a spin.
            let before = self.pos;
            units.push(self.parse_program_unit()?);
            if self.pos == before {
                return Err(self.error(format!(
                    "parser made no progress at '{}' (internal error — please report)",
                    self.peek_text()
                )));
            }
        }
        Ok(units)
    }

    /// Parse a single program unit.
    pub fn parse_program_unit(&mut self) -> Result<SpannedUnit, ParseError> {
        self.skip_newlines();
        let start = self.current_span();

        // Prefixes and a single optional return-type spec may appear in
        // any order before `function` / `subroutine` / `procedure`.
        // Fortran 2008 R1226: prefix-spec ::= type-spec | declaration-prefix
        // where declaration-prefix is one of pure/impure/elemental/
        // recursive/non_recursive/module.  Stdlib uses every order:
        //   pure module function foo
        //   elemental module logical function bar
        //   logical pure module function baz
        //   pure real(sp) module function qux
        let mut prefixes: Vec<Prefix> = Vec::new();
        let mut return_type: Option<crate::ast::decl::TypeSpec> = None;
        loop {
            let text = self.peek_text().to_lowercase();
            match text.as_str() {
                "pure" => {
                    self.advance();
                    prefixes.push(Prefix::Pure);
                }
                "impure" => {
                    self.advance();
                    prefixes.push(Prefix::Impure);
                }
                "elemental" => {
                    self.advance();
                    prefixes.push(Prefix::Elemental);
                }
                "recursive" => {
                    self.advance();
                    prefixes.push(Prefix::Recursive);
                }
                "non_recursive" => {
                    self.advance();
                    prefixes.push(Prefix::NonRecursive);
                }
                "module" => {
                    // `module` is a prefix iff the *eventual* keyword
                    // afterward is subroutine/function/procedure.  The
                    // intervening tokens may be other prefixes or a
                    // type-spec; check the next token cheaply and treat
                    // it as a prefix when it can lead to those keywords.
                    let next = if self.pos + 1 < self.tokens.len() {
                        self.tokens[self.pos + 1].text.to_lowercase()
                    } else {
                        String::new()
                    };
                    let is_simple_prefix =
                        matches!(next.as_str(), "subroutine" | "function" | "procedure");
                    let is_followed_by_decl_prefix = matches!(
                        next.as_str(),
                        "pure" | "impure" | "elemental" | "recursive" | "non_recursive"
                    );
                    let is_type_then_function = matches!(
                        next.as_str(),
                        "integer"
                            | "real"
                            | "double"
                            | "complex"
                            | "logical"
                            | "character"
                            | "type"
                            | "class"
                    );
                    if is_simple_prefix || is_followed_by_decl_prefix || is_type_then_function {
                        self.advance();
                        prefixes.push(Prefix::Module);
                    } else {
                        break;
                    }
                }
                _ => {
                    if return_type.is_none() {
                        if let Some(ts_result) = self.try_parse_type_spec() {
                            return_type = Some(ts_result?);
                            continue;
                        }
                    }
                    break;
                }
            }
        }

        let text = self.peek_text().to_lowercase();
        match text.as_str() {
            "program" => self.parse_program(start),
            "module" => self.parse_module(start),
            "submodule" => self.parse_submodule(start),
            "subroutine" => self.parse_subroutine(start, prefixes),
            "function" => self.parse_function(start, prefixes, return_type),
            // F2008 §12.6.2.5: separate module procedure body
            // (module procedure NAME ... end procedure [NAME])
            // — the procedure's signature is inherited from the
            // parent module's interface block, so args/return type
            // are not repeated here.  Only valid when the `module`
            // prefix was consumed above.
            "procedure" if prefixes.iter().any(|p| matches!(p, Prefix::Module)) => {
                self.parse_separate_module_procedure(start, prefixes)
            }
            "blockdata" | "block" => {
                if text == "block"
                    && self.pos + 1 < self.tokens.len()
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
        } else {
            None
        };
        self.skip_newlines();

        let (uses, imports, implicit, decls, body, ifaces) = self.parse_unit_body(&["program"])?;
        let mut contains = self.parse_contains_section()?;
        contains.extend(ifaces); // Interface blocks resolved by sema, ignored by lowering.
        self.consume_end("program")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            ProgramUnit::Program {
                name,
                uses,
                imports,
                implicit,
                decls,
                body,
                contains,
            },
            span,
        ))
    }

    fn parse_implicit_program(
        &mut self,
        start: crate::lexer::Span,
    ) -> Result<SpannedUnit, ParseError> {
        // No PROGRAM keyword — implicit main program.
        let (uses, imports, implicit, decls, body, ifaces) = self.parse_unit_body(&["program"])?;

        // A bare main may carry internal procedures (F2018 R1401: the
        // program-stmt is optional, the rest of main-program is not
        // restricted). Without this, `contains` was left unconsumed
        // and parse_file spun on it (OOM via the unit Vec before the
        // progress guard existed).
        self.skip_newlines();
        let mut contains = if self.peek_text().eq_ignore_ascii_case("contains") {
            self.parse_contains_section()?
        } else {
            Vec::new()
        };
        contains.extend(ifaces);

        // Consume the END [PROGRAM] if present — parse_unit_body breaks
        // *before* consuming the terminator, so we must advance past it
        // or parse_file will re-enter parse_program_unit at the same
        // position forever.
        self.skip_newlines();
        if self.peek() != &TokenKind::Eof {
            let _ = self.consume_end("program");
        }

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            ProgramUnit::Program {
                name: None,
                uses,
                imports,
                implicit,
                decls,
                body,
                contains,
            },
            span,
        ))
    }

    fn parse_module(&mut self, start: crate::lexer::Span) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'module'
        let name = self.advance().clone().text;
        self.skip_newlines();

        let (uses, imports, implicit, decls, _body, ifaces) = self.parse_unit_body(&["module"])?;
        let mut contains = self.parse_contains_section()?;
        contains.extend(ifaces);
        self.consume_end("module")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            ProgramUnit::Module {
                name,
                uses,
                imports,
                implicit,
                decls,
                contains,
            },
            span,
        ))
    }

    fn parse_submodule(&mut self, start: crate::lexer::Span) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'submodule'
        self.expect(&TokenKind::LParen)?;
        let parent = self.advance().clone().text;
        let ancestor = if self.eat(&TokenKind::Colon) {
            Some(self.advance().clone().text)
        } else {
            None
        };
        self.expect(&TokenKind::RParen)?;
        let name = self.advance().clone().text;
        self.skip_newlines();

        let (uses, imports, implicit, decls, _body, ifaces) =
            self.parse_unit_body(&["submodule"])?;
        let mut contains = self.parse_contains_section()?;
        // Carry interface blocks declared at the submodule's
        // specification section into `contains` so sema sees them
        // (without this, generic interfaces declared inside the
        // submodule — e.g. stdlib_quadrature_simps's
        // `interface simps38_weights` — are silently dropped).
        contains.extend(ifaces);
        self.consume_end("submodule")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            ProgramUnit::Submodule {
                parent,
                ancestor,
                name,
                uses,
                imports,
                implicit,
                decls,
                contains,
            },
            span,
        ))
    }

    fn parse_subroutine(
        &mut self,
        start: crate::lexer::Span,
        prefix: Vec<Prefix>,
    ) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'subroutine'
        let name = self.advance().clone().text;

        let args = if self.eat(&TokenKind::LParen) {
            let a = self.parse_dummy_arg_list()?;
            self.expect(&TokenKind::RParen)?;
            a
        } else {
            Vec::new()
        };

        let bind = self.try_parse_bind()?;
        self.skip_newlines();

        let (uses, imports, implicit, decls, body, ifaces) =
            self.parse_unit_body(&["subroutine"])?;
        let mut contains = self.parse_contains_section()?;
        contains.extend(ifaces);
        self.consume_end("subroutine")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            ProgramUnit::Subroutine {
                name,
                args,
                bind,
                prefix,
                uses,
                imports,
                implicit,
                decls,
                body,
                contains,
            },
            span,
        ))
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

        // RESULT and BIND clauses may appear in either order
        // (F2008 R1229). Scan for both repeatedly so either
        // `result(r) bind(C)` or `bind(C) result(r)` parses.
        let mut result: Option<String> = None;
        let mut bind: Option<BindInfo> = None;
        loop {
            if result.is_none() && self.peek_text().eq_ignore_ascii_case("result") {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let r = self.advance().clone().text;
                self.expect(&TokenKind::RParen)?;
                result = Some(r);
                continue;
            }
            if bind.is_none() && self.peek_text().eq_ignore_ascii_case("bind") {
                bind = self.try_parse_bind()?;
                continue;
            }
            break;
        }
        self.skip_newlines();

        let (uses, imports, implicit, decls, body, ifaces) = self.parse_unit_body(&["function"])?;
        let mut contains = self.parse_contains_section()?;
        contains.extend(ifaces);
        self.consume_end("function")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            ProgramUnit::Function {
                name,
                args,
                result,
                return_type,
                bind,
                prefix,
                uses,
                imports,
                implicit,
                decls,
                body,
                contains,
            },
            span,
        ))
    }

    /// Parse the F2008 separate module procedure body form:
    ///   `module procedure NAME` [ body ] `end [procedure [NAME]]`
    /// The signature (args, return type, etc.) is inherited from the
    /// matching `module subroutine`/`module function` interface in the
    /// parent module — sema fills it in once both files are processed.
    /// We always emit a Subroutine here; if the parent's interface was
    /// actually a function, sema rewrites it (sema/resolve.rs).
    fn parse_separate_module_procedure(
        &mut self,
        start: crate::lexer::Span,
        prefix: Vec<Prefix>,
    ) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'procedure'
        let name = self.advance().clone().text;
        self.skip_newlines();

        // Body is parsed normally; declarations may appear (e.g. local
        // vars).  The dummy arguments themselves are *not* redeclared
        // here per F2008 §12.6.2.5 — sema injects them from the
        // parent module's interface.
        let (uses, imports, implicit, decls, body, ifaces) =
            self.parse_unit_body(&["procedure"])?;
        let mut contains = self.parse_contains_section()?;
        contains.extend(ifaces);
        self.consume_end("procedure")?;

        let span = span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            ProgramUnit::Subroutine {
                name,
                args: Vec::new(),
                bind: None,
                prefix,
                uses,
                imports,
                implicit,
                decls,
                body,
                contains,
            },
            span,
        ))
    }

    fn parse_block_data(&mut self, start: crate::lexer::Span) -> Result<SpannedUnit, ParseError> {
        self.advance(); // consume 'block'
        self.advance(); // consume 'data'
        let name = if self.peek() == &TokenKind::Identifier {
            Some(self.advance().clone().text)
        } else {
            None
        };
        self.skip_newlines();

        let (uses, _imports, _implicit, decls, _body, _ifaces) =
            self.parse_unit_body(&["blockdata", "block"])?;
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
        Ok(Spanned::new(
            ProgramUnit::BlockData { name, uses, decls },
            span,
        ))
    }

    fn parse_interface_block(
        &mut self,
        start: crate::lexer::Span,
    ) -> Result<SpannedUnit, ParseError> {
        let is_abstract = if self.peek_text().eq_ignore_ascii_case("abstract") {
            self.advance();
            true
        } else {
            false
        };
        self.advance(); // consume 'interface'

        // Optional name or generic spec.
        // Check generic specs BEFORE generic identifier — they lex as identifiers.
        let kw_lc = self.peek_text().to_lowercase();
        let is_generic_spec =
            matches!(kw_lc.as_str(), "operator" | "assignment" | "read" | "write")
                && self.pos + 1 < self.tokens.len()
                && self.tokens[self.pos + 1].kind == TokenKind::LParen;
        let name = if is_generic_spec {
            let op_kw = self.advance().clone().text;
            self.expect(&TokenKind::LParen)?;
            // Consume balanced contents — operators can span multiple
            // tokens (==, /=, //, .lt., etc.) and defined I/O uses
            // `formatted` / `unformatted` identifiers.
            let mut op = String::new();
            let mut depth = 1;
            while depth > 0 && self.peek() != &TokenKind::Eof {
                match self.peek() {
                    TokenKind::LParen => {
                        op.push_str(self.advance().clone().text.as_str());
                        depth += 1;
                    }
                    TokenKind::RParen => {
                        if depth == 1 {
                            self.advance();
                            depth = 0;
                        } else {
                            op.push_str(self.advance().clone().text.as_str());
                            depth -= 1;
                        }
                    }
                    _ => {
                        op.push_str(self.advance().clone().text.as_str());
                    }
                }
            }
            Some(format!("{}({})", op_kw, op))
        } else if self.peek() == &TokenKind::Identifier {
            Some(self.advance().clone().text)
        } else {
            None
        };
        self.skip_newlines();

        let mut bodies = Vec::new();
        loop {
            self.skip_newlines();
            let text = self.peek_text().to_lowercase();
            if text == "endinterface" || text == "end" {
                break;
            }

            if text == "module" {
                let next = if self.pos + 1 < self.tokens.len() {
                    self.tokens[self.pos + 1].text.to_lowercase()
                } else {
                    String::new()
                };
                if next == "procedure" {
                    self.advance(); // module
                    self.advance(); // procedure
                    self.eat(&TokenKind::ColonColon);
                    let mut names = Vec::new();
                    loop {
                        names.push(self.advance().clone().text);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    bodies.push(InterfaceBody::ModuleProcedure(names));
                    self.skip_newlines();
                    continue;
                }
            }

            // F2003 R1207: bare `procedure :: NAME [, NAME...]` inside a
            // generic interface dispatches to the named specifics with
            // the same semantics as `module procedure NAME` here.
            // Several stdlib generic interfaces (e.g. `interface arg`,
            // `interface deg2rad`) use this form.
            if text == "procedure" {
                let next_kind = if self.pos + 1 < self.tokens.len() {
                    self.tokens[self.pos + 1].kind.clone()
                } else {
                    TokenKind::Eof
                };
                // Disambiguate from `procedure(iface), attr :: name`
                // (procedure-pointer / abstract-iface declaration) which
                // takes a parenthesized interface name; that form is a
                // subprogram declaration the regular path handles.
                if next_kind == TokenKind::ColonColon
                    || next_kind == TokenKind::Identifier
                    || next_kind == TokenKind::Comma
                {
                    self.advance(); // procedure
                    self.eat(&TokenKind::ColonColon);
                    let mut names = Vec::new();
                    loop {
                        names.push(self.advance().clone().text);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
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
        Ok(Spanned::new(
            ProgramUnit::InterfaceBlock {
                name,
                is_abstract,
                bodies,
            },
            span,
        ))
    }

    // ---- Helpers ----

    /// Parse the body of a program unit: uses, implicit, declarations, then executable statements.
    #[allow(clippy::type_complexity)]
    pub(crate) fn parse_unit_body(
        &mut self,
        terminators: &[&str],
    ) -> Result<
        (
            Vec<SpannedDecl>,
            Vec<ImportStmt>,
            Vec<SpannedDecl>,
            Vec<SpannedDecl>,
            Vec<SpannedStmt>,
            Vec<SpannedUnit>,
        ),
        ParseError,
    > {
        let mut uses = Vec::new();
        let mut imports = Vec::new();
        let mut implicit = Vec::new();
        let mut decls = Vec::new();
        let mut body = Vec::new();
        let mut interfaces = Vec::new();

        // Phase 1: USE statements.
        loop {
            self.skip_newlines();
            if self.peek_text().eq_ignore_ascii_case("use") {
                self.advance();
                uses.push(self.parse_use_stmt()?);
            } else {
                break;
            }
        }

        // Phase 1.5: IMPORT statements.
        loop {
            self.skip_newlines();
            if self.peek_text().eq_ignore_ascii_case("import") {
                self.advance();
                imports.push(self.parse_import()?);
            } else {
                break;
            }
        }

        // Phase 2: IMPLICIT statements.
        loop {
            self.skip_newlines();
            if self.peek_text().eq_ignore_ascii_case("implicit") {
                self.advance();
                implicit.push(self.parse_implicit()?);
            } else {
                break;
            }
        }

        // Phase 3: Declarations and executable statements.
        // In practice, declarations and statements can be intermixed in modern Fortran.
        // We'll parse everything as statements and let sema separate them.
        loop {
            self.skip_newlines();
            if self.peek() == &TokenKind::Eof {
                break;
            }
            let text = self.peek_text().to_lowercase();

            // Check for end of unit.
            if terminators.iter().any(|t| text == format!("end{}", t)) {
                break;
            }
            if text == "end" {
                let next = if self.pos + 1 < self.tokens.len() {
                    self.tokens[self.pos + 1].text.to_lowercase()
                } else {
                    String::new()
                };
                if terminators.iter().any(|t| next == *t)
                    || next.is_empty()
                    || self.at_stmt_end_after(1)
                {
                    break;
                }
            }
            if text == "contains" {
                break;
            }

            // Check for derived type definition: type name
            // or type [, attrs] :: name.
            if text == "type" {
                let next_pos = self.pos + 1;
                // type(name) is a declaration type-specifier, but bare
                // type name starts a derived-type definition.
                if self.tokens.get(next_pos).is_some_and(|t| {
                    matches!(
                        t.kind,
                        TokenKind::Identifier | TokenKind::Comma | TokenKind::ColonColon
                    )
                }) {
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
                let iface = self.parse_interface_block(istart)?;
                interfaces.push(iface);
                continue;
            }

            // PROCEDURE(interface_name) [, attrs] :: name [=> null()]
            // Procedure pointer / procedure component declarations.
            if text == "procedure" {
                let next_pos = self.pos + 1;
                if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::LParen {
                    let start = self.current_span();
                    self.advance(); // consume 'procedure'
                    self.advance(); // consume '('
                    let iface_name = if self.peek() == &TokenKind::Identifier {
                        self.advance().clone().text
                    } else {
                        String::new()
                    };
                    self.expect(&TokenKind::RParen)?;

                    // Parse attributes: regular declaration attrs like
                    // OPTIONAL/VALUE plus procedure-specific ones like POINTER.
                    let mut attrs = Vec::new();
                    while self.eat(&TokenKind::Comma) {
                        let attr_text = self.peek_text().to_lowercase();
                        if matches!(attr_text.as_str(), "pass" | "deferred" | "non_overridable") {
                            self.advance();
                            continue;
                        }
                        if attr_text == "nopass" {
                            self.advance();
                            attrs.push(crate::ast::decl::Attribute::NoPass);
                            continue;
                        }
                        if let Some(attr) = self.try_parse_attribute() {
                            attrs.push(attr?);
                        } else {
                            self.advance();
                        }
                    }

                    // :: separator
                    if self.peek() == &TokenKind::ColonColon {
                        self.advance();
                    }

                    // Comma-separated entity list. Each entity may
                    // carry its own optional `=> null()` initializer.
                    // Previously the parser stopped after the first
                    // name, dropping `g` in `procedure(...) :: f, g`
                    // and tripping the next-token check on the comma.
                    let mut entities = Vec::new();
                    loop {
                        let entity_name = if self.peek() == &TokenKind::Identifier {
                            self.advance().clone().text
                        } else {
                            String::new()
                        };

                        if self.eat(&TokenKind::Arrow)
                            && self.peek_text().eq_ignore_ascii_case("null")
                        {
                            self.advance();
                            if self.peek() == &TokenKind::LParen {
                                self.advance();
                                let _ = self.expect(&TokenKind::RParen);
                            }
                        }

                        entities.push(crate::ast::decl::EntityDecl {
                            name: entity_name,
                            array_spec: None,
                            init: None,
                            char_len: None,
                            ptr_init: None,
                        });

                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }

                    // Emit as a variable declaration with Pointer attribute.
                    // The interface name is stored but the full procedure
                    // pointer call semantics are deferred.
                    let span = span_from_to(start, self.prev_span());
                    let mut all_attrs = attrs;
                    all_attrs.push(crate::ast::decl::Attribute::External);
                    decls.push(crate::ast::Spanned::new(
                        crate::ast::decl::Decl::TypeDecl {
                            type_spec: crate::ast::decl::TypeSpec::Type(iface_name),
                            attrs: all_attrs,
                            entities,
                        },
                        span,
                    ));
                    continue;
                }
            }

            // Try as type declaration.
            if let Some(ts_result) = self.try_parse_type_spec() {
                let ts = ts_result?;
                decls.push(self.parse_type_decl(ts)?);
                continue;
            }

            // Standalone declaration statements that introduce no
            // new type. Audit MAJOR-2: prior to this dispatch the
            // PARAMETER/COMMON/DATA parsers existed but were never
            // called, so `parameter (x = 42)` at statement-start was
            // silently dropped and the program ran with x=0.
            //
            // Audit Maj-5: Fortran has no reserved words, so a
            // legacy F77 program may use `parameter`, `common`, or
            // `data` as a variable name. Disambiguate by peeking
            // at the next token: the declaration form is always
            // followed by `(` (for PARAMETER and DATA) or `/` (for
            // COMMON); an expression-statement use as an LHS is
            // followed by `=`. This is a single-token lookahead.
            let next_tok = self.tokens.get(self.pos + 1).map(|t| t.kind.clone());
            if text == "parameter" && next_tok.as_ref() == Some(&TokenKind::LParen) {
                self.advance(); // consume 'parameter'
                decls.push(self.parse_parameter_stmt()?);
                continue;
            }
            if text == "common" && matches!(next_tok.as_ref(), Some(TokenKind::Slash)) {
                self.advance(); // consume 'common'
                decls.push(self.parse_common_block()?);
                continue;
            }
            let data_has_value_delimiter = self
                .tokens
                .iter()
                .skip(self.pos + 1)
                .take_while(|tok| {
                    !matches!(
                        tok.kind,
                        TokenKind::Newline | TokenKind::Semicolon | TokenKind::Eof
                    )
                })
                .any(|tok| tok.kind == TokenKind::Slash);
            if text == "data"
                && data_has_value_delimiter
                && matches!(
                    next_tok.as_ref(),
                    Some(TokenKind::Identifier | TokenKind::LParen)
                )
            {
                self.advance(); // consume 'data'
                decls.push(self.parse_data_stmt()?);
                continue;
            }
            if text == "equivalence" && next_tok.as_ref() == Some(&TokenKind::LParen) {
                self.advance(); // consume 'equivalence'
                decls.push(self.parse_equivalence_stmt()?);
                continue;
            }
            if text == "enum" && next_tok.as_ref() == Some(&TokenKind::Comma) {
                decls.push(self.parse_enum_def()?);
                continue;
            }
            // F2023 R766: ENUMERATION TYPE [..] :: name
            if text == "enumeration" {
                decls.push(self.parse_enumeration_type_def()?);
                continue;
            }

            // INTRINSIC / EXTERNAL :: name-list — informational
            // declarations that mark functions as intrinsic or external.
            // We consume and discard them; sema already knows which names
            // are intrinsic.
            if (text == "intrinsic" || text == "external")
                && (next_tok.as_ref() == Some(&TokenKind::ColonColon)
                    || next_tok.as_ref() == Some(&TokenKind::Identifier))
            {
                self.advance(); // consume keyword
                let _ = self.eat(&TokenKind::ColonColon);
                // Eat the name list.
                loop {
                    if self.peek() == &TokenKind::Identifier {
                        self.advance();
                    } else {
                        break;
                    }
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.skip_newlines();
                continue;
            }

            // SAVE statement (F2018 §8.6.14):
            //   bare `save`            — saves all locals in this scope
            //   `save :: a, b`         — saves listed entities
            //   `save a, b`            — same, no `::`
            //   `save /cb/, x`         — common-block and entity mix
            // Disambiguate from a variable named `save` by requiring
            // the next token to start a SAVE list (`::`, identifier,
            // `/`) or end the statement.
            if text == "save" {
                let next_kind = self.tokens.get(self.pos + 1).map(|t| t.kind.clone());
                let is_save_stmt = self.at_stmt_end_after(1)
                    || matches!(
                        next_kind,
                        Some(TokenKind::ColonColon)
                            | Some(TokenKind::Identifier)
                            | Some(TokenKind::Slash)
                    );
                if is_save_stmt {
                    let start = self.current_span();
                    self.advance(); // consume 'save'
                    let _ = self.eat(&TokenKind::ColonColon);
                    let mut entities = Vec::new();
                    while !self.at_stmt_end() {
                        if self.peek() == &TokenKind::Slash {
                            // /common-block-name/ — consume bracketing slashes.
                            self.advance();
                            if self.peek() == &TokenKind::Identifier {
                                entities.push(self.advance().clone().text);
                            }
                            let _ = self.eat(&TokenKind::Slash);
                        } else if self.peek() == &TokenKind::Identifier {
                            entities.push(self.advance().clone().text);
                        } else {
                            break;
                        }
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.skip_newlines();
                    let span = span_from_to(start, self.prev_span());
                    decls.push(crate::ast::Spanned::new(
                        crate::ast::decl::Decl::AttributeStmt {
                            attr: crate::ast::decl::Attribute::Save,
                            entities,
                        },
                        span,
                    ));
                    continue;
                }
            }

            // ALLOCATABLE / POINTER / TARGET attribute statements
            // (F2018 R526/R535/R859): `allocatable :: a, b`, `pointer p`,
            // `target :: t`. Parsed to AttributeStmt; fold_attribute_statements
            // (run at end of the unit body) merges each into the entity's
            // type declaration. Disambiguate from a same-named variable by
            // requiring `::` or an entity identifier next — `pointer = x`
            // (`=`) and `pointer(i) = x` (`(`) fall through to assignment.
            if text == "allocatable" || text == "pointer" || text == "target" {
                let next_kind = self.tokens.get(self.pos + 1).map(|t| t.kind.clone());
                let is_attr_stmt = matches!(
                    next_kind,
                    Some(TokenKind::ColonColon) | Some(TokenKind::Identifier)
                );
                if is_attr_stmt {
                    let start = self.current_span();
                    let attr = match text.as_str() {
                        "allocatable" => crate::ast::decl::Attribute::Allocatable,
                        "pointer" => crate::ast::decl::Attribute::Pointer,
                        _ => crate::ast::decl::Attribute::Target,
                    };
                    self.advance(); // consume the attribute keyword
                    let _ = self.eat(&TokenKind::ColonColon);
                    let mut entities = Vec::new();
                    while self.peek() == &TokenKind::Identifier {
                        entities.push(self.advance().clone().text);
                        // F2018 permits an array-spec here (`allocatable ::
                        // a(:)`), but AttributeStmt carries only names —
                        // reject the spec form loudly rather than silently
                        // dropping the shape.
                        if self.peek() == &TokenKind::LParen {
                            return Err(self.error(
                                "array-spec in a standalone ALLOCATABLE/POINTER/TARGET \
                                 statement is not supported yet; declare the shape on \
                                 the type declaration instead"
                                    .to_string(),
                            ));
                        }
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.skip_newlines();
                    let span = span_from_to(start, self.prev_span());
                    decls.push(crate::ast::Spanned::new(
                        crate::ast::decl::Decl::AttributeStmt { attr, entities },
                        span,
                    ));
                    continue;
                }
            }

            // PRIVATE / PUBLIC access statements.
            if text == "private" || text == "public" {
                let start = self.current_span();
                let attr = if text == "private" {
                    crate::ast::decl::Attribute::Private
                } else {
                    crate::ast::decl::Attribute::Public
                };
                if self.at_stmt_end_after(1) {
                    // Standalone: sets default access for the module.
                    self.advance();
                    let span = span_from_to(start, self.prev_span());
                    decls.push(crate::ast::Spanned::new(
                        crate::ast::decl::Decl::AccessDefault { access: attr },
                        span,
                    ));
                    continue;
                }
                // PUBLIC :: name-list or PRIVATE :: name-list
                let next_pos = self.pos + 1;
                let has_colons = next_pos < self.tokens.len()
                    && self.tokens[next_pos].kind == TokenKind::ColonColon;
                let ident_pos = if has_colons { next_pos + 1 } else { next_pos };
                if ident_pos < self.tokens.len()
                    && self.tokens[ident_pos].kind == TokenKind::Identifier
                {
                    self.advance(); // consume PUBLIC/PRIVATE
                    if has_colons {
                        self.advance();
                    } // consume ::
                    let mut names = Vec::new();
                    while let Some(name) = self.parse_access_list_item()? {
                        names.push(name);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    if !names.is_empty() {
                        let span = span_from_to(start, self.prev_span());
                        decls.push(crate::ast::Spanned::new(
                            crate::ast::decl::Decl::AccessList {
                                access: attr,
                                names,
                            },
                            span,
                        ));
                        continue;
                    }
                }
            }

            // Try as executable statement.
            body.push(self.parse_stmt()?);
        }

        fold_attribute_statements(&mut decls);
        Ok((uses, imports, implicit, decls, body, interfaces))
    }

    fn parse_access_list_item(&mut self) -> Result<Option<String>, ParseError> {
        if self.peek() != &TokenKind::Identifier {
            return Ok(None);
        }

        let kw = self.peek_text().to_lowercase();
        let is_generic_spec = matches!(kw.as_str(), "operator" | "assignment" | "read" | "write")
            && self.pos + 1 < self.tokens.len()
            && self.tokens[self.pos + 1].kind == TokenKind::LParen;

        if !is_generic_spec {
            return Ok(Some(self.advance().clone().text));
        }

        let generic_kw = self.advance().clone().text;
        self.expect(&TokenKind::LParen)?;
        // Consume the parenthesized contents until the matching ).
        // Operators can be `==`, `/=`, `//`, etc. — multi-token. Defined
        // I/O uses `formatted` / `unformatted` identifiers.
        let mut op = String::new();
        let mut depth = 1;
        while depth > 0 && self.peek() != &TokenKind::Eof {
            match self.peek() {
                TokenKind::LParen => {
                    op.push_str(self.advance().clone().text.as_str());
                    depth += 1;
                }
                TokenKind::RParen => {
                    if depth == 1 {
                        self.advance();
                        depth = 0;
                    } else {
                        op.push_str(self.advance().clone().text.as_str());
                        depth -= 1;
                    }
                }
                _ => {
                    op.push_str(self.advance().clone().text.as_str());
                }
            }
        }
        Ok(Some(format!("{}({})", generic_kw, op)))
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
            if self.peek() == &TokenKind::Eof {
                break;
            }
            let text = self.peek_text().to_lowercase();
            // Only break on END that closes the parent unit — not on inner subprograms'
            // END keywords (those are consumed by parse_program_unit).
            // Combined forms like "endprogram", "endmodule" etc. close the parent.
            if text == "end" {
                let next = if self.pos + 1 < self.tokens.len() {
                    self.tokens[self.pos + 1].text.to_lowercase()
                } else {
                    String::new()
                };
                // Bare "end" or "end program/module/submodule" closes the parent.
                if next.is_empty()
                    || self.at_stmt_end_after(1)
                    || matches!(
                        next.as_str(),
                        "program" | "module" | "submodule" | "subroutine" | "function"
                    )
                {
                    break;
                }
            }
            if matches!(
                text.as_str(),
                "endprogram" | "endmodule" | "endsubmodule" | "endsubroutine" | "endfunction"
            ) {
                break;
            }
            units.push(self.parse_program_unit()?);
        }
        Ok(units)
    }

    fn parse_dummy_arg_list(&mut self) -> Result<Vec<DummyArg>, ParseError> {
        let mut args = Vec::new();
        if self.peek() == &TokenKind::RParen {
            return Ok(args);
        }
        loop {
            if self.eat(&TokenKind::Star) {
                args.push(DummyArg::Star);
            } else {
                args.push(DummyArg::Name(self.advance().clone().text));
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(args)
    }

    /// Parse an IMPORT statement.
    pub fn parse_import(&mut self) -> Result<ImportStmt, ParseError> {
        // Already consumed 'import'.
        if self.eat(&TokenKind::Comma) {
            let text = self.peek_text().to_lowercase();
            match text.as_str() {
                "all" => {
                    self.advance();
                    return Ok(ImportStmt::All);
                }
                "none" => {
                    self.advance();
                    return Ok(ImportStmt::None);
                }
                "only" => {
                    self.advance();
                    self.expect(&TokenKind::Colon)?;
                    let mut names = Vec::new();
                    loop {
                        names.push(self.advance().clone().text);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
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
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
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
            } else {
                None
            }
        } else {
            None
        };
        self.expect(&TokenKind::RParen)?;
        Ok(Some(BindInfo { name }))
    }
}

/// Fold standalone ALLOCATABLE/POINTER/TARGET attribute statements into the
/// type declaration of each named entity, so every downstream consumer
/// (resolve plus all lowering storage sites) sees the attribute through the
/// normal `Decl::TypeDecl` path with no extra plumbing.
///
/// A declaration that names several entities is split so the attribute lands
/// on only its entity: `integer :: y, z` + `allocatable :: y` becomes
/// `integer :: z` and `integer, allocatable :: y`. An entity with no type
/// declaration in this scope — e.g. a function result typed by its function
/// statement — has no fold target; its statement is left in place (inert),
/// and the allocatable-result ABI remains a separate concern.
fn fold_attribute_statements(decls: &mut Vec<SpannedDecl>) {
    use crate::ast::decl::{Attribute, Decl};
    let mut i = 0;
    while i < decls.len() {
        let (attr, entities) = match &decls[i].node {
            Decl::AttributeStmt { attr, entities }
                if matches!(
                    attr,
                    Attribute::Allocatable | Attribute::Pointer | Attribute::Target
                ) =>
            {
                (attr.clone(), entities.clone())
            }
            _ => {
                i += 1;
                continue;
            }
        };
        let unfolded: Vec<String> = entities
            .into_iter()
            .filter(|name| !fold_one_attribute(decls, name, &attr))
            .collect();
        if unfolded.is_empty() {
            decls.remove(i); // fully folded — drop the now-redundant statement
        } else {
            if let Decl::AttributeStmt { entities, .. } = &mut decls[i].node {
                *entities = unfolded;
            }
            i += 1;
        }
    }
}

/// Apply `attr` to the type declaration of entity `name`, splitting a
/// multi-entity declaration so only `name` gets it. Returns false if no type
/// declaration in `decls` declares `name`.
fn fold_one_attribute(
    decls: &mut Vec<SpannedDecl>,
    name: &str,
    attr: &crate::ast::decl::Attribute,
) -> bool {
    use crate::ast::decl::Decl;
    let mut found: Option<(usize, usize, usize)> = None;
    for (di, d) in decls.iter().enumerate() {
        if let Decl::TypeDecl { entities, .. } = &d.node {
            if let Some(ei) = entities
                .iter()
                .position(|e| e.name.eq_ignore_ascii_case(name))
            {
                found = Some((di, ei, entities.len()));
                break;
            }
        }
    }
    let Some((di, ei, count)) = found else {
        return false;
    };
    if count == 1 {
        if let Decl::TypeDecl { attrs, .. } = &mut decls[di].node {
            if !attrs.iter().any(|a| a == attr) {
                attrs.push(attr.clone());
            }
        }
    } else {
        let span = decls[di].span;
        let (type_spec, mut new_attrs, ent) = match &mut decls[di].node {
            Decl::TypeDecl {
                type_spec,
                attrs,
                entities,
            } => {
                let ent = entities.remove(ei);
                (type_spec.clone(), attrs.clone(), ent)
            }
            _ => unreachable!(),
        };
        if !new_attrs.iter().any(|a| a == attr) {
            new_attrs.push(attr.clone());
        }
        decls.push(Spanned::new(
            Decl::TypeDecl {
                type_spec,
                attrs: new_attrs,
                entities: vec![ent],
            },
            span,
        ));
    }
    true
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
        let u = parse_unit(
            "program hello\n  implicit none\n  integer :: x\n  x = 42\nend program hello\n",
        );
        if let ProgramUnit::Program {
            name, decls, body, ..
        } = &u.node
        {
            assert_eq!(name.as_deref(), Some("hello"));
            assert!(!decls.is_empty());
            assert!(!body.is_empty());
        } else {
            panic!("not Program");
        }
    }

    #[test]
    fn program_with_contains() {
        let u = parse_unit(
            "program main\n  x = 1\ncontains\n  subroutine sub()\n  end subroutine\nend program\n",
        );
        if let ProgramUnit::Program { contains, .. } = &u.node {
            assert_eq!(contains.len(), 1);
        } else {
            panic!("not Program");
        }
    }

    #[test]
    fn program_with_bare_end() {
        let u = parse_unit("program main\n  integer :: x\n  x = 1\nend\n");
        if let ProgramUnit::Program { name, body, .. } = &u.node {
            assert_eq!(name.as_deref(), Some("main"));
            assert_eq!(body.len(), 1);
        } else {
            panic!("not Program");
        }
    }

    #[test]
    fn standalone_attribute_statement_folds_into_type_decl() {
        use crate::ast::decl::{Attribute, Decl};
        // `allocatable :: a` folds onto a (splitting the `integer :: a, b`
        // declaration) without affecting b; the AttributeStmt is consumed.
        let u =
            parse_unit("program p\n  integer :: a, b\n  allocatable :: a\n  a = 0\nend program\n");
        let ProgramUnit::Program { decls, .. } = &u.node else {
            panic!("not Program");
        };
        assert!(
            !decls
                .iter()
                .any(|d| matches!(d.node, Decl::AttributeStmt { .. })),
            "AttributeStmt should have been folded away"
        );
        let mut a_allocatable = false;
        let mut b_allocatable = false;
        for d in decls {
            if let Decl::TypeDecl {
                attrs, entities, ..
            } = &d.node
            {
                let has_alloc = attrs.iter().any(|x| matches!(x, Attribute::Allocatable));
                for e in entities {
                    match e.name.as_str() {
                        "a" => a_allocatable = has_alloc,
                        "b" => b_allocatable = has_alloc,
                        _ => {}
                    }
                }
            }
        }
        assert!(a_allocatable, "a should be allocatable");
        assert!(!b_allocatable, "b should not be allocatable");
    }

    // ---- SUBROUTINE ----

    #[test]
    fn simple_subroutine() {
        let u = parse_unit("subroutine foo(x, y)\n  real :: x, y\nend subroutine\n");
        if let ProgramUnit::Subroutine { name, args, .. } = &u.node {
            assert_eq!(name, "foo");
            assert_eq!(args.len(), 2);
        } else {
            panic!("not Subroutine");
        }
    }

    #[test]
    fn pure_elemental_subroutine() {
        let u = parse_unit(
            "pure elemental subroutine bar(x)\n  real, intent(in) :: x\nend subroutine\n",
        );
        if let ProgramUnit::Subroutine { prefix, .. } = &u.node {
            assert!(prefix.contains(&Prefix::Pure));
            assert!(prefix.contains(&Prefix::Elemental));
        } else {
            panic!("not Subroutine");
        }
    }

    // ---- FUNCTION ----

    #[test]
    fn simple_function() {
        let u =
            parse_unit("function square(x) result(y)\n  real :: x, y\n  y = x * x\nend function\n");
        if let ProgramUnit::Function { name, result, .. } = &u.node {
            assert_eq!(name, "square");
            assert_eq!(result.as_deref(), Some("y"));
        } else {
            panic!("not Function");
        }
    }

    #[test]
    fn typed_function() {
        let u =
            parse_unit("real function add(a, b)\n  real :: a, b\n  add = a + b\nend function\n");
        if let ProgramUnit::Function { return_type, .. } = &u.node {
            assert!(return_type.is_some());
        } else {
            panic!("not Function");
        }
    }

    #[test]
    fn recursive_function() {
        let u = parse_unit("recursive function fact(n) result(f)\n  integer :: n, f\n  if (n <= 1) then\n    f = 1\n  else\n    f = n * fact(n - 1)\n  end if\nend function\n");
        if let ProgramUnit::Function { prefix, .. } = &u.node {
            assert!(prefix.contains(&Prefix::Recursive));
        } else {
            panic!("not Function");
        }
    }

    // ---- MODULE ----

    #[test]
    fn simple_module() {
        let u = parse_unit("module my_mod\n  implicit none\n  integer :: x\ncontains\n  subroutine sub()\n  end subroutine\nend module\n");
        if let ProgramUnit::Module { name, contains, .. } = &u.node {
            assert_eq!(name, "my_mod");
            assert_eq!(contains.len(), 1);
        } else {
            panic!("not Module");
        }
    }

    #[test]
    fn module_with_use() {
        let u = parse_unit("module b\n  use a\n  implicit none\nend module\n");
        if let ProgramUnit::Module { uses, .. } = &u.node {
            assert_eq!(uses.len(), 1);
        } else {
            panic!("not Module");
        }
    }

    #[test]
    fn submodule_preserves_imports() {
        let u = parse_unit("submodule(parent) child\n  import, only: visible\nend submodule\n");
        let ProgramUnit::Submodule { imports, .. } = &u.node else {
            panic!("not Submodule");
        };
        assert!(matches!(
            imports.as_slice(),
            [ImportStmt::Only(names)] if names.len() == 1 && names[0] == "visible"
        ));
    }

    #[test]
    fn submodule_preserves_implicit_none() {
        let u = parse_unit("submodule(parent) child\n  implicit none\nend submodule\n");
        let ProgramUnit::Submodule { implicit, .. } = &u.node else {
            panic!("not Submodule");
        };
        assert!(matches!(
            implicit.as_slice(),
            [Spanned {
                node: crate::ast::decl::Decl::ImplicitNone { .. },
                ..
            }]
        ));
    }

    // ---- INTERFACE ----

    #[test]
    fn interface_explicit() {
        let u = parse_unit(
            "interface\n  subroutine ext(x)\n    real :: x\n  end subroutine\nend interface\n",
        );
        if let ProgramUnit::InterfaceBlock { bodies, .. } = &u.node {
            assert_eq!(bodies.len(), 1);
        } else {
            panic!("not InterfaceBlock");
        }
    }

    #[test]
    fn interface_generic() {
        let u = parse_unit("interface sort\n  module procedure sort_int\n  module procedure sort_real\nend interface\n");
        if let ProgramUnit::InterfaceBlock { name, bodies, .. } = &u.node {
            assert_eq!(name.as_deref(), Some("sort"));
            assert_eq!(bodies.len(), 2);
        } else {
            panic!("not InterfaceBlock");
        }
    }

    #[test]
    fn interface_operator_end_spec() {
        let u = parse_unit(
            "interface operator(+)\n  module procedure add_int\nend interface operator(+)\n",
        );
        if let ProgramUnit::InterfaceBlock { name, bodies, .. } = &u.node {
            assert_eq!(name.as_deref(), Some("operator(+)"));
            assert_eq!(bodies.len(), 1);
        } else {
            panic!("not InterfaceBlock");
        }
    }

    #[test]
    fn module_access_list_accepts_generic_specs() {
        let u = parse_unit(
            "module m\n  implicit none\n  private\n  public :: assignment(=), operator(+), box_t\n  type :: box_t\n    integer :: value\n  end type\nend module\n",
        );
        if let ProgramUnit::Module { decls, .. } = &u.node {
            let access = decls
                .iter()
                .find_map(|decl| match &decl.node {
                    crate::ast::decl::Decl::AccessList { names, .. } => Some(names.clone()),
                    _ => None,
                })
                .expect("expected access list");
            assert_eq!(
                access,
                vec![
                    "assignment(=)".to_string(),
                    "operator(+)".to_string(),
                    "box_t".to_string()
                ]
            );
        } else {
            panic!("not Module");
        }
    }

    #[test]
    fn module_accepts_derived_type_def_without_colon_colon() {
        let u = parse_unit(
            "module m\n  implicit none\n  type node_ptr\n    integer :: value\n  end type node_ptr\nend module\n",
        );
        if let ProgramUnit::Module { decls, .. } = &u.node {
            assert!(decls
                .iter()
                .any(|decl| matches!(decl.node, crate::ast::decl::Decl::DerivedTypeDef { .. })));
        } else {
            panic!("not Module");
        }
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
        } else {
            panic!("not Subroutine");
        }
    }

    #[test]
    fn subroutine_bind_c_with_name() {
        let u =
            parse_unit("subroutine foo(x) bind(c, name='c_foo')\n  real :: x\nend subroutine\n");
        if let ProgramUnit::Subroutine { bind, .. } = &u.node {
            assert!(bind.is_some());
            assert_eq!(bind.as_ref().unwrap().name.as_deref(), Some("'c_foo'"));
        } else {
            panic!("not Subroutine");
        }
    }
}
