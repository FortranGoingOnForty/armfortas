# Selected OpenMP V&V tests

This directory contains a deliberately small, manifest-driven host subset from
the OpenMP Validation and Verification suite. Imported test sources are copied
unchanged so local rewrites cannot hide frontend or semantic failures.

- Upstream: <https://github.com/OpenMP-Validation-and-Verification/OpenMP_VV>
- Revision: `f7d95b342b9330ecac735ed31b9fb193d8f60a9b`
- Revision date: 2026-09-17
- License: [LICENSE](LICENSE)

`manifest.tsv` records the upstream path, local source, requested operation,
expected upstream result, OpenMP version, coverage, current ARMFORTAS state,
and SHA-256 for every imported source. `tests/openmp_vv.rs` builds each passing
row at `-O0` and `-O3` and performs its requested host operation when the test
runner has a native execution path.

The selected OpenMP 5.0 `loop` cases currently cover scalar `private`, scalar
INTEGER addition `reduction`, and rectangular `collapse(1)`/`collapse(2)`
clauses with implicit binding to an enclosing `parallel` region. The collapse
case also verifies that a loop outside the requested collapse depth remains
sequential.

The upstream `ompvv/ompvv.F90` umbrella header contains device-offload probes
outside the current ARMFORTAS tranche. The local `support/ompvv.F90` adapter
implements only the result-reporting macros required by the selected host
tests. It is clearly marked as local support and is not represented as an
unchanged upstream file.

This corpus is feature-level conformance pressure, not a claim of complete
support for the OpenMP version named by a source.
