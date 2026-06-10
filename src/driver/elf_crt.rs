//! ELF crt-object and dynamic-linker discovery (sprint x06).
//!
//! The driver owns the whole link line — crt objects, dynamic linker,
//! library set — and invokes `ld` directly. We deliberately do NOT
//! shell out to `cc -print-file-name=crt1.o`: that trades a probe list
//! we control for a dependency on a C compiler being installed and on
//! its sysroot matching ours. The closed probe list plus the `-B`
//! override covers the same ground deterministically.
//!
//! Override (first-class configuration, not an escape hatch):
//! `-B <dir>` or `AFS_CRT_DIR` is searched before the built-in list.
//! On NixOS there is no FHS crt location at all — crt1.o lives at a
//! nix-store glibc path — so the override IS the configuration there:
//! `armfortas -B "$(dirname "$(realpath /run/current-system/sw/lib/crt1.o 2>/dev/null || echo /nix/store/.../crt1.o)")"`,
//! or simply `-B "$(nix eval ...)"`-style resolution by the user.
//!
//! musl paths (`/lib/ld-musl-x86_64.so.1`, crt from the musl sysroot)
//! are wired in x11; `Libc::Musl` errors cleanly until then.

use std::path::{Path, PathBuf};

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
            // Closed list, first hit wins: Debian/Ubuntu multiarch,
            // then Fedora/RHEL. Unlisted layouts (NixOS!) use -B; the
            // answer to a new distro is the override, not another
            // hardcode.
            roots.push(PathBuf::from("/usr/lib/x86_64-linux-gnu"));
            roots.push(PathBuf::from("/usr/lib64"));
        }
        Os::MacOs => unreachable!(),
    }

    let find_in_roots =
        |name: &str| -> Option<PathBuf> { roots.iter().map(|r| r.join(name)).find(|p| p.exists()) };

    let crt1 = find_in_roots(crt1_name).ok_or_else(|| missing(crt1_name, &roots))?;
    let crti = find_in_roots("crti.o").ok_or_else(|| missing("crti.o", &roots))?;
    let crtn = find_in_roots("crtn.o").ok_or_else(|| missing("crtn.o", &roots))?;

    // crtbegin/crtend: same roots on FreeBSD; on Linux/glibc they live
    // in the GCC dir — probe the two documented layouts, highest
    // numeric version wins.
    let (crtbegin, crtend) =
        if let (Some(b), Some(e)) = (find_in_roots(begin_name), find_in_roots(end_name)) {
            (b, e)
        } else if target.os == Os::Linux {
            let gcc_dir = newest_gcc_dir(&[
                Path::new("/usr/lib/gcc/x86_64-linux-gnu"),
                Path::new("/usr/lib/gcc/x86_64-redhat-linux"),
            ])
            .ok_or_else(|| {
                format!(
                    "cannot find {}: no crt root has it and no GCC dir found under \
                 /usr/lib/gcc/x86_64-linux-gnu or .../x86_64-redhat-linux; \
                 pass -B <dir> (or set AFS_CRT_DIR) pointing at a directory \
                 containing the crt objects",
                    begin_name
                )
            })?;
            let b = gcc_dir.join(begin_name);
            let e = gcc_dir.join(end_name);
            if !b.exists() || !e.exists() {
                return Err(missing(begin_name, &[gcc_dir]));
            }
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

/// Highest-numbered version directory under the given GCC lib roots.
fn newest_gcc_dir(roots: &[&Path]) -> Option<PathBuf> {
    let mut best: Option<(u32, PathBuf)> = None;
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(major) = name
                .to_str()
                .and_then(|s| s.split('.').next())
                .and_then(|s| s.parse::<u32>().ok())
            {
                if best.as_ref().map(|(b, _)| major > *b).unwrap_or(true) {
                    best = Some((major, entry.path()));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Build the complete `ld` argv for one ELF executable link. Link
/// order is load-bearing: crt1/crti/crtbegin precede user objects;
/// libraries follow the runtime archive (`-lc` after
/// libarmfortas_rt.a or runtime externs go unresolved); crtend/crtn
/// close. `--eh-frame-hdr` because Rust unwinding needs
/// `PT_GNU_EH_FRAME`.
#[allow(clippy::too_many_arguments)]
pub fn elf_link_args(
    target: &TargetSpec,
    crt: &CrtSet,
    objects: &[PathBuf],
    runtime_lib: &Path,
    output: &Path,
    pie: bool,
    library_search_paths: &[PathBuf],
    link_libs: &[String],
) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = Vec::new();
    if pie {
        args.push("-pie".into());
    }
    args.push("--eh-frame-hdr".into());
    args.push("--dynamic-linker".into());
    args.push(dynamic_linker(target)?.to_string());
    args.push("-o".into());
    args.push(output.to_string_lossy().into_owned());
    args.push(crt.crt1.to_string_lossy().into_owned());
    args.push(crt.crti.to_string_lossy().into_owned());
    args.push(crt.crtbegin.to_string_lossy().into_owned());
    for obj in objects {
        args.push(obj.to_string_lossy().into_owned());
    }
    args.push(runtime_lib.to_string_lossy().into_owned());
    for dir in library_search_paths {
        args.push(format!("-L{}", dir.display()));
    }
    for lib in link_libs {
        args.push(format!("-l{}", lib));
    }
    // System search dirs after user -L: ld resolves -l left-to-right
    // through the -L list in argv order, so user dirs win. The crt1
    // dir rides along so a -B/AFS_CRT_DIR root (NixOS) also resolves
    // its libc; the crtbegin dir because on Debian/Ubuntu (and in nix
    // gcc store paths) the unversioned libgcc_s.so linker script lives
    // in the GCC dir, nowhere on the standard -L path.
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
        let crt = find_crt(&t("x86_64-freebsd"), &[root.clone()], true).unwrap();
        assert_eq!(crt.crt1, root.join("Scrt1.o"));
        assert_eq!(crt.crtbegin, root.join("crtbeginS.o"));
        let crt = find_crt(&t("x86_64-freebsd"), &[root.clone()], false).unwrap();
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
        let msg = missing("Scrt1.o", &[empty.clone()]);
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
    fn link_args_order_is_load_bearing() {
        let root = fake_crt_root("argv");
        let crt = find_crt(&t("x86_64-freebsd"), &[root.clone()], true).unwrap();
        let args = elf_link_args(
            &t("x86_64-freebsd"),
            &crt,
            &[PathBuf::from("/tmp/a.o"), PathBuf::from("/tmp/b.o")],
            Path::new("/rt/libarmfortas_rt.a"),
            Path::new("/tmp/out"),
            true,
            &[PathBuf::from("/userlibs")],
            &["foo".to_string()],
        )
        .unwrap();
        let expect_prefix = [
            "-pie",
            "--eh-frame-hdr",
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
        // crt1 < crti < crtbegin < objects < runtime < -lfoo < -lc < crtend < crtn
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
        assert!(crt1 < crti && crti < begin && begin < a_o && a_o < b_o && b_o < rt);
        assert!(rt < userdir && userdir < userlib && userlib < libc);
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
        let crt = find_crt(&t("x86_64-freebsd"), &[root.clone()], false).unwrap();
        let args = elf_link_args(
            &t("x86_64-freebsd"),
            &crt,
            &[PathBuf::from("/tmp/a.o")],
            Path::new("/rt/libarmfortas_rt.a"),
            Path::new("/tmp/out"),
            false,
            &[],
            &[],
        )
        .unwrap();
        assert!(!args.contains(&"-pie".to_string()));
        assert!(args.iter().any(|a| a.ends_with("/crt1.o")));
        assert!(args.iter().any(|a| a.ends_with("/crtbegin.o")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
