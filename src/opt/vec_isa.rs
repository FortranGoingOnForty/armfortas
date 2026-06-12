//! Per-target vector ISA descriptions (x10). Plain data, no trait
//! registry: the analysis in `vec_analysis` consults exactly these
//! flags where targets genuinely differ. Both supported ISAs are
//! 128-bit with the same lane shapes (4×i32/f32, 2×i64/f64 — the
//! `lane_count_for` table in vec_analysis); what differs is op
//! legality. A loop whose op the table refuses simply stays scalar.

use crate::target::Arch;

pub struct VectorIsa {
    pub name: &'static str,
    /// Element-wise integer lane multiply. NEON has `mul.4s`; SSE2
    /// has no 32-bit lane multiply (`pmulld` is SSE4.1; the
    /// `pmuludq` even/odd-shuffle synthesis is deferred until the
    /// benchmark gate can judge it).
    pub int_mul: bool,
    /// Element-wise integer min/max (select-of-compare). NEON has
    /// `smax/smin.4s`; SSE2 only has the 8-bit-unsigned and
    /// 16-bit-signed forms, neither of which matches our i32 lanes.
    pub int_min_max: bool,
    /// Across-lane MIN/MAX reduction legality per element type.
    /// NEON: i32 via `smaxv/sminv.4s`, f32 via `fmaxv/fminv.4s`,
    /// f64 via `fmaxp/fminp.2d`. SSE2: float forms reduce via a
    /// shuffle tree (`pshufd`/`movhlps` + `minps`-family); the i32
    /// form would need the missing integer min/max, so it is
    /// refused with it.
    pub reduce_min_max_i32: bool,
    pub reduce_min_max_f32: bool,
    pub reduce_min_max_f64: bool,
}

pub const NEON: VectorIsa = VectorIsa {
    name: "neon",
    int_mul: true,
    int_min_max: true,
    reduce_min_max_i32: true,
    reduce_min_max_f32: true,
    reduce_min_max_f64: true,
};

pub const SSE2_BASELINE: VectorIsa = VectorIsa {
    name: "sse2",
    int_mul: false,
    int_min_max: false,
    reduce_min_max_i32: false,
    reduce_min_max_f32: true,
    reduce_min_max_f64: true,
};

/// The vector ISA for an architecture at the baseline capability
/// level (`--target-cpu=baseline` is the only level today; higher
/// levels would dispatch here when a sprint takes them).
pub fn baseline_isa(arch: Arch) -> &'static VectorIsa {
    match arch {
        Arch::Arm64 => &NEON,
        Arch::X86_64 => &SSE2_BASELINE,
    }
}
