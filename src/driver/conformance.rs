//! Source-form conformance warnings (sprint l01).
//!
//! F2023 raised the free-form line limit to 10,000 characters and
//! replaced the continuation-count limit with a one-million-character
//! statement limit (23-007r1 6.3.2). armfortas accepts arbitrarily
//! long lines and statements at every `--std` level and always will —
//! these are warnings, never errors, and acceptance never changes
//! (flang behaves the same; gfortran truncates, which we deliberately
//! do not).
//!
//! Mechanism: one standalone scan over the ORIGINAL source text,
//! invoked by the driver before preprocessing. The sprint doc offered
//! a choice between threading `--std` into the lexer or reporting via
//! the driver; this is the driver route, picked because (a)
//! `FortranStandard` lives in sema and the lexer must not depend on
//! sema, and (b) both line joiners (the lexer's `try_continuation`
//! and the preprocessor's logical-line join) run AFTER this scan, so
//! the diagnostic fires identically no matter which path a file takes.
//!
//! Statement length counts the characters of every physical line of a
//! continued statement (the standard's limit is on the statement as
//! written, 6.3.2.6); continuation detection follows the free-form
//! rule — last significant character of the line is `&` — with
//! string-literal state carried across lines so a `!` inside a
//! continued character context does not read as a comment.

use crate::lexer::{Position, SourceForm, Span};
use crate::sema::validate::FortranStandard;

pub struct LimitWarning {
    pub span: Span,
    pub msg: String,
}

/// The standard's free-form line limit for `std`.
fn line_limit(std: FortranStandard) -> usize {
    if std >= FortranStandard::F2023 {
        10_000
    } else {
        132
    }
}

const STMT_LIMIT: usize = 1_000_000;

/// Scan `source` for F2023 source-limit conformance violations.
/// `suppress_line_limit` is `-ffree-line-length-none`: gfortran's
/// meaning of that flag is "no line-length conformance concern", so it
/// silences the line warning (the statement limit still applies).
/// Fixed form is untouched — F2023's new limits are free-form.
pub fn check_source_limits(
    source: &str,
    std: FortranStandard,
    form: SourceForm,
    suppress_line_limit: bool,
) -> Vec<LimitWarning> {
    if form != SourceForm::FreeForm {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    let limit = line_limit(std);

    // Statement tracking: in_string carries across continued lines;
    // stmt_start/stmt_chars accumulate until a non-continued line ends
    // the statement.
    let mut in_string: Option<char> = None;
    let mut stmt_start: u32 = 0;
    let mut stmt_chars: usize = 0;

    for (idx, line) in source.lines().enumerate() {
        let line_no = idx as u32 + 1;
        let len = line.chars().count();

        if !suppress_line_limit && len > limit {
            let at = Span {
                file_id: 0,
                start: Position {
                    line: line_no,
                    col: 1,
                },
                end: Position {
                    line: line_no,
                    col: 1,
                },
            };
            warnings.push(LimitWarning {
                span: at,
                msg: if limit == 132 {
                    format!(
                        "line is {} characters long; {:?} limits free-form lines to 132 \
                         (F2023 raises the limit to 10,000 — this is a conformance \
                         warning, the line is compiled in full)",
                        len, std
                    )
                } else {
                    format!(
                        "line is {} characters long, over the F2023 free-form limit of \
                         10,000 (conformance warning; the line is compiled in full)",
                        len
                    )
                },
            });
        }

        // ---- statement accounting ----
        if stmt_chars == 0 {
            stmt_start = line_no;
        }
        stmt_chars += len;

        let continued = line_continues(line, &mut in_string);
        if !continued {
            if std >= FortranStandard::F2023 && stmt_chars > STMT_LIMIT {
                let at = Span {
                    file_id: 0,
                    start: Position {
                        line: stmt_start,
                        col: 1,
                    },
                    end: Position {
                        line: stmt_start,
                        col: 1,
                    },
                };
                warnings.push(LimitWarning {
                    span: at,
                    msg: format!(
                        "statement is {} characters long, over the F2023 limit of \
                         1,000,000 (conformance warning; the statement is compiled \
                         in full)",
                        stmt_chars
                    ),
                });
            }
            stmt_chars = 0;
            in_string = None;
        }
    }
    warnings
}

/// Free-form continuation test: the line's last significant character
/// is `&`. Significant means outside a trailing comment; characters
/// inside string literals count (a trailing `&` inside a character
/// context is a string continuation and continues the statement).
/// `in_string` carries quote state across lines of one statement.
fn line_continues(line: &str, in_string: &mut Option<char>) -> bool {
    let mut last_significant: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match *in_string {
            Some(q) => {
                if c == q {
                    if chars.peek() == Some(&q) {
                        chars.next(); // doubled quote, still in string
                    } else {
                        *in_string = None;
                    }
                }
                if !c.is_whitespace() {
                    last_significant = Some(c);
                }
            }
            None => {
                if c == '!' {
                    break; // trailing comment: nothing after is significant
                }
                if c == '\'' || c == '"' {
                    *in_string = Some(c);
                }
                if !c.is_whitespace() {
                    last_significant = Some(c);
                }
            }
        }
    }
    last_significant == Some('&')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(src: &str, std: FortranStandard) -> Vec<LimitWarning> {
        check_source_limits(src, std, SourceForm::FreeForm, false)
    }

    #[test]
    fn long_line_warns_per_std_level() {
        let src = format!("program p\nprint *, {}\nend program\n", "1".repeat(200));
        let w = check(&src, FortranStandard::F2018);
        assert_eq!(w.len(), 1);
        assert!(w[0].msg.contains("132"));
        assert_eq!(w[0].span.start.line, 2);
        assert!(check(&src, FortranStandard::F2023).is_empty());
    }

    #[test]
    fn f2023_line_limit_is_ten_thousand() {
        // 9,999 chars: clean under f2023 (the line_length_13.f90 bound).
        let src = format!("x = 0{}\n", " ".repeat(9_994));
        assert!(check(&src, FortranStandard::F2023).is_empty());
        let src = format!("x = 0{}!c\n", " ".repeat(10_000));
        let w = check(&src, FortranStandard::F2023);
        assert_eq!(w.len(), 1);
        assert!(w[0].msg.contains("10,000"));
    }

    #[test]
    fn line_limit_suppressed_by_compat_flag() {
        let src = format!("print *, {}\n", "1".repeat(200));
        let w = check_source_limits(&src, FortranStandard::F2018, SourceForm::FreeForm, true);
        assert!(w.is_empty());
    }

    #[test]
    fn fixed_form_is_exempt() {
        let src = format!("      x = {}\n", "1".repeat(200));
        let w = check_source_limits(&src, FortranStandard::F2018, SourceForm::FixedForm, false);
        assert!(w.is_empty());
    }

    #[test]
    fn million_char_statement_warns_under_f2023_only() {
        // 101 lines of ~9,990 chars joined by & — past one million.
        let mut src = String::from("x = 1");
        for _ in 0..101 {
            src.push_str(" &\n");
            src.push_str(&format!("  + {}", "1".repeat(9_990)));
        }
        src.push('\n');
        let w: Vec<_> = check(&src, FortranStandard::F2023)
            .into_iter()
            .filter(|w| w.msg.contains("statement"))
            .collect();
        assert_eq!(w.len(), 1, "expected exactly one statement-length warning");
        assert_eq!(w[0].span.start.line, 1, "warning points at statement start");
        assert!(check(&src, FortranStandard::F2018)
            .iter()
            .all(|w| !w.msg.contains("statement")));
    }

    #[test]
    fn continuation_state_handles_strings_and_comments() {
        let mut s = None;
        assert!(line_continues("x = 1 + &", &mut s));
        assert!(line_continues("x = 1 + & ! and more", &mut s));
        assert!(!line_continues("x = 1 ! & in comment", &mut s));
        // & inside a continued string: string continuation.
        let mut s = None;
        assert!(line_continues("y = 'abc &", &mut s));
        assert_eq!(s, Some('\''));
        assert!(!line_continues("def'", &mut s));
        assert_eq!(s, None);
        // ! inside a string is not a comment.
        let mut s = None;
        assert!(line_continues("z = 'not!comment' // 'x &", &mut s));
        // Doubled quotes stay in-string.
        let mut s = None;
        assert!(!line_continues("w = 'it''s fine'", &mut s));
        assert_eq!(s, None);
    }
}
