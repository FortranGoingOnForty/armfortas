//! Fixed-form (F77) Fortran lexer.
//!
//! Two-pass approach:
//! 1. Preprocess lines: identify comments, extract labels, join continuations,
//!    strip columns 73+, handle tab-form extension.
//! 2. Tokenize each logical statement body, handling whitespace insensitivity
//!    and Hollerith constants.
//!
//! Produces the same Token types as the free-form lexer.

use super::{Token, TokenKind, Span, Position, LexError, is_known_dot_op};

/// Tokenize fixed-form Fortran source.
pub fn tokenize_fixed(src: &str, file_id: u32) -> Result<Vec<Token>, LexError> {
    let statements = preprocess_lines(src, file_id);
    let mut tokens = Vec::new();

    for stmt in &statements {
        match stmt {
            FixedLine::Comment { text, span } => {
                tokens.push(Token {
                    kind: TokenKind::Comment,
                    text: text.clone(),
                    span: *span,
                });
                tokens.push(Token {
                    kind: TokenKind::Newline,
                    text: "\n".into(),
                    span: *span,
                });
            }
            FixedLine::Statement { label, body, start_line, file_id: fid } => {
                // Emit label as integer literal if present.
                if let Some(label_text) = label {
                    let label_trimmed = label_text.trim();
                    if !label_trimmed.is_empty() {
                        tokens.push(Token {
                            kind: TokenKind::IntegerLiteral,
                            text: label_trimmed.to_string(),
                            span: Span {
                                file_id: *fid,
                                start: Position { line: *start_line, col: 1 },
                                end: Position { line: *start_line, col: 6 },
                            },
                        });
                    }
                }

                // Tokenize the body with the whitespace-insensitive scanner.
                let body_tokens = tokenize_body(body, *fid, *start_line)?;
                tokens.extend(body_tokens);

                tokens.push(Token {
                    kind: TokenKind::Newline,
                    text: "\n".into(),
                    span: Span {
                        file_id: *fid,
                        start: Position { line: *start_line, col: 1 },
                        end: Position { line: *start_line, col: 1 },
                    },
                });
            }
            FixedLine::Blank { span } => {
                tokens.push(Token {
                    kind: TokenKind::Newline,
                    text: "\n".into(),
                    span: *span,
                });
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        text: String::new(),
        span: Span {
            file_id,
            start: Position { line: src.lines().count() as u32 + 1, col: 1 },
            end: Position { line: src.lines().count() as u32 + 1, col: 1 },
        },
    });

    Ok(tokens)
}

// ---- Whitespace-insensitive body tokenizer ----

/// Tokenize a fixed-form statement body with whitespace insensitivity.
///
/// Three-phase approach:
/// 1. Protect Hollerith constants (nH...) by converting to string literals before stripping
/// 2. Strip all whitespace outside string literals
/// 3. Tokenize with keyword-splitting: longest keyword prefix match at letter runs
fn tokenize_body(body: &str, file_id: u32, line: u32) -> Result<Vec<Token>, LexError> {
    // Phase 1: Convert Hollerith constants to string literals (preserves their spaces).
    let hollerith_protected = protect_hollerith(body);
    // Phase 2: Strip whitespace outside string literals.
    let stripped = strip_whitespace_outside_strings(&hollerith_protected);
    let bytes = stripped.as_bytes();
    let mut tokens = Vec::new();
    let mut pos = 0;

    while pos < bytes.len() {
        let col = (pos as u32) + 7;
        let start = Position { line, col };
        let ch = bytes[pos];

        // Comment (! to end).
        if ch == b'!' {
            tokens.push(Token {
                kind: TokenKind::Comment,
                text: stripped[pos..].to_string(),
                span: Span { file_id, start, end: Position { line, col: col + (bytes.len() - pos) as u32 } },
            });
            break;
        }

        // String literal.
        if ch == b'\'' || ch == b'"' {
            let (tok, consumed) = lex_fixed_string(&stripped, pos, file_id, line)?;
            tokens.push(tok);
            pos += consumed;
            continue;
        }

        // Dot-operator or real starting with dot.
        if ch == b'.' {
            if pos + 1 < bytes.len() && bytes[pos + 1].is_ascii_digit() {
                let (tok, consumed) = lex_fixed_number(&stripped, pos, file_id, line);
                tokens.push(tok);
                pos += consumed;
            } else {
                let (tok, consumed) = lex_fixed_dot_op(&stripped, pos, file_id, line)?;
                tokens.push(tok);
                pos += consumed;
            }
            continue;
        }

        // Number (integer or real).
        if ch.is_ascii_digit() {
            let (tok, consumed) = lex_fixed_number(&stripped, pos, file_id, line);
            tokens.push(tok);
            pos += consumed;
            continue;
        }

        // BOZ literal: B/O/Z followed by quote.
        if matches!(ch, b'B' | b'b' | b'O' | b'o' | b'Z' | b'z')
            && pos + 1 < bytes.len() && matches!(bytes[pos + 1], b'\'' | b'"')
        {
            let (tok, consumed) = lex_fixed_boz(&stripped, pos, file_id, line)?;
            tokens.push(tok);
            pos += consumed;
            continue;
        }

        // Letter — keyword or identifier with keyword-splitting.
        if ch.is_ascii_alphabetic() || ch == b'_' {
            let (tok, consumed) = lex_fixed_ident_or_keyword(&stripped, pos, file_id, line);
            tokens.push(tok);
            pos += consumed;
            continue;
        }

        // Operators and punctuation.
        let (tok, consumed) = lex_fixed_punct(&stripped, pos, file_id, line)?;
        tokens.push(tok);
        pos += consumed;
    }

    Ok(tokens)
}

/// Convert Hollerith constants (nH...) to quoted string literals BEFORE whitespace stripping.
/// This preserves spaces inside Hollerith content: `6H HELLO` → `' HELLO'`.
fn protect_hollerith(body: &str) -> String {
    let bytes = body.as_bytes();
    let mut result = String::with_capacity(body.len());
    let mut i = 0;

    while i < bytes.len() {
        // Inside a string literal: copy verbatim.
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            let quote = bytes[i];
            result.push(bytes[i] as char);
            i += 1;
            while i < bytes.len() {
                result.push(bytes[i] as char);
                if bytes[i] == quote {
                    i += 1;
                    if i < bytes.len() && bytes[i] == quote {
                        result.push(bytes[i] as char);
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Check for Hollerith: digits followed by H, not preceded by a letter/digit.
        if bytes[i].is_ascii_digit() {
            let preceded_by_alnum = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            if !preceded_by_alnum {
                let digit_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
                if i < bytes.len() && (bytes[i] == b'H' || bytes[i] == b'h') {
                    if let Ok(count) = body[digit_start..i].parse::<usize>() {
                        i += 1; // skip H
                        if count > 0 && i + count <= bytes.len() {
                            // Replace nH... with '...'
                            result.push('\'');
                            result.push_str(&body[i..i + count]);
                            result.push('\'');
                            i += count;
                            continue;
                        }
                    }
                }
                // Not Hollerith — put the digits back.
                result.push_str(&body[digit_start..i]);
                continue;
            }
        }

        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Strip whitespace from body text, preserving content inside string literals.
fn strip_whitespace_outside_strings(body: &str) -> String {
    let mut result = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            let quote = bytes[i];
            result.push(quote as char);
            i += 1;
            while i < bytes.len() {
                result.push(bytes[i] as char);
                if bytes[i] == quote {
                    i += 1;
                    if i < bytes.len() && bytes[i] == quote {
                        result.push(bytes[i] as char);
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b' ' || bytes[i] == b'\t' {
            i += 1;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

/// Lex a string literal in whitespace-stripped body.
fn lex_fixed_string(text: &str, pos: usize, file_id: u32, line: u32) -> Result<(Token, usize), LexError> {
    let bytes = text.as_bytes();
    let quote = bytes[pos];
    let mut end = pos + 1;
    let mut tok_text = String::new();
    tok_text.push(quote as char);

    while end < bytes.len() {
        tok_text.push(bytes[end] as char);
        if bytes[end] == quote {
            end += 1;
            if end < bytes.len() && bytes[end] == quote {
                tok_text.push(bytes[end] as char);
                end += 1;
            } else {
                break;
            }
        } else {
            end += 1;
        }
    }

    let col = (pos as u32) + 7;
    Ok((Token {
        kind: TokenKind::StringLiteral,
        text: tok_text,
        span: Span {
            file_id,
            start: Position { line, col },
            end: Position { line, col: col + (end - pos) as u32 },
        },
    }, end - pos))
}

/// Lex a dot-operator (.AND., .EQ., .TRUE., .myop.) in whitespace-stripped body.
fn lex_fixed_dot_op(text: &str, pos: usize, file_id: u32, line: u32) -> Result<(Token, usize), LexError> {
    let bytes = text.as_bytes();
    let mut end = pos + 1; // skip first dot
    let mut name = String::new();

    while end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'_') {
        name.push(bytes[end] as char);
        end += 1;
    }

    if end < bytes.len() && bytes[end] == b'.' {
        end += 1; // closing dot
    }

    let lower = name.to_lowercase();
    let col = (pos as u32) + 7;
    let tok_text = format!(".{}.", name);
    let span = Span { file_id, start: Position { line, col }, end: Position { line, col: col + (end - pos) as u32 } };

    if lower == "true" || lower == "false" {
        // Check for kind suffix.
        let mut full_text = tok_text;
        if end < bytes.len() && bytes[end] == b'_' {
            full_text.push('_');
            end += 1;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                full_text.push(bytes[end] as char);
                end += 1;
            }
        }
        return Ok((Token { kind: TokenKind::LogicalLiteral, text: full_text, span }, end - pos));
    }

    let kind = if is_known_dot_op(&lower) {
        TokenKind::DotOp(lower)
    } else {
        TokenKind::DefinedOp(name.to_lowercase())
    };

    Ok((Token { kind, text: tok_text, span }, end - pos))
}

/// Lex a number (integer or real) in whitespace-stripped body.
fn lex_fixed_number(text: &str, pos: usize, file_id: u32, line: u32) -> (Token, usize) {
    let bytes = text.as_bytes();
    let mut end = pos;
    let mut is_real = false;
    let mut tok_text = String::new();

    // Leading digits.
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        tok_text.push(bytes[end] as char);
        end += 1;
    }

    // Decimal point — but not if followed by letter (dot-op like .EQ.).
    if end < bytes.len() && bytes[end] == b'.' {
        let after_dot = if end + 1 < bytes.len() { bytes[end + 1] } else { 0 };
        let dot_is_numeric = after_dot.is_ascii_digit()
            || tok_text.is_empty() // leading dot
            || {
                // Check for exponent: .e5 vs .eq.
                if matches!(after_dot, b'e' | b'E' | b'd' | b'D') {
                    let after_ed = if end + 2 < bytes.len() { bytes[end + 2] } else { 0 };
                    matches!(after_ed, b'0'..=b'9' | b'+' | b'-')
                } else {
                    !after_dot.is_ascii_alphabetic() // 5. followed by op/end
                }
            };

        if dot_is_numeric {
            is_real = true;
            tok_text.push(bytes[end] as char);
            end += 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                tok_text.push(bytes[end] as char);
                end += 1;
            }
        }
    }

    // Exponent — only consume e/d if followed by digit or +/- then digit.
    // This prevents `10DO` from being lexed as real `10D` + identifier `O`.
    if end < bytes.len() && matches!(bytes[end], b'e' | b'E' | b'd' | b'D') {
        let after_ed = if end + 1 < bytes.len() { bytes[end + 1] } else { 0 };
        let has_exponent_digits = after_ed.is_ascii_digit()
            || (matches!(after_ed, b'+' | b'-') && end + 2 < bytes.len() && bytes[end + 2].is_ascii_digit());

        if has_exponent_digits {
            is_real = true;
            tok_text.push(bytes[end] as char);
            end += 1;
            if end < bytes.len() && matches!(bytes[end], b'+' | b'-') {
                tok_text.push(bytes[end] as char);
                end += 1;
            }
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                tok_text.push(bytes[end] as char);
                end += 1;
            }
        }
    }

    // Kind suffix.
    if end < bytes.len() && bytes[end] == b'_' {
        tok_text.push(bytes[end] as char);
        end += 1;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            tok_text.push(bytes[end] as char);
            end += 1;
        }
    }

    let col = (pos as u32) + 7;
    let kind = if is_real { TokenKind::RealLiteral } else { TokenKind::IntegerLiteral };
    (Token {
        kind,
        text: tok_text,
        span: Span { file_id, start: Position { line, col }, end: Position { line, col: col + (end - pos) as u32 } },
    }, end - pos)
}

/// Lex an identifier or keyword in whitespace-stripped body.
///
/// Lex an identifier in whitespace-stripped fixed-form body.
///
/// Emits the full alphanumeric run as a single Identifier token. The parser
/// handles keyword recognition from context — this matches how gfortran,
/// flang, and LFortran handle fixed-form. Keyword prefix splitting (e.g.,
/// splitting INTEGERI into INTEGER+I) is a parser concern because the lexer
/// can't distinguish INDEX (intrinsic) from INTEGERI (keyword+ident) without
/// statement context.
///
/// The one exception is the DO/assignment ambiguity, which genuinely must be
/// resolved at lex time: DO10I=1,10 (loop) vs DO10I=1.10 (assignment) changes
/// whether digits after DO are a label or part of a variable name.
fn lex_fixed_ident_or_keyword(text: &str, pos: usize, file_id: u32, line: u32) -> (Token, usize) {
    let bytes = text.as_bytes();
    let mut run_end = pos;
    while run_end < bytes.len() && (bytes[run_end].is_ascii_alphanumeric() || bytes[run_end] == b'_') {
        run_end += 1;
    }
    let run = &text[pos..run_end];
    let run_lower = run.to_lowercase();

    // DO/assignment ambiguity: if the run starts with "do" followed by digits,
    // check if this is a DO loop (has comma after =) or an assignment.
    if run_lower.starts_with("do") && run.len() > 2 && run.as_bytes()[2].is_ascii_digit() {
        if is_do_loop_context(text, pos + 2) {
            // IS a DO loop — emit just "DO" (2 chars). Subsequent calls
            // will pick up the label (digits) and variable (letters) separately.
            let col = (pos as u32) + 7;
            return (Token {
                kind: TokenKind::Identifier,
                text: run[..2].to_string(),
                span: Span {
                    file_id,
                    start: Position { line, col },
                    end: Position { line, col: col + 2 },
                },
            }, 2);
        }
        // Not a DO loop — fall through to emit entire run as identifier.
    }

    // Emit the entire alphanumeric run as one identifier.
    make_ident_token(run, pos, file_id, line)
}

fn make_ident_token(text: &str, pos: usize, file_id: u32, line: u32) -> (Token, usize) {
    let col = (pos as u32) + 7;
    (Token {
        kind: TokenKind::Identifier,
        text: text.to_string(),
        span: Span {
            file_id,
            start: Position { line, col },
            end: Position { line, col: col + text.len() as u32 },
        },
    }, text.len())
}

/// Check if the rest of the statement after DO+digits looks like a DO loop.
/// A DO loop has: DO [label] variable = start , end [, step]
/// An assignment has: DO[label][var] = expr (no top-level comma after =).
fn is_do_loop_context(text: &str, after_do: usize) -> bool {
    let bytes = text.as_bytes();

    // Look for '=' anywhere after this position.
    let eq_pos = bytes[after_do..].iter().position(|&b| b == b'=');
    let eq_pos = match eq_pos {
        Some(p) => after_do + p,
        None => return false,
    };

    // Make sure '=' is not '==' (comparison).
    if eq_pos + 1 < bytes.len() && bytes[eq_pos + 1] == b'=' {
        return false;
    }

    // Check for a top-level comma after the '='.
    let mut depth = 0i32;
    for &b in &bytes[(eq_pos + 1)..] {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

/// Lex a BOZ literal in fixed-form body.
fn lex_fixed_boz(text: &str, pos: usize, file_id: u32, line: u32) -> Result<(Token, usize), LexError> {
    let bytes = text.as_bytes();
    let mut end = pos;
    let mut tok_text = String::new();

    tok_text.push(bytes[end] as char); // B/O/Z
    end += 1;
    let quote = bytes[end];
    tok_text.push(quote as char); // opening quote
    end += 1;

    while end < bytes.len() && bytes[end] != quote {
        tok_text.push(bytes[end] as char);
        end += 1;
    }
    if end >= bytes.len() {
        return Err(LexError {
            span: Span { file_id, start: Position { line, col: (pos as u32) + 7 }, end: Position { line, col: (pos as u32) + 7 } },
            msg: "unterminated BOZ literal".into(),
        });
    }
    tok_text.push(bytes[end] as char); // closing quote
    end += 1;

    let col = (pos as u32) + 7;
    Ok((Token {
        kind: TokenKind::BozLiteral,
        text: tok_text,
        span: Span { file_id, start: Position { line, col }, end: Position { line, col: col + (end - pos) as u32 } },
    }, end - pos))
}

/// Lex an operator or punctuation in whitespace-stripped body.
fn lex_fixed_punct(text: &str, pos: usize, file_id: u32, line: u32) -> Result<(Token, usize), LexError> {
    let bytes = text.as_bytes();
    let ch = bytes[pos];
    let next = if pos + 1 < bytes.len() { bytes[pos + 1] } else { 0 };
    let col = (pos as u32) + 7;
    let start = Position { line, col };

    let (kind, tok_text, consumed) = match ch {
        b'+' => (TokenKind::Plus, "+", 1),
        b'-' => (TokenKind::Minus, "-", 1),
        b'*' if next == b'*' => (TokenKind::Power, "**", 2),
        b'*' => (TokenKind::Star, "*", 1),
        b'/' if next == b'/' => (TokenKind::Concat, "//", 2),
        b'/' if next == b'=' => (TokenKind::Ne, "/=", 2),
        b'/' => (TokenKind::Slash, "/", 1),
        b'=' if next == b'=' => (TokenKind::Eq, "==", 2),
        b'=' if next == b'>' => (TokenKind::Arrow, "=>", 2),
        b'=' => (TokenKind::Assign, "=", 1),
        b'<' if next == b'=' => (TokenKind::Le, "<=", 2),
        b'<' => (TokenKind::Lt, "<", 1),
        b'>' if next == b'=' => (TokenKind::Ge, ">=", 2),
        b'>' => (TokenKind::Gt, ">", 1),
        b'(' => (TokenKind::LParen, "(", 1),
        b')' => (TokenKind::RParen, ")", 1),
        b'[' => (TokenKind::LBracket, "[", 1),
        b']' => (TokenKind::RBracket, "]", 1),
        b',' => (TokenKind::Comma, ",", 1),
        b':' if next == b':' => (TokenKind::ColonColon, "::", 2),
        b':' => (TokenKind::Colon, ":", 1),
        b';' => (TokenKind::Semicolon, ";", 1),
        b'%' => (TokenKind::Percent, "%", 1),
        b'&' => (TokenKind::Ampersand, "&", 1),
        _ => {
            return Err(LexError {
                span: Span { file_id, start, end: start },
                msg: format!("unexpected character in fixed-form body: '{}'", ch as char),
            });
        }
    };

    Ok((Token {
        kind,
        text: tok_text.into(),
        span: Span { file_id, start, end: Position { line, col: col + consumed as u32 } },
    }, consumed))
}

// ---- Line preprocessing ----

enum FixedLine {
    Comment { text: String, span: Span },
    Statement { label: Option<String>, body: String, start_line: u32, file_id: u32 },
    Blank { span: Span },
}

/// Preprocess fixed-form lines: identify comments, extract labels, join
/// continuations, strip columns 73+, handle tab-form.
fn preprocess_lines(src: &str, file_id: u32) -> Vec<FixedLine> {
    let lines: Vec<&str> = src.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let line_num = (i + 1) as u32;

        // Blank line.
        if line.trim().is_empty() {
            result.push(FixedLine::Blank {
                span: Span {
                    file_id,
                    start: Position { line: line_num, col: 1 },
                    end: Position { line: line_num, col: 1 },
                },
            });
            i += 1;
            continue;
        }

        let first_byte = line.as_bytes().first().copied().unwrap_or(0);

        // Comment line: C, c, *, or ! in column 1.
        if matches!(first_byte, b'C' | b'c' | b'*' | b'!') {
            result.push(FixedLine::Comment {
                text: line.to_string(),
                span: Span {
                    file_id,
                    start: Position { line: line_num, col: 1 },
                    end: Position { line: line_num, col: line.len() as u32 },
                },
            });
            i += 1;
            continue;
        }

        // Extract columns from this line.
        let (label, body) = extract_fixed_columns(line);

        // Collect continuation lines.
        let start_line = line_num;
        let mut full_body = body;
        i += 1;

        while i < lines.len() {
            let next = lines[i];

            // Blank lines between continuations: skip them (F77 allows this).
            if next.trim().is_empty() {
                i += 1;
                continue;
            }

            let next_first = next.as_bytes().first().copied().unwrap_or(0);
            // Comment lines between continuations: skip them.
            if matches!(next_first, b'C' | b'c' | b'*' | b'!') {
                // Emit the comment but don't break the continuation.
                result.push(FixedLine::Comment {
                    text: next.to_string(),
                    span: Span {
                        file_id,
                        start: Position { line: (i + 1) as u32, col: 1 },
                        end: Position { line: (i + 1) as u32, col: next.len() as u32 },
                    },
                });
                i += 1;
                continue;
            }

            // Check column 6 for continuation marker.
            if is_continuation_line(next) {
                let (_, cont_body) = extract_fixed_columns(next);
                full_body.push_str(&cont_body);
                i += 1;
            } else {
                break;
            }
        }

        result.push(FixedLine::Statement {
            label: if label.trim().is_empty() { None } else { Some(label) },
            body: full_body,
            start_line,
            file_id,
        });
    }

    result
}

/// Check if a line is a continuation line (non-space, non-zero in column 6).
fn is_continuation_line(line: &str) -> bool {
    let bytes = line.as_bytes();

    // Tab-form: tab followed by digit 1-9 is continuation.
    if bytes.first() == Some(&b'\t') {
        if let Some(&d) = bytes.get(1) {
            return (b'1'..=b'9').contains(&d);
        }
    }

    // Standard: column 6 (0-indexed: byte 5) is non-space, non-zero.
    if bytes.len() >= 6 {
        let col6 = bytes[5];
        return col6 != b' ' && col6 != b'0' && col6 != b'\t';
    }

    false
}

/// Extract label (columns 1-5) and body (columns 7-72) from a fixed-form line.
/// Handles tab-form extension.
fn extract_fixed_columns(line: &str) -> (String, String) {
    let bytes = line.as_bytes();

    // Tab-form: if first character is a tab, everything after is body (or continuation).
    if bytes.first() == Some(&b'\t') {
        // Tab followed by digit 1-9: continuation (body starts after the digit).
        if let Some(&d) = bytes.get(1) {
            if (b'1'..=b'9').contains(&d) {
                let body = if bytes.len() > 2 {
                    String::from_utf8_lossy(&bytes[2..]).to_string()
                } else {
                    String::new()
                };
                return (String::new(), body);
            }
        }
        // Tab followed by anything else: body starts at position after tab.
        let body = if bytes.len() > 1 {
            String::from_utf8_lossy(&bytes[1..]).to_string()
        } else {
            String::new()
        };
        return (String::new(), body);
    }

    // Standard fixed-form: columns 1-5 label, column 6 continuation marker, 7-72 body.
    let label = if bytes.len() >= 5 {
        String::from_utf8_lossy(&bytes[0..5]).to_string()
    } else {
        String::from_utf8_lossy(bytes).to_string()
    };

    let body_start = 6.min(bytes.len());
    let body_end = 72.min(bytes.len()); // columns 73+ are ignored
    let body = if body_start < bytes.len() {
        String::from_utf8_lossy(&bytes[body_start..body_end]).to_string()
    } else {
        String::new()
    };

    (label, body)
}

// ---- Hollerith constants ----

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::TokenKind;

    fn fixed_toks(src: &str) -> Vec<Token> {
        tokenize_fixed(src, 0).unwrap()
    }

    fn fixed_kinds(src: &str) -> Vec<TokenKind> {
        fixed_toks(src).into_iter()
            .map(|t| t.kind)
            .filter(|k| !matches!(k, TokenKind::Eof | TokenKind::Newline))
            .collect()
    }

    fn fixed_texts(src: &str) -> Vec<String> {
        fixed_toks(src).into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::Eof | TokenKind::Newline | TokenKind::Comment))
            .map(|t| t.text)
            .collect()
    }

    // ---- Comment detection ----

    #[test]
    fn comment_c_uppercase() {
        let k = fixed_kinds("C     This is a comment\n");
        assert_eq!(k, vec![TokenKind::Comment]);
    }

    #[test]
    fn comment_c_lowercase() {
        let k = fixed_kinds("c     This is a comment\n");
        assert_eq!(k, vec![TokenKind::Comment]);
    }

    #[test]
    fn comment_star() {
        let k = fixed_kinds("*     This is a comment\n");
        assert_eq!(k, vec![TokenKind::Comment]);
    }

    #[test]
    fn comment_bang() {
        let k = fixed_kinds("!     This is a comment\n");
        assert_eq!(k, vec![TokenKind::Comment]);
    }

    // ---- Statement labels ----

    #[test]
    fn statement_with_label() {
        // "   10 CONTINUE" — label 10 in columns 1-5, CONTINUE in 7+
        let texts = fixed_texts("   10 CONTINUE\n");
        assert!(texts.contains(&"10".to_string()), "got: {:?}", texts);
        assert!(texts.contains(&"CONTINUE".to_string()), "got: {:?}", texts);
    }

    #[test]
    fn statement_without_label() {
        // No label means the first token should be the identifier X, not a label number.
        let toks = fixed_toks("      X = 42\n");
        let first_meaningful = toks.iter()
            .find(|t| !matches!(t.kind, TokenKind::Newline | TokenKind::Eof | TokenKind::Comment))
            .unwrap();
        assert_eq!(first_meaningful.kind, TokenKind::Identifier);
        assert_eq!(first_meaningful.text, "X");
    }

    // ---- Column 73+ ignored ----

    #[test]
    fn columns_past_72_ignored() {
        // Columns 73+ should be stripped. Place code in 7-72, junk in 73+.
        let line = format!("      X = 42{}\n", " ".repeat(60) + "JUNK");
        // Body should be "X = 42" + spaces, NOT including JUNK.
        let texts = fixed_texts(&line);
        assert!(texts.contains(&"X".to_string()));
        assert!(!texts.iter().any(|t| t.contains("JUNK")), "got: {:?}", texts);
    }

    // ---- Continuation lines ----

    #[test]
    fn continuation_in_column_6() {
        let src = "      X = 1 +\n     +  2\n";
        let kinds = fixed_kinds(src);
        assert!(kinds.contains(&TokenKind::Plus));
        // Should have both integer literals.
        let int_count = kinds.iter().filter(|k| **k == TokenKind::IntegerLiteral).count();
        assert_eq!(int_count, 2, "expected 2 integer literals, got {:?}", kinds);
    }

    #[test]
    fn continuation_dollar_sign() {
        // Any non-space, non-zero character in column 6 is continuation.
        let src = "      X = 1 +\n     $  2\n";
        let kinds = fixed_kinds(src);
        let int_count = kinds.iter().filter(|k| **k == TokenKind::IntegerLiteral).count();
        assert_eq!(int_count, 2);
    }

    // ---- Tab-form extension ----

    #[test]
    fn tab_form_statement() {
        let src = "\tX = 42\n";
        let texts = fixed_texts(src);
        assert!(texts.contains(&"X".to_string()));
        assert!(texts.contains(&"42".to_string()));
    }

    #[test]
    fn tab_form_continuation() {
        // Tab followed by digit 1-9 is continuation.
        let src = "\tX = 1 +\n\t1  2\n";
        let kinds = fixed_kinds(src);
        let int_count = kinds.iter().filter(|k| **k == TokenKind::IntegerLiteral).count();
        assert_eq!(int_count, 2, "got: {:?}", kinds);
    }

    // ---- Simple programs ----

    #[test]
    fn simple_fixed_form_program() {
        let src = "\
C     Hello World
      PROGRAM HELLO
      INTEGER I
      DO 10 I = 1, 10
         WRITE(*,*) I
   10 CONTINUE
      STOP
      END
";
        let tokens = tokenize_fixed(src, 0).unwrap();
        let ident_count = tokens.iter().filter(|t| t.kind == TokenKind::Identifier).count();
        assert!(ident_count >= 8, "expected 8+ identifiers, got {}", ident_count);

        // Should have a label "10".
        assert!(tokens.iter().any(|t| t.kind == TokenKind::IntegerLiteral && t.text == "10"));
    }

    // ---- Mode detection ----

    #[test]
    fn detect_free_form() {
        use super::super::detect_source_form;
        assert_eq!(detect_source_form("test.f90"), super::super::SourceForm::FreeForm);
        assert_eq!(detect_source_form("test.f95"), super::super::SourceForm::FreeForm);
        assert_eq!(detect_source_form("test.f03"), super::super::SourceForm::FreeForm);
        assert_eq!(detect_source_form("test.f08"), super::super::SourceForm::FreeForm);
        assert_eq!(detect_source_form("test.f18"), super::super::SourceForm::FreeForm);
    }

    #[test]
    fn detect_fixed_form() {
        use super::super::detect_source_form;
        assert_eq!(detect_source_form("test.f"), super::super::SourceForm::FixedForm);
        assert_eq!(detect_source_form("test.for"), super::super::SourceForm::FixedForm);
        assert_eq!(detect_source_form("test.ftn"), super::super::SourceForm::FixedForm);
    }

    // ---- Unified token stream ----

    #[test]
    fn fixed_and_free_produce_same_tokens() {
        let free_src = "integer :: x\nx = 42\n";
        let fixed_src = "      integer :: x\n      x = 42\n";

        let free_kinds: Vec<_> = super::super::Lexer::tokenize(free_src, 0).unwrap()
            .into_iter().map(|t| t.kind)
            .filter(|k| !matches!(k, TokenKind::Eof | TokenKind::Newline))
            .collect();

        let fixed_kinds = fixed_kinds(fixed_src);

        assert_eq!(free_kinds, fixed_kinds,
            "free-form and fixed-form produced different tokens:\n  free:  {:?}\n  fixed: {:?}",
            free_kinds, fixed_kinds);
    }

    // ---- Blank lines ----

    #[test]
    fn blank_lines_handled() {
        let src = "      X = 1\n\n      Y = 2\n";
        let kinds = fixed_kinds(src);
        assert!(kinds.iter().filter(|k| **k == TokenKind::Identifier).count() >= 2);
    }

    // ---- Hollerith ----

    #[test]
    fn hollerith_protect_converts_to_string() {
        assert_eq!(protect_hollerith("3HABC"), "'ABC'");
        assert_eq!(protect_hollerith("6HFOOBAR"), "'FOOBAR'");
    }

    #[test]
    fn hollerith_with_spaces_preserved() {
        // 6H HELLO has a leading space — must be preserved.
        assert_eq!(protect_hollerith("6H HELLO"), "' HELLO'");
    }

    #[test]
    fn hollerith_not_after_letter() {
        // X3HABC — the 3H is preceded by a letter, so it's NOT a Hollerith.
        assert_eq!(protect_hollerith("X3HABC"), "X3HABC");
    }

    #[test]
    fn hollerith_after_operator() {
        // =3HABC — preceded by =, not a letter, so IS a Hollerith.
        assert_eq!(protect_hollerith("=3HABC"), "='ABC'");
    }

    // ---- Real fixed-form files from refs ----

    #[test]
    fn tokenize_flang_fixed_form_test() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.refs/llvm/flang/test/Driver/Inputs/fixed-form-test.f"
        );
        if !std::path::Path::new(path).exists() { return; }
        let src = std::fs::read_to_string(path).unwrap();
        let tokens = tokenize_fixed(&src, 0);
        assert!(tokens.is_ok(), "failed: {:?}", tokens.err());
    }

    #[test]
    fn tokenize_gcc_nested_forall() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.refs/gcc/gcc/testsuite/gfortran.dg/nested_forall_1.f"
        );
        if !std::path::Path::new(path).exists() { return; }
        let src = std::fs::read_to_string(path).unwrap();
        let tokens = tokenize_fixed(&src, 0);
        assert!(tokens.is_ok(), "failed: {:?}", tokens.err());
        let toks = tokens.unwrap();
        assert!(toks.len() > 50, "expected 50+ tokens, got {}", toks.len());
    }

    // ======================================================================
    // Whitespace insensitivity tests — the core challenge of fixed-form
    // ======================================================================

    #[test]
    fn whitespace_stripped_goto() {
        // GOTO100 → single identifier (parser handles keyword+number split).
        // This matches gfortran behavior: the lexer doesn't split keywords in
        // fixed-form. The parser recognizes GOTO from statement context.
        let texts = fixed_texts("      GOTO100\n");
        assert_eq!(texts, vec!["GOTO100"], "got: {:?}", texts);
    }

    #[test]
    fn whitespace_stripped_integer_decl() {
        // INTEGERI → single identifier (parser splits INTEGER+I from context).
        let texts = fixed_texts("      INTEGERI\n");
        assert_eq!(texts, vec!["INTEGERI"], "got: {:?}", texts);
    }

    #[test]
    fn whitespace_stripped_doubleprecision() {
        // DOUBLEPRECISIONX → single identifier (parser handles).
        let texts = fixed_texts("      DOUBLEPRECISIONX\n");
        assert_eq!(texts, vec!["DOUBLEPRECISIONX"], "got: {:?}", texts);
    }

    #[test]
    fn index_not_broken() {
        // INDEX must NOT be split into IN+DEX — this was the showstopper bug.
        let kinds = fixed_kinds("      X=INDEX(A,'B')\n");
        let texts = fixed_texts("      X=INDEX(A,'B')\n");
        assert!(texts.contains(&"INDEX".to_string()), "INDEX was incorrectly split, got: {:?}", texts);
    }

    #[test]
    fn include_not_broken() {
        // INCLUDE must not become IN+CLUDE.
        let texts = fixed_texts("      INCLUDEVAR=1\n");
        assert_eq!(texts[0], "INCLUDEVAR", "got: {:?}", texts);
    }

    #[test]
    fn if_ident_not_broken() {
        // IFLAG must not become IF+LAG.
        let texts = fixed_texts("      IFLAG=1\n");
        assert_eq!(texts[0], "IFLAG", "got: {:?}", texts);
    }

    #[test]
    fn whitespace_stripped_assignment() {
        // X=42 → identifier, =, integer
        let kinds = fixed_kinds("      X=42\n");
        assert_eq!(kinds, vec![
            TokenKind::Identifier, TokenKind::Assign, TokenKind::IntegerLiteral,
        ]);
    }

    #[test]
    fn whitespace_stripped_expression() {
        // A+B*C → identifier, +, identifier, *, identifier
        let kinds = fixed_kinds("      A+B*C\n");
        assert_eq!(kinds, vec![
            TokenKind::Identifier, TokenKind::Plus,
            TokenKind::Identifier, TokenKind::Star,
            TokenKind::Identifier,
        ]);
    }

    #[test]
    fn whitespace_stripped_with_parens() {
        // X=REAL(I) → identifier, =, identifier, (, identifier, )
        let kinds = fixed_kinds("      X=REAL(I)\n");
        assert_eq!(kinds, vec![
            TokenKind::Identifier, TokenKind::Assign,
            TokenKind::Identifier, TokenKind::LParen,
            TokenKind::Identifier, TokenKind::RParen,
        ]);
    }

    #[test]
    fn whitespace_stripped_dot_op() {
        // A.AND.B → identifier, .and., identifier
        let kinds = fixed_kinds("      A.AND.B\n");
        assert_eq!(kinds, vec![
            TokenKind::Identifier,
            TokenKind::DotOp("and".into()),
            TokenKind::Identifier,
        ]);
    }

    #[test]
    fn whitespace_stripped_real_literal() {
        // X=1.0D0 → identifier, =, real
        let kinds = fixed_kinds("      X=1.0D0\n");
        assert_eq!(kinds, vec![
            TokenKind::Identifier, TokenKind::Assign, TokenKind::RealLiteral,
        ]);
    }

    #[test]
    fn whitespace_stripped_comparison() {
        // 1.EQ.2 → integer, .eq., integer
        let kinds = fixed_kinds("      IF(I.EQ.1)STOP\n");
        assert!(kinds.contains(&TokenKind::DotOp("eq".into())), "got: {:?}", kinds);
    }

    #[test]
    fn whitespace_stripped_string_preserved() {
        // Whitespace INSIDE strings must be preserved.
        let kinds = fixed_kinds("      X='HELLO WORLD'\n");
        assert!(kinds.contains(&TokenKind::StringLiteral));
        let texts = fixed_texts("      X='HELLO WORLD'\n");
        assert!(texts.iter().any(|t| t.contains("HELLO WORLD")), "got: {:?}", texts);
    }

    // ---- Continuation over blank lines ----

    #[test]
    fn continuation_over_blank_line() {
        let src = "      X = 1 +\n\n     +  2\n";
        let kinds = fixed_kinds(src);
        let int_count = kinds.iter().filter(|k| **k == TokenKind::IntegerLiteral).count();
        assert_eq!(int_count, 2, "blank line should not break continuation, got: {:?}", kinds);
    }

    // ---- DO/assignment ambiguity ----

    #[test]
    fn do_loop_with_comma() {
        // DO10I=1,10 → DO loop: DO + 10 + I + = + 1 + , + 10
        let kinds = fixed_kinds("      DO10I=1,10\n");
        assert!(kinds.contains(&TokenKind::Comma),
            "DO loop must have comma, got: {:?}", kinds);
        let texts = fixed_texts("      DO10I=1,10\n");
        assert_eq!(texts[0], "DO", "first token should be DO keyword, got: {:?}", texts);
    }

    #[test]
    fn do_assignment_no_comma() {
        // DO10I=1.10 → assignment: DO10I + = + 1.10 (no comma → not a loop)
        let kinds = fixed_kinds("      DO10I=1.10\n");
        assert!(!kinds.contains(&TokenKind::Comma),
            "assignment should have no comma, got: {:?}", kinds);
        let texts = fixed_texts("      DO10I=1.10\n");
        assert_eq!(texts[0], "DO10I", "should be single identifier, got: {:?}", texts);
    }

    #[test]
    fn do_assignment_no_comma_integer() {
        // DO10I=1 → assignment (no comma)
        let kinds = fixed_kinds("      DO10I=1\n");
        assert!(!kinds.contains(&TokenKind::Comma));
        let texts = fixed_texts("      DO10I=1\n");
        assert_eq!(texts[0], "DO10I");
    }

    // ---- BOZ in fixed-form ----

    #[test]
    fn boz_in_fixed_form() {
        let kinds = fixed_kinds("      X=B'1010'\n");
        assert!(kinds.contains(&TokenKind::BozLiteral), "got: {:?}", kinds);
    }

    #[test]
    fn boz_hex_in_fixed_form() {
        let kinds = fixed_kinds("      X=Z'FF'\n");
        assert!(kinds.contains(&TokenKind::BozLiteral), "got: {:?}", kinds);
    }

    // ---- Hollerith integration ----

    #[test]
    fn hollerith_in_source() {
        // 3HABC in a statement should produce a string literal.
        let kinds = fixed_kinds("      X=3HABC\n");
        assert!(kinds.contains(&TokenKind::StringLiteral), "got: {:?}", kinds);
        let texts = fixed_texts("      X=3HABC\n");
        assert!(texts.iter().any(|t| t.contains("ABC")), "Hollerith content missing, got: {:?}", texts);
    }

    #[test]
    fn hollerith_with_spaces_in_source() {
        // 6H HELLO preserves the space.
        let texts = fixed_texts("      X=6H HELLO\n");
        assert!(texts.iter().any(|t| t.contains(" HELLO")), "space lost, got: {:?}", texts);
    }

    // ---- String in fixed-form ----

    #[test]
    fn string_literal_in_fixed_form() {
        let kinds = fixed_kinds("      X = 'IT''S'\n");
        assert!(kinds.contains(&TokenKind::StringLiteral));
        let texts = fixed_texts("      X = 'IT''S'\n");
        assert!(texts.iter().any(|t| t.contains("IT''S")), "got: {:?}", texts);
    }

}
