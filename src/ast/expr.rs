//! Expression AST nodes.
//!
//! Represents all Fortran expression forms: literals, names, operators,
//! function calls, array constructors, component access, and more.

use crate::lexer::Span;

/// A Fortran expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    // ---- Literals ----

    /// Integer literal: `42`, `42_8`, `42_int64`
    IntegerLiteral {
        text: String,
        kind: Option<String>,
    },
    /// Real literal: `3.14`, `1.0d0`, `6.022e23`, `1.0_8`, `.5`, `5.`
    RealLiteral {
        text: String,
        kind: Option<String>,
    },
    /// String literal: `'hello'`, `"hello"`, `'it''s'`
    StringLiteral {
        value: String,
        kind: Option<String>,
    },
    /// Logical literal: `.true.`, `.false.`, `.true._4`
    LogicalLiteral {
        value: bool,
        kind: Option<String>,
    },
    /// Complex literal: `(1.0, 2.0)`
    ComplexLiteral {
        real: Box<SpannedExpr>,
        imag: Box<SpannedExpr>,
    },
    /// BOZ literal: `B'1010'`, `O'777'`, `Z'FF'`
    BozLiteral {
        text: String,
        base: BozBase,
    },

    // ---- Names and access ----

    /// Simple name (variable, function, type, etc.)
    Name {
        name: String,
    },
    /// Component access: `x%field`, `x%inner%deep`
    ComponentAccess {
        base: Box<SpannedExpr>,
        component: String,
    },

    // ---- Operations ----

    /// Unary operation: `-x`, `.not. x`, `+x`
    UnaryOp {
        op: UnaryOp,
        operand: Box<SpannedExpr>,
    },
    /// Binary operation: `a + b`, `x .and. y`, `s // t`
    BinaryOp {
        op: BinaryOp,
        left: Box<SpannedExpr>,
        right: Box<SpannedExpr>,
    },

    // ---- Calls and subscripts ----
    // Note: A(I) is ambiguous — could be array access, function call, or substring.
    // The parser produces FunctionCall for all of them; sema disambiguates.

    /// Function call or array access: `sin(x)`, `a(i,j)`, `s(1:5)`
    FunctionCall {
        callee: Box<SpannedExpr>,
        args: Vec<Argument>,
    },

    // ---- Array constructors ----

    /// Array constructor: `[1, 2, 3]` or `(/ 1, 2, 3 /)`
    ArrayConstructor {
        type_spec: Option<String>,  // [integer :: 1, 2, 3]
        values: Vec<AcValue>,
    },

    // ---- Parenthesized ----

    /// Parenthesized expression: `(x + y)`
    ParenExpr {
        inner: Box<SpannedExpr>,
    },
}

/// An expression with source location.
pub type SpannedExpr = super::Spanned<Expr>;

impl SpannedExpr {
    /// Pretty-print the expression with full parenthesization for testing.
    pub fn to_sexpr(&self) -> String {
        match &self.node {
            Expr::IntegerLiteral { text, .. } => text.clone(),
            Expr::RealLiteral { text, .. } => text.clone(),
            Expr::StringLiteral { value, .. } => format!("'{}'", value),
            Expr::LogicalLiteral { value, .. } => {
                if *value { ".true.".into() } else { ".false.".into() }
            }
            Expr::ComplexLiteral { real, imag } => {
                format!("({}, {})", real.to_sexpr(), imag.to_sexpr())
            }
            Expr::BozLiteral { text, .. } => text.clone(),
            Expr::Name { name } => name.clone(),
            Expr::ComponentAccess { base, component } => {
                format!("{}%{}", base.to_sexpr(), component)
            }
            Expr::UnaryOp { op, operand } => {
                format!("({} {})", op, operand.to_sexpr())
            }
            Expr::BinaryOp { op, left, right } => {
                format!("({} {} {})", left.to_sexpr(), op, right.to_sexpr())
            }
            Expr::FunctionCall { callee, args } => {
                let args_str: Vec<String> = args.iter().map(|a| {
                    if let Some(kw) = &a.keyword {
                        format!("{}={}", kw, a.value.to_sexpr())
                    } else {
                        a.value.to_sexpr()
                    }
                }).collect();
                format!("{}({})", callee.to_sexpr(), args_str.join(", "))
            }
            Expr::ArrayConstructor { values, type_spec } => {
                let vals: Vec<String> = values.iter().map(|v| match v {
                    AcValue::Expr(e) => e.to_sexpr(),
                    AcValue::ImpliedDo { .. } => "(implied-do)".into(),
                }).collect();
                if let Some(ts) = type_spec {
                    format!("[{} :: {}]", ts, vals.join(", "))
                } else {
                    format!("[{}]", vals.join(", "))
                }
            }
            Expr::ParenExpr { inner } => {
                format!("({})", inner.to_sexpr())
            }
        }
    }
}

/// BOZ literal base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BozBase {
    Binary,
    Octal,
    Hex,
}

/// Unary operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,       // +
    Minus,      // -
    Not,        // .not.
    Defined(String), // .myop.
}

impl std::fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnaryOp::Plus => write!(f, "+"),
            UnaryOp::Minus => write!(f, "-"),
            UnaryOp::Not => write!(f, ".not."),
            UnaryOp::Defined(s) => write!(f, ".{}.", s),
        }
    }
}

/// Binary operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryOp {
    // Arithmetic
    Add,        // +
    Sub,        // -
    Mul,        // *
    Div,        // /
    Pow,        // **

    // String
    Concat,     // //

    // Comparison
    Eq,         // == or .eq.
    Ne,         // /= or .ne.
    Lt,         // < or .lt.
    Le,         // <= or .le.
    Gt,         // > or .gt.
    Ge,         // >= or .ge.

    // Logical
    And,        // .and.
    Or,         // .or.
    Eqv,        // .eqv.
    Neqv,       // .neqv.

    // User-defined
    Defined(String), // .myop.
}

impl std::fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BinaryOp::Add => write!(f, "+"),
            BinaryOp::Sub => write!(f, "-"),
            BinaryOp::Mul => write!(f, "*"),
            BinaryOp::Div => write!(f, "/"),
            BinaryOp::Pow => write!(f, "**"),
            BinaryOp::Concat => write!(f, "//"),
            BinaryOp::Eq => write!(f, "=="),
            BinaryOp::Ne => write!(f, "/="),
            BinaryOp::Lt => write!(f, "<"),
            BinaryOp::Le => write!(f, "<="),
            BinaryOp::Gt => write!(f, ">"),
            BinaryOp::Ge => write!(f, ">="),
            BinaryOp::And => write!(f, ".and."),
            BinaryOp::Or => write!(f, ".or."),
            BinaryOp::Eqv => write!(f, ".eqv."),
            BinaryOp::Neqv => write!(f, ".neqv."),
            BinaryOp::Defined(s) => write!(f, ".{}.", s),
        }
    }
}

/// A function/subroutine argument (positional or keyword).
/// The value is a SectionSubscript to support both plain expressions
/// and range subscripts like a(1:5) or a(1:10:2).
#[derive(Debug, Clone, PartialEq)]
pub struct Argument {
    pub keyword: Option<String>,
    pub value: SectionSubscript,
}

/// Array constructor value — either an expression or an implied-do loop.
#[derive(Debug, Clone, PartialEq)]
pub enum AcValue {
    Expr(SpannedExpr),
    ImpliedDo {
        values: Vec<AcValue>,
        var: String,
        start: SpannedExpr,
        end: SpannedExpr,
        step: Option<SpannedExpr>,
    },
}

/// Section subscript — either a single element or a range (for array sections/substrings).
#[derive(Debug, Clone, PartialEq)]
pub enum SectionSubscript {
    Element(SpannedExpr),
    Range {
        start: Option<SpannedExpr>,
        end: Option<SpannedExpr>,
        stride: Option<SpannedExpr>,
    },
}

impl SectionSubscript {
    pub fn to_sexpr(&self) -> String {
        match self {
            SectionSubscript::Element(e) => e.to_sexpr(),
            SectionSubscript::Range { start, end, stride } => {
                let s = start.as_ref().map_or(String::new(), |e| e.to_sexpr());
                let e = end.as_ref().map_or(String::new(), |e| e.to_sexpr());
                if let Some(st) = stride {
                    format!("{}:{}:{}", s, e, st.to_sexpr())
                } else {
                    format!("{}:{}", s, e)
                }
            }
        }
    }
}
