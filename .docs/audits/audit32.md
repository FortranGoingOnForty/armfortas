# Sprint 32 Brutal Audit

Date: 2026-04-14
Branch: `trunk` at `b89f543` (sprint 32 CLI driver merge).
Compiler build: `cargo build` (debug) clean. Binaries `target/debug/armfortas`
and `target/debug/afs` both present.

## Executive summary

Sprint 32 wires a gfortran-shaped CLI skin over the existing pipeline. The
happy path works well — flag parsing, response files, exit-code mapping for
the obvious cases, NO_COLOR/CLICOLOR_FORCE handling, caret rendering for sema
diagnostics, per-thread default-kind isolation, and `armfortas` vs `afs`
name-symmetry all hold up under inspection. The tests in `tests/cli_driver.rs`
exercise the easy corners.

The trouble is that **half of the `Options` struct is parsed and then
thrown away**. `-fcheck=bounds`, `-fcheck=all`, `-fmax-stack-var-size=N`,
`-frecursive`, `-fbackslash`, `-Wall`, `-Wextra`, `-Wpedantic`, `-Wdeprecated`,
`-Werror`, `-g`, and `--diagnostics-format=json` are all *recognised* by
`parse_cli` but none of them reach any pipeline consumer. A user who wanted
`-fcheck=bounds` off cannot turn it off (bounds checks are always on); a user
who wanted `-fcheck=bounds` on cannot tell the difference (bounds checks are
always on). `-Werror` and `-Werror`'s interaction with `-Wall` is moot while
no warning is ever emitted. `--diagnostics-format=json` silently keeps the text
format. The driver is an API stub with a gfortran-shaped sticker on it.

Worse, `.amod` writer does not record the **default kind** in effect at
compile time. A module compiled with `-fdefault-integer-8` writes `@var x :
integer` — with no kind — and a consumer compiled *without* `-fdefault-
integer-8` happily reads 4 bytes out of the 8-byte slot and silently gets a
miscompile (we reproduced: `gx = 7` in a `-fdefault-integer-8` module prints
as `7` with `0.0` for a companion `real` that actually holds `1.5`). This is
a **CRITICAL ABI-corruption** bug; explicit `integer(8)` IS persisted, so the
fix is tightly scoped to the default-kind path in the amod writer.

On top of those, `--std f2018` (space-form long option) consumes the next
positional as the std value; `-E` without `-o` silently writes preprocessed
output to a bare-stem file in CWD (users expect stdout); `-shared` hardcodes
`-no_uuid` which makes the produced dylib unlinkable by `ld`; `-c f1.f90
f2.f90` silently ignores `-c` and produces a linked executable named
`multi_c.o`; a 5000-deep nested expression crashes with a stack overflow and
**bypasses the ICE handler** (SIGABRT, exit 134, no bug-report template); `2**200`
silently saturates to `INT32_MAX`; and the Fortran parser treats an arbitrary
text file as a valid empty `PROGRAM` body populated with implicit-typed
procedure calls — "this is garbage" compiles to `call this; call is; call
garbage; ret`.

Finding count: **4 CRITICAL, 12 MAJOR, 11 MINOR**. See per-finding details
below and the remediation table at the end.

## Baseline state

- `cargo build` → clean (no warnings during this session).
- `tests/cli_driver.rs` — 21 tests exist (per the scope brief); test bench
  gap summary at the end lists missing coverage.
- `target/debug/armfortas` and `target/debug/afs` both produce identical
  binaries when invoked with identical args and identical output paths
  (FNV-hashed temp naming from `8d72bfa` holds; `cmp` reports byte-identical).
- Determinism scan: for every `(arithmetic, array_assign, allocatable,
  array_intrinsics, array_bulk_arithmetic)` × `(O0, O1, O2, O3, Os, Ofast)`
  combination, back-to-back compilation to `--emit-ir` and to `-S` is
  byte-identical. Same-output-path binary compilation is byte-identical.
  Different-output-path binaries differ *only* at offset 2112646 as
  documented (the OSO stab name). **Determinism is a bright WORKS.**
- Cross-opt runtime output: matched programs produce identical stdout on
  all six levels. No miscompile found in that subset.
- i128 (integer16): at `-O0` the backend rejects anything other than
  add/move/internal/external call; at `-O1+` the const-folded multiply works;
  runtime mul still blocked at every level — this matches the known
  limitation. Cross-opt `integer16_print.f90` and `integer16_format.f90`
  produce identical runtime output.

---

## Findings (severity-sorted)

All reproducers under `/tmp/audit32/**`. Commands assume
`ARM=/Users/matthewwolffe/Documents/GithubOrgs/FortranGoingOnForty/armfortas/target/debug/armfortas`.

### 1. CRITICAL: `.amod` writer drops the default-kind context, producing silent cross-TU ABI corruption

- **Reproducer:** `/tmp/audit32/amod_mismatch/m1.f90` + `user.f90`.
- **Command:**
  ```
  mkdir -p /tmp/audit32/amod_mismatch && cd /tmp/audit32/amod_mismatch
  $ARM -c -fdefault-integer-8 -fdefault-real-8 m1.f90 -o m1.o
  $ARM -I . -c user.f90 -o user.o
  ld -o prog m1.o user.o -lSystem -no_uuid -syslibroot $(xcrun --show-sdk-path) -e _main $WS/target/debug/libarmfortas_rt.a
  ./prog
  ```
  where `m1.f90` is `module m1; integer :: gx = 7; real :: gy = 1.5; end module`
  and `user.f90` prints `gx, gy`.
- **Expected:** either (a) a diagnostic at `use m1`, (b) a kind-mismatch link
  error, or (c) the values 7 and 1.5.
- **Actual:** prints `7   0.0000000E0`. The `integer(8)` storage happens to
  overlap with the `integer(4)` field at offset 0 so the low word reads 7,
  but the `real(4)` at the wrong offset reads garbage (zero, pad after the
  wide int). Silent data corruption across a TU boundary.
- **Root cause:** `src/sema/amod.rs` writer emits `@var gx : integer` with
  no kind annotation for names whose kind comes from
  `driver::defaults::default_int_kind()` rather than an explicit
  `integer(N)` declaration. The reader on the other side sees bare
  `integer` and assumes kind 4. Explicit `integer(8) :: big` IS stored
  correctly as `@var big : integer(8)` (verified in
  `/tmp/audit32/ekm.amod`) — so the amod schema supports kinds; the
  writer just doesn't consult `defaults::default_int_kind()`.
- **Impact:** any multi-file build where one TU uses `-fdefault-integer-8`
  or `-fdefault-real-8` and a consumer doesn't, silently miscompiles.
  Mixed-kind scientific code is the exact target use-case for these
  flags.
- **Suggested fix:** in `sema/amod.rs` when writing each `@var`, always
  emit the concrete kind (4 or 8 per type_layout) rather than omitting it
  when it matches the "standard default." Alternative: stamp the flags
  into the amod header (e.g. `# default_int_kind: 8`) and have the reader
  refuse to consume an amod whose default-kind context differs from the
  current compile's — gfortran's approach.

### 2. CRITICAL: `armfortas -c` silently ignores `-c` when passed multiple input files

- **Reproducer:** `/tmp/audit32/multi_di8_m.f90` + `/tmp/audit32/multi_di8_p.f90`.
- **Command:** `$ARM -c /tmp/audit32/multi_di8_m.f90 /tmp/audit32/multi_di8_p.f90 -o /tmp/audit32/multi_c.o`
- **Expected:** either two `.o` files (one per input) or a diagnostic that
  `-c` with `-o` requires exactly one input.
- **Actual:** exit 0, `multi_c.o` is a **Mach-O executable** (not an
  object!) 2.1MB, statically linked, callable. `file multi_c.o` confirms
  `Mach-O 64-bit executable arm64`. The driver chose the `compile_multi`
  path (because `extra_inputs` is non-empty) which hardcodes
  `emit_obj: true` on each sub-file but then links at the end anyway.
- **Impact:** build systems (Make, Meson, etc.) expect `-c x.f90 y.f90`
  to produce `x.o y.o`. Ours produces one linked binary with a `.o`
  extension that will break downstream ar/ld/link-dedup logic.
- **Root cause:** `src/driver/mod.rs` `compile_multi` at the end calls
  `link_multi` unconditionally without checking `opts.emit_obj`.
- **Suggested fix:** at the top of `compile_multi`, if `opts.emit_obj`,
  either (a) error out unless exactly one input is given, or (b) compile
  each input to its own `.o` next to the source and skip the final link.
  Option (b) is what gfortran does.

### 3. CRITICAL: `-E` without `-o` silently writes preprocessed output to a bare-stem file in the CWD

- **Reproducer:** `/tmp/audit32/hello.f90`.
- **Command:** `cd /tmp/audit32 && $ARM -E hello.f90`
- **Expected:** preprocessed output on stdout (gfortran/clang convention).
- **Actual:** creates a file named `hello` (no extension) in CWD containing
  the preprocessed text. On Unix this looks like an **executable** to the
  shell completer / `file(1)` identifies it as ASCII text but it has no
  shebang so trying to run it prints the Fortran source.
- **Impact:** silent footgun — `$ARM -E *.f90` in a build script would
  clutter CWD with dozens of bare-stem files, and if any of them happen
  to collide with an existing binary (`make`, `test`, `config` etc.) the
  compiler overwrites it without warning.
- **Root cause:** `Options::output_path` in `src/driver/mod.rs:215–234`
  has a cascade `if emit_asm … else if emit_obj … else if emit_ir … else
  stem`. The `preprocess_only` case falls to `stem`. Meanwhile
  `compile()` at line 647 already handles `out.as_os_str() == "-"` for
  stdout but never triggers it on the default path.
- **Suggested fix:** (a) in `output_path`, return `PathBuf::from("-")`
  when `preprocess_only && output.is_none()`; or (b) in the `-E` block
  in `compile()`, if `opts.output.is_none()`, write to stdout directly.
  gfortran uses (b).

### 4. CRITICAL: Arbitrary text silently parses as an implicit Fortran PROGRAM with implicit procedure calls

- **Reproducer:** `/tmp/audit32/garbage.f90` containing literally
  `this is garbage`.
- **Command:** `$ARM /tmp/audit32/garbage.f90 -o /tmp/audit32/garbage`
- **Expected:** parse error — a bare identifier at statement level is not
  a Fortran statement.
- **Actual:** parses to a PROGRAM with three `Call` statements: `call this`,
  `call is`, `call garbage`. The linker refuses at link time because the
  symbols don't exist (the only reason the user sees ANY diagnostic). Run
  `$ARM --emit-ast /tmp/audit32/garbage.f90 -o /tmp/audit32/g.ast` to see
  the misparse.
- **Impact:** if a user accidentally passes a shell script / README / text
  file to the compiler, they get a link error that doesn't mention their
  source file is nonsense. Worse, if the text happens to contain an
  identifier that matches a real module procedure (e.g. a .md file with
  the word `exit`), the compiler would silently emit a program that
  calls that function with zero arguments. Silent path to wrong-code.
- **Root cause:** the parser's statement recogniser accepts a bare
  identifier as a "call" statement without checking the spelling of
  `CALL`. Similar to gfortran's "line label only" leniency but far more
  permissive.
- **Suggested fix:** a bare identifier at statement level must be
  followed by `(` (function call / array reference assignment) or `=`
  (assignment). A bare identifier should be a parse error.

---

### 5. MAJOR: `--std f2018` (space-form) consumes the input file as the standard value

- **Reproducer:** `/tmp/audit32/hello.f90`.
- **Command:** `$ARM --std f2018 /tmp/audit32/hello.f90 -o /tmp/audit32/hf`
- **Expected:** --std=f2018 accepted; hello.f90 compiled. The help text
  documents `--std=<standard>` with no hint that the space form is
  rejected.
- **Actual:** `armfortas: unknown --std value: /tmp/audit32/hello.f90`
  (the filename was consumed as the value). Exit 1.
- **Root cause:** `src/driver/mod.rs:329–336` correctly accepts the
  space form, but `FortranStandard::parse_flag` rejects the input path.
  The error message says "unknown --std value" as if the user did
  `--std=/tmp/audit32/hello.f90` — they did not; they put the path
  after a space. The real bug is that the subsequent positional can no
  longer be used as input.
- **Suggested fix:** validate the std value looks like `f<digits>` before
  consuming it; if it starts with `/` or `.`, reject `--std` with "value
  required" and push the path back onto the positional list.

### 6. MAJOR: `-shared` emits dylib with hardcoded `-no_uuid` which `ld` then refuses to link against

- **Reproducer:** `/tmp/audit32/lib.f90`.
- **Command:**
  ```
  $ARM -c /tmp/audit32/lib.f90 -o /tmp/audit32/lib.o   # produces m.amod
  $ARM -shared /tmp/audit32/lib.f90 -o /tmp/audit32/liblib.dylib
  $ARM -I /tmp/audit32 -L /tmp/audit32 -llib /tmp/audit32/link_main.f90 -o /tmp/audit32/linked
  ```
- **Expected:** `linked` is an executable that prints `42`.
- **Actual:** `ld: missing LC_UUID load command in '/tmp/audit32/liblib.dylib'`.
  Exit 2.
- **Root cause:** `src/driver/mod.rs:1054` unconditionally passes
  `-no_uuid` to `ld` in both `link` and `link_multi`. Apple `ld` requires
  LC_UUID when the linked-against object is a dylib (dyld depends on it
  for dyld cache validation). Static executable linkage is fine without
  UUID; dylibs are not.
- **Suggested fix:** when `opts.shared`, skip the `-no_uuid` flag. The
  reproducibility argument for `-no_uuid` only applies to final
  executables and .o files, not dylibs that will be loaded dynamically.

### 7. MAJOR: `-shared` compilation does not emit `.amod` module-interface files

- **Reproducer:** `/tmp/audit32/lib.f90` (module `m`).
- **Command:** `$ARM -shared /tmp/audit32/lib.f90 -o /tmp/audit32/liblib.dylib`
- **Expected:** `m.amod` is written next to the dylib (or into `-J <dir>`)
  so downstream consumers can `use m`.
- **Actual:** no `.amod` is written. Users must compile a second time
  with `-c` to get the interface file.
- **Root cause:** `.amod` emission lives inside the `if opts.emit_obj`
  block in `compile()` (line ~981). Any non-object compilation skips it.
- **Suggested fix:** hoist the `.amod` loop out of the `emit_obj` block
  so it also runs for `-shared` and for final-binary compilations that
  expose a `MODULE` unit.

### 8. MAJOR: Eight CLI flags are parsed then thrown away — `-fcheck=bounds`, `-fcheck=all`, `-fmax-stack-var-size`, `-frecursive`, `-fbackslash`, `-Wall`/`-Wextra`/`-Wpedantic`/`-Wdeprecated`/`-Werror`, `-g`, `--diagnostics-format=json`

- **Reproducers:** `/tmp/audit32/bc.f90` (bounds-check toggle ineffective),
  `/tmp/audit32/warntest.f90` (`-Wall -Wextra -Werror` emit nothing),
  `/tmp/audit32/diagerr.f90` (`--diagnostics-format=json` outputs text).
- **Evidence:** `grep -r '(check_bounds|max_stack_var_size|recursive_default|backslash_escapes|warn_all|warn_extra|warn_pedantic|warn_deprecated|warn_as_error|diagnostics_format|debug_info)'`
  in `src/` turns up only field definitions, defaults, CLI assignments,
  and the copy in `compile_multi`. Never any pipeline consumer.
- **Impact:**
  - `-fcheck=bounds` does nothing. Bounds checks are always emitted; the
    flag cannot enable or disable them. Users who want a release build
    without bounds checks have no way to do it.
  - `-Werror -Wall` cannot promote any warning because no warning is
    emitted anywhere in sema/validate (validate returns DiagKind::Error
    or Warning but nothing pushes Warning from `-Wall`).
  - `--diagnostics-format=json` prints text exactly like the default.
    Build systems that parse JSON error streams will see gibberish.
  - `-fmax-stack-var-size=N` rejects non-numeric values correctly but
    `N` is never compared against any array size; the 64KB threshold
    from CLAUDE.md is a hardcoded constant.
  - `-g` accepts and sets `debug_info = true` but no DWARF, no STABS, no
    source-line load command is emitted. `dsymutil -dump-debug-map` on
    the output confirms the binary has none of our lines; the only
    debug symbols present are Rust-std ones pulled in transitively by
    `libarmfortas_rt.a`. Help text says "(DWARF emission TODO)" but not
    "(has no effect beyond being accepted)". Users with IDE tooling
    that pass `-g` expect at minimum a diagnostic that it's a no-op.
  - `-frecursive` should cause every procedure to be treated as
    RECURSIVE (local storage on stack, no SAVE by default). Not wired.
  - `-fbackslash` should cause `'\\n'` in a string literal to mean
    newline. The lexer handles `'\\n'` based on a per-file default; our
    CLI flag never reaches it.
- **Suggested fix:** for each of these, either (a) wire it to a pipeline
  consumer, (b) print a one-time stderr warning "--flag recognised but
  not yet implemented" and track it in `noted_issues.md`, or (c) reject
  it at parse time. Silent acceptance is the worst option.

### 9. MAJOR: `-J <dir>` for a non-existent / read-only / bogus dir silently emits a WARNING and exits 0, leaving the build in a broken state

- **Reproducer:** `/tmp/audit32/mymod.f90`.
- **Commands:**
  ```
  $ARM -c -J /tmp/audit32/nonexistent_dir /tmp/audit32/mymod.f90 -o /tmp/audit32/mymod_ne.o
  # → warning: cannot write .../mymod.amod: No such file or directory
  # → exit 0
  ```
  Also reproduces with a read-only dir.
- **Expected:** exit non-zero. A build that expected the `.amod` to exist
  for a later `use mymod` would cascade-fail silently.
- **Actual:** `warning:` prefix, exit 0, `.amod` missing. Consumers see
  "module not found" an arbitrary number of steps later.
- **Root cause:** `src/driver/mod.rs:1010` — `if let Err(e) = fs::write(...)
  { eprintln!("warning: ...") }`.
- **Suggested fix:** return an error (IO class, exit 3). If the user
  really wanted a best-effort, they didn't pass `-J` explicitly.

### 10. MAJOR: ICE handler bypassed by stack overflow; no bug-report template printed

- **Reproducer:** `/tmp/audit32/deepexpr.f90` (Python-generated, 5000-deep
  nested `(((...+1)+1)+1)` expression).
- **Command:** `$ARM /tmp/audit32/deepexpr.f90 -o /tmp/audit32/de`
- **Expected:** either (a) the compiler's ICE path: exit 4, bug-report
  template, version/platform/input info; or (b) the compiler internally
  grows the parser stack and compiles cleanly.
- **Actual:** stderr: `thread 'main' (...) has overflowed its stack /
  fatal runtime error: stack overflow, aborting`. Exit 134 (SIGABRT). No
  bug-report template. Users see "it crashed" with no direction.
- **Root cause:** `install_ice_hook` / `catch_unwind` can only catch
  panics, not stack overflows. The parser in `src/parser/expr.rs` is
  recursive descent with no explicit depth guard.
- **Suggested fix:** add a depth counter in `Parser::parse_primary_expr`
  / `parse_binop` that returns a diagnostic at e.g. 500 levels. This is
  what gfortran does (error: `Fortran 2018: expression too complex`).
  Alternatively, handle SIGSEGV with a sigaltstack handler that writes
  the ICE template — considerably more fragile.

### 11. MAJOR: `--time-report` produces NO output when compilation fails

- **Reproducer:** `/tmp/audit32/diagerr.f90`.
- **Command:** `$ARM --time-report /tmp/audit32/diagerr.f90 -o /tmp/audit32/de 2>&1`
- **Expected:** even on failure, print phase table up to the failing
  phase. `--time-report` is usually used for *debugging slow builds*,
  and a slow-build-that-fails is exactly what you want to profile.
- **Actual:** `aborting due to errors in …` and nothing else. The phase
  table was built in memory and discarded because `compile()` returns
  `Err(...)` and never calls `phases.report()`.
- **Suggested fix:** `PhaseTimer::report` should be callable from a
  `Drop` impl or from the top-level `cli_entry` regardless of the
  compile result. Move the `phases.report()` call to a location that
  runs on both Ok and Err paths.

### 12. MAJOR: `-fimplicit-none` overrides explicit `IMPLICIT INTEGER (i-n)` in program-unit scopes

- **Reproducer:** `/tmp/audit32/fim_block.f90`.
- **Command:** `$ARM -fimplicit-none /tmp/audit32/fim_block.f90`
- **Expected (gfortran-compatible):** per gfortran docs, `-fimplicit-none`
  is equivalent to adding `IMPLICIT NONE` where none is present. An
  explicit `IMPLICIT INTEGER (i-n)` in the source should WIN — the user
  explicitly asked for implicit typing.
- **Actual:** every program-unit scope has its explicit `implicit` rules
  clobbered by `none_type = true`. `i = 5` with an explicit
  `implicit integer (i-n)` preceding it is rejected as "variable 'i'
  used but not declared (IMPLICIT NONE is active)".
- **Root cause:** `SymbolTable::force_implicit_none_all_units` in
  `src/sema/symtab.rs:242–255` unconditionally sets `none_type = true`
  without checking if the scope already has explicit `implicit` rules.
- **Suggested fix:** only force `none_type = true` if
  `scope.implicit_rules.rules.is_empty() && !scope.implicit_rules.none_type`
  — i.e. scope has no explicit implicit statement at all. This matches
  gfortran.

### 13. MAJOR: `.amod` format stores no kind for module variables declared with an implicit default kind (even when -fdefault-integer-8/-fdefault-real-8 is NOT in effect)

- **Reproducer:** `/tmp/audit32/amod_mismatch/m1.amod` (auto-generated).
- **Observation:** `integer :: gx = 7` produces `@var gx : integer` (no
  kind). `integer(8) :: gx = 7` produces `@var gx : integer(8)`.
- **Impact:** even without `-fdefault-integer-8`, if the default kind
  ever changes across compile runs (some future `--std=f90-legacy` mode,
  or a cross-target build where processor default differs), the
  implicit-kind amods are unrecoverable. Make the format tell the truth
  always.
- **Suggested fix:** same file as Finding 1 — always write the concrete
  byte-width. This is a lower-severity companion; it's not exercised
  until #1's root cause is fixed. Filing separately so the fix sweep is
  complete.

### 14. MAJOR: `armfortas` and `afs` binaries are behaviourally identical but `afs --version` prints "armfortas 0.1.0"

- **Reproducer:** `$ARM --version; $AFS --version`
- **Expected:** `afs --version` identifies as `afs 0.1.0` (the short name
  is a first-class alias per the help text `armfortas | afs`, not a
  hidden backend).
- **Actual:** both print `armfortas 0.1.0 (aarch64-apple-darwin)`. Users
  who invoke `afs --version` in a script and parse the first word of
  stdout get `armfortas`, not `afs`.
- **Root cause:** `driver::version_string()` hard-codes `"armfortas"`;
  neither `main.rs` nor `bin/afs.rs` tells the library which invocation
  name was used.
- **Suggested fix:** pass `argv[0]` (or better, `std::env::current_exe()`
  file_stem) into `version_string`. Low-priority but a packaging/
  reproducibility smell.

### 15. MAJOR: `armfortas -o A -o B` silently drops `A` and writes `B` with no warning

- **Reproducer:** `/tmp/audit32/hello.f90`.
- **Command:** `$ARM /tmp/audit32/hello.f90 -o /tmp/audit32/out_a -o /tmp/audit32/out_b`
- **Expected:** either error ("-o already given") or warning. Most
  compilers error out or take the last one with a warning.
- **Actual:** exit 0, `out_a` does not exist, `out_b` was produced. No
  warning.
- **Impact:** shell aliases that prepend `-o log` to the command line
  interact badly with user-supplied `-o`. Easy to lose output.
- **Suggested fix:** in `parse_cli`, if `opts.output.is_some()` when
  `-o` is seen a second time, either reject or warn to stderr.

### 16. MAJOR: BOM-prefixed Fortran source files produce a bogus lexer error

- **Reproducer:** `/tmp/audit32/bom.f90` (UTF-8 BOM + `program p; …`).
- **Command:** `$ARM /tmp/audit32/bom.f90 -o /tmp/audit32/bom`
- **Expected:** BOM stripped silently; compile succeeds. gfortran, clang,
  and most tools handle this.
- **Actual:** `/tmp/audit32/bom.f90:1:1: lexer error: unexpected
  character: 'Ã'` (the first byte of the BOM decoded as a Latin-1 char).
- **Impact:** any Fortran source saved from Notepad or from a C/C++
  editor that defaults to "UTF-8 with BOM" is rejected with a cryptic
  error.
- **Suggested fix:** in the preprocessor or tokenizer, strip a leading
  `\u{FEFF}` (UTF-8 BOM) before lexing.

### 17. MINOR: UTF-8 lexer error quotes the byte, not the character

- **Reproducer:** `/tmp/audit32/utf8.f90` (contains `café`).
- **Command:** `$ARM /tmp/audit32/utf8.f90`
- **Actual:** `unexpected character: 'Ã'` (first byte of `é`).
- **Expected:** `unexpected character: 'é'` (or at least a hex escape).
- **Root cause:** lexer iterates on bytes, not on `char`s. When it hits
  a non-ASCII byte it cannot recognize, it formats as a single-byte
  char, truncating the grapheme.

### 18. MINOR: Parse errors and lexer errors do NOT go through the caret renderer

- **Reproducer:** `/tmp/audit32/lexerr.f90`, `/tmp/audit32/emptyline.f90`.
- **Expected:** same caret-and-gutter output as sema errors. Consistent
  diagnostic UX is a sprint-32 deliverable.
- **Actual:** `armfortas: <file>:<line>:<col>: lexer error: ...` — one
  line, no snippet, no caret. Only sema diagnostics run through
  `driver::diag::render`.
- **Suggested fix:** route lexer and parser errors through the same
  renderer. Needs access to the `source` string; plumb it in.

### 19. MINOR: Diagnostic gutter misaligned at 6+-digit line numbers

- **Reproducer:** `/tmp/audit32/bigline.f90` (100,000 comment lines +
  one error).
- **Actual:** caret line is `      |` (6 spaces before `|`) but the
  line-number line is `100003 |` (6 digits + space + `|` — also 6
  chars before `|`). Close inspection shows the pipes are column-
  aligned only up to 5-digit line numbers; beyond that, the `|` in the
  caret row sits to the left of the line-number row's `|`.
- **Root cause:** `src/driver/diag.rs:89` uses `{gutter:>5}` for the
  line-number row but `      |` (6 spaces) for the caret row.
- **Suggested fix:** dynamic width — compute width = `max(5, digits)`
  and use that for both rows.

### 20. MINOR: Nested response files (`@file` inside a `@file`) are not recursively expanded

- **Reproducer:** `/tmp/audit32/outer.rsp` (contains `@/tmp/audit32/inner.rsp`).
- **Expected:** `@inner` is expanded inside the outer expansion.
- **Actual:** `armfortas: cannot read '@/tmp/audit32/inner.rsp': No such
  file or directory (os error 2)`. The outer response file was expanded
  once and the `@inner` token was fed to `fs::read_to_string` with the
  literal `@` prefix still stripped, then failed because the filename
  passed to `fs::read_to_string` is `@/tmp/...` (not a valid path).
- **Root cause:** `expand_response_files` is single-pass and checks
  `arg.strip_prefix('@')` only on the input list.
- **Suggested fix:** recurse (with a depth-limit, say 8, to avoid the
  circular-response-file case below).

### 21. MINOR: Circular response files don't loop, but the error is misleading

- **Reproducer:** `/tmp/audit32/self.rsp` containing `@/tmp/audit32/self.rsp`.
- **Expected:** error ("circular response file") or loop-safe one-pass
  behaviour.
- **Actual:** one-pass expand reads the self-reference, tokenizes as
  `@/tmp/audit32/self.rsp`, feeds to parse_cli which does NOT re-expand
  (since that's done up-front), so the `@` prefix is never stripped and
  `parse_cli` reports "cannot read response file"… but actually the
  message says `cannot read response file '/tmp/audit32/self.rsp': No
  such file or directory` — except the file DOES exist. The actual
  error is that parse_cli got a filename-looking token that strip_prefix
  stripped, and `fs::read_to_string` on the stripped path worked and
  returned the same circular body, and… actually in practice it errors
  because `expand_response_files` was not called again. But the error
  message makes the user think the file doesn't exist.
- **Root cause:** same as Finding 20.
- **Suggested fix:** same (recursive with depth limit).

### 22. MINOR: Response file parser uses bare `split_whitespace()` — no quoting, no escape, no paths-with-spaces

- **Reproducer:** `/tmp/audit32/spaced.rsp` containing
  `/tmp/audit32/file with spaces.f90 -o /tmp/audit32/spaced_out`.
- **Expected:** `"/tmp/audit32/file with spaces.f90"` (quoted) or
  backslash-escaped would be accepted, per GNU ld's response file
  conventions.
- **Actual:** splits on whitespace, treats `/tmp/audit32/file` as one
  arg, `with`/`spaces.f90` as two more. Error: `cannot read
  '/tmp/audit32/file'`.
- **Root cause:** `expand_response_files` calls
  `body.split_whitespace()`.
- **Suggested fix:** borrow GNU ld's parser (respects double-quotes,
  backslash escapes, single-quote spans). A 20-line function.

### 23. MINOR: Response files containing CRLF line endings are parsed as space-separated so the CR is silently tolerated, but binary / non-UTF8 content crashes with a generic message

- **Reproducer:** `/tmp/audit32/binary.rsp` (2K random bytes).
- **Actual:** `cannot read response file '/tmp/audit32/binary.rsp': stream did not contain valid UTF-8`.
- **Exit code:** 1 (compile). The driver's error message contains
  "cannot read" which should trigger EXIT_IO (3) via the classifier, but
  the parse-CLI error path uses direct `process::exit(EXIT_COMPILE)`
  without running it through `classify_compile_error`.
- **Suggested fix:** call the classifier from the parse-CLI failure
  path as well. Low-priority — binary-file response file isn't a real
  scenario — but the exit-code taxonomy should be consistent.

### 24. MINOR: Compile-time integer overflow silently saturates to INT32_MAX

- **Reproducer:** `/tmp/audit32/tricky1.f90` (prints `2**200`),
  `/tmp/audit32/overflow.f90` (prints `-2147483649` as parameter).
- **Actual:** both print `2147483647`. No warning, no error.
- **Expected:** per Fortran 2018, evaluation of constant expressions
  outside the representable range is an error (processor may detect at
  compile time). Silently producing `INT_MAX` is the worst option.
- **Suggested fix:** in constant evaluation, detect overflow and emit a
  diagnostic. Already a pattern in sema/resolve.rs for other ranges.

### 25. MINOR: Compile-time division by zero silently produces 0

- **Reproducer:** `/tmp/audit32/tricky3.f90` (`integer, parameter :: x = 1 / 0`).
- **Actual:** prints `0`, no warning.
- **Expected:** compile-time diagnostic (gfortran warns). At the very
  least, signal it as an ERROR STOP at runtime would be defensible.
- **Suggested fix:** sema constant evaluator returns `Err` when the RHS
  of `IntDiv` is a literal zero.

### 26. MINOR: `-I=<dir>` (clang-style equals form) parses the `=` into the path

- **Reproducer:** `/tmp/audit32/mod_test/mymod.f90`.
- **Command:** `$ARM -I=/tmp/audit32/mod_test -c /tmp/audit32/use_mod.f90`
- **Expected (clang/gcc compat):** `=` is stripped, path is
  `/tmp/audit32/mod_test`, module found.
- **Actual:** path is literally `=/tmp/audit32/mod_test`, lookup fails,
  error: `module 'mymod' not found`.
- **Suggested fix:** in the joined-form arm of `-I` (and friends),
  `strip_prefix("=")` on the tail.

### 27. MINOR: Filename-starting-with-`@` cannot be passed as a compile input

- **Reproducer:** `/tmp/audit32/@file.f90`.
- **Command:** `$ARM @file.f90` (cwd is `/tmp/audit32`) or
  `$ARM @/tmp/audit32/@file.f90`.
- **Actual:** in both cases the `@` prefix is interpreted as
  response-file marker, the first character of the path is stripped,
  and `fs::read_to_string("file.f90")` fails. No way to pass a
  literally-named `@file.f90`.
- **Suggested fix:** document that filenames starting with `@` are not
  supported directly, and allow an escape like `@\@file.f90` or
  `--input @file.f90`. Minor corner case but worth documenting.

### 28. MINOR: `-gBLAH` (arbitrary suffix) silently turns on debug info

- **Reproducer:** `$ARM -gblob /tmp/audit32/hello.f90 -o /tmp/audit32/h`
- **Expected:** error ("unknown option: -gblob") or at least a warning
  — `-gdwarf-5` etc. in gcc only work with specific suffixes.
- **Actual:** silently treats it as `-g` (line 378: `arg if
  arg.starts_with("-g") => opts.debug_info = true`). Typo-tolerant
  in the worst way.
- **Suggested fix:** whitelist the `-g*` suffixes that are supported
  (empty, `0`-`3`, `dwarf*`, `stabs*`, `ggdb*`) and reject the rest.

### 29. MINOR: `-Weverything` and other unknown `-W*` flags are silently accepted

- **Reproducer:** `$ARM -Weverything /tmp/audit32/hello.f90 -o …`
- **Actual:** line 369-374 unconditionally pushes any unknown `-W` into
  `disabled_warnings` without a warning. The comment explains this is
  gfortran-compat, but gfortran actually prints `Warning: unrecognized
  command line option '-Weverything'` when such a flag would affect
  behaviour.
- **Suggested fix:** emit a warning on stderr for each unknown `-W`
  flag (subject to `-Wno-unknown-warning-option`).

### 30. MINOR: `-shared` + `-static` together: both silently applied

- **Reproducer:** `$ARM -shared -static /tmp/audit32/hello.f90 -o /tmp/audit32/ss`
- **Actual:** both flags set; `push_link_flags` passes `-dylib` AND
  `-search_paths_first`; `ld` happens to produce an executable because
  `-dylib` forces a dylib but the compile stem decides it's an
  executable (I haven't traced through exactly what ld does). The
  behaviour is determined by ld's last-flag-wins policy, not ours.
- **Suggested fix:** mutually-exclusive check in `parse_cli` — later
  flag wins with a warning, or straight error.

### 31. MINOR: No-input-files prints HELP_TEXT to stderr then exits 1

- **Reproducer:** `$ARM`
- **Expected:** HELP_TEXT to stdout, exit 0 — conventional CLI help
  behaviour. Or at least HELP_TEXT to stdout, exit 2 (usage error,
  documented in POSIX as distinct from compile errors).
- **Actual:** HELP_TEXT to stderr, exit 1. Also exit 1 is documented as
  "compile error" in `cli_entry` so this is a misclassification.
- **Suggested fix:** exit 2 (usage) and direct to stdout, matching
  gfortran/clang behaviour. Keep the "no input" error message in
  stderr if you want, but don't smear the usage text across both.

### 32. MINOR: `-o=path` (equals form) is rejected but `--std=value` is the ONLY long option with mandatory `=`

- **Reproducer:** `$ARM /tmp/audit32/hello.f90 -o=/tmp/out`
- **Actual:** `armfortas: unknown option: -o=/tmp/out`. Silent
  inconsistency with the `--long=value` style available for `--std`,
  `--diagnostics-format`, `-fmax-stack-var-size`. Users trying to be
  consistent stumble.
- **Suggested fix:** support `-o=<path>` as a synonym, either via
  strip-prefix in the joined-form arm or by normalising `-o=foo` ->
  `-o foo` during tokenization.

### 33. MINOR: `--help --version` takes whichever comes last; no consistent precedence documented

- **Reproducer:** `$ARM --help --version` prints version.
- **Minor** but footgun-adjacent if scripts set e.g. `ALIAS_ARGS=--help` and
  user passes `--version`; they expect version.

---

## Determinism (WORKS)

Confirmed from the scope-mandated scan:

- `--emit-ir` at every opt level: byte-identical across two runs (12
  programs × 6 levels = 72 cases; 0 diffs).
- `-S` at every opt level: byte-identical (same scope; 0 diffs).
- Binary to same output path: byte-identical.
- Binary to different output paths: differ at offset 2112646 only (OSO
  stab), as documented in `src/driver/mod.rs` lines 922-941 of `compile()`.
- `armfortas` vs `afs` with identical args and identical output paths:
  byte-identical. (Version-string issue is Finding 14.)

## Cross-opt consistency (WORKS)

5 diverse test programs × 6 opt levels (O0..Ofast) produce byte-identical
stdout for each program. No miscompile surfaces in that matrix.

i128 (integer16) cross-opt: `integer16_print.f90` and `integer16_format.f90`
produce identical output at every opt level. Backend rejection at O0 for
runtime-i128-mul is a known limitation (const-folded mul at O1+ works).

## Other WORKS (surprising things that did hold up)

- Circular module dependency detection: both direct (`use a; use b; use
  a`) and self-use are diagnosed at compile_multi time.
- Long filenames (200 chars): accepted by the Mach-O emitter.
- Filenames with spaces in positional args (via shell-quoted input):
  handled.
- Filenames with Unicode (héllo.f90): handled.
- `-I` with a non-existent dir: silently accepted (matches clang).
- CRLF-terminated source: handled.
- `NO_COLOR=1` / `CLICOLOR_FORCE=1` / `CLICOLOR_FORCE=0`: all honored
  correctly.
- F2003 `IMPORT` inside interface body (sprint 31 Finding 7): now
  WORKS.
- `sub_opts.default_integer_8` propagation to `compile()` per sub-file
  in `compile_multi`: propagates correctly; the kind info is just lost
  at amod-write time (Finding 1).
- The `-v` verbose output goes to stderr (correct).
- Exit code 2 fires for linker failures (tested with broken AFS_RUNTIME_PATH
  pointing at an empty .a file).
- Exit code 3 fires for "input file doesn't exist" (via `cannot read`
  classifier match).
- Exit code 4 fires for a Rust panic in the pipeline (harder to trigger;
  stack overflow is Finding 10, which escapes the catch_unwind).

---

## Test-bench gap summary

Existing `tests/cli_driver.rs` covers flag parsing. Missing coverage:

1. **amod-default-kind round-trip consistency**: write a module with
   `-fdefault-integer-8`, compile a consumer without it, assert either a
   diagnostic or correct runtime behaviour. Catches Finding 1.
2. **`-c` with multiple inputs**: assert either two `.o` files OR a
   meaningful error. Catches Finding 2.
3. **`-E` with no `-o`**: assert output on stdout, not a CWD bare-stem
   file. Catches Finding 3.
4. **Garbage-text input**: assert a compile error, not silent acceptance.
   Catches Finding 4.
5. **`--std f2018` space-form**: assert it's accepted (preferred) or that
   the next positional is NOT eaten. Catches Finding 5.
6. **`-shared` end-to-end**: build a dylib, then link against it; assert
   success. Catches Findings 6 and 7.
7. **Flag round-trip assertion**: for each of the eight no-op flags, some
   structural check that it reaches a pipeline consumer (e.g. for
   `-fcheck=bounds`, assert the emitted IR does NOT contain
   `__afs_check_bounds` when the flag is OFF; when ON, it does). Catches
   Finding 8.
8. **`-J` into non-existent / RO dir**: assert non-zero exit. Catches
   Finding 9.
9. **Parser / lexer depth guard**: assert a 5000-deep expression produces
   a graceful error, not SIGABRT. Catches Finding 10.
10. **`--time-report` on error**: assert phase table prints even when the
    compile fails. Catches Finding 11.
11. **`-fimplicit-none` with explicit `implicit integer (i-n)`**: assert
    the explicit rule wins, matching gfortran. Catches Finding 12.
12. **`afs --version`**: assert output starts with `afs`, not `armfortas`.
    Catches Finding 14.
13. **Duplicate `-o`**: assert later flag wins with a stderr warning OR
    an error. Catches Finding 15.
14. **BOM source**: assert compile succeeds. Catches Finding 16.
15. **Diag caret alignment at ≥6-digit line numbers**: string-match the
    rendered output to assert column-alignment. Catches Finding 19.
16. **Nested response files** (`@outer` contains `@inner`): assert
    content is fully expanded. Catches Finding 20.
17. **Response files with quoted/spaced tokens**: pick a convention and
    test it. Catches Finding 22.
18. **Constant overflow / div0**: assert a diagnostic instead of silent
    saturation. Catches Findings 24-25.
19. **`-I=<dir>` equals form**: assert it's handled like `-I <dir>`.
    Catches Finding 26.
20. **`-gBLAH` suffix**: assert rejection. Catches Finding 28.
21. **Unknown `-W` flags**: assert stderr warning. Catches Finding 29.
22. **`-shared -static`**: assert error or warning. Catches Finding 30.
23. **No-input**: assert exit 2 (usage), not 1. Catches Finding 31.

At least 23 test cases above would be good additions. None of them require
pipeline changes to *write* — all exercise the CLI / driver surface that
sprint 32 introduced.

---

## Summary table

| # | Severity | Area | Finding | Fix location |
|---|----------|------|---------|---|
| 1 | CRITICAL | amod / ABI | .amod writer drops default kind; cross-TU ABI corruption | `src/sema/amod.rs` writer |
| 2 | CRITICAL | driver | `-c` with multi-input silently links to executable | `src/driver/mod.rs:compile_multi` |
| 3 | CRITICAL | driver | `-E` without `-o` writes CWD bare-stem file instead of stdout | `src/driver/mod.rs:output_path` / `compile()` |
| 4 | CRITICAL | parser | Garbage text parses as implicit PROGRAM with implicit `call` stmts | `src/parser/stmt.rs` statement recogniser |
| 5 | MAJOR | CLI | `--std f2018` eats the input filename as std value | `src/driver/mod.rs:329` |
| 6 | MAJOR | linker | `-shared` + `-no_uuid` produces unlinkable dylib | `src/driver/mod.rs:1054` |
| 7 | MAJOR | driver | `-shared` skips .amod emission | `src/driver/mod.rs:981-1020` |
| 8 | MAJOR | driver | Eight CLI flags parsed and ignored (`-fcheck=bounds`, `-Wall`, `-g`, `--diagnostics-format=json`, ...) | multiple — wire or warn |
| 9 | MAJOR | driver | `-J` failure is a warning + exit 0 | `src/driver/mod.rs:1010` |
| 10 | MAJOR | parser / ICE | Deep expression → SIGABRT, bypasses ICE handler | `src/parser/expr.rs`; depth guard |
| 11 | MAJOR | driver | `--time-report` prints nothing on failure | `src/driver/mod.rs:PhaseTimer` / `compile()` |
| 12 | MAJOR | sema | `-fimplicit-none` overrides explicit `IMPLICIT INTEGER (i-n)` | `src/sema/symtab.rs:242` |
| 13 | MAJOR | amod format | Implicit-kind module variables lose kind in amod | same as #1 |
| 14 | MAJOR | branding | `afs --version` prints "armfortas" | `src/driver/mod.rs:version_string` |
| 15 | MAJOR | CLI | Duplicate `-o` silently drops the first | `src/driver/mod.rs:258-260` |
| 16 | MAJOR | lexer | BOM source → bogus lexer error | `src/preprocess` or lexer |
| 17 | MINOR | lexer | UTF-8 byte displayed instead of char in error | `src/lexer/` |
| 18 | MINOR | diag | Parser/lexer errors bypass caret renderer | `src/driver/mod.rs` call sites |
| 19 | MINOR | diag | Caret alignment broken at 6+-digit line numbers | `src/driver/diag.rs:89` |
| 20 | MINOR | CLI | Nested response files not expanded | `src/driver/mod.rs:expand_response_files` |
| 21 | MINOR | CLI | Circular response file gives misleading error | same |
| 22 | MINOR | CLI | Response file parser lacks quoting | same |
| 23 | MINOR | CLI | `cannot read response file` → exit 1 instead of 3 | `src/lib.rs:cli_entry` |
| 24 | MINOR | sema | Constant integer overflow saturates silently | sema const evaluator |
| 25 | MINOR | sema | Constant `1/0` silently evaluates to 0 | sema const evaluator |
| 26 | MINOR | CLI | `-I=<dir>` parsed with `=` in the path | `src/driver/mod.rs:284` |
| 27 | MINOR | CLI | Filename starting with `@` cannot be input | `src/driver/mod.rs:expand_response_files` |
| 28 | MINOR | CLI | `-gBLAH` silently sets debug_info | `src/driver/mod.rs:378` |
| 29 | MINOR | CLI | Unknown `-W*` flags silently accepted | `src/driver/mod.rs:369-374` |
| 30 | MINOR | CLI | `-shared` + `-static` both applied | `src/driver/mod.rs:318-319` |
| 31 | MINOR | CLI | No-input sets HELP to stderr + exit 1 | `src/lib.rs:cli_entry` |
| 32 | MINOR | CLI | `-o=path` rejected (inconsistent with `--std=`) | `src/driver/mod.rs:258` |
| 33 | MINOR | CLI | `--help --version` precedence undocumented | `src/driver/mod.rs:parse_cli` info_action |

**Severity breakdown:** 4 CRITICAL, 12 MAJOR, 17 MINOR (33 total).

### Recommended remediation order

1. **#1 + #13** (one sweep of the amod writer) — silent ABI corruption
   is worse than any other class of compiler bug. Fix together.
2. **#4** (garbage-text compiles) — the parser accepting bare identifier
   statements is a trust-destroying default. Real Fortran code gets
   misparsed fewer ways when the parser is strict here.
3. **#2** (`-c` + multi-input behaviour) — break build systems today,
   will get worse as adopters appear.
4. **#3** (`-E` default output) — classic UNIX convention violation, one
   line of code.
5. **#8** (eight dead flags) — either wire or warn, but not silently
   accept. This is the single largest cluster of sprint-32 stubs.
6. **#10** (ICE handler bypassed by stack overflow) — parser depth
   guard is a small PR.
7. **#12** (`-fimplicit-none` vs explicit `implicit`) — behavioural
   incompatibility with gfortran.
8. **#5 / #6 / #7 / #9** (CLI edge cases that break real workflows).
9. **#11 / #14 / #15 / #16** (quality-of-life, one-liners mostly).
10. Everything else as a sweep.

## Reproducer index

All paths absolute, all re-runnable.

```
/tmp/audit32/amod_mismatch/m1.f90, user.f90     # #1 / #13
/tmp/audit32/multi_di8_m.f90, multi_di8_p.f90   # #2
/tmp/audit32/hello.f90                          # #3 / #5 / #6
/tmp/audit32/garbage.f90                        # #4
/tmp/audit32/lib.f90, link_main.f90             # #6 / #7
/tmp/audit32/mymod.f90                          # #9
/tmp/audit32/deepexpr.f90                       # #10
/tmp/audit32/diagerr.f90                        # #11
/tmp/audit32/fim_block.f90                      # #12
/tmp/audit32/out_a, out_b                       # #15 residue
/tmp/audit32/bom.f90                            # #16
/tmp/audit32/utf8.f90                           # #17
/tmp/audit32/lexerr.f90, emptyline.f90          # #18
/tmp/audit32/bigline.f90                        # #19
/tmp/audit32/outer.rsp, inner.rsp               # #20
/tmp/audit32/self.rsp                           # #21
/tmp/audit32/spaced.rsp                         # #22
/tmp/audit32/binary.rsp                         # #23
/tmp/audit32/tricky1.f90, overflow.f90          # #24
/tmp/audit32/tricky3.f90                        # #25
/tmp/audit32/mod_test/mymod.f90, use_mod.f90    # #26
/tmp/audit32/@file.f90                          # #27
```
