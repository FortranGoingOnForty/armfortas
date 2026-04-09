# Testing 00: Harness Reset, Sitrep, and Doctrine

## Goal

Reset the testing roadmap around an armfortas-first mission without discarding
`bencch`.

The point of this sprint is not new harness code. The point is to settle the
product story, testing doctrine, and ownership boundaries before more testing
surface area lands.

## Sitrep

The root harness already has the most interesting raw ideas in the repo:

- source-embedded `! CHECK:` runtime assertions
- living bug tracking via `! XFAIL:`
- expected-diagnostic assertions via `! ERROR_EXPECTED:`
- IR shape assertions via `! IR_CHECK:` and `! IR_NOT:`
- full opt-matrix end-to-end runs
- explicit determinism tests for emitted assembly

`bencch` is already strongest at other things:

- authored opt matrices
- differential/reference execution
- capability-aware planning and reporting
- module graphs
- report output and failure bundles

The problem is not that one of these harnesses should die. The problem is that
they were drifting toward two different product stories.

## Decision

We explicitly pivot to:

- root harness as the creative, armfortas-first testing lab
- `bencch` as the structured matrix/reporting/differential runner
- one shared source-directed assertion language across both

We explicitly do **not** keep "compiler-agnostic bench" as the main vision.

## Preservation Posture

That pivot is about the mission, not about pretending the older `bencch` work
never happened.

The preserved structured-runner line should stay available as a parallel branch
family, with selective later ports of ideas that help the armfortas-first
testing doctrine. See `.docs/testing/preservation.md` for the branch/worktree
inventory and the keep / maybe-port / leave-frozen policy.

## Deliverables

- testing doctrine documented in `.docs/testing/overview.md`
- a parallel testing sprint track under `.docs/testing/`
- repo-facing docs updated to tell one coherent story
- the first implementation sequence written down so later work does not drift

## Definition Of Done

- the testing doctrine is decision-complete
- the division of labor between root harness and `bencch` is explicit
- the shared-language choice is explicit
- the next testing sprints are concrete enough to implement without revisiting
  the product question
