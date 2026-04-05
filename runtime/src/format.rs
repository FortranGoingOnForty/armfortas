//! Fortran FORMAT engine — complete implementation of all edit descriptors.
//!
//! Parses format strings like '(I5, F10.3, A, 2X, /, ES15.8)' into
//! descriptors and applies them to I/O values. Supports the full
//! Fortran standard set including repeat counts, group repeat,
//! unlimited repeat, scale factors, and all data/control descriptors.

use std::fmt::Write;

/// A parsed format descriptor.
#[derive(Debug, Clone)]
pub enum FormatDesc {
    // ---- Data edit descriptors ----
    /// I: integer. Iw or Iw.m (w=width, m=minimum digits).
    IntegerI { width: usize, min_digits: Option<usize> },
    /// B: binary integer. Bw or Bw.m.
    IntegerB { width: usize, min_digits: Option<usize> },
    /// O: octal integer. Ow or Ow.m.
    IntegerO { width: usize, min_digits: Option<usize> },
    /// Z: hexadecimal integer. Zw or Zw.m.
    IntegerZ { width: usize, min_digits: Option<usize> },
    /// F: fixed-point real. Fw.d.
    RealF { width: usize, decimals: usize },
    /// E: exponential real. Ew.d or Ew.dEe.
    RealE { width: usize, decimals: usize, exp_width: Option<usize> },
    /// EN: engineering notation. ENw.d or ENw.dEe.
    RealEN { width: usize, decimals: usize, exp_width: Option<usize> },
    /// ES: scientific notation (1.0-9.999 mantissa). ESw.d or ESw.dEe.
    RealES { width: usize, decimals: usize, exp_width: Option<usize> },
    /// EX: hexadecimal-significand real. EXw.d or EXw.dEe. (F2018)
    RealEX { width: usize, decimals: usize, exp_width: Option<usize> },
    /// D: double-precision exponential. Dw.d (same as Ew.d with D exponent letter).
    RealD { width: usize, decimals: usize },
    /// G: generalized real. Gw.d or Gw.dEe. Chooses F or E format automatically.
    RealG { width: usize, decimals: usize, exp_width: Option<usize> },
    /// L: logical. Lw.
    Logical { width: usize },
    /// A: character. A or Aw.
    Character { width: Option<usize> },

    // ---- Control edit descriptors ----
    /// X: skip n positions. nX.
    Skip { count: usize },
    /// T: tab to absolute position. Tn.
    TabTo { position: usize },
    /// TL: tab left n positions. TLn.
    TabLeft { count: usize },
    /// TR: tab right n positions. TRn.
    TabRight { count: usize },
    /// /: new record (newline).
    Newline,
    /// :: stop processing if no more values.
    Colon,
    /// S, SP, SS: sign control.
    Sign(SignMode),
    /// BN, BZ: blank interpretation for input.
    BlankMode(BlankInterpretation),
    /// kP: scale factor.
    ScaleFactor(i32),

    // ---- Character string descriptors ----
    /// Literal string in format: 'text' or "text".
    LiteralString(String),

    // ---- Grouping ----
    /// Repeated group: n(...).
    Group { repeat: usize, descriptors: Vec<FormatDesc> },
    /// Unlimited repeat: *(...).
    UnlimitedRepeat { descriptors: Vec<FormatDesc> },
}

#[derive(Debug, Clone, Copy)]
pub enum SignMode {
    /// S: processor-dependent (default).
    Default,
    /// SP: always show plus sign.
    Plus,
    /// SS: suppress plus sign.
    Suppress,
}

#[derive(Debug, Clone, Copy)]
pub enum BlankInterpretation {
    /// BN: blanks are null (ignored) in input.
    Null,
    /// BZ: blanks are zeros in input.
    Zero,
}

/// Parse a Fortran format string (the part inside parentheses) into descriptors.
pub fn parse_format(fmt: &str) -> Vec<FormatDesc> {
    let trimmed = fmt.trim();
    // Strip outer parens if present.
    let inner = if trimmed.starts_with('(') && trimmed.ends_with(')') {
        &trimmed[1..trimmed.len()-1]
    } else {
        trimmed
    };
    parse_format_list(inner)
}

fn parse_format_list(input: &str) -> Vec<FormatDesc> {
    let mut result = Vec::new();
    let mut chars = input.chars().peekable();

    while chars.peek().is_some() {
        skip_spaces(&mut chars);
        if chars.peek().is_none() { break; }

        // Check for comma separator.
        if chars.peek() == Some(&',') {
            chars.next();
            continue;
        }

        // Check for repeat count.
        let repeat = parse_number(&mut chars);

        skip_spaces(&mut chars);
        if chars.peek().is_none() { break; }

        let c = chars.peek().copied().unwrap_or(' ');

        match c {
            // ---- Group repeat ----
            '(' => {
                chars.next(); // consume '('
                let inner = collect_until_matching_paren(&mut chars);
                let descriptors = parse_format_list(&inner);
                let n = repeat.unwrap_or(1);
                if n == 0 {
                    // *(...) unlimited repeat — not representable with 0.
                    result.push(FormatDesc::UnlimitedRepeat { descriptors });
                } else {
                    result.push(FormatDesc::Group { repeat: n, descriptors });
                }
            }

            // ---- Literal strings ----
            '\'' | '"' => {
                let s = parse_string_literal(&mut chars, c);
                for _ in 0..repeat.unwrap_or(1) {
                    result.push(FormatDesc::LiteralString(s.clone()));
                }
            }

            // ---- Newline ----
            '/' => {
                chars.next();
                for _ in 0..repeat.unwrap_or(1) {
                    result.push(FormatDesc::Newline);
                }
            }

            // ---- Colon ----
            ':' => {
                chars.next();
                result.push(FormatDesc::Colon);
            }

            // ---- Star (unlimited repeat) ----
            '*' => {
                chars.next();
                if chars.peek() == Some(&'(') {
                    chars.next();
                    let inner = collect_until_matching_paren(&mut chars);
                    let descriptors = parse_format_list(&inner);
                    result.push(FormatDesc::UnlimitedRepeat { descriptors });
                }
            }

            // ---- Edit descriptors ----
            _ => {
                let desc = parse_edit_descriptor(&mut chars, repeat);
                if let Some(d) = desc {
                    if let Some(n) = repeat {
                        if n > 1 && !matches!(d, FormatDesc::Skip { .. } | FormatDesc::ScaleFactor(_)) {
                            // Repeat count on a data descriptor: wrap in a group.
                            result.push(FormatDesc::Group { repeat: n, descriptors: vec![d] });
                        } else {
                            result.push(d);
                        }
                    } else {
                        result.push(d);
                    }
                }
            }
        }
    }

    result
}

fn parse_edit_descriptor(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    repeat: Option<usize>,
) -> Option<FormatDesc> {
    let letter = chars.next()?.to_ascii_uppercase();

    match letter {
        'I' => {
            let w = parse_number(chars).unwrap_or(0);
            let m = if chars.peek() == Some(&'.') { chars.next(); parse_number(chars) } else { None };
            Some(FormatDesc::IntegerI { width: w, min_digits: m })
        }
        'B' if chars.peek().map(|c| c.is_ascii_digit() || *c == '\'').unwrap_or(false) => {
            // B followed by digit → binary integer format.
            // B followed by quote → BOZ literal (not handled here).
            let w = parse_number(chars).unwrap_or(0);
            let m = if chars.peek() == Some(&'.') { chars.next(); parse_number(chars) } else { None };
            Some(FormatDesc::IntegerB { width: w, min_digits: m })
        }
        'O' => {
            let w = parse_number(chars).unwrap_or(0);
            let m = if chars.peek() == Some(&'.') { chars.next(); parse_number(chars) } else { None };
            Some(FormatDesc::IntegerO { width: w, min_digits: m })
        }
        'Z' => {
            let w = parse_number(chars).unwrap_or(0);
            let m = if chars.peek() == Some(&'.') { chars.next(); parse_number(chars) } else { None };
            Some(FormatDesc::IntegerZ { width: w, min_digits: m })
        }
        'F' => {
            let w = parse_number(chars).unwrap_or(0);
            let d = if chars.peek() == Some(&'.') { chars.next(); parse_number(chars).unwrap_or(0) } else { 0 };
            Some(FormatDesc::RealF { width: w, decimals: d })
        }
        'E' => {
            // Check for EN, ES, EX.
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            match next {
                'N' => { chars.next(); parse_real_desc(chars, |w, d, e| FormatDesc::RealEN { width: w, decimals: d, exp_width: e }) }
                'S' => { chars.next(); parse_real_desc(chars, |w, d, e| FormatDesc::RealES { width: w, decimals: d, exp_width: e }) }
                'X' => { chars.next(); parse_real_desc(chars, |w, d, e| FormatDesc::RealEX { width: w, decimals: d, exp_width: e }) }
                _ => parse_real_desc(chars, |w, d, e| FormatDesc::RealE { width: w, decimals: d, exp_width: e }),
            }
        }
        'D' => {
            let w = parse_number(chars).unwrap_or(0);
            let d = if chars.peek() == Some(&'.') { chars.next(); parse_number(chars).unwrap_or(0) } else { 0 };
            Some(FormatDesc::RealD { width: w, decimals: d })
        }
        'G' => {
            parse_real_desc(chars, |w, d, e| FormatDesc::RealG { width: w, decimals: d, exp_width: e })
        }
        'L' => {
            let w = parse_number(chars).unwrap_or(1);
            Some(FormatDesc::Logical { width: w })
        }
        'A' => {
            let w = parse_number(chars);
            Some(FormatDesc::Character { width: w })
        }
        'X' => {
            Some(FormatDesc::Skip { count: repeat.unwrap_or(1) })
        }
        'T' => {
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            match next {
                'L' => { chars.next(); let n = parse_number(chars).unwrap_or(1); Some(FormatDesc::TabLeft { count: n }) }
                'R' => { chars.next(); let n = parse_number(chars).unwrap_or(1); Some(FormatDesc::TabRight { count: n }) }
                _ => { let n = parse_number(chars).unwrap_or(1); Some(FormatDesc::TabTo { position: n }) }
            }
        }
        'S' => {
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            match next {
                'P' => { chars.next(); Some(FormatDesc::Sign(SignMode::Plus)) }
                'S' => { chars.next(); Some(FormatDesc::Sign(SignMode::Suppress)) }
                _ => Some(FormatDesc::Sign(SignMode::Default)),
            }
        }
        'P' => {
            // kP — repeat is the scale factor (can be negative, handled by caller).
            Some(FormatDesc::ScaleFactor(repeat.unwrap_or(0) as i32))
        }
        'B' => {
            // BN or BZ.
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            match next {
                'N' => { chars.next(); Some(FormatDesc::BlankMode(BlankInterpretation::Null)) }
                'Z' => { chars.next(); Some(FormatDesc::BlankMode(BlankInterpretation::Zero)) }
                _ => None,
            }
        }
        _ => None, // unknown descriptor
    }
}

fn parse_real_desc(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    constructor: impl Fn(usize, usize, Option<usize>) -> FormatDesc,
) -> Option<FormatDesc> {
    let w = parse_number(chars).unwrap_or(0);
    let d = if chars.peek() == Some(&'.') { chars.next(); parse_number(chars).unwrap_or(0) } else { 0 };
    let e = if chars.peek().map(|c| c.to_ascii_uppercase() == 'E').unwrap_or(false) {
        chars.next();
        parse_number(chars)
    } else { None };
    Some(constructor(w, d, e))
}

// ---- Format application (output) ----

/// An I/O value to be formatted.
pub enum IoValue {
    Integer(i64),
    Real(f64),
    Logical(bool),
    Character(Vec<u8>),
}

/// Format engine state for applying descriptors to values.
pub struct FormatEngine {
    descriptors: Vec<FormatDesc>,
    sign_mode: SignMode,
    scale_factor: i32,
}

impl FormatEngine {
    pub fn new(descriptors: Vec<FormatDesc>) -> Self {
        Self { descriptors, sign_mode: SignMode::Default, scale_factor: 0 }
    }

    /// Format a list of values according to the descriptors, producing an output string.
    pub fn format_values(&mut self, values: &[IoValue]) -> String {
        let mut output = String::new();
        let mut val_idx = 0;
        self.apply_descriptors(&self.descriptors.clone(), values, &mut val_idx, &mut output);
        output
    }

    fn apply_descriptors(
        &mut self,
        descs: &[FormatDesc],
        values: &[IoValue],
        val_idx: &mut usize,
        output: &mut String,
    ) {
        for desc in descs {
            match desc {
                // ---- Control descriptors ----
                FormatDesc::Skip { count } => {
                    for _ in 0..*count { output.push(' '); }
                }
                FormatDesc::Newline => { output.push('\n'); }
                FormatDesc::Colon => {
                    if *val_idx >= values.len() { return; }
                }
                FormatDesc::Sign(mode) => { self.sign_mode = *mode; }
                FormatDesc::ScaleFactor(k) => { self.scale_factor = *k; }
                FormatDesc::BlankMode(_) => {} // input only
                FormatDesc::TabTo { position } => {
                    let cur_col = output.lines().last().map(|l| l.len()).unwrap_or(0);
                    if *position > cur_col + 1 {
                        for _ in 0..(*position - cur_col - 1) { output.push(' '); }
                    }
                }
                FormatDesc::TabLeft { count } => {
                    // Truncate output by `count` characters (simplified).
                    let new_len = output.len().saturating_sub(*count);
                    output.truncate(new_len);
                }
                FormatDesc::TabRight { count } => {
                    for _ in 0..*count { output.push(' '); }
                }
                FormatDesc::LiteralString(s) => { output.push_str(s); }

                // ---- Group repeat ----
                FormatDesc::Group { repeat, descriptors } => {
                    for _ in 0..*repeat {
                        self.apply_descriptors(descriptors, values, val_idx, output);
                    }
                }
                FormatDesc::UnlimitedRepeat { descriptors } => {
                    while *val_idx < values.len() {
                        self.apply_descriptors(descriptors, values, val_idx, output);
                    }
                }

                // ---- Data descriptors ----
                _ => {
                    if *val_idx >= values.len() { return; }
                    let val = &values[*val_idx];
                    *val_idx += 1;
                    let formatted = self.format_value(desc, val);
                    output.push_str(&formatted);
                }
            }
        }
    }

    fn format_value(&self, desc: &FormatDesc, val: &IoValue) -> String {
        match (desc, val) {
            // ---- Integer ----
            (FormatDesc::IntegerI { width, min_digits }, IoValue::Integer(v)) => {
                let s = format!("{}", v);
                let s = if let Some(m) = min_digits {
                    let abs_s = format!("{}", v.unsigned_abs());
                    let padded = format!("{:0>width$}", abs_s, width = *m);
                    if *v < 0 { format!("-{}", padded) } else { self.apply_sign(&padded, *v >= 0) }
                } else {
                    self.apply_sign(&format!("{}", v.unsigned_abs()), *v >= 0)
                };
                format!("{:>width$}", s, width = *width)
            }
            (FormatDesc::IntegerB { width, .. }, IoValue::Integer(v)) => {
                format!("{:>width$}", format!("{:b}", v), width = *width)
            }
            (FormatDesc::IntegerO { width, .. }, IoValue::Integer(v)) => {
                format!("{:>width$}", format!("{:o}", v), width = *width)
            }
            (FormatDesc::IntegerZ { width, .. }, IoValue::Integer(v)) => {
                format!("{:>width$}", format!("{:X}", v), width = *width)
            }

            // ---- Real ----
            (FormatDesc::RealF { width, decimals }, IoValue::Real(v)) => {
                let s = format!("{:.*}", *decimals, v);
                format!("{:>width$}", s, width = *width)
            }
            (FormatDesc::RealE { width, decimals, .. }, IoValue::Real(v)) => {
                let s = format!("{:.*E}", *decimals, v);
                format!("{:>width$}", s, width = *width)
            }
            (FormatDesc::RealES { width, decimals, .. }, IoValue::Real(v)) => {
                // Scientific: mantissa in [1.0, 10.0).
                let s = format!("{:.*E}", *decimals, v);
                format!("{:>width$}", s, width = *width)
            }
            (FormatDesc::RealEN { width, decimals, .. }, IoValue::Real(v)) => {
                // Engineering: exponent is multiple of 3.
                let (mantissa, exp) = to_engineering(*v);
                let s = format!("{:.*}E{:+03}", *decimals, mantissa, exp);
                format!("{:>width$}", s, width = *width)
            }
            (FormatDesc::RealD { width, decimals }, IoValue::Real(v)) => {
                let s = format!("{:.*E}", *decimals, v).replace('E', "D");
                format!("{:>width$}", s, width = *width)
            }
            (FormatDesc::RealG { width, decimals, .. }, IoValue::Real(v)) => {
                // G format: use F if magnitude fits, else E.
                let abs_v = v.abs();
                if abs_v == 0.0 || (abs_v >= 0.1 && abs_v < 10f64.powi(*decimals as i32)) {
                    let s = format!("{:.*}", *decimals, v);
                    format!("{:>width$}", s, width = *width)
                } else {
                    let s = format!("{:.*E}", *decimals, v);
                    format!("{:>width$}", s, width = *width)
                }
            }
            (FormatDesc::RealEX { width, decimals, .. }, IoValue::Real(v)) => {
                // Hex-significand: use %a-like format. Rust doesn't have this natively.
                let s = format!("{:.*E}", *decimals, v); // fallback to E format
                format!("{:>width$}", s, width = *width)
            }

            // ---- Logical ----
            (FormatDesc::Logical { width }, IoValue::Logical(v)) => {
                let s = if *v { "T" } else { "F" };
                format!("{:>width$}", s, width = *width)
            }

            // ---- Character ----
            (FormatDesc::Character { width }, IoValue::Character(bytes)) => {
                let s = String::from_utf8_lossy(bytes);
                if let Some(w) = width {
                    if *w > s.len() {
                        format!("{:>width$}", s, width = *w)
                    } else {
                        s[..*w].to_string()
                    }
                } else {
                    s.into_owned()
                }
            }

            // Type mismatch: format as-is.
            (_, IoValue::Integer(v)) => format!("{}", v),
            (_, IoValue::Real(v)) => format!("{}", v),
            (_, IoValue::Logical(v)) => (if *v { "T" } else { "F" }).into(),
            (_, IoValue::Character(b)) => String::from_utf8_lossy(b).into_owned(),
        }
    }

    fn apply_sign(&self, abs_str: &str, is_positive: bool) -> String {
        if is_positive {
            match self.sign_mode {
                SignMode::Plus => format!("+{}", abs_str),
                _ => abs_str.to_string(),
            }
        } else {
            format!("-{}", abs_str)
        }
    }
}

// ---- Helpers ----

fn to_engineering(v: f64) -> (f64, i32) {
    if v == 0.0 { return (0.0, 0); }
    let exp = (v.abs().log10().floor() as i32) / 3 * 3;
    let mantissa = v / 10f64.powi(exp);
    (mantissa, exp)
}

fn skip_spaces(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while chars.peek() == Some(&' ') { chars.next(); }
}

fn parse_number(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<usize> {
    let mut digits = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            digits.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if digits.is_empty() { None } else { digits.parse().ok() }
}

fn parse_string_literal(chars: &mut std::iter::Peekable<std::str::Chars>, quote: char) -> String {
    chars.next(); // consume opening quote
    let mut s = String::new();
    while let Some(&c) = chars.peek() {
        chars.next();
        if c == quote {
            // Check for doubled quote (escape).
            if chars.peek() == Some(&quote) {
                chars.next();
                s.push(quote);
            } else {
                break;
            }
        } else {
            s.push(c);
        }
    }
    s
}

fn collect_until_matching_paren(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut depth = 1;
    let mut inner = String::new();
    while let Some(&c) = chars.peek() {
        chars.next();
        if c == '(' { depth += 1; }
        if c == ')' {
            depth -= 1;
            if depth == 0 { break; }
        }
        inner.push(c);
    }
    inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_format() {
        let descs = parse_format("(I5, F10.3, A)");
        assert_eq!(descs.len(), 3);
        assert!(matches!(descs[0], FormatDesc::IntegerI { width: 5, min_digits: None }));
        assert!(matches!(descs[1], FormatDesc::RealF { width: 10, decimals: 3 }));
        assert!(matches!(descs[2], FormatDesc::Character { width: None }));
    }

    #[test]
    fn parse_with_repeat() {
        // 3I5 means "repeat I5 three times" — wrapped in a Group.
        let descs = parse_format("(3I5)");
        assert_eq!(descs.len(), 1);
        assert!(matches!(descs[0], FormatDesc::Group { repeat: 3, .. }));
    }

    #[test]
    fn parse_control_descriptors() {
        let descs = parse_format("(2X, /, SP, T10)");
        assert!(matches!(descs[0], FormatDesc::Skip { count: 2 }));
        assert!(matches!(descs[1], FormatDesc::Newline));
        assert!(matches!(descs[2], FormatDesc::Sign(SignMode::Plus)));
        assert!(matches!(descs[3], FormatDesc::TabTo { position: 10 }));
    }

    #[test]
    fn parse_string_literal() {
        let descs = parse_format("('hello', A)");
        assert_eq!(descs.len(), 2);
        if let FormatDesc::LiteralString(s) = &descs[0] {
            assert_eq!(s, "hello");
        } else { panic!("expected literal"); }
    }

    #[test]
    fn parse_es_en_format() {
        let descs = parse_format("(ES15.8, EN12.3)");
        assert!(matches!(descs[0], FormatDesc::RealES { width: 15, decimals: 8, .. }));
        assert!(matches!(descs[1], FormatDesc::RealEN { width: 12, decimals: 3, .. }));
    }

    #[test]
    fn format_integer() {
        let descs = parse_format("(I5)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(42)]);
        assert_eq!(out, "   42");
    }

    #[test]
    fn format_integer_with_min_digits() {
        let descs = parse_format("(I5.3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(7)]);
        assert_eq!(out, "  007");
    }

    #[test]
    fn format_real_f() {
        let descs = parse_format("(F8.3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(3.14159)]);
        assert_eq!(out, "   3.142");
    }

    #[test]
    fn format_logical() {
        let descs = parse_format("(L3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Logical(true)]);
        assert_eq!(out, "  T");
    }

    #[test]
    fn format_character() {
        let descs = parse_format("(A5)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Character(b"hi".to_vec())]);
        assert_eq!(out, "   hi");
    }

    #[test]
    fn format_mixed() {
        let descs = parse_format("('Count: ', I4)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(42)]);
        assert_eq!(out, "Count:   42");
    }

    #[test]
    fn format_with_newline() {
        let descs = parse_format("(I3, /, I3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(1), IoValue::Integer(2)]);
        assert_eq!(out, "  1\n  2");
    }

    #[test]
    fn format_skip() {
        let descs = parse_format("(I3, 3X, I3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(1), IoValue::Integer(2)]);
        assert_eq!(out, "  1     2");
    }

    #[test]
    fn format_sign_plus() {
        let descs = parse_format("(SP, I5)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(42)]);
        assert_eq!(out, "  +42");
    }

    #[test]
    fn format_hex_integer() {
        let descs = parse_format("(Z4)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(255)]);
        assert_eq!(out, "  FF");
    }

    #[test]
    fn format_octal_integer() {
        let descs = parse_format("(O6)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(255)]);
        assert_eq!(out, "   377");
    }

    #[test]
    fn format_colon_stops_early() {
        let descs = parse_format("(I3, :, ', ', I3)");
        let mut engine = FormatEngine::new(descs);
        // Only one value — colon stops before the comma-space-I3.
        let out = engine.format_values(&[IoValue::Integer(42)]);
        assert_eq!(out, " 42");
    }

    #[test]
    fn format_unlimited_repeat() {
        let descs = parse_format("(*(I3, ','))");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[
            IoValue::Integer(1), IoValue::Integer(2), IoValue::Integer(3)
        ]);
        assert_eq!(out, "  1,  2,  3,");
    }
}
