use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const INLINE_CHAIN_RSS_CEILING_KB: u64 = 512 * 1024;

#[derive(Debug, Clone, Copy)]
struct CompileStats {
    elapsed: Duration,
    peak_rss_kb: Option<u64>,
}

fn compiler() -> PathBuf {
    option_env!("CARGO_BIN_EXE_armfortas")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            p.push("target/debug/armfortas");
            p
        })
}

fn unique_dir(stem: &str, size: usize) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "armfortas_compile_scaling_inline_{}_{}_{}_{}",
        stem,
        std::process::id(),
        size,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create inline scaling test dir");
    dir
}

fn inline_chain_source(functions: usize) -> String {
    assert!(functions >= 2);

    let mut src = String::new();
    src.push_str("module inline_chain_m\n  implicit none\ncontains\n");
    src.push_str("  integer function p0()\n    p0 = 0\n  end function p0\n");

    for idx in 1..functions {
        src.push_str(&format!(
            "  integer function p{}()\n    p{} = p{}() + 1\n  end function p{}\n",
            idx,
            idx,
            idx - 1,
            idx
        ));
    }

    src.push_str("end module inline_chain_m\n");
    src
}

#[cfg(target_os = "linux")]
fn read_peak_rss_kb(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmHWM:")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

#[cfg(not(target_os = "linux"))]
fn read_peak_rss_kb(_pid: u32) -> Option<u64> {
    None
}

fn compile_timed(compiler: &Path, src: &Path, ir: &Path, timeout: Duration) -> CompileStats {
    let start = Instant::now();
    let mut child = Command::new(compiler)
        .arg(src)
        .args(["-O2", "--emit-ir", "-o"])
        .arg(ir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn armfortas");

    let mut peak_rss_kb: Option<u64> = None;
    loop {
        if let Some(sample) = read_peak_rss_kb(child.id()) {
            peak_rss_kb = Some(peak_rss_kb.map_or(sample, |peak| peak.max(sample)));
        }

        if let Some(status) = child.try_wait().expect("poll armfortas") {
            assert!(status.success(), "armfortas compile failed with {status}");
            return CompileStats {
                elapsed: start.elapsed(),
                peak_rss_kb,
            };
        }

        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "armfortas compile exceeded {timeout:?} for {}",
                src.display()
            );
        }

        thread::sleep(Duration::from_millis(20));
    }
}

fn measure(functions: usize) -> CompileStats {
    let dir = unique_dir("chain", functions);
    let src = dir.join("inline_chain.f90");
    let ir = dir.join("inline_chain.ir");
    fs::write(&src, inline_chain_source(functions)).expect("write generated source");
    let stats = compile_timed(&compiler(), &src, &ir, Duration::from_secs(45));
    let _ = fs::remove_dir_all(dir);
    stats
}

#[test]
fn inline_chain_compile_time_and_rss_stay_bounded() {
    let _ = measure(20);

    let small = measure(100);
    let large = measure(200);
    let ceiling = small.elapsed.mul_f64(4.0) + Duration::from_secs(5);

    assert!(
        large.elapsed < ceiling,
        "O2 compile time for a 200-function inline chain should stay under a quadratic ceiling: \
         small={:?}, large={:?}, ceiling={:?}",
        small.elapsed,
        large.elapsed,
        ceiling
    );

    if let Some(peak_rss_kb) = large.peak_rss_kb {
        assert!(
            peak_rss_kb < INLINE_CHAIN_RSS_CEILING_KB,
            "O2 inline-chain compile used too much memory: peak={} KiB, ceiling={} KiB",
            peak_rss_kb,
            INLINE_CHAIN_RSS_CEILING_KB
        );
    }
}
