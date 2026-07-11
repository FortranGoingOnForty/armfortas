//! Per-target vector ISA descriptions (x10). Plain data, no trait
//! registry: the analysis in `vec_analysis` consults exactly these
//! flags where targets genuinely differ. Both supported ISAs are
//! 128-bit with the same lane shapes (4×i32/f32, 2×i64/f64 — the
//! `lane_count_for` table in vec_analysis); what differs is op
//! legality. A loop whose op the table refuses simply stays scalar.

use crate::target::Arch;

pub struct VectorIsa {
    pub name: &'static str,
    /// Element-wise integer lane multiply (i32). NEON has `mul.4s`;
    /// SSE2 has no `pmulld` (SSE4.1) so x86 synthesizes it from two
    /// `pmuludq` (even/odd lanes) plus shuffles (x10c-3). i64 lane mul
    /// is unsupported on both (no NEON 2D mul, no SSE2 pmullq) and stays
    /// scalar — vec_analysis gates this flag to i32.
    pub int_mul: bool,
    /// Element-wise integer min/max (select-of-compare). NEON has
    /// `smax/smin.4s`; SSE2 has no native i32 form (`pminsd`/`pmaxsd`
    /// are SSE4.1), but the signed `pcmpgtd` compare-and-blend
    /// synthesis (x10c-1) covers it — the same expansion gfortran and
    /// LLVM emit at the SSE2 baseline.
    pub int_min_max: bool,
    /// Across-lane MIN/MAX reduction legality per element type.
    /// NEON: i32 via `smaxv/sminv.4s`, f32 via `fmaxv/fminv.4s`,
    /// f64 via `fmaxp/fminp.2d`. SSE2: float forms reduce via a
    /// shuffle tree (`pshufd`/`movhlps` + `minps`-family); the i32
    /// form uses the same tree over the synthesized integer min/max.
    pub reduce_min_max_i32: bool,
    pub reduce_min_max_f32: bool,
    pub reduce_min_max_f64: bool,
    /// Packed not-equal comparison. SSE2 exposes this directly or via
    /// inversion; the current NEON selector has no lowering for it.
    pub cmp_ne: bool,
    /// Packed i64 lane compare (WHERE masks over 64-bit integer
    /// arrays). Neither baseline selector implements this yet.
    pub int_cmp_i64: bool,
}

pub const NEON: VectorIsa = VectorIsa {
    name: "neon",
    int_mul: true,
    int_min_max: true,
    reduce_min_max_i32: true,
    reduce_min_max_f32: true,
    reduce_min_max_f64: true,
    cmp_ne: false,
    int_cmp_i64: false,
};

pub const SSE2_BASELINE: VectorIsa = VectorIsa {
    name: "sse2",
    // i32 lane multiply via the pmuludq even/odd synthesis (x10c-3);
    // i64 stays scalar (no pmullq at SSE2). Gated to i32 in vec_analysis.
    int_mul: true,
    // Signed i32 min/max via pcmpgtd compare-and-blend (x10c-1).
    int_min_max: true,
    reduce_min_max_i32: true,
    reduce_min_max_f32: true,
    reduce_min_max_f64: true,
    cmp_ne: true,
    int_cmp_i64: false,
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
