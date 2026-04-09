# ARMFORTAS Testing Track

This is the parallel roadmap for compiler testing and harness design.

It does **not** replace the main compiler implementation sprints under
`.docs/sprints/`. Those remain the roadmap for frontend, IR, codegen, runtime,
and language feature work. This track exists because the compiler now has two
serious testing surfaces:

- the root `armfortas` in-tree harness in `tests/` and `test_programs/`
- the structured `bencch/` runner and suite corpus

The old north star for `bencch` drifted too far toward "generic compiler bench"
as the product. That is no longer the center of gravity. The new north star is:

> build the most interesting, creative, efficient full-pipeline compiler test
> system for `armfortas`, with `bencch` as a power tool around it.

## Doctrine

### Armfortas-first, not bench-first

The compiler repo is the place where new testing ideas should be born first.
The root harness is where we can be the most direct, weird, and ambitious about
full-pipeline compiler assertions.

`bencch` stays important, but its value is now clearer:

- structured matrices
- reference/differential runs
- capability-aware execution
- report output and bundles
- module graphs and larger authored campaigns

### Shared language, separate roles

Source comments are the canonical leaf-assertion language.

That means comment directives inside fixture programs are the primary way to
describe:

- runtime expectations
- expected diagnostics
- stage-shape expectations
- future per-test side-effect and reproducibility assertions

The root harness implements new directives first. `bencch` consumes the same
directives where supported and reports unsupported directives explicitly.

The suite DSL in `bencch` remains the orchestration layer for:

- opt matrices
- compiler selection
- reference selection
- graph composition
- capability policy
- report/bundle behavior

The suite DSL should not invent separate leaf-assertion semantics when shared
source directives can do the job.

### Execution lanes

Testing work is organized into three execution lanes:

- `fast lane`
  - runtime stdout/stderr/exit assertions
  - expected diagnostics
  - small source-directed checks that should run constantly
- `deep lane`
  - IR and ASM shape checks
  - reproducibility checks
  - phase-triangulation and artifact consistency assertions
- `heavy lane`
  - differential/reference campaigns
  - module graph campaigns
  - generated families
  - large-program and fortsh-scale campaigns

The root harness should dominate `fast` and much of `deep`.
`bencch` should carry more of `heavy`.

## Division Of Labor

### Root armfortas harness

- canonical home of source-embedded directive semantics
- fastest path for end-to-end `armfortas` testing
- best place for creative full-pipeline assertions
- best place for armfortas-only internals like deep stage shape checks
- default home for new annotation ideas

### bencch

- structured runner for the same source-directed testing language
- best place for opt matrices, references, graphs, bundles, and reports
- should reuse source directives whenever possible instead of duplicating leaf
  assertions in suite text
- should explain unsupported directives clearly when adapter/build constraints
  prevent evaluation

## Acceptance Standard

This testing track is succeeding when we can add a new idea once in the shared
annotation language, prove it quickly in the root harness, then scale it out
through `bencch` campaigns without inventing a second testing dialect.
