//! Default kind selection knobs (sprint 32 #504 / #4).
//!
//! `-fdefault-integer-8` and `-fdefault-real-8` change the implicit
//! kind for unsuffixed `integer` / `real` declarations.  The
//! information needs to reach two places that don't share the
//! Options struct directly:
//!   - `crate::ir::lower::extract_kind*` — for IR type lowering
//!   - `crate::sema::type_layout::size_of_type` — for struct layout
//!
//! Plumbing it through every type_spec_to_type_info call would touch
//! ~20 sites; instead we publish per-thread defaults that the driver
//! sets at the start of `compile()` and the consumers read.
//!
//! Per-thread (not process-wide) because cargo runs tests in parallel
//! within one process — a global AtomicU8 lets one test's
//! -fdefault-integer-8 bleed into another test running on a
//! different thread.  thread_local Cell isolates them.

use std::cell::Cell;

thread_local! {
    /// Default kind for unsuffixed `integer` declarations.  4 (the
    /// Fortran standard's processor default) unless
    /// `-fdefault-integer-8` is in effect.
    static DEFAULT_INT_KIND: Cell<u8> = const { Cell::new(4) };

    /// Default kind for unsuffixed `real` declarations.  4 unless
    /// `-fdefault-real-8` is in effect.
    static DEFAULT_REAL_KIND: Cell<u8> = const { Cell::new(4) };
}

/// Reset to the standard defaults.  Called by the driver at the
/// start of every compile() so a previous run with non-default
/// kinds doesn't leak into the current one.
pub fn reset() {
    DEFAULT_INT_KIND.with(|c| c.set(4));
    DEFAULT_REAL_KIND.with(|c| c.set(4));
}

pub fn set_default_int_kind(k: u8) {
    DEFAULT_INT_KIND.with(|c| c.set(k));
}

pub fn set_default_real_kind(k: u8) {
    DEFAULT_REAL_KIND.with(|c| c.set(k));
}

pub fn default_int_kind() -> u8 {
    DEFAULT_INT_KIND.with(|c| c.get())
}

pub fn default_real_kind() -> u8 {
    DEFAULT_REAL_KIND.with(|c| c.get())
}
