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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompactFixedUnitKind {
    Function,
    Subroutine,
    Procedure,
}

/// Recognize a whitespace-insensitive fixed-form program-unit header without
/// guessing from the author's blank placement. The caller still validates the
/// following token shape and the grammar context in which MODULE PROCEDURE is
/// legal before applying the returned first boundary.
fn compact_fixed_unit_split(
    text: &str,
    allow_procedure: bool,
) -> Option<(usize, CompactFixedUnitKind)> {
    fn valid_name(name: &str) -> bool {
        let mut bytes = name.bytes();
        matches!(bytes.next(), Some(first) if first.is_ascii_alphabetic() || first == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }

    let lower = text.to_ascii_lowercase();
    let mut rest = lower.as_str();
    let mut first_prefix_len = None;
    let mut type_allowed = true;
    let mut has_type = false;
    let mut procedure_allowed = allow_procedure;

    loop {
        for (keyword, kind) in [
            ("subroutine", CompactFixedUnitKind::Subroutine),
            ("function", CompactFixedUnitKind::Function),
            ("procedure", CompactFixedUnitKind::Procedure),
        ] {
            let Some(name) = rest.strip_prefix(keyword) else {
                continue;
            };
            let permitted = match kind {
                CompactFixedUnitKind::Function => true,
                CompactFixedUnitKind::Subroutine => !has_type,
                CompactFixedUnitKind::Procedure => procedure_allowed && !has_type,
            };
            if permitted && valid_name(name) {
                return Some((first_prefix_len.unwrap_or(keyword.len()), kind));
            }
        }

        let mut consumed = false;
        for keyword in [
            "non_recursive",
            "elemental",
            "recursive",
            "impure",
            "module",
            "pure",
        ] {
            let Some(tail) = rest.strip_prefix(keyword) else {
                continue;
            };
            if tail.is_empty() {
                continue;
            }
            first_prefix_len.get_or_insert(keyword.len());
            procedure_allowed |= keyword == "module";
            rest = tail;
            consumed = true;
            break;
        }
        if consumed {
            continue;
        }

        if type_allowed {
            for keyword in [
                "doubleprecision",
                "doublecomplex",
                "character",
                "integer",
                "logical",
                "complex",
                "real",
            ] {
                let Some(tail) = rest.strip_prefix(keyword) else {
                    continue;
                };
                if tail.is_empty() {
                    continue;
                }
                first_prefix_len.get_or_insert(keyword.len());
                type_allowed = false;
                has_type = true;
                rest = tail;
                consumed = true;
                break;
            }
        }
        if !consumed {
            return None;
        }
    }
}

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
        self.parse_program_unit_context(false)
    }

    fn parse_program_unit_context(
        &mut self,
        allow_module_prefix: bool,
    ) -> Result<SpannedUnit, ParseError> {
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
            let allow_compact_procedure = prefixes
                .iter()
                .any(|prefix| matches!(prefix, Prefix::Module));
            if let Some(prefix_len) =
                self.compact_fixed_unit_prefix_len(self.pos, allow_compact_procedure)
            {
                self.split_fixed_identifier_at(self.pos, prefix_len);
            }
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
                    let is_compact_prefix = self
                        .compact_fixed_unit_prefix_len(self.pos + 1, true)
                        .is_some();
                    let module_prefix_allowed = !self.fixed_form || allow_module_prefix;
                    if module_prefix_allowed
                        && (is_simple_prefix
                            || is_followed_by_decl_prefix
                            || is_type_then_function
                            || is_compact_prefix)
                    {
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

    fn compact_fixed_unit_prefix_len(
        &self,
        token_index: usize,
        allow_procedure: bool,
    ) -> Option<usize> {
        if !self.fixed_form {
            return None;
        }
        let token = self.tokens.get(token_index)?;
        if token.kind != TokenKind::Identifier {
            return None;
        }
        let (prefix_len, kind) = compact_fixed_unit_split(&token.text, allow_procedure)?;
        let next = self.tokens.get(token_index + 1);
        let mut depth = 0usize;
        let mut has_assignment = false;
        for candidate in self.tokens.iter().skip(token_index + 1) {
            match candidate.kind {
                TokenKind::Newline | TokenKind::Comment | TokenKind::Semicolon | TokenKind::Eof => {
                    break
                }
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => depth = depth.saturating_sub(1),
                TokenKind::Assign | TokenKind::Arrow if depth == 0 => {
                    has_assignment = true;
                    break;
                }
                _ => {}
            }
        }
        let shape_matches = match kind {
            CompactFixedUnitKind::Function => {
                next.is_some_and(|candidate| candidate.kind == TokenKind::LParen) && !has_assignment
            }
            CompactFixedUnitKind::Subroutine => next.is_some_and(|candidate| {
                matches!(
                    candidate.kind,
                    TokenKind::LParen
                        | TokenKind::Newline
                        | TokenKind::Comment
                        | TokenKind::Semicolon
                        | TokenKind::Eof
                ) || (candidate.kind == TokenKind::Identifier
                    && candidate.text.eq_ignore_ascii_case("bind"))
            }),
            CompactFixedUnitKind::Procedure => next.is_some_and(|candidate| {
                matches!(
                    candidate.kind,
                    TokenKind::Newline | TokenKind::Comment | TokenKind::Semicolon | TokenKind::Eof
                )
            }),
        };
        shape_matches.then_some(prefix_len)
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
        self.consume_named_end("program", name.as_deref())?;

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
            self.consume_named_end("program", None)?;
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
        self.consume_named_end("module", Some(&name))?;

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
        self.consume_named_end("submodule", Some(&name))?;

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
        self.consume_named_end("subroutine", Some(&name))?;

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
        self.consume_named_end("function", Some(&name))?;

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
        self.consume_named_end("procedure", Some(&name))?;

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
        self.consume_named_end("block data", name.as_deref())?;

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
                let next_index = self.pos + 1;
                let compact_procedure_prefix = if self.fixed_form {
                    self.tokens.get(next_index).and_then(|token| {
                        let (prefix_len, kind) = compact_fixed_unit_split(&token.text, true)?;
                        (kind == CompactFixedUnitKind::Procedure).then_some(prefix_len)
                    })
                } else {
                    None
                };
                if let Some(prefix_len) = compact_procedure_prefix {
                    self.split_fixed_identifier_at(next_index, prefix_len);
                }
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
            let sub = self.parse_program_unit_context(true)?;
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

                    // Comma-separated entity list. Each entity may carry
                    // its own optional procedure-pointer initializer.
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

                        let ptr_init = if self.eat(&TokenKind::Arrow) {
                            if self.peek_text().eq_ignore_ascii_case("null") {
                                self.advance();
                                self.expect(&TokenKind::LParen)?;
                                self.expect(&TokenKind::RParen)?;
                                None
                            } else {
                                Some(self.parse_expr()?)
                            }
                        } else {
                            None
                        };

                        entities.push(crate::ast::decl::EntityDecl {
                            name: entity_name,
                            array_spec: None,
                            init: None,
                            char_len: None,
                            ptr_init,
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
                    all_attrs.push(crate::ast::decl::Attribute::Procedure);
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

            // INTRINSIC / EXTERNAL statements establish procedure identity.
            // Preserve them so resolution can distinguish a default intrinsic
            // from a same-named external procedure and PURE validation can
            // require an explicit purity contract for external calls.
            if (text == "intrinsic" || text == "external")
                && (next_tok.as_ref() == Some(&TokenKind::ColonColon)
                    || next_tok.as_ref() == Some(&TokenKind::Identifier))
            {
                let start = self.current_span();
                let attr = if text == "intrinsic" {
                    crate::ast::decl::Attribute::Intrinsic
                } else {
                    crate::ast::decl::Attribute::External
                };
                self.advance(); // consume keyword
                let _ = self.eat(&TokenKind::ColonColon);
                let mut entities = Vec::new();
                while self.peek() == &TokenKind::Identifier {
                    entities.push(self.advance().clone().text);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                if entities.is_empty() {
                    return Err(self.error(format!(
                        "{} statement requires at least one procedure name",
                        text.to_ascii_uppercase()
                    )));
                }
                self.skip_newlines();
                let span = span_from_to(start, self.prev_span());
                decls.push(crate::ast::Spanned::new(
                    crate::ast::decl::Decl::AttributeStmt { attr, entities },
                    span,
                ));
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

            // ALLOCATABLE / POINTER / TARGET / VOLATILE attribute statements
            // (F2018 R526/R535/R859): `allocatable :: a, b`, `pointer p`,
            // `target :: t`. Parsed to AttributeStmt; fold_attribute_statements
            // (run at end of the unit body) merges each into the entity's
            // type declaration. Disambiguate from a same-named variable by
            // requiring `::` or an entity identifier next — `pointer = x`
            // (`=`) and `pointer(i) = x` (`(`) fall through to assignment.
            if matches!(
                text.as_str(),
                "allocatable" | "pointer" | "target" | "volatile"
            ) {
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
                        "target" => crate::ast::decl::Attribute::Target,
                        _ => crate::ast::decl::Attribute::Volatile,
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
                                "array-spec in a standalone ALLOCATABLE/POINTER/TARGET/VOLATILE \
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

            // Standalone DIMENSION statement (F2018 R832):
            // `dimension [::] a(10), b(2, 3)`. Every entity has its own
            // array-spec, so this uses a dedicated AST node rather than the
            // shared DIMENSION(...) type-declaration attribute. As with other
            // keyword statements, preserve Fortran's non-reserved keywords:
            // `dimension = x` and `dimension(i) = x` remain assignments.
            if text == "dimension" {
                let next_kind = self.tokens.get(self.pos + 1).map(|t| t.kind.clone());
                if matches!(
                    next_kind,
                    Some(TokenKind::ColonColon) | Some(TokenKind::Identifier)
                ) {
                    decls.push(self.parse_dimension_stmt()?);
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
        fold_dimension_statements(&mut decls)?;
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
            units.push(self.parse_program_unit_context(true)?);
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

    /// Parse optional BIND(C [, NAME=scalar-default-char-constant-expr]) clause.
    /// Returns `None` if no BIND, `Some(BindInfo)` if present.
    fn try_parse_bind(&mut self) -> Result<Option<BindInfo>, ParseError> {
        if !self.peek_text().eq_ignore_ascii_case("bind") {
            return Ok(None);
        }
        self.advance(); // bind
        self.expect(&TokenKind::LParen)?;
        if self.peek() != &TokenKind::Identifier || !self.peek_text().eq_ignore_ascii_case("c") {
            return Err(self.error("expected C language binding in BIND clause".into()));
        }
        self.advance(); // c
        let name = if self.eat(&TokenKind::Comma) {
            if self.peek() != &TokenKind::Identifier
                || !self.peek_text().eq_ignore_ascii_case("name")
            {
                return Err(self.error("expected NAME in BIND(C) clause".into()));
            }
            self.advance();
            self.expect(&TokenKind::Assign)?;
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::RParen)?;
        Ok(Some(BindInfo { name }))
    }
}

/// Fold standalone entity attribute statements into the type declaration of
/// each named entity, so every downstream consumer sees the attribute through
/// the normal `Decl::TypeDecl` path with no extra plumbing.
///
/// A declaration that names several entities is split so the attribute lands
/// on only its entity: `integer :: y, z` + `allocatable :: y` becomes
/// `integer :: z` and `integer, allocatable :: y`. An entity with no type
/// declaration in this scope — e.g. a function result typed by its function
/// statement — has no fold target, so its statement remains available for
/// semantic resolution. Lowering normalizes semantically typed standalone
/// VOLATILE entities later; other result-attribute ABIs remain separate
/// concerns.
fn fold_attribute_statements(decls: &mut Vec<SpannedDecl>) {
    use crate::ast::decl::{Attribute, Decl};
    let mut i = 0;
    while i < decls.len() {
        let (attr, entities) = match &decls[i].node {
            Decl::AttributeStmt { attr, entities }
                if matches!(
                    attr,
                    Attribute::Allocatable
                        | Attribute::Pointer
                        | Attribute::Target
                        | Attribute::Volatile
                        | Attribute::External
                        | Attribute::Intrinsic
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
            .filter(|name| !crate::ast::decl::fold_attribute_into_type_decl(decls, name, &attr))
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

/// Fold standalone DIMENSION entities into matching type declarations.
///
/// A DIMENSION statement may precede or follow the entity's type declaration,
/// so the full declaration list is searched. Entities with no explicit type
/// declaration remain as `DimensionStmt` nodes: semantic resolution assigns
/// their implicit type (or diagnoses IMPLICIT NONE), and the lowering
/// normalization then materializes an equivalent typed declaration.
fn fold_dimension_statements(decls: &mut Vec<SpannedDecl>) -> Result<(), ParseError> {
    use crate::ast::decl::Decl;
    use std::collections::HashSet;

    let mut seen = HashSet::new();
    for decl in decls.iter() {
        if let Decl::DimensionStmt { entities } = &decl.node {
            for entity in entities {
                let key = entity.name.to_ascii_lowercase();
                if !seen.insert(key) {
                    return Err(ParseError {
                        span: decl.span,
                        msg: format!(
                            "duplicate DIMENSION attribute specified for '{}'",
                            entity.name
                        ),
                    });
                }
            }
        }
    }

    let mut i = 0;
    while i < decls.len() {
        let (entities, stmt_span) = match &decls[i].node {
            Decl::DimensionStmt { entities } => (entities.clone(), decls[i].span),
            _ => {
                i += 1;
                continue;
            }
        };

        let mut unfolded = Vec::new();
        for entity in entities {
            if !fold_one_dimension(decls, &entity.name, &entity.array_spec, stmt_span)? {
                unfolded.push(entity);
            }
        }

        if unfolded.is_empty() {
            decls.remove(i);
        } else {
            if let Decl::DimensionStmt { entities } = &mut decls[i].node {
                *entities = unfolded;
            }
            i += 1;
        }
    }
    Ok(())
}

fn fold_one_dimension(
    decls: &mut Vec<SpannedDecl>,
    name: &str,
    array_spec: &[crate::ast::decl::ArraySpec],
    stmt_span: crate::lexer::Span,
) -> Result<bool, ParseError> {
    use crate::ast::decl::{Attribute, Decl};

    let mut found: Option<(usize, usize, usize)> = None;
    for (decl_index, decl) in decls.iter().enumerate() {
        if let Decl::TypeDecl { entities, .. } = &decl.node {
            if let Some(entity_index) = entities
                .iter()
                .position(|entity| entity.name.eq_ignore_ascii_case(name))
            {
                found = Some((decl_index, entity_index, entities.len()));
                break;
            }
        }
    }
    let Some((decl_index, entity_index, entity_count)) = found else {
        return Ok(false);
    };

    let has_decl_dimension = match &decls[decl_index].node {
        Decl::TypeDecl {
            attrs, entities, ..
        } => {
            attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::Dimension(_)))
                || entities[entity_index].array_spec.is_some()
        }
        _ => unreachable!(),
    };
    if has_decl_dimension {
        return Err(ParseError {
            span: stmt_span,
            msg: format!("duplicate DIMENSION attribute specified for '{}'", name),
        });
    }

    if entity_count == 1 {
        if let Decl::TypeDecl { entities, .. } = &mut decls[decl_index].node {
            entities[entity_index].array_spec = Some(array_spec.to_vec());
        }
    } else {
        let decl_span = decls[decl_index].span;
        let (type_spec, attrs, mut entity) = match &mut decls[decl_index].node {
            Decl::TypeDecl {
                type_spec,
                attrs,
                entities,
            } => (
                type_spec.clone(),
                attrs.clone(),
                entities.remove(entity_index),
            ),
            _ => unreachable!(),
        };
        entity.array_spec = Some(array_spec.to_vec());
        decls.push(Spanned::new(
            Decl::TypeDecl {
                type_spec,
                attrs,
                entities: vec![entity],
            },
            decl_span,
        ));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::decl::Decl;
    use crate::ast::expr::Expr;
    use crate::ast::stmt::Stmt;
    use crate::lexer::{fixed::tokenize_fixed, Lexer};

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

    fn parse_fixed_unit(src: &str) -> SpannedUnit {
        let tokens = tokenize_fixed(src, 0).unwrap();
        let mut parser = Parser::new_for_form(&tokens, crate::lexer::SourceForm::FixedForm);
        let units = parser.parse_file().unwrap();
        assert_eq!(units.len(), 1, "expected 1 unit, got {}", units.len());
        units.into_iter().next().unwrap()
    }

    fn parse_error(src: &str) -> ParseError {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        parser.parse_file().unwrap_err()
    }

    fn parse_fixed_error(src: &str) -> ParseError {
        let tokens = tokenize_fixed(src, 0).unwrap();
        let mut parser = Parser::new_for_form(&tokens, crate::lexer::SourceForm::FixedForm);
        parser.parse_file().unwrap_err()
    }

    #[test]
    fn compact_fixed_unit_recognition_is_iterative_for_long_prefix_runs() {
        let header = format!("{}functionf", "pure".repeat(20_000));
        assert_eq!(
            compact_fixed_unit_split(&header, false),
            Some((4, CompactFixedUnitKind::Function))
        );
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
    fn standalone_double_is_an_identifier_in_free_and_fixed_form() {
        let free = parse_unit(
            "\
program contextual_name
  integer :: double
  double = 41
end program contextual_name
",
        );
        let fixed = parse_fixed_unit(concat!(
            "      PROGRAM P\n",
            "      INTEGER DOUBLE\n",
            "      DOUBLE = 41\n",
            "      END\n",
        ));

        for unit in [free, fixed] {
            let ProgramUnit::Program { body, .. } = unit.node else {
                panic!("not Program");
            };
            assert!(matches!(
                body.as_slice(),
                [stmt]
                    if matches!(
                        &stmt.node,
                        Stmt::Assignment {
                            target,
                            value: _,
                        } if matches!(
                            &target.node,
                            Expr::Name { name } if name.eq_ignore_ascii_case("double")
                        )
                    )
            ));
        }
    }

    #[test]
    fn bare_double_declaration_is_rejected() {
        parse_error(
            "\
program malformed
  double :: value
end program malformed
",
        );
        parse_error(
            "\
program malformed
  implicit double (a-h)
end program malformed
",
        );
    }

    #[test]
    fn procedure_pointer_declaration_preserves_named_initializer() {
        let unit = parse_unit(
            "\
program test
  procedure(callback), pointer :: first => action, second => null(), third
end program test
",
        );
        let ProgramUnit::Program { decls, .. } = unit.node else {
            panic!("not Program");
        };
        let Some(Decl::TypeDecl { entities, .. }) =
            decls.iter().find_map(|decl| match &decl.node {
                Decl::TypeDecl { entities, .. }
                    if entities.iter().any(|entity| entity.name == "first") =>
                {
                    Some(&decl.node)
                }
                _ => None,
            })
        else {
            panic!("procedure-pointer declaration not preserved");
        };
        assert_eq!(entities.len(), 3);
        assert!(matches!(
            entities[0].ptr_init.as_ref().map(|expr| &expr.node),
            Some(Expr::Name { name }) if name == "action"
        ));
        assert!(entities[1].ptr_init.is_none());
        assert!(entities[2].ptr_init.is_none());
    }

    #[test]
    fn procedure_pointer_null_initializer_requires_an_opening_parenthesis() {
        for error in [
            parse_error(
                "\
program malformed
  procedure(callback), pointer :: handler => null
end program malformed
",
            ),
            parse_fixed_error(concat!(
                "      PROGRAM MALFORMED\n",
                "      PROCEDURE(CALLBACK),POINTER::HANDLER=>NULL\n",
                "      ENDPROGRAMMALFORMED\n",
            )),
        ] {
            assert!(error.msg.contains("expected ("), "{error}");
        }
    }

    #[test]
    fn procedure_pointer_null_initializer_requires_a_closing_parenthesis() {
        for error in [
            parse_error(
                "\
program malformed
  procedure(callback), pointer :: handler => null(
end program malformed
",
            ),
            parse_fixed_error(concat!(
                "      PROGRAM MALFORMED\n",
                "      PROCEDURE(CALLBACK),POINTER::HANDLER=>NULL(\n",
                "      ENDPROGRAMMALFORMED\n",
            )),
        ] {
            assert!(error.msg.contains("expected )"), "{error}");
        }
    }

    #[test]
    fn procedure_pointer_component_null_initializer_requires_an_opening_parenthesis() {
        for error in [
            parse_error(
                "\
module malformed_m
  type :: holder
    procedure(callback), pointer, nopass :: handler => null
  end type holder
end module malformed_m
",
            ),
            parse_fixed_error(concat!(
                "      MODULE MALFORMED_M\n",
                "      TYPE HOLDER\n",
                "      PROCEDURE(CALLBACK),POINTER,NOPASS::HANDLER=>NULL\n",
                "      ENDTYPEHOLDER\n",
                "      ENDMODULEMALFORMED_M\n",
            )),
        ] {
            assert!(error.msg.contains("expected ("), "{error}");
        }
    }

    #[test]
    fn procedure_pointer_component_null_initializer_requires_a_closing_parenthesis() {
        for error in [
            parse_error(
                "\
module malformed_m
  type :: holder
    procedure(callback), pointer, nopass :: handler => null(
  end type holder
end module malformed_m
",
            ),
            parse_fixed_error(concat!(
                "      MODULE MALFORMED_M\n",
                "      TYPE HOLDER\n",
                "      PROCEDURE(CALLBACK),POINTER,NOPASS::HANDLER=>NULL(\n",
                "      ENDTYPEHOLDER\n",
                "      ENDMODULEMALFORMED_M\n",
            )),
        ] {
            assert!(error.msg.contains("expected )"), "{error}");
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
    fn block_data_end_requires_complete_multiword_keyword() {
        let err = parse_error("block data foo\nend block foo\n");
        assert!(
            err.msg.contains("expected 'data' after 'end block'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn closing_program_unit_names_match_case_insensitively() {
        let sources = [
            "program Alpha\nend program ALPHA\n",
            "module Alpha\nend module ALPHA\n",
            "submodule(parent) Alpha\nend submodule ALPHA\n",
            "subroutine Alpha()\nend subroutine ALPHA\n",
            "function Alpha()\nend function ALPHA\n",
            "module procedure Alpha\nend procedure ALPHA\n",
            "block data Alpha\nend block data ALPHA\n",
        ];

        for source in sources {
            parse_unit(source);
        }
    }

    #[test]
    fn closing_program_unit_names_must_match() {
        let sources = [
            "program alpha\nend program beta\n",
            "module alpha\nend module beta\n",
            "submodule(parent) alpha\nend submodule beta\n",
            "subroutine alpha()\nend subroutine beta\n",
            "function alpha()\nend function beta\n",
            "module procedure alpha\nend procedure beta\n",
            "block data alpha\nend block data beta\n",
        ];

        for source in sources {
            let err = parse_error(source);
            assert!(
                err.msg.contains("does not match opening name 'alpha'"),
                "unexpected error for {source:?}: {err}"
            );
        }
    }

    #[test]
    fn closing_name_requires_an_opening_name() {
        let sources = [
            "program\nend program alpha\n",
            "value = 1\nend program alpha\n",
            "block data\nend block data alpha\n",
        ];

        for source in sources {
            let err = parse_error(source);
            assert!(
                err.msg.contains("name 'alpha' has no opening name"),
                "unexpected error for {source:?}: {err}"
            );
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

    #[test]
    fn standalone_volatile_statement_folds_into_type_decl() {
        use crate::ast::decl::{Attribute, Decl};
        let unit =
            parse_unit("program p\n  integer :: watched\n  volatile :: watched\nend program p\n");
        let ProgramUnit::Program { decls, .. } = &unit.node else {
            panic!("not Program");
        };
        assert!(decls.iter().any(|decl| {
            matches!(
                &decl.node,
                Decl::TypeDecl { attrs, entities, .. }
                    if entities.iter().any(|entity| entity.name == "watched")
                        && attrs.iter().any(|attr| matches!(attr, Attribute::Volatile))
            )
        }));
    }

    #[test]
    fn standalone_external_and_intrinsic_statements_are_preserved() {
        use crate::ast::decl::{Attribute, Decl};

        let unit = parse_unit(
            "program p\n\
               real :: typed_external\n\
               external :: typed_external\n\
               external :: external_work\n\
               intrinsic :: sin\n\
             end program p\n",
        );
        let ProgramUnit::Program { decls, .. } = &unit.node else {
            panic!("not Program");
        };

        assert!(decls.iter().any(|decl| {
            matches!(
                &decl.node,
                Decl::TypeDecl { attrs, entities, .. }
                    if entities.iter().any(|entity| entity.name == "typed_external")
                        && attrs.iter().any(|attr| matches!(attr, Attribute::External))
            )
        }));
        assert!(decls.iter().any(|decl| {
            matches!(
                &decl.node,
                Decl::AttributeStmt {
                    attr: Attribute::External,
                    entities,
                } if entities == &["external_work"]
            )
        }));
        assert!(decls.iter().any(|decl| {
            matches!(
                &decl.node,
                Decl::AttributeStmt {
                    attr: Attribute::Intrinsic,
                    entities,
                } if entities == &["sin"]
            )
        }));
    }

    #[test]
    fn standalone_procedure_attribute_statements_require_a_name() {
        for keyword in ["external", "intrinsic"] {
            let error = parse_error(&format!("program p\n  {keyword} ::\nend program p\n"));
            assert!(
                error
                    .msg
                    .contains("statement requires at least one procedure name"),
                "unexpected error for {keyword}: {error}"
            );
        }
    }

    #[test]
    fn standalone_dimension_preserves_entity_shapes_and_implicit_entities() {
        let unit = parse_unit(
            "program p\n\
               dimension :: a(3), b(2, 4), x(-1:1)\n\
               integer :: a, b\n\
             end program p\n",
        );
        let ProgramUnit::Program { decls, .. } = &unit.node else {
            panic!("not Program");
        };

        let mut explicit_ranks = std::collections::HashMap::new();
        let mut implicit_dimension = None;
        for decl in decls {
            match &decl.node {
                Decl::TypeDecl { entities, .. } => {
                    for entity in entities {
                        if let Some(specs) = &entity.array_spec {
                            explicit_ranks.insert(entity.name.to_ascii_lowercase(), specs.len());
                        }
                    }
                }
                Decl::DimensionStmt { entities } => {
                    assert_eq!(
                        entities.len(),
                        1,
                        "only the implicitly typed entity should remain standalone"
                    );
                    implicit_dimension = entities.first();
                }
                _ => {}
            }
        }

        assert_eq!(explicit_ranks.get("a"), Some(&1));
        assert_eq!(explicit_ranks.get("b"), Some(&2));
        let implicit = implicit_dimension.expect("missing implicit DIMENSION entity");
        assert_eq!(implicit.name, "x");
        assert_eq!(implicit.array_spec.len(), 1);
    }

    #[test]
    fn standalone_dimension_rejects_duplicate_shape_sources() {
        for source in [
            "program p\ninteger :: a(2)\ndimension a(3)\nend program p\n",
            "program p\ninteger, dimension(2) :: a\ndimension :: a(3)\nend program p\n",
            "program p\ndimension a(2)\ndimension a(3)\nend program p\n",
            "program p\ndimension :: a(2), A(3)\nend program p\n",
        ] {
            let error = parse_error(source);
            assert!(
                error.msg.contains("duplicate DIMENSION attribute"),
                "unexpected error for {source:?}: {error}"
            );
        }
    }

    #[test]
    fn standalone_dimension_requires_each_entity_shape() {
        let error = parse_error("program p\ndimension :: a\nend program p\n");
        assert!(
            error.msg.contains("requires an array-spec"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn fixed_form_standalone_dimension_folds_into_the_type_declaration() {
        let unit = parse_fixed_unit(
            "      PROGRAM P
      INTEGER A
      DIMENSION A(3)
      END
",
        );
        let ProgramUnit::Program { decls, .. } = &unit.node else {
            panic!("not Program");
        };
        let array_rank = decls.iter().find_map(|decl| {
            let Decl::TypeDecl { entities, .. } = &decl.node else {
                return None;
            };
            entities
                .iter()
                .find(|entity| entity.name.eq_ignore_ascii_case("a"))
                .and_then(|entity| entity.array_spec.as_ref())
                .map(Vec::len)
        });
        assert_eq!(array_rank, Some(1));
        assert!(!decls
            .iter()
            .any(|decl| matches!(decl.node, Decl::DimensionStmt { .. })));
    }

    #[test]
    fn dimension_keyword_can_still_name_an_assignment_target() {
        let unit = parse_unit(
            "program p\n\
               integer :: dimension, i\n\
               integer :: values(2)\n\
               dimension = 1\n\
               values(i) = dimension\n\
             end program p\n",
        );
        let ProgramUnit::Program { decls, body, .. } = &unit.node else {
            panic!("not Program");
        };
        assert!(!decls
            .iter()
            .any(|decl| matches!(decl.node, Decl::DimensionStmt { .. })));
        assert_eq!(body.len(), 2);
        assert!(body
            .iter()
            .all(|stmt| matches!(stmt.node, crate::ast::stmt::Stmt::Assignment { .. })));
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
    fn fixed_typed_function_headers_do_not_depend_on_blank_placement() {
        for header in [
            "      INTEGER FUNCTION F(X)\n",
            "      INTEGER FUNCTIONF(X)\n",
            "      INTEGERFUNCTIONF(X)\n",
            "      INTEGER PURE FUNCTIONF(X)\n",
            "      PUREINTEGERFUNCTIONF(X)\n",
        ] {
            let source = format!("{header}      INTEGER X\n      F=X\n      END\n");
            let unit = parse_fixed_unit(&source);
            let ProgramUnit::Function {
                name, return_type, ..
            } = &unit.node
            else {
                panic!("not Function for {header:?}");
            };
            assert_eq!(name, "F");
            assert!(return_type.is_some());
            if header.to_ascii_lowercase().contains("pure") {
                let ProgramUnit::Function { prefix, .. } = &unit.node else {
                    unreachable!()
                };
                assert!(prefix.contains(&Prefix::Pure));
            }
        }
    }

    #[test]
    fn free_form_keyword_prefixed_array_name_stays_a_declaration() {
        let unit =
            parse_unit("program p\n  integer functionf(3)\n  functionf(1) = 7\nend program p\n");
        let ProgramUnit::Program { decls, .. } = &unit.node else {
            panic!("not Program");
        };
        assert!(!decls.is_empty());
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
    fn fixed_module_name_does_not_depend_on_blank_placement() {
        for header in ["      MODULE PROCEDURAL\n", "      MODULEPROCEDURAL\n"] {
            let source = format!("{header}      END MODULE PROCEDURAL\n");
            let unit = parse_fixed_unit(&source);
            let ProgramUnit::Module { name, .. } = &unit.node else {
                panic!("not Module for {header:?}");
            };
            assert_eq!(name, "PROCEDURAL");
        }
    }

    #[test]
    fn fixed_compact_module_procedure_headers_are_resolved_in_contains_context() {
        let unit = parse_fixed_unit(concat!(
            "      MODULE M\n",
            "      CONTAINS\n",
            "      MODULESUBROUTINECALLABLE()\n",
            "      ENDSUBROUTINECALLABLE\n",
            "      ENDMODULEM\n",
        ));
        let ProgramUnit::Module { contains, .. } = &unit.node else {
            panic!("not Module");
        };
        let [contained] = contains.as_slice() else {
            panic!("expected one contained procedure");
        };
        let ProgramUnit::Subroutine { name, prefix, .. } = &contained.node else {
            panic!("not Subroutine");
        };
        assert_eq!(name, "CALLABLE");
        assert!(prefix.contains(&Prefix::Module));
    }

    #[test]
    fn fixed_compact_module_procedure_lists_stay_interface_declarations() {
        let unit = parse_fixed_unit(concat!(
            "      INTERFACE GENERIC_NAME\n",
            "      MODULEPROCEDUREPRINTABLE,REALIGNER\n",
            "      ENDINTERFACEGENERIC_NAME\n",
        ));
        let ProgramUnit::InterfaceBlock { bodies, .. } = &unit.node else {
            panic!("not InterfaceBlock");
        };
        let [InterfaceBody::ModuleProcedure(names)] = bodies.as_slice() else {
            panic!("expected one module-procedure declaration");
        };
        assert_eq!(names, &["PRINTABLE", "REALIGNER"]);
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

    #[test]
    fn derived_type_requires_end_type() {
        for error in [
            parse_error(
                "\
module m
  type :: item
    integer :: value
  end
end module m
",
            ),
            parse_fixed_error(concat!(
                "      MODULE M\n",
                "      TYPE ITEM\n",
                "      INTEGER VALUE\n",
                "      END\n",
                "      ENDMODULEM\n",
            )),
        ] {
            assert!(
                error.to_string().contains("expected 'type' after 'end'"),
                "{error}"
            );
        }
    }

    #[test]
    fn interface_requires_end_interface() {
        for error in [
            parse_error(
                "\
module m
  interface
    subroutine ext()
    end subroutine ext
  end
end module m
",
            ),
            parse_fixed_error(concat!(
                "      MODULE M\n",
                "      INTERFACE\n",
                "      SUBROUTINE EXT\n",
                "      ENDSUBROUTINEEXT\n",
                "      END\n",
                "      ENDMODULEM\n",
            )),
        ] {
            assert!(
                error
                    .to_string()
                    .contains("expected 'interface' after 'end'"),
                "{error}"
            );
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
        use crate::ast::expr::Expr;

        let u =
            parse_unit("subroutine foo(x) bind(c, name='c_foo')\n  real :: x\nend subroutine\n");
        if let ProgramUnit::Subroutine { bind, .. } = &u.node {
            assert!(bind.is_some());
            let name = bind.as_ref().unwrap().name.as_ref().unwrap();
            assert!(
                matches!(
                    &name.node,
                    Expr::StringLiteral { value, kind: None } if value == "c_foo"
                ),
                "unexpected NAME expression: {name:?}"
            );
        } else {
            panic!("not Subroutine");
        }
    }

    #[test]
    fn bind_name_parses_constant_expression() {
        use crate::ast::expr::{BinaryOp, Expr};

        let u = parse_unit("subroutine foo() bind(c, name=prefix // suffix)\nend subroutine\n");
        let ProgramUnit::Subroutine {
            bind: Some(bind), ..
        } = &u.node
        else {
            panic!("expected bound Subroutine");
        };
        assert!(
            matches!(
                bind.name.as_ref().map(|expr| &expr.node),
                Some(Expr::BinaryOp {
                    op: BinaryOp::Concat,
                    ..
                })
            ),
            "unexpected NAME expression: {:?}",
            bind.name
        );
    }

    #[test]
    fn bind_rejects_non_c_language() {
        let err = parse_error("subroutine foo() bind(fortran)\nend subroutine\n");
        assert!(
            err.msg.contains("expected C language binding"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn bind_rejects_unknown_specifier() {
        let err = parse_error("subroutine foo() bind(c, value='c_foo')\nend subroutine\n");
        assert!(err.msg.contains("expected NAME"), "unexpected error: {err}");
    }
}
