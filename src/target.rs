//! Target identity: the architecture, OS, and libc armfortas compiles *for*.
//!
//! `TargetSpec::host()` is the only place in the workspace allowed to consult
//! `cfg!(target_arch)` / `cfg!(target_os)` / `cfg!(target_env)` for target
//! decisions. Everything else takes a `TargetSpec` value. A `cfg!` anywhere
//! else conflates host with target and rots the cross-targeting story.
//!
//! The triple grammar is a closed set, not an LLVM triple parser. Adding a
//! target means adding a variant here and teaching the backend about it.

use std::fmt;

/// Instruction set architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    Arm64,
    X86_64,
}

/// Operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    MacOs,
    FreeBsd,
    Linux,
}

/// C library flavor. Only meaningful for Linux; everywhere else the OS has
/// exactly one libc and the value is `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Libc {
    Default,
    Gnu,
    Musl,
}

/// Object file format, derived from the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectFormat {
    MachO,
    Elf,
}

/// The complete target identity threaded from the CLI through the driver,
/// preprocessor, and codegen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetSpec {
    pub arch: Arch,
    pub os: Os,
    pub libc: Libc,
}

/// Canonical spellings, in the order shown in diagnostics.
pub const SUPPORTED_TARGETS: &[&str] = &[
    "arm64-macos",
    "x86_64-freebsd",
    "x86_64-linux-gnu",
    "x86_64-linux-musl",
];

impl TargetSpec {
    /// The build host as a target. The single sanctioned use of compile-time
    /// `cfg!(target_*)` in the workspace.
    pub fn host() -> Self {
        let arch = if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else if cfg!(target_arch = "x86_64") {
            Arch::X86_64
        } else {
            panic!("armfortas does not support this host architecture");
        };
        let os = if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "freebsd") {
            Os::FreeBsd
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            panic!("armfortas does not support this host operating system");
        };
        let libc = if os == Os::Linux {
            if cfg!(target_env = "musl") {
                Libc::Musl
            } else {
                Libc::Gnu
            }
        } else {
            Libc::Default
        };
        TargetSpec { arch, os, libc }
    }

    /// Parse a target triple. Closed grammar over `SUPPORTED_TARGETS` plus
    /// spelling aliases: `aarch64` for `arm64`, `amd64` for `x86_64`
    /// (FreeBSD's `uname -m` says `amd64`), `darwin` for `macos`, and bare
    /// `x86_64-linux` for `x86_64-linux-gnu`.
    pub fn parse(triple: &str) -> Result<Self, String> {
        let reject = || {
            format!(
                "unknown target '{}'; supported targets: {}",
                triple,
                SUPPORTED_TARGETS.join(", ")
            )
        };

        let mut parts = triple.split('-');
        let arch = match parts.next() {
            Some("arm64") | Some("aarch64") => Arch::Arm64,
            Some("x86_64") | Some("amd64") => Arch::X86_64,
            _ => return Err(reject()),
        };
        let os = match parts.next() {
            Some("macos") | Some("darwin") => Os::MacOs,
            Some("freebsd") => Os::FreeBsd,
            Some("linux") => Os::Linux,
            _ => return Err(reject()),
        };
        let libc = match (os, parts.next()) {
            (Os::Linux, None) | (Os::Linux, Some("gnu")) => Libc::Gnu,
            (Os::Linux, Some("musl")) => Libc::Musl,
            (_, None) => Libc::Default,
            _ => return Err(reject()),
        };
        if parts.next().is_some() {
            return Err(reject());
        }

        let spec = TargetSpec { arch, os, libc };
        // The closed set: combinations outside SUPPORTED_TARGETS are rejected
        // even when each component parses (e.g. arm64-linux is x15's, not
        // x00's).
        if !SUPPORTED_TARGETS.contains(&spec.triple()) {
            return Err(reject());
        }
        Ok(spec)
    }

    /// Canonical triple spelling.
    pub fn triple(&self) -> &'static str {
        match (self.arch, self.os, self.libc) {
            (Arch::Arm64, Os::MacOs, _) => "arm64-macos",
            (Arch::X86_64, Os::FreeBsd, _) => "x86_64-freebsd",
            (Arch::X86_64, Os::Linux, Libc::Musl) => "x86_64-linux-musl",
            (Arch::X86_64, Os::Linux, _) => "x86_64-linux-gnu",
            // Unreachable through parse()/host(); spelled out so a new
            // variant fails to compile until it gets a canonical name.
            (Arch::Arm64, Os::FreeBsd, _) | (Arch::Arm64, Os::Linux, _) => "arm64-unsupported",
            (Arch::X86_64, Os::MacOs, _) => "x86_64-unsupported",
        }
    }

    pub fn object_format(&self) -> ObjectFormat {
        match self.os {
            Os::MacOs => ObjectFormat::MachO,
            Os::FreeBsd | Os::Linux => ObjectFormat::Elf,
        }
    }
}

impl fmt::Display for TargetSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.triple())
    }
}

/// Sizes and alignments the frontend computes with (sprint x02). Plain
/// data derived from a `TargetSpec` — no trait, no global, no Default;
/// layout threads explicitly from the driver down.
///
/// Both supported arches are LP64, little-endian, IEEE-754 with the
/// same natural alignments, so `of()` returns identical values today.
/// The match on arch is the seam where a future ILP32 or wider-vector
/// target would differ in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetLayout {
    pub ptr_bytes: usize,
    pub ptr_align: usize,
    pub bool_bytes: usize,
    pub i128_align: usize,
    /// Vector register width. NEON and SSE2 baseline are both 128-bit;
    /// AVX is a capability query (sprint x10), not a layout field.
    pub vector_bytes: usize,
    pub max_scalar_align: usize,
}

/// Closure slots reserved alongside a procedure-pointer component.
/// Pre-dates x02 (`PROC_PTR_CLOSURE_SLOTS` in sema); restated here so
/// `proc_ptr_component()` records the derivation from pointer width.
const PROC_PTR_CLOSURE_SLOTS: usize = 8;

impl TargetLayout {
    /// The LP64 layout every supported target shares (the coincidence
    /// asserted by `layouts_coincide_across_supported_targets`). For
    /// unit tests building IR modules directly; production code derives
    /// layout from the selected target via `of()`.
    pub const LP64: TargetLayout = TargetLayout {
        ptr_bytes: 8,
        ptr_align: 8,
        bool_bytes: 1,
        i128_align: 16,
        vector_bytes: 16,
        max_scalar_align: 16,
    };

    pub fn of(spec: &TargetSpec) -> TargetLayout {
        match spec.arch {
            // Identical on purpose; see the struct doc.
            Arch::Arm64 | Arch::X86_64 => TargetLayout {
                ptr_bytes: 8,
                ptr_align: 8,
                bool_bytes: 1,
                i128_align: 16,
                vector_bytes: 16,
                max_scalar_align: 16,
            },
        }
    }

    /// `{base_addr, elem_size, rank, flags, dims[15]}` — the stable
    /// array-descriptor ABI. The runtime asserts 384 independently
    /// (`runtime/src/descriptor.rs`); if this formula and that assert
    /// ever disagree, the formula is wrong.
    pub fn array_descriptor(&self) -> (usize, usize) {
        // Header: base_addr (ptr) + elem_size (i64) + rank:i32/flags:u32
        // packed into one slot = 3 pointer-sized slots. Then 15 dims ×
        // (lower, upper, stride) = 45 slots. 8 × 48 = 384.
        (self.ptr_bytes * (3 + 45), self.ptr_align)
    }

    /// `{data, len, capacity, flags}` — the stable string-descriptor ABI.
    pub fn string_descriptor(&self) -> (usize, usize) {
        (self.ptr_bytes * 4, self.ptr_align)
    }

    /// `{data_ptr, type_tag}` carried for `class(T)` / `class(*)` /
    /// `type(*)` entities.
    pub fn class_descriptor(&self) -> (usize, usize) {
        (self.ptr_bytes * 2, self.ptr_align)
    }

    /// Procedure-pointer component: target pointer plus closure slots.
    pub fn proc_ptr_component(&self) -> usize {
        self.ptr_bytes * (1 + PROC_PTR_CLOSURE_SLOTS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_triples() {
        for &t in SUPPORTED_TARGETS {
            let spec = TargetSpec::parse(t).unwrap();
            assert_eq!(spec.triple(), t, "round-trip of {}", t);
        }
    }

    #[test]
    fn parses_aliases() {
        assert_eq!(
            TargetSpec::parse("aarch64-macos").unwrap().triple(),
            "arm64-macos"
        );
        assert_eq!(
            TargetSpec::parse("arm64-darwin").unwrap().triple(),
            "arm64-macos"
        );
        assert_eq!(
            TargetSpec::parse("amd64-freebsd").unwrap().triple(),
            "x86_64-freebsd"
        );
        assert_eq!(
            TargetSpec::parse("x86_64-linux").unwrap().triple(),
            "x86_64-linux-gnu"
        );
    }

    #[test]
    fn rejects_unknown_and_unsupported() {
        for bad in [
            "",
            "garbage",
            "arm64",
            "x86_64",
            "arm64-linux",   // x15, not yet
            "arm64-freebsd", // not planned
            "x86_64-macos",  // not planned
            "x86_64-freebsd-gnu",
            "arm64-macos-gnu",
            "x86_64-linux-uclibc",
            "riscv64-linux-gnu",
        ] {
            let err = TargetSpec::parse(bad).unwrap_err();
            assert!(
                err.contains("supported targets: arm64-macos"),
                "diagnostic for '{}' must list supported targets, got: {}",
                bad,
                err
            );
        }
    }

    #[test]
    fn host_is_a_supported_target() {
        let host = TargetSpec::host();
        assert!(SUPPORTED_TARGETS.contains(&host.triple()));
        // Cross-check arch/os against the build configuration. These cfg!
        // uses live in src/target.rs and are therefore inside the grep gate.
        if cfg!(target_os = "freebsd") {
            assert_eq!(host.triple(), "x86_64-freebsd");
        }
        if cfg!(target_os = "macos") {
            assert_eq!(host.triple(), "arm64-macos");
        }
    }

    #[test]
    fn object_format_follows_os() {
        assert_eq!(
            TargetSpec::parse("arm64-macos").unwrap().object_format(),
            ObjectFormat::MachO
        );
        assert_eq!(
            TargetSpec::parse("x86_64-freebsd").unwrap().object_format(),
            ObjectFormat::Elf
        );
        assert_eq!(
            TargetSpec::parse("x86_64-linux-musl")
                .unwrap()
                .object_format(),
            ObjectFormat::Elf
        );
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn layouts_coincide_across_supported_targets() {
        let layouts: Vec<TargetLayout> = SUPPORTED_TARGETS
            .iter()
            .map(|t| TargetLayout::of(&TargetSpec::parse(t).unwrap()))
            .collect();
        for pair in layouts.windows(2) {
            assert_eq!(pair[0], pair[1], "LP64 layout coincidence is the contract");
        }
    }

    #[test]
    fn descriptor_footprints_match_the_stable_abi() {
        let layout = TargetLayout::of(&TargetSpec::parse("arm64-macos").unwrap());
        // 384 is asserted independently by runtime/src/descriptor.rs;
        // these are the compiler-side halves of that contract.
        assert_eq!(layout.array_descriptor(), (384, 8));
        assert_eq!(layout.string_descriptor(), (32, 8));
        assert_eq!(layout.class_descriptor(), (16, 8));
        assert_eq!(layout.proc_ptr_component(), 72);
    }
}
