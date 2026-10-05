# Selected official OpenMP examples

This directory contains a deliberately small, manifest-driven selection from
the OpenMP Architecture Review Board's Examples repository. The sources are
copied unchanged so that local rewrites cannot hide frontend or semantic
compatibility failures.

- Upstream: <https://github.com/OpenMP/Examples>
- Revision: `3e4757ae2b52e51df6cd5d363d6fc1d894509719`
- Release: `v6.0.1`
- Revision date: 2025-11-14
- License: [LICENSE](LICENSE)

`manifest.tsv` records the upstream path, source form, requested operation
(`compile`, `link`, or `run`),
upstream expectation, OpenMP version, covered behavior, and current ARMFORTAS
state for every imported example. `tests/openmp_official_examples.rs` runs all
rows whose state is `pass` at `-O0` and `-O3`.

This is a conformance pressure set, not a claim that ARMFORTAS implements the
whole OpenMP version named by an example. Add examples only when their specific
constructs and clauses are within the advertised subset. Record larger gaps in
`OPENMP.md` rather than weakening or modifying an upstream source.
