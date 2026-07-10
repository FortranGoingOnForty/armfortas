//! x16 rung 2 gate: afs-ld statically links real armfortas-compiled
//! Fortran on FreeBSD — crt1/crti/crtn + libarmfortas_rt.a + libc.a and
//! the native tail — into a working `-static` executable. Exercises the
//! whole rung-2 machinery end to end (archive selection, GOT, TLS,
//! IFUNC/IPLT, init arrays, symbol versioning).
//!
//! FreeBSD-scoped: /usr/lib carries crt1.o and the static libc set. Other
//! ELF hosts (glibc/musl static discovery) are a later story; non-ELF
//! hosts skip. Behavioral parity is checked against `ld -static`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn tool(names: &[&str]) -> Option<PathBuf> {
    names
        .iter()
        .find_map(|name| armfortas::testing::built_binary(name))
}

fn runtime_lib() -> Option<PathBuf> {
    armfortas::testing::built_runtime_archive()
}

/// FreeBSD crt + static libc set, or None (skip) when unavailable.
fn freebsd_static_inputs() -> Option<StaticInputs> {
    if !cfg!(target_os = "freebsd") {
        return None;
    }
    let need = |p: &str| {
        let pb = PathBuf::from(p);
        pb.exists().then_some(pb)
    };
    Some(StaticInputs {
        crt1: need("/usr/lib/crt1.o")?,
        crti: need("/usr/lib/crti.o")?,
        crtn: need("/usr/lib/crtn.o")?,
        libc: need("/usr/lib/libc.a")?,
        libgcc_eh: need("/usr/lib/libgcc_eh.a")?,
        libthr: need("/usr/lib/libthr.a")?,
    })
}

struct StaticInputs {
    crt1: PathBuf,
    crti: PathBuf,
    crtn: PathBuf,
    libc: PathBuf,
    libgcc_eh: PathBuf,
    libthr: PathBuf,
}

/// Assemble the FreeBSD static link argv for `linker`, producing `out`
/// from object `obj` + runtime `rt`.
fn static_link_args(
    inp: &StaticInputs,
    rt: &Path,
    obj: &Path,
    out: &Path,
    extra_front: &[&str],
) -> Vec<String> {
    let mut a: Vec<String> = extra_front.iter().map(|s| s.to_string()).collect();
    a.push("-static".into());
    a.push("-L/usr/lib".into());
    a.push("-o".into());
    a.push(out.display().to_string());
    for p in [&inp.crt1, &inp.crti] {
        a.push(p.display().to_string());
    }
    a.push(obj.display().to_string());
    a.push(rt.display().to_string());
    a.push("--start-group".into());
    for p in [&inp.libc, &inp.libgcc_eh, &inp.libthr] {
        a.push(p.display().to_string());
    }
    for l in [
        "-lm",
        "-lexecinfo",
        "-lkvm",
        "-lmemstat",
        "-lprocstat",
        "-ldevstat",
        "-lutil",
    ] {
        a.push(l.into());
    }
    a.push("--end-group".into());
    a.push(inp.crtn.display().to_string());
    a
}

fn skip(test: &str, reason: &str) {
    eprintln!("\nHARNESS_SKIP suite=elf_static_link test={test} count=1 reason=\"{reason}\"");
}

#[test]
fn afs_ld_statically_links_and_runs_fortran() {
    let Some(inp) = freebsd_static_inputs() else {
        skip(
            "afs_ld_statically_links_and_runs_fortran",
            "not a FreeBSD host with /usr/lib static crt+libc",
        );
        return;
    };
    let afs_ld = tool(&["afs-ld"]).expect("afs-ld binary not built for this test profile");
    let armfortas = tool(&["armfortas"]).expect("armfortas binary not built for this test profile");
    let rt = runtime_lib().expect("libarmfortas_rt.a not built for this test profile");

    let dir = std::env::temp_dir().join(format!("afs_static_gate_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("prog.f90");
    std::fs::write(
        &src,
        "program prog\n\
         implicit none\n\
         integer :: i\n\
         real(8), allocatable :: a(:)\n\
         real(8) :: s\n\
         allocate(a(8))\n\
         do i = 1, 8\n\
           a(i) = sqrt(real(i,8))\n\
         end do\n\
         s = sum(a)\n\
         print '(A,F0.3)', 'sum=', s\n\
         if (s > 15.0d0) print '(A)', 'OK'\n\
         deallocate(a)\n\
         end program\n",
    )
    .unwrap();

    // Compile to a relocatable object with armfortas.
    let obj = dir.join("prog.o");
    let c = Command::new(&armfortas)
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("run armfortas");
    assert!(
        c.status.success(),
        "armfortas -c: {}",
        String::from_utf8_lossy(&c.stderr)
    );

    // Link statically with afs-ld and run.
    let out = dir.join("prog");
    let args = static_link_args(&inp, &rt, &obj, &out, &[]);
    let l = Command::new(&afs_ld)
        .args(&args)
        .output()
        .expect("run afs-ld");
    assert!(
        l.status.success(),
        "afs-ld static link: {}",
        String::from_utf8_lossy(&l.stderr)
    );

    let run = Command::new(&out).output().expect("run static binary");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        run.status.code(),
        Some(0),
        "exit code; stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        stdout.contains("sum=") && stdout.contains("OK"),
        "unexpected output: {stdout:?}"
    );

    // Byte-determinism across two afs-ld links.
    let out2 = dir.join("prog2");
    let args2 = static_link_args(&inp, &rt, &obj, &out2, &[]);
    assert!(Command::new(&afs_ld)
        .args(&args2)
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(
        std::fs::read(&out).unwrap(),
        std::fs::read(&out2).unwrap(),
        "afs-ld static output must be byte-deterministic"
    );

    // Behavioral parity with the reference static linker, if present.
    for ld in ["/usr/bin/ld", "/usr/local/bin/ld"] {
        if !Path::new(ld).exists() {
            continue;
        }
        let refo = dir.join(format!("prog_ref_{}", ld.replace('/', "_")));
        let refargs = static_link_args(&inp, &rt, &obj, &refo, &[]);
        let r = Command::new(ld).args(&refargs).output().unwrap();
        if !r.status.success() {
            continue; // reference linker rejected the set; not our concern
        }
        let refrun = Command::new(&refo).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&refrun.stdout),
            stdout,
            "{ld} static output must match afs-ld's"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
