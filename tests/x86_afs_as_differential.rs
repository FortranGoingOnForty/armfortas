//! x14: whole-corpus assembler differential. Every ordinary test_programs
//! source in the exact corpus manifest is compiled to x86_64 assembly at
//! -O0/-O2/-O3, and each .s is assembled by both GNU as and the in-process
//! afs-as x86 pipeline
//! (parse -> encode -> relax -> ELF model). The two objects must
//! agree exactly on section contents and hold policy-equal
//! relocations and symbols (same normalization as afs-as's own
//! fixture differential: STT_SECTION symbols excluded, reloc targets
//! resolved to names).
//!
//! x86_64 ELF hosts with a GNU assembler only; skips loudly
//! elsewhere.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use afs_as::elf::{
    parse_elf, ObjectFile, SymbolPlace, ELFOSABI_FREEBSD, ELFOSABI_NONE, SHT_NOBITS, STT_SECTION,
};
use afs_as::x86::assemble::assemble_x86;
use armfortas::target::{Arch, ObjectFormat, TargetSpec};

const OPT_LEVELS: &[&str] = &["-O0", "-O2", "-O3"];
const CORPUS_MANIFEST: &str = include_str!("fixtures/x86_afs_as_differential_sources.txt");

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn programs_dir() -> PathBuf {
    for dir in ["test_programs", "../test_programs"] {
        if Path::new(dir).exists() {
            return PathBuf::from(dir);
        }
    }
    panic!("cannot find test_programs/");
}

fn manifest_programs() -> Vec<PathBuf> {
    let root = programs_dir();
    let mut manifest_names = BTreeSet::new();
    let mut compare_programs = Vec::new();
    let mut previous_name: Option<&str> = None;

    for (line_index, raw) in CORPUS_MANIFEST.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let class = fields
            .next()
            .unwrap_or_else(|| panic!("manifest line {} has no class", line_index + 1));
        let name = fields
            .next()
            .unwrap_or_else(|| panic!("manifest line {} has no source", line_index + 1));
        assert!(
            fields.next().is_none(),
            "manifest line {} has extra fields: {line}",
            line_index + 1
        );
        assert!(
            matches!(class, "compare" | "diagnostic"),
            "manifest line {} has unknown class {class:?}",
            line_index + 1
        );
        assert!(
            !name.contains('/') && (name.ends_with(".f90") || name.ends_with(".f")),
            "manifest line {} has invalid source identity {name:?}",
            line_index + 1
        );
        if let Some(previous) = previous_name {
            assert!(
                previous < name,
                "manifest must be strictly sorted: {previous:?} before {name:?}"
            );
        }
        previous_name = Some(name);
        assert!(
            manifest_names.insert(name.to_string()),
            "duplicate manifest source {name:?}"
        );

        let path = root.join(name);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read manifest source {}: {e}", path.display()));
        let is_diagnostic = source
            .lines()
            .any(|line| line.trim_start().starts_with("! ERROR_EXPECTED:"));
        assert_eq!(
            class == "diagnostic",
            is_diagnostic,
            "manifest classification disagrees with ERROR_EXPECTED for {name}"
        );
        if class == "compare" {
            compare_programs.push(path);
        }
    }

    let discovered_names: BTreeSet<String> = std::fs::read_dir(&root)
        .expect("read test_programs")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "f90" || extension == "f")
        })
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        manifest_names, discovered_names,
        "x86 differential corpus manifest does not exactly match top-level test_programs sources"
    );
    assert!(
        compare_programs.len() > 400,
        "x86 differential comparison corpus shrank to {} sources",
        compare_programs.len()
    );
    compare_programs
}

#[test]
fn corpus_manifest_matches_test_programs() {
    let programs = manifest_programs();
    assert!(
        !programs.is_empty(),
        "x86 differential manifest has no comparison sources"
    );
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

/// Rewrite maximal runs of the gas 1..=11-byte NOP-filler patterns
/// as repeated 0x90. binutils changed the fill split order between
/// 2.44 (longest-first) and 2.46 (remainder-first); padding is not
/// architectural output, so it is compared modulo that choice. Same
/// rewrite as afs-as tests/common/elf.rs.
fn canonicalize_nop_fill(text: &[u8]) -> Vec<u8> {
    const NOPS: [&[u8]; 11] = [
        &[
            0x66, 0x66, 0x2e, 0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
        &[0x66, 0x2e, 0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
        &[0x66, 0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
        &[0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
        &[0x0f, 0x1f, 0x80, 0x00, 0x00, 0x00, 0x00],
        &[0x66, 0x0f, 0x1f, 0x44, 0x00, 0x00],
        &[0x0f, 0x1f, 0x44, 0x00, 0x00],
        &[0x0f, 0x1f, 0x40, 0x00],
        &[0x0f, 0x1f, 0x00],
        &[0x66, 0x90],
        &[0x90],
    ];
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    'outer: while i < text.len() {
        for pat in NOPS {
            if text[i..].starts_with(pat) {
                out.resize(out.len() + pat.len(), 0x90);
                i += pat.len();
                continue 'outer;
            }
        }
        out.push(text[i]);
        i += 1;
    }
    out
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
                if sec.name == ".text" {
                    canonicalize_nop_fill(&sec.data)
                } else {
                    sec.data.clone()
                },
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
                        let first =
                            g.2.iter()
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
    let programs = manifest_programs();

    let tmp = std::env::temp_dir().join(format!("afs_x86_corpdiff_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("mkdir tmp");

    let work: Vec<(usize, &PathBuf, &str)> = programs
        .iter()
        .enumerate()
        .flat_map(|(i, p)| OPT_LEVELS.iter().map(move |o| (i, p, *o)))
        .collect();
    let totals = Mutex::new(Totals {
        compared: 0,
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
                    totals.lock().unwrap().failures.push(format!(
                        "{}: ordinary corpus source failed compiler -S:\n{}",
                        tag,
                        String::from_utf8_lossy(&r.stderr)
                    ));
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
        "x86_afs_as_differential: {}/{} exact manifest objects compared, {} failures",
        t.compared,
        work.len(),
        t.failures.len()
    );
    let report = Path::new("target").join("x86_afs_as_differential_report.txt");
    if t.failures.is_empty() {
        assert_eq!(
            t.compared,
            work.len(),
            "successful differential did not compare every exact manifest cell"
        );
        std::fs::remove_file(&report).ok(); // don't leave a stale report
    } else {
        std::fs::write(&report, t.failures.join("\n\n")).ok();
        panic!(
            "{} differential failures (full report: {}):\n\n{}",
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
