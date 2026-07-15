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

/// Unconditional statement-size cap, enforced at every `--std` level
/// (unlike the conformance warnings). Every recursive walker in the
/// pipeline (parser nesting, sema, IR lowering) has depth bounded by the
/// statement's token count, which is bounded by its character count. Twice
/// the F2023 limit keeps pathological work finite without rejecting a
/// conforming program; past the cap the compiler errors before parsing.
pub const STMT_HARD_CAP: usize = 2 * STMT_LIMIT;

/// Scan for a statement exceeding [`STMT_HARD_CAP`]. Both source
/// forms: free-form joins on trailing `&`, fixed form on a nonblank
/// column 6 of the following line.
pub fn find_over_cap_statement(source: &str, form: SourceForm) -> Option<(Span, usize)> {
    let mut stmt_start: u32 = 0;
    let mut stmt_chars: usize = 0;
    let mut in_string: Option<char> = None;
    let lines: Vec<&str> = source.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let line_no = idx as u32 + 1;
        let is_gap = match form {
            SourceForm::FreeForm => is_free_form_continuation_gap(line),
            SourceForm::FixedForm => is_fixed_form_continuation_gap(line),
        };
        if is_gap {
            continue;
        }
        if stmt_chars == 0 {
            stmt_start = line_no;
        }
        stmt_chars += line.chars().count();
        let continued = match form {
            SourceForm::FreeForm => line_continues(line, &mut in_string),
            SourceForm::FixedForm => lines
                .iter()
                .skip(idx + 1)
                .find(|next| !is_fixed_form_continuation_gap(next))
                .is_some_and(|next| crate::lexer::fixed::is_continuation_line(next)),
        };
        if !continued {
            if stmt_chars > STMT_HARD_CAP {
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
                return Some((at, stmt_chars));
            }
            stmt_chars = 0;
            in_string = None;
        }
    }
    None
}

/// Scan `source` for F2023 source-limit conformance violations.
/// `suppress_line_limit` is `-ffree-line-length-none`: gfortran's
/// meaning of that flag is "no line-length conformance concern", so it
/// silences the line warning (the statement limit still applies).
/// `line_limit_override` is numeric `-ffree-line-length-N`: armfortas
/// still compiles the full line, but conformance warnings use the
/// requested GNU-compatible limit.
/// Fixed form is untouched — F2023's new limits are free-form.
pub fn check_source_limits(
    source: &str,
    std: FortranStandard,
    form: SourceForm,
    suppress_line_limit: bool,
    line_limit_override: Option<usize>,
) -> Vec<LimitWarning> {
    if form != SourceForm::FreeForm {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    let limit = line_limit_override.unwrap_or_else(|| line_limit(std));

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
                msg: if let Some(limit) = line_limit_override {
                    format!(
                        "line is {} characters long; -ffree-line-length-{} limits \
                         free-form lines to {} (conformance warning; the line is \
                         compiled in full)",
                        len, limit, limit
                    )
                } else if limit == 132 {
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
        if is_free_form_continuation_gap(line) {
            continue;
        }
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

fn is_free_form_continuation_gap(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.is_empty() || trimmed.starts_with('!')
}

fn is_fixed_form_continuation_gap(line: &str) -> bool {
    line.trim().is_empty()
        || matches!(
            line.chars().next(),
            Some('c') | Some('C') | Some('*') | Some('!')
        )
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

    fn continued_sum_with_comment_gap(limit: usize) -> String {
        let chunk = "+1".repeat(limit / 4 + 1);
        format!("x = 0 {chunk} &\n! legal continuation gap\n\n  & {chunk}\n")
    }

    fn fixed_form_sum_with_comment_gap(limit: usize) -> String {
        let chunk = "+1".repeat(limit / 4 + 1);
        format!("      x = 0 {chunk}\nC legal continuation gap\n\n     + {chunk}\n")
    }

    fn tab_form_sum_with_comment_gap(limit: usize) -> String {
        let chunk = "+1".repeat(limit / 4 + 1);
        format!("\tx = 0 {chunk}\nC legal continuation gap\n\n\t1{chunk}\n")
    }

    #[test]
    fn hard_cap_finds_oversized_statements_in_both_forms() {
        // Free form: fat continuation lines past the cap.
        let line = format!(
            "  x = x {} &
",
            "+ 1".repeat(400)
        );
        let mut src = String::from(
            "x = 0 &
",
        );
        for _ in 0..2_000 {
            src.push_str(&line);
        }
        src.push_str(
            "  + 1
",
        );
        let hit = find_over_cap_statement(&src, SourceForm::FreeForm);
        assert!(
            hit.is_some(),
            "2.4M-char free-form statement must trip the cap"
        );
        assert_eq!(hit.unwrap().0.start.line, 1);

        // Fixed form: column-6 continuation marks.
        let mut src = String::from(
            "      x = 0
",
        );
        let cont = format!(
            "     + {}
",
            "+ 1 ".repeat(300)
        );
        for _ in 0..2_000 {
            src.push_str(&cont);
        }
        assert!(
            find_over_cap_statement(&src, SourceForm::FixedForm).is_some(),
            "fixed-form continuation chain past the cap must trip"
        );

        // Under the cap: nothing fires.
        assert!(find_over_cap_statement(
            "x = 1
y = 2
",
            SourceForm::FreeForm
        )
        .is_none());
    }

    #[test]
    fn comment_gaps_do_not_bypass_the_hard_cap() {
        let src = continued_sum_with_comment_gap(STMT_HARD_CAP);
        let (span, chars) = find_over_cap_statement(&src, SourceForm::FreeForm)
            .expect("comment and blank gaps must not split a continued statement");
        assert_eq!(span.start.line, 1);
        assert!(chars > STMT_HARD_CAP);
        let source_chars: usize = src.lines().map(|line| line.chars().count()).sum();
        let statement_chars: usize = src
            .lines()
            .filter(|line| !is_free_form_continuation_gap(line))
            .map(|line| line.chars().count())
            .sum();
        assert_eq!(chars, statement_chars);
        assert!(
            source_chars > chars,
            "comment text must not count as statement text"
        );

        let src = fixed_form_sum_with_comment_gap(STMT_HARD_CAP);
        let (span, chars) = find_over_cap_statement(&src, SourceForm::FixedForm)
            .expect("fixed-form comment and blank gaps must preserve continuation");
        assert_eq!(span.start.line, 1);
        assert!(chars > STMT_HARD_CAP);

        let src = tab_form_sum_with_comment_gap(STMT_HARD_CAP);
        let (span, chars) = find_over_cap_statement(&src, SourceForm::FixedForm)
            .expect("tab-form continuation must use the fixed-form lexer contract");
        assert_eq!(span.start.line, 1);
        assert!(chars > STMT_HARD_CAP);
    }

    fn check(src: &str, std: FortranStandard) -> Vec<LimitWarning> {
        check_source_limits(src, std, SourceForm::FreeForm, false, None)
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
        let w = check_source_limits(
            &src,
            FortranStandard::F2018,
            SourceForm::FreeForm,
            true,
            None,
        );
        assert!(w.is_empty());
    }

    #[test]
    fn numeric_free_line_limit_overrides_std_warning_threshold() {
        let src = format!("print *, {}\n", "1".repeat(200));
        let w = check_source_limits(
            &src,
            FortranStandard::F2023,
            SourceForm::FreeForm,
            false,
            Some(132),
        );
        assert_eq!(w.len(), 1);
        assert!(w[0].msg.contains("-ffree-line-length-132"));
    }

    #[test]
    fn fixed_form_is_exempt() {
        let src = format!("      x = {}\n", "1".repeat(200));
        let w = check_source_limits(
            &src,
            FortranStandard::F2018,
            SourceForm::FixedForm,
            false,
            None,
        );
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
    fn comment_gaps_do_not_bypass_the_f2023_statement_limit() {
        let src = continued_sum_with_comment_gap(STMT_LIMIT);
        let warnings: Vec<_> = check(&src, FortranStandard::F2023)
            .into_iter()
            .filter(|warning| warning.msg.contains("statement"))
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].span.start.line, 1);
    }

    #[test]
    fn comment_gap_lines_still_obey_the_physical_line_limit() {
        let src = format!("x = 1 &\n!{}\n  & + 1\n", "x".repeat(10_000));
        let warnings = check(&src, FortranStandard::F2023);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].span.start.line, 2);
        assert!(warnings[0].msg.contains("line is 10001 characters long"));
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
