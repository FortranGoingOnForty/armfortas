# Model-drift audit — 2026-07-06

Five hostile auditors ran in parallel over recent work (afs-ld ELF linker,
afs-as x86_64 assembler, armfortas core since 2026-06-01, real-project
capability sweep, cross-repo test integrity). Trigger: sessions may have
silently run on a different model than intended over the June–July window, so
recent work needed an independent correctness pass rather than a style review.
Every miscompile below was reproduced on built binaries before being recorded.
Host: dorado (FreeBSD 15 x86_64), HEAD `1fb154a`.

## Verdict

Real-project capability: **6 of 16 attempted projects behave correctly.**
gfortran builds and passes all 16 on the same trees — every failure is
armfortas-specific, not tree rot. Ten verified silent miscompiles on current
HEAD in the compiler alone, two demonstrated in afs-ld, four in afs-as. The
rot concentrates in the espadonne fpm/stdlib edge-fix wave (112 commits) and
the vectorizer/ABI seams; the mfwolffe-authored severity-queue work
(L-tail #96–#101), x86 regalloc/SSE2 legality, and driver routing held up
under attack. That split lines up with the model-drift hypothesis.

Common thread across the compiler findings: a `_ => {}` or `unwrap_or(0)`
dispatch gap on a type/rank/keyword, and fixes shaped like their reproducer
(the typed-char-constructor and fpm-spelling cases). None have fixture
coverage — which is why every suite is green while these ship.

## Severity-ranked findings

### Compiler core (armfortas)

- **C1 — reduction vectorizer drops elementwise stores at -O3/-Ofast.
  FIXED 2026-07-06 — first a conservative bail (36a51ad), then the full
  fusion (b113ba3).** A loop carrying both `c(i)=a(i)*b(i)` and
  `dot=dot+a(i)*b(i)` widened the IV to stride 4 but left the scalar store
  in the widened body → 3 of 4 stores lost, uninitialized output; the
  reduction stayed correct so nothing downstream noticed. `detect_reduction_plan`
  now collects the body's stores and classifies each with the existing
  elementwise machinery (`classify_body_op`); `apply_reduction_plan` widens
  them to VStores beside the vector reduction, reusing any load/product it
  already shares with the reduction. Every load and store must be
  index-`i`-aligned over the full trip, so widening in place preserves
  program order with no cross-lane or aliasing hazard; a store that doesn't
  fit bails the whole plan to scalar. Fixture
  `test_programs/vec_reduction_with_elementwise_store.f90` guards
  correctness at every opt level; `tests/vectorize_reduction_fused_store.rs`
  asserts both a VStore and a VReduceSum appear (fusion actually happens,
  not a scalar fallback). Analysis is shared with NEON → arm64 covered by
  the same code. Hole predated June (May f9f3610); x10 SSE `int_mul`
  (9cce9f6) made it reachable on x86.
- **C2 — BIND(C) aggregate ABI silently wrong both directions.
  LOUD-REJECTED 2026-07-06 (9b50a7e).** The SysV struct classifier
  (`src/codegen/x86/abi.rs`, 2443a79/180d615) is faithful to the psABI but
  has no producer — unit-test-only dead code. `src/codegen/x86/isel.rs:3513`
  never emits aggregates. Struct-by-VALUE goes as a pointer (callee read
  components as const 0); struct return reads a never-written buffer (both
  directions, all sizes, all targets — the by-pointer IR is
  target-independent). Interim: sema now rejects derived-type VALUE dummies
  and derived-type BIND(C) results, mirroring the char-VALUE rejection;
  `type(c_ptr)`/`type(c_funptr)` exempt (ABI-scalar). Full aggregate
  calling convention (wire the classifier into isel + returns) remains
  scheduled. Residual: an interface-only external C function taking/returning
  a struct by value has no Fortran definition to reject — caller-side ABI
  still unhandled. Fixtures `test_programs/bind_c_derived_type_{value,result}_rejected.f90`,
  `bind_c_cptr_value_and_result_ok.f90`.
- **C3 — strided component views ignore stride in whole-array I/O.**
  `associate(ids => a%id)` prints the sibling component's bit patterns
  interleaved; `sum(ids)` is correct so the descriptor is right and I/O
  ignores the stride. June component-projection work (c585e5e, 70a5e6b).
- **C4 — elemental defined operators broadcast a scalar dispatch on array
  operands.** `arr = arr + one` → `11 11 11` not `11 12 13`; through a
  component it's opt-level-dependent garbage. `defined_binary_operator_result_rank`
  (4b3c7d2, `unwrap_or(0)` on actual ranks).
- **C5 — FINDLOC ignores MASK= and BACK=.** a938b53. Silently returns the
  unmasked/forward result.
- **C6 — untyped char array constructor with an array element corrupts.**
  Fix (c6e094a) is gated to the exact `[character(len=n) :: ...]` spelling fpm
  emits; the plain F2008 `c = [c, 'cc']` flattens into wrong-size slots.
  `src/ir/lower/core.rs:42621`.
- **C7 — `real(16)`/`complex(16)` silently compile as single precision.**
  `src/ir/types.rs:170` (`8 => F64, _ => F32`); `kind` reports 4. A
  `complex(16)` result additionally corrupts the stack (ComplexBuffer sizing
  "falls back to sp") → correct-looking single-precision output then SIGSEGV
  at exit. Pre-June; should be a loud unsupported-kind error.
- **C8 — implied-do list-directed output prints a blank record.**
  `write(*,*) (ia(i),i=1,3)` and `print *, (…)` drop all values;
  `parse_print` (`src/parser/stmt.rs:876`) uses a plain expr loop, not
  `parse_io_expr_list`. Pre-June; the most basic F77 output idiom.
- **C9 — C_F_POINTER with a component FPTR and non-constant SHAPE mis-sets
  dims.** `src/ir/lower/intrinsic_sub.rs:796` (d1a8a12) recovers rank only for
  a bare-name FPTR. `size(h%p)=1` after a rank-1 bind.
- **C10 — rank-2 deferred-length char element comparisons read as length 0.**
  `g(1,1)=='aa'` false after assignment; substring read works. rank==1-shaped
  guard in the June deferred-char cluster (153ea67).
- **C11 — CLASS mold=/source= loses dynamic type / zeroes extension
  components.** `allocate(dst,mold=child_poly)` allocates the declared type;
  scalar `source=` copies only the base size. 7a05e24/567ed0e.
- **C12 — internal WRITE deferred-length: reallocation regressed to silent
  truncation, expectations rewritten in the same commit.** 426a29d changed
  allocated deferred-length targets from F2023-conforming realloc (PR #64,
  2dc803a) to fixed-length truncation and rewrote the fixture CHECK lines to
  match; no noted_items entry; `.docs/audits/f2023-feature-matrix.md:86` still
  claims realloc. Truncates even under `--std=f2023` (gfortran errors). One
  auditor read the standard as forbidding the old realloc; adjudicate against
  F2023 12.6.4.5.1 in `.docs/refs/` before fixing — but silent truncation is
  the one option the policy forbids either way.

### afs-ld

L1+L2 **FIXED 2026-07-06 (afs-ld PR #7, pin b46543a)**: extracted one
`resolve_globals` used by both `link_static_exec` and `link_dynamic_exec`
— strong beats weak regardless of order, duplicate strong is an error,
version aliasing applies default (`@@`) versions first then by link order
(never HashMap order). Dynamic path also gained version aliasing + the
COMMON hard error it lacked. Unit tests incl. a 64-run determinism check.

- **L1 — no weak/strong resolution in the dynamic path; first def wins.**
  `src/elf.rs:1629` (`globals.entry(name).or_insert(...)`). Weak `foo=7`
  before strong `foo=42` → exits 7; static path and system ld exit 42. Also
  silently accepts duplicate strong defs the static path rejects.
- **L2 — version-alias base resolution iterates a HashMap (nondeterministic
  bytes).** `src/elf.rs:906`. Two non-default `@`-versions of one base → 12
  identical links gave exit `1 2 2 2 1…` and two checksums. glibc compat
  symbols (`sys_errlist@GLIBC_2.x`) are exactly this shape → the Linux leg
  will hit it. Violates the byte-determinism gate.
- **L3 — command-line library order permuted.** `src/main.rs:258,303`: all
  positional archives queue before all `-l` regardless of interleaving.
  `pmain.o -L. -lb ./liba.a` → afs-ld picks liba, GNU ld picks libb.
- **L4 — dynamic-path `R_X86_64_32/32S` truncate silently** (`src/elf.rs:2102`);
  static path checks range (`:1311`). Copy the checks.
- **L5 — `parse_shared` misclassifies `STT_GNU_IFUNC` (=10) as data.**
  `src/elf.rs:510` (`func: typ == STT_FUNC`). FreeBSD libc exports 28 IFUNCs
  (`stpcpy`, `strrchr`…) → calling them misdiagnoses as "needs a COPY
  relocation". **Blocks 3f**; ~2-line fix.
- **L6 — no canonical-PLT st_value: `&func` differs across the .so boundary.**
  `src/elf.rs:1820`. Non-PIE contract sets the UNDEF entry's st_value to the
  PLT stub; kept 0. C function-pointer identity silently breaks.
- **L7 — GOTPCREL against a linker-defined symbol panics.**
  `_end@GOTPCREL(%rip)` → index-out-of-bounds at `src/elf.rs:925`
  (`objects[LINKER_MARK]`). Static PIC allocator code does this.
- **L8 — parsers panic instead of diagnosing on malformed/truncated input;**
  two silent fallbacks (`entsize==0 → 0 symbols/relocations`, `sent=24`).
  `src/elf.rs:236,307,328,465`.
- **L9 — static ET_EXEC emits no PT_GNU_STACK** (READ_IMPLIES_EXEC → exec
  stack on Linux) and brands **EI_OSABI by compile host, not target**
  (`src/elf.rs:1434`), breaking cross-linking.

### afs-as

A1-A4 all **FIXED 2026-07-06 (afs-as PR #11, one commit)**: each assembled
a valid-looking line into wrong bytes silently. New tests —
`tests/x86_encode_rejects.rs` (A2-A4 error and gas rejects the same forms)
and two RIP+imm8 cases in `tests/x86_encode_differential.rs` (A1 addend).

- **A1 — SSE imm8 + RIP memory: PC32 addend off by one.** imm8 pushed after
  the addend is computed. `pshufd $3,tbl(%rip),%xmm0` → gas `tbl-5`, afs
  `tbl-4`; links, reads one byte past the constant. `src/x86/encode.rs:616`.
  Backend emits reg,reg only today; assembler accepts the mem form.
  Fix: route the trailing immediate through the tail before `finish()` so
  the existing trailing-byte addend accounting counts it.
- **A2 — `movhlps` with a memory operand silently assembles as `movlps`**
  (different instruction). The guard comment claiming rejection is dead code;
  the `SSE_RM` lookup wins first. `src/x86/encode.rs:517,596`. Fix: move the
  guard ahead of the `SSE_RM` lookup.
- **A3 — displacements outside i32 silently truncate.**
  `movq 4294967296(%rax),%rbx` → `movq 0(%rax)`. `src/x86/encode.rs:165`.
  Fix: reject (`i32::try_from`) after the RIP early-return, matching gas.
- **A4 — `%ch/%dh/%bh` accepted as a shift count, encoded as `%cl`.**
  `src/x86/encode.rs:1124` (no register-class check). Fix: require the
  count to be a low GP register (`class == RegClass::Gp`).
- **A5 — panic-instead-of-diagnostic:** `.ascii/.asciz/.zero` before any
  section (`assemble.rs:217`); PC-rel patch into a NOBITS section
  (`assemble.rs:627`).
- **A6 — nonzero `.bss` data silently dropped** (gas errors);
  **`.p2align N,,M` max-skip ignored** (gcc emits this; layout diverges);
  **octal `\012` escapes corrupted** (latent — backend emits `.byte` lists).
  `test $imm,%al` uses the long form not A8/A9 (byte-parity claim broken;
  the one differential case dodges it).

### Test integrity (ours)

- **T1 — FIXED 2026-07-06 (afs-ld PR #8, pin 44ba4af): implemented, not
  loud-rejected.** `.eh_frame` is now retained (SHT_X86_64_UNWIND added to
  kept types) and `--eh-frame-hdr` synthesizes `.eh_frame_hdr` +
  PT_GNU_EH_FRAME in both static and dynamic paths (parse_eh_frame_fdes +
  build_eh_frame_hdr; pcrel|sdata4 only, else hard error). Generation
  gated on the flag to match GNU ld; retention unconditional. Verified on
  FreeBSD (static CFI binary + dynamic-hello both carry a byte-correct
  header and run). Unit tests + an integration test that the flag emits a
  well-formed header pointing at `.eh_frame` and its absence emits none.
- **T1 (original finding) — `--eh-frame-hdr` moved to silent-accept AND its
  rejection test re-pointed to `-pie` in the same undisclosed commit** (afs-ld 2cb05d7).
  afs-ld emits no eh_frame_hdr; the driver passes the flag for Rust-runtime
  unwinding (`src/driver/elf_crt.rs:227`) → under `AFS_LD=1`, binaries
  silently lack the unwind header. The one genuinely buried test retirement
  found across four repos.
- **T2 — the arm64 -O2+ default-init miscompile now has zero failing
  coverage.** ffb7d0a and 5e250a4 both reworked the fixtures that exposed it;
  no `XFAIL(arm64)` replaced them. Structural cause: the XFAIL grammar has no
  opt-level qualifier, so a plain XFAIL panics as "unexpectedly passed" at
  -O0/-O1 — which channels developers into fixture-softening.
  **GRAMMAR FIXED 2026-07-06 (99cdc2d)**: opt-level selectors (`O2` exact,
  `O2+` rank-and-above; target ∧ opt conjunction) + unit tests.
  **Fixture reinstatement pending an arm64 run** (nomad unreachable
  2026-07-06): repro written and x86-validated, but committing an
  unvalidated `XFAIL(arm64,O2+)` risks an XPASS that breaks the macOS gate,
  so it is held until arm64 confirms the minimal form triggers the bug —
  source + procedure in `noted_items.md`.
- **T3 — silent-degrade stubs added since June:** `.amod` rank/dims
  `unwrap_or(0/1)` (`src/sema/amod.rs:1781`), C_F_POINTER rank `unwrap_or(0)`
  (`intrinsic_sub.rs:801`), "emit zero as a safe fallback" ELF global
  (`src/codegen/shared.rs:293`), afs-ld `entsize==0 → 0` (`elf.rs:307`).
  `coerce_to_type`'s `Ptr→Float => 0.0` arm (`helpers.rs:198`) survived the
  loud-fallback hardening (6ec427b) that panicked the arm right below it.
- **T4 — bencch drift:** 31 stale XPASS marks, one Mach-O-asserting asm case
  on an ELF host; nothing in CI runs bencch so the drift is silent. bencch
  itself is honest (hard-errors on zero matched cases, no skip concept).

## Real-project capability (sweep)

| project | build | compile | behavior | verdict |
|---|---|---|---|---|
| fortbite | make | OK | 81/81 | PASS |
| fit | make/fpm | OK | 51/51 | PASS |
| fortress | fpm | OK | 0 fails | PASS |
| fgof-termios | fpm | OK | 0 fails | PASS |
| fgof-lineedit | fpm | OK | 0 fails | PASS |
| toml-f (cand) | fpm | OK | full suite + toml2json | PASS |
| test-drive (cand) | fpm | OK | 0 fails | PASS |
| fuss | make | OK | CLI ok, TUI not run | PASS (partial) |
| ferp | make | OK | `[0-9]` regex miscompiles | **FAIL (silent)** |
| fgof-fs/process/pty/watch/temp/cache/state | fpm | OK | SIGSEGV | **FAIL (runtime)** |
| sniffert | make | stalls treemap_layout.f90 | – | **FAIL (compile-time)** |
| facsimile | make | stalls editor_state_module.f90 | – | **FAIL (compile-time)** |
| fortty | cmake | not attempted (GLFW/GL/FreeType) | – | SKIP |

Three real-world bug classes, each with a reproducer in `/tmp/afsproj-sweep2/`:
- **memmove −1 length → SIGSEGV** on derived-type-with-allocatable-char
  assignment (`options = clear_temp_options()`); fault addr
  `0xffffffffffffffff`. Crashes 7 fgof libraries. **Highest leverage — one
  fix likely flips 7 projects.** Same family as compiler findings C10/C12.
- **digit-range regex miscompile** (ferp `[0-9]`); `[a-z]/[A-Z]` fine.
- **superlinear compile time**: `treemap_layout.f90` (498 ln, >2 min -O2),
  `facsimile/editor_state_module.f90` (1420 ln, >120 s -O0). gfortran <1 s each.

Harness caveats: C-PIC gate (cc C objects non-PIC vs our PIE link;
`--c-flag -fPIC` fixes it — driver gap, not codegen); `bencch projects run`
is non-functional on dorado (pass verdict requires flang, absent) — builds
were driven manually; FreeBSD `make` needs `gmake` for GNU `filter-out`.

## Claimed vs observed

| claim | observed 2026-07-06 | verdict |
|---|---|---|
| stdlib "library builds 100%, 1288/0" (976de26) | build fails ~52% on `sort_coo` generic (9bcecbd open item); 0 tests run. gfortran control 1289/0 | **REGRESSED** |
| fpm "stage0–3 fixed point" | stage0 holds — armfortas builds a working fpm (`fpm run` → `FPM_DEMO_OK 42`); stage1-3 not re-run | stage0 confirmed |
| lib 1301/0 | 1301/0 (one perf flake under full-load parallel) | match |
| run_programs 120/120 | 120/120, 648/650×6 opt levels | match |
| gas corpus "byte-identical" | 1656 objects, 0 divergences (modulo symmetric NOP-fill) | match |
| afs-ld "9/9 green" | 10 binaries green on dorado; hasu not re-run | match |

## Held up under attack (verified clean)

L-tail #96–#101 (arity table checked bound-by-bound vs F2023 16.9; SPLIT POS;
SYSTEM_CLOCK kind keying; one-record internal-I/O rule). x86 regalloc + SSE2
legality (adversarial FP-across-call kernels, zero post-SSE2 instructions in
84 -Ofast programs, pmulld/pminsd/pcmpgtq correctly refused). afs-as REX/
ModRM/SIB corner matrix (544 forms byte-identical to gas), immediate
selection, branch relaxation. afs-ld hash table, dynsym index consistency,
all PLT/GOT/verneed encodings, TLS variant-II math, archive fixpoint, layout
congruence, determinism (except L2). XFAIL hygiene, skip-budget ledgers, the
gas differential methodology. Full assertion-weakening sweep clean except T1.

## Recommended fix ladder

1. **memmove −1 length** (flips 7 fgof libs) — likely C10/C12 family.
2. **stdlib regression** — `sort_coo` generic resolution (9bcecbd); restores a
   claimed-green headline state.
3. **C1 reduction vectorizer** (silent bad output at -O3, arm64 too) +
   **C2 BIND(C) aggregate ABI** (FFI corruption, no tripwire).
4. **afs-as A1–A4** (silent-wrong encodings; fix even where the backend
   doesn't emit the form yet — they're in the accepted surface).
5. **afs-ld L1/L2 + L5** — fold L5 (IFUNC) into 3f's opening; L1/L2 before the
   Linux dynamic leg. Add differential tests: weak/strong mixes, duplicate
   defs across archives, interleaved `-l`/positional, multi-`@`-version.
6. **T1 eh_frame_hdr** — either implement or loud-reject, restore the test.
7. **T2 XFAIL opt-level grammar** — unblocks holding the arm64 miscompile open
   without softening a fixture; then reinstate the arm64 fixture as XFAIL.
8. Remaining C3–C11, L3/L4/L6–L9, A5/A6, T3/T4 as scheduled.

C7 (`real(16)` → single) and C8 (blank implied-do record) are pre-June and
independent — loud-reject C7, fix C8's parser path.
