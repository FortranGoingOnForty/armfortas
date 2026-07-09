//! Fortran FORMAT engine — complete implementation of all edit descriptors.
//!
//! Parses format strings like '(I5, F10.3, A, 2X, /, ES15.8)' into
//! descriptors and applies them to I/O values. Supports the full
//! Fortran standard set including repeat counts, group repeat,
//! unlimited repeat, scale factors, and all data/control descriptors.

/// A parsed format descriptor.
#[derive(Debug, Clone)]
pub enum FormatDesc {
    // ---- Data edit descriptors ----
    /// I: integer. Iw or Iw.m (w=width, m=minimum digits).
    IntegerI {
        width: usize,
        min_digits: Option<usize>,
    },
    /// B: binary integer. Bw or Bw.m.
    IntegerB {
        width: usize,
        min_digits: Option<usize>,
    },
    /// O: octal integer. Ow or Ow.m.
    IntegerO {
        width: usize,
        min_digits: Option<usize>,
    },
    /// Z: hexadecimal integer. Zw or Zw.m.
    IntegerZ {
        width: usize,
        min_digits: Option<usize>,
    },
    /// F: fixed-point real. Fw.d.
    RealF { width: usize, decimals: usize },
    /// E: exponential real. Ew.d or Ew.dEe.
    RealE {
        width: usize,
        decimals: usize,
        exp_width: Option<usize>,
    },
    /// EN: engineering notation. ENw.d or ENw.dEe.
    RealEN {
        width: usize,
        decimals: usize,
        exp_width: Option<usize>,
    },
    /// ES: scientific notation (1.0-9.999 mantissa). ESw.d or ESw.dEe.
    RealES {
        width: usize,
        decimals: usize,
        exp_width: Option<usize>,
    },
    /// EX: hexadecimal-significand real. EXw.d or EXw.dEe. (F2018)
    RealEX {
        width: usize,
        decimals: usize,
        exp_width: Option<usize>,
    },
    /// D: double-precision exponential. Dw.d (same as Ew.d with D exponent letter).
    RealD { width: usize, decimals: usize },
    /// G: generalized real. Gw.d or Gw.dEe. Chooses F or E format automatically.
    RealG {
        width: usize,
        decimals: usize,
        exp_width: Option<usize>,
    },
    /// L: logical. Lw.
    Logical { width: usize },
    /// A: character. A or Aw.
    Character { width: Option<usize> },
    /// AT: character output trimmed to len_trim — A with trailing
    /// blanks removed (F2023). No width form.
    CharTrimmed,

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
    /// RU, RD, RZ, RN, RC, RP: rounding mode (F2003).
    RoundingMode(RoundMode),
    /// DC, DP: decimal comma or point mode (F2003).
    DecimalMode(DecimalSep),
    /// LZ, LZS, LZP: leading-zero control for F/E/D/G output (F2023).
    LeadingZero(LeadingZeroMode),
    /// DT: derived type I/O (F2003). Placeholder — requires user-defined I/O procedures.
    DerivedType { type_name: String },

    // ---- Character string descriptors ----
    /// Literal string in format: 'text' or "text".
    LiteralString(String),

    // ---- Grouping ----
    /// Repeated group: n(...).
    Group {
        repeat: usize,
        descriptors: Vec<FormatDesc>,
    },
    /// Unlimited repeat: *(...).
    UnlimitedRepeat { descriptors: Vec<FormatDesc> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    InvalidFormat,
    TypeMismatch,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadingZeroMode {
    /// LZ: return to the processor default (armfortas prints the zero).
    Default,
    /// LZP: print the leading zero before the decimal point.
    Print,
    /// LZS: suppress the leading zero (`.25` instead of `0.25`).
    Suppress,
}

impl LeadingZeroMode {
    /// Map a LEADING_ZERO= specifier value to a mode. `'PRINT'` and
    /// `'SUPPRESS'` set the obvious modes; `'PROCESSOR_DEFINED'` and any
    /// unrecognized value fall back to the processor default. Case- and
    /// whitespace-insensitive, matching the existing specifier handling.
    pub fn from_specifier(s: &str) -> LeadingZeroMode {
        match s.trim().to_ascii_lowercase().as_str() {
            "print" => LeadingZeroMode::Print,
            "suppress" => LeadingZeroMode::Suppress,
            _ => LeadingZeroMode::Default,
        }
    }

    /// The INQUIRE LEADING_ZERO= string for a formatted connection's
    /// current mode (F2023 12.10.2.15). `UNDEFINED` is handled by the
    /// caller for the no-connection / unformatted cases.
    pub fn inquire_str(self) -> &'static str {
        match self {
            LeadingZeroMode::Print => "PRINT",
            LeadingZeroMode::Suppress => "SUPPRESS",
            LeadingZeroMode::Default => "PROCESSOR_DEFINED",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum BlankInterpretation {
    /// BN: blanks are null (ignored) in input.
    Null,
    /// BZ: blanks are zeros in input.
    Zero,
}

#[derive(Debug, Clone, Copy)]
pub enum RoundMode {
    Up,               // RU
    Down,             // RD
    Zero,             // RZ
    Nearest,          // RN
    Compatible,       // RC
    ProcessorDefined, // RP
}

#[derive(Debug, Clone, Copy)]
pub enum DecimalSep {
    Comma, // DC
    Point, // DP
}

/// Parse a Fortran format string (the part inside parentheses) into descriptors.
pub fn parse_format(fmt: &str) -> Vec<FormatDesc> {
    let trimmed = fmt.trim();
    // Strip outer parens if present.
    let inner = if trimmed.starts_with('(') && trimmed.ends_with(')') {
        &trimmed[1..trimmed.len() - 1]
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
        if chars.peek().is_none() {
            break;
        }

        // Check for comma separator.
        if chars.peek() == Some(&',') {
            chars.next();
            continue;
        }

        // Check for negative sign (for scale factor: -kP).
        let negative = if chars.peek() == Some(&'-') {
            chars.next();
            true
        } else {
            false
        };

        // Check for repeat count.
        let repeat = parse_number(&mut chars);

        skip_spaces(&mut chars);
        if chars.peek().is_none() {
            break;
        }

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
                    result.push(FormatDesc::Group {
                        repeat: n,
                        descriptors,
                    });
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
                let desc = parse_edit_descriptor(&mut chars, repeat, negative);
                if let Some(d) = desc {
                    if let Some(n) = repeat {
                        if n > 1
                            && !matches!(d, FormatDesc::Skip { .. } | FormatDesc::ScaleFactor(_))
                        {
                            // Repeat count on a data descriptor: wrap in a group.
                            result.push(FormatDesc::Group {
                                repeat: n,
                                descriptors: vec![d],
                            });
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
    negative: bool,
) -> Option<FormatDesc> {
    let letter = chars.next()?.to_ascii_uppercase();

    match letter {
        'I' => {
            let w = parse_number(chars).unwrap_or(0);
            let m = if chars.peek() == Some(&'.') {
                chars.next();
                parse_number(chars)
            } else {
                None
            };
            Some(FormatDesc::IntegerI {
                width: w,
                min_digits: m,
            })
        }
        'B' if chars
            .peek()
            .map(|c| c.is_ascii_digit() || *c == '\'')
            .unwrap_or(false) =>
        {
            // B followed by digit → binary integer format.
            // B followed by quote → BOZ literal (not handled here).
            let w = parse_number(chars).unwrap_or(0);
            let m = if chars.peek() == Some(&'.') {
                chars.next();
                parse_number(chars)
            } else {
                None
            };
            Some(FormatDesc::IntegerB {
                width: w,
                min_digits: m,
            })
        }
        'O' => {
            let w = parse_number(chars).unwrap_or(0);
            let m = if chars.peek() == Some(&'.') {
                chars.next();
                parse_number(chars)
            } else {
                None
            };
            Some(FormatDesc::IntegerO {
                width: w,
                min_digits: m,
            })
        }
        'Z' => {
            let w = parse_number(chars).unwrap_or(0);
            let m = if chars.peek() == Some(&'.') {
                chars.next();
                parse_number(chars)
            } else {
                None
            };
            Some(FormatDesc::IntegerZ {
                width: w,
                min_digits: m,
            })
        }
        'F' => {
            let w = parse_number(chars).unwrap_or(0);
            let d = if chars.peek() == Some(&'.') {
                chars.next();
                parse_number(chars).unwrap_or(0)
            } else {
                0
            };
            Some(FormatDesc::RealF {
                width: w,
                decimals: d,
            })
        }
        'E' => {
            // Check for EN, ES, EX.
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            match next {
                'N' => {
                    chars.next();
                    parse_real_desc(chars, |w, d, e| FormatDesc::RealEN {
                        width: w,
                        decimals: d,
                        exp_width: e,
                    })
                }
                'S' => {
                    chars.next();
                    parse_real_desc(chars, |w, d, e| FormatDesc::RealES {
                        width: w,
                        decimals: d,
                        exp_width: e,
                    })
                }
                'X' => {
                    chars.next();
                    parse_real_desc(chars, |w, d, e| FormatDesc::RealEX {
                        width: w,
                        decimals: d,
                        exp_width: e,
                    })
                }
                _ => parse_real_desc(chars, |w, d, e| FormatDesc::RealE {
                    width: w,
                    decimals: d,
                    exp_width: e,
                }),
            }
        }
        'D' => {
            // DC/DP (decimal mode) vs DT (derived type) vs Dw.d (real format).
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            match next {
                'C' => {
                    chars.next();
                    Some(FormatDesc::DecimalMode(DecimalSep::Comma))
                }
                'P' => {
                    chars.next();
                    Some(FormatDesc::DecimalMode(DecimalSep::Point))
                }
                'T' => {
                    chars.next();
                    // DT optionally followed by 'typename'.
                    let name = if chars.peek() == Some(&'\'') || chars.peek() == Some(&'"') {
                        let q = *chars.peek().unwrap();
                        parse_string_literal(chars, q)
                    } else {
                        String::new()
                    };
                    Some(FormatDesc::DerivedType { type_name: name })
                }
                _ => {
                    let w = parse_number(chars).unwrap_or(0);
                    let d = if chars.peek() == Some(&'.') {
                        chars.next();
                        parse_number(chars).unwrap_or(0)
                    } else {
                        0
                    };
                    Some(FormatDesc::RealD {
                        width: w,
                        decimals: d,
                    })
                }
            }
        }
        'G' => parse_real_desc(chars, |w, d, e| FormatDesc::RealG {
            width: w,
            decimals: d,
            exp_width: e,
        }),
        'L' => {
            // LZ/LZS/LZP (F2023 leading-zero control) — matched
            // longest-first so LZS is not read as LZ + S(ign). Plain
            // `L`/`Lw` is logical.
            if chars.peek().map(|c| c.to_ascii_uppercase()) == Some('Z') {
                chars.next();
                let mode = match chars.peek().map(|c| c.to_ascii_uppercase()) {
                    Some('S') => {
                        chars.next();
                        LeadingZeroMode::Suppress
                    }
                    Some('P') => {
                        chars.next();
                        LeadingZeroMode::Print
                    }
                    _ => LeadingZeroMode::Default,
                };
                Some(FormatDesc::LeadingZero(mode))
            } else {
                let w = parse_number(chars).unwrap_or(1);
                Some(FormatDesc::Logical { width: w })
            }
        }
        'A' => {
            // AT (F2023): A with trailing blanks trimmed. Distinguished
            // from `Aw` — AT takes no width (AT4 is malformed).
            if chars.peek().map(|c| c.to_ascii_uppercase()) == Some('T') {
                chars.next();
                Some(FormatDesc::CharTrimmed)
            } else {
                let w = parse_number(chars);
                Some(FormatDesc::Character { width: w })
            }
        }
        'X' => Some(FormatDesc::Skip {
            count: repeat.unwrap_or(1),
        }),
        'T' => {
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            match next {
                'L' => {
                    chars.next();
                    let n = parse_number(chars).unwrap_or(1);
                    Some(FormatDesc::TabLeft { count: n })
                }
                'R' => {
                    chars.next();
                    let n = parse_number(chars).unwrap_or(1);
                    Some(FormatDesc::TabRight { count: n })
                }
                _ => {
                    let n = parse_number(chars).unwrap_or(1);
                    Some(FormatDesc::TabTo { position: n })
                }
            }
        }
        'S' => {
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            match next {
                'P' => {
                    chars.next();
                    Some(FormatDesc::Sign(SignMode::Plus))
                }
                'S' => {
                    chars.next();
                    Some(FormatDesc::Sign(SignMode::Suppress))
                }
                _ => Some(FormatDesc::Sign(SignMode::Default)),
            }
        }
        'P' => {
            // kP — repeat is the scale factor magnitude, sign from negative flag.
            let k = repeat.unwrap_or(0) as i32;
            Some(FormatDesc::ScaleFactor(if negative { -k } else { k }))
        }
        'R' => {
            // Rounding modes: RU, RD, RZ, RN, RC, RP.
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            let mode = match next {
                'U' => {
                    chars.next();
                    Some(RoundMode::Up)
                }
                'D' => {
                    chars.next();
                    Some(RoundMode::Down)
                }
                'Z' => {
                    chars.next();
                    Some(RoundMode::Zero)
                }
                'N' => {
                    chars.next();
                    Some(RoundMode::Nearest)
                }
                'C' => {
                    chars.next();
                    Some(RoundMode::Compatible)
                }
                'P' => {
                    chars.next();
                    Some(RoundMode::ProcessorDefined)
                }
                _ => None,
            };
            mode.map(FormatDesc::RoundingMode)
        }
        'B' => {
            // BN or BZ.
            let next = chars.peek().copied().unwrap_or(' ').to_ascii_uppercase();
            match next {
                'N' => {
                    chars.next();
                    Some(FormatDesc::BlankMode(BlankInterpretation::Null))
                }
                'Z' => {
                    chars.next();
                    Some(FormatDesc::BlankMode(BlankInterpretation::Zero))
                }
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
    let d = if chars.peek() == Some(&'.') {
        chars.next();
        parse_number(chars).unwrap_or(0)
    } else {
        0
    };
    let e = if chars
        .peek()
        .map(|c| c.eq_ignore_ascii_case(&'E'))
        .unwrap_or(false)
    {
        chars.next();
        parse_number(chars)
    } else {
        None
    };
    Some(constructor(w, d, e))
}

// ---- Format application (output) ----

/// An I/O value to be formatted.
pub enum IoValue {
    Integer(i128),
    Real(f64),
    Logical(bool),
    Character(Vec<u8>),
}

/// Format engine state for applying descriptors to values.
pub struct FormatEngine {
    descriptors: Vec<FormatDesc>,
    sign_mode: SignMode,
    scale_factor: i32,
    round_mode: RoundMode,
    decimal_sep: DecimalSep,
    leading_zero: LeadingZeroMode,
}

impl FormatEngine {
    pub fn new(descriptors: Vec<FormatDesc>) -> Self {
        Self {
            descriptors,
            sign_mode: SignMode::Default,
            scale_factor: 0,
            round_mode: RoundMode::Compatible,
            decimal_sep: DecimalSep::Point,
            leading_zero: LeadingZeroMode::Default,
        }
    }

    /// Override the connection-level leading-zero mode (LEADING_ZERO= on
    /// OPEN, or a per-statement WRITE override). The default is the
    /// processor default (print). Wired in l05-2.
    pub fn set_leading_zero(&mut self, mode: LeadingZeroMode) {
        self.leading_zero = mode;
    }

    /// Suppress the single leading zero before the decimal point when in
    /// LZS mode: `0.25` -> `.25`, `-0.25` -> `-.25`. Other magnitudes
    /// (`10.25`, non-finite text) are untouched. Applied before field
    /// fitting so the freed column becomes leading blank. Print/Default
    /// keep the processor-default output (the zero), byte-identical to
    /// pre-F2023 behavior.
    fn apply_leading_zero(&self, s: &str) -> String {
        if self.leading_zero != LeadingZeroMode::Suppress {
            return s.to_string();
        }
        let (sign, rest) = match s.as_bytes().first() {
            Some(b'+') | Some(b'-') => (&s[..1], &s[1..]),
            _ => ("", s),
        };
        match rest.strip_prefix("0.") {
            Some(frac) => format!("{}.{}", sign, frac),
            None => s.to_string(),
        }
    }

    /// Format a list of values according to the descriptors, producing an output string.
    pub fn format_values(&mut self, values: &[IoValue]) -> String {
        self.format_values_checked(values).unwrap_or_default()
    }

    pub fn format_values_checked(&mut self, values: &[IoValue]) -> Result<String, FormatError> {
        self.format_values_bytes_checked(values)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn format_values_bytes_checked(
        &mut self,
        values: &[IoValue],
    ) -> Result<Vec<u8>, FormatError> {
        let mut output = FormatOutput::new();
        let mut val_idx = 0;
        let descriptors = self.descriptors.clone();
        self.apply_descriptors(&descriptors, values, &mut val_idx, &mut output)?;
        if !values.is_empty() && !format_has_data_descriptor(&descriptors) {
            return Err(FormatError::InvalidFormat);
        }
        Ok(output.finish())
    }

    /// Format output records using Fortran format reversion. When the I/O list
    /// outlives one complete scan of the format, the next scan starts a new
    /// external record.
    pub fn format_values_reverting_checked(
        &mut self,
        values: &[IoValue],
    ) -> Result<String, FormatError> {
        self.format_values_reverting_bytes_checked(values)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn format_values_reverting_bytes_checked(
        &mut self,
        values: &[IoValue],
    ) -> Result<Vec<u8>, FormatError> {
        let mut output = FormatOutput::new();
        let mut val_idx = 0;
        let descriptors = self.descriptors.clone();
        if !values.is_empty() && !format_has_data_descriptor(&descriptors) {
            return Err(FormatError::InvalidFormat);
        }
        if values.is_empty() {
            self.apply_descriptors(&descriptors, values, &mut val_idx, &mut output)?;
            return Ok(output.finish());
        }

        let mut first_record = true;
        while val_idx < values.len() {
            if !first_record {
                output.new_record();
            }
            let before = val_idx;
            self.apply_descriptors(&descriptors, values, &mut val_idx, &mut output)?;
            if val_idx == before {
                return Err(FormatError::InvalidFormat);
            }
            first_record = false;
        }
        Ok(output.finish())
    }

    fn apply_descriptors(
        &mut self,
        descs: &[FormatDesc],
        values: &[IoValue],
        val_idx: &mut usize,
        output: &mut FormatOutput,
    ) -> Result<(), FormatError> {
        for desc in descs {
            match desc {
                // ---- Control descriptors ----
                FormatDesc::Skip { count } => {
                    output.advance(*count);
                }
                FormatDesc::Newline => {
                    output.new_record();
                }
                FormatDesc::Colon => {
                    if *val_idx >= values.len() {
                        return Ok(());
                    }
                }
                FormatDesc::Sign(mode) => {
                    self.sign_mode = *mode;
                }
                FormatDesc::ScaleFactor(k) => {
                    self.scale_factor = *k;
                }
                FormatDesc::BlankMode(_) => {} // input only
                FormatDesc::RoundingMode(mode) => {
                    self.round_mode = *mode;
                }
                FormatDesc::DecimalMode(sep) => {
                    self.decimal_sep = *sep;
                }
                FormatDesc::LeadingZero(mode) => {
                    self.leading_zero = *mode;
                }
                FormatDesc::DerivedType { .. } => {} // requires user-defined I/O — no-op for now
                FormatDesc::TabTo { position } => {
                    output.tab_to(*position);
                }
                FormatDesc::TabLeft { count } => {
                    output.tab_left(*count);
                }
                FormatDesc::TabRight { count } => {
                    output.advance(*count);
                }
                FormatDesc::LiteralString(s) => {
                    output.write(s.as_bytes());
                }

                // ---- Group repeat ----
                FormatDesc::Group {
                    repeat,
                    descriptors,
                } => {
                    for _ in 0..*repeat {
                        self.apply_descriptors(descriptors, values, val_idx, output)?;
                    }
                }
                FormatDesc::UnlimitedRepeat { descriptors } => {
                    while *val_idx < values.len() {
                        self.apply_descriptors(descriptors, values, val_idx, output)?;
                    }
                }

                // ---- Data descriptors ----
                _ => {
                    if *val_idx >= values.len() {
                        return Ok(());
                    }
                    let val = &values[*val_idx];
                    *val_idx += 1;
                    let formatted = self.format_value(desc, val)?;
                    output.write(&formatted);
                }
            }
        }

        Ok(())
    }

    fn format_value(&self, desc: &FormatDesc, val: &IoValue) -> Result<Vec<u8>, FormatError> {
        match (desc, val) {
            (FormatDesc::RealG { width, .. }, IoValue::Character(bytes)) => {
                Ok(fit_bytes(bytes, *width))
            }
            (FormatDesc::Character { width }, IoValue::Character(bytes)) => {
                if let Some(w) = width {
                    if *w > bytes.len() {
                        let mut out = vec![b' '; *w - bytes.len()];
                        out.extend_from_slice(bytes);
                        Ok(out)
                    } else {
                        Ok(bytes[..*w].to_vec())
                    }
                } else {
                    Ok(bytes.clone())
                }
            }
            (FormatDesc::CharTrimmed, IoValue::Character(bytes)) => {
                let end = bytes
                    .iter()
                    .rposition(|&b| b != b' ')
                    .map(|idx| idx + 1)
                    .unwrap_or(0);
                Ok(bytes[..end].to_vec())
            }
            _ => self
                .format_value_text(desc, val)
                .map(|text| text.into_bytes()),
        }
    }

    fn format_value_text(&self, desc: &FormatDesc, val: &IoValue) -> Result<String, FormatError> {
        match (desc, val) {
            // ---- Integer ----
            (FormatDesc::IntegerI { width, min_digits }, IoValue::Integer(v)) => {
                let s = if let Some(m) = min_digits {
                    let abs_s = format!("{}", v.unsigned_abs());
                    let padded = format!("{:0>width$}", abs_s, width = *m);
                    if *v < 0 {
                        format!("-{}", padded)
                    } else {
                        self.apply_sign(&padded, *v >= 0)
                    }
                } else {
                    self.apply_sign(&format!("{}", v.unsigned_abs()), *v >= 0)
                };
                Ok(fit_field(&s, *width))
            }
            (FormatDesc::IntegerB { width, min_digits }, IoValue::Integer(v)) => {
                let s = format_radix_integer(*v, *min_digits, 2, *width);
                Ok(fit_field(&s, *width))
            }
            (FormatDesc::IntegerO { width, min_digits }, IoValue::Integer(v)) => {
                let s = format_radix_integer(*v, *min_digits, 8, *width);
                Ok(fit_field(&s, *width))
            }
            (FormatDesc::IntegerZ { width, min_digits }, IoValue::Integer(v)) => {
                let s = format_radix_integer(*v, *min_digits, 16, *width);
                Ok(fit_field(&s, *width))
            }

            // ---- Real ----
            (FormatDesc::RealF { width, decimals }, IoValue::Real(v)) => {
                if let Some(s) = self.format_nonfinite(*v) {
                    return Ok(self.apply_decimal_sep(&fit_field(&s, *width)));
                }
                // kP scale factor: F format multiplies value by 10^k.
                let scaled = *v * 10f64.powi(self.scale_factor);
                let rounded = self.apply_explicit_rounding(scaled, *decimals);
                let s = self.apply_leading_zero(&self.format_fixed(rounded, *decimals));
                Ok(self.apply_decimal_sep(&fit_field(&s, *width)))
            }
            (
                FormatDesc::RealE {
                    width,
                    decimals,
                    exp_width,
                },
                IoValue::Real(v),
            ) => {
                if let Some(s) = self.format_nonfinite(*v) {
                    return Ok(self.apply_decimal_sep(&fit_field(&s, *width)));
                }
                let s =
                    self.apply_leading_zero(&self.format_e_style(*v, *decimals, *exp_width, 'E'));
                Ok(self.apply_decimal_sep(&fit_exponential_field(&s, *width)))
            }
            (
                FormatDesc::RealES {
                    width,
                    decimals,
                    exp_width,
                },
                IoValue::Real(v),
            ) => {
                if let Some(s) = self.format_nonfinite(*v) {
                    return Ok(self.apply_decimal_sep(&fit_field(&s, *width)));
                }
                // Scientific: mantissa in [1.0, 10.0). Equivalent to 1P,E.
                let s = self.format_es_style(*v, *decimals, *exp_width);
                Ok(self.apply_decimal_sep(&fit_field(&s, *width)))
            }
            (
                FormatDesc::RealEN {
                    width, decimals, ..
                },
                IoValue::Real(v),
            ) => {
                if let Some(s) = self.format_nonfinite(*v) {
                    return Ok(self.apply_decimal_sep(&fit_field(&s, *width)));
                }
                // Engineering: exponent is multiple of 3.
                let (mantissa, exp) = to_engineering(v.abs());
                let rounded = self.apply_explicit_rounding(mantissa, *decimals);
                let s = format!(
                    "{}{:.*}E{:+03}",
                    self.real_sign(*v),
                    *decimals,
                    rounded,
                    exp
                );
                Ok(self.apply_decimal_sep(&fit_field(&s, *width)))
            }
            (FormatDesc::RealD { width, decimals }, IoValue::Real(v)) => {
                if let Some(s) = self.format_nonfinite(*v) {
                    return Ok(self.apply_decimal_sep(&fit_field(&s, *width)));
                }
                let s = self.apply_leading_zero(&self.format_e_style(*v, *decimals, None, 'D'));
                Ok(self.apply_decimal_sep(&fit_exponential_field(&s, *width)))
            }
            (
                FormatDesc::RealG {
                    width,
                    decimals,
                    exp_width,
                },
                IoValue::Real(v),
            ) => {
                if let Some(s) = self.format_nonfinite(*v) {
                    return Ok(self.apply_decimal_sep(&fit_field(&s, *width)));
                }
                if *width == 0 && *decimals == 0 {
                    return Ok(self.apply_decimal_sep(&self.format_g0(*v)));
                }
                // G format: use F if magnitude fits, else E.
                let abs_v = v.abs();
                if abs_v == 0.0 || (abs_v >= 0.1 && abs_v < 10f64.powi(*decimals as i32)) {
                    let rounded = self.apply_explicit_rounding(*v, *decimals);
                    let s = self.apply_leading_zero(&self.format_fixed(rounded, *decimals));
                    Ok(self.apply_decimal_sep(&fit_field(&s, *width)))
                } else {
                    let s = self.format_e_style(*v, *decimals, *exp_width, 'E');
                    Ok(self.apply_decimal_sep(&fit_exponential_field(&s, *width)))
                }
            }
            (FormatDesc::RealG { width, .. }, IoValue::Integer(v)) => {
                let s = if *v < 0 {
                    format!("-{}", v.unsigned_abs())
                } else {
                    self.apply_sign(&v.unsigned_abs().to_string(), true)
                };
                Ok(fit_field(&s, *width))
            }
            (FormatDesc::RealG { width, .. }, IoValue::Logical(v)) => {
                let s = if *v { "T" } else { "F" };
                Ok(fit_field(s, *width))
            }
            (FormatDesc::RealG { width, .. }, IoValue::Character(bytes)) => {
                let s = String::from_utf8_lossy(bytes);
                Ok(fit_field(&s, *width))
            }
            (
                FormatDesc::RealEX {
                    width, decimals, ..
                },
                IoValue::Real(v),
            ) => {
                if let Some(s) = self.format_nonfinite(*v) {
                    return Ok(self.apply_decimal_sep(&fit_field(&s, *width)));
                }
                // Hex-significand: use %a-like format. Rust doesn't have this natively.
                let s = format!("{:.*E}", *decimals, v); // fallback to E format
                Ok(self.apply_decimal_sep(&fit_field(&s, *width)))
            }

            // ---- Logical ----
            (FormatDesc::Logical { width }, IoValue::Logical(v)) => {
                let s = if *v { "T" } else { "F" };
                Ok(fit_field(s, *width))
            }

            // ---- Character ----
            (FormatDesc::Character { width }, IoValue::Character(bytes)) => {
                let s = String::from_utf8_lossy(bytes);
                if let Some(w) = width {
                    if *w > s.len() {
                        Ok(format!("{:>width$}", s, width = *w))
                    } else {
                        Ok(s[..*w].to_string())
                    }
                } else {
                    Ok(s.into_owned())
                }
            }
            // AT (F2023): character output trimmed to len_trim — the
            // value with trailing blanks removed, no field width.
            (FormatDesc::CharTrimmed, IoValue::Character(bytes)) => {
                let s = String::from_utf8_lossy(bytes);
                Ok(s.trim_end_matches(' ').to_string())
            }

            _ => Err(FormatError::TypeMismatch),
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

    /// Apply non-default rounding modes before decimal formatting. The default
    /// paths leave rounding to Rust's formatter to avoid double rounding values
    /// that need to round-trip through text.
    fn apply_explicit_rounding(&self, v: f64, decimals: usize) -> f64 {
        match self.round_mode {
            RoundMode::Compatible | RoundMode::ProcessorDefined => v,
            _ => self.apply_rounding(v, decimals),
        }
    }

    /// Apply rounding mode to a value at the given number of decimal places.
    fn apply_rounding(&self, v: f64, decimals: usize) -> f64 {
        let factor = 10f64.powi(decimals as i32);
        let scaled = v * factor;
        match self.round_mode {
            RoundMode::Up => scaled.ceil() / factor,
            RoundMode::Down => scaled.floor() / factor,
            RoundMode::Zero => scaled.trunc() / factor,
            RoundMode::Nearest => {
                // IEEE 754 round-to-nearest-even (banker's rounding).
                let rounded = scaled.round();
                // Check for exact halfway: round to even.
                if (scaled - scaled.floor() - 0.5).abs() < 1e-15 {
                    if rounded as i64 % 2 != 0 {
                        (rounded - scaled.signum()) / factor
                    } else {
                        rounded / factor
                    }
                } else {
                    rounded / factor
                }
            }
            RoundMode::Compatible => {
                // Round half away from zero (standard mathematical rounding).
                (scaled + 0.5 * scaled.signum()).trunc() / factor
            }
            RoundMode::ProcessorDefined => {
                // Use Rust's default (round-half-to-even).
                scaled.round() / factor
            }
        }
    }

    /// Format a fixed-point number (for F and G-as-F).
    fn format_fixed(&self, v: f64, decimals: usize) -> String {
        let sign = self.real_sign(v);
        let digits = if decimals == 0 {
            format!("{:.0}.", v.abs())
        } else {
            format!("{:.*}", decimals, v.abs())
        };
        format!("{sign}{digits}")
    }

    fn real_sign(&self, v: f64) -> &'static str {
        if v.is_sign_negative() {
            "-"
        } else if matches!(self.sign_mode, SignMode::Plus) {
            "+"
        } else {
            ""
        }
    }

    fn format_g0(&self, v: f64) -> String {
        if v == 0.0 {
            return format!("{}0.00000000", self.real_sign(v));
        }

        let abs_v = v.abs();
        if (0.1..1.0e6).contains(&abs_v) {
            let digits_before_decimal = abs_v.log10().floor().max(0.0) as usize + 1;
            let decimals = 9usize.saturating_sub(digits_before_decimal).max(1);
            self.format_fixed(v, decimals)
        } else {
            self.format_e_style(v, 8, Some(2), 'E')
        }
    }

    fn format_nonfinite(&self, v: f64) -> Option<String> {
        if v.is_nan() {
            Some("NaN".to_string())
        } else if v.is_infinite() {
            let sign = if v.is_sign_negative() {
                "-"
            } else if matches!(self.sign_mode, SignMode::Plus) {
                "+"
            } else {
                ""
            };
            Some(format!("{}Inf", sign))
        } else {
            None
        }
    }

    /// Format in E/D style with scale factor applied.
    ///
    /// Fortran kP with E format: the mantissa is multiplied by 10^k,
    /// and the exponent is decreased by k. With 0P (default), the mantissa
    /// is in [0.1, 1.0) — Fortran's convention, not C's.
    fn e_fractional_digits(&self, decimals: usize) -> usize {
        if self.scale_factor > 0 {
            decimals
                .saturating_add(1)
                .saturating_sub(self.scale_factor as usize)
        } else {
            decimals
        }
    }

    fn format_e_style(
        &self,
        v: f64,
        decimals: usize,
        exp_width: Option<usize>,
        exp_char: char,
    ) -> String {
        if matches!(
            self.round_mode,
            RoundMode::Compatible | RoundMode::ProcessorDefined
        ) && self.scale_factor == 0
        {
            return self.format_e_style_default(v, decimals, exp_width, exp_char);
        }

        let fractional_digits = self.e_fractional_digits(decimals);
        if v == 0.0 {
            let ew = exp_width.unwrap_or(2);
            let sign = self.real_sign(v);
            let zeros_before_decimal = "0".repeat((self.scale_factor.max(1)) as usize);
            let zeros_after_decimal = "0".repeat(fractional_digits);
            return format!(
                "{}{}.{}{}{:+0ew$}",
                sign,
                zeros_before_decimal,
                zeros_after_decimal,
                exp_char,
                0,
                ew = ew + 1
            );
        }

        let abs_v = v.abs();
        let base_exp = abs_v.log10().floor() as i32;
        // Fortran default (0P): mantissa in [0.1, 1.0), so exponent = base_exp + 1.
        let mut fort_exp = base_exp + 1 - self.scale_factor;
        let mantissa = abs_v / 10f64.powi(base_exp + 1 - self.scale_factor);
        let mut rounded = self.apply_explicit_rounding(mantissa, fractional_digits);
        let carry_threshold =
            10f64.powi(self.scale_factor) - 0.5 * 10f64.powi(-(fractional_digits as i32));
        if self.scale_factor > 0 && rounded >= carry_threshold {
            rounded /= 10.0;
            fort_exp += 1;
        }

        let ew = exp_width.unwrap_or(2);
        let sign = if v < 0.0 {
            "-"
        } else if matches!(self.sign_mode, SignMode::Plus) {
            "+"
        } else {
            ""
        };
        format!(
            "{}{:.*}{}{:+0ew$}",
            sign,
            fractional_digits,
            rounded,
            exp_char,
            fort_exp,
            ew = ew + 1
        )
    }

    fn format_e_style_default(
        &self,
        v: f64,
        decimals: usize,
        exp_width: Option<usize>,
        exp_char: char,
    ) -> String {
        if v == 0.0 {
            let ew = exp_width.unwrap_or(2);
            return format!(
                "0.{:0>d$}{}{:+0ew$}",
                "",
                exp_char,
                0,
                d = decimals,
                ew = ew + 1
            );
        }

        let sci_decimals = decimals.saturating_sub(1);
        let raw = format!("{:.*E}", sci_decimals, v.abs());
        let Some(pos) = raw.find('E') else {
            return raw;
        };
        let raw_mantissa = &raw[..pos];
        let raw_exp = raw[pos + 1..].parse::<i32>().unwrap_or(0);
        let sign = if v < 0.0 {
            "-"
        } else if matches!(self.sign_mode, SignMode::Plus) {
            "+"
        } else {
            ""
        };

        let mut digits: String = raw_mantissa.chars().filter(|c| *c != '.').collect();
        if digits.len() < decimals {
            digits.extend(std::iter::repeat_n('0', decimals - digits.len()));
        }
        let mantissa = if decimals == 0 {
            format!("{sign}0")
        } else {
            format!("{sign}0.{}", &digits[..decimals])
        };

        let ew = exp_width.unwrap_or(2);
        format!("{}{}{:+0ew$}", mantissa, exp_char, raw_exp + 1, ew = ew + 1)
    }

    /// Format in ES style (scientific): mantissa in [1.0, 10.0).
    fn format_es_style(&self, v: f64, decimals: usize, exp_width: Option<usize>) -> String {
        if matches!(
            self.round_mode,
            RoundMode::Compatible | RoundMode::ProcessorDefined
        ) {
            let raw = if v.is_sign_positive() && matches!(self.sign_mode, SignMode::Plus) {
                format!("{:+.*E}", decimals, v)
            } else {
                format!("{:.*E}", decimals, v)
            };
            return pad_exponent_width(&raw, exp_width, 'E');
        }

        if v == 0.0 {
            let ew = exp_width.unwrap_or(2);
            return format!("0.{:0>d$}E{:+0ew$}", "", 0, d = decimals, ew = ew + 1);
        }

        let abs_v = v.abs();
        let base_exp = abs_v.log10().floor() as i32;
        let mantissa = abs_v / 10f64.powi(base_exp);
        let rounded = self.apply_explicit_rounding(mantissa, decimals);

        let ew = exp_width.unwrap_or(2);
        let sign = if v < 0.0 {
            "-"
        } else if matches!(self.sign_mode, SignMode::Plus) {
            "+"
        } else {
            ""
        };
        format!(
            "{}{:.*}E{:+0ew$}",
            sign,
            decimals,
            rounded,
            base_exp,
            ew = ew + 1
        )
    }

    /// Replace '.' with ',' when decimal mode is DC (comma).
    fn apply_decimal_sep(&self, s: &str) -> String {
        match self.decimal_sep {
            DecimalSep::Point => s.to_string(),
            DecimalSep::Comma => s.replace('.', ","),
        }
    }
}

struct FormatOutput {
    bytes: Vec<u8>,
    record: Vec<u8>,
    pos: usize,
    high_water: usize,
}

impl FormatOutput {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            record: Vec::new(),
            pos: 0,
            high_water: 0,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let end = self.pos.saturating_add(bytes.len());
        if self.record.len() < end {
            self.record.resize(end, b' ');
        }
        self.record[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
        self.high_water = self.high_water.max(end);
    }

    fn advance(&mut self, count: usize) {
        self.pos = self.pos.saturating_add(count);
    }

    fn tab_to(&mut self, position: usize) {
        self.pos = position.saturating_sub(1);
    }

    fn tab_left(&mut self, count: usize) {
        self.pos = self.pos.saturating_sub(count);
    }

    fn new_record(&mut self) {
        self.flush_record();
        self.bytes.push(b'\n');
    }

    fn flush_record(&mut self) {
        self.bytes
            .extend_from_slice(&self.record[..self.high_water]);
        self.record.clear();
        self.pos = 0;
        self.high_water = 0;
    }

    fn finish(mut self) -> Vec<u8> {
        self.flush_record();
        self.bytes
    }
}

fn format_radix_integer(
    value: i128,
    min_digits: Option<usize>,
    radix: u32,
    width: usize,
) -> String {
    if value < 0 && radix == 16 {
        let digits = min_digits.unwrap_or(width).max(1);
        let bit_width = digits.saturating_mul(4).min(128);
        let mask = if bit_width == 128 {
            u128::MAX
        } else {
            (1u128 << bit_width) - 1
        };
        return format!("{:0>width$X}", (value as u128) & mask, width = digits);
    }

    let digits = match radix {
        2 => format!("{:b}", value.unsigned_abs()),
        8 => format!("{:o}", value.unsigned_abs()),
        16 => format!("{:X}", value.unsigned_abs()),
        _ => unreachable!("unsupported radix"),
    };
    let padded = if let Some(min_digits) = min_digits {
        format!("{:0>width$}", digits, width = min_digits)
    } else {
        digits
    };
    if value < 0 {
        format!("-{}", padded)
    } else {
        padded
    }
}

fn fit_field(s: &str, width: usize) -> String {
    if width == 0 {
        s.to_string()
    } else if s.len() > width {
        "*".repeat(width)
    } else {
        format!("{:>width$}", s, width = width)
    }
}

fn fit_bytes(bytes: &[u8], width: usize) -> Vec<u8> {
    if width == 0 {
        bytes.to_vec()
    } else if bytes.len() > width {
        vec![b'*'; width]
    } else {
        let mut out = vec![b' '; width - bytes.len()];
        out.extend_from_slice(bytes);
        out
    }
}

fn fit_exponential_field(s: &str, width: usize) -> String {
    if width != 0 && s.len() > width {
        if let Some(compact) = compact_exponential_leading_zero(s) {
            if compact.len() <= width {
                return fit_field(&compact, width);
            }
        }
    }
    fit_field(s, width)
}

fn compact_exponential_leading_zero(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix("0.") {
        Some(format!(".{rest}"))
    } else if let Some(rest) = s.strip_prefix("-0.") {
        Some(format!("-.{rest}"))
    } else {
        s.strip_prefix("+0.").map(|rest| format!("+.{rest}"))
    }
}

fn pad_exponent_width(raw: &str, exp_width: Option<usize>, exp_char: char) -> String {
    let Some(pos) = raw.find(['E', 'e']) else {
        return raw.to_string();
    };
    let mantissa = &raw[..pos];
    let exponent = &raw[pos + 1..];
    let (sign, digits) = match exponent.as_bytes().first().copied() {
        Some(b'-') => ('-', &exponent[1..]),
        Some(b'+') => ('+', &exponent[1..]),
        _ => ('+', exponent),
    };
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let width = exp_width.unwrap_or(2);
    format!("{mantissa}{exp_char}{sign}{digits:0>width$}", width = width)
}

fn format_has_data_descriptor(descs: &[FormatDesc]) -> bool {
    descs.iter().any(|desc| match desc {
        FormatDesc::IntegerI { .. }
        | FormatDesc::IntegerB { .. }
        | FormatDesc::IntegerO { .. }
        | FormatDesc::IntegerZ { .. }
        | FormatDesc::RealF { .. }
        | FormatDesc::RealE { .. }
        | FormatDesc::RealEN { .. }
        | FormatDesc::RealES { .. }
        | FormatDesc::RealEX { .. }
        | FormatDesc::RealD { .. }
        | FormatDesc::RealG { .. }
        | FormatDesc::Logical { .. }
        | FormatDesc::Character { .. }
        | FormatDesc::CharTrimmed => true,
        FormatDesc::Group { descriptors, .. } | FormatDesc::UnlimitedRepeat { descriptors } => {
            format_has_data_descriptor(descriptors)
        }
        _ => false,
    })
}

// ---- Helpers ----

fn to_engineering(v: f64) -> (f64, i32) {
    if v == 0.0 {
        return (0.0, 0);
    }
    let exp = (v.abs().log10().floor() as i32) / 3 * 3;
    let mantissa = v / 10f64.powi(exp);
    (mantissa, exp)
}

fn skip_spaces(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while chars.peek() == Some(&' ') {
        chars.next();
    }
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
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
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
        if c == '(' {
            depth += 1;
        }
        if c == ')' {
            depth -= 1;
            if depth == 0 {
                break;
            }
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
        assert!(matches!(
            descs[0],
            FormatDesc::IntegerI {
                width: 5,
                min_digits: None
            }
        ));
        assert!(matches!(
            descs[1],
            FormatDesc::RealF {
                width: 10,
                decimals: 3
            }
        ));
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
        } else {
            panic!("expected literal");
        }
    }

    #[test]
    fn parse_es_en_format() {
        let descs = parse_format("(ES15.8, EN12.3)");
        assert!(matches!(
            descs[0],
            FormatDesc::RealES {
                width: 15,
                decimals: 8,
                ..
            }
        ));
        assert!(matches!(
            descs[1],
            FormatDesc::RealEN {
                width: 12,
                decimals: 3,
                ..
            }
        ));
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
        let out = engine.format_values(&[IoValue::Real(1.23456)]);
        assert_eq!(out, "   1.235");
    }

    #[test]
    fn format_real_f_zero_decimals_keeps_decimal_point() {
        let descs = parse_format("(F4.0)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.0)]);
        assert_eq!(out, "  1.");
    }

    #[test]
    fn format_real_sign_plus() {
        let descs = parse_format("(SP,F6.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.0)]);
        assert_eq!(out, " +1.00");
    }

    #[test]
    fn format_real_overflow_uses_stars() {
        let descs = parse_format("(F6.2,1X,F6.3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(-100.0), IoValue::Real(1000.0)]);
        assert_eq!(out, "****** ******");
    }

    #[test]
    fn format_exponential_omits_leading_zero_to_fit_width() {
        let descs = parse_format("(E7.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(289.0)]);
        assert_eq!(out, ".29E+03");
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
    fn format_tab_descriptors_overlay_without_truncation() {
        let out = FormatEngine::new(parse_format("(A,T3,A)")).format_values(&[
            IoValue::Character(b"abcdef".to_vec()),
            IoValue::Character(b"XY".to_vec()),
        ]);
        assert_eq!(out, "abXYef");

        let out = FormatEngine::new(parse_format("(A,TL3,A)")).format_values(&[
            IoValue::Character(b"abcdef".to_vec()),
            IoValue::Character(b"XY".to_vec()),
        ]);
        assert_eq!(out, "abcXYf");

        let out = FormatEngine::new(parse_format("(A,TR3,A)")).format_values(&[
            IoValue::Character(b"ab".to_vec()),
            IoValue::Character(b"Z".to_vec()),
        ]);
        assert_eq!(out, "ab   Z");
    }

    #[test]
    fn format_trailing_x_only_moves_position() {
        let out =
            FormatEngine::new(parse_format("(I0,1X)")).format_values(&[IoValue::Integer(5)]);
        assert_eq!(out, "5");

        let out = FormatEngine::new(parse_format("(I0,1X,I0)"))
            .format_values(&[IoValue::Integer(5), IoValue::Integer(6)]);
        assert_eq!(out, "5 6");

        let out = FormatEngine::new(parse_format("(3(I0,1X))")).format_values(&[
            IoValue::Integer(1),
            IoValue::Integer(2),
            IoValue::Integer(3),
        ]);
        assert_eq!(out, "1 2 3");
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
    fn format_hex_negative_uses_requested_twos_complement_width() {
        let descs = parse_format("(Z16.16)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(-1)]);
        assert_eq!(out, "FFFFFFFFFFFFFFFF");
    }

    #[test]
    fn format_octal_integer() {
        let descs = parse_format("(O6)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(255)]);
        assert_eq!(out, "   377");
    }

    #[test]
    fn format_octal_integer_with_min_digits() {
        let descs = parse_format("(O4.4)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(18)]);
        assert_eq!(out, "0022");
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
            IoValue::Integer(1),
            IoValue::Integer(2),
            IoValue::Integer(3),
        ]);
        assert_eq!(out, "  1,  2,  3,");
    }

    #[test]
    fn parse_dc_dp_decimal_mode() {
        let descs = parse_format("(DC, F8.3, DP, F8.3)");
        assert_eq!(descs.len(), 4);
        assert!(matches!(
            descs[0],
            FormatDesc::DecimalMode(DecimalSep::Comma)
        ));
        assert!(matches!(
            descs[2],
            FormatDesc::DecimalMode(DecimalSep::Point)
        ));
    }

    #[test]
    fn parse_dt_derived_type() {
        let descs = parse_format("(DT'mytype')");
        assert_eq!(descs.len(), 1);
        if let FormatDesc::DerivedType { ref type_name } = descs[0] {
            assert_eq!(type_name, "mytype");
        } else {
            panic!("expected DerivedType, got {:?}", descs[0]);
        }
    }

    #[test]
    fn parse_dt_no_name() {
        let descs = parse_format("(DT)");
        assert_eq!(descs.len(), 1);
        if let FormatDesc::DerivedType { ref type_name } = descs[0] {
            assert_eq!(type_name, "");
        } else {
            panic!("expected DerivedType");
        }
    }

    #[test]
    fn parse_rounding_modes() {
        let descs = parse_format("(RU, F8.3, RD, F8.3, RZ, F8.3, RN, F8.3, RC, F8.3, RP, F8.3)");
        assert!(matches!(descs[0], FormatDesc::RoundingMode(RoundMode::Up)));
        assert!(matches!(
            descs[2],
            FormatDesc::RoundingMode(RoundMode::Down)
        ));
        assert!(matches!(
            descs[4],
            FormatDesc::RoundingMode(RoundMode::Zero)
        ));
        assert!(matches!(
            descs[6],
            FormatDesc::RoundingMode(RoundMode::Nearest)
        ));
        assert!(matches!(
            descs[8],
            FormatDesc::RoundingMode(RoundMode::Compatible)
        ));
        assert!(matches!(
            descs[10],
            FormatDesc::RoundingMode(RoundMode::ProcessorDefined)
        ));
    }

    #[test]
    fn parse_leading_zero_modes() {
        let descs = parse_format("(LZ, F6.3, LZS, F6.3, LZP, F6.3)");
        assert!(matches!(
            descs[0],
            FormatDesc::LeadingZero(LeadingZeroMode::Default)
        ));
        assert!(matches!(
            descs[2],
            FormatDesc::LeadingZero(LeadingZeroMode::Suppress)
        ));
        assert!(matches!(
            descs[4],
            FormatDesc::LeadingZero(LeadingZeroMode::Print)
        ));
    }

    #[test]
    fn leading_zero_from_specifier_maps_values() {
        // LEADING_ZERO= specifier values, case/space-insensitive. Anything
        // else (including PROCESSOR_DEFINED) is the processor default.
        assert_eq!(
            LeadingZeroMode::from_specifier("PRINT"),
            LeadingZeroMode::Print
        );
        assert_eq!(
            LeadingZeroMode::from_specifier("  suppress "),
            LeadingZeroMode::Suppress
        );
        assert_eq!(
            LeadingZeroMode::from_specifier("Processor_Defined"),
            LeadingZeroMode::Default
        );
        assert_eq!(
            LeadingZeroMode::from_specifier(""),
            LeadingZeroMode::Default
        );
        assert_eq!(
            LeadingZeroMode::from_specifier("garbage"),
            LeadingZeroMode::Default
        );
    }

    #[test]
    fn leading_zero_inquire_str_round_trips() {
        assert_eq!(LeadingZeroMode::Print.inquire_str(), "PRINT");
        assert_eq!(LeadingZeroMode::Suppress.inquire_str(), "SUPPRESS");
        assert_eq!(LeadingZeroMode::Default.inquire_str(), "PROCESSOR_DEFINED");
    }

    #[test]
    fn connection_mode_seeds_engine_without_format_descriptor() {
        // set_leading_zero models the OPEN/WRITE connection mode: a plain
        // (F6.3) with no LZ descriptor honors the seeded mode, and an
        // explicit LZP descriptor in the format overrides it mid-string.
        let mut engine = FormatEngine::new(parse_format("(F6.3)"));
        engine.set_leading_zero(LeadingZeroMode::Suppress);
        assert_eq!(engine.format_values(&[IoValue::Real(0.25)]).trim(), ".250");

        let mut overridden = FormatEngine::new(parse_format("(LZP, F6.3)"));
        overridden.set_leading_zero(LeadingZeroMode::Suppress);
        assert_eq!(
            overridden.format_values(&[IoValue::Real(0.25)]).trim(),
            "0.250"
        );
    }

    #[test]
    fn parse_lz_not_confused_with_logical_or_sign() {
        // LZS must tokenize as one descriptor, not LZ + S(ign default);
        // bare L is still logical.
        let descs = parse_format("(LZS, L2)");
        assert!(matches!(
            descs[0],
            FormatDesc::LeadingZero(LeadingZeroMode::Suppress)
        ));
        assert!(matches!(descs[1], FormatDesc::Logical { width: 2 }));
    }

    #[test]
    fn parse_at_descriptor_not_tab() {
        // AT is the trimmed-character descriptor, distinct from A and
        // from a T position descriptor.
        let descs = parse_format("(1X,AT,F4.1)");
        assert!(matches!(descs[0], FormatDesc::Skip { .. }));
        assert!(matches!(descs[1], FormatDesc::CharTrimmed));
        assert!(matches!(descs[2], FormatDesc::RealF { .. }));
        let plain = parse_format("(A4)");
        assert!(matches!(plain[0], FormatDesc::Character { width: Some(4) }));
    }

    #[test]
    fn format_leading_zero_suppress() {
        // LZS drops the leading zero; default/LZP keep it.
        let s =
            FormatEngine::new(parse_format("(LZS, F6.3)")).format_values(&[IoValue::Real(0.25)]);
        assert_eq!(s.trim(), ".250");
        let neg =
            FormatEngine::new(parse_format("(LZS, F7.3)")).format_values(&[IoValue::Real(-0.25)]);
        assert_eq!(neg.trim(), "-.250");
        let def = FormatEngine::new(parse_format("(F6.3)")).format_values(&[IoValue::Real(0.25)]);
        assert_eq!(def.trim(), "0.250");
        let lzp =
            FormatEngine::new(parse_format("(LZP, F6.3)")).format_values(&[IoValue::Real(0.25)]);
        assert_eq!(lzp.trim(), "0.250");
    }

    #[test]
    fn format_leading_zero_suppress_only_below_one() {
        // No leading zero to drop when |value| >= 1.
        let s =
            FormatEngine::new(parse_format("(LZS, F7.3)")).format_values(&[IoValue::Real(10.25)]);
        assert_eq!(s.trim(), "10.250");
    }

    #[test]
    fn format_leading_zero_suppress_exponential() {
        // E-format mantissa leading zero is suppressed too.
        let s =
            FormatEngine::new(parse_format("(LZS, E10.3)")).format_values(&[IoValue::Real(0.25)]);
        assert!(s.contains(".250"), "got {s}");
        assert!(
            !s.trim().starts_with('0'),
            "leading zero not suppressed: {s}"
        );
    }

    #[test]
    fn format_at_trims_trailing_blanks() {
        let s = FormatEngine::new(parse_format("(AT)"))
            .format_values(&[IoValue::Character(b"hi   ".to_vec())]);
        assert_eq!(s, "hi");
        let blank = FormatEngine::new(parse_format("(AT)"))
            .format_values(&[IoValue::Character(b"     ".to_vec())]);
        assert_eq!(blank, "");
    }

    #[test]
    fn parse_negative_scale_factor() {
        let descs = parse_format("(-2P, E15.8)");
        assert!(matches!(descs[0], FormatDesc::ScaleFactor(-2)));
    }

    #[test]
    fn parse_positive_scale_factor() {
        let descs = parse_format("(3P, E15.8)");
        assert!(matches!(descs[0], FormatDesc::ScaleFactor(3)));
    }

    #[test]
    fn format_decimal_comma() {
        let descs = parse_format("(DC, F8.3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.23)]);
        assert!(out.contains(','), "expected comma in output: {}", out);
        assert!(!out.contains('.'), "expected no dot in output: {}", out);
    }

    #[test]
    fn format_rounding_up() {
        let descs = parse_format("(RU, F6.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.234)]);
        // RU rounds 1.234 up to 1.24 at 2 decimals.
        assert_eq!(out.trim(), "1.24");
    }

    #[test]
    fn format_rounding_down() {
        let descs = parse_format("(RD, F6.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.236)]);
        // RD rounds 1.236 down to 1.23 at 2 decimals.
        assert_eq!(out.trim(), "1.23");
    }

    #[test]
    fn format_rounding_zero() {
        let descs = parse_format("(RZ, F6.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(-1.236)]);
        // RZ truncates toward zero: -1.236 → -1.23.
        assert_eq!(out.trim(), "-1.23");
    }

    #[test]
    fn format_scale_factor_f() {
        // 2P with F: multiplies value by 10^2 before formatting.
        let descs = parse_format("(2P, F8.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.5)]);
        // 1.5 * 100 = 150.00
        assert_eq!(out.trim(), "150.00");
    }

    #[test]
    fn format_scale_factor_e_reduces_fractional_digits() {
        let descs = parse_format("(2P,E12.4,1X,E12.4,1X,E12.4)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[
            IoValue::Real(12345.678),
            IoValue::Real(0.0),
            IoValue::Real(99999.99),
        ]);
        assert_eq!(out, "  12.346E+03   00.000E+00   10.000E+04");
    }

    #[test]
    fn format_g0_nonfinite_reals() {
        let descs = parse_format("(G0, 1X, G0, 1X, G0)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[
            IoValue::Real(f64::INFINITY),
            IoValue::Real(f64::NEG_INFINITY),
            IoValue::Real(f64::NAN),
        ]);
        assert_eq!(out, "Inf -Inf NaN");
    }

    #[test]
    fn format_g0_finite_reals() {
        let descs = parse_format("(G0,1X,G0)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(100.0), IoValue::Real(1.0)]);
        assert_eq!(out, "100.000000 1.00000000");
    }

    #[test]
    fn format_g0_integer_and_logical() {
        let descs = parse_format("(G0,1X,G0,1X,SP,G0)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[
            IoValue::Integer(-1026191),
            IoValue::Logical(true),
            IoValue::Integer(124787),
        ]);
        assert_eq!(out, "-1026191 T +124787");
    }

    #[test]
    fn format_checked_rejects_values_without_data_descriptor() {
        let descs = parse_format("(1X)");
        let mut engine = FormatEngine::new(descs);
        assert_eq!(
            engine.format_values_checked(&[IoValue::Logical(false)]),
            Err(FormatError::InvalidFormat)
        );
    }

    #[test]
    fn format_reversion_starts_new_records() {
        let descs = parse_format("(A)");
        let mut engine = FormatEngine::new(descs);
        let out = engine
            .format_values_reverting_checked(&[
                IoValue::Character(b"abc".to_vec()),
                IoValue::Character(b"def".to_vec()),
                IoValue::Character(b"ghi".to_vec()),
            ])
            .unwrap();
        assert_eq!(out, "abc\ndef\nghi");
    }

    #[test]
    fn format_reversion_reuses_multi_descriptor_format() {
        let descs = parse_format("(I2,1X,I2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine
            .format_values_reverting_checked(&[
                IoValue::Integer(1),
                IoValue::Integer(2),
                IoValue::Integer(3),
                IoValue::Integer(4),
            ])
            .unwrap();
        assert_eq!(out, " 1  2\n 3  4");
    }

    #[test]
    fn format_es_dp_precision_roundtrips() {
        let value = -1.9972267279387788e-1f64;
        let descs = parse_format("(ES24.16E3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(value)]);
        assert_eq!(out.trim().parse::<f64>().unwrap(), value);
    }

    #[test]
    fn format_exponential_nonfinite_reals() {
        let descs = parse_format("(E8.1, 1X, ES8.1, 1X, EN8.1, 1X, D8.1)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[
            IoValue::Real(f64::INFINITY),
            IoValue::Real(f64::INFINITY),
            IoValue::Real(f64::INFINITY),
            IoValue::Real(f64::INFINITY),
        ]);
        assert_eq!(out.split_whitespace().collect::<Vec<_>>(), vec!["Inf"; 4]);
    }

    #[test]
    fn format_e_huge_real_has_roundtripping_mantissa() {
        let descs = parse_format("(E30.18E3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(f64::MAX)]);
        let text = out.trim();
        assert!(
            text.starts_with("0.179"),
            "expected a nonzero Fortran-E mantissa for huge real, got {text}"
        );
        assert!(
            text.ends_with("E+309"),
            "expected Fortran-E exponent one larger than scientific exponent, got {text}"
        );
        assert_eq!(text.parse::<f64>().unwrap(), f64::MAX);
    }

    #[test]
    fn format_d_descriptor() {
        let descs = parse_format("(D12.5)");
        assert!(matches!(
            descs[0],
            FormatDesc::RealD {
                width: 12,
                decimals: 5
            }
        ));
    }

    #[test]
    fn format_d_vs_dc() {
        // D12.5 is a real descriptor; DC is decimal comma mode.
        let descs = parse_format("(DC, D12.5)");
        assert!(matches!(
            descs[0],
            FormatDesc::DecimalMode(DecimalSep::Comma)
        ));
        assert!(matches!(
            descs[1],
            FormatDesc::RealD {
                width: 12,
                decimals: 5
            }
        ));
    }

    #[test]
    fn format_integer16_full_width() {
        let descs = parse_format("(I40)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(
            170141183460469231731687303715884105727i128,
        )]);
        assert_eq!(out, " 170141183460469231731687303715884105727");
    }
}
