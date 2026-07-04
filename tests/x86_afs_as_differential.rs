//! x14: whole-corpus assembler differential. Every test_programs
//! source is compiled to x86_64 assembly at -O0/-O2/-O3, and each .s
//! is assembled by both GNU as and the in-process afs-as x86 pipeline
//! (parse -> encode -> relax -> ELF model). The two objects must
//! agree exactly on section contents and hold policy-equal
//! relocations and symbols (same normalization as afs-as's own
//! fixture differential: STT_SECTION symbols excluded, reloc targets
//! resolved to names).
//!
//! x86_64 ELF hosts with a GNU assembler only; skips loudly
//! elsewhere.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use afs_as::elf::{
    parse_elf, ObjectFile, SymbolPlace, ELFOSABI_FREEBSD, ELFOSABI_NONE, SHT_NOBITS, STT_SECTION,
};
use afs_as::x86::assemble::assemble_x86;
use armfortas::target::{Arch, ObjectFormat, TargetSpec};

const OPT_LEVELS: &[&str] = &["-O0", "-O2", "-O3"];

fn compiler() -> PathBuf {
    for dir in ["target/debug", "../target/debug"] {
        let p = Path::new(dir).join("armfortas");
        if p.exists() {
            return p;
        }
    }
    panic!("armfortas binary not built — run cargo build first");
}

fn programs_dir() -> PathBuf {
    for dir in ["test_programs", "../test_programs"] {
        if Path::new(dir).exists() {
            return PathBuf::from(dir);
        }
    }
    panic!("cannot find test_programs/");
}

fn gas_path() -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(target_os = "freebsd") {
        &["/usr/local/bin/as"]
    } else if cfg!(target_os = "linux") {
        &["as"]
    } else {
        &[]
    };
    for cand in candidates {
        if let Ok(out) = Command::new(cand).arg("--version").output() {
            let banner = String::from_utf8_lossy(&out.stdout).to_string();
            if out.status.success() && banner.contains("GNU assembler") {
                return Some(PathBuf::from(cand));
            }
        }
    }
    None
}

fn host_osabi() -> u8 {
    if cfg!(target_os = "freebsd") {
        ELFOSABI_FREEBSD
    } else {
        ELFOSABI_NONE
    }
}

// Same policy as afs-as tests/common/elf.rs normalize().
type NormSections = BTreeMap<String, (u32, u64, Vec<u8>, u64)>;
type NormRelocs = Vec<(String, u64, u32, String, i64)>;
type NormSymbols = Vec<(String, u8, u8, String, u64, u64)>;

fn normalize(obj: &ObjectFile) -> (NormSections, NormRelocs, NormSymbols) {
    let mut sections = BTreeMap::new();
    let mut relocs = Vec::new();
    for sec in &obj.sections {
        sections.insert(
            sec.name.clone(),
            (
                sec.sh_type,
                sec.sh_flags,
                sec.data.clone(),
                if sec.sh_type == SHT_NOBITS {
                    sec.nobits_size
                } else {
                    0
                },
            ),
        );
        for r in &sec.relas {
            relocs.push((
                sec.name.clone(),
                r.offset,
                r.r_type,
                obj.symbols[r.symbol].name.clone(),
                r.addend,
            ));
        }
    }
    relocs.sort();
    let mut symbols: Vec<_> = obj
        .symbols
        .iter()
        .filter(|s| s.typ != STT_SECTION)
        .map(|s| {
            let place = match s.place {
                SymbolPlace::Undef => "<undef>".to_string(),
                SymbolPlace::Abs => "<abs>".to_string(),
                SymbolPlace::Common => "<common>".to_string(),
                SymbolPlace::Section(idx) => obj.sections[idx].name.clone(),
            };
            (s.name.clone(), s.bind, s.typ, place, s.value, s.size)
        })
        .collect();
    symbols.sort();
    (sections, relocs, symbols)
}

/// Compact first-divergence report, or None on agreement.
fn compare(tag: &str, gas: &ObjectFile, ours: &ObjectFile) -> Option<String> {
    let (gsec, grel, gsym) = normalize(gas);
    let (osec, orel, osym) = normalize(ours);
    if gsec != osec {
        for (name, g) in &gsec {
            match osec.get(name) {
                None => return Some(format!("{}: section {} missing from ours", tag, name)),
                Some(o) if o != g => {
                    if g.2 != o.2 {
                        let first = g
                            .2
                            .iter()
                            .zip(o.2.iter())
                            .position(|(a, b)| a != b)
                            .unwrap_or(g.2.len().min(o.2.len()));
                        let lo = first.saturating_sub(8);
                        return Some(format!(
                            "{}: {} bytes diverge at {} (gas len {}, ours {})\n  gas:  {:02x?}\n  ours: {:02x?}",
                            tag,
                            name,
                            first,
                            g.2.len(),
                            o.2.len(),
                            &g.2[lo..(first + 8).min(g.2.len())],
                            &o.2[lo..(first + 8).min(o.2.len())],
                        ));
                    }
                    return Some(format!(
                        "{}: {} header diverges gas=({},{:#x},len {},nobits {}) ours=({},{:#x},len {},nobits {})",
                        tag, name, g.0, g.1, g.2.len(), g.3, o.0, o.1, o.2.len(), o.3
                    ));
                }
                _ => {}
            }
        }
        let extra: Vec<_> = osec.keys().filter(|k| !gsec.contains_key(*k)).collect();
        return Some(format!("{}: extra sections in ours: {:?}", tag, extra));
    }
    if grel != orel {
        let n = grel.iter().zip(orel.iter()).position(|(a, b)| a != b);
        return Some(format!(
            "{}: relocs diverge at index {:?}\n  gas:  {:?}\n  ours: {:?}",
            tag,
            n,
            n.map(|i| grel.get(i)),
            n.map(|i| orel.get(i)),
        ));
    }
    if gsym != osym {
        let n = gsym.iter().zip(osym.iter()).position(|(a, b)| a != b);
        return Some(format!(
            "{}: symbols diverge at index {:?}\n  gas:  {:?}\n  ours: {:?}",
            tag,
            n,
            n.map(|i| gsym.get(i)),
            n.map(|i| osym.get(i)),
        ));
    }
    None
}

struct Totals {
    compared: usize,
    compile_skips: usize,
    failures: Vec<String>,
}

#[test]
fn whole_corpus_matches_gas() {
    let host = TargetSpec::host();
    if host.arch != Arch::X86_64 || host.object_format() != ObjectFormat::Elf {
        eprintln!(
            "\nHARNESS_SKIP suite=x86_afs_as_differential test=whole_corpus_matches_gas count=1 reason=\"needs an x86_64 ELF host\""
        );
        return;
    }
    let Some(gas) = gas_path() else {
        eprintln!(
            "\nHARNESS_SKIP suite=x86_afs_as_differential test=whole_corpus_matches_gas count=1 reason=\"no GNU assembler on this host\""
        );
        return;
    };

    let compiler = compiler();
    let mut programs: Vec<PathBuf> = std::fs::read_dir(programs_dir())
        .expect("read test_programs")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "f90" || x == "f"))
        .collect();
    programs.sort();
    assert!(programs.len() > 400, "corpus shrank? {}", programs.len());

    let tmp = std::env::temp_dir().join(format!("afs_x86_corpdiff_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("mkdir tmp");

    let work: Vec<(usize, &PathBuf, &str)> = programs
        .iter()
        .enumerate()
        .flat_map(|(i, p)| OPT_LEVELS.iter().map(move |o| (i, p, *o)))
        .collect();
    let totals = Mutex::new(Totals {
        compared: 0,
        compile_skips: 0,
        failures: Vec::new(),
    });
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(&(idx, program, opt)) = work.get(i) else {
                    return;
                };
                let stem = program.file_stem().unwrap().to_string_lossy();
                let tag = format!("{} {}", stem, opt);
                let asm_path = tmp.join(format!("{}_{}_{}.s", idx, stem, &opt[1..]));
                let obj_path = asm_path.with_extension("o");

                // Honor the harness `! FLAGS:` annotation (--std=f2023
                // tests and friends).
                let fsrc = std::fs::read_to_string(program).unwrap_or_default();
                let extra_flags: Vec<&str> = fsrc
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("! FLAGS:"))
                    .map(|rest| rest.split_whitespace().collect())
                    .unwrap_or_default();

                let r = Command::new(&compiler)
                    .arg(opt)
                    .args(&extra_flags)
                    .arg("-S")
                    .arg(program)
                    .arg("-o")
                    .arg(&asm_path)
                    .output()
                    .expect("run armfortas");
                if !r.status.success() {
                    // Compile-error tests are run_programs' business;
                    // count them so a mass regression still trips the
                    // floor assert below.
                    totals.lock().unwrap().compile_skips += 1;
                    return_clean(&asm_path, &obj_path);
                    continue;
                }
                let src = std::fs::read_to_string(&asm_path).expect("read asm");

                let g = Command::new(&gas)
                    .args(["--64", "-o"])
                    .arg(&obj_path)
                    .arg(&asm_path)
                    .output()
                    .expect("run gas");
                if !g.status.success() {
                    totals.lock().unwrap().failures.push(format!(
                        "{}: gas rejected backend output:\n{}",
                        tag,
                        String::from_utf8_lossy(&g.stderr)
                    ));
                    return_clean(&asm_path, &obj_path);
                    continue;
                }
                let gas_obj = match parse_elf(&std::fs::read(&obj_path).unwrap()) {
                    Ok(o) => o,
                    Err(e) => {
                        totals
                            .lock()
                            .unwrap()
                            .failures
                            .push(format!("{}: cannot lift gas object: {}", tag, e));
                        return_clean(&asm_path, &obj_path);
                        continue;
                    }
                };
                let ours = match assemble_x86(&src, host_osabi()) {
                    Ok(o) => o,
                    Err(e) => {
                        totals
                            .lock()
                            .unwrap()
                            .failures
                            .push(format!("{}: afs-as failed: {}", tag, e));
                        return_clean(&asm_path, &obj_path);
                        continue;
                    }
                };
                let mut t = totals.lock().unwrap();
                t.compared += 1;
                if let Some(f) = compare(&tag, &gas_obj, &ours) {
                    t.failures.push(f);
                }
                drop(t);
                return_clean(&asm_path, &obj_path);
            });
        }
    });

    std::fs::remove_dir_all(&tmp).ok();
    let t = totals.into_inner().unwrap();
    eprintln!(
        "x86_afs_as_differential: {} objects compared, {} compile skips, {} divergences",
        t.compared,
        t.compile_skips,
        t.failures.len()
    );
    assert!(
        t.compared >= 1550,
        "only {} objects compared ({} compile skips) — corpus or -S path broke",
        t.compared,
        t.compile_skips
    );
    let report = Path::new("target").join("x86_afs_as_differential_report.txt");
    if t.failures.is_empty() {
        std::fs::remove_file(&report).ok(); // don't leave a stale report
    } else {
        std::fs::write(&report, t.failures.join("\n\n")).ok();
        panic!(
            "{} divergences from gas (full report: {}):\n\n{}",
            t.failures.len(),
            report.display(),
            t.failures
                .iter()
                .take(25)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n\n")
        );
    }
}

fn return_clean(a: &Path, b: &Path) {
    std::fs::remove_file(a).ok();
    std::fs::remove_file(b).ok();
}
