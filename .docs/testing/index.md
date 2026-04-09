# ARMFORTAS Testing Track Index

This is the parallel roadmap for harness design, testing language, and testing
campaigns. It complements `.docs/sprints/` instead of replacing it.

- [Testing 00](testing00.md) — Harness Reset, Sitrep, and Doctrine
- [Testing 01](testing01.md) — Shared Annotation Language
- [Testing 02](testing02.md) — Pipeline Oracles and Side-Effect Checks
- [Testing 03](testing03.md) — Metamorphic and Generated Testing
- [Testing 04](testing04.md) — Determinism, Reduction, Bundles, and Triage
- [Testing 05](testing05.md) — fortsh-Scale and Large-Program Campaigns

## First Implementation Sequence

The testing track starts in this order:

1. shared-language docs and examples
2. first-wave directives in the root harness
3. `bencch` compatibility for those directives
4. phase-triangulation and filesystem assertions
5. metamorphic and generated-family scaffolding
6. determinism/reduction expansion

That sequence is deliberate:

- first define one shared language
- then make the root harness the reference implementation
- then scale the ideas through `bencch`

This keeps the compiler repo as the creative lab while still letting `bencch`
stay strong at structured campaigns.

## Supporting Notes

- [Preservation and Branch Posture](preservation.md) — active worktrees,
  preserved branches, and what to keep live vs selectively port later
