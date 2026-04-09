# Testing 03: Metamorphic and Generated Testing

## Goal

Make the harness cleverer by testing compiler invariants across related program
families instead of only isolated handwritten examples.

## Metamorphic Testing

Metamorphic tests assert that semantics-preserving source rewrites preserve the
observable outcome and selected stage invariants.

Planned rewrite families:

- introduce/remove temporary variables
- reorder commutative arithmetic
- convert simple `if` forms
- inline or extract tiny helper procedures
- module import alias rewrites
- constant-hoist rewrites

The harness should treat these as paired or family tests, not unrelated files.

## Generated Families

Add stable generator-backed families for:

- loop and control-flow variants
- allocatable/string descriptor stress
- I/O formatting combinations
- module graph shapes
- register-pressure and spill shapes

Generated output must be:

- stable
- auditable
- either checked in directly or produced by a stable generator with predictable
  output

## Constraints

- generated families should remain readable enough to debug
- metamorphic families should identify the transformation used
- failures must preserve the exact variant that failed

## Acceptance Scenarios

- a metamorphic pair that must agree on runtime output
- a metamorphic pair that must also preserve an IR or ASM invariant
- one generated family where many variants share one expectation scheme
- one generated family aimed specifically at register pressure or spill behavior
