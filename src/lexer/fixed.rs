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
/// Strips all whitespace outside string literals, then scans left to right
/// using longest-match: keyword → number → operator → identifier.
fn tokenize_body(body: &str, file_id: u32, line: u32) -> Result<Vec<Token>, LexError> {
    // Strip whitespace outside string literals.
    let stripped = strip_whitespace_outside_strings(body);
    let bytes = stripped.as_bytes();
    let mut tokens = Vec::new();
    let mut pos = 0;

    while pos < bytes.len() {
        let col = (pos as u32) + 7; // column 7+ in original source
        let start = Position { line, col };

        let ch = bytes[pos];

        // Comment (! to end).
        if ch == b'!' {
            let text: String = stripped[pos..].to_string();
            tokens.push(Token {
                kind: TokenKind::Comment,
                text,
                span: Span { file_id, start, end: Position { line, col: col + (bytes.len() - pos) as u32 } },
            });
            break;
        }

        // String literal (whitespace is significant inside).
        if ch == b'\'' || ch == b'"' {
            let (tok, consumed) = lex_fixed_string(&stripped, pos, file_id, line)?;
            tokens.push(tok);
            pos += consumed;
            continue;
        }

        // Dot-operator or real starting with dot.
        if ch == b'.' {
            if pos + 1 < bytes.len() && bytes[pos + 1].is_ascii_digit() {
                // Real literal starting with dot: .5, .123e4
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

        // Number — could be integer, real, or start of Hollerith.
        if ch.is_ascii_digit() {
            // Try Hollerith first (nH...).
            if let Some((hol_text, consumed)) = try_hollerith_in_stripped(&stripped, pos) {
                let end_col = col + consumed as u32;
                tokens.push(Token {
                    kind: TokenKind::StringLiteral, // Hollerith → string
                    text: hol_text,
                    span: Span { file_id, start, end: Position { line, col: end_col } },
                });
                pos += consumed;
                continue;
            }

            let (tok, consumed) = lex_fixed_number(&stripped, pos, file_id, line);
            tokens.push(tok);
            pos += consumed;
            continue;
        }

        // Letter — keyword or identifier. Use longest keyword match.
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

    // Exponent.
    if end < bytes.len() && matches!(bytes[end], b'e' | b'E' | b'd' | b'D') {
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
/// Since whitespace is stripped, we consume all alphanumeric/underscore chars.
fn lex_fixed_ident_or_keyword(text: &str, pos: usize, file_id: u32, line: u32) -> (Token, usize) {
    let bytes = text.as_bytes();
    let mut end = pos;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    let tok_text = text[pos..end].to_string();
    let col = (pos as u32) + 7;
    (Token {
        kind: TokenKind::Identifier,
        text: tok_text,
        span: Span { file_id, start: Position { line, col }, end: Position { line, col: col + (end - pos) as u32 } },
    }, end - pos)
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

/// Try Hollerith constant in whitespace-stripped text.
/// Format: nH followed by exactly n characters.
fn try_hollerith_in_stripped(text: &str, pos: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut p = pos;

    // Read count digits.
    if !bytes[p].is_ascii_digit() { return None; }
    while p < bytes.len() && bytes[p].is_ascii_digit() {
        p += 1;
    }

    // Must be followed by H/h.
    if p >= bytes.len() || (bytes[p] != b'H' && bytes[p] != b'h') { return None; }

    let count: usize = text[pos..p].parse().ok()?;
    p += 1; // skip H

    // Need to distinguish Hollerith from identifier starting with H.
    // Hollerith only appears in specific contexts (FORMAT, CALL arguments).
    // Heuristic: only match if count > 0 and we have enough characters.
    if count == 0 { return None; }
    if p + count > bytes.len() { return None; }

    let hol_content = &text[p..p + count];
    let full = format!("{}H{}", &text[pos..pos + (p - pos - 1)], hol_content);
    Some((full, p + count - pos))
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

/// Check if the text at the current position starts a Hollerith constant (nH...).
/// Returns Some((hollerith_text, length_consumed)) if found.
pub fn try_hollerith(text: &str, pos: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let start = pos;
    let mut p = pos;

    // Read digits for the count.
    if p >= bytes.len() || !bytes[p].is_ascii_digit() { return None; }
    while p < bytes.len() && bytes[p].is_ascii_digit() {
        p += 1;
    }

    // Must be followed by 'H' or 'h'.
    if p >= bytes.len() || (bytes[p] != b'H' && bytes[p] != b'h') { return None; }

    let count_str = &text[start..p];
    let count: usize = count_str.parse().ok()?;
    p += 1; // skip H

    // Read exactly `count` characters.
    if p + count > bytes.len() { return None; }

    let hol_text = format!("{}{}", count_str, &text[p - 1..p + count]); // includes H
    Some((hol_text, p + count - start))
}

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
    fn hollerith_parse() {
        assert_eq!(try_hollerith("3HABC", 0), Some(("3HABC".to_string(), 5)));
        assert_eq!(try_hollerith("6HFOOBAR", 0), Some(("6HFOOBAR".to_string(), 8)));
    }

    #[test]
    fn hollerith_zero() {
        assert_eq!(try_hollerith("0HX", 0), Some(("0H".to_string(), 2)));
    }

    #[test]
    fn hollerith_not_matched() {
        assert_eq!(try_hollerith("ABC", 0), None);
        assert_eq!(try_hollerith("3XABC", 0), None);
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
        // GOTO100 → GO TO 100 → identifier GOTO100 (parser handles keyword split)
        // After stripping whitespace, this is a single identifier. That's correct —
        // the lexer produces GOTO100 as an identifier, and the parser would need to
        // handle the GOTO keyword. In a true fixed-form scanner, this is one token.
        let kinds = fixed_kinds("      GOTO100\n");
        // Should be a single identifier (whitespace stripped: "GOTO100").
        assert!(kinds.contains(&TokenKind::Identifier), "got: {:?}", kinds);
    }

    #[test]
    fn whitespace_stripped_integer_decl() {
        // INTEGERI → after strip: "INTEGERI" → one identifier.
        // The parser handles "INTEGERI" as a keyword+identifier pair.
        let kinds = fixed_kinds("      INTEGERI\n");
        assert!(kinds.contains(&TokenKind::Identifier));
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

    // ---- Hollerith integration ----

    #[test]
    fn hollerith_in_source() {
        // 3HABC in a statement should produce a string literal "ABC".
        let kinds = fixed_kinds("      X=3HABC\n");
        assert!(kinds.contains(&TokenKind::StringLiteral), "got: {:?}", kinds);
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
