//! Declaration parser.
//!
//! Parses type declarations, USE statements, IMPLICIT, derived type
//! definitions, and legacy declaration forms (COMMON, DATA, etc.).

use super::{ParseError, Parser};
use crate::ast::decl::*;
use crate::ast::Spanned;
use crate::lexer::TokenKind;

impl<'a> Parser<'a> {
    // ---- Type specifier parsing ----

    /// Try to parse a type specifier. Returns None if current token isn't a type keyword.
    pub fn try_parse_type_spec(&mut self) -> Option<Result<TypeSpec, ParseError>> {
        let text = self.peek_text().to_lowercase();
        match text.as_str() {
            "integer" => {
                self.advance();
                Some(self.parse_kind_selector().map(TypeSpec::Integer))
            }
            "real" => {
                self.advance();
                Some(self.parse_kind_selector().map(TypeSpec::Real))
            }
            "doubleprecision" | "double" => {
                self.advance();
                // Handle "double precision" / "double complex" as two tokens.
                if self.peek_text().eq_ignore_ascii_case("precision") {
                    self.advance();
                    Some(Ok(TypeSpec::DoublePrecision))
                } else if self.peek_text().eq_ignore_ascii_case("complex") {
                    self.advance();
                    Some(Ok(TypeSpec::DoubleComplex))
                } else {
                    Some(Ok(TypeSpec::DoublePrecision))
                }
            }
            "complex" => {
                self.advance();
                Some(self.parse_kind_selector().map(TypeSpec::Complex))
            }
            "doublecomplex" => {
                self.advance();
                Some(Ok(TypeSpec::DoubleComplex))
            }
            "logical" => {
                self.advance();
                Some(self.parse_kind_selector().map(TypeSpec::Logical))
            }
            "character" => {
                self.advance();
                Some(self.parse_char_selector().map(TypeSpec::Character))
            }
            "type" => {
                // type(name) is a type specifier, but type :: name is a derived type definition.
                // Only consume if followed by (.
                let next_pos = self.pos + 1;
                if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::LParen {
                    self.advance();
                    Some(self.parse_type_or_class_spec(false))
                } else {
                    None // Not a type specifier — could be a derived type def.
                }
            }
            "class" => {
                self.advance();
                Some(self.parse_type_or_class_spec(true))
            }
            _ => None,
        }
    }

    /// Parse a type specifier for IMPLICIT — without consuming a kind selector,
    /// since the parenthesized part is the letter range, not a kind.
    fn parse_implicit_type_spec(&mut self) -> Option<Result<TypeSpec, ParseError>> {
        let text = self.peek_text().to_lowercase();
        match text.as_str() {
            "integer" => {
                self.advance();
                Some(Ok(TypeSpec::Integer(None)))
            }
            "real" => {
                self.advance();
                Some(Ok(TypeSpec::Real(None)))
            }
            "doubleprecision" | "double" => {
                self.advance();
                if self.peek_text().eq_ignore_ascii_case("precision") {
                    self.advance();
                    Some(Ok(TypeSpec::DoublePrecision))
                } else if self.peek_text().eq_ignore_ascii_case("complex") {
                    self.advance();
                    Some(Ok(TypeSpec::DoubleComplex))
                } else {
                    Some(Ok(TypeSpec::DoublePrecision))
                }
            }
            "complex" => {
                self.advance();
                Some(Ok(TypeSpec::Complex(None)))
            }
            "logical" => {
                self.advance();
                Some(Ok(TypeSpec::Logical(None)))
            }
            "character" => {
                self.advance();
                Some(Ok(TypeSpec::Character(None)))
            }
            _ => None,
        }
    }

    fn parse_kind_selector(&mut self) -> Result<Option<KindSelector>, ParseError> {
        // Check for *N (old-style)
        if self.eat(&TokenKind::Star) {
            let expr = self.parse_expr()?;
            return Ok(Some(KindSelector::Star(expr)));
        }
        // Check for (kind=N) or (N)
        if self.peek() != &TokenKind::LParen {
            return Ok(None);
        }
        self.advance(); // (
                        // Check for kind= keyword
        if self.peek_text().eq_ignore_ascii_case("kind") {
            let next_pos = self.pos + 1;
            if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::Assign {
                self.advance(); // kind
                self.advance(); // =
            }
        }
        let expr = self.parse_expr()?;
        self.expect(&TokenKind::RParen)?;
        Ok(Some(KindSelector::Expr(expr)))
    }

    fn parse_char_selector(&mut self) -> Result<Option<CharSelector>, ParseError> {
        // Check for *N (old-style)
        if self.eat(&TokenKind::Star) {
            let len = self.parse_len_spec()?;
            return Ok(Some(CharSelector {
                len: Some(len),
                kind: None,
            }));
        }
        if self.peek() != &TokenKind::LParen {
            return Ok(None);
        }
        self.advance(); // (

        let mut len = None;
        let mut kind = None;

        // Parse len and/or kind parameters.
        if self.peek_text().eq_ignore_ascii_case("len") {
            let next_pos = self.pos + 1;
            if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::Assign {
                self.advance(); // len
                self.advance(); // =
                len = Some(self.parse_len_spec()?);
            } else {
                // Just a number — treat as len.
                len = Some(self.parse_len_spec()?);
            }
        } else if self.peek_text().eq_ignore_ascii_case("kind") {
            let next_pos = self.pos + 1;
            if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::Assign {
                self.advance(); // kind
                self.advance(); // =
                kind = Some(self.parse_expr()?);
            }
        } else {
            // Bare number or expression — treat as len.
            len = Some(self.parse_len_spec()?);
        }

        // Check for comma and second parameter.
        if self.eat(&TokenKind::Comma) {
            if self.peek_text().eq_ignore_ascii_case("kind") {
                let next_pos = self.pos + 1;
                if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::Assign {
                    self.advance(); // kind
                    self.advance(); // =
                }
            } else if self.peek_text().eq_ignore_ascii_case("len") {
                let next_pos = self.pos + 1;
                if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::Assign {
                    self.advance(); // len
                    self.advance(); // =
                    len = Some(self.parse_len_spec()?);
                    self.expect(&TokenKind::RParen)?;
                    return Ok(Some(CharSelector { len, kind }));
                }
            }
            if kind.is_none() {
                kind = Some(self.parse_expr()?);
            } else {
                len = Some(self.parse_len_spec()?);
            }
        }

        self.expect(&TokenKind::RParen)?;
        Ok(Some(CharSelector { len, kind }))
    }

    fn parse_len_spec(&mut self) -> Result<LenSpec, ParseError> {
        if self.eat(&TokenKind::Star) {
            return Ok(LenSpec::Star);
        }
        if self.peek() == &TokenKind::Colon {
            self.advance();
            return Ok(LenSpec::Colon);
        }
        // F77 entity-decl character-length form: `name*(*)`, `name*(:)`,
        // `name*(N)`. The leading `*` is consumed by the caller; here we
        // see the parenthesized inner form. Per F2018 §C.6.1 these are
        // the type-param-value alternatives `*`, `:`, and a scalar
        // int-expr. The plain `parse_expr` can't accept the bare `*` or
        // `:` form, so unwrap one level of parens before delegating.
        if self.peek() == &TokenKind::LParen {
            self.advance();
            let inner = if self.eat(&TokenKind::Star) {
                LenSpec::Star
            } else if self.eat(&TokenKind::Colon) {
                LenSpec::Colon
            } else {
                LenSpec::Expr(self.parse_expr()?)
            };
            self.expect(&TokenKind::RParen)?;
            return Ok(inner);
        }
        let expr = self.parse_expr()?;
        Ok(LenSpec::Expr(expr))
    }

    fn parse_type_or_class_spec(&mut self, is_class: bool) -> Result<TypeSpec, ParseError> {
        self.expect(&TokenKind::LParen)?;
        if self.eat(&TokenKind::Star) {
            self.expect(&TokenKind::RParen)?;
            return Ok(if is_class {
                TypeSpec::ClassStar
            } else {
                TypeSpec::TypeStar
            });
        }
        if self.peek() != &TokenKind::Identifier {
            return Err(self.error(format!("expected type name, got {}", self.peek())));
        }
        let name_tok = self.advance().clone();
        let name = name_tok.text;
        self.expect(&TokenKind::RParen)?;
        Ok(if is_class {
            TypeSpec::Class(name)
        } else {
            TypeSpec::Type(name)
        })
    }

    // ---- Attribute parsing ----

    /// Try to parse a declaration attribute after a comma.
    pub fn try_parse_attribute(&mut self) -> Option<Result<Attribute, ParseError>> {
        let text = self.peek_text().to_lowercase();
        match text.as_str() {
            "allocatable" => {
                self.advance();
                Some(Ok(Attribute::Allocatable))
            }
            "pointer" => {
                self.advance();
                Some(Ok(Attribute::Pointer))
            }
            "target" => {
                self.advance();
                Some(Ok(Attribute::Target))
            }
            "optional" => {
                self.advance();
                Some(Ok(Attribute::Optional))
            }
            "save" => {
                self.advance();
                Some(Ok(Attribute::Save))
            }
            "parameter" => {
                self.advance();
                Some(Ok(Attribute::Parameter))
            }
            "value" => {
                self.advance();
                Some(Ok(Attribute::Value))
            }
            "volatile" => {
                self.advance();
                Some(Ok(Attribute::Volatile))
            }
            "asynchronous" => {
                self.advance();
                Some(Ok(Attribute::Asynchronous))
            }
            "protected" => {
                self.advance();
                Some(Ok(Attribute::Protected))
            }
            "contiguous" => {
                self.advance();
                Some(Ok(Attribute::Contiguous))
            }
            "external" => {
                self.advance();
                Some(Ok(Attribute::External))
            }
            "intrinsic" => {
                self.advance();
                Some(Ok(Attribute::Intrinsic))
            }
            "public" => {
                self.advance();
                Some(Ok(Attribute::Public))
            }
            "private" => {
                self.advance();
                Some(Ok(Attribute::Private))
            }
            "dimension" => {
                self.advance();
                Some(self.parse_dimension_spec().map(Attribute::Dimension))
            }
            "intent" => {
                self.advance();
                Some(self.parse_intent_spec().map(Attribute::Intent))
            }
            "bind" => {
                self.advance();
                Some(self.parse_bind_spec().map(Attribute::Bind))
            }
            _ => None,
        }
    }

    fn parse_dimension_spec(&mut self) -> Result<Vec<ArraySpec>, ParseError> {
        self.expect(&TokenKind::LParen)?;
        let specs = self.parse_array_spec_list()?;
        self.expect(&TokenKind::RParen)?;
        Ok(specs)
    }

    fn parse_array_spec_list(&mut self) -> Result<Vec<ArraySpec>, ParseError> {
        let mut specs = Vec::new();
        loop {
            specs.push(self.parse_one_array_spec()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(specs)
    }

    fn parse_one_array_spec(&mut self) -> Result<ArraySpec, ParseError> {
        // Assumed rank: (..) — F2018. The lexer produces two Dot tokens or
        // a DotOp. We check for consecutive dots.
        if self.peek_text() == ".." || self.peek_text() == "." {
            // Try consuming .. as assumed rank.
            let save = self.pos;
            if self.peek_text() == ".." {
                self.advance();
                return Ok(ArraySpec::AssumedRank);
            }
            self.pos = save;
        }

        // Bare colon (:) — either deferred shape (allocatable/pointer) or
        // assumed shape (dummy argument). The distinction depends on context
        // (allocatable attribute), so we emit Deferred and let sema reclassify
        // to AssumedShape if needed.
        if self.peek() == &TokenKind::Colon {
            self.advance();
            return Ok(ArraySpec::Deferred);
        }

        // Assumed size: (*)
        if self.peek() == &TokenKind::Star {
            self.advance();
            return Ok(ArraySpec::AssumedSize { lower: None });
        }

        // Explicit or lower:upper
        let first = self.parse_expr()?;
        if self.eat(&TokenKind::Colon) {
            // Could be lower:upper, lower:*, or lower:
            if self.peek() == &TokenKind::Star {
                self.advance();
                return Ok(ArraySpec::AssumedSize { lower: Some(first) });
            }
            if matches!(self.peek(), TokenKind::Comma | TokenKind::RParen) {
                return Ok(ArraySpec::AssumedShape { lower: Some(first) });
            }
            let upper = self.parse_expr()?;
            return Ok(ArraySpec::Explicit {
                lower: Some(first),
                upper,
            });
        }

        // Just an upper bound (lower is 1 implicitly).
        Ok(ArraySpec::Explicit {
            lower: None,
            upper: first,
        })
    }

    fn parse_intent_spec(&mut self) -> Result<Intent, ParseError> {
        self.expect(&TokenKind::LParen)?;
        let text = self.peek_text().to_lowercase();
        let intent = match text.as_str() {
            "in" => {
                self.advance();
                if self.peek_text().eq_ignore_ascii_case("out") {
                    self.advance();
                    Intent::InOut
                } else {
                    Intent::In
                }
            }
            "out" => {
                self.advance();
                Intent::Out
            }
            "inout" => {
                self.advance();
                Intent::InOut
            }
            _ => {
                return Err(self.error(format!(
                    "expected intent specifier, got {}",
                    self.peek_text()
                )))
            }
        };
        self.expect(&TokenKind::RParen)?;
        Ok(intent)
    }

    fn parse_bind_spec(&mut self) -> Result<Option<String>, ParseError> {
        self.expect(&TokenKind::LParen)?;
        self.expect_ident_kw("c")?;
        let name = if self.eat(&TokenKind::Comma) {
            if self.peek_text().eq_ignore_ascii_case("name") {
                self.advance();
                self.expect(&TokenKind::Assign)?;
                let name_tok = self.advance().clone();
                Some(name_tok.text)
            } else {
                None
            }
        } else {
            None
        };
        self.expect(&TokenKind::RParen)?;
        Ok(name)
    }

    fn expect_ident_kw(&mut self, name: &str) -> Result<(), ParseError> {
        if self.peek_text().eq_ignore_ascii_case(name) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("expected '{}', got '{}'", name, self.peek_text())))
        }
    }

    // ---- Type declaration parsing ----

    /// Parse a type declaration statement:
    /// `type-spec [, attr-list] :: entity-list`
    /// or `type-spec entity-list` (old-style, no ::)
    pub fn parse_type_decl(&mut self, type_spec: TypeSpec) -> Result<SpannedDecl, ParseError> {
        let start = self.current_span();

        // Parse optional attributes (comma-separated before ::).
        let mut attrs = Vec::new();
        while self.eat(&TokenKind::Comma) {
            if let Some(attr_result) = self.try_parse_attribute() {
                attrs.push(attr_result?);
            } else {
                break;
            }
        }

        // Optional :: separator.
        let _has_double_colon = self.eat(&TokenKind::ColonColon);

        // Parse entity list.
        let entities = self.parse_entity_list()?;

        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            Decl::TypeDecl {
                type_spec,
                attrs,
                entities,
            },
            span,
        ))
    }

    fn parse_entity_list(&mut self) -> Result<Vec<EntityDecl>, ParseError> {
        let mut entities = Vec::new();
        loop {
            entities.push(self.parse_entity_decl()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(entities)
    }

    fn parse_entity_decl(&mut self) -> Result<EntityDecl, ParseError> {
        if self.peek() != &TokenKind::Identifier {
            return Err(self.error(format!("expected entity name, got {}", self.peek())));
        }
        let name_tok = self.advance().clone();
        let name = name_tok.text;

        // Optional array spec on the entity: x(10), x(:,:)
        let array_spec = if self.peek() == &TokenKind::LParen {
            self.advance();
            let specs = self.parse_array_spec_list()?;
            self.expect(&TokenKind::RParen)?;
            Some(specs)
        } else {
            None
        };

        if self.peek() == &TokenKind::LBracket {
            return Err(
                self.error("coarray declarations are recognized but not yet implemented".into())
            );
        }

        // Optional character length: character :: name*20
        let char_len = if self.eat(&TokenKind::Star) {
            Some(self.parse_len_spec()?)
        } else {
            None
        };

        // Initialization: = expr
        let init = if self.eat(&TokenKind::Assign) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        // Pointer initialization: => expr
        let ptr_init = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        Ok(EntityDecl {
            name,
            array_spec,
            char_len,
            init,
            ptr_init,
        })
    }

    // ---- USE statement ----

    pub fn parse_use_stmt(&mut self) -> Result<SpannedDecl, ParseError> {
        let start = self.current_span();
        // Already consumed 'use'.

        // Handle: use :: mod, use, intrinsic :: mod, use, non_intrinsic :: mod
        let mut nature = UseNature::Normal;

        // F2003: use :: module_name (optional :: without nature)
        if self.eat(&TokenKind::ColonColon) {
            // Just :: with no nature — normal use with explicit ::
        } else if self.eat(&TokenKind::Comma) {
            let text = self.peek_text().to_lowercase();
            if text == "intrinsic" {
                self.advance();
                nature = UseNature::Intrinsic;
                self.expect(&TokenKind::ColonColon)?;
            } else if text == "non_intrinsic" {
                self.advance();
                nature = UseNature::NonIntrinsic;
                self.expect(&TokenKind::ColonColon)?;
            }
        }

        let module = self.advance().clone().text;

        let mut renames = Vec::new();
        let mut only = None;

        if self.eat(&TokenKind::Comma) {
            if self.peek_text().eq_ignore_ascii_case("only") {
                self.advance();
                self.expect(&TokenKind::Colon)?;
                only = Some(self.parse_only_list()?);
            } else {
                // Rename list: local => remote
                renames = self.parse_rename_list()?;
            }
        }

        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            Decl::UseStmt {
                module,
                nature,
                renames,
                only,
            },
            span,
        ))
    }

    fn parse_only_list(&mut self) -> Result<Vec<OnlyItem>, ParseError> {
        let mut items = Vec::new();
        if self.at_stmt_end() {
            return Ok(items);
        }
        loop {
            let mut name = self.advance().clone().text;
            let mut is_generic_spec = false;
            if name.eq_ignore_ascii_case("operator") || name.eq_ignore_ascii_case("assignment") {
                self.expect(&TokenKind::LParen)?;
                let op = self.advance().clone().text;
                self.expect(&TokenKind::RParen)?;
                name = format!("{}({})", name, op);
                is_generic_spec = true;
            } else if (name.eq_ignore_ascii_case("read") || name.eq_ignore_ascii_case("write"))
                && self.peek() == &TokenKind::LParen
            {
                // F2018 §12.6.4.8: defined-IO generic-spec — `read (formatted)`,
                // `read (unformatted)`, `write (formatted)`, `write (unformatted)`.
                // Stored as a single OnlyItem::Generic so module resolution can
                // import the corresponding INTERFACE READ/WRITE binding.
                self.advance();
                let kind = self.advance().clone().text;
                self.expect(&TokenKind::RParen)?;
                name = format!(
                    "{}({})",
                    name.to_ascii_lowercase(),
                    kind.to_ascii_lowercase()
                );
                is_generic_spec = true;
            }
            if self.eat(&TokenKind::Arrow) {
                let remote = self.advance().clone().text;
                items.push(OnlyItem::Rename(Rename {
                    local: name,
                    remote,
                }));
            } else if is_generic_spec
                || name.eq_ignore_ascii_case("operator(+)")
                || name.eq_ignore_ascii_case("operator(-)")
                || name.eq_ignore_ascii_case("operator(*)")
                || name.eq_ignore_ascii_case("operator(/)")
                || name.eq_ignore_ascii_case("operator(**)")
                || name.eq_ignore_ascii_case("operator(//)")
                || name.eq_ignore_ascii_case("operator(==)")
                || name.eq_ignore_ascii_case("operator(/=)")
                || name.eq_ignore_ascii_case("operator(<)")
                || name.eq_ignore_ascii_case("operator(<=)")
                || name.eq_ignore_ascii_case("operator(>)")
                || name.eq_ignore_ascii_case("operator(>=)")
                || name.eq_ignore_ascii_case("assignment(=)")
            {
                items.push(OnlyItem::Generic(name));
            } else {
                items.push(OnlyItem::Name(name));
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(items)
    }

    fn parse_rename_list(&mut self) -> Result<Vec<Rename>, ParseError> {
        let mut renames = Vec::new();
        loop {
            let local = self.advance().clone().text;
            self.expect(&TokenKind::Arrow)?;
            let remote = self.advance().clone().text;
            renames.push(Rename { local, remote });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(renames)
    }

    // ---- IMPLICIT ----

    pub fn parse_implicit(&mut self) -> Result<SpannedDecl, ParseError> {
        let start = self.current_span();
        // Already consumed 'implicit'.

        if self.peek_text().eq_ignore_ascii_case("none") {
            self.advance();
            // Check for (type) or (external) or (type, external)
            let mut type_ = true;
            let mut external = false;
            if self.peek() == &TokenKind::LParen {
                self.advance();
                type_ = false;
                loop {
                    let spec = self.peek_text().to_lowercase();
                    match spec.as_str() {
                        "type" => {
                            self.advance();
                            type_ = true;
                        }
                        "external" => {
                            self.advance();
                            external = true;
                        }
                        _ => break,
                    }
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen)?;
            }
            let span = crate::parser::expr::span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Decl::ImplicitNone { external, type_ }, span));
        }

        // IMPLICIT type-spec (letter-range-list)
        // Note: we parse the type keyword WITHOUT its kind selector, because
        // the parenthesized part after the type keyword is the letter range,
        // not a kind selector. E.g., "implicit integer (i-n)" — the (i-n)
        // is a letter range, not kind=i-n.
        let mut specs = Vec::new();
        loop {
            let type_spec = self
                .parse_implicit_type_spec()
                .ok_or_else(|| self.error("expected type specifier in IMPLICIT".into()))??;
            self.expect(&TokenKind::LParen)?;
            let mut ranges = Vec::new();
            loop {
                let start_letter = self.advance().clone().text.chars().next().unwrap_or('a');
                self.expect(&TokenKind::Minus)?;
                let end_letter = self.advance().clone().text.chars().next().unwrap_or('z');
                ranges.push((start_letter, end_letter));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::RParen)?;
            specs.push(ImplicitSpec { type_spec, ranges });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }

        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(Decl::ImplicitStmt { specs }, span))
    }

    // ---- Derived type definition ----

    pub fn parse_derived_type_def(&mut self) -> Result<SpannedDecl, ParseError> {
        let start = self.current_span();
        // Already consumed 'type'. Next could be :: or , attrs.

        let mut attrs = Vec::new();

        // Parse type attributes: abstract, bind(c), extends(parent), public, private
        while self.eat(&TokenKind::Comma) {
            let text = self.peek_text().to_lowercase();
            match text.as_str() {
                "abstract" => {
                    self.advance();
                    attrs.push(TypeAttr::Abstract);
                }
                "public" => {
                    self.advance();
                    attrs.push(TypeAttr::Public);
                }
                "private" => {
                    self.advance();
                    attrs.push(TypeAttr::Private);
                }
                "bind" => {
                    self.advance();
                    let name = self.parse_bind_spec()?;
                    attrs.push(TypeAttr::Bind(name));
                }
                "extends" => {
                    self.advance();
                    self.expect(&TokenKind::LParen)?;
                    let parent = self.advance().clone().text;
                    self.expect(&TokenKind::RParen)?;
                    attrs.push(TypeAttr::Extends(parent));
                }
                _ => break,
            }
        }

        self.eat(&TokenKind::ColonColon);
        let name = self.advance().clone().text;
        self.skip_newlines();

        // Parse components until 'contains' or 'end type'.
        let mut components = Vec::new();
        let mut type_bound_procs = Vec::new();
        let mut final_procs = Vec::new();

        loop {
            self.skip_newlines();
            let text = self.peek_text().to_lowercase();

            if text == "contains" {
                self.advance();
                self.skip_newlines();
                // Parse type-bound procedures until 'end type'.
                loop {
                    self.skip_newlines();
                    let proc_text = self.peek_text().to_lowercase();
                    if proc_text == "end" {
                        break;
                    }
                    if proc_text == "endtype" {
                        break;
                    }

                    if proc_text == "procedure" {
                        self.advance();
                        let tbp = self.parse_type_bound_proc()?;
                        type_bound_procs.push(tbp);
                    } else if proc_text == "generic" {
                        self.advance();
                        let tbp = self.parse_type_bound_proc_generic()?;
                        type_bound_procs.push(tbp);
                    } else if proc_text == "final" {
                        self.advance();
                        self.eat(&TokenKind::ColonColon);
                        let name = self.advance().clone().text;
                        final_procs.push(name);
                    } else {
                        // Skip unknown lines in contains section.
                        while !self.at_stmt_end() {
                            self.advance();
                        }
                    }
                    self.skip_newlines();
                }
                break;
            }

            if text == "end" || text == "endtype" {
                break;
            }

            // PROCEDURE(interface_name) [, attrs] :: name [=> null()]
            // Procedure pointer components inside a derived type.
            if text == "procedure" {
                let next_pos = self.pos + 1;
                if next_pos < self.tokens.len() && self.tokens[next_pos].kind == TokenKind::LParen {
                    let comp_start = self.current_span();
                    self.advance(); // consume 'procedure'
                    self.advance(); // consume '('
                    let iface_name = if self.peek() == &TokenKind::Identifier {
                        self.advance().clone().text
                    } else {
                        String::new()
                    };
                    self.expect(&TokenKind::RParen)?;

                    let mut comp_attrs = Vec::new();
                    while self.eat(&TokenKind::Comma) {
                        let attr_text = self.peek_text().to_lowercase();
                        match attr_text.as_str() {
                            "pointer" => {
                                self.advance();
                                comp_attrs.push(crate::ast::decl::Attribute::Pointer);
                            }
                            "nopass" | "pass" | "deferred" | "non_overridable" => {
                                self.advance();
                            }
                            _ => {
                                self.advance();
                            }
                        }
                    }

                    if self.peek() == &TokenKind::ColonColon {
                        self.advance();
                    }

                    let mut entities = Vec::new();
                    loop {
                        let entity_name = if self.peek() == &TokenKind::Identifier {
                            self.advance().clone().text
                        } else {
                            String::new()
                        };

                        // F2008 §4.5.4.5: a procedure pointer
                        // component may carry a default initial
                        // association `=> proc_name` or `=> null()`.
                        // Without capturing the right-hand side the
                        // pointer field stays uninitialized — calling
                        // `instance%fn(args)` then jumps through
                        // garbage memory.  stdlib_hashmaps's
                        // `procedure(hasher_fun), pointer, nopass ::
                        // hasher => default_hasher` motivated this fix.
                        let mut ptr_init: Option<crate::ast::expr::SpannedExpr> = None;
                        if self.eat(&TokenKind::Arrow) {
                            let init_start = self.current_span();
                            if self.peek_text().eq_ignore_ascii_case("null") {
                                self.advance();
                                if self.peek() == &TokenKind::LParen {
                                    self.advance();
                                    let _ = self.expect(&TokenKind::RParen);
                                }
                                // Leave ptr_init as None for `=> null()`
                                // (matches the legacy behaviour, where
                                // the field is zero-initialised).
                            } else if self.peek() == &TokenKind::Identifier {
                                let target_name = self.advance().clone().text;
                                let span =
                                    crate::parser::expr::span_from_to(init_start, self.prev_span());
                                ptr_init = Some(crate::ast::Spanned::new(
                                    crate::ast::expr::Expr::Name { name: target_name },
                                    span,
                                ));
                            }
                        }

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

                    comp_attrs.push(crate::ast::decl::Attribute::External);
                    let span = crate::parser::expr::span_from_to(comp_start, self.prev_span());
                    components.push(crate::ast::Spanned::new(
                        crate::ast::decl::Decl::TypeDecl {
                            type_spec: crate::ast::decl::TypeSpec::Type(iface_name),
                            attrs: comp_attrs,
                            entities,
                        },
                        span,
                    ));
                    continue;
                }
            }

            // Try to parse a component declaration.
            if let Some(ts_result) = self.try_parse_type_spec() {
                let ts = ts_result?;
                let comp = self.parse_type_decl(ts)?;
                components.push(comp);
            } else {
                // Skip unrecognized lines.
                while !self.at_stmt_end() {
                    self.advance();
                }
                self.skip_newlines();
            }
        }

        // Consume 'end type [name]'.
        if self.peek_text().eq_ignore_ascii_case("endtype") {
            self.advance();
        } else if self.peek_text().eq_ignore_ascii_case("end") {
            self.advance();
            self.eat_ident("type");
        }
        // Optional name after end type.
        if self.peek() == &TokenKind::Identifier {
            self.advance();
        }

        let extends = attrs.iter().find_map(|a| {
            if let TypeAttr::Extends(ref p) = a {
                Some(p.clone())
            } else {
                None
            }
        });

        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(
            Decl::DerivedTypeDef {
                name,
                extends,
                attrs,
                components,
                type_bound_procs,
                final_procs,
            },
            span,
        ))
    }

    fn parse_type_bound_proc(&mut self) -> Result<TypeBoundProc, ParseError> {
        // procedure [(iface)] [, attrs] :: name [=> binding]
        let interface = if self.eat(&TokenKind::LParen) {
            let iface = self.advance().clone().text;
            self.expect(&TokenKind::RParen)?;
            Some(iface)
        } else {
            None
        };
        let mut proc_attrs = Vec::new();
        while self.eat(&TokenKind::Comma) {
            let text = self.peek_text().to_lowercase();
            match text.as_str() {
                "pass" => {
                    proc_attrs.push(self.advance().clone().text);
                    // Optional argument: pass(arg). Consume balanced parens
                    // — we don't track which arg the pass is on; F2018 says
                    // it defaults to the first dummy if not specified.
                    if self.peek() == &TokenKind::LParen {
                        let mut depth = 0;
                        loop {
                            match self.peek() {
                                TokenKind::LParen => {
                                    self.advance();
                                    depth += 1;
                                }
                                TokenKind::RParen => {
                                    self.advance();
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                TokenKind::Eof => break,
                                _ => {
                                    self.advance();
                                }
                            }
                        }
                    }
                }
                "nopass" | "deferred" | "non_overridable" | "public" | "private" => {
                    proc_attrs.push(self.advance().clone().text);
                }
                _ => break,
            }
        }
        self.eat(&TokenKind::ColonColon);
        let name = self.advance().clone().text;
        let binding = if self.eat(&TokenKind::Arrow) {
            Some(self.advance().clone().text)
        } else {
            None
        };
        let bindings = binding.iter().cloned().collect();
        Ok(TypeBoundProc {
            name,
            interface,
            binding,
            bindings,
            attrs: proc_attrs,
            is_generic: false,
        })
    }

    fn parse_type_bound_proc_generic(&mut self) -> Result<TypeBoundProc, ParseError> {
        // generic [, attrs] :: name => specific_name [, specific_name ...]
        // or generic [, attrs] :: operator(+) => specific_name
        // F2018 §4.5.5: access-spec (PUBLIC/PRIVATE) may appear after the
        // GENERIC keyword as a comma-separated attribute. The non-generic
        // parser already handles this for ordinary procedure bindings; the
        // generic parser was skipping it, so `generic, public :: name => ...`
        // mis-parsed name as `,` and dropped every binding.
        let mut proc_attrs = Vec::new();
        while self.eat(&TokenKind::Comma) {
            let text = self.peek_text().to_lowercase();
            match text.as_str() {
                "public" | "private" => {
                    proc_attrs.push(self.advance().clone().text);
                }
                _ => break,
            }
        }
        self.eat(&TokenKind::ColonColon);
        let mut name = self.advance().clone().text;
        // Handle operator(...) form.
        if name.eq_ignore_ascii_case("operator") || name.eq_ignore_ascii_case("assignment") {
            self.expect(&TokenKind::LParen)?;
            let op = self.advance().clone().text;
            self.expect(&TokenKind::RParen)?;
            name = format!("{}({})", name, op);
        }
        let mut bindings = Vec::new();
        if self.eat(&TokenKind::Arrow) {
            bindings.push(self.advance().clone().text);
            while self.eat(&TokenKind::Comma) {
                bindings.push(self.advance().clone().text);
            }
        }
        let binding = bindings.first().cloned();
        Ok(TypeBoundProc {
            name,
            interface: None,
            binding,
            bindings,
            attrs: proc_attrs,
            is_generic: true,
        })
    }

    // ---- PARAMETER, COMMON, EQUIVALENCE, DATA ----

    pub fn parse_parameter_stmt(&mut self) -> Result<SpannedDecl, ParseError> {
        let start = self.current_span();
        // Already consumed 'parameter'. Expect (name=expr, ...)
        self.expect(&TokenKind::LParen)?;
        let mut pairs = Vec::new();
        loop {
            let name = self.advance().clone().text;
            self.expect(&TokenKind::Assign)?;
            let value = self.parse_expr()?;
            pairs.push((name, value));
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen)?;
        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(Decl::ParameterStmt { pairs }, span))
    }

    pub fn parse_common_block(&mut self) -> Result<SpannedDecl, ParseError> {
        let start = self.current_span();
        // Already consumed 'common'. Expect /name/ var-list.
        let name = if self.eat(&TokenKind::Slash) {
            let n = self.advance().clone().text;
            self.expect(&TokenKind::Slash)?;
            Some(n)
        } else {
            None
        };
        let mut vars = Vec::new();
        loop {
            vars.push(self.advance().clone().text);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(Decl::CommonBlock { name, vars }, span))
    }

    pub fn parse_data_stmt(&mut self) -> Result<SpannedDecl, ParseError> {
        use crate::parser::expr::BP_MUL;
        let start = self.current_span();
        // Already consumed 'data'. Format: obj-list /value-list/ [, obj-list /value-list/]
        // Note: / delimiters conflict with division operator. We parse expressions
        // at a binding power that excludes * and / to prevent consuming the delimiter.
        let mut sets = Vec::new();
        loop {
            let mut objects = Vec::new();
            while self.peek() != &TokenKind::Slash {
                objects.push(self.parse_expr_bp(BP_MUL.right)?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::Slash)?;
            let mut values = Vec::new();
            while self.peek() != &TokenKind::Slash {
                values.push(self.parse_expr_bp(BP_MUL.right)?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::Slash)?;
            sets.push(DataSet { objects, values });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            // Check if next batch starts or if we're at end of statement.
            if self.at_stmt_end() {
                break;
            }
        }
        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(Decl::DataStmt { sets }, span))
    }

    pub fn parse_equivalence_stmt(&mut self) -> Result<SpannedDecl, ParseError> {
        let start = self.current_span();
        // Already consumed 'equivalence'. Format: (var-list), (var-list), ...
        let mut groups = Vec::new();
        loop {
            self.expect(&TokenKind::LParen)?;
            let mut group = Vec::new();
            loop {
                group.push(self.parse_expr()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::RParen)?;
            groups.push(group);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(Decl::EquivalenceStmt { groups }, span))
    }

    pub fn parse_enum_def(&mut self) -> Result<SpannedDecl, ParseError> {
        let start = self.current_span();
        self.advance(); // consume ENUM

        if self.eat(&TokenKind::Comma) {
            if !self.eat_ident("bind") {
                return Err(self.error("expected BIND(C) after ENUM,".into()));
            }
            self.expect(&TokenKind::LParen)?;
            if !self.eat_ident("c") {
                return Err(self.error("expected BIND(C) after ENUM,".into()));
            }
            self.expect(&TokenKind::RParen)?;
        }

        self.skip_newlines();
        let mut enumerators = Vec::new();
        loop {
            self.skip_newlines();
            let text = self.peek_text().to_lowercase();
            if text == "end" || text == "endenum" {
                break;
            }
            if text != "enumerator" {
                return Err(self.error(format!(
                    "expected ENUMERATOR or end enum, got '{}'",
                    self.peek_text()
                )));
            }
            self.advance(); // consume ENUMERATOR
            self.eat(&TokenKind::ColonColon);
            loop {
                if self.peek() != &TokenKind::Identifier {
                    return Err(self.error("expected enumerator name".into()));
                }
                let name = self.advance().clone().text;
                let value = if self.eat(&TokenKind::Assign) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                enumerators.push((name, value));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.skip_newlines();
        }

        self.consume_end("enum")?;
        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(Decl::EnumDef { enumerators }, span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse_decl(src: &str) -> SpannedDecl {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);

        // Try type specifier first.
        if let Some(ts_result) = parser.try_parse_type_spec() {
            let ts = ts_result.unwrap();
            return parser.parse_type_decl(ts).unwrap();
        }

        // Try USE.
        if parser.peek_text().eq_ignore_ascii_case("use") {
            parser.advance();
            return parser.parse_use_stmt().unwrap();
        }

        // Try IMPLICIT.
        if parser.peek_text().eq_ignore_ascii_case("implicit") {
            parser.advance();
            return parser.parse_implicit().unwrap();
        }

        // Try PARAMETER.
        if parser.peek_text().eq_ignore_ascii_case("parameter") {
            parser.advance();
            return parser.parse_parameter_stmt().unwrap();
        }

        // Try COMMON.
        if parser.peek_text().eq_ignore_ascii_case("common") {
            parser.advance();
            return parser.parse_common_block().unwrap();
        }

        // Try DATA.
        if parser.peek_text().eq_ignore_ascii_case("data") {
            parser.advance();
            return parser.parse_data_stmt().unwrap();
        }

        // Try EQUIVALENCE.
        if parser.peek_text().eq_ignore_ascii_case("equivalence") {
            parser.advance();
            return parser.parse_equivalence_stmt().unwrap();
        }

        // Try ENUM.
        if parser.peek_text().eq_ignore_ascii_case("enum") {
            return parser.parse_enum_def().unwrap();
        }

        panic!("could not parse as declaration: {}", src);
    }

    // ---- Type declarations ----

    #[test]
    fn integer_simple() {
        let d = parse_decl("integer :: x, y, z");
        if let Decl::TypeDecl {
            type_spec,
            entities,
            ..
        } = &d.node
        {
            assert!(matches!(type_spec, TypeSpec::Integer(None)));
            assert_eq!(entities.len(), 3);
            assert_eq!(entities[0].name, "x");
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn integer_with_init() {
        let d = parse_decl("integer :: x = 0, y = 1");
        if let Decl::TypeDecl { entities, .. } = &d.node {
            assert!(entities[0].init.is_some());
            assert!(entities[1].init.is_some());
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn integer_with_kind() {
        let d = parse_decl("integer(8) :: x");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Integer(Some(_))));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn type_bound_proc_with_interface_spec_preserves_method_name() {
        let tokens = Lexer::tokenize("procedure(push_iface), deferred :: push", 0).unwrap();
        let mut parser = Parser::new(&tokens);
        parser.advance();
        let tbp = parser.parse_type_bound_proc().unwrap();
        assert_eq!(tbp.name, "push");
        assert_eq!(tbp.interface.as_deref(), Some("push_iface"));
        assert!(tbp.binding.is_none());
        assert!(tbp.bindings.is_empty());
        assert_eq!(tbp.attrs, vec!["deferred"]);
    }

    #[test]
    fn generic_type_bound_proc_preserves_all_specific_bindings() {
        let tokens =
            Lexer::tokenize("generic :: set => set_float, set_integer, set_datetime", 0).unwrap();
        let mut parser = Parser::new(&tokens);
        parser.advance();
        let tbp = parser.parse_type_bound_proc_generic().unwrap();
        assert_eq!(tbp.name, "set");
        assert_eq!(tbp.binding.as_deref(), Some("set_float"));
        assert_eq!(
            tbp.bindings,
            vec![
                "set_float".to_string(),
                "set_integer".to_string(),
                "set_datetime".to_string()
            ]
        );
        assert!(tbp.is_generic);
    }

    #[test]
    fn real_allocatable() {
        let d = parse_decl("real(8), allocatable :: matrix(:,:)");
        if let Decl::TypeDecl {
            type_spec,
            attrs,
            entities,
        } = &d.node
        {
            assert!(matches!(type_spec, TypeSpec::Real(Some(_))));
            assert!(attrs.contains(&Attribute::Allocatable));
            assert!(entities[0].array_spec.is_some());
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn character_deferred_length() {
        let d = parse_decl("character(len=:), allocatable :: name");
        if let Decl::TypeDecl {
            type_spec, attrs, ..
        } = &d.node
        {
            if let TypeSpec::Character(Some(cs)) = type_spec {
                assert!(matches!(cs.len, Some(LenSpec::Colon)));
            } else {
                panic!("not character type");
            }
            assert!(attrs.contains(&Attribute::Allocatable));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn character_assumed_length() {
        let d = parse_decl("character(len=*), intent(in) :: input");
        if let Decl::TypeDecl {
            type_spec, attrs, ..
        } = &d.node
        {
            if let TypeSpec::Character(Some(cs)) = type_spec {
                assert!(matches!(cs.len, Some(LenSpec::Star)));
            } else {
                panic!("not character type");
            }
            assert!(attrs
                .iter()
                .any(|a| matches!(a, Attribute::Intent(Intent::In))));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn type_derived() {
        let d = parse_decl("type(my_type) :: obj");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Type(ref n) if n == "my_type"));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn class_star() {
        let d = parse_decl("class(*) :: poly");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::ClassStar));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn pointer_init() {
        let d = parse_decl("type(node), pointer :: ptr => null()");
        if let Decl::TypeDecl { entities, .. } = &d.node {
            assert!(entities[0].ptr_init.is_some());
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn intent_inout() {
        let d = parse_decl("real, intent(inout) :: x");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs
                .iter()
                .any(|a| matches!(a, Attribute::Intent(Intent::InOut))));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn intent_in_out_two_words() {
        let d = parse_decl("real, intent(in out) :: x");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs
                .iter()
                .any(|a| matches!(a, Attribute::Intent(Intent::InOut))));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn multiple_attributes() {
        let d = parse_decl("real(8), dimension(:,:), allocatable, intent(inout) :: matrix");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs.iter().any(|a| matches!(a, Attribute::Dimension(_))));
            assert!(attrs.contains(&Attribute::Allocatable));
            assert!(attrs
                .iter()
                .any(|a| matches!(a, Attribute::Intent(Intent::InOut))));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn old_style_no_double_colon() {
        let d = parse_decl("integer x, y");
        if let Decl::TypeDecl { entities, .. } = &d.node {
            assert_eq!(entities.len(), 2);
            assert_eq!(entities[0].name, "x");
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn double_precision() {
        let d = parse_decl("double precision :: x");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::DoublePrecision));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn bind_c() {
        let d = parse_decl("integer, bind(c) :: x");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs.iter().any(|a| matches!(a, Attribute::Bind(None))));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn enum_bind_c() {
        let d = parse_decl("enum, bind(c)\n  enumerator :: red = 1, blue = 2\nend enum\n");
        if let Decl::EnumDef { enumerators } = &d.node {
            assert_eq!(enumerators.len(), 2);
            assert_eq!(enumerators[0].0, "red");
            assert_eq!(enumerators[1].0, "blue");
        } else {
            panic!("not EnumDef");
        }
    }

    // ---- USE statements ----

    #[test]
    fn use_simple() {
        let d = parse_decl("use my_module");
        if let Decl::UseStmt { module, nature, .. } = &d.node {
            assert_eq!(module, "my_module");
            assert_eq!(*nature, UseNature::Normal);
        } else {
            panic!("not UseStmt");
        }
    }

    #[test]
    fn use_only() {
        let d = parse_decl("use my_module, only: foo, bar");
        if let Decl::UseStmt { only, .. } = &d.node {
            let items = only.as_ref().unwrap();
            assert_eq!(items.len(), 2);
        } else {
            panic!("not UseStmt");
        }
    }

    #[test]
    fn use_intrinsic() {
        let d = parse_decl("use, intrinsic :: iso_c_binding");
        if let Decl::UseStmt { module, nature, .. } = &d.node {
            assert_eq!(module, "iso_c_binding");
            assert_eq!(*nature, UseNature::Intrinsic);
        } else {
            panic!("not UseStmt");
        }
    }

    #[test]
    fn use_only_with_rename() {
        let d = parse_decl("use my_module, only: local => remote");
        if let Decl::UseStmt { only, .. } = &d.node {
            let items = only.as_ref().unwrap();
            assert!(matches!(&items[0], OnlyItem::Rename(_)));
        } else {
            panic!("not UseStmt");
        }
    }

    #[test]
    fn use_only_generic_specs() {
        let d = parse_decl("use my_module, only: operator(+), operator(//), assignment(=)");
        if let Decl::UseStmt { only, .. } = &d.node {
            let items = only.as_ref().unwrap();
            assert_eq!(items.len(), 3);
            assert!(matches!(&items[0], OnlyItem::Generic(name) if name == "operator(+)"));
            assert!(matches!(&items[1], OnlyItem::Generic(name) if name == "operator(//)"));
            assert!(matches!(&items[2], OnlyItem::Generic(name) if name == "assignment(=)"));
        } else {
            panic!("not UseStmt");
        }
    }

    // ---- IMPLICIT ----

    #[test]
    fn implicit_none() {
        let d = parse_decl("implicit none");
        assert!(matches!(
            d.node,
            Decl::ImplicitNone {
                type_: true,
                external: false
            }
        ));
    }

    #[test]
    fn implicit_none_type_external() {
        let d = parse_decl("implicit none(type, external)");
        assert!(matches!(
            d.node,
            Decl::ImplicitNone {
                type_: true,
                external: true
            }
        ));
    }

    #[test]
    fn implicit_double_precision() {
        let d = parse_decl("implicit double precision (a-h, o-z)");
        if let Decl::ImplicitStmt { specs } = &d.node {
            assert_eq!(specs.len(), 1);
            assert!(matches!(specs[0].type_spec, TypeSpec::DoublePrecision));
            assert_eq!(specs[0].ranges.len(), 2);
        } else {
            panic!("not ImplicitStmt");
        }
    }

    // ---- PARAMETER, COMMON, DATA, EQUIVALENCE ----

    #[test]
    fn parameter_stmt() {
        let d = parse_decl("parameter (pi = 3.14159, e = 2.71828)");
        if let Decl::ParameterStmt { pairs } = &d.node {
            assert_eq!(pairs.len(), 2);
            assert_eq!(pairs[0].0, "pi");
            assert_eq!(pairs[1].0, "e");
        } else {
            panic!("not ParameterStmt");
        }
    }

    #[test]
    fn common_block() {
        let d = parse_decl("common /block1/ x, y, z");
        if let Decl::CommonBlock { name, vars } = &d.node {
            assert_eq!(name.as_deref(), Some("block1"));
            assert_eq!(vars.len(), 3);
        } else {
            panic!("not CommonBlock");
        }
    }

    #[test]
    fn data_stmt() {
        let d = parse_decl("data x /1.0/, y /2.0/");
        if let Decl::DataStmt { sets } = &d.node {
            assert_eq!(sets.len(), 2);
        } else {
            panic!("not DataStmt");
        }
    }

    #[test]
    fn equivalence_stmt() {
        let d = parse_decl("equivalence (a, b), (c, d)");
        if let Decl::EquivalenceStmt { groups } = &d.node {
            assert_eq!(groups.len(), 2);
            assert_eq!(groups[0].len(), 2);
        } else {
            panic!("not EquivalenceStmt");
        }
    }

    // ---- Audit test gap coverage ----

    #[test]
    fn real_star8_old_style() {
        let d = parse_decl("real*8 :: x");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(
                type_spec,
                TypeSpec::Real(Some(KindSelector::Star(_)))
            ));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn character_bare_length() {
        let d = parse_decl("character(10) :: s");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            if let TypeSpec::Character(Some(cs)) = type_spec {
                assert!(matches!(cs.len, Some(LenSpec::Expr(_))));
            } else {
                panic!("not character type");
            }
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn integer_kind_keyword() {
        let d = parse_decl("integer(kind=4) :: x");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Integer(Some(_))));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn class_derived_type() {
        let d = parse_decl("class(my_type) :: x");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Class(ref n) if n == "my_type"));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn type_star_assumed() {
        let d = parse_decl("type(*) :: x");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::TypeStar));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn entity_array_spec() {
        let d = parse_decl("integer :: a(10), b(20,30)");
        if let Decl::TypeDecl { entities, .. } = &d.node {
            assert!(entities[0].array_spec.is_some());
            assert_eq!(entities[0].array_spec.as_ref().unwrap().len(), 1);
            assert!(entities[1].array_spec.is_some());
            assert_eq!(entities[1].array_spec.as_ref().unwrap().len(), 2);
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn coarray_entity_decl_reports_not_implemented() {
        let tokens = Lexer::tokenize("integer :: x[*]\n", 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let type_spec = parser
            .try_parse_type_spec()
            .expect("expected type specifier")
            .unwrap();
        let err = parser
            .parse_type_decl(type_spec)
            .expect_err("coarray declaration should not parse yet");
        assert!(err
            .msg
            .contains("coarray declarations are recognized but not yet implemented"));
    }

    #[test]
    fn logical_type() {
        let d = parse_decl("logical :: flag");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Logical(None)));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn complex_type() {
        let d = parse_decl("complex :: z");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Complex(None)));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn implicit_integer() {
        let d = parse_decl("implicit integer (i-n)");
        if let Decl::ImplicitStmt { specs } = &d.node {
            assert!(matches!(specs[0].type_spec, TypeSpec::Integer(_)));
        } else {
            panic!("not ImplicitStmt");
        }
    }

    #[test]
    fn use_double_colon() {
        let d = parse_decl("use :: my_module");
        if let Decl::UseStmt { module, .. } = &d.node {
            assert_eq!(module, "my_module");
        } else {
            panic!("not UseStmt");
        }
    }

    #[test]
    fn bind_with_name() {
        let d = parse_decl("integer, bind(c, name='cfunc') :: x");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs.iter().any(|a| matches!(a, Attribute::Bind(Some(_)))));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn save_attribute() {
        let d = parse_decl("integer, save :: x");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs.contains(&Attribute::Save));
        } else {
            panic!("not TypeDecl");
        }
    }

    #[test]
    fn value_attribute() {
        let d = parse_decl("integer, value :: x");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs.contains(&Attribute::Value));
        } else {
            panic!("not TypeDecl");
        }
    }
}
