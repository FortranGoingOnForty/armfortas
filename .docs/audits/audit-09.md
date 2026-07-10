# Audit 09: pinned `afs-as` assembler review

Reviewed submodule commit `fac26fb9c1c4064b9bf838e393fc1d7363ff3409` (`v0.1.0-37-gfac26fb`). The review covered the ARM64 lexer/parser/encoder and expression/fixup pipeline, the x86 parser/encoder/relaxer, Mach-O and ELF model construction/writers, relocation handling, diagnostics, determinism, scaling, and the differential-test policy. I used the existing focused debug binary plus tiny stdin probes against local clang 22.1.8 / LLVM tools and GNU binutils 2.46.1; I did not run the workspace test suite.

Commands below assume:

```sh
cd /tmp/armfortas-audit
CARGO_TARGET_DIR=/tmp/afs-as-audit-target cargo build -q -p afs-as --bin afs-as
export AFS_AS=/tmp/afs-as-audit-target/debug/afs-as
```

## Confirmed discrepancies

| ID | Severity | Summary |
|---|---|---|
| A09-01 | High | Valid ARM64 `sp` register arithmetic is encoded as `xzr` arithmetic |
| A09-02 | High | ARM64 immediates are narrowed before range/shape validation |
| A09-03 | Medium | ARM64 register-width mismatches are silently rewritten |
| A09-04 | Medium | ARM64 trailing tokens become extra statements |
| A09-05 | Medium | ARM64 data/directive operands wrap before validation |
| A09-06 | Medium | A compact valid expression can abort the process with stack overflow |
| A09-07 | High | x86 `testq` silently truncates an unencodable immediate to zero |
| A09-08 | High | x86 `ret $imm16` silently becomes plain `ret` |
| A09-09 | High | x86 relaxation binds jumps to a same-file weak definition |
| A09-10 | Medium | x86 `.p2align` discards an explicit fill byte |
| A09-11 | Medium | x86 `.comm` uses the wrong default ELF alignment |
| A09-12 | Medium | A defined-label/COMMON collision emits an ambiguous, mis-relocated ELF object |
| A09-13 | Medium | Global `.L*` symbols can panic x86 assembly when referenced |
| A09-14 | Medium | x86 undefined symbol declarations lose ELF metadata |
| A09-15 | Medium | Executable-stack section metadata is accepted and then cleared |
| A09-16 | Medium | ELF object bytes are nondeterministic for multiple local commons |
| A09-17 | Medium | Mach-O relocation symbol resolution is quadratic |
| A09-18 | Medium | Tiny x86 virtual-BSS input can overflow layout arithmetic and panic |
| A09-19 | Low | x86 encoder failures do not meet the documented diagnostic contract |

### A09-01 — valid ARM64 `sp` register arithmetic is encoded as `xzr` arithmetic

- **Severity:** High
- **Source:** `/tmp/armfortas-audit/afs-as/src/parse.rs:1881-1915`, especially the no-modifier selection at `:1918-1969`; `/tmp/armfortas-audit/afs-as/src/parse.rs:4981-4992`; `/tmp/armfortas-audit/afs-as/src/encode.rs:1638` and `:2887-2905`.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' 'add x0, sp, x1' | "$AFS_AS" - -o /tmp/a09-sp.o
  llvm-objdump --macho --disassemble /tmp/a09-sp.o
  printf '%s\n' 'add x0, sp, x1' |
    clang -cc1as -triple arm64-apple-macosx -filetype asm -show-encoding -o - -
  ```
- **Actual:** `afs-as` emits bytes `e0 03 01 8b`, which LLVM disassembles as `add x0, xzr, x1`.
- **Intended:** The valid source form must use the extended-register encoding. Clang emits `e0 63 21 8b`, which preserves `sp`. A destination such as `add sp, x1, x2` has the analogous problem: the selected shifted-register form interprets register 31 as `xzr`, discarding the result.
- **Consequence:** Valid stack-pointer arithmetic can be silently changed into zero-register arithmetic, corrupting stack/frame behavior.
- **Confidence:** High; reproduced and independently decoded by LLVM.

### A09-02 — ARM64 immediates are narrowed before range/shape validation

- **Severity:** High
- **Source:** Representative premature casts are `/tmp/armfortas-audit/afs-as/src/parse.rs:1435-1461` (`svc`/`brk`), `:1928-1959` (add/sub), `:2627-2677` (move-wide/shift/bitfield), and `:2704-2883` (branch/ADR). Trusted field insertion is in `/tmp/armfortas-audit/afs-as/src/encode.rs:1855-1867`, `:1907-1949`, `:2879`, `:2932-2939`, and `:2962-2969`.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' 'add x0, x1, #8192' | "$AFS_AS" - -o /tmp/a09-add.o
  llvm-objdump --macho --disassemble /tmp/a09-add.o
  printf '%s\n' 'add x0, x1, #8192' |
    clang -cc1as -triple arm64-apple-macosx -filetype asm -show-encoding -o - -

  for s in \
    'lsl x0, x1, #64' \
    'lsl x0, x1, #256' \
    'movz x0, #65536' \
    'ldur x0, [x1, #65536]' \
    'b #4294967296' \
    'svc #65536'
  do
    printf '.text\n%s\n' "$s" | "$AFS_AS" - -o /tmp/a09-imm.o
    printf 'status=%s source=%s\n' "$?" "$s"
  done
  ```
- **Actual:** The valid `add #8192` produces word `0x91800020`, which LLVM reports as an invalid instruction; the unchecked 13th immediate bit spills into fixed opcode bits. Clang emits `20 08 40 91` (`#2, lsl #12`). `lsl #64` panics at `encode.rs:1858` (exit 101), while the other out-of-range examples succeed after modulo narrowing: `lsl #256` becomes shift 0, `movz #65536` becomes `movz #0`, the `ldur` offset becomes 0, the large branch becomes `b #0`, and `svc #65536` becomes `svc #0`.
- **Intended:** `#8192` should be represented with the shifted immediate form. Values outside an instruction's representable range/alignment must produce a located error and exit 1; clang rejects every out-of-range case.
- **Consequence:** Both valid and invalid source can generate wrong or invalid machine words, and some source-level failures abort the compiler process.
- **Confidence:** High; reproduced across several independent instruction families and compared with LLVM MC.

### A09-03 — ARM64 register-width mismatches are silently rewritten

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/parse.rs:1764-1805` and `:1918-1964` use the destination width while discarding source-width booleans. Scalar FP does the same at `:4063-4068`, `:4580-4598`, and `:4606-4630`. The same pattern also occurs in logical, multiply/divide, conditional-select, and register-move parsers around `:2120-2489`.
- **Reproduction:**

  ```sh
  for s in \
    'add x0, w1, w2' \
    'mul x0, w1, w2' \
    'csel x0, w1, w2, eq' \
    'mov x0, w1' \
    'fadd d0, s1, s2'
  do
    printf '.text\n%s\n' "$s" | "$AFS_AS" - -o /dev/null
    printf 'afs=%s %s\n' "$?" "$s"
    printf '%s\n' "$s" |
      clang -cc1as -triple arm64-apple-macosx -filetype asm -o /dev/null -
    printf 'clang=%s\n' "$?"
  done
  ```
- **Actual:** `afs-as` exits 0 for every input. For example, `add x0,w1,w2` emits `20 00 02 8b`, exactly `add x0,x1,x2`.
- **Intended:** Operand widths in these instruction forms must agree; clang rejects every input.
- **Consequence:** A source typo is silently converted into a different-width instruction instead of receiving the explicit unsupported/invalid-form diagnostic promised by the standalone contract.
- **Confidence:** High; all listed cases were reproduced against clang.

### A09-04 — ARM64 trailing tokens become extra statements

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/parse.rs:442-449` calls `parse_line` again without first requiring a newline; `:452-522` may return after consuming only one statement.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' 'nop ret' | "$AFS_AS" - -o /tmp/a09-trailing.o
  llvm-objdump --macho --disassemble /tmp/a09-trailing.o
  printf '%s\n' 'nop ret' |
    clang -cc1as -triple arm64-apple-macosx -filetype asm -o /dev/null -
  ```
- **Actual:** `afs-as` succeeds and emits two instructions, bytes `1f2003d5 c0035fd6` (`nop; ret`).
- **Intended:** `ret` is invalid trailing syntax/an operand to `nop` without a statement separator; clang rejects the line.
- **Consequence:** Missing separators can insert executable instructions rather than producing a diagnostic.
- **Confidence:** High; reproduced.

### A09-05 — ARM64 data/directive operands wrap before validation

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/parse.rs:554-567` casts `.comm` size/alignment, and `:647-658` casts `.fill` operands, before representability checks. `/tmp/armfortas-audit/afs-as/src/assemble.rs:1173-1197` sees only the already-truncated fill width; common size reaches the output symbol at `:2186-2212`.
- **Reproduction:**

  ```sh
  printf '%s\n' '.comm _x,-1,0' | "$AFS_AS" - -o /tmp/a09-negcomm.o
  llvm-nm -m /tmp/a09-negcomm.o
  printf '%s\n' '.comm _x,-1,0' |
    clang -target arm64-apple-macos -c -x assembler -o /dev/null -

  printf '%s\n' '.data' '.byte 17' '.fill 1,256,170' '.byte 34' |
    "$AFS_AS" - -o /tmp/a09-fill.o
  llvm-objdump --macho --section=__data --full-contents /tmp/a09-fill.o
  printf '%s\n' '.data' '.byte 17' '.fill 1,256,170' '.byte 34' |
    clang -target arm64-apple-macos -c -x assembler -o /tmp/a09-fill-ref.o -
  llvm-objdump --macho --section=__data --full-contents /tmp/a09-fill-ref.o
  ```
- **Actual:** Negative `.comm` succeeds and emits common `_x` with value/size `0xffffffffffffffff`; clang diagnoses “size must be non-negative.” The `.fill` width 256 wraps to `u8` zero, so `afs-as` emits only `11 22`. Apple's assembler warns that widths over 8 are truncated to 8 and emits `11 aa 00 00 00 00 00 00 00 22`.
- **Intended:** Signed sizes must be validated before conversion. `.fill` must either follow Apple truncation semantics or reject the unsupported width explicitly, never silently emit zero bytes.
- **Consequence:** Supported directives can create nonsensical common symbols or silently shift all following data/symbol offsets.
- **Confidence:** High; both variants reproduced against clang's Mach-O assembler.

### A09-06 — a compact valid expression can abort the process with stack overflow

- **Severity:** Medium
- **Source:** Recursive unary parsing at `/tmp/armfortas-audit/afs-as/src/parse.rs:917-943`, especially self-recursion at `:937-940`; expression evaluation/classification is also recursive at `/tmp/armfortas-audit/afs-as/src/expr.rs:89-109` and `:259-303`.
- **Reproduction:**

  ```sh
  ulimit -c 0
  awk 'BEGIN { printf ".data\n.quad "; for(i=0;i<100000;i++)printf "-"; print "1" }' |
    "$AFS_AS" - -o /dev/null
  printf 'status=%s\n' "$?"
  ```
- **Actual:** The main thread reports `has overflowed its stack`, the Rust runtime aborts, and the pipeline exits 134. The same even-negation expression with 20,000 `-` tokens succeeds.
- **Intended:** This expression is valid under the parser's own unary grammar and evaluates to 1. It should assemble, or a deliberate resource/depth limit should return an ordinary located error rather than aborting.
- **Consequence:** Roughly 100 KiB of source can terminate a compiler process, bypassing the documented exit-1 diagnostic path.
- **Confidence:** High; reproduced with core dumps disabled.

### A09-07 — x86 `testq` silently truncates an unencodable immediate to zero

- **Severity:** High
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/encode.rs:1040-1070`, specifically `:1062-1066`, casts Q-width immediates to `i32` without the range check used by arithmetic instructions at `:1029-1030`.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' 'testq $4294967296, %rax' |
    "$AFS_AS" --64 - -o /tmp/a09-test-afs.o
  objdump -dr /tmp/a09-test-afs.o
  printf '%s\n' '.text' 'testq $4294967296, %rax' | as --64 -o /dev/null
  ```
- **Actual:** `afs-as` succeeds and emits `48 a9 00 00 00 00`, disassembled as `test $0x0,%rax`. GNU `as` exits 1 with `operand type mismatch for 'test'`.
- **Intended:** x86-64 `test r/m64,imm32` can represent only an imm32 field with architectural sign extension; a non-equivalent 64-bit value must be rejected.
- **Consequence:** Condition flags are completely wrong (the emitted test always sets ZF), so a compiler-generated mask above bit 31 can reverse control flow.
- **Confidence:** High; reproduced and disassembled.

### A09-08 — x86 `ret $imm16` silently becomes plain `ret`

- **Severity:** High
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/encode.rs:320-350`; the `ret|retq` arm at `:321-327` returns opcode `c3` without checking `ops`. `cqto`, `cltd`, `nop`, and `syscall` have the same operand-count defect; push/pop and setcc use only `ops.first()` at `:365-373` and `:823-832`.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' 'ret $8' | "$AFS_AS" --64 - -o /tmp/a09-ret-afs.o
  printf '%s\n' '.text' 'ret $8' | as --64 -o /tmp/a09-ret-gas.o
  objdump -dr /tmp/a09-ret-afs.o
  objdump -dr /tmp/a09-ret-gas.o
  ```
- **Actual:** `afs-as` emits `c3` (`ret`); GNU `as` emits `c2 08 00` (`ret $0x8`).
- **Intended:** If `ret imm16` is outside the intentionally supported subset, it must fail explicitly; it must not erase the operand and encode a different instruction.
- **Consequence:** The callee fails to pop the requested stack bytes, causing stack and return-path corruption.
- **Confidence:** High; reproduced byte-for-byte.

### A09-09 — x86 relaxation binds jumps to a same-file weak definition

- **Severity:** High
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:300-315` turns every symbolic jump into `Item::Branch`; `:366-371` and `:479-506` decide locality solely from label presence, ignoring `.weak`. This contradicts the weak/preemptible relocation policy implemented later at `:714-730`.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' '.weak foo' 'foo:' 'ret' '.globl caller' 'caller:' 'jmp foo' |
    "$AFS_AS" --64 - -o /tmp/a09-weak-afs.o
  printf '%s\n' '.text' '.weak foo' 'foo:' 'ret' '.globl caller' 'caller:' 'jmp foo' |
    as --64 -o /tmp/a09-weak-gas.o
  readelf -rW /tmp/a09-weak-afs.o
  readelf -rW /tmp/a09-weak-gas.o
  objdump -dr /tmp/a09-weak-afs.o
  objdump -dr /tmp/a09-weak-gas.o
  ```
- **Actual:** `afs-as` emits a resolved short jump `eb fd` and no relocation. GNU `as` emits a long jump with `R_X86_64_PLT32 foo - 4`.
- **Intended:** A weak definition is replaceable at link/load time, so the branch must retain the symbol relocation as GNU `as` does.
- **Consequence:** Interposition/override of a weak function is defeated for tail jumps assembled by `afs-as`.
- **Confidence:** High; reproduced. Ordinary same-section non-weak globals were checked separately and correctly excluded from this finding because current GNU `as` also resolves those directly.

### A09-10 — x86 `.p2align` discards an explicit fill byte

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/parse.rs:508-527` parses the fill into `_fill` and drops it; `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:173-180` stores no fill, and `:529-539` always emits NOPs in text or zeros elsewhere.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' '.byte 0' '.p2align 2,0xcc' 'ret' |
    "$AFS_AS" --64 - -o /tmp/a09-p2-afs.o
  printf '%s\n' '.text' '.byte 0' '.p2align 2,0xcc' 'ret' |
    as --64 -o /tmp/a09-p2-gas.o
  objdump -s -j .text /tmp/a09-p2-afs.o
  objdump -s -j .text /tmp/a09-p2-gas.o
  ```
- **Actual:** `afs-as` emits `00 0f 1f 00 c3`; GNU `as` emits the requested `00 cc cc cc c3`.
- **Intended:** Preserve the explicit fill byte, or reject a nonempty fill as unsupported.
- **Consequence:** Requested trap/pattern padding silently becomes executable NOP padding, and raw section bytes diverge.
- **Confidence:** High; reproduced.

### A09-11 — x86 `.comm` uses the wrong default ELF alignment

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/parse.rs:564-579`, especially hardcoded `None => 8` at `:571-572`; `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:697-711` copies it into `st_value` for `SHN_COMMON`.
- **Reproduction:**

  ```sh
  printf '%s\n' '.comm x,1' '.comm y,32' | "$AFS_AS" --64 - -o /tmp/a09-comm-afs.o
  printf '%s\n' '.comm x,1' '.comm y,32' | as --64 -o /tmp/a09-comm-gas.o
  readelf -sW /tmp/a09-comm-afs.o | rg ' (x|y)$'
  readelf -sW /tmp/a09-comm-gas.o | rg ' (x|y)$'
  ```
- **Actual:** `afs-as` gives both common symbols alignment/value 8. GNU ELF semantics choose 1 for size 1 and 16 for size 32 (largest suitable power of two, capped at 16).
- **Intended:** Match GNU `as`'s default when the third operand is omitted, or require an explicit alignment rather than inventing a different default.
- **Consequence:** Common storage can be under-aligned (breaking 16-byte objects/SSE assumptions) or unnecessarily over-aligned.
- **Confidence:** High; source behavior and reference output agree.

### A09-12 — a defined-label/COMMON collision emits an ambiguous, mis-relocated ELF object

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:169-171` records `.comm` without collision checks. Defined labels insert `model_sym_index` at `:632-681`; commons append a second symbol and overwrite that entry at `:697-711`; global relocations use the overwritten entry at `:726-730` and `:778`.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' '.globl foo' '.type foo,@function' 'foo: ret' \
    '.comm foo,8,8' 'caller: call foo' |
    "$AFS_AS" --64 - -o /tmp/a09-collision.o
  readelf -sW /tmp/a09-collision.o | rg ' foo$'
  readelf -rW /tmp/a09-collision.o

  printf '%s\n' '.text' '.globl foo' '.type foo,@function' 'foo: ret' \
    '.comm foo,8,8' 'caller: call foo' | as --64 -o /dev/null
  ```
- **Actual:** `afs-as` succeeds with two global `foo` symbols: one section-defined `FUNC` and one `COMMON OBJECT`. The call relocation's symbol index is the later COMMON entry. GNU `as` rejects redefinition of `foo` as common.
- **Intended:** Diagnose the symbol-class collision and emit no object.
- **Consequence:** The emitted ELF symbol table is ambiguous and a call relocation can target storage rather than the function definition.
- **Confidence:** High; follows the observed duplicate indices and was reproduced with `readelf`.

### A09-13 — global `.L*` symbols can panic x86 assembly when referenced

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:625-637` unconditionally omits `.L*` labels from the symbol table even when `.globl`/`.weak`; `:726-730` classifies the target as nonlocal, then `:778` indexes the missing map entry.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' '.globl .Lfoo' 'caller: call .Lfoo' '.Lfoo: ret' |
    "$AFS_AS" --64 - -o /dev/null
  printf 'status=%s\n' "$?"
  ```
- **Actual:** The debug binary panics at `x86/assemble.rs:778:56` with `no entry found for key` and exits 101.
- **Intended:** `.L` is a naming convention; an explicit global/weak directive overrides local omission. The source should assemble with the symbol present, or fail through `AsmX86Error`, never panic.
- **Consequence:** Valid symbol naming can crash the compiler's assembler subprocess/in-process pipeline.
- **Confidence:** High; reproduced.

### A09-14 — x86 undefined symbol declarations lose ELF metadata

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:146-168` records `.globl/.weak/.type/.size`, but undefined-symbol creation at `:785-799` preserves only binding and hardcodes `STT_NOTYPE`, value 0, size 0. Unreferenced declarations are never emitted at all.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' '.globl ext' '.type ext,@function' 'call ext' |
    "$AFS_AS" --64 - -o /tmp/a09-undef-afs.o
  printf '%s\n' '.text' '.globl ext' '.type ext,@function' 'call ext' |
    as --64 -o /tmp/a09-undef-gas.o
  readelf -sW /tmp/a09-undef-afs.o | rg ' ext$'
  readelf -sW /tmp/a09-undef-gas.o | rg ' ext$'
  ```
- **Actual:** `afs-as` emits `ext` as `NOTYPE`; GNU `as` emits `FUNC`. With only `.globl ext` and no relocation, `afs-as` omits `ext` while GNU `as` retains an undefined global.
- **Intended:** Explicit symbol declarations must survive model construction.
- **Consequence:** Linkers and tooling lose type/size/declaration information, affecting symbol classification and any type-sensitive processing.
- **Confidence:** High; reproduced for type and confirmed by the construction path for unreferenced declarations.

### A09-15 — executable-stack section metadata is accepted and then cleared

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/parse.rs:472-480` keeps only the section name and ignores all attributes; `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:145` ignores the marker; `/tmp/armfortas-audit/afs-as/src/elf.rs:748-763` always writes `.note.GNU-stack` with flags 0.
- **Reproduction:**

  ```sh
  printf '%s\n' '.section .note.GNU-stack,"x",@progbits' |
    "$AFS_AS" --64 - -o /tmp/a09-stack-afs.o
  printf '%s\n' '.section .note.GNU-stack,"x",@progbits' |
    as --64 -o /tmp/a09-stack-gas.o
  readelf -SW /tmp/a09-stack-afs.o | rg '\.note.GNU-stack'
  readelf -SW /tmp/a09-stack-gas.o | rg '\.note.GNU-stack'
  ```
- **Actual:** `afs-as` emits a non-executable GNU-stack marker; GNU `as` preserves the `X` flag.
- **Intended:** Preserve the accepted section attribute, or reject nonempty attributes as outside the narrow dialect.
- **Consequence:** Code that explicitly requires an executable stack (for example, stack trampolines) is marked non-executable and can fail at runtime. Other ignored section type/flag combinations are likewise silently canonicalized.
- **Confidence:** High; reproduced and follows the fixed writer flags.

### A09-16 — ELF object bytes are nondeterministic for multiple local commons

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:602` declares `local_bss` as `HashMap`; `:614-621` populates it; `:683-696` iterates it directly to append local symbols. `/tmp/armfortas-audit/afs-as/src/elf.rs:626-643` preserves model order within the local partition and string table.
- **Reproduction:**

  ```sh
  for i in $(seq 1 20); do
    printf '%s\n' \
      '.local alpha' '.comm alpha,8,8' \
      '.local beta'  '.comm beta,8,8' \
      '.local gamma' '.comm gamma,8,8' \
      '.local delta' '.comm delta,8,8' |
      "$AFS_AS" --64 - -o - | sha256sum | cut -d' ' -f1
  done | sort | uniq -c
  ```
- **Actual:** Twenty fresh processes produced 12 distinct SHA-256 hashes in the focused run; symbol/string-table order changes with `RandomState`.
- **Intended:** Identical source and target inputs must produce byte-identical objects.
- **Consequence:** Reproducible builds, fixed-point checks, content-addressed caches, and raw-object parity can fail nondeterministically.
- **Confidence:** High; reproduced across fresh processes and directly explained by the leaking `HashMap` iteration order.

### A09-17 — Mach-O relocation symbol resolution is quadratic

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/assemble.rs:2287-2305` linearly scans both existing and missing symbol vectors for every pending relocation; after sorting, `:2618-2628` again calls `symbols.iter().position` for every relocation.
- **Reproduction:**

  ```sh
  for n in 8000 16000; do
    TIMEFORMAT="n=$n elapsed=%R user=%U sys=%S"
    time (
      awk -v n="$n" 'BEGIN {
        print ".data";
        for (i=0;i<n;i++) { print ".extern ext" i; print ".quad ext" i }
      }' | "$AFS_AS" - -o - >/dev/null
    )
  done
  ```
- **Actual:** The focused debug run took 0.517 s for 8,000 relocations and 2.006 s for 16,000, approximately 3.9x time for 2x input. The nested linear searches make the quadratic relationship explicit.
- **Intended:** Symbol discovery and relocation-to-index resolution should use a name-to-index set/map, giving linear or `O((S+R) log S)` behavior.
- **Consequence:** Relocation-heavy generated code scales poorly and offers a low-cost CPU denial-of-service path.
- **Confidence:** High; measured and confirmed by source structure.

### A09-18 — tiny x86 virtual-BSS input can overflow layout arithmetic and panic

- **Severity:** Medium
- **Source:** `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:355-377`, `:405-423`, and `:455-477` use unchecked `pos += size` for virtual items. BSS avoids materializing the bytes, so enormous values reach the arithmetic from a tiny file.
- **Reproduction:**

  ```sh
  printf '%s\n' '.bss' \
    '.zero 9223372036854775807' \
    '.zero 9223372036854775807' \
    '.zero 3' |
    "$AFS_AS" --64 - -o /dev/null
  printf 'status=%s\n' "$?"
  ```
- **Actual:** In the audited debug build, layout panics on integer addition overflow and exits 101. With overflow checks disabled, the same additions wrap, yielding bogus offsets/section size.
- **Intended:** Detect non-representable section layout with checked arithmetic and return a located assembly error.
- **Consequence:** A roughly 100-byte source file can crash debug/compiler-test pipelines; release builds risk silently corrupt ELF layout state.
- **Confidence:** High; the edge was reproduced and all three layout walks contain the same unchecked operation.

### A09-19 — x86 encoder failures do not meet the documented diagnostic contract

- **Severity:** Low
- **Source:** README contract at `/tmp/armfortas-audit/afs-as/README.md:29-36`; `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:30-39` stores no column/snippet; encoder mapping at `:317-318`; CLI formatting at `/tmp/armfortas-audit/afs-as/src/main.rs:153-170` only prepends the path.
- **Reproduction:**

  ```sh
  printf '%s\n' '.text' 'bogus %rax' | "$AFS_AS" --64 - -o /dev/null
  ```
- **Actual:** stderr is only `-: line 2: bogus: unsupported mnemonic 'bogus' — grow the encoder with corpus evidence`; it has no column, source line, or caret. Some layout errors are constructed with synthetic line 0.
- **Intended:** The README promises file, line, column, source line, and caret for parse/assembly failures, with exit 1.
- **Consequence:** Diagnostics are less actionable and cannot reliably drive editor/IDE source locations. The x86 CLI smoke test checks only path and line, so it codifies the weaker behavior.
- **Confidence:** High; reproduced and directly contradicted by the documented contract.

## Unconfirmed concerns

- **Mach-O public-model overflow/truncation:** `/tmp/armfortas-audit/afs-as/src/macho.rs:283-373`, `:604-615`, and `:659-663` contain unchecked `u64` arithmetic, `as u32` layout conversions, saturating offsets, masked relocation fields, and fixed 16-byte name writes. Normal assembler-created models did not expose a malformed object in focused tests, so this remains a hardening concern rather than a confirmed source-input discrepancy.
- **ELF large-index/model boundaries:** `/tmp/armfortas-audit/afs-as/src/elf.rs:540-560`, `:656-675`, and parser code around `:1080-1100` use unchecked relocation-end addition and truncate section indexes/counts to `u16` without ELF extended-index support. I did not construct a practical focused model with enough sections/symbols to confirm emitted corruption.
- **Expression term merging:** `/tmp/armfortas-audit/afs-as/src/expr.rs:316-329` linearly searches accumulated terms. Distinct-symbol expressions are therefore structurally quadratic, but the tested expression is unrepresentable and GNU `as` was also slow on it; I did not promote this separately from the confirmed relocation-scaling issue.
- **Block-comment recursion:** `/tmp/armfortas-audit/afs-as/src/lex.rs` recursively asks for the next token after block comments. A sufficiently long run may mirror A09-06, but it was not exercised after the shared temporary filesystem reached its capacity limit.
- **Cross-section/external conditional jumps:** `/tmp/armfortas-audit/afs-as/src/x86/assemble.rs:507-525` explicitly rejects them rather than emitting `R_X86_64_PC32`. This is a compatibility/support gap, not classified as a discrepancy because the advertised x86 subset is compiler-output-driven and its exact standalone surface is not documented.

## Maintainability observations

- ARM add/sub parsing is duplicated between `parse_add_sub` and `parse_add_sub_stmt`/`parse_add_sub_operand`; width/range/SP validation can drift between the paths. More generally, the parser constructs trusted `Inst` fields via `as` casts while the encoder assumes validity, despite `assemble_source` being the documented validating boundary.
- The x86 implementation has no shared instruction schema for operand count, class, width, and immediate representability. Ad hoc slice matching versus `ops.first()` explains why most instructions reject extras but the zero-operand/push/setcc paths do not.
- `BuildVersion::default` invokes host `sw_vers` at `/tmp/armfortas-audit/afs-as/src/macho.rs:84-113`. This may deliberately mimic Apple `as`, but it makes otherwise identical no-`.build_version` source depend implicitly on the build host's macOS major version; cross-host reproducibility needs an explicit policy.
- Both object writers expose low-level public models with raw numeric fields. Central checked layout/index builders would make it harder for assembler and external library callers to bypass format invariants.
- x86 source locations are discarded when statements become `Item`s, leading later relaxation/layout errors to use line 0. Retaining a span per item would fix diagnostics and make panic/error reports attributable.

## Differential and regression test gaps

- ARM supported-case fuzzing chooses only small valid immediates (`/tmp/armfortas-audit/afs-as/tests/differential_fuzz.rs:61-81`), so it never reaches shifted add immediates or narrowing boundaries. The garbage companion (`:385-399`) checks only that `assemble_source` does not unwind; it does not compare acceptance/rejection or emitted bytes with Apple/LLVM MC. Native differential execution is also host-gated at `:364-368` even though LLVM MC cross-target comparison is available on this host.
- No ARM generator deliberately mixes `w`/`x` or `s`/`d` operand widths, uses `sp` in register add/sub forms, or concatenates valid mnemonics on one line. Those are exactly the near-valid cases that evade token-soup fuzzing.
- The x86 generator explicitly clamps qword arithmetic immediates to `i32` (`/tmp/armfortas-audit/afs-as/tests/common/x86_gen.rs:103-130`), emits only register-register `testq`, uses `.p2align` without a fill, has no weak branch case, and emits at most one local common per generated file (`:308-311`). Thus A09-07, A09-09, A09-10, and A09-16 are outside its support matrix.
- The structured x86 negative suite has only seven cases (`/tmp/armfortas-audit/afs-as/tests/x86_encode_rejects.rs:15-28`). It does not systematically mutate valid instructions by adding/removing operands or crossing each immediate-width boundary.
- ELF differential normalization deliberately removes symbol order and then sorts symbols (`/tmp/armfortas-audit/afs-as/tests/common/elf.rs:76-92`, `:163-178`), hiding A09-16. The unit determinism test writes the same prebuilt model twice (`/tmp/armfortas-audit/afs-as/src/elf.rs:1262-1266`); generated determinism sources contain only one local common, so assembly-stage randomized ordering is not exercised.
- The x86 CLI error test asserts only filename and `line 2` (`/tmp/armfortas-audit/afs-as/tests/cli_smoke.rs:274-290`), while ARM tests assert source/caret. This misses A09-19 despite the common README contract.
- The Mach-O performance gate uses only 96 versus 192 generated blocks and allows an 8x ratio plus 250 ms (`/tmp/armfortas-audit/afs-as/tests/perf_sanity.rs:159-180`). It does not isolate symbol/relocation cardinality, so the confirmed 8k/16k quadratic path remains below its sensitivity and the entire test skips off native arm64 macOS.
- Corpus/differential tests heavily normalize tool output or target existing compiler emissions. Add focused accept/reject parity tables for directive optional fields, symbol-class collisions, weak/preemptible relocations, explicit section flags, default common alignment, and writer byte determinism across fresh processes.

## Review disposition

The highest-risk issues are A09-01 and A09-02 on ARM64 and A09-07 through A09-09 on x86 because they emit semantically different machine code for accepted input. A09-16 and A09-17 directly undermine the project's reproducibility and scaling claims. No additional normal-input Mach-O relocation-type/pair-order or ELF RELA addend discrepancy was confirmed in the focused review; the remaining writer boundary concerns above need dedicated adversarial model tests.
