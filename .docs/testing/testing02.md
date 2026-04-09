# Testing 02: Pipeline Oracles and Side-Effect Checks

## Status

Active.

The first concrete slice is now rooted in the armfortas-first harness:

- `OPT_EQ` is the first explicit cross-opt oracle
- it lets one source assert that selected surfaces must agree across
  optimization levels
- the initial component set is:
  - `stdout`
  - `stderr`
  - `exit`
  - `asm`

That gives the harness a deliberate place to express "runtime invariant across
opt levels" without pretending every IR or ASM shape must remain frozen.

The next concrete slice is phase triangulation:

- `PHASE_TRIANGULATE` lets one runtime test require successful
  `--emit-ir`, `-S`, and/or `-c` materialization at the same opt level
- this is a bench feature, not a compiler feature: it strengthens how we
  observe pipeline coherence without changing what the compiler does

## Goal

Push the harness beyond "compile, run, compare stdout" into richer pipeline
oracles that catch full-compiler failures earlier and more precisely.

## Priority Work

### Phase triangulation

Add assertions that connect multiple surfaces of the same compilation:

- compile-and-run behavior
- `-S` emission
- `-c` object production
- emitted IR
- final linked execution

We want tests that can say "the runtime answer is right, but the IR/ASM/object
shape is wrong" or "the linked binary disagrees with the direct artifact path."

### Diagnostic quality

Expected diagnostics should assert both:

- the message content
- the source location

That means `ERROR_EXPECTED` plus `ERROR_SPAN` becomes a standard diagnostic
pairing.

### Filesystem behavior

The harness sandbox should become a first-class oracle surface:

- files created
- files not created
- expected file contents
- I/O side effects like rewind/flush behavior

### Cross-opt invariants

The docs must distinguish:

- invariants that must match across opt levels
- shape assertions that are meaningful only at `-O0`

That distinction prevents accidental over-constraint.

## Acceptance Scenarios

- one program that checks stdout, stderr, and exit code together
- one program that must diagnose at the right source span
- one program with assembly-shape assertions
- one program that checks sandbox file contents
- one program that proves a runtime invariant across multiple opt levels

## Definition Of Done

The implementation plan is complete when the first-wave directives and runner
rules are specific enough to add these tests without policy questions.
