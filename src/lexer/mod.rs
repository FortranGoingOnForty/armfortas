//! Fortran tokenizer (free-form).
//!
//! Tokenizes preprocessed Fortran source into a stream of tokens.
//! Handles continuation lines, string literals with doubled-quote escapes,
//! numeric literals with kind suffixes, BOZ constants, dot-operators,
//! and Fortran's context-sensitive keywords (lexed as identifiers).

use std::fmt;

// ---- Token types ----

/// A Fortran token with source location.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub span: Span,
}

/// Source location span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub file_id: u32,
    pub start: Position,
    pub end: Position,
}

/// A line:column position in source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

/// Token kinds for Fortran.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ---- Literals ----
    /// Integer literal: `42`, `42_8`, `42_int64`
    IntegerLiteral,
    /// Real literal: `3.14`, `1.0d0`, `6.022e23`, `1.0_8`, `.5`, `5.`
    RealLiteral,
    /// String literal: `'hello'` or `"hello"` (with doubled-quote escapes resolved)
    StringLiteral,
    /// BOZ literal: `B'1010'`, `O'777'`, `Z'FF'`
    BozLiteral,
    /// Logical literal: `.true.`, `.false.` (with optional kind: `.true._4`)
    LogicalLiteral,

    // ---- Identifiers ----
    /// Identifier or keyword name. Keywords are NOT reserved in Fortran;
    /// the parser determines from context whether an identifier is a keyword.
    Identifier,

    // ---- Operators ----
    Plus,           // +
    Minus,          // -
    Star,           // *
    Slash,          // /
    Power,          // **
    Concat,         // //
    Eq,             // ==
    Ne,             // /=
    Lt,             // <
    Gt,             // >
    Le,             // <=
    Ge,             // >=
    /// .eq. .ne. .lt. .gt. .le. .ge. .and. .or. .not. .eqv. .neqv.
    DotOp(String),
    /// User-defined operator: .myop.
    DefinedOp(String),

    // ---- Punctuation ----
    LParen,         // (
    RParen,         // )
    LBracket,       // [
    RBracket,       // ]
    Comma,          // ,
    Colon,          // :
    ColonColon,     // ::
    Semicolon,      // ;
    Percent,        // %
    Arrow,          // =>
    Assign,         // =
    Ampersand,      // & (when not continuation)

    // ---- Special ----
    Newline,
    Comment(String),
    Eof,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::IntegerLiteral => write!(f, "integer literal"),
            TokenKind::RealLiteral => write!(f, "real literal"),
            TokenKind::StringLiteral => write!(f, "string literal"),
            TokenKind::BozLiteral => write!(f, "BOZ literal"),
            TokenKind::LogicalLiteral => write!(f, "logical literal"),
            TokenKind::Identifier => write!(f, "identifier"),
            TokenKind::Plus => write!(f, "+"),
            TokenKind::Minus => write!(f, "-"),
            TokenKind::Star => write!(f, "*"),
            TokenKind::Slash => write!(f, "/"),
            TokenKind::Power => write!(f, "**"),
            TokenKind::Concat => write!(f, "//"),
            TokenKind::Eq => write!(f, "=="),
            TokenKind::Ne => write!(f, "/="),
            TokenKind::Lt => write!(f, "<"),
            TokenKind::Gt => write!(f, ">"),
            TokenKind::Le => write!(f, "<="),
            TokenKind::Ge => write!(f, ">="),
            TokenKind::DotOp(s) => write!(f, ".{}.", s),
            TokenKind::DefinedOp(s) => write!(f, ".{}.", s),
            TokenKind::LParen => write!(f, "("),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::LBracket => write!(f, "["),
            TokenKind::RBracket => write!(f, "]"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::Colon => write!(f, ":"),
            TokenKind::ColonColon => write!(f, "::"),
            TokenKind::Semicolon => write!(f, ";"),
            TokenKind::Percent => write!(f, "%"),
            TokenKind::Arrow => write!(f, "=>"),
            TokenKind::Assign => write!(f, "="),
            TokenKind::Ampersand => write!(f, "&"),
            TokenKind::Newline => write!(f, "newline"),
            TokenKind::Comment(_) => write!(f, "comment"),
            TokenKind::Eof => write!(f, "end of file"),
        }
    }
}

// ---- Keyword recognition ----

/// Fortran keywords. These are NOT reserved — the parser decides from context.
/// The lexer provides `is_keyword()` as a helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Program, EndProgram, Module, EndModule, Submodule, EndSubmodule,
    Subroutine, EndSubroutine, Function, EndFunction, BlockData, EndBlockData,
    Contains, Use, Only, Import,
    Implicit, None,
    Integer, Real, DoublePrecision, Complex, Character, Logical, Type, Class,
    Dimension, Allocatable, Pointer, Target, Intent, In, Out, InOut,
    Optional, Save, Parameter, Value, Volatile, Asynchronous, Protected,
    Contiguous, External, Intrinsic, Bind,
    Public, Private,
    If, Then, Else, ElseIf, EndIf,
    Do, EndDo, While, Concurrent,
    Select, Case, EndSelect, Default,
    Where, EndWhere, Elsewhere,
    Forall, EndForall,
    Block, EndBlock,
    Associate, EndAssociate,
    Critical, EndCritical,
    Exit, Cycle, Stop, ErrorStop, Return, GoTo,
    Call, Print, Write, Read, Open, Close, Inquire,
    Rewind, Backspace, Endfile, Flush, Wait,
    Allocate, Deallocate, Nullify,
    Data, Common, Equivalence, Namelist, Sequence,
    Format,
    Pure, Impure, Elemental, Recursive, NonRecursive,
    Abstract, Interface, EndInterface, Procedure, Generic, Operator, Assignment,
    Entry, Result,
    Enum, Enumerator, EndEnum,
    End,
}

/// Check if a name (case-insensitive) is a Fortran keyword.
pub fn is_keyword(name: &str) -> Option<Keyword> {
    match name.to_lowercase().as_str() {
        "program" => Some(Keyword::Program),
        "endprogram" | "end program" => Some(Keyword::EndProgram),
        "module" => Some(Keyword::Module),
        "endmodule" | "end module" => Some(Keyword::EndModule),
        "submodule" => Some(Keyword::Submodule),
        "endsubmodule" | "end submodule" => Some(Keyword::EndSubmodule),
        "subroutine" => Some(Keyword::Subroutine),
        "endsubroutine" | "end subroutine" => Some(Keyword::EndSubroutine),
        "function" => Some(Keyword::Function),
        "endfunction" | "end function" => Some(Keyword::EndFunction),
        "blockdata" | "block data" => Some(Keyword::BlockData),
        "endblockdata" | "end block data" => Some(Keyword::EndBlockData),
        "contains" => Some(Keyword::Contains),
        "use" => Some(Keyword::Use),
        "only" => Some(Keyword::Only),
        "import" => Some(Keyword::Import),
        "implicit" => Some(Keyword::Implicit),
        "none" => Some(Keyword::None),
        "integer" => Some(Keyword::Integer),
        "real" => Some(Keyword::Real),
        "doubleprecision" | "double precision" => Some(Keyword::DoublePrecision),
        "complex" => Some(Keyword::Complex),
        "character" => Some(Keyword::Character),
        "logical" => Some(Keyword::Logical),
        "type" => Some(Keyword::Type),
        "class" => Some(Keyword::Class),
        "dimension" => Some(Keyword::Dimension),
        "allocatable" => Some(Keyword::Allocatable),
        "pointer" => Some(Keyword::Pointer),
        "target" => Some(Keyword::Target),
        "intent" => Some(Keyword::Intent),
        "in" => Some(Keyword::In),
        "out" => Some(Keyword::Out),
        "inout" => Some(Keyword::InOut),
        "optional" => Some(Keyword::Optional),
        "save" => Some(Keyword::Save),
        "parameter" => Some(Keyword::Parameter),
        "value" => Some(Keyword::Value),
        "volatile" => Some(Keyword::Volatile),
        "asynchronous" => Some(Keyword::Asynchronous),
        "protected" => Some(Keyword::Protected),
        "contiguous" => Some(Keyword::Contiguous),
        "external" => Some(Keyword::External),
        "intrinsic" => Some(Keyword::Intrinsic),
        "bind" => Some(Keyword::Bind),
        "public" => Some(Keyword::Public),
        "private" => Some(Keyword::Private),
        "if" => Some(Keyword::If),
        "then" => Some(Keyword::Then),
        "else" => Some(Keyword::Else),
        "elseif" | "else if" => Some(Keyword::ElseIf),
        "endif" | "end if" => Some(Keyword::EndIf),
        "do" => Some(Keyword::Do),
        "enddo" | "end do" => Some(Keyword::EndDo),
        "while" => Some(Keyword::While),
        "concurrent" => Some(Keyword::Concurrent),
        "select" => Some(Keyword::Select),
        "case" => Some(Keyword::Case),
        "endselect" | "end select" => Some(Keyword::EndSelect),
        "default" => Some(Keyword::Default),
        "where" => Some(Keyword::Where),
        "endwhere" | "end where" => Some(Keyword::EndWhere),
        "elsewhere" => Some(Keyword::Elsewhere),
        "forall" => Some(Keyword::Forall),
        "endforall" | "end forall" => Some(Keyword::EndForall),
        "block" => Some(Keyword::Block),
        "endblock" | "end block" => Some(Keyword::EndBlock),
        "associate" => Some(Keyword::Associate),
        "endassociate" | "end associate" => Some(Keyword::EndAssociate),
        "critical" => Some(Keyword::Critical),
        "endcritical" | "end critical" => Some(Keyword::EndCritical),
        "exit" => Some(Keyword::Exit),
        "cycle" => Some(Keyword::Cycle),
        "stop" => Some(Keyword::Stop),
        "error stop" => Some(Keyword::ErrorStop),
        "return" => Some(Keyword::Return),
        "goto" | "go to" => Some(Keyword::GoTo),
        "call" => Some(Keyword::Call),
        "print" => Some(Keyword::Print),
        "write" => Some(Keyword::Write),
        "read" => Some(Keyword::Read),
        "open" => Some(Keyword::Open),
        "close" => Some(Keyword::Close),
        "inquire" => Some(Keyword::Inquire),
        "rewind" => Some(Keyword::Rewind),
        "backspace" => Some(Keyword::Backspace),
        "endfile" => Some(Keyword::Endfile),
        "flush" => Some(Keyword::Flush),
        "wait" => Some(Keyword::Wait),
        "allocate" => Some(Keyword::Allocate),
        "deallocate" => Some(Keyword::Deallocate),
        "nullify" => Some(Keyword::Nullify),
        "data" => Some(Keyword::Data),
        "common" => Some(Keyword::Common),
        "equivalence" => Some(Keyword::Equivalence),
        "namelist" => Some(Keyword::Namelist),
        "sequence" => Some(Keyword::Sequence),
        "format" => Some(Keyword::Format),
        "pure" => Some(Keyword::Pure),
        "impure" => Some(Keyword::Impure),
        "elemental" => Some(Keyword::Elemental),
        "recursive" => Some(Keyword::Recursive),
        "non_recursive" => Some(Keyword::NonRecursive),
        "abstract" => Some(Keyword::Abstract),
        "interface" => Some(Keyword::Interface),
        "endinterface" | "end interface" => Some(Keyword::EndInterface),
        "procedure" => Some(Keyword::Procedure),
        "generic" => Some(Keyword::Generic),
        "operator" => Some(Keyword::Operator),
        "assignment" => Some(Keyword::Assignment),
        "entry" => Some(Keyword::Entry),
        "result" => Some(Keyword::Result),
        "enum" => Some(Keyword::Enum),
        "enumerator" => Some(Keyword::Enumerator),
        "endenum" | "end enum" => Some(Keyword::EndEnum),
        "end" => Some(Keyword::End),
        _ => None,
    }
}

// ---- Known dot-operators ----

fn is_known_dot_op(name: &str) -> bool {
    matches!(name.to_lowercase().as_str(),
        "and" | "or" | "not" | "eqv" | "neqv" |
        "eq" | "ne" | "lt" | "gt" | "le" | "ge" |
        "true" | "false"
    )
}

// ---- Lexer error ----

#[derive(Debug, Clone)]
pub struct LexError {
    pub span: Span,
    pub msg: String,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: error: {}", self.span.start.line, self.span.start.col, self.msg)
    }
}

impl std::error::Error for LexError {}

// ---- Lexer ----

/// Fortran free-form lexer.
pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
    file_id: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str, file_id: u32) -> Self {
        Self { src: src.as_bytes(), pos: 0, line: 1, col: 1, file_id }
    }

    /// Tokenize the entire source into a Vec.
    pub fn tokenize(src: &str, file_id: u32) -> Result<Vec<Token>, LexError> {
        let mut lexer = Lexer::new(src, file_id);
        let mut tokens = Vec::new();
        loop {
            let tok = lexer.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof { break; }
        }
        Ok(tokens)
    }

    fn pos(&self) -> Position {
        Position { line: self.line, col: self.col }
    }

    fn span_from(&self, start: Position) -> Span {
        Span { file_id: self.file_id, start, end: self.pos() }
    }

    fn err(&self, start: Position, msg: String) -> LexError {
        LexError { span: self.span_from(start), msg }
    }

    fn peek(&self) -> u8 {
        if self.pos < self.src.len() { self.src[self.pos] } else { 0 }
    }

    fn peek2(&self) -> u8 {
        if self.pos + 1 < self.src.len() { self.src[self.pos + 1] } else { 0 }
    }

    fn advance(&mut self) -> u8 {
        let ch = self.peek();
        if ch == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        self.pos += 1;
        ch
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn skip_spaces(&mut self) {
        while self.pos < self.src.len() && matches!(self.peek(), b' ' | b'\t' | b'\r') {
            self.advance();
        }
    }

    /// Skip a continuation: & at end of line, optional comment, newline, optional leading &.
    /// Returns true if a continuation was consumed.
    fn try_continuation(&mut self) -> bool {
        if self.peek() != b'&' { return false; }

        // Save position in case this isn't a continuation.
        let save_pos = self.pos;
        let save_line = self.line;
        let save_col = self.col;

        self.advance(); // consume &

        // Skip spaces and optional comment until newline.
        while self.pos < self.src.len() && self.peek() != b'\n' {
            if self.peek() == b'!' {
                // Skip comment to end of line.
                while self.pos < self.src.len() && self.peek() != b'\n' {
                    self.advance();
                }
                break;
            }
            if self.peek() == b' ' || self.peek() == b'\t' || self.peek() == b'\r' {
                self.advance();
            } else {
                // Non-whitespace, non-comment after & — this is just a bare &, not continuation.
                self.pos = save_pos;
                self.line = save_line;
                self.col = save_col;
                return false;
            }
        }

        if self.at_end() {
            // & at very end of file — not a continuation.
            self.pos = save_pos;
            self.line = save_line;
            self.col = save_col;
            return false;
        }

        // Consume the newline.
        self.advance();

        // Skip leading whitespace on continuation line.
        self.skip_spaces();

        // Skip optional leading & on continuation line.
        if self.peek() == b'&' {
            self.advance();
        }

        true
    }

    pub fn next_token(&mut self) -> Result<Token, LexError> {
        // Skip whitespace (but not newlines).
        self.skip_spaces();

        // Check for continuation before checking anything else.
        while self.try_continuation() {
            self.skip_spaces();
        }

        let start = self.pos();

        if self.at_end() {
            return Ok(Token {
                kind: TokenKind::Eof,
                text: String::new(),
                span: self.span_from(start),
            });
        }

        let ch = self.peek();

        // Newline (statement terminator).
        if ch == b'\n' {
            self.advance();
            return Ok(Token {
                kind: TokenKind::Newline,
                text: "\n".into(),
                span: self.span_from(start),
            });
        }

        // Comment (! to end of line).
        if ch == b'!' {
            let mut text = String::new();
            while !self.at_end() && self.peek() != b'\n' {
                text.push(self.advance() as char);
            }
            return Ok(Token {
                kind: TokenKind::Comment(text.clone()),
                text,
                span: self.span_from(start),
            });
        }

        // String literal.
        if ch == b'\'' || ch == b'"' {
            return self.lex_string(start);
        }

        // Dot-operator or logical literal: .true., .and., .myop., etc.
        // Also handles: .5 (real literal starting with dot).
        if ch == b'.' {
            if self.peek2().is_ascii_digit() {
                // Real literal starting with dot: .5, .123e4
                return self.lex_number(start);
            }
            return self.lex_dot_token(start);
        }

        // Numeric literal (integer or real).
        if ch.is_ascii_digit() {
            return self.lex_number(start);
        }

        // BOZ literal: B'...', O'...', Z'...'
        if matches!(ch, b'B' | b'b' | b'O' | b'o' | b'Z' | b'z')
            && matches!(self.peek2(), b'\'' | b'"')
        {
            return self.lex_boz(start);
        }

        // Identifier (or keyword — we lex as identifier).
        if ch.is_ascii_alphabetic() || ch == b'_' {
            return self.lex_identifier(start);
        }

        // Multi-character operators and punctuation.
        self.lex_operator_or_punct(start)
    }

    // ---- String literals ----

    fn lex_string(&mut self, start: Position) -> Result<Token, LexError> {
        let quote = self.advance();
        let mut text = String::new();
        text.push(quote as char);

        loop {
            // Handle continuation inside strings.
            if self.peek() == b'&' {
                let saved_text_len = text.len();
                let save_pos = self.pos;
                let save_line = self.line;
                let save_col = self.col;

                self.advance(); // &
                // Check if rest of line is whitespace and/or comment, then newline.
                let mut is_cont = true;
                while !self.at_end() && self.peek() != b'\n' {
                    match self.peek() {
                        b' ' | b'\t' | b'\r' => { self.advance(); }
                        b'!' => {
                            // Comment after & — skip to newline.
                            while !self.at_end() && self.peek() != b'\n' {
                                self.advance();
                            }
                            break;
                        }
                        _ => { is_cont = false; break; }
                    }
                }
                if is_cont && !self.at_end() {
                    self.advance(); // newline
                    self.skip_spaces();
                    if self.peek() == b'&' { self.advance(); }
                    continue;
                }
                // Not a continuation — restore and treat & as literal character.
                self.pos = save_pos;
                self.line = save_line;
                self.col = save_col;
                text.truncate(saved_text_len);
            }

            if self.at_end() || self.peek() == b'\n' {
                return Err(self.err(start, "unterminated string literal".into()));
            }

            let c = self.advance();
            text.push(c as char);

            if c == quote {
                if self.peek() == quote {
                    // Doubled quote escape.
                    text.push(self.advance() as char);
                } else {
                    // End of string.
                    break;
                }
            }
        }

        Ok(Token {
            kind: TokenKind::StringLiteral,
            text,
            span: self.span_from(start),
        })
    }

    // ---- Numeric literals ----

    fn lex_number(&mut self, start: Position) -> Result<Token, LexError> {
        let mut text = String::new();
        let mut is_real = false;

        // Leading digits (may be empty for .5 style).
        while self.peek().is_ascii_digit() {
            text.push(self.advance() as char);
        }

        // Decimal point.
        if self.peek() == b'.' {
            // Ambiguity: `1.0` (real) vs `1.eq.2` (integer .eq. integer).
            // The dot is part of the number ONLY if what follows is clearly numeric.
            let next = self.peek2();

            let dot_is_numeric = if text.is_empty() {
                // Leading dot (.5) — always numeric.
                true
            } else if next.is_ascii_digit() {
                // 1.5 — digit after dot, clearly a real.
                true
            } else if matches!(next, b'e' | b'E' | b'd' | b'D') {
                // Could be 1.0e5 (exponent) or 1.eq.2 (dot-operator).
                // Lookahead past the e/d: if next is digit or +/-, it's an exponent.
                // If it's another letter (like 'q' in 'eq'), it's a dot-operator.
                let after_ed = if self.pos + 2 < self.src.len() { self.src[self.pos + 2] } else { 0 };
                matches!(after_ed, b'0'..=b'9' | b'+' | b'-')
            } else if !next.is_ascii_alphabetic() {
                // 5. followed by space/operator/newline/EOF — trailing dot real.
                true
            } else {
                // 1.and.2 — dot followed by letter that's not a valid exponent start.
                false
            };

            if dot_is_numeric {
                is_real = true;
                text.push(self.advance() as char); // .
                while self.peek().is_ascii_digit() {
                    text.push(self.advance() as char);
                }
            }
            // Otherwise: `1.and.2` — the dot is NOT part of this number.
        }

        // Exponent (e/E/d/D).
        if matches!(self.peek(), b'e' | b'E' | b'd' | b'D') {
            is_real = true;
            text.push(self.advance() as char);
            if matches!(self.peek(), b'+' | b'-') {
                text.push(self.advance() as char);
            }
            if !self.peek().is_ascii_digit() {
                return Err(self.err(start, "expected digits in exponent".into()));
            }
            while self.peek().is_ascii_digit() {
                text.push(self.advance() as char);
            }
        }

        // Kind suffix: _8, _int64, _dp
        if self.peek() == b'_' {
            text.push(self.advance() as char);
            if self.peek().is_ascii_digit() {
                while self.peek().is_ascii_digit() {
                    text.push(self.advance() as char);
                }
            } else if self.peek().is_ascii_alphabetic() || self.peek() == b'_' {
                while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                    text.push(self.advance() as char);
                }
            }
        }

        Ok(Token {
            kind: if is_real { TokenKind::RealLiteral } else { TokenKind::IntegerLiteral },
            text,
            span: self.span_from(start),
        })
    }

    // ---- BOZ literals ----

    fn lex_boz(&mut self, start: Position) -> Result<Token, LexError> {
        let mut text = String::new();
        text.push(self.advance() as char); // B/O/Z
        let quote = self.advance();
        text.push(quote as char);

        while !self.at_end() && self.peek() != quote {
            if self.peek() == b'\n' {
                return Err(self.err(start, "unterminated BOZ literal".into()));
            }
            text.push(self.advance() as char);
        }
        if self.at_end() {
            return Err(self.err(start, "unterminated BOZ literal".into()));
        }
        text.push(self.advance() as char); // closing quote

        Ok(Token {
            kind: TokenKind::BozLiteral,
            text,
            span: self.span_from(start),
        })
    }

    // ---- Dot-tokens (.and., .true., .myop.) ----

    fn lex_dot_token(&mut self, start: Position) -> Result<Token, LexError> {
        self.advance(); // consume first .
        let mut name = String::new();

        while self.peek().is_ascii_alphabetic() || self.peek() == b'_' {
            name.push(self.advance() as char);
        }

        if name.is_empty() {
            // Bare dot — could be part of number already handled, or an error.
            // In practice, could be a component separator in derived types
            // but Fortran uses % for that. Return as a period for now.
            return Err(self.err(start, "unexpected '.'".into()));
        }

        // Expect closing dot.
        if self.peek() == b'.' {
            self.advance();
        } else {
            return Err(self.err(start, format!("expected closing '.' after .{}", name)));
        }

        let lower = name.to_lowercase();
        let text = format!(".{}.", name); // preserve original case in text

        // Check for logical literals.
        if lower == "true" || lower == "false" {
            let mut full_text = text.clone();
            // Optional kind suffix: .true._4
            if self.peek() == b'_' {
                full_text.push(self.advance() as char);
                while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                    full_text.push(self.advance() as char);
                }
            }
            return Ok(Token {
                kind: TokenKind::LogicalLiteral,
                text: full_text,
                span: self.span_from(start),
            });
        }

        // Known dot-operator.
        if is_known_dot_op(&lower) {
            return Ok(Token {
                kind: TokenKind::DotOp(lower),
                text,
                span: self.span_from(start),
            });
        }

        // User-defined operator.
        Ok(Token {
            kind: TokenKind::DefinedOp(lower),
            text,
            span: self.span_from(start),
        })
    }

    // ---- Identifier ----

    fn lex_identifier(&mut self, start: Position) -> Result<Token, LexError> {
        let mut text = String::new();
        while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
            text.push(self.advance() as char);
        }

        Ok(Token {
            kind: TokenKind::Identifier,
            text,
            span: self.span_from(start),
        })
    }

    // ---- Operators and punctuation ----

    fn lex_operator_or_punct(&mut self, start: Position) -> Result<Token, LexError> {
        let ch = self.advance();
        let next = self.peek();

        let (kind, text) = match ch {
            b'+' => (TokenKind::Plus, "+"),
            b'-' => (TokenKind::Minus, "-"),
            b'*' if next == b'*' => { self.advance(); (TokenKind::Power, "**") }
            b'*' => (TokenKind::Star, "*"),
            b'/' if next == b'/' => { self.advance(); (TokenKind::Concat, "//") }
            b'/' if next == b'=' => { self.advance(); (TokenKind::Ne, "/=") }
            b'/' => (TokenKind::Slash, "/"),
            b'=' if next == b'=' => { self.advance(); (TokenKind::Eq, "==") }
            b'=' if next == b'>' => { self.advance(); (TokenKind::Arrow, "=>") }
            b'=' => (TokenKind::Assign, "="),
            b'<' if next == b'=' => { self.advance(); (TokenKind::Le, "<=") }
            b'<' => (TokenKind::Lt, "<"),
            b'>' if next == b'=' => { self.advance(); (TokenKind::Ge, ">=") }
            b'>' => (TokenKind::Gt, ">"),
            b'(' => (TokenKind::LParen, "("),
            b')' => (TokenKind::RParen, ")"),
            b'[' => (TokenKind::LBracket, "["),
            b']' => (TokenKind::RBracket, "]"),
            b',' => (TokenKind::Comma, ","),
            b':' if next == b':' => { self.advance(); (TokenKind::ColonColon, "::") }
            b':' => (TokenKind::Colon, ":"),
            b';' => (TokenKind::Semicolon, ";"),
            b'%' => (TokenKind::Percent, "%"),
            b'&' => (TokenKind::Ampersand, "&"),
            _ => return Err(self.err(start, format!("unexpected character: '{}'", ch as char))),
        };

        Ok(Token {
            kind,
            text: text.into(),
            span: self.span_from(start),
        })
    }
}

// ---- Tests ----

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Token> {
        Lexer::tokenize(src, 0).unwrap()
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        toks(src).into_iter().map(|t| t.kind).filter(|k| !matches!(k, TokenKind::Eof)).collect()
    }

    fn texts(src: &str) -> Vec<String> {
        toks(src).into_iter().map(|t| t.text).filter(|t| !t.is_empty()).collect()
    }

    // ---- Identifiers ----

    #[test]
    fn simple_identifier() {
        assert_eq!(kinds("foo"), vec![TokenKind::Identifier]);
        assert_eq!(texts("foo"), vec!["foo"]);
    }

    #[test]
    fn identifier_with_underscore_and_digits() {
        assert_eq!(texts("my_var_2"), vec!["my_var_2"]);
    }

    #[test]
    fn keyword_is_identifier() {
        // Keywords are not reserved — lexed as identifiers.
        let toks = kinds("integer real do if");
        assert_eq!(toks, vec![
            TokenKind::Identifier, TokenKind::Identifier,
            TokenKind::Identifier, TokenKind::Identifier,
        ]);
    }

    #[test]
    fn is_keyword_helper() {
        assert_eq!(is_keyword("integer"), Some(Keyword::Integer));
        assert_eq!(is_keyword("INTEGER"), Some(Keyword::Integer));
        assert_eq!(is_keyword("foo"), None);
    }

    // ---- Integer literals ----

    #[test]
    fn integer_literal() {
        assert_eq!(kinds("42"), vec![TokenKind::IntegerLiteral]);
        assert_eq!(texts("42"), vec!["42"]);
    }

    #[test]
    fn integer_with_kind() {
        assert_eq!(texts("42_8"), vec!["42_8"]);
        assert_eq!(kinds("42_8"), vec![TokenKind::IntegerLiteral]);
    }

    #[test]
    fn integer_with_named_kind() {
        assert_eq!(texts("42_int64"), vec!["42_int64"]);
        assert_eq!(kinds("42_int64"), vec![TokenKind::IntegerLiteral]);
    }

    // ---- Real literals ----

    #[test]
    fn real_literal_basic() {
        assert_eq!(kinds("3.14"), vec![TokenKind::RealLiteral]);
        assert_eq!(texts("3.14"), vec!["3.14"]);
    }

    #[test]
    fn real_literal_exponent_e() {
        assert_eq!(texts("6.022e23"), vec!["6.022e23"]);
        assert_eq!(kinds("6.022e23"), vec![TokenKind::RealLiteral]);
    }

    #[test]
    fn real_literal_exponent_d() {
        assert_eq!(texts("1.0d0"), vec!["1.0d0"]);
        assert_eq!(kinds("1.0d0"), vec![TokenKind::RealLiteral]);
    }

    #[test]
    fn real_literal_signed_exponent() {
        assert_eq!(texts("1.0e-5"), vec!["1.0e-5"]);
    }

    #[test]
    fn real_literal_leading_dot() {
        assert_eq!(texts(".5"), vec![".5"]);
        assert_eq!(kinds(".5"), vec![TokenKind::RealLiteral]);
    }

    #[test]
    fn real_literal_trailing_dot() {
        assert_eq!(texts("5."), vec!["5."]);
        assert_eq!(kinds("5."), vec![TokenKind::RealLiteral]);
    }

    #[test]
    fn real_with_kind_suffix() {
        assert_eq!(texts("3.14_8"), vec!["3.14_8"]);
        assert_eq!(kinds("3.14_8"), vec![TokenKind::RealLiteral]);
    }

    #[test]
    fn real_with_named_kind() {
        assert_eq!(texts("1.0_dp"), vec!["1.0_dp"]);
    }

    // ---- String literals ----

    #[test]
    fn string_single_quote() {
        let toks = toks("'hello'");
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].text, "'hello'");
    }

    #[test]
    fn string_double_quote() {
        let toks = toks("\"hello\"");
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].text, "\"hello\"");
    }

    #[test]
    fn string_doubled_quote_escape() {
        let toks = toks("'it''s'");
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].text, "'it''s'");
    }

    #[test]
    fn string_continuation() {
        // String continued across lines with &.
        // Content before & is part of the string (including the space).
        let src = "'hello &\n     &world'";
        let toks = toks(src);
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].text, "'hello world'");
    }

    #[test]
    fn string_continuation_no_space() {
        // No space before &: content is 'helloworld'.
        let src = "'hello&\n     &world'";
        let toks = toks(src);
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].text, "'helloworld'");
    }

    #[test]
    fn string_empty() {
        assert_eq!(toks("''")[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks("''")[0].text, "''");
    }

    // ---- BOZ literals ----

    #[test]
    fn boz_binary() {
        let t = &toks("B'1010'")[0];
        assert_eq!(t.kind, TokenKind::BozLiteral);
        assert_eq!(t.text, "B'1010'");
    }

    #[test]
    fn boz_octal() {
        let t = &toks("O'777'")[0];
        assert_eq!(t.kind, TokenKind::BozLiteral);
        assert_eq!(t.text, "O'777'");
    }

    #[test]
    fn boz_hex() {
        let t = &toks("Z'FF'")[0];
        assert_eq!(t.kind, TokenKind::BozLiteral);
        assert_eq!(t.text, "Z'FF'");
    }

    #[test]
    fn boz_lowercase() {
        let t = &toks("b\"1010\"")[0];
        assert_eq!(t.kind, TokenKind::BozLiteral);
        assert_eq!(t.text, "b\"1010\"");
    }

    // ---- Logical literals ----

    #[test]
    fn logical_true() {
        let t = &toks(".true.")[0];
        assert_eq!(t.kind, TokenKind::LogicalLiteral);
        assert_eq!(t.text, ".true.");
    }

    #[test]
    fn logical_false() {
        let t = &toks(".false.")[0];
        assert_eq!(t.kind, TokenKind::LogicalLiteral);
        assert_eq!(t.text, ".false.");
    }

    #[test]
    fn logical_with_kind() {
        let t = &toks(".true._4")[0];
        assert_eq!(t.kind, TokenKind::LogicalLiteral);
        assert_eq!(t.text, ".true._4");
    }

    // ---- Operators ----

    #[test]
    fn arithmetic_operators() {
        assert_eq!(kinds("+ - * /"), vec![
            TokenKind::Plus, TokenKind::Minus, TokenKind::Star, TokenKind::Slash,
        ]);
    }

    #[test]
    fn power_operator() {
        assert_eq!(kinds("**"), vec![TokenKind::Power]);
    }

    #[test]
    fn concat_operator() {
        assert_eq!(kinds("//"), vec![TokenKind::Concat]);
    }

    #[test]
    fn comparison_operators() {
        assert_eq!(kinds("== /= < > <= >="), vec![
            TokenKind::Eq, TokenKind::Ne, TokenKind::Lt, TokenKind::Gt,
            TokenKind::Le, TokenKind::Ge,
        ]);
    }

    #[test]
    fn dot_comparison_operators() {
        assert_eq!(kinds(".eq. .ne. .lt. .gt. .le. .ge."), vec![
            TokenKind::DotOp("eq".into()), TokenKind::DotOp("ne".into()),
            TokenKind::DotOp("lt".into()), TokenKind::DotOp("gt".into()),
            TokenKind::DotOp("le".into()), TokenKind::DotOp("ge".into()),
        ]);
    }

    #[test]
    fn dot_logical_operators() {
        assert_eq!(kinds(".and. .or. .not."), vec![
            TokenKind::DotOp("and".into()), TokenKind::DotOp("or".into()),
            TokenKind::DotOp("not".into()),
        ]);
    }

    #[test]
    fn dot_eqv_neqv() {
        assert_eq!(kinds(".eqv. .neqv."), vec![
            TokenKind::DotOp("eqv".into()), TokenKind::DotOp("neqv".into()),
        ]);
    }

    #[test]
    fn defined_operator() {
        assert_eq!(kinds(".myop."), vec![TokenKind::DefinedOp("myop".into())]);
    }

    // ---- Punctuation ----

    #[test]
    fn punctuation() {
        assert_eq!(kinds("( ) [ ] , : ; %"), vec![
            TokenKind::LParen, TokenKind::RParen,
            TokenKind::LBracket, TokenKind::RBracket,
            TokenKind::Comma, TokenKind::Colon, TokenKind::Semicolon, TokenKind::Percent,
        ]);
    }

    #[test]
    fn double_colon() {
        assert_eq!(kinds("::"), vec![TokenKind::ColonColon]);
    }

    #[test]
    fn arrow() {
        assert_eq!(kinds("=>"), vec![TokenKind::Arrow]);
    }

    #[test]
    fn assign() {
        assert_eq!(kinds("="), vec![TokenKind::Assign]);
    }

    // ---- Comments ----

    #[test]
    fn comment() {
        let toks = kinds("x ! this is a comment\n");
        assert_eq!(toks.len(), 3); // identifier, comment, newline
        assert!(matches!(toks[1], TokenKind::Comment(_)));
    }

    // ---- Newlines ----

    #[test]
    fn newline_is_statement_terminator() {
        assert_eq!(kinds("x\ny"), vec![
            TokenKind::Identifier, TokenKind::Newline, TokenKind::Identifier,
        ]);
    }

    #[test]
    fn semicolon_is_statement_separator() {
        assert_eq!(kinds("x; y"), vec![
            TokenKind::Identifier, TokenKind::Semicolon, TokenKind::Identifier,
        ]);
    }

    // ---- Continuation lines ----

    #[test]
    fn continuation_joins_tokens() {
        let k = kinds("x + &\n  y");
        assert_eq!(k, vec![TokenKind::Identifier, TokenKind::Plus, TokenKind::Identifier]);
    }

    #[test]
    fn continuation_with_leading_ampersand() {
        let k = kinds("x + &\n  &y");
        assert_eq!(k, vec![TokenKind::Identifier, TokenKind::Plus, TokenKind::Identifier]);
    }

    #[test]
    fn continuation_with_comment() {
        let k = kinds("x + & ! comment\n  y");
        assert_eq!(k, vec![TokenKind::Identifier, TokenKind::Plus, TokenKind::Identifier]);
    }

    // ---- Source locations ----

    #[test]
    fn source_locations() {
        let toks = toks("x = 42\n");
        // x at line 1, col 1
        assert_eq!(toks[0].span.start, Position { line: 1, col: 1 });
        // = at line 1, col 3
        assert_eq!(toks[1].span.start, Position { line: 1, col: 3 });
        // 42 at line 1, col 5
        assert_eq!(toks[2].span.start, Position { line: 1, col: 5 });
    }

    // ---- Complex expressions ----

    #[test]
    fn complex_declaration() {
        let k = kinds("integer, allocatable :: x(:,:)");
        assert_eq!(k, vec![
            TokenKind::Identifier, TokenKind::Comma, TokenKind::Identifier,
            TokenKind::ColonColon,
            TokenKind::Identifier, TokenKind::LParen, TokenKind::Colon,
            TokenKind::Comma, TokenKind::Colon, TokenKind::RParen,
        ]);
    }

    #[test]
    fn pointer_assignment() {
        let k = kinds("ptr => target");
        assert_eq!(k, vec![TokenKind::Identifier, TokenKind::Arrow, TokenKind::Identifier]);
    }

    #[test]
    fn component_access() {
        let k = kinds("obj%member");
        assert_eq!(k, vec![TokenKind::Identifier, TokenKind::Percent, TokenKind::Identifier]);
    }

    #[test]
    fn array_constructor_bracket() {
        let k = kinds("[1, 2, 3]");
        assert_eq!(k, vec![
            TokenKind::LBracket, TokenKind::IntegerLiteral, TokenKind::Comma,
            TokenKind::IntegerLiteral, TokenKind::Comma, TokenKind::IntegerLiteral,
            TokenKind::RBracket,
        ]);
    }

    // ---- Ambiguity cases from spec ----

    #[test]
    fn real_number_not_dot_op() {
        // 1.0 should lex as a single real literal, not 1 .0
        let k = kinds("1.0");
        assert_eq!(k, vec![TokenKind::RealLiteral]);
    }

    #[test]
    fn integer_dot_and_with_spaces() {
        let k = kinds("a .and. b");
        assert_eq!(k, vec![
            TokenKind::Identifier, TokenKind::DotOp("and".into()), TokenKind::Identifier,
        ]);
    }

    #[test]
    fn integer_dot_eq_no_spaces() {
        // Critical ambiguity: 1.eq.2 must NOT be parsed as a real with exponent.
        // It's: integer(1), .eq., integer(2)
        let k = kinds("1.eq.2");
        assert_eq!(k, vec![
            TokenKind::IntegerLiteral, TokenKind::DotOp("eq".into()), TokenKind::IntegerLiteral,
        ]);
    }

    #[test]
    fn integer_dot_and_no_spaces() {
        let k = kinds("1.and.2");
        assert_eq!(k, vec![
            TokenKind::IntegerLiteral, TokenKind::DotOp("and".into()), TokenKind::IntegerLiteral,
        ]);
    }

    #[test]
    fn integer_dot_ne_no_spaces() {
        let k = kinds("x.ne.y");
        assert_eq!(k, vec![
            TokenKind::Identifier, TokenKind::DotOp("ne".into()), TokenKind::Identifier,
        ]);
    }

    #[test]
    fn real_with_exponent_not_dot_op() {
        // 1.0e5 is a real, not 1 .0e5
        assert_eq!(kinds("1.0e5"), vec![TokenKind::RealLiteral]);
        assert_eq!(texts("1.0e5"), vec!["1.0e5"]);
    }

    #[test]
    fn real_with_d_exponent_not_dot_op() {
        // 1.0d0 is a double-precision real
        assert_eq!(kinds("1.0d0"), vec![TokenKind::RealLiteral]);
    }

    #[test]
    fn real_dot_e_plus_is_exponent() {
        // 1.e+5 — the e is followed by + so it's an exponent
        assert_eq!(kinds("1.e+5"), vec![TokenKind::RealLiteral]);
        assert_eq!(texts("1.e+5"), vec!["1.e+5"]);
    }

    #[test]
    fn five_dot_eq_three() {
        // 5.eq.3 — integer, .eq., integer (not a real)
        let k = kinds("5.eq.3");
        assert_eq!(k, vec![
            TokenKind::IntegerLiteral, TokenKind::DotOp("eq".into()), TokenKind::IntegerLiteral,
        ]);
    }

    // ---- Error cases ----

    #[test]
    fn unterminated_string() {
        let result = Lexer::tokenize("'unterminated\n", 0);
        assert!(result.is_err());
    }

    #[test]
    fn unterminated_boz() {
        let result = Lexer::tokenize("B'1010\n", 0);
        assert!(result.is_err());
    }

    // ---- Multi-line program ----

    #[test]
    fn simple_program() {
        let src = "\
program hello
    implicit none
    integer :: x
    x = 42
    print *, x
end program hello
";
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let ident_count = tokens.iter().filter(|t| t.kind == TokenKind::Identifier).count();
        assert!(ident_count >= 8, "expected 8+ identifiers, got {}", ident_count);
        let last_non_eof = tokens.iter().rev().find(|t| t.kind != TokenKind::Eof && t.kind != TokenKind::Newline).unwrap();
        assert_eq!(last_non_eof.text, "hello");
    }

    // ---- fortsh tokenization ----

    /// Try to tokenize a fortsh source file. Strips preprocessor directives first.
    fn try_lex_fortsh_file(path: &str) -> Result<usize, String> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: {}", path, e))?;

        // Strip preprocessor directives (lexer expects preprocessed input).
        let filtered: String = src.lines()
            .map(|line| if line.trim_start().starts_with('#') { "" } else { line })
            .collect::<Vec<_>>()
            .join("\n");

        let tokens = Lexer::tokenize(&filtered, 0)
            .map_err(|e| format!("{}: {}", path, e))?;

        Ok(tokens.len())
    }

    #[test]
    fn tokenize_fortsh_types() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../fortsh/src/common/types.f90");
        if !std::path::Path::new(path).exists() {
            // fortsh not available — skip gracefully.
            return;
        }
        let count = try_lex_fortsh_file(path).unwrap();
        assert!(count > 100, "expected 100+ tokens from types.f90, got {}", count);
    }

    #[test]
    fn tokenize_fortsh_error_handling() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../fortsh/src/common/error_handling.f90");
        if !std::path::Path::new(path).exists() { return; }
        let count = try_lex_fortsh_file(path).unwrap();
        assert!(count > 50, "expected 50+ tokens, got {}", count);
    }

    #[test]
    fn tokenize_fortsh_all_common() {
        let common_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../fortsh/src/common");
        let dir = std::path::Path::new(common_dir);
        if !dir.exists() { return; }

        let mut files_tested = 0;
        let mut total_tokens = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "f90") {
                let path_str = path.to_string_lossy();
                match try_lex_fortsh_file(&path_str) {
                    Ok(count) => {
                        total_tokens += count;
                        files_tested += 1;
                    }
                    Err(e) => panic!("failed to tokenize {}: {}", path_str, e),
                }
            }
        }
        assert!(files_tested > 0, "no .f90 files found in fortsh/src/common");
        eprintln!("tokenized {} fortsh common/ files, {} total tokens", files_tested, total_tokens);
    }

    #[test]
    fn tokenize_fortsh_all_sources() {
        let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../fortsh/src");
        let root = std::path::Path::new(src_dir);
        if !root.exists() { return; }

        let mut files_tested = 0;
        let mut total_tokens = 0;
        let mut failures = Vec::new();

        fn visit_dir(dir: &std::path::Path, files_tested: &mut usize, total_tokens: &mut usize, failures: &mut Vec<String>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        visit_dir(&path, files_tested, total_tokens, failures);
                    } else if path.extension().map_or(false, |e| e == "f90") {
                        let path_str = path.to_string_lossy().to_string();
                        match super::tests::try_lex_fortsh_file(&path_str) {
                            Ok(count) => {
                                *total_tokens += count;
                                *files_tested += 1;
                            }
                            Err(e) => failures.push(e),
                        }
                    }
                }
            }
        }

        visit_dir(root, &mut files_tested, &mut total_tokens, &mut failures);

        if !failures.is_empty() {
            panic!("{} of {} files failed to tokenize:\n{}", failures.len(), files_tested + failures.len(),
                failures.join("\n"));
        }

        assert!(files_tested > 40, "expected 40+ .f90 files, found {}", files_tested);
        eprintln!("tokenized ALL {} fortsh .f90 files, {} total tokens", files_tested, total_tokens);
    }
}
