//! Fortran FORMAT engine — complete implementation of all edit descriptors.
//!
//! Parses format strings like '(I5, F10.3, A, 2X, /, ES15.8)' into
//! descriptors and applies them to I/O values. Supports the full
//! Fortran standard set including repeat counts, group repeat,
//! unlimited repeat, scale factors, and all data/control descriptors.

use std::sync::Arc;

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
    /// DT: user-defined derived-type I/O (F2003). `type_name` is the optional
    /// character literal appended to `DT`; `v_list` preserves the signed
    /// default-integer literal values supplied by the edit descriptor.
    DerivedType { type_name: String, v_list: Vec<i32> },

    // ---- Character string descriptors ----
    /// Literal string in format: 'text' or "text".
    LiteralString(String),

    // ---- Grouping ----
    /// Repeated group: n(...).
    Group {
        repeat: usize,
        descriptors: Vec<FormatDesc>,
        /// Explicit parentheses establish a format reversion point;
        /// synthetic wrappers for repeated data descriptors do not.
        is_reversion_point: bool,
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

/// Maximum explicit parenthesis nesting accepted in a dynamic format.
///
/// Parsing is otherwise linear in the format string and repeat counts stay
/// structural, so hostile numeric counts cannot expand the descriptor vector.
/// A depth limit is still required because groups are parsed recursively.
const MAX_FORMAT_NESTING: usize = 64;

/// Parse a complete Fortran format specification into descriptors.
///
/// Dynamic format values include their outer parentheses. Rejecting a missing
/// or unmatched parenthesis here prevents malformed text from being
/// "repaired" into a different, consuming format.
pub fn parse_format(fmt: &str) -> Result<Vec<FormatDesc>, FormatError> {
    FormatParser::new(fmt.trim()).parse()
}

struct FormatParser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> FormatParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            chars: input.chars().peekable(),
        }
    }

    fn parse(mut self) -> Result<Vec<FormatDesc>, FormatError> {
        self.skip_spaces();
        if self.chars.next() != Some('(') {
            return Err(FormatError::InvalidFormat);
        }

        let descriptors = self.parse_list(1)?;
        self.skip_spaces();
        if self.chars.next().is_some() {
            return Err(FormatError::InvalidFormat);
        }
        Ok(descriptors)
    }

    fn parse_list(&mut self, depth: usize) -> Result<Vec<FormatDesc>, FormatError> {
        if depth > MAX_FORMAT_NESTING {
            return Err(FormatError::InvalidFormat);
        }

        let mut result = Vec::new();
        let mut has_item = false;
        loop {
            self.skip_spaces();
            match self.chars.peek().copied() {
                None => return Err(FormatError::InvalidFormat),
                Some(')') => {
                    self.chars.next();
                    return Ok(result);
                }
                Some(',') => {
                    if !has_item {
                        return Err(FormatError::InvalidFormat);
                    }
                    self.chars.next();
                    self.skip_spaces();
                    match self.chars.peek().copied() {
                        Some(')') => {
                            self.chars.next();
                            return Ok(result);
                        }
                        Some(',') | None => return Err(FormatError::InvalidFormat),
                        _ => {}
                    }
                }
                _ => {}
            }

            let sign = match self.chars.peek().copied() {
                Some('-') => {
                    self.chars.next();
                    -1
                }
                Some('+') => {
                    self.chars.next();
                    1
                }
                _ => 0,
            };
            let repeat = self.parse_number()?;
            self.skip_spaces();
            let next = self
                .chars
                .peek()
                .copied()
                .ok_or(FormatError::InvalidFormat)?;

            match next {
                '(' => {
                    if sign != 0 || repeat == Some(0) {
                        return Err(FormatError::InvalidFormat);
                    }
                    self.chars.next();
                    let descriptors = self.parse_list(depth + 1)?;
                    result.push(FormatDesc::Group {
                        repeat: repeat.unwrap_or(1),
                        descriptors,
                        is_reversion_point: true,
                    });
                }
                '\'' | '"' => {
                    if sign != 0 || repeat == Some(0) {
                        return Err(FormatError::InvalidFormat);
                    }
                    let literal = FormatDesc::LiteralString(self.parse_string_literal(next)?);
                    Self::push_repeated(&mut result, literal, repeat.unwrap_or(1));
                }
                '/' => {
                    if sign != 0 || repeat == Some(0) {
                        return Err(FormatError::InvalidFormat);
                    }
                    self.chars.next();
                    Self::push_repeated(&mut result, FormatDesc::Newline, repeat.unwrap_or(1));
                }
                ':' => {
                    if sign != 0 || repeat.is_some() {
                        return Err(FormatError::InvalidFormat);
                    }
                    self.chars.next();
                    result.push(FormatDesc::Colon);
                }
                '*' => {
                    if sign != 0 || repeat.is_some() {
                        return Err(FormatError::InvalidFormat);
                    }
                    self.chars.next();
                    self.skip_spaces();
                    if self.chars.next() != Some('(') {
                        return Err(FormatError::InvalidFormat);
                    }
                    let descriptors = self.parse_list(depth + 1)?;
                    result.push(FormatDesc::UnlimitedRepeat { descriptors });
                }
                _ => {
                    let descriptor = self.parse_edit_descriptor(repeat, sign)?;
                    match repeat {
                        Some(0) if !matches!(descriptor, FormatDesc::ScaleFactor(_)) => {
                            return Err(FormatError::InvalidFormat);
                        }
                        Some(n) if n > 1 && Self::is_repeatable_data_descriptor(&descriptor) => {
                            result.push(FormatDesc::Group {
                                repeat: n,
                                descriptors: vec![descriptor],
                                is_reversion_point: false,
                            });
                        }
                        Some(_)
                            if !matches!(
                                descriptor,
                                FormatDesc::Skip { .. } | FormatDesc::ScaleFactor(_)
                            ) =>
                        {
                            return Err(FormatError::InvalidFormat);
                        }
                        _ => result.push(descriptor),
                    }
                }
            }
            has_item = true;
        }
    }

    fn push_repeated(result: &mut Vec<FormatDesc>, descriptor: FormatDesc, repeat: usize) {
        if repeat == 1 {
            result.push(descriptor);
        } else {
            result.push(FormatDesc::Group {
                repeat,
                descriptors: vec![descriptor],
                is_reversion_point: false,
            });
        }
    }

    fn is_repeatable_data_descriptor(descriptor: &FormatDesc) -> bool {
        matches!(
            descriptor,
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
                | FormatDesc::CharTrimmed
                | FormatDesc::DerivedType { .. }
        )
    }

    fn parse_edit_descriptor(
        &mut self,
        repeat: Option<usize>,
        sign: i32,
    ) -> Result<FormatDesc, FormatError> {
        let letter = self
            .chars
            .next()
            .ok_or(FormatError::InvalidFormat)?
            .to_ascii_uppercase();

        if sign != 0 && letter != 'P' {
            return Err(FormatError::InvalidFormat);
        }

        match letter {
            'I' => {
                let (width, min_digits) = self.parse_integer_widths()?;
                Ok(FormatDesc::IntegerI { width, min_digits })
            }
            'B' => match self.peek_uppercase() {
                Some('N') => {
                    self.chars.next();
                    Ok(FormatDesc::BlankMode(BlankInterpretation::Null))
                }
                Some('Z') => {
                    self.chars.next();
                    Ok(FormatDesc::BlankMode(BlankInterpretation::Zero))
                }
                _ => {
                    let (width, min_digits) = self.parse_integer_widths()?;
                    Ok(FormatDesc::IntegerB { width, min_digits })
                }
            },
            'O' => {
                let (width, min_digits) = self.parse_integer_widths()?;
                Ok(FormatDesc::IntegerO { width, min_digits })
            }
            'Z' => {
                let (width, min_digits) = self.parse_integer_widths()?;
                Ok(FormatDesc::IntegerZ { width, min_digits })
            }
            'F' => {
                let (width, decimals) = self.parse_required_real_widths()?;
                Ok(FormatDesc::RealF { width, decimals })
            }
            'E' => match self.peek_uppercase() {
                Some('N') => {
                    self.chars.next();
                    self.parse_exponential_widths(|width, decimals, exp_width| FormatDesc::RealEN {
                        width,
                        decimals,
                        exp_width,
                    })
                }
                Some('S') => {
                    self.chars.next();
                    self.parse_exponential_widths(|width, decimals, exp_width| FormatDesc::RealES {
                        width,
                        decimals,
                        exp_width,
                    })
                }
                Some('X') => {
                    self.chars.next();
                    self.parse_exponential_widths(|width, decimals, exp_width| FormatDesc::RealEX {
                        width,
                        decimals,
                        exp_width,
                    })
                }
                _ => {
                    self.parse_exponential_widths(|width, decimals, exp_width| FormatDesc::RealE {
                        width,
                        decimals,
                        exp_width,
                    })
                }
            },
            'D' => match self.peek_uppercase() {
                Some('C') => {
                    self.chars.next();
                    Ok(FormatDesc::DecimalMode(DecimalSep::Comma))
                }
                Some('P') => {
                    self.chars.next();
                    Ok(FormatDesc::DecimalMode(DecimalSep::Point))
                }
                Some('T') => {
                    self.chars.next();
                    let type_name = match self.chars.peek().copied() {
                        Some(quote @ ('\'' | '"')) => self.parse_string_literal(quote)?,
                        _ => String::new(),
                    };
                    self.skip_spaces();
                    let v_list = if self.chars.peek() == Some(&'(') {
                        self.parse_dt_v_list()?
                    } else {
                        Vec::new()
                    };
                    Ok(FormatDesc::DerivedType { type_name, v_list })
                }
                _ => {
                    let (width, decimals) = self.parse_required_real_widths()?;
                    Ok(FormatDesc::RealD { width, decimals })
                }
            },
            'G' => {
                let width = self.parse_required_number()?;
                if width == 0 && self.chars.peek() != Some(&'.') {
                    return Ok(FormatDesc::RealG {
                        width,
                        decimals: 0,
                        exp_width: None,
                    });
                }
                let (decimals, exp_width) = self.parse_decimal_and_exponent_widths()?;
                Ok(FormatDesc::RealG {
                    width,
                    decimals,
                    exp_width,
                })
            }
            'L' => {
                if self.peek_uppercase() == Some('Z') {
                    self.chars.next();
                    let mode = match self.peek_uppercase() {
                        Some('S') => {
                            self.chars.next();
                            LeadingZeroMode::Suppress
                        }
                        Some('P') => {
                            self.chars.next();
                            LeadingZeroMode::Print
                        }
                        _ => LeadingZeroMode::Default,
                    };
                    Ok(FormatDesc::LeadingZero(mode))
                } else {
                    Ok(FormatDesc::Logical {
                        width: self.parse_number()?.unwrap_or(1),
                    })
                }
            }
            'A' => {
                if self.peek_uppercase() == Some('T') {
                    self.chars.next();
                    Ok(FormatDesc::CharTrimmed)
                } else {
                    Ok(FormatDesc::Character {
                        width: self.parse_number()?,
                    })
                }
            }
            'X' => {
                let count = repeat.ok_or(FormatError::InvalidFormat)?;
                if count == 0 {
                    return Err(FormatError::InvalidFormat);
                }
                Ok(FormatDesc::Skip { count })
            }
            'T' => {
                let direction = self.peek_uppercase();
                let descriptor = match direction {
                    Some('L') => {
                        self.chars.next();
                        FormatDesc::TabLeft {
                            count: self.parse_positive_number()?,
                        }
                    }
                    Some('R') => {
                        self.chars.next();
                        FormatDesc::TabRight {
                            count: self.parse_positive_number()?,
                        }
                    }
                    _ => FormatDesc::TabTo {
                        position: self.parse_positive_number()?,
                    },
                };
                Ok(descriptor)
            }
            'S' => match self.peek_uppercase() {
                Some('P') => {
                    self.chars.next();
                    Ok(FormatDesc::Sign(SignMode::Plus))
                }
                Some('S') => {
                    self.chars.next();
                    Ok(FormatDesc::Sign(SignMode::Suppress))
                }
                _ => Ok(FormatDesc::Sign(SignMode::Default)),
            },
            'P' => {
                let magnitude = repeat.ok_or(FormatError::InvalidFormat)?;
                let magnitude = i64::try_from(magnitude).map_err(|_| FormatError::InvalidFormat)?;
                let signed = if sign < 0 { -magnitude } else { magnitude };
                let scale = i32::try_from(signed).map_err(|_| FormatError::InvalidFormat)?;
                Ok(FormatDesc::ScaleFactor(scale))
            }
            'R' => {
                let mode = match self.peek_uppercase() {
                    Some('U') => RoundMode::Up,
                    Some('D') => RoundMode::Down,
                    Some('Z') => RoundMode::Zero,
                    Some('N') => RoundMode::Nearest,
                    Some('C') => RoundMode::Compatible,
                    Some('P') => RoundMode::ProcessorDefined,
                    _ => return Err(FormatError::InvalidFormat),
                };
                self.chars.next();
                Ok(FormatDesc::RoundingMode(mode))
            }
            _ => Err(FormatError::InvalidFormat),
        }
    }

    fn parse_dt_v_list(&mut self) -> Result<Vec<i32>, FormatError> {
        if self.chars.next() != Some('(') {
            return Err(FormatError::InvalidFormat);
        }
        self.skip_spaces();
        if self.chars.peek() == Some(&')') {
            return Err(FormatError::InvalidFormat);
        }

        let mut values = Vec::new();
        loop {
            values.push(self.parse_signed_default_integer()?);
            self.skip_spaces();
            match self.chars.next() {
                Some(',') => {
                    self.skip_spaces();
                    if self.chars.peek() == Some(&')') {
                        return Err(FormatError::InvalidFormat);
                    }
                }
                Some(')') => return Ok(values),
                _ => return Err(FormatError::InvalidFormat),
            }
        }
    }

    fn parse_signed_default_integer(&mut self) -> Result<i32, FormatError> {
        self.skip_spaces();
        let sign = match self.chars.peek().copied() {
            Some('+') => {
                self.chars.next();
                1i128
            }
            Some('-') => {
                self.chars.next();
                -1i128
            }
            _ => 1i128,
        };
        self.skip_spaces();
        let magnitude = self.parse_required_number()? as i128;
        i32::try_from(sign * magnitude).map_err(|_| FormatError::InvalidFormat)
    }

    fn parse_integer_widths(&mut self) -> Result<(usize, Option<usize>), FormatError> {
        let width = self.parse_required_number()?;
        let min_digits = if self.chars.peek() == Some(&'.') {
            self.chars.next();
            Some(self.parse_required_number()?)
        } else {
            None
        };
        Ok((width, min_digits))
    }

    fn parse_required_real_widths(&mut self) -> Result<(usize, usize), FormatError> {
        let width = self.parse_required_number()?;
        if self.chars.next() != Some('.') {
            return Err(FormatError::InvalidFormat);
        }
        let decimals = self.parse_required_number()?;
        Ok((width, decimals))
    }

    fn parse_exponential_widths(
        &mut self,
        constructor: impl Fn(usize, usize, Option<usize>) -> FormatDesc,
    ) -> Result<FormatDesc, FormatError> {
        let (width, decimals) = self.parse_required_real_widths()?;
        let exp_width = self.parse_optional_exponent_width(false)?;
        Ok(constructor(width, decimals, exp_width))
    }

    fn parse_decimal_and_exponent_widths(&mut self) -> Result<(usize, Option<usize>), FormatError> {
        if self.chars.next() != Some('.') {
            return Err(FormatError::InvalidFormat);
        }
        let decimals = self.parse_required_number()?;
        let exp_width = self.parse_optional_exponent_width(true)?;
        Ok((decimals, exp_width))
    }

    fn parse_optional_exponent_width(
        &mut self,
        allow_zero: bool,
    ) -> Result<Option<usize>, FormatError> {
        if self.peek_uppercase() != Some('E') {
            return Ok(None);
        }
        self.chars.next();
        let width = self.parse_required_number()?;
        if width == 0 && !allow_zero {
            return Err(FormatError::InvalidFormat);
        }
        Ok(Some(width))
    }

    fn parse_positive_number(&mut self) -> Result<usize, FormatError> {
        let number = self.parse_required_number()?;
        if number == 0 {
            return Err(FormatError::InvalidFormat);
        }
        Ok(number)
    }

    fn parse_required_number(&mut self) -> Result<usize, FormatError> {
        self.parse_number()?.ok_or(FormatError::InvalidFormat)
    }

    fn parse_number(&mut self) -> Result<Option<usize>, FormatError> {
        let mut value = 0usize;
        let mut found = false;
        while let Some(digit) = self
            .chars
            .peek()
            .filter(|digit| digit.is_ascii_digit())
            .map(|digit| *digit as usize - '0' as usize)
        {
            self.chars.next();
            value = value
                .checked_mul(10)
                .and_then(|number| number.checked_add(digit))
                .ok_or(FormatError::InvalidFormat)?;
            found = true;
        }
        Ok(found.then_some(value))
    }

    fn parse_string_literal(&mut self, quote: char) -> Result<String, FormatError> {
        if self.chars.next() != Some(quote) {
            return Err(FormatError::InvalidFormat);
        }
        let mut literal = String::new();
        loop {
            let next = self.chars.next().ok_or(FormatError::InvalidFormat)?;
            if next != quote {
                literal.push(next);
                continue;
            }
            if self.chars.peek() == Some(&quote) {
                self.chars.next();
                literal.push(quote);
            } else {
                return Ok(literal);
            }
        }
    }

    fn peek_uppercase(&mut self) -> Option<char> {
        self.chars.peek().map(|c| c.to_ascii_uppercase())
    }

    fn skip_spaces(&mut self) {
        while self.chars.peek() == Some(&' ') {
            self.chars.next();
        }
    }
}

// ---- Format application (output) ----

/// An I/O value to be formatted.
pub enum IoValue {
    Integer(i128),
    Real(f64),
    Real32(f64),
    Logical(bool),
    Character(Vec<u8>),
}

/// Format engine state for applying descriptors to values.
pub struct FormatEngine {
    descriptors: Arc<[FormatDesc]>,
    sign_mode: SignMode,
    scale_factor: i32,
    round_mode: RoundMode,
    decimal_sep: DecimalSep,
    leading_zero: LeadingZeroMode,
}

impl FormatEngine {
    pub fn new(descriptors: Vec<FormatDesc>) -> Self {
        Self::from_shared(Arc::from(descriptors.into_boxed_slice()))
    }

    pub fn from_shared(descriptors: Arc<[FormatDesc]>) -> Self {
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
        let descriptors = Arc::clone(&self.descriptors);
        self.apply_descriptors(descriptors.as_ref(), values, &mut val_idx, &mut output)?;
        if !values.is_empty() && !format_has_data_descriptor(descriptors.as_ref()) {
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
        let descriptors = Arc::clone(&self.descriptors);
        let reversion_descriptors = format_reversion_descriptors(descriptors.as_ref());
        if !values.is_empty() && !format_has_data_descriptor(descriptors.as_ref()) {
            return Err(FormatError::InvalidFormat);
        }
        if values.is_empty() {
            self.apply_descriptors(descriptors.as_ref(), values, &mut val_idx, &mut output)?;
            return Ok(output.finish());
        }

        let mut first_record = true;
        let mut active_descriptors = descriptors.as_ref();
        while val_idx < values.len() {
            if !first_record {
                output.new_record();
            }
            let before = val_idx;
            self.apply_descriptors(active_descriptors, values, &mut val_idx, &mut output)?;
            if val_idx == before {
                return Err(FormatError::InvalidFormat);
            }
            active_descriptors = reversion_descriptors;
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
                FormatDesc::DerivedType { .. } => {
                    // Defined-I/O dispatch is performed by compiler lowering,
                    // not by the intrinsic-value format engine. Reaching a DT
                    // descriptor with an unconsumed intrinsic value is a type
                    // mismatch, never a successful zero-byte conversion.
                    if *val_idx < values.len() {
                        return Err(FormatError::TypeMismatch);
                    }
                }
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
                    ..
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
                    self.write_value(desc, val, output)?;
                }
            }
        }

        Ok(())
    }

    fn write_value(
        &self,
        desc: &FormatDesc,
        val: &IoValue,
        output: &mut FormatOutput,
    ) -> Result<(), FormatError> {
        match (desc, val) {
            (FormatDesc::RealG { width, .. }, IoValue::Character(bytes)) => {
                output.write_fitted(bytes, *width);
                Ok(())
            }
            (FormatDesc::Character { width }, IoValue::Character(bytes)) => {
                if let Some(w) = width {
                    output.write_fitted(bytes, *w);
                } else {
                    output.write(bytes);
                }
                Ok(())
            }
            (FormatDesc::CharTrimmed, IoValue::Character(bytes)) => {
                let end = bytes
                    .iter()
                    .rposition(|&b| b != b' ')
                    .map(|idx| idx + 1)
                    .unwrap_or(0);
                output.write(&bytes[..end]);
                Ok(())
            }
            _ => {
                let formatted = self.format_value(desc, val)?;
                output.write(&formatted);
                Ok(())
            }
        }
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
            (FormatDesc::RealF { width, decimals }, IoValue::Real(v) | IoValue::Real32(v)) => {
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
                IoValue::Real(v) | IoValue::Real32(v),
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
                IoValue::Real(v) | IoValue::Real32(v),
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
                IoValue::Real(v) | IoValue::Real32(v),
            ) => {
                if let Some(s) = self.format_nonfinite(*v) {
                    return Ok(self.apply_decimal_sep(&fit_field(&s, *width)));
                }
                // Engineering: exponent is multiple of 3.
                let (mantissa, mut exp) = to_engineering(v.abs());
                let rounded = self.apply_explicit_rounding(mantissa, *decimals);
                let mut mantissa_text = format!("{:.*}", *decimals, rounded);
                if rounded_mantissa_reached_upper_bound(&mantissa_text, "1000") {
                    mantissa_text = format!("{:.*}", *decimals, 1.0);
                    exp += 3;
                }
                let s = format!("{}{}E{:+03}", self.real_sign(*v), mantissa_text, exp);
                Ok(self.apply_decimal_sep(&fit_field(&s, *width)))
            }
            (FormatDesc::RealD { width, decimals }, IoValue::Real(v) | IoValue::Real32(v)) => {
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
                IoValue::Real32(v),
            ) => self.format_g_text(*v, 9, *width, *decimals, *exp_width),
            (
                FormatDesc::RealG {
                    width,
                    decimals,
                    exp_width,
                },
                IoValue::Real(v),
            ) => self.format_g_text(*v, 17, *width, *decimals, *exp_width),
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
                IoValue::Real(v) | IoValue::Real32(v),
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

    fn format_g_text(
        &self,
        v: f64,
        significant_digits: usize,
        width: usize,
        decimals: usize,
        exp_width: Option<usize>,
    ) -> Result<String, FormatError> {
        if let Some(s) = self.format_nonfinite(v) {
            return Ok(self.apply_decimal_sep(&fit_field(&s, width)));
        }
        if width == 0 && decimals == 0 {
            return Ok(self.apply_decimal_sep(&self.format_g0(v, significant_digits)));
        }

        // Preserve the separate minimal-width G0.d behavior. Nonzero-width
        // Gw.d follows the significant-digit selection and field layout below.
        if width == 0 {
            let abs_v = v.abs();
            if abs_v == 0.0 || (abs_v >= 0.1 && abs_v < 10f64.powi(decimals as i32)) {
                let rounded = self.apply_explicit_rounding(v, decimals);
                let s = self.apply_leading_zero(&self.format_fixed(rounded, decimals));
                return Ok(self.apply_decimal_sep(&s));
            }
            let s = self.format_e_style(v, decimals, exp_width, 'E');
            return Ok(self.apply_decimal_sep(&s));
        }

        // For Gw.d, first round the internal value to d significant digits.
        // The resulting decimal scale s decides between E and F editing.
        let scale = self.g_rounded_decimal_scale(v, decimals);
        if decimals == 0 || scale < 0 || scale as usize > decimals {
            let s = self.format_e_style(v, decimals, exp_width, 'E');
            return Ok(self.apply_decimal_sep(&fit_exponential_field(&s, width)));
        }

        // F-style G output is F(w-n).(d-s) followed by n blanks. The
        // reserved suffix has the size of the exponent field that the
        // exponential form would have used.
        let reserved = exp_width
            .filter(|width| *width > 0)
            .map_or(4, |width| width.saturating_add(2));
        let Some(fixed_width) = width
            .checked_sub(reserved)
            .filter(|fixed_width| *fixed_width > 0)
        else {
            return Ok("*".repeat(width));
        };

        let fractional_digits = decimals - scale as usize;
        let rounded = self.apply_explicit_rounding(v, fractional_digits);
        let fixed = self.apply_leading_zero(&self.format_fixed(rounded, fractional_digits));
        if fixed.len() > fixed_width {
            return Ok("*".repeat(width));
        }

        let mut field = format!("{fixed:>fixed_width$}");
        field.extend(std::iter::repeat_n(' ', reserved));
        Ok(self.apply_decimal_sep(&field))
    }

    /// Return the decimal scale `s` after rounding `v` to `digits`
    /// significant digits, as required for nonzero-width G editing.
    fn g_rounded_decimal_scale(&self, v: f64, digits: usize) -> i32 {
        if v == 0.0 {
            return 1;
        }

        let raw_scale = decimal_scale(v);
        // Rounding can increase the scale by at most one. Values below
        // scale -1 therefore cannot cross into the fixed-form range.
        if raw_scale < -1 || (raw_scale > 0 && raw_scale as usize > digits) {
            return raw_scale;
        }

        let fractional_digits = if raw_scale >= 0 {
            digits - raw_scale as usize
        } else {
            digits.saturating_add(raw_scale.unsigned_abs() as usize)
        };
        let rounded = self.apply_explicit_rounding(v, fractional_digits);
        if !rounded.is_finite() {
            return raw_scale;
        }
        let fixed = format!("{:.*}", fractional_digits, rounded.abs());
        decimal_scale_from_fixed(&fixed)
    }

    fn format_g0(&self, v: f64, significant_digits: usize) -> String {
        if v == 0.0 {
            let decimals = significant_digits.saturating_sub(1);
            return self.format_fixed(v, decimals);
        }

        let abs_v = v.abs();
        if (0.1..1.0e6).contains(&abs_v) {
            let decimals = if abs_v < 1.0 {
                significant_digits
            } else {
                let digits_before_decimal = abs_v.log10().floor().max(0.0) as usize + 1;
                significant_digits
                    .saturating_sub(digits_before_decimal)
                    .max(1)
            };
            self.format_fixed(v, decimals)
        } else {
            self.format_e_style(v, significant_digits.saturating_sub(1), Some(2), 'E')
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
        let mut base_exp = abs_v.log10().floor() as i32;
        let mantissa = abs_v / 10f64.powi(base_exp);
        let rounded = self.apply_explicit_rounding(mantissa, decimals);
        let mut mantissa_text = format!("{:.*}", decimals, rounded);
        if rounded_mantissa_reached_upper_bound(&mantissa_text, "10") {
            mantissa_text = format!("{:.*}", decimals, 1.0);
            base_exp += 1;
        }

        let ew = exp_width.unwrap_or(2);
        let sign = if v < 0.0 {
            "-"
        } else if matches!(self.sign_mode, SignMode::Plus) {
            "+"
        } else {
            ""
        };
        format!("{}{}E{:+0ew$}", sign, mantissa_text, base_exp, ew = ew + 1)
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

    fn write_spaces(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        let end = self.pos.saturating_add(count);
        if self.record.len() < end {
            self.record.resize(end, b' ');
        }
        self.pos = end;
        self.high_water = self.high_water.max(end);
    }

    fn write_fitted(&mut self, bytes: &[u8], width: usize) {
        if width == 0 {
            self.write(bytes);
        } else if width > bytes.len() {
            self.write_spaces(width - bytes.len());
            self.write(bytes);
        } else {
            self.write(&bytes[..width]);
        }
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

fn decimal_scale(value: f64) -> i32 {
    debug_assert!(value.is_finite() && value != 0.0);
    let scientific = format!("{:E}", value.abs());
    let exponent = scientific
        .rsplit_once('E')
        .and_then(|(_, exponent)| exponent.parse::<i32>().ok())
        .expect("Rust scientific formatting always includes a decimal exponent");
    exponent.saturating_add(1)
}

fn decimal_scale_from_fixed(fixed: &str) -> i32 {
    let (integer, fraction) = fixed.split_once('.').unwrap_or((fixed, ""));
    let integer = integer.trim_start_matches('0');
    if !integer.is_empty() {
        return i32::try_from(integer.len()).unwrap_or(i32::MAX);
    }

    match fraction.bytes().position(|digit| digit != b'0') {
        Some(position) => -i32::try_from(position).unwrap_or(i32::MAX),
        None => 1,
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
        | FormatDesc::CharTrimmed
        | FormatDesc::DerivedType { .. } => true,
        FormatDesc::Group { descriptors, .. } | FormatDesc::UnlimitedRepeat { descriptors } => {
            format_has_data_descriptor(descriptors)
        }
        _ => false,
    })
}

fn append_data_descriptors(
    descriptors: &[FormatDesc],
    result: &mut Vec<FormatDesc>,
    limit: usize,
) -> Result<(), FormatError> {
    for descriptor in descriptors {
        if result.len() == limit {
            break;
        }
        match descriptor {
            FormatDesc::Group {
                repeat,
                descriptors,
                ..
            } => {
                if !format_has_data_descriptor(descriptors) {
                    continue;
                }
                for _ in 0..*repeat {
                    append_data_descriptors(descriptors, result, limit)?;
                    if result.len() == limit {
                        break;
                    }
                }
            }
            FormatDesc::UnlimitedRepeat { descriptors } => {
                if !format_has_data_descriptor(descriptors) {
                    return Err(FormatError::InvalidFormat);
                }
                while result.len() < limit {
                    let before = result.len();
                    append_data_descriptors(descriptors, result, limit)?;
                    if result.len() == before {
                        return Err(FormatError::InvalidFormat);
                    }
                }
            }
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
            | FormatDesc::CharTrimmed
            | FormatDesc::DerivedType { .. } => result.push(descriptor.clone()),
            _ => {}
        }
    }
    Ok(())
}

/// Parse a format and return the data edit descriptor corresponding to each
/// of the first `item_count` effective items, including group expansion and
/// format reversion. This is shared with compiler lowering so defined-I/O
/// dispatch observes the same descriptor order as the runtime engine.
pub fn parse_data_descriptors_for_items(
    format: &str,
    item_count: usize,
) -> Result<Vec<FormatDesc>, FormatError> {
    if item_count == 0 {
        return Ok(Vec::new());
    }

    let descriptors = parse_format(format)?;
    let mut result = Vec::with_capacity(item_count);
    append_data_descriptors(&descriptors, &mut result, item_count)?;
    if result.len() == item_count {
        return Ok(result);
    }

    let reversion = format_reversion_descriptors(&descriptors);
    if !format_has_data_descriptor(reversion) {
        return Err(FormatError::InvalidFormat);
    }
    while result.len() < item_count {
        let before = result.len();
        append_data_descriptors(reversion, &mut result, item_count)?;
        if result.len() == before {
            return Err(FormatError::InvalidFormat);
        }
    }
    Ok(result)
}

pub(crate) fn format_reversion_descriptors(descs: &[FormatDesc]) -> &[FormatDesc] {
    let start = descs
        .iter()
        .rposition(|desc| {
            matches!(
                desc,
                FormatDesc::Group {
                    is_reversion_point: true,
                    ..
                } | FormatDesc::UnlimitedRepeat { .. }
            )
        })
        .unwrap_or(0);
    &descs[start..]
}

// ---- Helpers ----

fn to_engineering(v: f64) -> (f64, i32) {
    if v == 0.0 {
        return (0.0, 0);
    }
    let decimal_exp = v.abs().log10().floor() as i32;
    let exp = decimal_exp.div_euclid(3) * 3;
    let mantissa = v / 10f64.powi(exp);
    (mantissa, exp)
}

fn rounded_mantissa_reached_upper_bound(text: &str, upper_bound: &str) -> bool {
    text.split_once('.').map_or(text, |(integer, _)| integer) == upper_bound
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_format(fmt: &str) -> Vec<FormatDesc> {
        parse_format(fmt).unwrap_or_else(|_| panic!("expected valid FORMAT: {fmt}"))
    }

    #[test]
    fn malformed_formats_are_rejected_instead_of_defaulted() {
        let malformed = [
            "",
            "F8.2",
            "(F8)",
            "(F.2)",
            "(F8.)",
            "(E12)",
            "(E8.2E)",
            "(E8.2E0)",
            "(I)",
            "(I5.)",
            "(T)",
            "(T0)",
            "(X)",
            "(P)",
            "(Q5)",
            "(,I1)",
            "(I1,,I1)",
            "('unterminated)",
            "(2(I3)",
            "(I3))",
            "(*)",
            "(AT4)",
            "(0(I1))",
            "(0X)",
            "(0/)",
            "(999999999999999999999999999999I1)",
        ];

        for format in malformed {
            assert!(
                matches!(parse_format(format), Err(FormatError::InvalidFormat)),
                "malformed FORMAT was accepted: {format}"
            );
        }
    }

    #[test]
    fn valid_format_boundaries_remain_accepted() {
        let descriptors =
            valid_format("(I0,F0.2,G0,G0.4,G12.4E0,E12.4E3,0P,+2P,-2P,2X,2/,'(',*(I1))");
        assert_eq!(descriptors.len(), 13);
        assert!(matches!(
            descriptors.last(),
            Some(FormatDesc::UnlimitedRepeat { .. })
        ));
    }

    #[test]
    fn parentheses_inside_literals_do_not_change_group_nesting() {
        let descriptors = valid_format("('(',2(')(',I1),')')");
        assert_eq!(descriptors.len(), 3);
        assert!(matches!(
            &descriptors[0],
            FormatDesc::LiteralString(text) if text == "("
        ));
        assert!(matches!(
            &descriptors[2],
            FormatDesc::LiteralString(text) if text == ")"
        ));
    }

    #[test]
    fn format_nesting_is_bounded() {
        let at_limit = format!(
            "{}I1{}",
            "(".repeat(MAX_FORMAT_NESTING),
            ")".repeat(MAX_FORMAT_NESTING)
        );
        assert!(parse_format(&at_limit).is_ok());

        let beyond_limit = format!(
            "{}I1{}",
            "(".repeat(MAX_FORMAT_NESTING + 1),
            ")".repeat(MAX_FORMAT_NESTING + 1)
        );
        assert!(matches!(
            parse_format(&beyond_limit),
            Err(FormatError::InvalidFormat)
        ));
    }

    #[test]
    fn large_valid_format_is_linear_and_repeat_counts_stay_structural() {
        let mut format = String::from("(");
        for index in 0..10_000 {
            if index != 0 {
                format.push(',');
            }
            format.push_str("I1");
        }
        format.push(')');
        assert_eq!(valid_format(&format).len(), 10_000);

        let repeated = valid_format("(1000000000/)");
        assert!(matches!(
            &repeated[..],
            [FormatDesc::Group {
                repeat: 1_000_000_000,
                descriptors,
                is_reversion_point: false,
            }] if matches!(&descriptors[..], [FormatDesc::Newline])
        ));
    }

    #[test]
    fn parse_simple_format() {
        let descs = valid_format("(I5, F10.3, A)");
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
        let descs = valid_format("(3I5)");
        assert_eq!(descs.len(), 1);
        assert!(matches!(descs[0], FormatDesc::Group { repeat: 3, .. }));
    }

    #[test]
    fn parse_control_descriptors() {
        let descs = valid_format("(2X, /, SP, T10)");
        assert!(matches!(descs[0], FormatDesc::Skip { count: 2 }));
        assert!(matches!(descs[1], FormatDesc::Newline));
        assert!(matches!(descs[2], FormatDesc::Sign(SignMode::Plus)));
        assert!(matches!(descs[3], FormatDesc::TabTo { position: 10 }));
    }

    #[test]
    fn parse_string_literal() {
        let descs = valid_format("('hello', A)");
        assert_eq!(descs.len(), 2);
        if let FormatDesc::LiteralString(s) = &descs[0] {
            assert_eq!(s, "hello");
        } else {
            panic!("expected literal");
        }
    }

    #[test]
    fn parse_es_en_format() {
        let descs = valid_format("(ES15.8, EN12.3)");
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
        let descs = valid_format("(I5)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(42)]);
        assert_eq!(out, "   42");
    }

    #[test]
    fn format_integer_with_min_digits() {
        let descs = valid_format("(I5.3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(7)]);
        assert_eq!(out, "  007");
    }

    #[test]
    fn format_real_f() {
        let descs = valid_format("(F8.3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.23456)]);
        assert_eq!(out, "   1.235");
    }

    #[test]
    fn format_real_f_zero_decimals_keeps_decimal_point() {
        let descs = valid_format("(F4.0)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.0)]);
        assert_eq!(out, "  1.");
    }

    #[test]
    fn format_real_sign_plus() {
        let descs = valid_format("(SP,F6.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.0)]);
        assert_eq!(out, " +1.00");
    }

    #[test]
    fn format_real_overflow_uses_stars() {
        let descs = valid_format("(F6.2,1X,F6.3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(-100.0), IoValue::Real(1000.0)]);
        assert_eq!(out, "****** ******");
    }

    #[test]
    fn format_exponential_omits_leading_zero_to_fit_width() {
        let descs = valid_format("(E7.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(289.0)]);
        assert_eq!(out, ".29E+03");
    }

    #[test]
    fn format_logical() {
        let descs = valid_format("(L3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Logical(true)]);
        assert_eq!(out, "  T");
    }

    #[test]
    fn format_character() {
        let descs = valid_format("(A5)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Character(b"hi".to_vec())]);
        assert_eq!(out, "   hi");

        let out = FormatEngine::new(valid_format("(A5)"))
            .format_values(&[IoValue::Character(Vec::new())]);
        assert_eq!(out, "     ");
    }

    #[test]
    fn format_g0_character_uses_unlimited_width() {
        let out = FormatEngine::new(valid_format("(G0)"))
            .format_values(&[IoValue::Character(b"txt".to_vec())]);
        assert_eq!(out, "txt");
    }

    #[test]
    fn format_mixed() {
        let descs = valid_format("('Count: ', I4)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(42)]);
        assert_eq!(out, "Count:   42");
    }

    #[test]
    fn format_with_newline() {
        let descs = valid_format("(I3, /, I3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(1), IoValue::Integer(2)]);
        assert_eq!(out, "  1\n  2");
    }

    #[test]
    fn format_skip() {
        let descs = valid_format("(I3, 3X, I3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(1), IoValue::Integer(2)]);
        assert_eq!(out, "  1     2");
    }

    #[test]
    fn format_tab_descriptors_overlay_without_truncation() {
        let out = FormatEngine::new(valid_format("(A,T3,A)")).format_values(&[
            IoValue::Character(b"abcdef".to_vec()),
            IoValue::Character(b"XY".to_vec()),
        ]);
        assert_eq!(out, "abXYef");

        let out = FormatEngine::new(valid_format("(A,TL3,A)")).format_values(&[
            IoValue::Character(b"abcdef".to_vec()),
            IoValue::Character(b"XY".to_vec()),
        ]);
        assert_eq!(out, "abcXYf");

        let out = FormatEngine::new(valid_format("(A,TR3,A)")).format_values(&[
            IoValue::Character(b"ab".to_vec()),
            IoValue::Character(b"Z".to_vec()),
        ]);
        assert_eq!(out, "ab   Z");
    }

    #[test]
    fn format_trailing_x_only_moves_position() {
        let out = FormatEngine::new(valid_format("(I0,1X)")).format_values(&[IoValue::Integer(5)]);
        assert_eq!(out, "5");

        let out = FormatEngine::new(valid_format("(I0,1X,I0)"))
            .format_values(&[IoValue::Integer(5), IoValue::Integer(6)]);
        assert_eq!(out, "5 6");

        let out = FormatEngine::new(valid_format("(3(I0,1X))")).format_values(&[
            IoValue::Integer(1),
            IoValue::Integer(2),
            IoValue::Integer(3),
        ]);
        assert_eq!(out, "1 2 3");
    }

    #[test]
    fn format_sign_plus() {
        let descs = valid_format("(SP, I5)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(42)]);
        assert_eq!(out, "  +42");
    }

    #[test]
    fn format_hex_integer() {
        let descs = valid_format("(Z4)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(255)]);
        assert_eq!(out, "  FF");
    }

    #[test]
    fn format_hex_negative_uses_requested_twos_complement_width() {
        let descs = valid_format("(Z16.16)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(-1)]);
        assert_eq!(out, "FFFFFFFFFFFFFFFF");
    }

    #[test]
    fn format_octal_integer() {
        let descs = valid_format("(O6)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(255)]);
        assert_eq!(out, "   377");
    }

    #[test]
    fn format_octal_integer_with_min_digits() {
        let descs = valid_format("(O4.4)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(18)]);
        assert_eq!(out, "0022");
    }

    #[test]
    fn format_colon_stops_early() {
        let descs = valid_format("(I3, :, ', ', I3)");
        let mut engine = FormatEngine::new(descs);
        // Only one value — colon stops before the comma-space-I3.
        let out = engine.format_values(&[IoValue::Integer(42)]);
        assert_eq!(out, " 42");
    }

    #[test]
    fn format_unlimited_repeat() {
        let descs = valid_format("(*(I3, ','))");
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
        let descs = valid_format("(DC, F8.3, DP, F8.3)");
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
        let descs = valid_format("(DT'mytype')");
        assert_eq!(descs.len(), 1);
        if let FormatDesc::DerivedType {
            ref type_name,
            ref v_list,
        } = descs[0]
        {
            assert_eq!(type_name, "mytype");
            assert!(v_list.is_empty());
        } else {
            panic!("expected DerivedType, got {:?}", descs[0]);
        }
    }

    #[test]
    fn parse_dt_no_name() {
        let descs = valid_format("(DT)");
        assert_eq!(descs.len(), 1);
        if let FormatDesc::DerivedType {
            ref type_name,
            ref v_list,
        } = descs[0]
        {
            assert_eq!(type_name, "");
            assert!(v_list.is_empty());
        } else {
            panic!("expected DerivedType");
        }
    }

    #[test]
    fn intrinsic_format_engine_rejects_dt_value_dispatch() {
        let mut engine = FormatEngine::new(valid_format("(DT'owned-by-lowering'(1))"));
        assert!(matches!(
            engine.format_values_checked(&[IoValue::Integer(7)]),
            Err(FormatError::TypeMismatch)
        ));
    }

    #[test]
    fn parse_dt_preserves_tag_and_signed_v_list() {
        let descs = valid_format("(DT'Link List'(10, -4, +2, 0))");
        assert_eq!(descs.len(), 1);
        let FormatDesc::DerivedType { type_name, v_list } = &descs[0] else {
            panic!("expected DerivedType, got {:?}", descs[0]);
        };
        assert_eq!(type_name, "Link List");
        assert_eq!(v_list, &[10, -4, 2, 0]);
    }

    #[test]
    fn reject_malformed_dt_v_lists() {
        for format in [
            "(DT())",
            "(DT(1,))",
            "(DT(,1))",
            "(DT(2147483648))",
            "(DT(-2147483649))",
            "(DT(1_8))",
        ] {
            assert!(
                matches!(parse_format(format), Err(FormatError::InvalidFormat)),
                "accepted malformed DT descriptor {format}"
            );
        }
    }

    #[test]
    fn data_descriptor_plan_expands_groups_and_reversion() {
        let descriptors = parse_data_descriptors_for_items("(2(DT'A'(1),I2),DT'B'(-2))", 7)
            .expect("valid descriptor plan");
        assert_eq!(descriptors.len(), 7);

        let tags = descriptors
            .iter()
            .map(|descriptor| match descriptor {
                FormatDesc::DerivedType { type_name, v_list } => {
                    format!("DT{type_name}:{v_list:?}")
                }
                FormatDesc::IntegerI { width, .. } => format!("I{width}"),
                other => panic!("unexpected data descriptor {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tags,
            ["DTA:[1]", "I2", "DTA:[1]", "I2", "DTB:[-2]", "DTA:[1]", "I2",]
        );
    }

    #[test]
    fn parse_rounding_modes() {
        let descs = valid_format("(RU, F8.3, RD, F8.3, RZ, F8.3, RN, F8.3, RC, F8.3, RP, F8.3)");
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
        let descs = valid_format("(LZ, F6.3, LZS, F6.3, LZP, F6.3)");
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
        let mut engine = FormatEngine::new(valid_format("(F6.3)"));
        engine.set_leading_zero(LeadingZeroMode::Suppress);
        assert_eq!(engine.format_values(&[IoValue::Real(0.25)]).trim(), ".250");

        let mut overridden = FormatEngine::new(valid_format("(LZP, F6.3)"));
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
        let descs = valid_format("(LZS, L2)");
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
        let descs = valid_format("(1X,AT,F4.1)");
        assert!(matches!(descs[0], FormatDesc::Skip { .. }));
        assert!(matches!(descs[1], FormatDesc::CharTrimmed));
        assert!(matches!(descs[2], FormatDesc::RealF { .. }));
        let plain = valid_format("(A4)");
        assert!(matches!(plain[0], FormatDesc::Character { width: Some(4) }));
    }

    #[test]
    fn format_leading_zero_suppress() {
        // LZS drops the leading zero; default/LZP keep it.
        let s =
            FormatEngine::new(valid_format("(LZS, F6.3)")).format_values(&[IoValue::Real(0.25)]);
        assert_eq!(s.trim(), ".250");
        let neg =
            FormatEngine::new(valid_format("(LZS, F7.3)")).format_values(&[IoValue::Real(-0.25)]);
        assert_eq!(neg.trim(), "-.250");
        let def = FormatEngine::new(valid_format("(F6.3)")).format_values(&[IoValue::Real(0.25)]);
        assert_eq!(def.trim(), "0.250");
        let lzp =
            FormatEngine::new(valid_format("(LZP, F6.3)")).format_values(&[IoValue::Real(0.25)]);
        assert_eq!(lzp.trim(), "0.250");
    }

    #[test]
    fn format_leading_zero_suppress_only_below_one() {
        // No leading zero to drop when |value| >= 1.
        let s =
            FormatEngine::new(valid_format("(LZS, F7.3)")).format_values(&[IoValue::Real(10.25)]);
        assert_eq!(s.trim(), "10.250");
    }

    #[test]
    fn format_leading_zero_suppress_exponential() {
        // E-format mantissa leading zero is suppressed too.
        let s =
            FormatEngine::new(valid_format("(LZS, E10.3)")).format_values(&[IoValue::Real(0.25)]);
        assert!(s.contains(".250"), "got {s}");
        assert!(
            !s.trim().starts_with('0'),
            "leading zero not suppressed: {s}"
        );
    }

    #[test]
    fn format_at_trims_trailing_blanks() {
        let s = FormatEngine::new(valid_format("(AT)"))
            .format_values(&[IoValue::Character(b"hi   ".to_vec())]);
        assert_eq!(s, "hi");
        let blank = FormatEngine::new(valid_format("(AT)"))
            .format_values(&[IoValue::Character(b"     ".to_vec())]);
        assert_eq!(blank, "");
    }

    #[test]
    fn parse_negative_scale_factor() {
        let descs = valid_format("(-2P, E15.8)");
        assert!(matches!(descs[0], FormatDesc::ScaleFactor(-2)));
    }

    #[test]
    fn parse_positive_scale_factor() {
        let descs = valid_format("(3P, E15.8)");
        assert!(matches!(descs[0], FormatDesc::ScaleFactor(3)));
    }

    #[test]
    fn format_decimal_comma() {
        let descs = valid_format("(DC, F8.3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.23)]);
        assert!(out.contains(','), "expected comma in output: {}", out);
        assert!(!out.contains('.'), "expected no dot in output: {}", out);
    }

    #[test]
    fn format_rounding_up() {
        let descs = valid_format("(RU, F6.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.234)]);
        // RU rounds 1.234 up to 1.24 at 2 decimals.
        assert_eq!(out.trim(), "1.24");
    }

    #[test]
    fn format_rounding_down() {
        let descs = valid_format("(RD, F6.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.236)]);
        // RD rounds 1.236 down to 1.23 at 2 decimals.
        assert_eq!(out.trim(), "1.23");
    }

    #[test]
    fn format_rounding_zero() {
        let descs = valid_format("(RZ, F6.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(-1.236)]);
        // RZ truncates toward zero: -1.236 → -1.23.
        assert_eq!(out.trim(), "-1.23");
    }

    #[test]
    fn format_scale_factor_f() {
        // 2P with F: multiplies value by 10^2 before formatting.
        let descs = valid_format("(2P, F8.2)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(1.5)]);
        // 1.5 * 100 = 150.00
        assert_eq!(out.trim(), "150.00");
    }

    #[test]
    fn format_scale_factor_e_reduces_fractional_digits() {
        let descs = valid_format("(2P,E12.4,1X,E12.4,1X,E12.4)");
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
        let descs = valid_format("(G0, 1X, G0, 1X, G0)");
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
        let descs = valid_format("(G0,1X,G0,1X,G0,1X,G0)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[
            IoValue::Real32(100.0),
            IoValue::Real32(1.0),
            IoValue::Real(100.0),
            IoValue::Real(1.0),
        ]);
        assert_eq!(
            out,
            "100.000000 1.00000000 100.00000000000000 1.0000000000000000"
        );
    }

    #[test]
    fn format_g0_real_kinds_use_kind_precision() {
        let descs = valid_format("(G0,1X,G0,1X,G0)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[
            IoValue::Real32((1.0f32 / 3.0f32) as f64),
            IoValue::Real(1.0f64 / 3.0f64),
            IoValue::Real(std::f64::consts::PI),
        ]);
        assert_eq!(out, "0.333333343 0.33333333333333331 3.1415926535897931");
    }

    #[test]
    fn format_nonzero_width_g_uses_significant_digits_and_reserved_columns() {
        let format = |value| {
            FormatEngine::new(valid_format("(G12.4)")).format_values(&[IoValue::Real(value)])
        };

        assert_eq!(format(0.0), "   0.000    ");
        assert_eq!(format(0.1), "  0.1000    ");
        assert_eq!(format(1.2345), "   1.234    ");
        assert_eq!(format(12.345), "   12.35    ");
        assert_eq!(format(1234.5), "   1234.    ");
        assert_eq!(format(-12.345), "  -12.35    ");
        assert_eq!(format(0.012345), "  0.1235E-01");
    }

    #[test]
    fn format_nonzero_width_g_selects_style_after_significant_rounding() {
        let format = |value| {
            FormatEngine::new(valid_format("(G12.4)")).format_values(&[IoValue::Real(value)])
        };

        assert_eq!(format(0.099995), "  0.1000    ");
        assert_eq!(format(9999.5), "  0.1000E+05");
    }

    #[test]
    fn format_nonzero_width_g_honors_explicit_exponent_reservation_and_overflow() {
        let out =
            FormatEngine::new(valid_format("(G14.4E3)")).format_values(&[IoValue::Real(12.345)]);
        assert_eq!(out, "    12.35     ");

        let out =
            FormatEngine::new(valid_format("(G14.4E3)")).format_values(&[IoValue::Real(0.012345)]);
        assert_eq!(out, "   0.1235E-001");

        let out = FormatEngine::new(valid_format("(G8.4)")).format_values(&[IoValue::Real(12.345)]);
        assert_eq!(out, "********");
    }

    #[test]
    fn format_nonzero_width_g_rounding_modes_control_boundary_selection() {
        let format = |descriptor, value| {
            FormatEngine::new(valid_format(descriptor)).format_values(&[IoValue::Real(value)])
        };

        assert_eq!(format("(RU,G12.4)", 0.099991), "  0.1000    ");
        assert_eq!(format("(RD,G12.4)", 0.099991), "  0.9999E-01");
        assert_eq!(format("(RD,G12.4)", 9999.1), "   9999.    ");

        let rounded_up = format("(RU,G12.4)", 9999.1);
        assert!(rounded_up.contains('E'), "{rounded_up:?}");
        let rounded_up = format("(RU,G12.4)", -0.099991);
        assert!(rounded_up.contains('E'), "{rounded_up:?}");
        let rounded_down = format("(RD,G12.4)", -0.099991);
        assert!(!rounded_down.contains('E'), "{rounded_down:?}");
        assert!(rounded_down.ends_with("    "), "{rounded_down:?}");

        let rounded_up = format("(RU,G12.4)", -9999.1);
        assert!(!rounded_up.contains('E'), "{rounded_up:?}");
        assert!(rounded_up.ends_with("    "), "{rounded_up:?}");
        let rounded_down = format("(RD,G12.4)", -9999.1);
        assert!(rounded_down.contains('E'), "{rounded_down:?}");
    }

    #[test]
    fn format_g0_integer_and_logical() {
        let descs = valid_format("(G0,1X,G0,1X,SP,G0)");
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
        let descs = valid_format("(1X)");
        let mut engine = FormatEngine::new(descs);
        assert_eq!(
            engine.format_values_checked(&[IoValue::Logical(false)]),
            Err(FormatError::InvalidFormat)
        );
    }

    #[test]
    fn format_reversion_starts_new_records() {
        let descs = valid_format("(A)");
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
        let descs = valid_format("(I2,1X,I2)");
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
    fn format_reversion_starts_at_rightmost_parenthesized_group() {
        let mut engine = FormatEngine::new(valid_format("(\"P\",2(I0,1X))"));
        let out = engine
            .format_values_reverting_checked(&[
                IoValue::Integer(1),
                IoValue::Integer(2),
                IoValue::Integer(3),
                IoValue::Integer(4),
            ])
            .unwrap();
        assert_eq!(out, "P1 2\n3 4");
    }

    #[test]
    fn format_reversion_includes_descriptors_after_group() {
        let mut engine = FormatEngine::new(valid_format("(\"E\",2(I0,1X),\"Z\",I0)"));
        let out = engine
            .format_values_reverting_checked(&[
                IoValue::Integer(1),
                IoValue::Integer(2),
                IoValue::Integer(3),
                IoValue::Integer(4),
                IoValue::Integer(5),
                IoValue::Integer(6),
            ])
            .unwrap();
        assert_eq!(out, "E1 2 Z3\n4 5 Z6");
    }

    #[test]
    fn format_reversion_uses_outer_group_nearest_format_end() {
        let mut engine = FormatEngine::new(valid_format("(\"B\",2(I0,3(\"x\",I0,1X)))"));
        let values: Vec<_> = (1..=12).map(IoValue::Integer).collect();
        let out = engine.format_values_reverting_checked(&values).unwrap();
        assert_eq!(out, "B1x2 x3 x4 5x6 x7 x8\n9x10 x11 x12");
    }

    #[test]
    fn format_reversion_ignores_data_descriptor_repeat_wrapper() {
        let mut engine = FormatEngine::new(valid_format("(\"P\",2I0)"));
        let out = engine
            .format_values_reverting_checked(&[
                IoValue::Integer(1),
                IoValue::Integer(2),
                IoValue::Integer(3),
                IoValue::Integer(4),
            ])
            .unwrap();
        assert_eq!(out, "P12\nP34");
    }

    #[test]
    fn format_reversion_preserves_control_state_before_group() {
        let mut engine = FormatEngine::new(valid_format("(SP,\"P\",(I0))"));
        let out = engine
            .format_values_reverting_checked(&[IoValue::Integer(1), IoValue::Integer(2)])
            .unwrap();
        assert_eq!(out, "P+1\n+2");
    }

    #[test]
    fn format_reversion_keeps_unlimited_group_on_one_record() {
        let mut engine = FormatEngine::new(valid_format("(\"P\",*(I0,1X))"));
        let out = engine
            .format_values_reverting_checked(&[
                IoValue::Integer(1),
                IoValue::Integer(2),
                IoValue::Integer(3),
            ])
            .unwrap();
        assert_eq!(out, "P1 2 3");
    }

    #[test]
    fn format_es_dp_precision_roundtrips() {
        let value = -1.9972267279387788e-1f64;
        let descs = valid_format("(ES24.16E3)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Real(value)]);
        assert_eq!(out.trim().parse::<f64>().unwrap(), value);
    }

    #[test]
    fn format_es_renormalizes_rounding_carry() {
        let format = |descriptor, value| {
            FormatEngine::new(valid_format(descriptor)).format_values(&[IoValue::Real(value)])
        };

        assert_eq!(format("(RU,ES13.3)", 9.9991), "    1.000E+01");
        assert_eq!(format("(RN,ES13.3)", 9.9996), "    1.000E+01");
        assert_eq!(format("(RZ,ES13.3)", 9.9996), "    9.999E+00");
        assert_eq!(format("(RU,ES14.3E3)", 9.9991), "    1.000E+001");
        assert_eq!(format("(RU,ES13.3)", 0.99991), "    1.000E+00");
    }

    #[test]
    fn format_en_renormalizes_rounding_carry() {
        let format = |descriptor, value| {
            FormatEngine::new(valid_format(descriptor)).format_values(&[IoValue::Real(value)])
        };

        assert_eq!(format("(RU,EN14.3)", 999.9991), "     1.000E+03");
        assert_eq!(format("(RN,EN14.3)", 999.9996), "     1.000E+03");
        assert_eq!(format("(EN14.3)", 999.9996), "     1.000E+03");
        assert_eq!(format("(RZ,EN14.3)", 999.9996), "   999.999E+00");
        assert_eq!(format("(RU,EN14.3)", 0.9999991), "     1.000E+00");
        assert_eq!(format("(RU,EN14.3)", 0.0009999991), "     1.000E-03");
        assert_eq!(format("(RU,EN14.3)", 999999.9), "     1.000E+06");
    }

    #[test]
    fn format_en_subunit_values_use_engineering_exponents() {
        let mut engine = FormatEngine::new(valid_format("(EN14.3)"));
        let out = engine
            .format_values_reverting_checked(&[
                IoValue::Real(0.1),
                IoValue::Real(0.01),
                IoValue::Real(0.001),
                IoValue::Real(0.0001),
                IoValue::Real(-0.01),
            ])
            .unwrap();
        assert_eq!(
            out,
            "   100.000E-03\n    10.000E-03\n     1.000E-03\n   100.000E-06\n   -10.000E-03"
        );
    }

    #[test]
    fn format_exponential_nonfinite_reals() {
        let descs = valid_format("(E8.1, 1X, ES8.1, 1X, EN8.1, 1X, D8.1)");
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
        let descs = valid_format("(E30.18E3)");
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
        let descs = valid_format("(D12.5)");
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
        let descs = valid_format("(DC, D12.5)");
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
        let descs = valid_format("(I40)");
        let mut engine = FormatEngine::new(descs);
        let out = engine.format_values(&[IoValue::Integer(
            170141183460469231731687303715884105727i128,
        )]);
        assert_eq!(out, " 170141183460469231731687303715884105727");
    }
}
