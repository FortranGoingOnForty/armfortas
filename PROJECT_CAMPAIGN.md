# Project Campaign

This document turns the next differential-testing wave into a concrete
execution plan inside the repository. `fortsh` remains the flagship proof
point, but it is no longer the only real-world target we use to evaluate
`armfortas`.

The corresponding runner/catalog lives in [`bencch`](./bencch/README.md) and
is exposed through:

```bash
cargo run -p afs-tests -- projects list
cargo run -p afs-tests -- projects run --project fortbite --armfortas-bin ./target/release/armfortas
```

## Objectives

- Prioritize compiler coverage first, with some C interop allowed.
- Use native project build systems before inventing custom harnesses.
- Compare `armfortas` against `flang-new` on real codebases, not only reduced
  reproducers.
- Classify every armfortas-only divergence before accepting it as a project bug.
- Seed a reusable Fortran app/systems library surface from repeated patterns
  across the FortranGoingOnForty repos.

## Benchmark Ladder

Current staged order:

1. `fortbite`
2. `ferp`
3. `sniffert`
4. `fortress`
5. `fit`
6. `facsimile`
7. `fortty` later

Secondary targets worth keeping warm:

- `fuss`
  - smaller git-oriented TUI with UTF-8 tree rendering and shell/workflow hooks
  - good backup interactive target when we want less dependency noise than the editor/TUI stack
- `fgof-fs`
  - filesystem/path library with dense native tests
  - good for `.amod`, packaging, and runtime I/O truthfulness
- `fgof-process`
  - subprocess/process library with capture, timeout, and shell behavior tests
  - good system-interop target without full application complexity
- `fgof-pty`
  - PTY/session library with interactive subprocess and resize coverage
  - good terminal-truthfulness target before the heavier TUI/editor programs
- `fgof-termios`
  - terminal-mode helper library with raw/cbreak/echo and terminal-size coverage
  - good low-noise tty truthfulness target before the heavier expect and line-edit stacks
- `fgof-lineedit`
  - line editing state/history library for shells and REPL-style tools
  - good focused target for interactive editing semantics without terminal-mode noise
- `fgof-watch`
  - portable file-watching library with polling-backed watch-session semantics
  - good low-dependency target for filesystem change detection and library packaging behavior

Current de-prioritized projects:

- `convolution`
- `aero-emulation`
- `wasm`
- `feducative`
- `karaoPy`

Those repos are still useful for spot checks, but they are too small or too
specialized to drive the next compiler phase.

## Differential Method

For each project:

- Build it cleanly with `armfortas` and `flang-new`.
- Use the project’s native build system first (`make`, `fpm`, `cmake`).
- If a project’s checked-in default flags are compiler-vendor-specific, keep the
  native build system and record the smallest override needed to reach a fair
  cross-compiler comparison.
- Record:
  - build success/failure
  - test success/failure
  - runtime smoke behavior
  - wall-clock compile time
  - link/runtime crashes
  - materially different warnings or stderr
- For interactive targets, keep the same feature toggles and build mode on both
  compilers.
- When behavior differs:
  - reduce to the smallest project-local repro
  - reduce again to a standalone compiler repro
  - only then patch `armfortas`

Use these buckets when summarizing each project:

- `Pure semantics/codegen`
- `Cross-TU / .amod truthfulness`
- `System / C interop`
- `Interactive / terminal behavior`
- `Performance`

## Acceptance Bar

The next campaign is considered successful when:

- `fortbite`, `ferp`, `sniffert`, `fortress`, and `fit` all build under both
  `armfortas` and `flang-new`
- their native tests or smoke workflows run under both
- all armfortas-only failures are reduced and classified
- `facsimile` becomes the next flagship bring-up target after that smaller
  ladder is mostly green
- no compiler fix lands without either:
  - a standalone regression, or
  - a project-local regression plus a documented reason no smaller repro exists

## Library Seeding

The gap worth filling is not BLAS-style numerics. The real missing batteries
are app/systems modules that C ecosystems take for granted.

Target module families:

- `fortargs`
  - argument parsing, subcommands, help text, env fallback
- `fortpath`
  - path join/split/normalize, cwd/home/temp helpers, directory walking basics
- `fortproc`
  - subprocesses, env vars, pipes, signals, exit codes, platform wrappers
- `fortterm`
  - ANSI styling, terminal size, raw mode, key decoding, screen redraw helpers
- `fortds`
  - hash maps, deques, ring buffers, gap buffers, tries, lookup tables
- `fortconfig`
  - config loading and precedence helpers for TOML/env/CLI layering

These libraries should be module-first packages, ideally `fpm`-friendly, with
minimal C wrappers only where system APIs demand them.

## Library-Backed Sample Apps

Use the library surface to drive the second wave of realistic but controlled
sample projects:

- grep-like search CLI on `fortargs` + `fortpath`
- file tree / disk usage explorer on `fortterm` + `fortpath`
- merge conflict resolver on `fortterm` + diff helpers
- process runner / task launcher on `fortproc`
- HTTP/REST client only later, after the terminal/filesystem/process substrate
  is solid

## Notes

- `fortsh` remains the current flagship proof that the compiler can build and
  run a large interactive program.
- The `bencch` project ladder is intentionally staged so dependency-heavy
  targets like `fortty` do not hide compiler bugs too early.
- Success here is not “we built one more repo once.” Success is a repeatable,
  explainable differential story across multiple real Fortran applications.
