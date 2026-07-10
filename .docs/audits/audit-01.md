# Audit 01: front-end and driver correctness

## Scope and baseline

This review covered armfortas preprocessing, free-form and fixed-form lexing,
parsing, source-size enforcement, diagnostics, dependency scanning, depfile
generation, runtime-library discovery, and single/multi-file driver
orchestration.

- Dynamic baseline: commit `23857aa4` (`adversarial-audit-20260709` at the
  start of the review).
- Compiler: isolated build at
  `/tmp/armfortas-audit-01-target-23857aa4/debug/armfortas`, produced with
  `CARGO_TARGET_DIR=/tmp/armfortas-audit-01-target-23857aa4 cargo build -p armfortas --bin armfortas`.
- Experiments: `/tmp/armfortas-audit-01-exp-23857aa4`.
- Host: x86_64 Linux; reference compiler: GNU Fortran 16.1.1.
- No implementation, test, CI, or submodule files were changed. No commit was
  created.

The shared worktree advanced externally to `a6ef0b1d` while evidence was being
collected. Dynamic results below are therefore tied to the isolated
`23857aa4` binary. The cited code paths were still present when line anchors
were collected, but this was not a full re-baseline of the later commit.
Following the review coordinator's instruction, deduplication against other
audit reports is left to the parent review.

Severity meanings used here:

- **Critical**: silently emits an artifact for the wrong target or ABI.
- **High**: rejects valid/common source, corrupts preprocessing, bypasses a
  safety limit, crashes the compiler, or breaks ordinary build orchestration.
- **Medium**: produces materially wrong diagnostics/dependencies, accepts an
  invalid structural constraint, or breaks a narrower compatibility path.

## Confirmed findings

### AUD01-001 - Multi-file compilation silently resets target and preprocessing options

**Severity:** Critical  
**Area:** driver orchestration

**Evidence:** `compile_multi` constructs each child job from
`Options::default()` and manually copies an incomplete subset of fields at
`src/driver/mod.rs:2432-2469`. In particular, `target`,
`preprocessor_defines`, `cpp_compat`, `std_explicit`, free-line-limit options,
depfile options, and `target_cpu` are not inherited.

**Exact reproduction:** `one.f90` and `two.f90` each contain one empty
subroutine.

```sh
AFS=/tmp/armfortas-audit-01-target-23857aa4/debug/armfortas
cd /tmp/armfortas-audit-01-exp-23857aa4/multi-target
$AFS --target x86_64-freebsd -c one.f90 two.f90
file one.o two.o
readelf -h one.o | sed -n '1,10p'
$AFS --target x86_64-freebsd -c one.f90 -o one-freebsd.o
file one-freebsd.o
```

The multi-file objects were ELF x86-64 with `OS/ABI: UNIX - System V`; the
single-file control was ELF x86-64 with `OS/ABI: UNIX - FreeBSD`.

A second manifestation uses this module source:

```fortran
#ifndef AUDIT_FLAG
#error AUDIT_FLAG was dropped
#endif
module flag_mod
end module flag_mod
```

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/multi-define
$AFS -DAUDIT_FLAG -c consumer.F90 flag_mod.F90
$AFS -DAUDIT_FLAG -c flag_mod.F90 -o flag-control.o
```

The multi-file command exited 1 at `#error AUDIT_FLAG was dropped`; the
single-file control exited 0.

**Expected:** every per-source child job inherits all compilation-affecting
options from the parent.  
**Impact:** cross-compilation can silently produce host-ABI objects, and valid
multi-file preprocessed builds fail or select the wrong source branches.  
**Confidence:** High.

### AUD01-002 - Same-basename sources overwrite one multi-file temporary object

**Severity:** High  
**Area:** driver orchestration

**Evidence:** link-mode multi-file builds use one PID-scoped directory
(`src/driver/mod.rs:2407-2414`) and derive each object name solely from
`file_stem()` (`src/driver/mod.rs:2423-2430`).

**Exact reproduction:** `a/unit.f90` defines `alpha`, `b/unit.f90` defines
`beta`, and `main.f90` calls both.

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/same-stem
$AFS a/unit.f90 b/unit.f90 main.f90 -o afs-same
gfortran a/unit.f90 b/unit.f90 main.f90 -o gf-same
./gf-same
```

armfortas exited 2. The linker reported two definitions of `beta` from the
same `/tmp/afs_multi_<pid>/unit.o` and an undefined reference to `alpha`.
The gfortran control linked and printed `1` then `2`.

**Expected:** temporary object identity includes the complete source identity
or another unique key.  
**Impact:** valid projects with repeated conventional basenames cannot be
linked in one invocation.  
**Confidence:** High.

### AUD01-003 - Dependency scanning treats inactive preprocessor branches as real edges

**Severity:** High  
**Area:** dependency scanning

**Evidence:** `scan_file` reads raw UTF-8 text and applies a line-prefix scan
without preprocessing at `src/driver/dep_scan.rs:31-45`; raw `MODULE` and
`USE` matches are collected at `src/driver/dep_scan.rs:82-130`.

**Exact reproduction:** `a.F90` contains an inactive `use b`; `b.F90`
actually uses `a`.

```fortran
module a
#if 0
  use b
#endif
end module a
```

```fortran
module b
  use a
end module b
```

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/dep-cycle
$AFS -c b.F90 a.F90
$AFS -c a.F90 -o a-control.o
$AFS -I. -c b.F90 -o b-control.o
```

The combined command exited 1 with `circular module dependency detected among:
b.F90, a.F90`; compiling in the real dependency order succeeded.

**Expected:** dependency discovery sees the same preprocessed source as the
compiler, or conservatively avoids inventing impossible cycles.  
**Impact:** valid unordered multi-source invocations are rejected; macros and
includes can similarly hide real definitions or dependencies from the graph.  
**Confidence:** High.

### AUD01-004 - Legal comment gaps bypass statement warnings and the hard cap

**Severity:** High  
**Area:** source limits / preprocessing

**Evidence:** both hard-cap accounting (`src/driver/conformance.rs:60-103`)
and F2023 statement accounting (`src/driver/conformance.rs:176-210`) decide
continuation from each physical line independently. A comment line therefore
ends their count. The actual preprocessor explicitly skips comment/blank lines
without breaking continuation at `src/preprocess/mod.rs:298-327`.

**Exact reproduction:** generate a statement over the two-million-character
compiler cap with a comment between each continued source line.

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/preprocess
awk 'BEGIN {
  chunk=""; for (i=0;i<4990;i++) chunk=chunk "+1";
  print "program p"; print "  integer :: x";
  print "  x = 0 " chunk " &";
  for (j=0;j<202;j++) {
    print "! continuation gap " j;
    if (j==201) print "  & " chunk; else print "  & " chunk " &";
  }
  print "  print *, x"; print "end program p";
}' > cap-gap.f90
$AFS --std=f2023 -E cap-gap.f90 -o cap-gap.pp
awk '{if(length($0)>m)m=length($0)}END{print m}' cap-gap.pp
```

armfortas exited 0, emitted no statement-length warning, and produced one
preprocessed line of 2,026,352 characters. The configured hard cap is
2,000,000 and the F2023 warning threshold is 1,000,000.

**Expected:** the comment lines are skipped while continuation state and
statement character totals remain active; this input must hit the hard-cap
diagnostic.  
**Impact:** the scanner's stated guarantee that oversized statements cannot
reach recursive front-end passes is false.  
**Confidence:** High.

### AUD01-005 - Every compile reserves a 2 GiB thread stack and can panic before compilation

**Severity:** High  
**Area:** reliability / driver

**Evidence:** `cli_entry` unconditionally requests a 2 GiB stack and calls
`.expect("cannot spawn compile thread")` outside the worker's `catch_unwind`
at `src/lib.rs:85-102`.

**Exact reproduction:** use a tiny preprocess-only input.

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/preprocess
bash -c 'ulimit -v 1048576; /tmp/armfortas-audit-01-target-23857aa4/debug/armfortas -E line.F90 >/dev/null'
echo $?
bash -c 'ulimit -v 2621440; /tmp/armfortas-audit-01-target-23857aa4/debug/armfortas -E line.F90 >/dev/null'
```

At a 1 GiB address-space limit the first command exited 101 and panicked at
`src/lib.rs:100` with `cannot spawn compile thread: Resource temporarily
unavailable`. The 2.5 GiB control exited 0.

**Expected:** tiny inputs do not require a 2 GiB virtual stack reservation;
spawn failure must at minimum become a structured compiler error with a
documented exit code.  
**Impact:** ordinary constrained containers, CI workers, and high-parallelism
builds can fail before reading source.  
**Confidence:** High.

### AUD01-006 - The `#if` evaluator rejects valid mixed arithmetic and can ICE on overflow

**Severity:** High  
**Area:** preprocessing

**Evidence:** additive parsing searches for `+` before `-` and passes the
entire right side to the multiplicative parser (`src/preprocess/mod.rs:1106-1123`);
multiplicative parsing similarly searches operator classes independently
(`src/preprocess/mod.rs:1125-1147`). Arithmetic uses checked debug-profile
`i64` operations without overflow handling.

**Exact reproductions:** 

```fortran
#if 10 + 5 - 2 == 13
program p
#else
#error arithmetic precedence broken
#endif
end program p
```

```fortran
#if 9223372036854775807 + 1
program p
end program p
#endif
```

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/preprocess
$AFS -E arithmetic.F90
$AFS -E overflow.F90
gfortran -cpp -E -P arithmetic.F90
gfortran -cpp -E -P overflow.F90
```

The first armfortas command exited 1 with `unexpected token ... '5 - 2'`.
The second exited 4 with an ICE at `src/preprocess/mod.rs:1112` (`attempt to
add with overflow`). Both gfortran controls completed; overflow produced a
warning rather than a crash.

**Expected:** standard preprocessor precedence/associativity and defined
integer handling, never a compiler panic.  
**Impact:** valid build-configuration expressions fail, and adversarial or
generated constants crash debug compiler builds.  
**Confidence:** High.

### AUD01-007 - Function-like macro argument splitting ignores quoted strings

**Severity:** High  
**Area:** preprocessing

**Evidence:** `expand_function_macro` tracks only parenthesis depth while
splitting on every depth-one comma (`src/preprocess/mod.rs:851-895`); it has no
quote state.

**Exact reproduction:**

```fortran
#define ID(x) x
program p
  print *, ID("a,b")
end program p
```

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/preprocess
$AFS -E function-string.F90
gfortran -cpp -E -P function-string.F90
```

armfortas exited 0 but emitted `print *, "a`; gfortran emitted
`print *, "a,b"`.

**Expected:** commas and parentheses inside quoted preprocessing tokens do not
split macro arguments.  
**Impact:** preprocessing silently truncates source and commonly produces a
later, misleading lexer error.  
**Confidence:** High.

### AUD01-008 - Fixed-form `PRINT` with a numeric FORMAT label is tokenized as an identifier

**Severity:** High  
**Area:** fixed-form lexing

**Evidence:** fixed-form text is whitespace-collapsed, then keyword-prefix
splitting rejects digit-starting suffixes except after `GOTO` or `CALL` at
`src/lexer/fixed.rs:543-674`. Thus `PRINT 10` becomes `PRINT10`.

**Exact reproduction:**

```fortran
      PROGRAM P
      PRINT 10
   10 FORMAT('OK')
      END
```

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/fixed
$AFS print-label.f -o print-label-afs
gfortran print-label.f -o print-label-gf
./print-label-gf
```

armfortas exited 1 at line 2 with `unexpected expression statement`.
The gfortran control exited 0 and printed `OK`.

**Expected:** `PRINT` is recognized as the statement keyword and `10` as its
format label.  
**Impact:** a core fixed-form/F77 formatted-I/O spelling is unusable.  
**Confidence:** High.

### AUD01-009 - ARM64 Mach-O compile-only cross-targeting invokes the host assembler

**Severity:** High  
**Area:** target driver orchestration

**Evidence:** ELF targets have explicit cross-assembly handling and the
in-process assembler (`src/driver/mod.rs:1865-1929`), but the non-ELF path
falls through to `Command::new("as")` at `src/driver/mod.rs:1945-1957` without
a host/target check.

**Exact reproduction:**

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/multi-target
$AFS --target arm64-macos -c one.f90 -o one-single.o
```

On x86_64 Linux this exited 1. GNU `as` reported Mach-O directives as junk and
AArch64 instructions such as `stp x29,x30` as unknown.

**Expected:** compile-only cross-targeting uses an ARM64/Mach-O-capable
in-process assembler, or rejects the unsupported route before invoking the
host assembler.  
**Impact:** the advertised ARM64 compile-only cross-target path cannot produce
an object from Linux.  
**Confidence:** High.

### AUD01-010 - `CARGO_TARGET_DIR` breaks runtime-linked integration tests

**Severity:** High  
**Area:** test/CI reliability

**Evidence:** `tests/multifile.rs:37-54` searches only relative
`target/debug` and `target/release`. The library-side helper repeats those
workspace-relative candidates at `src/testing.rs:1106-1175`. The driver also
starts with hard-coded workspace candidates at `src/driver/mod.rs:2535-2545`
and `src/driver/mod.rs:2641-2658`, though its executable-sibling fallback can
mitigate installed/compiler-process cases.

**Exact reproduction:** a clean shadow copy had no default `target/`, while
the runtime archive existed in the custom target directory.

```sh
rsync -a --delete --exclude=.git --exclude=target --exclude=.docs \
  /tmp/armfortas-audit/ /tmp/armfortas-audit-01-shadow-23857aa4/
cd /tmp/armfortas-audit-01-shadow-23857aa4
CARGO_TARGET_DIR=/tmp/armfortas-audit-01-shadow-target-23857aa4 \
  cargo build -p armfortas-rt
CARGO_TARGET_DIR=/tmp/armfortas-audit-01-shadow-target-23857aa4 \
  cargo test -p armfortas --test multifile \
    basic_module_variable_and_subroutine -- --exact --nocapture
```

The archive was built at
`/tmp/armfortas-audit-01-shadow-target-23857aa4/debug/libarmfortas_rt.a`, but
the test exited 101 and panicked at `tests/multifile.rs:54` with
`libarmfortas_rt.a not found`.

**Expected:** tests and shared runtime lookup honor Cargo's active target
directory or locate artifacts relative to Cargo-provided executable paths.  
**Impact:** out-of-tree Cargo builds and CI cache layouts fail despite having
successfully built the required runtime.  
**Confidence:** High for the integration-test failure; the complete driver
failure matrix was not independently exercised.

### AUD01-011 - Preprocessor source maps are generated but discarded by diagnostics

**Severity:** Medium  
**Area:** free-form/fixed-form lexing and diagnostics

**Evidence:** `PreprocOutput` carries a per-output-line source map at
`src/preprocess/mod.rs:120-134`. The driver retains only `pp_result.text` at
`src/driver/mod.rs:1434-1437`, tokenizes that transformed text, then renders
lexer/parser/sema spans against the original top-level source at
`src/driver/mod.rs:1465-1475`, `src/driver/mod.rs:1500-1517`, and
`src/driver/mod.rs:1574-1588`. Fixed-form continuation bodies are likewise
tokenized using only the first physical line (`src/lexer/fixed.rs:32-77` and
`src/lexer/fixed.rs:962-1024`).

**Exact reproductions:**

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/diag
$AFS continuation.f90 -c -o continuation.o
$AFS include-main.F90 -c -o include-main.o
gfortran -fsyntax-only continuation.f90
gfortran -cpp -fsyntax-only include-main.F90

cd /tmp/armfortas-audit-01-exp-23857aa4/fixed
$AFS -c continuation-error.f -o continuation-error.o
gfortran -fsyntax-only continuation-error.f
```

For free form, an `@` physically on line 6 was reported as line 5 and the
snippet displayed the preceding continuation text `2`. An `@` at
`bad.inc:1` was reported as `include-main.F90:2` and displayed the `#include`
directive. For fixed form, an `@` on continuation line 4 was reported at line
3. The gfortran controls identified the actual file and physical lines.

**Expected:** transformed spans are remapped through `source_map`, including
included filenames and continuation offsets.  
**Impact:** editor navigation and diagnostic snippets point at unrelated
source, obscuring the actual error.  
**Confidence:** High.

### AUD01-012 - `#line` does not affect `__LINE__` or `__FILE__`

**Severity:** Medium  
**Area:** preprocessing

**Evidence:** `do_line` records only a `line_override` for source-map entries
(`src/preprocess/mod.rs:594-609`), while each logical line overwrites
`__LINE__` from its physical index at `src/preprocess/mod.rs:334-338`.
`__FILE__` remains the physical input name. The source map is then discarded
as described in AUD01-011.

**Exact reproduction:**

```fortran
#line 100 "virtual-source.F90"
program p
  print *, __LINE__, __FILE__
end program p
```

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/preprocess
$AFS -E line.F90
gfortran -cpp -E -P line.F90
```

armfortas emitted `3, "line.F90"`; gfortran emitted
`101, "virtual-source.F90"`.

**Expected:** `#line` changes the presumed line and file used by subsequent
predefined macros and diagnostics.  
**Impact:** generated sources embed wrong location metadata and cannot redirect
diagnostics to their logical source.  
**Confidence:** High.

### AUD01-013 - Non-UTF-8 source is mutated at top level and rejected in includes

**Severity:** Medium  
**Area:** source ingestion / preprocessing

**Evidence:** the driver deliberately uses `String::from_utf8_lossy` for the
top-level input at `src/driver/mod.rs:1339-1354`, changing invalid bytes into
U+FFFD. `#include` instead uses `read_to_string` and rejects the same bytes at
`src/preprocess/mod.rs:645-651`.

**Exact reproduction:**

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/preprocess
printf "program p\n  print *, 'A\\377B'\nend program p\n" > nonutf8-top.F90
$AFS -E nonutf8-top.F90 -o nonutf8-afs.out
gfortran -cpp -E -P nonutf8-top.F90 -o nonutf8-gf.out
od -An -tx1 nonutf8-top.F90 nonutf8-afs.out nonutf8-gf.out

printf "! byte: \\377\ninteger :: included_x\n" > nonutf8.inc
printf '%s\n' '#include "nonutf8.inc"' 'program p' 'end program p' \
  > nonutf8-include.F90
$AFS -E nonutf8-include.F90
gfortran -cpp -E -P nonutf8-include.F90
```

The top-level input byte `ff` became `ef bf bd` in armfortas output; gfortran
preserved `ff`. The include case exited 1 with `stream did not contain valid
UTF-8`; gfortran preserved and processed the include.

**Expected:** the claimed non-UTF-8 acceptance is byte-preserving and
consistent between primary and included source.  
**Impact:** string data can silently change length/value, while harmless
high-byte comments in includes break otherwise accepted builds.  
**Confidence:** High.

### AUD01-014 - Make depfiles omit included prerequisites

**Severity:** Medium  
**Area:** dependency generation

**Evidence:** `write_dependency_file` always writes only `opts.input` after
the targets at `src/driver/mod.rs:2036-2055`; no preprocessor include list,
module dependency, or scanner result reaches it.

**Exact reproduction:** `main.F90` includes `value.inc`, which defines the
constant used by the program.

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/depfile
$AFS -MMD -MF deps.d -c main.F90 -o main.o
cat deps.d
```

Observed depfile:

```make
main.o: main.F90
```

**Expected:** `value.inc` is a prerequisite, so changing it rebuilds
`main.o`; module interface dependencies should likewise be represented where
the supported dialect requires them.  
**Impact:** incremental and parallel builds can retain stale objects after an
included source changes.  
**Confidence:** High for omitted `#include` prerequisites.

### AUD01-015 - Program-unit terminating names are consumed without validation

**Severity:** Medium  
**Area:** parsing

**Evidence:** `parse_program` records the opening name at
`src/parser/unit.rs:171-183`, but passes only the unit keyword to
`consume_end`. That helper consumes any trailing identifier without comparing
it to the opening name at `src/parser/stmt.rs:1845-1876`.

**Exact reproduction:**

```fortran
program p
  print *, 1
end program q
```

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/parser
$AFS --emit-ast end-name-mismatch.f90 -o end-name.ast
gfortran -fsyntax-only end-name-mismatch.f90
```

armfortas exited 0 and emitted an AST. gfortran exited 1 with
`Expected label 'p' for END PROGRAM statement`.

**Expected:** when both opening and closing names are present, they must match
case-insensitively.  
**Impact:** structurally invalid source is accepted, masking copy/paste and
generated-source errors. The same helper is shared by other named program
units and constructs.  
**Confidence:** High.

### AUD01-016 - Quote-bearing Hollerith FORMAT text is silently changed

**Severity:** Medium  
**Area:** fixed-form lexing

**Evidence:** `protect_hollerith` rewrites `nH...` as a single-quoted string
without escaping quote bytes in the Hollerith payload at
`src/lexer/fixed.rs:212-273`.

**Exact reproduction:**

```fortran
      PROGRAM P
      WRITE(*,10)
   10 FORMAT(3H'A')
      END
```

```sh
cd /tmp/armfortas-audit-01-exp-23857aa4/fixed
$AFS write-hollerith-quote.f -o whq-afs
./whq-afs
gfortran -std=legacy write-hollerith-quote.f -o whq-gf
./whq-gf
```

armfortas exited 0 but printed a blank record. The gfortran control printed
`'A'` (with the expected deleted-feature warning).

**Expected:** the three Hollerith payload characters are emitted verbatim.  
**Impact:** legacy/F77 FORMAT output silently changes when the payload contains
a quote.  
**Confidence:** High.

## Coverage gaps

- The full workspace test suite was not rerun after the focused findings; the
  review was stopped after targeted verification. Only one representative
  `CARGO_TARGET_DIR` integration test was independently rerun in the clean
  shadow workspace.
- Dynamic testing used the debug/dev compiler profile. The `#if` overflow ICE
  was not remeasured in a release profile; the valid-expression rejection is
  profile-independent.
- No ARM64 macOS host was available. The Mach-O finding covers Linux-to-ARM64
  compile-only routing, not native macOS assembly/link behavior.
- No broad fuzz campaign, concurrent compiler race campaign, or exhaustive
  parser grammar matrix was run. Same-basename object loss was reproduced
  deterministically in one process.
- Free-form semantic tokenization beyond preprocessing/location handling,
  fixed-form tab extensions, deeply nested include graphs, response files,
  and all diagnostic warning-group combinations remain only partially covered.
- The worktree advanced while reviewers were working in parallel. Findings
  should be rechecked against the final merge tip before remediation is marked
  complete.

## Excluded concerns

Only discrepancies reproduced by a compiler command (or, for artifact lookup,
a focused Cargo test) are listed above. Weaker static concerns and unexecuted
hypotheses were intentionally omitted.
