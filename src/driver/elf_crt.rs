//! ELF crt-object and dynamic-linker discovery (sprint x06).
//!
//! The driver owns the whole link line — crt objects, dynamic linker,
//! library set — and invokes `ld` directly. We deliberately do NOT
//! shell out to `cc -print-file-name=crt1.o`: that trades filesystem
//! discovery we control for a dependency on a C compiler being installed
//! and on its sysroot matching ours. Built-in FHS probes cover common
//! distro layouts; `-B` remains the explicit override for non-FHS roots.
//!
//! Override (first-class configuration, not an escape hatch):
//! `-B <dir>` (repeatable) or `AFS_CRT_DIR` (colon-separated) is
//! searched before the built-in list, and `LIBRARY_PATH` (the
//! cc-compatible env knob) adds `-L` dirs. On NixOS there is no FHS
//! crt location at all and the pieces live in three store paths —
//! verified recipe (hasu, 2026-06-10):
//!
//! ```sh
//! armfortas -B <glibc>/lib -B <gcc>/lib/gcc/x86_64-unknown-linux-gnu/<ver> \
//!           -L <gcc-libgcc>/lib hello.f90 -o hello   # or the env forms
//! ```
//!
//! musl paths (`/lib/ld-musl-x86_64.so.1`, crt from the musl sysroot)
//! are wired in x11; `Libc::Musl` errors cleanly until then.

use std::path::{Path, PathBuf};

use super::LinkOperand;
use crate::target::{Libc, Os, TargetSpec};

/// The crt objects one PIE link needs, in link order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrtSet {
    /// Scrt1.o (PIE) or crt1.o (-no-pie). The PIE flag and the crt1
    /// variant are selected together — mixing crt1.o with `-pie`
    /// segfaults at startup on some glibc versions instead of failing
    /// to link.
    pub crt1: PathBuf,
    pub crti: PathBuf,
    /// `__dso_handle` lives here; Rust std references it for TLS
    /// destructors, so trivial programs link without it and the first
    /// thread-local breaks at runtime. Always included.
    pub crtbegin: PathBuf,
    pub crtend: PathBuf,
    pub crtn: PathBuf,
}

/// Dynamic linker path for the target.
pub fn dynamic_linker(target: &TargetSpec) -> Result<&'static str, String> {
    match (target.os, target.libc) {
        (Os::FreeBsd, _) => Ok("/libexec/ld-elf.so.1"),
        (Os::Linux, Libc::Gnu | Libc::Default) => Ok("/lib64/ld-linux-x86-64.so.2"),
        (Os::Linux, Libc::Musl) => {
            Err("x86_64-linux-musl linking (ld-musl, musl crt set) lands in sprint x11".to_string())
        }
        (Os::MacOs, _) => unreachable!("Mach-O targets do not take the ELF link path"),
    }
}

/// Native libraries the Rust runtime staticlib needs, measured per
/// platform with `cargo rustc -p armfortas-rt -- --print
/// native-static-libs` (2026-06-10; re-measure when the runtime grows
/// a dependency). Order preserved as printed; duplicates are harmless
/// for shared libraries.
pub fn native_libs(target: &TargetSpec) -> &'static [&'static str] {
    match target.os {
        // FreeBSD 15, rustc 1.95:
        Os::FreeBsd => &[
            "-lexecinfo",
            "-lpthread",
            "-lgcc_s",
            "-lc",
            "-lm",
            "-lrt",
            "-lutil",
            "-lkvm",
            "-lmemstat",
            "-lprocstat",
            "-ldevstat",
        ],
        // glibc (measured on NixOS, rustc 1.95; glibc >= 2.34 folds
        // most of the historical tail into libc):
        Os::Linux => &[
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ],
        Os::MacOs => unreachable!("Mach-O targets do not take the ELF link path"),
    }
}

/// System library search directories. `ld` invoked directly searches
/// nothing by default — the `-L/usr/lib` everyone takes for granted is
/// something `cc` adds — so the driver supplies the per-OS list.
/// Nonexistent dirs are filtered at use; lld warns on missing -L dirs.
fn system_lib_dirs(target: &TargetSpec) -> &'static [&'static str] {
    match target.os {
        Os::FreeBsd => &["/usr/lib", "/lib"],
        Os::Linux => &[
            "/usr/lib/x86_64-linux-gnu",
            "/lib/x86_64-linux-gnu",
            "/usr/lib64",
            "/lib64",
            "/usr/lib",
            "/lib",
        ],
        Os::MacOs => unreachable!("Mach-O targets do not take the ELF link path"),
    }
}

/// Locate the crt set for `target`, searching `override_dirs` (from
/// `-B` / `AFS_CRT_DIR`) before the per-OS built-in list.
pub fn find_crt(
    target: &TargetSpec,
    override_dirs: &[PathBuf],
    pie: bool,
) -> Result<CrtSet, String> {
    let crt1_name = if pie { "Scrt1.o" } else { "crt1.o" };
    let begin_name = if pie { "crtbeginS.o" } else { "crtbegin.o" };
    let end_name = if pie { "crtendS.o" } else { "crtend.o" };

    // Roots that carry crt1/crti/crtn.
    let mut roots: Vec<PathBuf> = override_dirs.to_vec();
    match target.os {
        Os::FreeBsd => roots.push(PathBuf::from("/usr/lib")),
        Os::Linux => {
            // First hit wins: Debian/Ubuntu multiarch, lib64-style
            // layouts, then plain /usr/lib used by Arch/CachyOS.
            roots.push(PathBuf::from("/usr/lib/x86_64-linux-gnu"));
            roots.push(PathBuf::from("/usr/lib64"));
            roots.push(PathBuf::from("/usr/lib"));
            roots.push(PathBuf::from("/lib/x86_64-linux-gnu"));
            roots.push(PathBuf::from("/lib64"));
            roots.push(PathBuf::from("/lib"));
        }
        Os::MacOs => unreachable!(),
    }

    let find_in_roots =
        |name: &str| -> Option<PathBuf> { roots.iter().map(|r| r.join(name)).find(|p| p.exists()) };

    let crt1 = find_in_roots(crt1_name).ok_or_else(|| missing(crt1_name, &roots))?;
    let crti = find_in_roots("crti.o").ok_or_else(|| missing("crti.o", &roots))?;
    let crtn = find_in_roots("crtn.o").ok_or_else(|| missing("crtn.o", &roots))?;

    // crtbegin/crtend: same roots on FreeBSD; on Linux/glibc they live
    // in the GCC dir. Probe common triples plus host-like target
    // directories discovered under /usr/lib/gcc; highest numeric
    // version wins.
    let (crtbegin, crtend) =
        if let (Some(b), Some(e)) = (find_in_roots(begin_name), find_in_roots(end_name)) {
            (b, e)
        } else if target.os == Os::Linux {
            let gcc_roots = linux_gcc_roots();
            let gcc_dir = newest_gcc_dir(&gcc_roots, begin_name, end_name).ok_or_else(|| {
                format!(
                    "cannot find {} and {}: no crt root has the pair and no complete \
                     GCC crt pair was found under {}; \
                     pass -B <dir> (or set AFS_CRT_DIR) pointing at a directory \
                     containing the crt objects",
                    begin_name,
                    end_name,
                    format_roots(&gcc_roots)
                )
            })?;
            let b = gcc_dir.join(begin_name);
            let e = gcc_dir.join(end_name);
            (b, e)
        } else {
            return Err(missing(begin_name, &roots));
        };

    Ok(CrtSet {
        crt1,
        crti,
        crtbegin,
        crtend,
        crtn,
    })
}

fn missing(name: &str, roots: &[PathBuf]) -> String {
    format!(
        "cannot find {}: searched {}; pass -B <dir> (or set AFS_CRT_DIR) \
         pointing at a directory containing the crt objects — required on \
         layouts without an FHS crt location (e.g. NixOS, where crt1.o \
         lives in the nix store)",
        name,
        roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    )
}

fn format_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn linux_gcc_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/usr/lib/gcc/x86_64-linux-gnu"),
        PathBuf::from("/usr/lib/gcc/x86_64-redhat-linux"),
        PathBuf::from("/usr/lib/gcc/x86_64-pc-linux-gnu"),
        PathBuf::from("/usr/lib/gcc/x86_64-unknown-linux-gnu"),
    ];

    let mut discovered = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/usr/lib/gcc") {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("x86_64-")
                && name.contains("linux")
                && !roots.iter().any(|root| root == &path)
            {
                discovered.push(path);
            }
        }
    }
    discovered.sort();
    discovered.dedup();
    roots.extend(discovered);

    roots
}

fn parse_gcc_version(name: &std::ffi::OsStr) -> Option<Vec<u32>> {
    let name = name.to_str()?;
    if name.is_empty() {
        return None;
    }

    let mut components = Vec::new();
    for component in name.split('.') {
        if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        components.push(component.parse().ok()?);
    }
    while components.len() > 1 && components.last() == Some(&0) {
        components.pop();
    }
    Some(components)
}

/// Highest-numbered complete GCC crt directory under the given roots.
///
/// Root order is the deterministic tie-breaker for two installations with
/// the same numeric version. Within one root, the lexical path is the final
/// tie-breaker for equivalent spellings such as `16` and `16.0`.
fn newest_gcc_dir(roots: &[PathBuf], begin_name: &str, end_name: &str) -> Option<PathBuf> {
    let mut best: Option<(Vec<u32>, usize, PathBuf)> = None;
    for (root_index, root) in roots.iter().enumerate() {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join(begin_name).is_file() || !path.join(end_name).is_file()
            {
                continue;
            }
            let Some(version) = parse_gcc_version(&entry.file_name()) else {
                continue;
            };
            let replace = match &best {
                None => true,
                Some((best_version, best_root_index, best_path)) => {
                    version > *best_version
                        || (version == *best_version
                            && (root_index < *best_root_index
                                || (root_index == *best_root_index && path < *best_path)))
                }
            };
            if replace {
                best = Some((version, root_index, path));
            }
        }
    }
    best.map(|(_, _, path)| path)
}

/// Build the complete `ld` argv for one ELF executable link. Link
/// order is load-bearing: crt1/crti/crtbegin precede the ordered user
/// operand stream; the runtime and its native libraries follow it
/// (`-lc` after libarmfortas_rt.a or runtime externs go unresolved);
/// crtend/crtn close. `--eh-frame-hdr` because Rust unwinding needs
/// `PT_GNU_EH_FRAME`; `--gc-sections` keeps the coarse Rust runtime
/// archive from dragging unused intrinsic surfaces into small binaries.
pub fn elf_link_args(
    target: &TargetSpec,
    crt: &CrtSet,
    operands: &[LinkOperand],
    runtime_lib: &Path,
    output: &Path,
    pie: bool,
    library_search_paths: &[PathBuf],
) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = Vec::new();
    if pie {
        args.push("-pie".into());
    }
    args.push("--eh-frame-hdr".into());
    args.push("--gc-sections".into());
    args.push("--dynamic-linker".into());
    args.push(dynamic_linker(target)?.to_string());
    args.push("-o".into());
    args.push(output.to_string_lossy().into_owned());
    args.push(crt.crt1.to_string_lossy().into_owned());
    args.push(crt.crti.to_string_lossy().into_owned());
    args.push(crt.crtbegin.to_string_lossy().into_owned());

    // Search paths precede every user library while preserving user-before-
    // system precedence. GNU ld applies -L globally, but putting them first
    // also preserves the contract on linkers that process argv strictly.
    for dir in library_search_paths {
        args.push(format!("-L{}", dir.display()));
    }
    // The crt1 dir rides along so a -B/AFS_CRT_DIR root (NixOS) also
    // resolves its libc; the crtbegin dir because on Debian/Ubuntu (and in
    // nix gcc store paths) the unversioned libgcc_s.so linker script lives in
    // the GCC dir, nowhere on the standard -L path.
    let mut lib_dirs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = crt.crt1.parent() {
        lib_dirs.push(parent.to_path_buf());
    }
    if let Some(parent) = crt.crtbegin.parent() {
        if !lib_dirs.contains(&parent.to_path_buf()) {
            lib_dirs.push(parent.to_path_buf());
        }
    }
    lib_dirs.extend(system_lib_dirs(target).iter().map(PathBuf::from));
    lib_dirs.dedup();
    for dir in lib_dirs.iter().filter(|d| d.is_dir()) {
        args.push(format!("-L{}", dir.display()));
    }

    for operand in operands {
        match operand {
            LinkOperand::Input(path) => args.push(path.to_string_lossy().into_owned()),
            LinkOperand::Library(name) => args.push(format!("-l{name}")),
        }
    }
    args.push(runtime_lib.to_string_lossy().into_owned());
    for lib in native_libs(target) {
        args.push((*lib).to_string());
    }
    args.push(crt.crtend.to_string_lossy().into_owned());
    args.push(crt.crtn.to_string_lossy().into_owned());
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(triple: &str) -> TargetSpec {
        TargetSpec::parse(triple).unwrap()
    }

    #[test]
    fn dynamic_linker_table() {
        assert_eq!(
            dynamic_linker(&t("x86_64-freebsd")).unwrap(),
            "/libexec/ld-elf.so.1"
        );
        assert_eq!(
            dynamic_linker(&t("x86_64-linux-gnu")).unwrap(),
            "/lib64/ld-linux-x86-64.so.2"
        );
        let err = dynamic_linker(&t("x86_64-linux-musl")).unwrap_err();
        assert!(err.contains("x11"), "musl error should name x11: {}", err);
    }

    #[test]
    fn native_libs_end_with_libc_family() {
        for triple in ["x86_64-freebsd", "x86_64-linux-gnu"] {
            let libs = native_libs(&t(triple));
            assert!(libs.contains(&"-lc"), "{} set lacks -lc", triple);
            assert!(libs.contains(&"-lm"), "{} set lacks -lm", triple);
        }
    }

    /// A fake crt root exercising the override-first path: every name
    /// (PIE and non-PIE variants) present in one tempdir.
    fn fake_crt_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("afs_crt_fake_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "Scrt1.o",
            "crt1.o",
            "crti.o",
            "crtn.o",
            "crtbeginS.o",
            "crtbegin.o",
            "crtendS.o",
            "crtend.o",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        dir
    }

    #[test]
    fn find_crt_prefers_override_dir() {
        let root = fake_crt_root("override");
        let crt = find_crt(&t("x86_64-freebsd"), std::slice::from_ref(&root), true).unwrap();
        assert_eq!(crt.crt1, root.join("Scrt1.o"));
        assert_eq!(crt.crtbegin, root.join("crtbeginS.o"));
        let crt = find_crt(&t("x86_64-freebsd"), std::slice::from_ref(&root), false).unwrap();
        assert_eq!(crt.crt1, root.join("crt1.o"));
        assert_eq!(crt.crtend, root.join("crtend.o"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_crt_missing_error_names_the_override() {
        let empty = std::env::temp_dir().join(format!("afs_crt_empty_{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        // Probing only the empty dir is not constructible for the
        // built-in roots, but a Linux target on a box without the
        // multiarch layout exercises the message via override-only
        // miss when the built-ins are absent too. Assert on the
        // message text from missing() directly to stay host-neutral.
        let msg = missing("Scrt1.o", std::slice::from_ref(&empty));
        assert!(msg.contains("-B"), "error should name -B: {}", msg);
        assert!(
            msg.contains("AFS_CRT_DIR"),
            "error should name AFS_CRT_DIR: {}",
            msg
        );
        assert!(msg.contains("NixOS"), "error should name NixOS: {}", msg);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn newest_gcc_dir_accepts_pc_linux_layout() {
        let root = std::env::temp_dir().join(format!("afs_gcc_pc_linux_{}", std::process::id()));
        let target_root = root.join("x86_64-pc-linux-gnu");
        let older = target_root.join("15.3.0");
        let newer = target_root.join("16");
        std::fs::create_dir_all(&older).unwrap();
        std::fs::create_dir_all(&newer).unwrap();
        for dir in [&older, &newer] {
            std::fs::write(dir.join("crtbeginS.o"), b"").unwrap();
            std::fs::write(dir.join("crtendS.o"), b"").unwrap();
        }

        assert_eq!(
            newest_gcc_dir(&[target_root], "crtbeginS.o", "crtendS.o").unwrap(),
            newer
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn newest_gcc_dir_compares_complete_numeric_versions() {
        let root =
            std::env::temp_dir().join(format!("afs_gcc_numeric_version_{}", std::process::id()));
        let first_root = root.join("first");
        let second_root = root.join("second");
        let older = first_root.join("15.9.0");
        let newer = second_root.join("15.10.0");
        std::fs::create_dir_all(&older).unwrap();
        std::fs::create_dir_all(&newer).unwrap();
        for dir in [&older, &newer] {
            std::fs::write(dir.join("crtbeginS.o"), b"").unwrap();
            std::fs::write(dir.join("crtendS.o"), b"").unwrap();
        }

        assert_eq!(
            newest_gcc_dir(&[first_root, second_root], "crtbeginS.o", "crtendS.o").unwrap(),
            newer
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn newest_gcc_dir_skips_incomplete_newer_installations() {
        let root =
            std::env::temp_dir().join(format!("afs_gcc_complete_pair_{}", std::process::id()));
        let complete_root = root.join("complete");
        let missing_end_root = root.join("missing-end");
        let missing_begin_root = root.join("missing-begin");
        let complete = complete_root.join("15.10.0");
        let missing_end = missing_end_root.join("16.0.0");
        let missing_begin = missing_begin_root.join("17.0.0");
        std::fs::create_dir_all(&complete).unwrap();
        std::fs::create_dir_all(&missing_end).unwrap();
        std::fs::create_dir_all(&missing_begin).unwrap();
        std::fs::write(complete.join("crtbeginS.o"), b"").unwrap();
        std::fs::write(complete.join("crtendS.o"), b"").unwrap();
        std::fs::write(missing_end.join("crtbeginS.o"), b"").unwrap();
        std::fs::write(missing_begin.join("crtendS.o"), b"").unwrap();

        assert_eq!(
            newest_gcc_dir(
                &[complete_root, missing_end_root, missing_begin_root],
                "crtbeginS.o",
                "crtendS.o"
            )
            .unwrap(),
            complete
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn newest_gcc_dir_uses_the_requested_crt_variant_and_stable_ties() {
        let root =
            std::env::temp_dir().join(format!("afs_gcc_requested_pair_{}", std::process::id()));
        let preferred_root = root.join("preferred");
        let fallback_root = root.join("fallback");
        let preferred = preferred_root.join("16.0");
        let fallback = fallback_root.join("16");
        std::fs::create_dir_all(&preferred).unwrap();
        std::fs::create_dir_all(&fallback).unwrap();
        for dir in [&preferred, &fallback] {
            std::fs::write(dir.join("crtbeginS.o"), b"").unwrap();
            std::fs::write(dir.join("crtendS.o"), b"").unwrap();
        }
        std::fs::write(fallback.join("crtbegin.o"), b"").unwrap();
        std::fs::write(fallback.join("crtend.o"), b"").unwrap();

        assert_eq!(
            newest_gcc_dir(
                &[preferred_root.clone(), fallback_root.clone()],
                "crtbeginS.o",
                "crtendS.o"
            )
            .unwrap(),
            preferred
        );
        assert_eq!(
            newest_gcc_dir(&[preferred_root, fallback_root], "crtbegin.o", "crtend.o").unwrap(),
            fallback
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gcc_version_parser_rejects_ambiguous_names_and_component_overflow() {
        assert_eq!(
            parse_gcc_version(std::ffi::OsStr::new("15.10.0")),
            Some(vec![15, 10])
        );
        assert_eq!(
            parse_gcc_version(std::ffi::OsStr::new("0015.010.1")),
            Some(vec![15, 10, 1])
        );
        for invalid in [
            "",
            ".16",
            "16.",
            "16..1",
            "16-rc1",
            "16.backup",
            "4294967296",
        ] {
            assert_eq!(
                parse_gcc_version(std::ffi::OsStr::new(invalid)),
                None,
                "{invalid:?} must not be treated as a GCC version"
            );
        }
    }

    #[test]
    fn link_args_order_is_load_bearing() {
        let root = fake_crt_root("argv");
        let crt = find_crt(&t("x86_64-freebsd"), std::slice::from_ref(&root), true).unwrap();
        let args = elf_link_args(
            &t("x86_64-freebsd"),
            &crt,
            &[
                LinkOperand::Input(PathBuf::from("/tmp/a.o")),
                LinkOperand::Library("foo".to_string()),
                LinkOperand::Input(PathBuf::from("/tmp/b.o")),
            ],
            Path::new("/rt/libarmfortas_rt.a"),
            Path::new("/tmp/out"),
            true,
            &[PathBuf::from("/userlibs")],
        )
        .unwrap();
        let expect_prefix = [
            "-pie",
            "--eh-frame-hdr",
            "--gc-sections",
            "--dynamic-linker",
            "/libexec/ld-elf.so.1",
            "-o",
            "/tmp/out",
        ];
        assert_eq!(&args[..expect_prefix.len()], &expect_prefix[..]);
        let pos = |needle: &str| {
            args.iter()
                .position(|a| a == needle)
                .unwrap_or_else(|| panic!("missing {} in {:?}", needle, args))
        };
        // Search paths precede an exactly ordered a.o, -lfoo, b.o stream;
        // the compiler runtime and native libraries follow that stream.
        let crt1 = pos(crt.crt1.to_str().unwrap());
        let crti = pos(crt.crti.to_str().unwrap());
        let begin = pos(crt.crtbegin.to_str().unwrap());
        let a_o = pos("/tmp/a.o");
        let b_o = pos("/tmp/b.o");
        let rt = pos("/rt/libarmfortas_rt.a");
        let userdir = pos("-L/userlibs");
        let userlib = pos("-lfoo");
        let libc = pos("-lc");
        let end = pos(crt.crtend.to_str().unwrap());
        let crtn = pos(crt.crtn.to_str().unwrap());
        assert!(crt1 < crti && crti < begin && begin < userdir && userdir < a_o);
        assert!(a_o < userlib && userlib < b_o && b_o < rt && rt < libc);
        assert!(libc < end && end < crtn && crtn == args.len() - 1);
        // User -L outranks every system -L (left-to-right resolution).
        if let Some(sys) = args
            .iter()
            .position(|a| a.starts_with("-L") && a != "-L/userlibs")
        {
            assert!(userdir < sys, "user -L must precede system -L: {:?}", args);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_pie_drops_the_flag_and_uses_crt1() {
        let root = fake_crt_root("nopie");
        let crt = find_crt(&t("x86_64-freebsd"), std::slice::from_ref(&root), false).unwrap();
        let args = elf_link_args(
            &t("x86_64-freebsd"),
            &crt,
            &[LinkOperand::Input(PathBuf::from("/tmp/a.o"))],
            Path::new("/rt/libarmfortas_rt.a"),
            Path::new("/tmp/out"),
            false,
            &[],
        )
        .unwrap();
        assert!(!args.contains(&"-pie".to_string()));
        assert!(args.iter().any(|a| a.ends_with("/crt1.o")));
        assert!(args.iter().any(|a| a.ends_with("/crtbegin.o")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
