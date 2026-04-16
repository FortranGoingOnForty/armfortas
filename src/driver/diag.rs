//! Diagnostic rendering: file:line:col header + source line + caret,
//! optionally colorised when stderr is a TTY.
//!
//! Sprint 32 #5 / #506.  Output format mirrors gfortran / clang so
//! editors and IDEs can parse it with their existing regexes.

use std::io::IsTerminal;

use crate::lexer::Span;

/// Severity of a rendered diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
    Note,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warning => "warning",
            Level::Note => "note",
        }
    }
    fn color(self) -> &'static str {
        // ANSI SGR colour codes; matched to clang's defaults.
        match self {
            Level::Error => "\x1b[31;1m",   // bright red
            Level::Warning => "\x1b[33;1m", // bright yellow
            Level::Note => "\x1b[34;1m",    // bright blue
        }
    }
}

/// True when the given fd looks like a TTY and ANSI escapes are
/// safe to emit.  Honours NO_COLOR and CLICOLOR_FORCE per the
/// no-color.org convention so users / CI can override.
fn use_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var_os("CLICOLOR_FORCE")
        .map(|v| v != "0")
        .unwrap_or(false)
    {
        return true;
    }
    std::io::stderr().is_terminal()
}

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";

/// Render a single diagnostic to stderr, gfortran-style:
///   file:line:col: error: message
///       12 |     print *, xyz
///          |              ^^^
///
/// `source` is the full original source text — needed to extract the
/// line for the gutter view.  `span_len` controls how wide the caret
/// underline is (defaults to a single `^` when zero).
pub fn render(file: &str, source: &str, span: Span, level: Level, message: &str, span_len: usize) {
    let color = use_color();
    let (col_start, reset) = if color {
        (level.color(), ANSI_RESET)
    } else {
        ("", "")
    };
    let bold = if color { ANSI_BOLD } else { "" };

    eprintln!(
        "{bold}{file}:{line}:{col}:{reset} {col_start}{label}:{reset} {bold}{msg}{reset}",
        bold = bold,
        file = file,
        line = span.start.line,
        col = span.start.col,
        reset = reset,
        col_start = col_start,
        label = level.label(),
        msg = message,
    );

    if let Some((gutter, line_text, caret_indent, caret_len)) = snippet_for(source, span, span_len)
    {
        let blue = if color { "\x1b[34m" } else { "" };
        let gutter_width = gutter.to_string().len().max(5);
        let caret_gutter = " ".repeat(gutter_width);
        eprintln!(
            "{blue}{gutter:>width$} |{reset} {line}",
            gutter = gutter,
            width = gutter_width,
            reset = reset,
            blue = blue,
            line = line_text
        );
        let mut caret = String::new();
        for _ in 0..caret_indent {
            caret.push(' ');
        }
        for _ in 0..caret_len.max(1) {
            caret.push('^');
        }
        eprintln!(
            "{blue}{caret_gutter} |{reset} {col_start}{caret}{reset}",
            blue = blue,
            caret_gutter = caret_gutter,
            reset = reset,
            col_start = col_start,
            caret = caret
        );
    }
}

/// Pull the requested source line out of `source` for display.
/// Returns `(line_number, line_text, caret_indent, caret_len)`.
/// caret_indent is in display columns assuming a tab is replaced by
/// 4 spaces, matching how the line_text is emitted.
fn snippet_for(source: &str, span: Span, span_len: usize) -> Option<(u32, String, usize, usize)> {
    if span.start.line == 0 {
        return None;
    }
    let target = span.start.line as usize;
    let raw_line = source.lines().nth(target.saturating_sub(1))?;
    let display_line: String = raw_line.replace('\t', "    ");
    // span.start.col is 1-based.  Convert tabs in the prefix to the
    // same 4-space expansion so the caret lines up.
    let prefix_chars = span.start.col.saturating_sub(1) as usize;
    let mut indent = 0usize;
    for (i, ch) in raw_line.chars().enumerate() {
        if i >= prefix_chars {
            break;
        }
        indent += if ch == '\t' { 4 } else { 1 };
    }
    let caret_len = if span_len == 0 { 1 } else { span_len };
    Some((span.start.line, display_line, indent, caret_len))
}
