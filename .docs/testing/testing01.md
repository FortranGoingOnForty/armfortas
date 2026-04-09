# Testing 01: Shared Annotation Language

## Goal

Define one source-directed assertion language for both the root harness and
`bencch`.

The root harness is the source-of-truth implementation for new directives.
`bencch` consumes the same directives where supported and reports unsupported
directives explicitly.

## Canonical Existing Directives

These stay canonical:

- `! CHECK: <substring>`
- `! XFAIL: <reason>`
- `! ERROR_EXPECTED: <substring>`
- `! IR_CHECK: <substring>`
- `! IR_NOT: <substring>`

## First-Wave New Directives

These are the next directives to implement and document together:

- `! STDERR_CHECK: <substring>`
  - ordered substring checks against stderr
- `! EXIT_CODE: <int>`
  - exact process exit code
- `! ERROR_SPAN: <line>:<col>`
  - composes with `ERROR_EXPECTED`
- `! ASM_CHECK: <substring>`
- `! ASM_NOT: <substring>`
  - assembly-shape assertions
- `! FILE_CHECK: <relative-path> => <substring>`
- `! FILE_NOT: <relative-path> => <substring>`
  - sandbox file side-effect assertions
- `! REPRO_CHECK: asm|obj|run`
  - per-test reproducibility assertions

## Later Directives

Do not implement these in the first wave, but reserve the shape now:

- `! MIR_CHECK: <substring>`
- `! MIR_NOT: <substring>`
- `! OPT_EQ: O0,O1,O2 => stdout|stderr|exit|asm`

## Rules

- source comments are the canonical leaf-assertion layer
- `bencch` suite text stays the orchestration layer, not a second assertion
  language
- unsupported directives must report as unsupported in the active
  runner/build/adapter
- unsupported directives must never be silently ignored

## Acceptance Scenarios

The language definition is ready when the docs clearly specify:

- a runtime test with `CHECK`, `STDERR_CHECK`, and `EXIT_CODE`
- a diagnostic test with `ERROR_EXPECTED` and `ERROR_SPAN`
- an IR test with `IR_CHECK` and `IR_NOT`
- an ASM test with `ASM_CHECK` and `ASM_NOT`
- a filesystem-side-effect test with `FILE_CHECK` and `FILE_NOT`
- a reproducibility test with `REPRO_CHECK`
