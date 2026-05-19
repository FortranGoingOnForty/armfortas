//! IR type system.
//!
//! Low-level types for the SSA IR. Fortran types are lowered to these
//! during AST→IR construction. Array descriptors, character descriptors,
//! and derived types become IR structs.

/// An IR type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IrType {
    Void,
    Bool,
    Int(IntWidth),
    Float(FloatWidth),
    Ptr(Box<IrType>),
    /// Fixed-size array: [T; N].
    Array(Box<IrType>, u64),
    /// Named struct (index into Module::struct_defs).
    Struct(StructId),
    /// Function pointer.
    FuncPtr(Box<FuncSig>),
    /// SIMD vector: `lanes` elements of `elem`. Only the lane counts
    /// the NEON ISA supports are accepted by the verifier — 4×i32,
    /// 2×i64, 4×f32, 2×f64, plus narrow integer packings (16×i8,
    /// 8×i16). All vectors are 128 bits wide.
    Vector {
        lanes: u8,
        elem: Box<IrType>,
    },
}

/// Integer bit widths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntWidth {
    I8,
    I16,
    I32,
    I64,
    I128,
}

impl IntWidth {
    pub fn bits(self) -> u32 {
        match self {
            Self::I8 => 8,
            Self::I16 => 16,
            Self::I32 => 32,
            Self::I64 => 64,
            Self::I128 => 128,
        }
    }

    pub fn bytes(self) -> u32 {
        self.bits() / 8
    }
}

/// Floating-point widths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatWidth {
    F32,
    F64,
}

impl FloatWidth {
    pub fn bits(self) -> u32 {
        match self {
            Self::F32 => 32,
            Self::F64 => 64,
        }
    }

    pub fn bytes(self) -> u32 {
        self.bits() / 8
    }
}

/// Struct type identifier (index into Module::struct_defs).
pub type StructId = u32;

/// Function signature.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FuncSig {
    pub params: Vec<IrType>,
    pub ret: IrType,
}

/// Named struct definition.
#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, IrType)>,
}

impl IrType {
    /// Size in bytes on ARM64.
    pub fn size_bytes(&self) -> u64 {
        match self {
            Self::Void => 0,
            Self::Bool => 1,
            Self::Int(w) => w.bytes() as u64,
            Self::Float(w) => w.bytes() as u64,
            Self::Ptr(_) => 8, // 64-bit pointers
            Self::Array(elem, count) => elem.size_bytes() * count,
            Self::Struct(_) => {
                panic!("Struct size requires struct_defs; use Module::struct_size()")
            }
            Self::FuncPtr(_) => 8,
            // 128-bit NEON: every supported lane combo lands at 16
            // bytes. The verifier rejects shapes that don't.
            Self::Vector { .. } => 16,
        }
    }

    /// Is this a vector type?
    pub fn is_vector(&self) -> bool {
        matches!(self, Self::Vector { .. })
    }

    /// If this is a vector, return (lanes, element type).
    pub fn vector_shape(&self) -> Option<(u8, &IrType)> {
        if let Self::Vector { lanes, elem } = self {
            Some((*lanes, elem))
        } else {
            None
        }
    }

    /// Is this a numeric (integer or float) type?
    pub fn is_numeric(&self) -> bool {
        matches!(self, Self::Int(_) | Self::Float(_))
    }

    /// Extract the IntWidth if this is an integer type.
    pub fn int_width(&self) -> Option<IntWidth> {
        if let Self::Int(w) = self {
            Some(*w)
        } else {
            None
        }
    }

    /// Is this an integer type?
    pub fn is_int(&self) -> bool {
        matches!(self, Self::Int(_))
    }

    /// Is this a float type?
    pub fn is_float(&self) -> bool {
        matches!(self, Self::Float(_))
    }

    /// Is this a pointer type?
    pub fn is_ptr(&self) -> bool {
        matches!(self, Self::Ptr(_))
    }

    /// Fortran integer(1) → i8, integer(2) → i16, integer(4) → i32,
    /// integer(8) → i64, integer(16) → i128.
    pub fn int_from_kind(kind: u8) -> Self {
        Self::Int(match kind {
            1 => IntWidth::I8,
            2 => IntWidth::I16,
            8 => IntWidth::I64,
            16 => IntWidth::I128,
            _ => IntWidth::I32, // default
        })
    }

    /// Fortran real(4) → f32, real(8) → f64.
    pub fn float_from_kind(kind: u8) -> Self {
        Self::Float(match kind {
            8 => FloatWidth::F64,
            _ => FloatWidth::F32,
        })
    }
}

impl std::fmt::Display for IrType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Void => write!(f, "void"),
            Self::Bool => write!(f, "bool"),
            Self::Int(w) => write!(f, "i{}", w.bits()),
            Self::Float(FloatWidth::F32) => write!(f, "f32"),
            Self::Float(FloatWidth::F64) => write!(f, "f64"),
            Self::Ptr(inner) => write!(f, "ptr<{}>", inner),
            Self::Array(elem, n) => write!(f, "[{} x {}]", elem, n),
            Self::Struct(id) => write!(f, "struct.{}", id),
            Self::FuncPtr(sig) => {
                write!(f, "fn(")?;
                for (i, p) in sig.params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                write!(f, ") -> {}", sig.ret)
            }
            Self::Vector { lanes, elem } => write!(f, "<{} x {}>", lanes, elem),
        }
    }
}

impl std::fmt::Display for IntWidth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "i{}", self.bits())
    }
}

impl std::fmt::Display for FloatWidth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "f{}", self.bits())
    }
}
