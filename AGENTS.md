# ARMFORTAS

Local working guide for agents in the `armfortas` workspace root. `CLAUDE.md`
is the tracked, authoritative policy file; this document adds a reality-checked
snapshot of the current implementation so we do not confuse the roadmap with
shipped code.

## Repository Context

`armfortas` is the bespoke ARM64 Fortran compiler workspace. The root crate is
the compiler proper; the workspace also carries the standalone assembler
(`afs-as`), the standalone linker (`afs-ld`), the runtime archive, and the
benchmark harness crates.

The boundary between the major pieces matters:

- `armfortas` owns preprocessing, lexing, parsing, semantic analysis, IR,
  optimization, ARM64 codegen, CLI orchestration, and `.amod` module files.
- `afs-as` is the standalone ARM64 assembler submodule.
- `afs-ld` is the standalone Mach-O linker submodule.
- The parent driver still shells out to the system `as` and system `ld` on the
  default compile-and-link path in this checkout.

The project is macOS-only, arm64-only, and intentionally bespoke. No LLVM, no
borrowed frontend, no parser generators, no compiler-infrastructure crates.

## Definition Of Done

The finish line is not "parses a subset" or "links hello world once." It is:

- compile fortsh cleanly on Apple Silicon with zero ARM-specific workarounds
- own the whole toolchain end-to-end, including linker parity through `afs-ld`
- support the real Fortran language, not just the subset one codebase happens
  to exercise
- produce deterministic, ABI-correct output across separate compilation units

## Current Reality

This repo is much further along than the early sprint plans imply, but it is
not "finished compiler" territory either.

What is implemented now:

- full front/middle/back pipeline in the root crate:
  preprocess -> lexer -> parser -> sema -> IR -> optimization -> ARM64 codegen
- assembly text emission and object/binary production through system tools
- `.amod` module interface writing/reading for separate compilation
- a substantial optimizer stack under `src/opt/`
- a large top-level regression suite in `tests/*.rs`
- standalone `afs-as` and `afs-ld` submodules in the same workspace

What is still true in this checkout:

- the parent driver still links with Apple's `ld`, not `afs-ld`
- sprint docs are partly aspirational; code, tests, and audits are the real
  source of truth for what exists today
- audit 32 is the active closeout document for the current CLI/driver surface
- `.amod` is now a critical ABI boundary; bugs there are cross-TU miscompile
  bugs, not "just metadata" bugs

## Known Risk Areas

As of the current audit cycle, pay extra attention to:

- CLI flags that are parsed but not wired to real behavior
- `.amod` truthfulness, especially concrete kinds and cross-unit defaults
- parser permissiveness that turns nonsense input into executable code
- diagnostic consistency across lexer, parser, sema, and driver paths
- multi-file driver behavior (`-c`, `-shared`, `-J`, response files, exit codes)

Read `.docs/audits/audit32.md` and `.docs/audits/audit32_todo.md` before making
substantial driver-facing changes.

## Build And Test

Primary commands:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace
```

Useful targeted commands:

```bash
cargo build --bins
cargo test --test cli_driver
cargo test --test <name>
cargo test -p afs-as
cargo test -p afs-ld
cargo test --workspace -- <substring>
```

Integration tests shell out to system tools in several places. Do not replace
real toolchain interactions with fake fixtures when the system behavior is the
thing being tested.

Run long jobs deliberately. Prefer the smallest targeted command that proves the
change you just made.

## Project Structure

Actual source tree today:

```text
armfortas/
├── CLAUDE.md
├── AGENTS.md
├── Cargo.toml
├── src/
│   ├── ast/
│   ├── preprocess/
│   ├── lexer/
│   ├── parser/
│   ├── sema/
│   ├── ir/
│   ├── opt/
│   ├── codegen/
│   ├── driver/
│   ├── runtime/
│   ├── main.rs
│   └── lib.rs
├── tests/                      # top-level integration/regression tests
├── runtime/                    # libarmfortas_rt crate
├── afs-as/                     # assembler submodule
├── afs-ld/                     # linker submodule
├── sample_programs/
├── test_programs/
├── fuzz/
├── bencch/
└── .docs/
```

The working tree may also contain many generated `.ir`, `.s`, binaries, and
audit repro artifacts. Do not assume a pristine checkout.

## Implemented Pipeline Vs Planned Pipeline

Implemented default driver path today:

```text
argv
  -> driver::parse_cli
  -> preprocess
  -> lexer
  -> parser
  -> sema
  -> IR lowering
  -> optimization
  -> ARM64 codegen
  -> assembly text
  -> system as
  -> object file
  -> system ld
```

Toolchain direction of travel:

```text
armfortas frontend/backend -> afs-as -> afs-ld
```

When planning work, distinguish between:

- what the parent driver does today
- what `afs-as` can do standalone
- what `afs-ld` can do standalone
- what the sprint docs say should eventually be wired together

## Development Guidance

### 1. Trust code, tests, and audits over roadmap prose

Read these in order before substantial work:

1. `CLAUDE.md`
2. the active audit in `.docs/audits/`
3. the relevant source module
4. the tests covering that module
5. the relevant sprint doc only after the above

If docs and code disagree, treat code plus tests plus audits as the truth about
what exists today.

### 2. Preserve the bespoke contract

- No parser generators.
- No LLVM, Cranelift, or borrowed compiler frontend.
- No compiler-infrastructure dependencies without an explicit discussion.
- Keep `afs-as` and `afs-ld` independent at the Rust type level unless there is
  a very strong reason not to.

### 3. Treat CLI flags as user-facing contracts

A parsed flag must do one of three things:

- change pipeline behavior
- produce an explicit warning that it is not implemented
- be rejected

Silently accepting a no-op is the worst option.

### 4. `.amod` must tell the truth

- Emit concrete ABI-relevant type information.
- Do not rely on ambient defaults surviving across translation units.
- Separate compilation bugs are correctness bugs, not UX bugs.

### 5. Hard errors beat silent wrong answers

If behavior is incomplete, fail loudly. Do not quietly compile nonsense, erase
user intent, or produce binaries/interfaces that look valid but are wrong.

### 6. Keep regression tests paired with fixes

Every audit closeout item should land with a focused regression test in the
same patch when practical. Driver bugs belong in `tests/cli_driver.rs` or a new
integration test file if subprocess/runtime behavior matters.

### 7. Use the references before inventing semantics

If behavior is unclear, read `.refs/` and compare against gfortran/flang/clang
or Apple toolchain behavior before choosing semantics. The standard is the spec;
reference toolchains are reality checks.

### 8. Commit discipline still matters

- terse imperative messages
- no co-authors
- per-file / per-chunk commits
- no monoliths
- no sprint numbers in commit subjects
