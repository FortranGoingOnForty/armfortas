# Testing Track Preservation and Branch Posture

## Purpose

Record where the active testing work lives, what older worktrees still contain
valuable code, and how to treat that preserved work while the testing roadmap
continues on the armfortas-first track.

This note exists so we do not lose work through confusion, duplicate work by
accident, or merge large old branches without a clear reason.

## Active Worktrees

The testing track now uses this split:

- main checkout: `/Users/matthewwolffe/Documents/GithubOrgs/FortranGoingOnForty/armfortas`
  - stays on `trunk`
  - not the place for ongoing `codex/*` work
- active harness worktree: `/tmp/armfortas-harness-reset`
  - branch: `codex/harness-reset`
  - mission: armfortas-first harness expansion and testing roadmap execution
- preserved structured-runner worktree: `/private/tmp/bencch-next`
  - branch: `codex/bencch-next`
  - mission: the larger `bencch`-centered structured runner line
- older preserved worktree: `/private/tmp/afs-tests`
  - branch: `codex/afs-tests`
  - mission: earlier checkpoint from the same general `bencch` line

## Remote Preservation Status

These branches are intentionally preserved on remotes:

- `armfortas`
  - `codex/harness-reset`
  - `codex/bencch-next`
  - `codex/afs-tests`
- `bencch`
  - `codex/harness-reset`
  - `codex/bencch-next`

That means the committed history is no longer “only in a worktree.”

## Dirty Local Preservation

There is still uncommitted older work in the preserved worktrees, especially in:

- `/private/tmp/bencch-next`
- `/private/tmp/bencch-next/afs-as`
- `/private/tmp/afs-tests`
- `/private/tmp/afs-tests/afs-as`

Those dirty states are preserved locally as patch snapshots in:

- `/tmp/armfortas-preservation/2026-04-09/`

Treat those as archival recovery material until a specific revival decision is
made. Do not assume they should be merged wholesale.

## Keep Live

These lines remain active and worth preserving as first-class branches:

- `codex/harness-reset`
  - this is the active testing-track branch
  - continue Testing 01+ here
  - use this for armfortas-first harness design and shared-language work
- `codex/bencch-next`
  - keep as the preserved structured-runner branch
  - this is the best stopping point of the larger `bencch` line
  - keep it available for reference, selective porting, and possible future
    heavy-lane work

## Maybe Port Later

These ideas from `codex/bencch-next` are worth porting selectively when they
help the armfortas-first testing doctrine:

- shared source-directive compatibility in `bencch`
- report output, failure bundle, and provenance UX
- capability-aware planning and clearer unsupported-surface reporting
- authored matrix and differential orchestration for heavier campaigns
- module-graph campaign structure
- selective standalone/bootstrap improvements that make testing surfaces easier
  to run, not because they restore “generic bench” as the main product

Port these by small cherry-picks or reimplementation slices, not by merging the
entire branch back into the active harness line.

## Leave Frozen

These parts of the old `bencch` line should remain preserved but not treated as
the current product center:

- “compiler-agnostic bench” as the primary north star
- `bencch compare` / `introspect` as the main testing story for the repo
- broad generic-adapter expansion for its own sake
- standalone productization work that does not directly help armfortas-first
  testing campaigns
- older `codex/afs-tests` as a historical checkpoint once `codex/bencch-next`
  exists

Frozen does **not** mean “delete.” It means “do not keep extending this by
default.”

## Branch Policy

Use this rule set going forward:

1. Continue new testing-track implementation on `codex/harness-reset`.
2. Do not move the main checkout off `trunk`.
3. When older `bencch` work is needed, prefer:
   - read the preserved branch
   - isolate the idea
   - cherry-pick or reimplement only the useful slice
4. Do not merge `codex/bencch-next` wholesale into the current harness branch
   unless the product story changes again.
5. Treat the dirty preserved worktree state as archival until there is a named
   recovery goal.

## Practical Recommendation

The pivot should be real, but not destructive.

We should:

- pivot hard on **mission**
- not pivot hard on **memory**

In practice that means:

- armfortas-first testing doctrine stays in charge
- `bencch` remains a power tool
- preserved structured-runner branches remain available
- useful ideas move across by deliberate, narrow slices
