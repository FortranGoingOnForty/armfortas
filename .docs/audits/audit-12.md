# Audit 12: reproducibility and performance across compiler/toolchain boundaries

Reviewed implementation commit: `23857aa48f3bc0160303842488e8578acb487fb1`

## Scope and method

This review covered repeated IR/assembly/object generation, source-path and input-order sensitivity, temporary naming and cleanup, unordered collections at serialization boundaries, structural compile-time scaling, allocation/copy hot paths, assembler/linker scaling, and benchmark-gate coverage. I used focused source inspection and small local reproducers only; I did not run the workspace test suite.

Because other audit workers were active and `/tmp` was under heavy concurrent load, I did not draw wall-clock conclusions. Performance findings below use exact structural lower bounds or output-shape counts. As a negative control, 12 fresh-process `-O2` compilations of `test_programs/two_loops.f90` produced one SHA-256 each for IR, assembly, and object output. Additional path-neutral fixtures were stable across relative, absolute, copied, and renamed paths. The failures below are specific boundaries rather than general process noise.

## Confirmed discrepancies

### A12-01 — High — `.amod` array-result bounds depend on `HashMap` seed

Source: `src/sema/amod.rs:854-897`, especially the unsorted `pscope.symbols.iter().find(...)` at `src/sema/amod.rs:881-891`. The adjacent result-name selection correctly sorts candidates at `src/sema/amod.rs:803-847`, but the bounds lookup independently chooses the first non-argument variable. The consumer falls back to assumed shape when the field is absent at `src/sema/resolve/use_resolution.rs:560-589`.

Reproducer from the repository root:

```bash
d=$(mktemp -d)
: >"$d/hashes"
for n in $(seq 1 30); do
  target/debug/armfortas \
    test_programs/defined_unary_array_result_assignment.f90 \
    -O0 -c -J "$d" -o "$d/out.o" >/dev/null
  sha256sum "$d/defined_unary_array_result_assignment_m.amod" >>"$d/hashes"
  grep '^@function flip_matrix' \
    "$d/defined_unary_array_result_assignment_m.amod"
done
cut -d' ' -f1 "$d/hashes" | sort | uniq -c
```

Actual: 30 fresh processes produced two interface forms. In the observed run, 25 omitted bounds and five emitted them:

```text
@function flip_matrix -> real, result_rank=2, result_name=out
@function flip_matrix -> real, result_rank=2, result_name=out, result_array_bounds="(size(a, 2); size(a, 1))"
```

The object hash remained identical. Intended behavior is to identify `out`, the actual result variable, and always serialize its explicit bounds. The missing form is loaded as `AssumedShape`; separate or submodule compilation can therefore change result allocation and hit the runtime-shape failure that the comments at `src/sema/amod.rs:854-859` say this field prevents. Confidence: very high.

### A12-02 — High — the optimizer silently stops before its claimed fixpoint

Source: `src/opt/inline.rs:134-138` transforms one site and explicitly relies on another fixpoint iteration; `src/opt/pass.rs:44,67-103` stops after 32 rounds even if round 32 changed IR; `src/driver/mod.rs:1686` discards `PassRunResult`, so truncation is neither reported nor rejected.

```bash
for n in 32 33 40; do
  remaining=$(
    target/release/armfortas -ffree-form -O1 --emit-ir -o /dev/stdout <(
      {
        printf '%s\n' 'program p' 'implicit none' 'integer :: x' 'x = 0'
        for i in $(seq 1 "$n"); do printf '%s\n' 'x = inc(x)'; done
        printf '%s\n' 'print *, x' 'contains' \
          'integer function inc(y)' 'integer :: y' 'inc = y + 1' \
          'end function inc' 'end program p'
      }
    ) | rg -c 'call @afs_internal_' || true
  )
  printf 'source_calls=%d residual_internal_calls=%d\n' \
    "$n" "${remaining:-0}"
done
```

Actual:

```text
source_calls=32 residual_internal_calls=0
source_calls=33 residual_internal_calls=1
source_calls=40 residual_internal_calls=8
```

Intended behavior is a true fixpoint or an explicit non-convergence diagnostic. Actual optimization quality has a hard, silent 32-site cliff; O1 can run roughly 480 pass invocations (and O2 roughly 960) and still retain eligible calls. This costs compiler work and leaves avoidable runtime calls in generated code. Confidence: very high.

### A12-03 — High — liveness materializes a quadratic call-crossing matrix outside the OOM guard

Source: both `src/codegen/x86/liveness.rs:299-320` and `src/codegen/arm64/liveness.rs:307-336` scan every call for every interval and allocate a separate `Vec<u32>` of crossings. The x86 guard at `src/codegen/x86/liveness.rs:123-130` and `src/codegen/mod.rs:60-80` accounts only for two block bitsets; ARM64 has no corresponding guard.

```bash
for n in 10 100; do
  target/release/armfortas -ffree-form -O1 --emit-ir -o /dev/stdout <(
    {
      printf '%s\n' 'program p' 'implicit none' 'external opaque'
      for i in $(seq 1 "$n"); do printf 'integer :: v%d\n' "$i"; done
      for i in $(seq 1 "$n"); do printf 'v%d = %d\n' "$i" "$i"; done
      for i in $(seq 1 "$n"); do printf '%s\n' 'call opaque()'; done
      for i in $(seq 1 "$n"); do printf 'print *, v%d\n' "$i"; done
      printf '%s\n' 'end program p'
    }
  ) | awk -v n="$n" \
    '/const_int/{c++} /call @opaque/{o++} /call @afs_write_int/{w++}
     END{printf "N=%d values=%d opaque_calls=%d later_uses=%d\n",n,c,o,w}'
done
```

Actual shapes were `10/10/10/10` and `100/100/100/100`; the N=100 case also reached ARM codegen with `--target arm64-macos -O1 -S`. Each value is a machine vreg (`src/codegen/x86/isel.rs:578-594`, `src/codegen/arm64/isel.rs:814-817`) live across the opaque calls, so the implementation records at least N² crossings. A one-block x86 function appears cheap to the guard while crossing storage is quadratic.

Intended behavior is interval/call indexing or lazy crossing queries whose memory is proportional to the liveness representation. Actual behavior can exhaust memory without tripping the stated OOM guard; ARM64 is even less protected. Confidence: high from exact structure; I intentionally did not raise N until OOM.

### A12-04 — High — ARM64 recomputes liveness and sorts all historical assignments for every instruction

Source: `src/codegen/arm64/mod.rs:46-53` computes liveness, then `src/codegen/arm64/linearscan.rs:118-120` computes it again. In `apply_allocation`, every instruction rebuilds and sorts the complete assignment vector and allocates fresh used-register sets at `src/codegen/arm64/linearscan.rs:646-711`; it also linearly searches split records inside the assignment scan at line 697. The assignments map deliberately retains expired vregs (`src/codegen/arm64/linearscan.rs:124-129`).

```bash
for n in 10 100; do
  target/release/armfortas -ffree-form -O1 --emit-ir -o /dev/stdout <(
    {
      printf '%s\n' 'subroutine chain(x)' 'implicit none' \
        'integer, intent(inout) :: x'
      for i in $(seq 1 "$n"); do printf '%s\n' 'x = x + 1'; done
      printf '%s\n' 'end subroutine chain'
    }
  ) | awk -v n="$n" \
    '/ = iadd /{a++} /^      store /{s++}
     END{printf "N=%d iadds=%d stores=%d\n",n,a,s}'
done

target/release/armfortas --target arm64-macos -ffree-form -O1 -S \
  -o /dev/null <(
    {
      printf '%s\n' 'subroutine chain(x)' 'implicit none' \
        'integer, intent(inout) :: x'
      for i in $(seq 1 100); do printf '%s\n' 'x = x + 1'; done
      printf '%s\n' 'end subroutine chain'
    }
  )
```

Actual IR contained 10/10 and 100/100 add/store pairs and the ARM backend accepted N=100. Even this low-pressure sequence leaves Θ(V) historical assignments. Applying allocation is therefore Θ(I×V log V), or Θ(I² log I) when V grows with I, plus repeated transient allocation; liveness and its potentially quadratic crossing vectors are also built twice. Intended behavior is to compute liveness once and precompute stable assignment order/occupancy outside the instruction loop. Consequence: large ARM functions can suffer disproportionate compile time and memory pressure. Confidence: very high.

### A12-05 — High — multi-source linking aliases objects with equal basenames

Source: `src/driver/mod.rs:2411-2430` creates one PID-only directory and derives every temporary object solely from `file_stem`; `src/driver/mod.rs:2470-2474` then records each aliased path in the link list.

```bash
d=$(mktemp -d); mkdir -p "$d/a" "$d/b"
cat >"$d/a/foo.f90" <<'EOF'
subroutine helper()
  print *, "helper"
end subroutine
EOF
cat >"$d/b/foo.f90" <<'EOF'
program main
  call helper()
end program
EOF
(cd "$d" && /tmp/armfortas-audit/target/release/armfortas \
  a/foo.f90 b/foo.f90 -o collided)
```

Actual: both sources compile to `/tmp/afs_multi_<pid>/foo.o`; the second overwrites the first and the linker receives the same program object twice, reporting duplicate `__prog_main` and `main` definitions. Intended behavior is one distinct object per source while preserving the requested link order. This blocks legitimate builds containing common basenames in different directories and can substitute the wrong translation unit before the linker diagnoses it. Confidence: very high.

### A12-06 — High — Mach-O archive member fetching reparses the archive quadratically

Source: initial loading validates and discards parsed metadata at `afs-ld/src/lib.rs:1122-1146`; symbol seeding opens it again at `afs-ld/src/resolve.rs:1008-1021`; each fetched member opens it again at `afs-ld/src/resolve.rs:1260-1279`; `-all_load` schedules every member at `afs-ld/src/resolve.rs:1381-1408`. Each open walks members and rebuilds the index (`afs-ld/src/archive.rs:202-214`), and member lookup is another linear scan (`afs-ld/src/archive.rs:244-249`).

```bash
archive=afs-ld/tests/parity_corpus/runtime_fortran_three_func_exec/files/libarmfortas_rt.a
ar t "$archive" | wc -l
stat -c '%s bytes' "$archive" 2>/dev/null || stat -f '%z bytes' "$archive"
target/debug/afs-ld -arch arm64 -dylib -undefined dynamic_lookup \
  -all_load "$archive" -o /tmp/a12-all-load.dylib
```

Actual fixture shape: 482 entries and 11,587,656 bytes. With 481 ordinary members, the `-all_load` path performs about 484 full metadata parses and roughly 233,000 member-header visits before parsing member objects; parallel jobs duplicate the same scans. Intended complexity is approximately O(M+K) by retaining the parsed member/offset/index metadata. Actual complexity is O(K×M), becoming O(M²) under `-all_load`. Consequence: large runtime/static archives waste CPU, allocation, and memory bandwidth, and adding `-j` does not remove the duplicated work. Confidence: very high from the forced-load path and fixture inventory.

### A12-07 — Medium — different Mach-O images receive the same `LC_UUID`

Source: `afs-ld/src/macho/writer.rs:460-479` calls `stable_uuid`; `afs-ld/src/macho/writer.rs:707-745` hashes only layout names, addresses, sizes, offsets, and flags—not code/data bytes, symbols, relocations, dylib identity, or other load-command content.

```bash
d=$(mktemp -d); mkdir -p "$d/u1" "$d/u2"
printf '.text\n.globl _main\n_main:\n mov w0, #1\n ret\n' |
  target/debug/afs-as - -o "$d/u1/main.o"
printf '.text\n.globl _main\n_main:\n mov w0, #2\n ret\n' |
  target/debug/afs-as - -o "$d/u2/main.o"
target/debug/afs-ld -arch arm64 -e _main \
  -o "$d/u1/same.out" "$d/u1/main.o"
target/debug/afs-ld -arch arm64 -e _main \
  -o "$d/u2/same.out" "$d/u2/main.o"
sha256sum "$d"/u{1,2}/same.out
for f in "$d"/u{1,2}/same.out; do
  llvm-objdump --macho --private-headers "$f" | grep -A2 LC_UUID
done
```

Actual: output hashes differed (`c16cf2f…` versus `73d559c2…`) but both UUIDs were `ACAC29DF-4E8C-4BFA-BC11-513090D757F9`. Intended behavior is stable UUIDs for identical rebuilds and different UUIDs for different linked image content. UUID-keyed dSYM lookup, crash symbolication, and caches can conflate unrelated equal-shaped binaries. Confidence: complete.

### A12-08 — Medium — x86 `afs-as` emits randomized local-COMMON symbol order

Source: `afs-as/src/x86/assemble.rs:602-620` stores local COMMON metadata in a `HashMap`; `afs-as/src/x86/assemble.rs:683-696` directly iterates it into the object model. The ELF writer preserves model order within the local partition at `afs-as/src/elf.rs:626-643`.

```bash
d=$(mktemp -d)
{
  printf '%s\n' '.text' '.globl entry' '.type entry,@function' \
    'entry:' '  ret' '.size entry, .-entry'
  for s in alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima; do
    printf '.local %s\n.comm %s,8,8\n' "$s" "$s"
  done
} >"$d/local-common.s"
for n in $(seq 1 24); do
  target/debug/afs-as --64 "$d/local-common.s" -o "$d/$n.o"
done
sha256sum "$d"/*.o | cut -d' ' -f1 | sort -u | wc -l
readelf -sW "$d/1.o"; readelf -sW "$d/2.o"
```

Actual: 24 fresh processes produced 24 object hashes, with different local-symbol orders. Intended behavior is byte-identical output governed by declaration order or an explicit stable order. Runtime semantics are equivalent, but object hashes, content-addressed caches, reproducible builds, and byte-differential validation fail. Confidence: very high.

### A12-09 — Medium — ARM/Mach-O `afs-as` resolves every relocation by scanning every symbol

Source: after sorting symbols, `afs-as/src/assemble.rs:2618-2637` calls `symbols.iter().position(...)` for every pending symbolic relocation.

```bash
gen() {
  n=$1
  printf '.data\n'
  for i in $(seq 1 "$n"); do printf '.quad _external_%d\n' "$i"; done
}
for n in 1000 10000; do
  gen "$n" | target/debug/afs-as - -o "/tmp/a12-relocs-$n.o"
done
```

This input has N distinct symbols and N relocations. Since every symbol is searched once, the implementation performs at least `N(N+1)/2` name comparisons: 500,500 at N=1,000 and 50,005,000 at N=10,000, before Mach-O writing. Intended behavior is to build a symbol-to-index map once after sorting, making resolution O(S+R). Consequence: relocation-heavy generated assembly has quadratic assembler cost at a compiler/tool boundary. Confidence: very high from the exact loop structure.

### A12-10 — Medium — successful symbol-cache population is quadratic in wide scopes

Source: every lookup allocates `visited` and a local map (`src/sema/symtab.rs:395-404`) and allocates `visit_key` before the persistent-cache check (`src/sema/symtab.rs:414-417`). Cache hits clone `CachedSymbolRef` and its string (`src/sema/symtab.rs:141-152`). More importantly, the first successful lookup calls `locate_symbol`, which pointer-scans the scope's entire `HashMap` (`src/sema/symtab.rs:155-196`).

```bash
for n in 10 100; do
  target/release/armfortas -ffree-form -O0 --emit-ir -o /dev/stdout <(
    {
      printf '%s\n' 'program wide_scope' 'implicit none'
      for i in $(seq 1 "$n"); do printf 'integer :: v%d\n' "$i"; done
      for i in $(seq 1 "$n"); do printf 'v%d = %d\n' "$i" "$i"; done
      for i in $(seq 1 "$n"); do printf 'print *, v%d\n' "$i"; done
      printf '%s\n' 'end program wide_scope'
    }
  ) | awk -v n="$n" \
    '/const_int/{c++} /call @afs_write_int/{w++}
     END{printf "names=%d ir_consts=%d write_uses=%d\n",n,c,w}'
done
```

Actual shapes were `10/20/10` and `100/200/100`. Looking up all N distinct symbols once necessarily sums their map iterator ranks, Θ(N²) pointer comparisons, then later cache hits still allocate and clone. Intended cache insertion already knows scope and canonical key and should be O(1), with an allocation-free hit path. Consequence: generated code with wide scopes pays quadratic cache-fill cost and allocator churn. Confidence: high.

### A12-11 — Medium — linear block lookup makes control-flow lowering quadratic

Source: `Function::block`, `block_mut`, and `try_block` linearly search `Function.blocks` at `src/ir/inst.rs:319-345`; every instruction emitted through `FuncBuilder` calls `block_mut(current_block)` at `src/ir/builder.rs:83-100`.

```bash
for n in 10 100; do
  target/release/armfortas -ffree-form -O0 --emit-ir -o /dev/stdout <(
    {
      printf '%s\n' 'program p' 'implicit none' 'integer :: x' 'x = 0'
      for i in $(seq 1 "$n"); do
        printf 'if (x == %d) then\n' "$i"
        printf '%s\n' 'x = x + 1' 'end if'
      done
      printf '%s\n' 'print *, x' 'end program p'
    }
  ) | awk -v n="$n" \
    '/^    [[:alnum:]_]+\(.*\):$/{b++}
     END{printf "ifs=%d ir_blocks=%d\n",n,b}'
done
```

Actual shapes were 10 IFs/21 blocks and 100 IFs/201 blocks. Instructions in successively later blocks rescan an increasingly long vector, so chained control flow incurs Θ(B²) block lookup even before later passes. Intended access by monotonic `BlockId` should be O(1), or the function should maintain an index. Generated state machines and large IF/SELECT chains compile superlinearly. Confidence: very high.

### A12-12 — Medium — release output can silently link the debug runtime

Source: build-tree discovery checks `target/debug/libarmfortas_rt.a` before release at `src/driver/mod.rs:2641-2645`; `fresh_runtime_lib` returns the first archive newer than runtime sources at `src/driver/mod.rs:2648-2658`. If neither is fresh, `src/driver/mod.rs:2615-2638` always runs a debug `cargo build`, and discovery again prefers debug.

```bash
cargo build -p armfortas-rt
cargo build --release -p armfortas -p armfortas-rt
target/release/armfortas -v -ffree-form \
  <(printf 'program p\nprint *, 1\nend program\n') \
  -o /tmp/a12-runtime-choice 2>&1 | rg 'libarmfortas_rt\.a'
```

Actual: with both archives fresh, the release compiler's verbose link line names `target/debug/libarmfortas_rt.a`; removing that incidental candidate changes the linked runtime. Intended behavior is a deterministic runtime/profile selection (or an explicit mandatory path), especially for a release benchmark. Consequence: binary size, runtime speed, link work, and benchmark baselines depend on unrelated build-tree history; concurrent stale-runtime compilations can also queue behind redundant Cargo builds. `AFS_RUNTIME_PATH` avoids the issue but is not set by `scripts/benchmark_gate.sh`. Confidence: high from candidate order and the targeted command.

### A12-13 — Medium/Low — temporary assembly lifecycle has two observable failures

Source: Mach-O `-c` returns at `src/driver/mod.rs:1969-1972` before cleanup at lines 1984-1986. Both ELF external-assembler failure (`src/driver/mod.rs:1907-1920`) and Mach assembler failure (`src/driver/mod.rs:1945-1963`) also return without removing the already-written file from line 1774.

On a native macOS/ARM64 host, successful compile-only jobs reproduce the leak:

```bash
d=$(mktemp -d)
TMPDIR="$d" target/release/armfortas -c test_programs/two_loops.f90 \
  -o "$d/two_loops.o"
find "$d" -name 'armfortas_*.s' -print
```

On ELF, the failure path is locally reproducible without a system-tool dependency:

```bash
d=$(mktemp -d)
TMPDIR="$d" AFS_AS_PATH=/bin/false target/release/armfortas \
  -ffree-form -c <(printf 'program p\nend program\n') -o /tmp/a12-fail.o || :
find "$d" -type f -printf '%f %s bytes\n'
```

Actual ELF result was a failed compile plus a retained 3,766-byte `armfortas_*.s`. The Mach success path retains one assembly file per distinct object output. Intended behavior is cleanup on every return after the temporary is created (or an explicitly documented keep-temp option). Large builds and failure matrices accumulate source-derived assembly in shared temporary storage and can exhaust it. Confidence: complete from local failure reproduction and the unconditional Mach early return. Severity is Medium for native compile-only accumulation and Low for failure-only leakage.

### A12-14 — Low — valid long output basenames make the temporary basename exceed `NAME_MAX`

Source: `src/driver/mod.rs:1753-1767` copies the complete output stem into `armfortas_<stem>_<16-hex>.s` without bounding or hashing the displayed stem.

```bash
d=$(mktemp -d)
name=$(printf 'x%.0s' {1..230}).o
(cd "$d" && /tmp/armfortas-audit/target/release/armfortas \
  -ffree-form -c <(printf 'program p\nend program\n') -o "$name")
```

Actual: the 232-byte output filename is valid, but compilation exits with `cannot write temp assembly: File name too long (os error 36)` because the derived temporary component is 259 bytes. Intended behavior is to accept any valid destination basename by truncating the decorative portion or using only the fixed hash. Consequence: generated build targets that the filesystem accepts cannot be compiled. Confidence: very high.

### A12-15 — Low — module artifacts encode raw input-path spelling

Source: `src/sema/amod.rs:241` writes `source_path` verbatim; the driver passes raw `opts.input` at `src/driver/mod.rs:1803-1806`. `.smod` repeats it at `src/driver/mod.rs:1849-1853`.

```bash
d=$(mktemp -d); mkdir -p "$d/rel" "$d/abs"
src=test_programs/multifile_basic_module.f90
target/debug/armfortas "$src" -O0 -c -J "$d/rel" -o "$d/rel/out.o"
target/debug/armfortas "$(realpath "$src")" -O0 -c \
  -J "$d/abs" -o "$d/abs/out.o"
sha256sum "$d"/{rel,abs}/out.o "$d"/{rel,abs}/m.amod
diff -u "$d"/{rel,abs}/m.amod
```

Actual: objects were identical, while `.amod` hashes differed solely in `# source:` (relative versus absolute spelling). Intended path-independent reproducibility needs normalized/remapped provenance or exclusion of this comment from artifact bytes. Relocated worktrees and invocation spelling invalidate content-addressed module caches despite identical source and interfaces. No semantic change was observed. Confidence: high.

## Unconfirmed concerns and policy-dependent behavior

- `src/preprocess/mod.rs:264-278,1525-1532` expands `__FILE__` from the raw input spelling and `__DATE__`/`__TIME__` from `SystemTime::now()`. No `SOURCE_DATE_EPOCH`, file-prefix-map, or macro-prefix-map support was found. This predictably makes programs using those compatibility macros path/clock-dependent, but whether deterministic overrides are part of armfortas's contract is not documented.
- Other unsorted candidate selection remains at `src/sema/resolve/core.rs:835-842` and `src/ir/lower/core.rs:15164-15182,16117-16130`. Six focused generic/operator fixtures stayed stable over 20 fresh processes at O0 and O2, so I did not promote these to confirmed output failures.
- When a multi-source command has no prebuilt artifact, `src/driver/mod.rs:2481-2486` links in topological compilation order, not original CLI order; adding any artifact switches to original order. Link-order-sensitive weak definitions or constructor ordering could therefore change, but I did not establish a valid Fortran reproducer.
- The single-file temporary name deliberately aliases concurrent compilations targeting the exact same output path (`src/driver/mod.rs:1727-1744`). That race may be acceptable because the destination itself aliases, but PID-only multi-file directories and module atomic-write names can also collide across PID namespaces sharing a `TMPDIR`; this environment was not available locally.

## Maintainability and allocation observations

- `afs-ld` safe ICF has a feature-conditional quadratic path: each atom is visited in a fixed-point loop (`afs-ld/src/icf.rs:118-145`), rebuilds the full input map (`afs-ld/src/icf.rs:395-399`), and filters the section's complete relocation vector (`afs-ld/src/icf.rs:376-400`). A section with A atoms and R relocations costs O(A×R+A×inputs) per round. I did not elevate this without an end-to-end ICF corpus measurement.
- Compact-unwind synthesis repeatedly searches layout atoms, relocation lists, atoms, and symbols per record (`afs-ld/src/synth/unwind.rs:232-324,328-386,426-502`; `afs-ld/src/layout.rs:310-329`). It deserves indexed large-unwind coverage.
- The default in-process ELF path writes the complete assembly string then rereads it while the original remains live (`src/driver/mod.rs:1774,1893-1900`). The x86 assembler clones laid section bytes (`afs-as/src/x86/assemble.rs:584-594`), and the ELF writer builds a full body then copies it into the final output (`afs-as/src/elf.rs:697-732,851-869`). These are avoidable whole-artifact copies, but no RSS ceiling was exceeded in the focused inputs.

## Benchmark and test-gate gaps

- Repository CI disables compile-time comparison in every benchmark invocation with `BENCH_SKIP_TIME=1` (`.github/workflows/ci.yml:85,295,352`). Only binary size is gated in CI.
- `scripts/benchmark_gate.sh:31-39,80-102` measures one full compile/link sample for five small O2 programs. It has no scale axis, per-phase/RSS/allocation gate, multi-file case, O1/O3 case, wide scope/CFG, live-across-many-calls function, assembler relocation stress, or linker archive/ICF stress.
- Missing benchmark programs are skipped (`scripts/benchmark_gate.sh:127-135`); comparison iterates only current results, so a deleted fixture's baseline row disappears silently. Missing environment baselines are created and pass (`scripts/benchmark_gate.sh:144-148`), and new rows without baselines are informational (`scripts/benchmark_gate.sh:155-160`).
- Existing compiler scaling tests permit quadratic or worse growth: `tests/compile_scaling_lsf.rs:92` and `tests/compile_scaling_defop.rs:64` permit approximately 4× on a 2× input, while `tests/compile_scaling_usechain.rs:80` permits 8×. `tests/compile_scaling_inline.rs:41` gives each caller one call and stops at IR, so it cannot expose the 33-sites-in-one-caller cutoff or backend costs.
- `afs-as/tests/perf_sanity.rs:159-175` allows `8× + 250 ms` when doubling 96 to 192 blocks, so the confirmed quadratic relocation lookup passes its ratio gate. The test is also native macOS/ARM64-only and does not exercise x86 local COMMON construction.
- `afs-ld/tests/perf_baseline.rs:208-240,243-288` enforces budgets only when optional `AFS_LD_*_BUDGET_MS` variables are present and skips without `xcrun`; no ELF ceiling, `-all_load`, ICF, or large-unwind case is present. No CI job was found that sets those budget variables.
- `tests/determinism_sweep.rs:1-9,49-109,134-204` checks assembly only, normally twice at O2, skips multi-file fixtures, and skips the whole corpus when native runtime support is unavailable. It cannot catch `.amod` or x86 object-model nondeterminism.
- `afs-as/src/elf.rs:1262-1266` serializes the same already-built model twice; it cannot detect nondeterminism introduced while building that model from a newly seeded `HashMap`. The bencch assembly/object reproducibility cases remain `xfail` at `bencch/suites/consistency/reproducibility.afs:3-40`.

## Negative validation summary

- Twelve fresh processes for each of IR, assembly, and object output on `two_loops.f90 -O2`: one hash per stage.
- Fresh-process O0/O2/O3 runs on path-neutral runtime/entry fixtures: stable IR, assembly, and objects.
- Relative, absolute, changed-CWD, copied, and renamed paths were stable for ordinary IR/assembly/object output when neither module provenance nor `__FILE__` participated.
- A small repeated ELF `afs-ld` link was byte-identical; both outputs had SHA-256 `d4877cb1ff7888839b93803c86b85759656332c89bb1ef552c2b207815dc0853` and exited zero.
- No unordered `HashMap`/`HashSet` iteration reaching ordinary compiler assembly was confirmed beyond the two specific `.amod`/local-COMMON boundaries above.
