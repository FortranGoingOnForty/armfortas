//! Declaration parser.
//!
//! Parses type declarations, USE statements, IMPLICIT, derived type
//! definitions, and legacy declaration forms (COMMON, DATA, etc.).

use crate::ast::Spanned;
use crate::ast::decl::*;
use crate::ast::expr::SpannedExpr;
use crate::lexer::{TokenKind, Span};
use super::{Parser, ParseError};

impl<'a> Parser<'a> {
    // ---- Type specifier parsing ----

    /// Try to parse a type specifier. Returns None if current token isn't a type keyword.
    pub fn try_parse_type_spec(&mut self) -> Option<Result<TypeSpec, ParseError>> {
        let text = self.peek_text().to_lowercase();
        match text.as_str() {
            "integer" => { self.advance(); Some(self.parse_kind_selector().map(TypeSpec::Integer)) }
            "real" => { self.advance(); Some(self.parse_kind_selector().map(TypeSpec::Real)) }
            "doubleprecision" | "double" => {
                self.advance();
                // Handle "double precision" as two tokens in free-form.
                if self.peek_text().eq_ignore_ascii_case("precision") {
                    self.advance();
                }
                Some(Ok(TypeSpec::DoublePrecision))
            }
            "complex" => { self.advance(); Some(self.parse_kind_selector().map(TypeSpec::Complex)) }
            "doublecomplex" => { self.advance(); Some(Ok(TypeSpec::DoubleComplex)) }
            "logical" => { self.advance(); Some(self.parse_kind_selector().map(TypeSpec::Logical)) }
            "character" => { self.advance(); Some(self.parse_char_selector().map(|cs| TypeSpec::Character(cs))) }
            "type" => {
                self.advance();
                Some(self.parse_type_or_class_spec(false))
            }
            "class" => {
                self.advance();
                Some(self.parse_type_or_class_spec(true))
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
            return Ok(Some(CharSelector { len: Some(len), kind: None }));
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
        let expr = self.parse_expr()?;
        Ok(LenSpec::Expr(expr))
    }

    fn parse_type_or_class_spec(&mut self, is_class: bool) -> Result<TypeSpec, ParseError> {
        self.expect(&TokenKind::LParen)?;
        if self.eat(&TokenKind::Star) {
            self.expect(&TokenKind::RParen)?;
            return Ok(if is_class { TypeSpec::ClassStar } else { TypeSpec::TypeStar });
        }
        let name_tok = self.advance().clone();
        let name = name_tok.text;
        self.expect(&TokenKind::RParen)?;
        Ok(if is_class { TypeSpec::Class(name) } else { TypeSpec::Type(name) })
    }

    // ---- Attribute parsing ----

    /// Try to parse a declaration attribute after a comma.
    pub fn try_parse_attribute(&mut self) -> Option<Result<Attribute, ParseError>> {
        let text = self.peek_text().to_lowercase();
        match text.as_str() {
            "allocatable" => { self.advance(); Some(Ok(Attribute::Allocatable)) }
            "pointer" => { self.advance(); Some(Ok(Attribute::Pointer)) }
            "target" => { self.advance(); Some(Ok(Attribute::Target)) }
            "optional" => { self.advance(); Some(Ok(Attribute::Optional)) }
            "save" => { self.advance(); Some(Ok(Attribute::Save)) }
            "parameter" => { self.advance(); Some(Ok(Attribute::Parameter)) }
            "value" => { self.advance(); Some(Ok(Attribute::Value)) }
            "volatile" => { self.advance(); Some(Ok(Attribute::Volatile)) }
            "asynchronous" => { self.advance(); Some(Ok(Attribute::Asynchronous)) }
            "protected" => { self.advance(); Some(Ok(Attribute::Protected)) }
            "contiguous" => { self.advance(); Some(Ok(Attribute::Contiguous)) }
            "external" => { self.advance(); Some(Ok(Attribute::External)) }
            "intrinsic" => { self.advance(); Some(Ok(Attribute::Intrinsic)) }
            "public" => { self.advance(); Some(Ok(Attribute::Public)) }
            "private" => { self.advance(); Some(Ok(Attribute::Private)) }
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
            if !self.eat(&TokenKind::Comma) { break; }
        }
        Ok(specs)
    }

    fn parse_one_array_spec(&mut self) -> Result<ArraySpec, ParseError> {
        // Assumed rank: (..)
        if self.peek() == &TokenKind::DotOp("dot".into()) {
            // Not quite right — need to check for ".." specifically.
            // For now, handle the common cases.
        }

        // Deferred shape / assumed shape: (:)
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
            return Ok(ArraySpec::Explicit { lower: Some(first), upper });
        }

        // Just an upper bound (lower is 1 implicitly).
        Ok(ArraySpec::Explicit { lower: None, upper: first })
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
            "out" => { self.advance(); Intent::Out }
            "inout" => { self.advance(); Intent::InOut }
            _ => return Err(self.error(format!("expected intent specifier, got {}", self.peek_text()))),
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
        Ok(Spanned::new(Decl::TypeDecl { type_spec, attrs, entities }, span))
    }

    fn parse_entity_list(&mut self) -> Result<Vec<EntityDecl>, ParseError> {
        let mut entities = Vec::new();
        loop {
            entities.push(self.parse_entity_decl()?);
            if !self.eat(&TokenKind::Comma) { break; }
        }
        Ok(entities)
    }

    fn parse_entity_decl(&mut self) -> Result<EntityDecl, ParseError> {
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

        Ok(EntityDecl { name, array_spec, char_len, init, ptr_init })
    }

    // ---- USE statement ----

    pub fn parse_use_stmt(&mut self) -> Result<SpannedDecl, ParseError> {
        let start = self.current_span();
        // Already consumed 'use'.

        // Optional nature: use, intrinsic :: mod or use, non_intrinsic :: mod
        let mut nature = UseNature::Normal;
        if self.eat(&TokenKind::Comma) {
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
        Ok(Spanned::new(Decl::UseStmt { module, nature, renames, only }, span))
    }

    fn parse_only_list(&mut self) -> Result<Vec<OnlyItem>, ParseError> {
        let mut items = Vec::new();
        if self.at_stmt_end() { return Ok(items); }
        loop {
            let name = self.advance().clone().text;
            if self.eat(&TokenKind::Arrow) {
                let remote = self.advance().clone().text;
                items.push(OnlyItem::Rename(Rename { local: name, remote }));
            } else {
                items.push(OnlyItem::Name(name));
            }
            if !self.eat(&TokenKind::Comma) { break; }
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
            if !self.eat(&TokenKind::Comma) { break; }
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
                        "type" => { self.advance(); type_ = true; }
                        "external" => { self.advance(); external = true; }
                        _ => break,
                    }
                    if !self.eat(&TokenKind::Comma) { break; }
                }
                self.expect(&TokenKind::RParen)?;
            }
            let span = crate::parser::expr::span_from_to(start, self.prev_span());
            return Ok(Spanned::new(Decl::ImplicitNone { external, type_ }, span));
        }

        // IMPLICIT type-spec (letter-range-list)
        let mut specs = Vec::new();
        loop {
            let type_spec = self.try_parse_type_spec()
                .ok_or_else(|| self.error("expected type specifier in IMPLICIT".into()))??;
            self.expect(&TokenKind::LParen)?;
            let mut ranges = Vec::new();
            loop {
                let start_letter = self.advance().clone().text.chars().next().unwrap_or('a');
                self.expect(&TokenKind::Minus)?;
                let end_letter = self.advance().clone().text.chars().next().unwrap_or('z');
                ranges.push((start_letter, end_letter));
                if !self.eat(&TokenKind::Comma) { break; }
            }
            self.expect(&TokenKind::RParen)?;
            specs.push(ImplicitSpec { type_spec, ranges });
            if !self.eat(&TokenKind::Comma) { break; }
        }

        let span = crate::parser::expr::span_from_to(start, self.prev_span());
        Ok(Spanned::new(Decl::ImplicitStmt { specs }, span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::ast::decl::*;

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

        panic!("could not parse as declaration: {}", src);
    }

    // ---- Type declarations ----

    #[test]
    fn integer_simple() {
        let d = parse_decl("integer :: x, y, z");
        if let Decl::TypeDecl { type_spec, entities, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Integer(None)));
            assert_eq!(entities.len(), 3);
            assert_eq!(entities[0].name, "x");
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn integer_with_init() {
        let d = parse_decl("integer :: x = 0, y = 1");
        if let Decl::TypeDecl { entities, .. } = &d.node {
            assert!(entities[0].init.is_some());
            assert!(entities[1].init.is_some());
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn integer_with_kind() {
        let d = parse_decl("integer(8) :: x");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Integer(Some(_))));
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn real_allocatable() {
        let d = parse_decl("real(8), allocatable :: matrix(:,:)");
        if let Decl::TypeDecl { type_spec, attrs, entities } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Real(Some(_))));
            assert!(attrs.contains(&Attribute::Allocatable));
            assert!(entities[0].array_spec.is_some());
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn character_deferred_length() {
        let d = parse_decl("character(len=:), allocatable :: name");
        if let Decl::TypeDecl { type_spec, attrs, .. } = &d.node {
            if let TypeSpec::Character(Some(cs)) = type_spec {
                assert!(matches!(cs.len, Some(LenSpec::Colon)));
            } else { panic!("not character type"); }
            assert!(attrs.contains(&Attribute::Allocatable));
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn character_assumed_length() {
        let d = parse_decl("character(len=*), intent(in) :: input");
        if let Decl::TypeDecl { type_spec, attrs, .. } = &d.node {
            if let TypeSpec::Character(Some(cs)) = type_spec {
                assert!(matches!(cs.len, Some(LenSpec::Star)));
            } else { panic!("not character type"); }
            assert!(attrs.iter().any(|a| matches!(a, Attribute::Intent(Intent::In))));
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn type_derived() {
        let d = parse_decl("type(my_type) :: obj");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::Type(ref n) if n == "my_type"));
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn class_star() {
        let d = parse_decl("class(*) :: poly");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::ClassStar));
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn pointer_init() {
        let d = parse_decl("type(node), pointer :: ptr => null()");
        if let Decl::TypeDecl { entities, .. } = &d.node {
            assert!(entities[0].ptr_init.is_some());
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn intent_inout() {
        let d = parse_decl("real, intent(inout) :: x");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs.iter().any(|a| matches!(a, Attribute::Intent(Intent::InOut))));
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn intent_in_out_two_words() {
        let d = parse_decl("real, intent(in out) :: x");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs.iter().any(|a| matches!(a, Attribute::Intent(Intent::InOut))));
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn multiple_attributes() {
        let d = parse_decl("real(8), dimension(:,:), allocatable, intent(inout) :: matrix");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs.iter().any(|a| matches!(a, Attribute::Dimension(_))));
            assert!(attrs.contains(&Attribute::Allocatable));
            assert!(attrs.iter().any(|a| matches!(a, Attribute::Intent(Intent::InOut))));
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn old_style_no_double_colon() {
        let d = parse_decl("integer x, y");
        if let Decl::TypeDecl { entities, .. } = &d.node {
            assert_eq!(entities.len(), 2);
            assert_eq!(entities[0].name, "x");
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn double_precision() {
        let d = parse_decl("double precision :: x");
        if let Decl::TypeDecl { type_spec, .. } = &d.node {
            assert!(matches!(type_spec, TypeSpec::DoublePrecision));
        } else { panic!("not TypeDecl"); }
    }

    #[test]
    fn bind_c() {
        let d = parse_decl("integer, bind(c) :: x");
        if let Decl::TypeDecl { attrs, .. } = &d.node {
            assert!(attrs.iter().any(|a| matches!(a, Attribute::Bind(None))));
        } else { panic!("not TypeDecl"); }
    }

    // ---- USE statements ----

    #[test]
    fn use_simple() {
        let d = parse_decl("use my_module");
        if let Decl::UseStmt { module, nature, .. } = &d.node {
            assert_eq!(module, "my_module");
            assert_eq!(*nature, UseNature::Normal);
        } else { panic!("not UseStmt"); }
    }

    #[test]
    fn use_only() {
        let d = parse_decl("use my_module, only: foo, bar");
        if let Decl::UseStmt { only, .. } = &d.node {
            let items = only.as_ref().unwrap();
            assert_eq!(items.len(), 2);
        } else { panic!("not UseStmt"); }
    }

    #[test]
    fn use_intrinsic() {
        let d = parse_decl("use, intrinsic :: iso_c_binding");
        if let Decl::UseStmt { module, nature, .. } = &d.node {
            assert_eq!(module, "iso_c_binding");
            assert_eq!(*nature, UseNature::Intrinsic);
        } else { panic!("not UseStmt"); }
    }

    #[test]
    fn use_only_with_rename() {
        let d = parse_decl("use my_module, only: local => remote");
        if let Decl::UseStmt { only, .. } = &d.node {
            let items = only.as_ref().unwrap();
            assert!(matches!(&items[0], OnlyItem::Rename(_)));
        } else { panic!("not UseStmt"); }
    }

    // ---- IMPLICIT ----

    #[test]
    fn implicit_none() {
        let d = parse_decl("implicit none");
        assert!(matches!(d.node, Decl::ImplicitNone { type_: true, external: false }));
    }

    #[test]
    fn implicit_none_type_external() {
        let d = parse_decl("implicit none(type, external)");
        assert!(matches!(d.node, Decl::ImplicitNone { type_: true, external: true }));
    }

    #[test]
    fn implicit_double_precision() {
        let d = parse_decl("implicit double precision (a-h, o-z)");
        if let Decl::ImplicitStmt { specs } = &d.node {
            assert_eq!(specs.len(), 1);
            assert!(matches!(specs[0].type_spec, TypeSpec::DoublePrecision));
            assert_eq!(specs[0].ranges.len(), 2);
        } else { panic!("not ImplicitStmt"); }
    }
}
