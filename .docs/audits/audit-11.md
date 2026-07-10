# Audit 11 — test and CI integrity

Date: 2026-07-09
Superproject implementation: `23857aa48f3bc0160303842488e8578acb487fb1`
Submodules: `afs-as` `fac26fb9c1c4064b9bf838e393fc1d7363ff3409`; `afs-ld` `615de762090c8a9c73033ca1659b021cefe4331d`; `bencch` `8da8e1da967b5a641da822e8aff180587be93a33`

## Scope and method

This was a targeted, local review of the superproject and the checked-in CI/test surfaces in both toolchain submodules. Existing `.docs/audits` reports were not read. I inspected workflow commands and host gates, enumerated Cargo test targets, traced runtime/tool artifact lookup, and ran focused read-only checks and shell reproductions. I did not run the full workspace suite.

The focused Rust checks used Rust/Cargo/Clippy 1.96.1 and rustfmt 1.9.0-stable on x86_64 Linux. Platform-only behavior was verified from the checked-in gates and commands. At the review date, GitHub's [official runner inventory](https://github.com/actions/runner-images) maps both `macos-latest` and `macos-14` to arm64, so the workflows' Apple-Silicon assumption is currently valid.

## What each platform actually runs

| Platform/workflow | Commands that really execute | Clean skips or missing coverage |
|---|---|---|
| Superproject macOS | `.github/workflows/ci.yml:40-111`: workspace build; non-all-target Clippy; armfortas library tests; the release `run_programs` target; all 110 armfortas integration target files; size-only benchmark. Lines 113-267 add 12 path-gated `afs-as` targets. Native arm64-Mach-O execution is real. | Five x86/ELF-only suite names are allowlisted in `ci/expected_skips_macos.txt:1-7`. No macOS job captures or validates skip lines. `run_programs` is run once in the e2e job and again inside the all-integration job. |
| Standalone `afs-as` macOS | `afs-as/.github/workflows/ci.yml:13-65`: build, all-target Clippy, lib/doc tests, and every integration source (36 targets, with the clang dashboard split out). Mach-O/system-Apple-tool tests execute for real. | ELF/gas/readelf tests are invoked but return successful `HARNESS_SKIP`s because the only runner is macOS (`afs-as/tests/common/elf.rs:21-45`). |
| Standalone `afs-ld` macOS | `afs-ld/.github/workflows/parity-matrix.yml:23-39`: six named integration targets (`diff_harness_tolerates_known_linkedit`, `parity_harness`, `parity_canary`, `determinism`, `perf_baseline`, `parity_matrix`). | No library/unit command and 23 of 29 integration targets are absent. Several selected tests return success after tool/runtime discovery failures. The runtime performance case substitutes a synthetic archive in a standalone checkout. |
| Linux glibc | `.github/workflows/ci.yml:273-297`: debug workspace build, armfortas lib tests, every armfortas integration target, skip-count check, x87 scan, SSE2-ceiling scan, release compiler/runtime build, and size-only benchmark. Native x86_64 glibc e2e runs at all optimization levels. | Seven suite names are expected to skip (`ci/expected_skips_posix-elf.txt:11-17`). No `afs-as` or `afs-ld` package tests run. |
| Linux musl | `.github/workflows/ci.yml:299-318`: Alpine dependency setup, debug workspace build, armfortas lib tests, every armfortas integration target, and skip-count check. | The seven base suite names plus 79 musl-extra names (86 total) are expected to skip; native linking/running is intentionally disabled by `src/testing.rs:35-49,95-110`. No x87, ISA, benchmark, `afs-as`, or `afs-ld` test gate runs. |
| FreeBSD 15 | `.github/workflows/ci.yml:320-352`: debug workspace build, armfortas lib tests, only `run_programs::test_programs_end_to_end` at `-O0`, release compiler/runtime build, and size-only benchmark. The selected O0 program sweep is intended to execute natively. | No other armfortas integration target, optimization level, skip gate, `afs-as` test, or `afs-ld` test runs. Worse, the one e2e command's exit status is masked (finding 1). |

## Confirmed discrepancies

### 1. Critical — FreeBSD e2e failures are masked by `tee`

- **Source location:** `.github/workflows/ci.yml:333,348-349`; `tests/run_programs.rs:2969-2988`.
- **Reproduction:**

  ```sh
  sh -c '
    (printf "[-O0] 2034 passed, 3 xfailed, 1 failed out of 2038 test programs\n"; exit 101) |
      tee /dev/null
    echo "pipeline_status=$?"
    printf "[-O0] 2034 passed, 3 xfailed, 1 failed out of 2038 test programs\n" |
      grep -E "\[-O0\] .* passed"
    echo "grep_status=$?"
  '
  ```

- **Actual behavior:** both printed statuses are `0`. The VM uses `sh`, the Cargo pipeline has no `pipefail`, and the test harness prints its `[-O0] N passed, ... M failed` summary before returning `Err`. Therefore `tee` hides Cargo exit 101 and the following grep still matches a failing summary.
- **Intended behavior:** any non-XFAIL program failure must make the FreeBSD job fail.
- **Consequence:** every FreeBSD-specific runtime regression in the only e2e sweep can be green as long as at least one program passed and the later benchmark completes.
- **Confidence:** certain; the shell behavior was reproduced locally without filesystem output.

### 2. High — a release compiler prefers the debug runtime, invalidating Linux/FreeBSD release-size gates

- **Source location:** `src/driver/mod.rs:2535-2545,2641-2658`; the same debug-first list is duplicated at `src/testing.rs:1157-1174`. The relevant CI order is `.github/workflows/ci.yml:281,294-295,340-352`; the benchmark invokes `./target/release/armfortas` at `scripts/benchmark_gate.sh:31,89`. The profile-correct ordering expected by tests is explicit at `tests/elf_static_link.rs:26-33`.
- **Reproduction:** after both profiles have been built, force a harmless linker failure so verbose output exposes the selected archive:

  ```sh
  cargo build --workspace
  cargo build --release -p armfortas -p armfortas-rt
  crt=$(cc -print-file-name=crt1.o)
  AFS_LD_PATH=/bin/false target/release/armfortas -v "$crt" -o /dev/null
  ```

  The focused local probe printed a link line containing `/tmp/armfortas-audit/target/debug/libarmfortas_rt.a`; that debug archive was 36,568,808 bytes.
- **Actual behavior:** `fresh_runtime_lib` searches `target/debug` before `target/release` and returns the first archive newer than the runtime sources. Linux and FreeBSD CI build debug first and release second, so both are fresh and the release compiler selects debug. The isolated macOS release jobs normally have no preceding debug build and avoid the defect by accident.
- **Intended behavior:** a release compiler/benchmark must link the release runtime (or an explicitly selected runtime), independent of whether a fresh debug build also exists.
- **Consequence:** release-produced binaries can contain debug-profile runtime code/data. Linux and FreeBSD benchmark baselines normalize the wrong artifact, so they do not gate the size of the profile that would actually be shipped and can hide release-only bloat or optimization regressions.
- **Confidence:** certain from the executed selection probe, deterministic candidate order, and CI build order.

### 3. High — `afs-ld` CI omits all 328 source-unit tests and 23 of 29 integration targets

- **Source location:** `afs-ld/.github/workflows/parity-matrix.yml:23-39`; superproject test selections at `.github/workflows/ci.yml:62-111,273-352`; intended package command at `afs-ld/README.md:13-17`. A FreeBSD-only whole-toolchain gate that never runs is `tests/elf_static_link.rs:1-9,45-61,108-177`.
- **Reproduction/inventory:**

  ```sh
  find afs-ld/tests -maxdepth 1 -type f -name '*.rs' | wc -l
  rg '^\s*#\[test\]' afs-ld/tests --glob '*.rs' | wc -l
  rg '^\s*#\[test\]' afs-ld/src --glob '*.rs' | wc -l
  rg -o -- '--test [A-Za-z0-9_-]+' afs-ld/.github/workflows/parity-matrix.yml |
    sed 's/--test //' | sort -u
  rg 'cargo test.*afs-ld' .github/workflows/ci.yml
  ```

- **Actual behavior:** the inventory is 29 integration files/178 integration tests and 328 source-unit tests. The standalone workflow names only six integration targets, runs no `--lib` command, and the superproject contains zero direct `cargo test -p afs-ld` invocations. Root armfortas tests exercise a few linker routes, but not the omitted parser, malformed-input, writer, archive, resolver, ELF, TBD, or large `linker_run` suites. FreeBSD does not invoke its purpose-built `elf_static_link` gate at all.
- **Intended behavior:** the documented `cargo test -p afs-ld` package gate, or equivalent split jobs, should execute the linker tests on hosts where their prerequisites are real.
- **Consequence:** most linker regressions can merge with all workflows green. FreeBSD archive/CRT/static-libc/TLS/IFUNC behavior has a test but no platform that executes it.
- **Confidence:** certain; target enumeration and all workflow command lines are static.

### 4. High — macOS skip discipline is disconnected from CI, and the dormant macOS checker accepts invalid skips

- **Source location:** macOS test commands at `.github/workflows/ci.yml:82,110-111`; Linux-only checker calls at lines 287 and 318; macOS checker branch at `ci/check_skips.sh:33-47`; policy text at `tests/README.md:52-59`; allowlist at `ci/expected_skips_macos.txt:1-7`.
- **Reproduction:**

  ```sh
  bash -c 'ci/check_skips.sh <(printf '\''HARNESS_SKIP suite=x86_emit_golden test=fake count=0 reason="all work vanished"\n'\'') macos'
  echo $?
  bash -c 'ci/check_skips.sh <(printf "") macos'
  echo $?
  ```

- **Actual behavior:** both invocations return 0 (`macOS profile clean`), including a malformed zero-count skip and an empty log. More importantly, no macOS CI job calls the checker at all. Unlike the POSIX branch, the macOS branch validates neither positive counts nor presence of the expected platform-only suites.
- **Intended behavior:** native macOS coverage should permit only the closed platform allowlist, require well-formed positive discovered counts, and fail if a previously observed suite silently vanishes or a native suite starts skipping.
- **Consequence:** a widened host gate, missing runtime/tool artifact, or new clean-return path can turn native Apple-Silicon tests into green no-ops without a CI signal.
- **Confidence:** certain for the missing invocation and reproduced checker behavior; high for impact because current GitHub images normally provide the required Apple tools.

### 5. Medium — custom `CARGO_TARGET_DIR` causes hard failures or successful no-op tests

- **Source location:** `scripts/benchmark_gate.sh:31,43-45`; `tests/standalone_toolchain.rs:24-50,141-168`; `afs-ld/tests/archive_runtime.rs:14-35`; `afs-ld/tests/linker_run.rs:68-79`; `afs-ld/tests/perf_baseline.rs:13-31`; `src/testing.rs:1106-1128,1157-1161`. In contrast, `tests/run_programs.rs:361-391` correctly derives the active profile directory from `current_exe`.
- **Reproduction on a clean checkout:**

  ```sh
  CARGO_TARGET_DIR=/tmp/afs-target cargo build --workspace --release
  CARGO_TARGET_DIR=/tmp/afs-target cargo test -p armfortas --release \
    --test standalone_toolchain -- --nocapture

  CARGO_TARGET_DIR=/tmp/afs-target cargo build --release -p armfortas -p armfortas-rt
  BENCH_SKIP_TIME=1 ./scripts/benchmark_gate.sh
  ```

- **Actual behavior:** the standalone tests probe only workspace `target/debug` and `target/release`; missing tools/runtime print lowercase `skipping:` and return success. The `afs-ld` tests repeat the default-parent lookup, sometimes skipping and sometimes substituting a synthetic runtime. The benchmark ignores Cargo's target directory and exits with `Build the compiler first`. Existing default artifacts can instead mask the problem and make a custom-target run test stale binaries.
- **Intended behavior:** all test and gate artifact lookup should use Cargo-provided binary paths or the active target/profile directory, and mandatory coverage should fail rather than cleanly disappear.
- **Consequence:** a custom-target CI/cache layout can be red for the wrong reason, or worse, green without standalone linker/runtime coverage. Local results can silently use stale default-target artifacts.
- **Confidence:** high; the clean rebuild was not repeated because shared `/tmp` became full, but every relevant lookup is an exhaustive hard-coded list and the failure/skip branches are explicit.

### 6. Medium — root Clippy is green only because test targets are excluded

- **Source location:** `.github/workflows/ci.yml:50-60`; current diagnostics at `runtime/src/string.rs:1024,1028,1063`, `tests/fortran_alias_licm.rs:46,52`, `tests/fortsh_module_graph.rs:62,82,198`, `tests/standalone_toolchain.rs:42`, and `tests/sroa_shape_audit_29_11.rs:60,66`.
- **Reproduction:**

  ```sh
  CARGO_INCREMENTAL=0 cargo clippy --workspace \
    --exclude bencch-core --exclude afs-tests \
    -- -D warnings -A clippy::too_many_arguments

  CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets \
    --exclude bencch-core --exclude afs-tests \
    -- -D warnings -A clippy::too_many_arguments
  ```

- **Actual behavior:** the exact CI command exits 0. Adding `--all-targets` exits 101, first on runtime test code (`manual_c_str_literals`, `manual_repeat_n`) and then on multiple armfortas integration targets. Focused `afs-as` and `afs-ld` all-target Clippy commands each exit 0.
- **Intended behavior:** `-D warnings` should cover the test harnesses that define CI correctness, consistent with the all-target commands documented in `afs-as/README.md:74-78` and `afs-ld/README.md:13-16`.
- **Consequence:** lint defects and suspicious harness idioms merge while the Clippy job reports success; enabling the missing scope now would immediately break CI.
- **Confidence:** high; commands were run at this tip with Clippy 1.96.1 (future stable lint sets can differ).

### 7. Medium — rustfmt is substantially adrift and no workflow checks it

- **Source location:** representative current diffs at `afs-as/src/elf.rs:81`, `afs-as/src/lib.rs:8-14`, `afs-ld/tests/writer_smoke.rs:65,81`, `runtime/src/array.rs:3369`, and `tests/determinism_sweep.rs:91`. None of `.github/workflows/ci.yml`, `afs-as/.github/workflows/ci.yml`, or `afs-ld/.github/workflows/parity-matrix.yml` contains a format command.
- **Reproduction:**

  ```sh
  cargo fmt --all -- --check
  ```

- **Actual behavior:** exit 1. Rustfmt reports drift in 55 unique files: 16 superproject files, 30 `afs-as` files, six `afs-ld` files, and three runtime files.
- **Intended behavior:** the checked-in Rust workspace should satisfy its formatter and CI should prevent new drift.
- **Consequence:** formatting debt grows unchecked, review diffs stay noisy, and adding a formatter gate later requires a large unrelated rewrite.
- **Confidence:** high; reproduced with rustfmt 1.9.0-stable.

### 8. Medium — benchmark fixtures can silently disappear while the gate passes

- **Source location:** `scripts/benchmark_gate.sh:32-38,104-109,125-136,151-185`.
- **Reproduction in a disposable checkout:**

  ```sh
  mv test_programs/array_bulk_kernels.f90{,.audit11-hidden}
  trap 'mv test_programs/array_bulk_kernels.f90{.audit11-hidden,}' EXIT
  BENCH_SKIP_TIME=1 ./scripts/benchmark_gate.sh
  ```

- **Actual behavior:** a missing member of the fixed five-program set prints `SKIP`, is omitted from `RESULTS`, and is never compared with its stale baseline entry. If all five disappear, only the separately mandatory BSS sentinel remains and `FAIL` stays zero.
- **Intended behavior:** the fixed benchmark corpus should be mandatory, just as the BSS sentinel already is.
- **Consequence:** deleting or renaming a benchmark silently weakens the size gate and can make a regression invisible.
- **Confidence:** certain from the control flow; the destructive reproduction was not run in this worktree.

### 9. Medium — the documented audit-reference rule for XFAIL is not enforced

- **Source location:** acceptance at `tests/run_programs.rs:727-749`; XFAIL/XPASS classification at lines 2899-2908; the narrow x86-only cross-check at lines 3027-3116; policy at `README.md:250-253`.
- **Reproduction/inventory:**

  ```sh
  rg -n '^\s*!\s*XFAIL' test_programs --glob '*.{f,f90,for,ftn}'
  ```

  Current output includes bare, non-ID reasons in `test_programs/error_pure_allocate_host.f90:2` and `test_programs/error_pure_deallocate_host.f90:2`.
- **Actual behavior:** any active `! XFAIL:` text, including an empty reason, converts a failure into `TestOutcome::Xfail`. The reference cross-check applies only to non-musl `XFAIL(x86_64...)` lines containing `X64-O0-NNN`; bare, macOS, FreeBSD, and musl waivers are not required to reference tracked debt. XPASS is correctly fatal.
- **Intended behavior:** the README says programs with known bugs carry XFAIL annotations that reference the audit finding.
- **Consequence:** a regression can be waived into green CI without a durable finding, owner, or removal trail.
- **Confidence:** certain for parser/cross-check behavior and the current unreferenced annotations.

## Unconfirmed concerns

- `scripts/benchmark_gate.sh:144-160` deliberately creates a missing platform baseline and exits successfully, and treats a missing per-case baseline as `NEW`. The three size-gated CI environments appear to have matching committed baseline names (`arm64-macos`, `x86_64-linux-gnu-ubuntu`, `x86_64-freebsd`), so I did not classify this as a current CI failure. A runner target/distro rename would nevertheless disarm the gate for its first run rather than fail closed. The local CachyOS target has no matching baseline and would take this bootstrap path.
- Linux baseline selection uses only target triple plus `/etc/os-release` `ID` (`scripts/benchmark_gate.sh:48-64`). Runner image, linker, Rust, and system-library revisions can change underneath the same filename. I did not establish a current false pass/failure from this, but the baseline is less hermetic than its name suggests.
- The mutable `macos-latest` label is arm64 today, so native macOS execution is real. A future label change is external to this repository; pinning an explicit arm64 label would make the invariant reviewable from the workflow alone.

## Maintainability observations

- Runtime/tool discovery is independently reimplemented in the driver, `src/testing.rs`, many root integration targets, and `afs-ld` tests. The copies disagree on profile order and target-directory handling; findings 2 and 5 are direct consequences.
- `afs-ld/tests` contains 351 lowercase `skipping:` sites across 17 integration files. These returns are not machine-counted and the standalone linker workflow has no skip checker, so it is difficult to distinguish real execution from clean no-ops in job status.
- `tests/README.md:52-59` still says zero skips are allowed on macOS, while `ci/expected_skips_macos.txt` and `ci/check_skips.sh:33-47` now allow five ELF-only suite names. The implementation and policy text should describe the same contract.
- The macOS superproject runs the large `run_programs` target both in `test-end-to-end` and again under `test-integration`. This buys no additional target coverage and makes the already long macOS lane more expensive.
- No Rust test carries `#[ignore]`; the integrity risks here are clean-return gates and omitted commands, not Cargo's ignored-test mechanism.

## Test and integration gaps

These are coverage gaps rather than additional claims that a currently invoked command is false-green.

### `afs-as` has no real ELF/system oracle in CI

The standalone workflow runs every integration source, but only on macOS (`afs-as/.github/workflows/ci.yml:13-65`). ELF helpers intentionally return no gas off Linux/FreeBSD (`afs-as/tests/common/elf.rs:21-45`), and `elf_smoke` returns success at `afs-as/tests/elf_smoke.rs:23-30,84-89,129-137`. Superproject Linux/FreeBSD jobs build `afs-as` but never run its package tests. Thus the x86 encoder's gas differential, ELF writer/readelf/system-link, rejection, stress, alignment, and corpus tests lack a real CI host even though their files are enumerated on macOS.

### Real fuzz targets are dormant

`fuzz/Cargo.toml:14-25` makes `fuzz/` a separate workspace containing `fuzz_lexer` and `fuzz_parser`; root workspace commands cannot discover them. No checked-in workflow invokes `cargo fuzz`. `tests/fuzz_smoke.rs:1-5,116-129` runs deterministic ASCII smoke inputs, which is useful but does not exercise libFuzzer, sanitizer instrumentation, corpus evolution, or arbitrary-byte paths over time.

Reproduction of the integration gap:

```sh
git grep -n -E 'cargo fuzz|fuzz_(lexer|parser)' -- .github afs-as/.github afs-ld/.github
```

This produces no workflow references.

### Coverage is manual and library-only

Every mode in `scripts/coverage.sh:24-40` uses `--workspace --lib`. That excludes the root, assembler, and linker integration targets that drive CLIs, real objects, archives, runtime binaries, and end-to-end compilation. No workflow invokes the script, uploads its output, or enforces a threshold. The local coverage tools were not installed, so I did not generate a report; the target selection itself is unambiguous.

### `bencch` suites and their XFAIL/XPASS accounting are not gated

The workspace contains 55 `.afs` suite files with 357 `case` declarations. CI builds the crates, but `.github/workflows/ci.yml:60` explicitly excludes `bencch-core` and `afs-tests` from Clippy, and no workflow runs `cargo test -p afs-tests` or `cargo run -p afs-tests -- run ...`. The runner correctly treats XPASS as fatal at `bencch/bench/src/lib.rs:473-493,1491-1526`, but that policy and the authored differential/consistency matrices are dormant in CI.

### Intentional platform gaps remain large

- Musl proves compilation and non-native logic, not native Fortran linking/running: 86 suite names are expected to skip.
- FreeBSD runs only one O0 program sweep. O1 through Ofast, the other 109 root integration targets, the FreeBSD-only `elf_static_link` gate, and both toolchain packages' tests are absent.
- Root benchmark jobs set `BENCH_SKIP_TIME=1`, so they gate binary size and the BSS sentinel, not compile-time performance. The standalone `afs-ld` workflow does set time budgets, but `afs-ld/tests/perf_baseline.rs:13-40` cannot find a parent armfortas runtime in a standalone checkout and substitutes a small synthetic archive for its “runtime” budget.

## Focused command outcomes

| Command/check | Outcome |
|---|---|
| Exact root CI Clippy command | exit 0 |
| Same Clippy command plus `--all-targets` | exit 101 |
| `cargo clippy -p afs-as --all-targets -- -D warnings` | exit 0 |
| `cargo clippy -p afs-ld --all-targets -- -D warnings` | exit 0 |
| `cargo fmt --all -- --check` | exit 1; 55 files drift |
| FreeBSD-style failing command piped through `tee`, followed by current grep | both shell statuses 0 |
| macOS skip checker given `count=0` allowlisted line | exit 0 |
| macOS skip checker given an empty log | exit 0 |
| Release compiler verbose link selection | selected `target/debug/libarmfortas_rt.a` |
