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
