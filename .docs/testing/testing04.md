# Testing 04: Determinism, Reduction, Bundles, and Triage

## Goal

Turn determinism and failure analysis into a testing program instead of a small
handful of one-off regressions.

## Determinism Program

Move from "compile this file twice" to targeted reproducibility coverage:

- pass-specific reproducibility
- normalized assembly comparisons
- normalized object comparisons where feasible
- module/global ordering stability
- graph-shape determinism where generated inputs are involved

`REPRO_CHECK` becomes the leaf-assertion way to ask for this intentionally.

## Reduction And Triage

Every deep failure should leave behind a reducer-friendly bundle:

- source that failed
- observed artifacts that failed
- exact assertion class that failed
- enough provenance to replay the case

`bencch` already has stronger bundle/reporting surfaces; this sprint is about
making those surfaces serve the shared source-directed testing language.

## Shared Outcome Model

The root harness and `bencch` should agree on the conceptual failure families:

- runtime mismatch
- diagnostic mismatch
- IR/ASM shape mismatch
- unsupported directive/capability
- reproducibility failure
- reference divergence

## Acceptance Scenarios

- one reproducibility test for `asm`
- one reproducibility test for `obj`
- one failure bundle that preserves the exact source and artifact surface
- one unsupported-directive/capability example reported cleanly instead of
  misleadingly
