# afs-ld end-to-end quality audit (audit 10)

Date: 2026-07-09
Pinned afs-ld revision: `615de762090c8a9c73033ca1659b021cefe4331d`

## Scope and method

This review covered the pinned `afs-ld` submodule end to end: CLI parsing and input ordering; Mach-O object, archive, dylib, and text-stub ingestion; symbol resolution and atomization; dead stripping and ICF; layout and relocation application; synthetic/linkedit sections; Mach-O writing and code signing; ELF static and dynamic writing; diagnostics; deterministic parallel work; and scaling risks. I read `afs-ld/CLAUDE.md` before inspecting implementation. I did not read any pre-existing `.docs/audits` report and did not run the full workspace suite.

The findings below are confirmed by complete source-path traces. I prepared focused, self-contained fixtures for each. The initial worker build was interrupted when the shared temporary filesystem filled, and this host does not provide Apple's loader. After space was recovered, the coordinator built `afs-ld` and executed representative Linux ELF fixtures for DSO retention, initializer metadata, and executable-defined IFUNC handling. The remaining fixtures are exact source-derived reproductions rather than claims of local execution; Mach-O runtime behavior still requires an arm64 macOS host. This limitation is reflected in the confidence statements.

Common setup from the repository root:

```sh
cd /tmp/armfortas-audit
cargo build -p afs-ld
AFS=/tmp/armfortas-audit/target/debug/afs-ld
```

For Mach-O reproductions:

```sh
SDK=$(xcrun --sdk macosx --show-sdk-path)
TBD="$SDK/usr/lib/libSystem.tbd"
```

For dynamic ELF reproductions:

```sh
RTLD=$(cc -print-file-name=ld-linux-x86-64.so.2)
```

Severity follows the submodule's own quality bar: **Critical** means a loader-accepted binary can silently execute different semantics or corrupt addresses; **Major** means valid input fails, a malformed input crashes the linker, or an important ABI contract is violated; **Moderate** is incorrect metadata with downstream tooling impact; **Minor** is a material diagnostic defect.

| Area | Confirmed findings |
| --- | ---: |
| Arguments and input order | 2 |
| Mach-O ingestion and symbols | 6 |
| Mach-O atomization, layout, relocation, and writing | 10 |
| ELF ingestion, layout, relocation, and writing | 9 |
| Diagnostics | 1 |
| **Total** | **28 (10 Critical, 16 Major, 1 Moderate, 1 Minor)** |

## Confirmed discrepancies

### A1. Mixed positional, `-l`, and framework order is destroyed

**Severity:** Critical

**Source:** `afs-ld/src/args.rs:189-218` and `afs-ld/src/args.rs:450-452` store positional files, libraries, and frameworks in separate vectors. `afs-ld/src/lib.rs:487-513` reconstructs a new order: positional non-dylibs, all `-l` inputs, all frameworks, then positional dylibs.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > main.s <<'EOF'
.text
.globl _main
_main:
  stp x29, x30, [sp, #-16]!
  mov x29, sp
  bl _choice
  ldp x29, x30, [sp], #16
  ret
.subsections_via_symbols
EOF
cat > a.s <<'EOF'
.text
.globl _choice
_choice: mov w0, #11; ret
EOF
cat > b.s <<'EOF'
.text
.globl _choice
_choice: mov w0, #22; ret
EOF
for n in main a b; do xcrun as -arch arm64 "$n.s" -o "$n.o"; done
xcrun ar rcs libA.a a.o
xcrun ar rcs libB.a b.o
xcrun ld -arch arm64 -platform_version macos 13.0 13.0 \
  -syslibroot "$SDK" main.o -L "$PWD" -lA libB.a -lSystem \
  -e _main -o apple.out
$AFS -arch arm64 -platform_version macos 13.0 13.0 \
  -syslibroot "$SDK" main.o -L "$PWD" -lA libB.a -lSystem \
  -e _main -o afs.out
./apple.out; echo "apple=$?"
./afs.out; echo "afs=$?"
```

**Actual behavior:** `afs-ld` moves positional `libB.a` ahead of `-lA`, so B supplies `_choice` and the program returns 22. Positional dylibs are likewise moved after all `-l` libraries/frameworks, changing provider selection, load-command order, and bind ordinals.

**Intended behavior:** Preserve the user's single left-to-right input stream. In the command above A precedes B, so Apple `ld` selects A and the program returns 11.

**Consequence:** A successful binary can silently call a different implementation. Dylib reordering can also encode different dependency ordinals and weak/normal load order.

**Confidence:** High; the parser data model and reconstruction loops make the reordering unconditional.

### A2. `-force_load` does not add its archive

**Severity:** Major

**Source:** `afs-ld/src/args.rs:411-416` records the operand only in `force_load_archives`. `afs-ld/src/lib.rs:566-576` later searches already registered positional archives and errors unless an identical raw `PathBuf` is present.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > plugin.s <<'EOF'
.text
.globl _plugin
_plugin: ret
EOF
cat > main.s <<'EOF'
.text
.globl _main
_main: mov w0, #0; ret
EOF
xcrun as -arch arm64 plugin.s -o plugin.o
xcrun as -arch arm64 main.s -o main.o
xcrun ar rcs libplugin.a plugin.o
$AFS -arch arm64 -e _main main.o -force_load libplugin.a -o force.out
```

**Actual behavior:** The command errors that `libplugin.a` must also be present as an archive input. Even duplicating it positionally can fail for equivalent spellings such as `libplugin.a` and `./libplugin.a`.

**Intended behavior:** `-force_load path` itself opens `path` and loads every member, matching the Darwin linker contract.

**Consequence:** Plugin/category archives and other intentionally unreferenced registration objects cannot be linked using the standard option.

**Confidence:** High; there is no path from the option vector to archive registration.

### I1. Extension-based dispatch ingests extensionless dylibs as object files

**Severity:** Major

**Source:** Dispatch is based on `.a`, `.dylib`, and `.tbd` suffixes in `afs-ld/src/lib.rs:487-525`, `afs-ld/src/lib.rs:1027-1039`, and `afs-ld/src/lib.rs:1165-1237`. Framework lookup deliberately returns extensionless `Foo.framework/Foo` at `afs-ld/src/lib.rs:972-980`. `ObjectFile::parse` at `afs-ld/src/input.rs:60-109` never requires `MH_OBJECT`; `afs-ld/src/macho/reader.rs:88-113` checks magic and CPU but not file type.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > foo.s <<'EOF'
.text
.globl _foo
_foo: mov w0, #7; ret
EOF
cat > main.s <<'EOF'
.text
.globl _main
_main:
  stp x29, x30, [sp, #-16]!
  mov x29, sp
  bl _foo
  ldp x29, x30, [sp], #16
  ret
EOF
xcrun as -arch arm64 foo.s -o foo.o
xcrun as -arch arm64 main.s -o main.o
xcrun ld -arch arm64 -dylib -platform_version macos 13.0 13.0 \
  -install_name "$PWD/Foo" foo.o -o Foo
$AFS -arch arm64 -e _main main.o "$PWD/Foo" "$TBD" -o dispatch.out
xcrun nm -m dispatch.out | grep _foo
xcrun otool -L dispatch.out
```

**Actual behavior:** `Foo` is atomized as an object; `_foo` is copied into the output as a definition, and no `LC_LOAD_DYLIB` for `Foo` is emitted. Renamed archives are similarly misclassified, while an object whose name ends in `.a` is forced through the archive parser.

**Intended behavior:** Dispatch from archive magic and Mach-O `filetype`; ingest `MH_DYLIB` as a provider/import and emit its load command.

**Consequence:** Extensionless framework binaries and renamed libraries are silently linked with the wrong ownership and dependency model.

**Confidence:** High; the accepted header fields and path-suffix branch are explicit.

### I2. Real GNU thin archives are not decoded

**Severity:** Major

**Source:** `afs-ld/src/archive.rs:397-414` treats every thin member, including `/`, `//`, and `/NNN`, as an ordinary zero-body member. `afs-ld/src/archive.rs:382-390` nevertheless advances by the external member's declared size. `afs-ld/src/resolve.rs:1013-1019` ignores an archive when this loses its symbol index.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > helper.s <<'EOF'
.text
.globl _helper
_helper: ret
EOF
cat > main.s <<'EOF'
.text
.globl _main
_main: bl _helper; mov w0, #0; ret
EOF
xcrun as -arch arm64 helper.s -o helper.o
xcrun as -arch arm64 main.s -o main.o
"$(xcrun --find llvm-ar)" rcsT libthin.a helper.o
$AFS --dump-archive libthin.a
$AFS -arch arm64 -e _main main.o libthin.a -o thin.out
```

**Actual behavior:** The dump identifies `GNU-thin` but reports zero symbols, and linking reports `_helper` undefined.

**Intended behavior:** Parse inline special tables, resolve `/NNN` via `//`, advance ordinary thin headers without assuming an inline body, and lazily open `helper.o`.

**Consequence:** Archives produced by `llvm-ar rcsT` cannot supply members.

**Confidence:** High; the special-member decoding path is bypassed for all thin members.

### S1. Common symbols are resolved but never allocated

**Severity:** Major

**Source:** `afs-ld/src/resolve.rs:1773-1781` creates `Symbol::Common`. `afs-ld/src/lib.rs:631-640` atomizes only existing input sections, and `afs-ld/src/reloc/arm64.rs:1274-1318` has no relocation target case for `Common`. `afs-ld/src/macho/writer.rs:1787-1800` skips non-`Defined` output globals.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > common.s <<'EOF'
.text
.globl _main
_main:
  adrp x0, _shared@PAGE
  add x0, x0, _shared@PAGEOFF
  mov w0, #0
  ret
.comm _shared,8,3
.subsections_via_symbols
EOF
xcrun as -arch arm64 common.s -o common.o
$AFS -arch arm64 -e _main common.o "$TBD" -o common.out
```

**Actual behavior:** A reference reaches relocation with state `Common` and errors as unsupported. If unreferenced, the common symbol and storage are silently omitted.

**Intended behavior:** Coalesce common declarations and allocate eight bytes at alignment 2^3, conventionally in `__DATA,__common`, then resolve references to that storage.

**Consequence:** Valid C/Fortran tentative definitions cannot link, or vanish when not referenced internally.

**Confidence:** High; no common-to-atom promotion exists.

### S2. Unresolved weak references pass classification but cannot be linked

**Severity:** Major

**Source:** `afs-ld/src/resolve.rs:1638-1674` accepts a remaining weak reference but leaves it `Symbol::Undefined`. GOT planning at `afs-ld/src/synth/mod.rs:589-604` covers imports/definitions, while `afs-ld/src/reloc/arm64.rs:1143-1150` may locally relax `Undefined` and `afs-ld/src/reloc/arm64.rs:1274-1318` ultimately rejects it.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > weak.s <<'EOF'
.text
.globl _main
_main:
  adrp x0, _optional@GOTPAGE
  ldr x0, [x0, _optional@GOTPAGEOFF]
  mov w0, #0
  ret
.weak_reference _optional
.subsections_via_symbols
EOF
xcrun as -arch arm64 weak.s -o weak.o
$AFS -arch arm64 -e _main weak.o "$TBD" -o weak.out
```

**Actual behavior:** Undefined-symbol classification succeeds, then relocation reports that `_optional` remains in unsupported state `Undefined`.

**Intended behavior:** Emit a weak undefined import/bind whose value is zero when no provider exists.

**Consequence:** Standard optional-symbol probes and weak compatibility hooks fail to link.

**Confidence:** High; accepted state has no synthesis or relocation consumer.

### S3. `N_INDR` aliases are discarded

**Severity:** Major

**Source:** `afs-ld/src/resolve.rs:1763-1806` returns `None` for `SymKind::Indirect`. `afs-ld/src/input.rs:117-123` can decode the target name, but production resolution never calls it.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
python3 - <<'PY'
import struct
s=b"\0_alias\0_target\0"
h=struct.pack("<IIIIIIII",0xfeedfacf,0x0100000c,0,1,2,96,0,0)
seg=struct.pack("<II16sQQQQIIII",0x19,72,b"",0,0,0,0,7,7,0,0)
st=struct.pack("<IIIIII",2,24,128,1,144,len(s))
n=struct.pack("<IBBHQ",1,0x0b,0,0,8) # N_INDR|N_EXT -> _target
open("alias.o","wb").write(h+seg+st+n+s)
PY
cat > target.s <<'EOF'
.text
.globl _target
_target: ret
EOF
cat > main.s <<'EOF'
.text
.globl _main
_main: bl _alias; mov w0, #0; ret
EOF
xcrun as -arch arm64 target.s -o target.o
xcrun as -arch arm64 main.s -o main.o
$AFS -arch arm64 -e _main alias.o target.o main.o "$TBD" -o alias.out
```

**Actual behavior:** `_alias` is absent from the resolver and is diagnosed undefined.

**Intended behavior:** Resolve `_alias` to the same definition/value as `_target`.

**Consequence:** Mach-O compatibility aliases and re-export-style indirect symbols are unusable.

**Confidence:** High; the resolver explicitly drops this symbol kind.

### S4. External absolute symbols are represented as atoms and fail relocation

**Severity:** Major

**Source:** `afs-ld/src/resolve.rs:1790-1800` represents `N_ABS` as `Defined { atom: AtomId(0), value }`. `afs-ld/src/reloc/arm64.rs:1282-1300` interprets every `Defined` target through atom-address lookup, so atom zero has no final address.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > defs.s <<'EOF'
.globl _answer
.set _answer,42
EOF
cat > use.s <<'EOF'
.data
.globl _p
_p: .quad _answer
.text
.globl _main
_main: mov w0, #0; ret
EOF
xcrun as -arch arm64 defs.s -o defs.o
xcrun as -arch arm64 use.s -o use.o
$AFS -arch arm64 -e _main defs.o use.o "$TBD" -o abs.out
```

**Actual behavior:** Relocation resolution attempts to obtain a final address for atom zero and errors.

**Intended behavior:** Apply the absolute value 42 directly and preserve an absolute output symbol.

**Consequence:** Linker/assembly constants exported from one object cannot be consumed by another.

**Confidence:** High; relocation conflates absolute and section-defined symbols.

### M1. Rebase streams omit pointers in custom segments

**Severity:** Critical

**Source:** `afs-ld/src/macho/writer.rs:1238-1292` builds classic rebases, but `afs-ld/src/macho/writer.rs:1257-1260` accepts relocation sites only in segments named `__DATA` or `__DATA_CONST`.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > rebase.s <<'EOF'
.section __CUSTOM,__ptrs,regular
.p2align 3
.globl _p
_p: .quad _target
.data
.p2align 3
.globl _target
_target: .quad 7
.text
.globl _main
_main:
  adrp x8, _p@PAGE
  add x8, x8, _p@PAGEOFF
  ldr x9, [x8]
  adrp x10, _target@PAGE
  add x10, x10, _target@PAGEOFF
  cmp x9, x10
  cset w0, ne
  ret
EOF
xcrun as -arch arm64 rebase.s -o rebase.o
$AFS -arch arm64 rebase.o "$TBD" -o rebase.out
./rebase.out; echo $?
```

**Actual behavior:** `_p` contains `_target`'s preferred VM address but has no rebase opcode. With ASLR, the pointer remains unslid and the program returns 1 (dereferencing it instead can fault).

**Intended behavior:** Every runtime-rebased pointer in a mapped segment is represented in rebase metadata; the program returns 0.

**Consequence:** Custom data segments contain invalid pointers under ASLR, causing crashes or silent memory corruption in otherwise loadable binaries.

**Confidence:** High; the segment-name filter unconditionally discards the site after the static address is written.

### M2. PC-relative `ARM64_RELOC_POINTER_TO_GOT` is patched as absolute

**Severity:** Major

**Source:** `afs-ld/src/reloc/arm64.rs:809-816` dispatches this relocation to `patch_unsigned`; `afs-ld/src/reloc/arm64.rs:1494-1531` adds target/addends but never subtracts the relocation place when `pcrel` is true.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > got-delta.s <<'EOF'
.data
.globl _delta
_delta: .long _puts@GOT - .
.text
.globl _main
_main: ret
EOF
xcrun as -arch arm64 got-delta.s -o got-delta.o
$AFS -arch arm64 got-delta.o "$TBD" -o got-delta.out
xcrun otool -l got-delta.out
xcrun otool -s __DATA __data got-delta.out
```

Use the load-command addresses to evaluate the four-byte field.

**Actual behavior:** The field contains the low 32 bits of `GOT(_puts)`.

**Intended behavior:** The signed field is `GOT(_puts) - &_delta`, as requested by the PC-relative relocation.

**Consequence:** Valid pointer-to-GOT data references and jump-table entries resolve to unrelated addresses.

**Confidence:** High; the relocation's `pcrel` bit is parsed but ignored by its patcher.

### M3. Imported `UNSIGNED` pointer addends are discarded

**Severity:** Critical

**Source:** Direct-bind planning at `afs-ld/src/synth/mod.rs:152-170` records only `reloc.addend`. For an ordinary `UNSIGNED` relocation the assembler stores the implicit addend in the slot; `afs-ld/src/reloc/arm64.rs:699-713` clears that slot, and `afs-ld/src/macho/writer.rs:2452-2476` emits only the recorded zero addend.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > bind-addend.s <<'EOF'
.data
.p2align 3
.globl _p
_p: .quad _puts + 4
.text
.globl _main
_main:
  adrp x8, _p@PAGE
  ldr x8, [x8, _p@PAGEOFF]
  adrp x9, _puts@GOTPAGE
  ldr x9, [x9, _puts@GOTPAGEOFF]
  add x9, x9, #4
  cmp x8, x9
  cset w0, ne
  ret
EOF
xcrun as -arch arm64 bind-addend.s -o bind-addend.o
$AFS -arch arm64 bind-addend.o "$TBD" -o bind-addend.out
./bind-addend.out; echo $?
```

**Actual behavior:** The bind stream uses addend zero, so `_p == _puts` and the program returns 1.

**Intended behavior:** Preserve the implicit `+4` in bind metadata, making `_p == _puts + 4` and returning 0.

**Consequence:** Loader-accepted imported function/data pointers silently reference the wrong byte.

**Confidence:** High; the input addend is overwritten before any code transfers it to the bind record.

### M4. ICF conflates same-numbered section referents from different objects

**Severity:** Critical

**Source:** `afs-ld/src/icf.rs:387-450` represents `FoldReferent::Section` with only a `u8` section ordinal; normalization at `afs-ld/src/icf.rs:449` omits both `InputId` and resolved target atom identity.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > a.s <<'EOF'
.text
.globl _fa
.private_extern _fa
_fa:
  adr x0, Lpa
  ldr x0, [x0]
  ldr w0, [x0]
  ret
.p2align 3
Lpa:
  .data_region
  .quad Lda
  .end_data_region
.data
.p2align 3
Lda: .quad 1
EOF
sed 's/_fa/_fb/g; s/Lpa/Lpb/g; s/Lda/Ldb/g; s/\.quad 1/.quad 2/' a.s > b.s
cat > main.s <<'EOF'
.text
.globl _main
_main:
  sub sp, sp, #32
  stp x29, x30, [sp, #16]
  bl _fa
  str w0, [sp, #12]
  bl _fb
  ldr w1, [sp, #12]
  add w0, w0, w1
  ldp x29, x30, [sp, #16]
  add sp, sp, #32
  ret
EOF
for f in a b main; do xcrun as -arch arm64 "$f.s" -o "$f.o"; done
$AFS -arch arm64 -icf=safe a.o b.o main.o "$TBD" -o icf.out
./icf.out; echo $?
```

**Actual behavior:** `_fa` and `_fb` receive the same fold signature despite referring to distinct local data and can fold, producing 2 or 4.

**Intended behavior:** Include input/target identity in the signature; the functions remain distinct and return 3.

**Consequence:** Safe ICF can silently replace one function with a semantically different function from another object.

**Confidence:** High; section referents are explicitly reduced to an object-local ordinal without the object.

### M5. Dead stripping does not root initializer sections or section-level retention flags

**Severity:** Critical

**Source:** Section classification at `afs-ld/src/section.rs:61-86` omits `S_MOD_INIT_FUNC_POINTERS` and `S_MOD_TERM_FUNC_POINTERS`. Root discovery at `afs-ld/src/why_live.rs:449-493` handles entry points, symbol-level `N_NO_DEAD_STRIP`, and dylib exports, but not initializer/terminator sections, `S_ATTR_NO_DEAD_STRIP`, or `S_ATTR_LIVE_SUPPORT`.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > ctor.s <<'EOF'
.data
.globl _seen
_seen: .long 0
.text
_ctor:
  adrp x8, _seen@PAGE
  add x8, x8, _seen@PAGEOFF
  mov w9, #1
  str w9, [x8]
  ret
.globl _main
_main:
  adrp x8, _seen@PAGE
  ldr w0, [x8, _seen@PAGEOFF]
  ret
.section __DATA,__mod_init_func,mod_init_funcs
.p2align 3
.quad _ctor
EOF
xcrun as -arch arm64 ctor.s -o ctor.o
$AFS -arch arm64 -dead_strip ctor.o "$TBD" -o ctor.out
./ctor.out; echo $?
```

**Actual behavior:** The initializer pointer and `_ctor` are removed; `_main` returns 0.

**Intended behavior:** The initializer section is a root, retains `_ctor`, and dyld runs it before `_main`, which returns 1.

**Consequence:** Constructors, destructors, and explicitly retained support code silently disappear from successful binaries.

**Confidence:** High; none of the relevant section flags enter liveness construction.

### M6. Incompatible same-name Mach-O sections merge and initialized bytes can vanish

**Severity:** Critical

**Source:** Output sections are keyed only by `(segment, section)` at `afs-ld/src/layout.rs:45-49` and `afs-ld/src/layout.rs:154-214`; the first contribution fixes section kind/flags. `afs-ld/src/macho/writer.rs:344-370` skips all atom bytes when that output section is zero-fill.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > z.s <<'EOF'
.globl _z
.zerofill __DATA,__foo,_z,8,3
EOF
cat > r.s <<'EOF'
.section __DATA,__foo,regular
.p2align 3
.globl _x
_x: .quad 42
EOF
cat > main.s <<'EOF'
.text
.globl _main
_main:
  adrp x8, _x@PAGE
  ldr x0, [x8, _x@PAGEOFF]
  ret
EOF
for f in z r main; do xcrun as -arch arm64 "$f.s" -o "$f.o"; done
$AFS -arch arm64 z.o r.o main.o "$TBD" -o mixed.out
./mixed.out; echo $?
```

**Actual behavior:** Link succeeds; `__DATA,__foo` retains zero-fill kind and `_x`'s initialized bytes are omitted, so the program returns 0.

**Intended behavior:** Reject incompatible section types or produce a regular section that preserves 42.

**Consequence:** Valid initialized data is silently replaced by zeros in a loadable executable.

**Confidence:** High; the first section's kind controls the sole output section and the writer skips its entire payload.

### M7. Same-address aliases detach the entry symbol from code under `-dead_strip`

**Severity:** Critical

**Source:** `afs-ld/src/atom.rs:523-569` creates separate boundaries for equal-offset non-`N_ALT_ENTRY` symbols, giving the first a zero-size atom and the second the bytes. `afs-ld/src/why_live.rs:449-493` roots only the entry's atom.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > same-address.s <<'EOF'
.text
.globl _main
.globl _zalias
_main:
_zalias:
  mov w0, #42
  ret
.subsections_via_symbols
EOF
xcrun as -arch arm64 same-address.s -o same-address.o
$AFS -arch arm64 -dead_strip same-address.o "$TBD" -o same-address.out
xcrun otool -tvV same-address.out
./same-address.out; echo $?
```

**Actual behavior:** The zero-size `_main` atom is retained while the alias atom owning the instructions is dead; `LC_MAIN` points at padding or unrelated bytes instead of the function.

**Intended behavior:** Equal-address aliases denote one content atom, which remains live and returns 42.

**Consequence:** A successful dead-stripped executable can start at non-code or wrong code.

**Confidence:** High; atom sizing and liveness operate on the two artificial atoms independently.

### M8. Executables with no entry symbol link successfully

**Severity:** Major

**Source:** `afs-ld/src/lib.rs:1278-1291` returns no entry when neither `_main` nor `_start` exists. `afs-ld/src/macho/writer.rs:2599-2620` silently falls back to the beginning of `__text` for `LC_MAIN`.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > no-main.s <<'EOF'
.text
.globl _foo
_foo: mov w0, #7; ret
EOF
xcrun as -arch arm64 no-main.s -o no-main.o
$AFS -arch arm64 no-main.o "$TBD" -o no-main.out
echo "link status=$?"
xcrun otool -l no-main.out | grep -A3 LC_MAIN
```

**Actual behavior:** Linking succeeds and `LC_MAIN` selects `_foo`/the first text byte.

**Intended behavior:** Diagnose a missing default entry point unless the user explicitly selects one or requests a non-executable output.

**Consequence:** Broken build configurations yield apparently valid executables with accidental startup behavior.

**Confidence:** High; the writer's fallback is unconditional.

### M9. CodeDirectory executable-segment metadata does not describe `__TEXT`

**Severity:** Major

**Source:** `afs-ld/src/synth/code_sig.rs:118-139` sets `execSegBase` to zero and derives `execSegLimit` from executable sections, excluding headers, non-code `__TEXT` content, and page padding instead of using `__TEXT.fileoff/filesize`.

**Reproduction:** Link any normal Mach-O executable, then run:

```sh
python3 - ./program <<'PY'
import struct,sys
b=open(sys.argv[1],"rb").read(); n=struct.unpack_from("<I",b,16)[0]
p=32; text=sig=None
for _ in range(n):
    cmd,size=struct.unpack_from("<II",b,p)
    if cmd==0x19 and b[p+8:p+24].split(b"\0",1)[0]==b"__TEXT":
        text=struct.unpack_from("<QQ",b,p+40)
    if cmd==0x1d: sig=struct.unpack_from("<I",b,p+8)[0]
    p+=size
cd=sig+struct.unpack_from(">I",b,sig+16)[0]
print("TEXT fileoff/filesize",text)
print("execSeg base/limit/flags",struct.unpack_from(">QQQ",b,cd+64))
PY
```

**Actual behavior:** `execSegLimit` is the small executable-section extent and does not equal the page-aligned `__TEXT.filesize` printed above.

**Intended behavior:** CodeDirectory executable-segment fields cover the output `__TEXT` file range.

**Consequence:** The signature carries inaccurate security metadata and may be rejected or interpreted differently by policy and signing tools even when basic ad-hoc validation accepts it.

**Confidence:** High; the code never reads the final `__TEXT` segment range for these fields.

### M10. Mach-O UUIDs do not depend on binary contents

**Severity:** Moderate

**Source:** `afs-ld/src/macho/writer.rs:707-745` hashes output kind plus segment/section names, addresses, sizes, offsets, and flags, but no atom bytes, relocation results, imports, or symbols.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"; mkdir one two
cat > one.s <<'EOF'
.text
.globl _main
_main: mov w0, #1; ret
EOF
cat > two.s <<'EOF'
.text
.globl _main
_main: mov w0, #2; ret
EOF
for f in one two; do xcrun as -arch arm64 "$f.s" -o "$f.o"; done
$AFS -arch arm64 one.o "$TBD" -o one/program
$AFS -arch arm64 two.o "$TBD" -o two/program
xcrun dwarfdump --uuid one/program two/program
```

**Actual behavior:** Same-layout programs with different instructions receive identical UUIDs.

**Intended behavior:** Content changes produce a different output identity.

**Consequence:** dSYM lookup, crash symbolication, and caches can associate a binary with the wrong artifacts.

**Confidence:** High; content bytes are absent from the UUID hash inputs.

### E1. `--gc-sections` is accepted but has no effect

**Severity:** Major

**Source:** `afs-ld/src/main.rs:197-200` accepts/ignores the option. ELF parsing retains every `SHF_ALLOC` section at `afs-ld/src/elf.rs:376-417`; static layout/relocation retains them at `afs-ld/src/elf.rs:1358-1407` and `afs-ld/src/elf.rs:1960-2003`, as does the dynamic path at `afs-ld/src/elf.rs:2385-2430` and `afs-ld/src/elf.rs:3139-3226`.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > gc.s <<'EOF'
.section .text.start,"ax",@progbits
.globl _start
_start:
  mov $60,%eax
  xor %edi,%edi
  syscall
.section .text.dead,"ax",@progbits
.globl dead
dead: call missing
.globl missing
EOF
as --64 -o gc.o gc.s
$AFS --gc-sections -o afs-gc gc.o
ld --gc-sections -o gnu-gc gc.o
```

**Actual behavior:** `afs-ld` keeps `.text.dead` and reports `missing` undefined.

**Intended behavior:** Discard the unreachable section; GNU `ld` links and its output exits 0.

**Consequence:** Function-section builds fail on dead references, retain dead data, and can spuriously extract archive members.

**Confidence:** High; there is no ELF liveness graph or conditional discard path.

### E2. Same-named ELF sections inherit the first contribution's flags

**Severity:** Major

**Source:** Static grouping at `afs-ld/src/elf.rs:1358-1407` and dynamic grouping at `afs-ld/src/elf.rs:2385-2430` use only the section name as the map key despite a `(name, flags)` comment; flags are neither merged nor checked.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > ro.s <<'EOF'
.section .same,"a",@progbits
.byte 0
EOF
cat > rw.s <<'EOF'
.section .same,"aw",@progbits
.globl cell
cell: .long 0
.text
.globl _start
_start:
  movl $42,cell(%rip)
  movl cell(%rip),%edi
  mov $60,%eax
  syscall
EOF
as --64 -o ro.o ro.s
as --64 -o rw.o rw.s
$AFS -o afs-flags ro.o rw.o
ld -o gnu-flags ro.o rw.o
./afs-flags; echo "afs=$?"
./gnu-flags; echo "gnu=$?"
```

**Actual behavior:** `.same` retains the first object's non-writable flags, placing `cell` in a read-only/RX mapping; the store faults.

**Intended behavior:** Reject/separate incompatible contributions or preserve the required writable attribute; GNU output exits 42.

**Consequence:** The linker silently emits wrong segment permissions and a valid program crashes at runtime.

**Confidence:** High; the first-created output section owns the only flags field.

### E3. Shared libraries are always treated as `--as-needed`

**Severity:** Critical

**Source:** `afs-ld/src/main.rs:199-200` ignores `--no-as-needed`. `afs-ld/src/elf.rs:2561-2589` marks `used_lib` only after resolving an imported symbol; `afs-ld/src/elf.rs:2750-2757` and `afs-ld/src/elf.rs:3368-3370` emit `DT_NEEDED` only for such libraries.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > ctor.s <<'EOF'
.text
.globl boom
.type boom,@function
boom:
  mov $60,%eax
  mov $42,%edi
  syscall
EOF
cat > start.s <<'EOF'
.text
.globl _start
_start:
  mov $60,%eax
  xor %edi,%edi
  syscall
EOF
as --64 -o ctor.o ctor.s
as --64 -o start.o start.s
ld -shared -soname libctor.so -init boom -o libctor.so ctor.o
$AFS --dynamic-linker "$RTLD" --no-as-needed -o afs-needed start.o ./libctor.so
ld --dynamic-linker "$RTLD" --no-as-needed -o gnu-needed start.o ./libctor.so
readelf -d afs-needed | grep NEEDED || true
readelf -d gnu-needed | grep NEEDED
./afs-needed; echo "afs=$?"
./gnu-needed; echo "gnu=$?"
```

**Actual behavior:** `afs-ld` omits `DT_NEEDED` because no ordinary symbol import uses the DSO; the output exits 0 and never runs `boom`.

**Intended behavior:** `--no-as-needed` retains the DSO; GNU output loads it and its initializer exits 42.

**Consequence:** Constructor-only libraries, plugins, registration modules, and audit/interposition dependencies silently disappear from a successful binary.

**Confidence:** High; the option is discarded and DSO retention has exactly one symbol-use gate.

### E4. Archive-only, linker-script-only, and `-l`-only invocations do not dispatch to ELF

**Severity:** Major

**Source:** `afs-ld/src/main.rs:215-224` selects ELF only when a positional file begins directly with ELF magic. `afs-ld/src/elf.rs:1012-1031`, `afs-ld/src/elf.rs:1223-1266`, and `afs-ld/src/elf.rs:1333-1344` also fail to seed archive extraction from the entry symbol.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > start.s <<'EOF'
.text
.globl _start
_start:
  mov $60,%eax
  mov $42,%edi
  syscall
EOF
as --64 -o start.o start.s
ar rcs libstart.a start.o
$AFS -melf_x86_64 -o afs-archive libstart.a
file afs-archive 2>/dev/null || true
ld -m elf_x86_64 -o gnu-archive libstart.a
./gnu-archive; echo $?
```

**Actual behavior:** The CLI falls through to the Mach-O path because archive magic is not ELF magic. Even the ELF library path has no initial `_start` demand with which to extract the archive member.

**Intended behavior:** `-melf_x86_64` and/or archive contents select ELF, and the executable entry is an archive extraction root; GNU output exits 42.

**Consequence:** Advertised ELF archive ingestion is unusable for archive-only entry points, and script/`-l`-driven links can select the wrong output format.

**Confidence:** High; format detection examines only direct positional ELF objects and entry seeding is absent.

### E5. Malformed ELF relocation indices and offsets panic

**Severity:** Major

**Source:** `afs-ld/src/elf.rs:452-480` records `r_sym`/`r_offset` without range validation. Symbol access is unchecked at `afs-ld/src/elf.rs:1526-1529`, `afs-ld/src/elf.rs:2617-2643`, and `afs-ld/src/elf.rs:3073-3078`; output slicing is unchecked at `afs-ld/src/elf.rs:1980-2028` and `afs-ld/src/elf.rs:3171-3238`.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > bad.s <<'EOF'
.text
.globl _start
_start: call target
EOF
cat > target.s <<'EOF'
.text
.globl target
target: ret
EOF
as --64 -o bad.o bad.s
as --64 -o target.o target.s
python3 - <<'PY'
from pathlib import Path
import struct
p=Path("bad.o"); b=bytearray(p.read_bytes())
shoff=struct.unpack_from("<Q",b,40)[0]
entsz=struct.unpack_from("<H",b,58)[0]
shnum=struct.unpack_from("<H",b,60)[0]
for i in range(shnum):
    sh=shoff+i*entsz
    if struct.unpack_from("<I",b,sh+4)[0] == 4: # SHT_RELA
        rela=struct.unpack_from("<Q",b,sh+24)[0]
        struct.pack_into("<Q",b,rela,0x100000)
        break
p.write_bytes(b)
PY
$AFS -o malformed bad.o target.o
```

Changing the upper 32 bits of the first relocation's `r_info` to `0xffffffff` exercises the symbol-index variant.

**Actual behavior:** The offset case slices past the output section and panics; the symbol-index case indexes past the symbol vector and panics.

**Intended behavior:** Reject the object with a deterministic diagnostic naming its path, target section, relocation index, and invalid field.

**Consequence:** Corrupt or untrusted input crashes the linker instead of producing a controlled diagnostic.

**Confidence:** High; both untrusted values reach Rust indexing operators without a preceding bound check.

### E6. Executable-stack requirements are discarded

**Severity:** Major

**Source:** `afs-ld/src/elf.rs:376-390` discards non-`SHF_ALLOC` `.note.GNU-stack` before retaining its execute requirement. Static and dynamic writers hard-code non-executable `PT_GNU_STACK` at `afs-ld/src/elf.rs:2199-2201` and `afs-ld/src/elf.rs:3484`.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > xstack.s <<'EOF'
.text
.globl _start
_start:
  sub $16,%rsp
  movb $0xc3,(%rsp)
  call *%rsp
  mov $42,%edi
  mov $60,%eax
  syscall
.section .note.GNU-stack,"x",@progbits
EOF
as --64 -o xstack.o xstack.s
$AFS -o afs-xstack xstack.o
ld -o gnu-xstack xstack.o
./afs-xstack; echo "afs=$?"
./gnu-xstack; echo "gnu=$?"
```

**Actual behavior:** The afs output requests an RW/NX stack and faults at the stack trampoline.

**Intended behavior:** Aggregate input `.note.GNU-stack` requirements; GNU output requests an executable stack and exits 42.

**Consequence:** GCC nested-function trampolines and other explicitly stack-generated code fail at runtime.

**Confidence:** High; execute intent is discarded before output program headers are fixed.

### E7. Dynamic initializer, preinitializer, and finalizer arrays lack dynamic tags

**Severity:** Critical

**Source:** The linker merges array sections, but `afs-ld/src/elf.rs:92-110` lacks the relevant dynamic-tag constants and `afs-ld/src/elf.rs:3362-3393` emits none of `DT_PREINIT_ARRAY{,SZ}`, `DT_INIT_ARRAY{,SZ}`, or `DT_FINI_ARRAY{,SZ}`. Merged output headers also describe these arrays as generic `SHT_PROGBITS`.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > init.s <<'EOF'
.data
value: .long 0
.text
ctor:
  movl $42,value(%rip)
  ret
.globl _start
_start:
  call dep
  mov value(%rip),%edi
  mov $60,%eax
  syscall
.section .init_array,"aw",@init_array
.quad ctor
EOF
cat > dep.s <<'EOF'
.text
.globl dep
.type dep,@function
dep: ret
EOF
as --64 -o init.o init.s
as --64 -o dep.o dep.s
ld -shared -soname libdep.so -o libdep.so dep.o
$AFS --dynamic-linker "$RTLD" -o afs-init init.o ./libdep.so
ld --dynamic-linker "$RTLD" -o gnu-init init.o ./libdep.so
readelf -d afs-init | grep INIT_ARRAY || true
readelf -d gnu-init | grep INIT_ARRAY
```

**Actual behavior:** Both outputs retain `libdep.so`, proving that they are dynamic links. The afs output contains the initializer-array bytes but no discoverable `INIT_ARRAY` tags. GNU `ld` emits `DT_INIT_ARRAY` and `DT_INIT_ARRAYSZ` for the same input.

**Intended behavior:** Emit the address/size tags and correct section types so the normal startup path can discover and invoke the array.

**Consequence:** Constructors/destructors silently do not run in otherwise loadable dynamic executables.

**Confidence:** High; no dynamic entry can advertise the merged arrays.

### E8. Executable-defined GNU IFUNCs are called as ordinary functions in dynamic links

**Severity:** Critical

**Source:** Dynamic symbol addressing at `afs-ld/src/elf.rs:3073-3120` returns the raw definition address and does not share the static path's IFUNC/IPLT logic. `afs-ld/src/elf.rs:3220-3226` resolves `PC32`/`PLT32` directly, and no local `R_X86_64_IRELATIVE` entries are synthesized.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > ifunc.s <<'EOF'
.text
.globl _start
_start:
  call dep
  call pick
  mov %eax,%edi
  mov $60,%eax
  syscall
.globl pick
.type pick,@gnu_indirect_function
pick:
  lea impl(%rip),%rax
  ret
impl:
  mov $42,%eax
  ret
EOF
cat > dep.s <<'EOF'
.text
.globl dep
.type dep,@function
dep: ret
EOF
as --64 -o ifunc.o ifunc.s
as --64 -o dep.o dep.s
ld -shared -soname libdep.so -o libdep.so dep.o
$AFS --dynamic-linker "$RTLD" -o afs-ifunc ifunc.o ./libdep.so
ld --dynamic-linker "$RTLD" -o gnu-ifunc ifunc.o ./libdep.so
readelf -r afs-ifunc
readelf -r gnu-ifunc
LD_LIBRARY_PATH=. ./afs-ifunc; echo "afs=$?"
LD_LIBRARY_PATH=. ./gnu-ifunc; echo "gnu=$?"
```

**Actual behavior:** The afs output has a PLT relocation for `dep` but no `R_X86_64_IRELATIVE`; the call enters the resolver as if it were `impl`, and the process exits with a pointer-derived value instead of 42. GNU emits `R_X86_64_IRELATIVE` and exits 42.

**Intended behavior:** Synthesize IPLT/GOT state and an `R_X86_64_IRELATIVE` relocation so startup resolves `pick` and calls `impl`; GNU output exits 42.

**Consequence:** Loader-accepted executables using compiler/libc IFUNC dispatch silently execute resolver semantics at each call site.

**Confidence:** High; the dynamic path has no local-IFUNC branch or IRELATIVE producer.

### E9. Explicit non-default dynamic symbol versions cannot resolve

**Severity:** Major

**Source:** `afs-ld/src/elf.rs:638-687` collapses shared-library dynsyms into a base-name `HashMap`, losing multiple version identities. `afs-ld/src/elf.rs:2563-2584` then looks up the object's exact symbol spelling, such as `answer@V1`.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > lib.s <<'EOF'
.text
foo1: mov $7,%eax; ret
foo2: mov $42,%eax; ret
.symver foo1,answer@V1
.symver foo2,answer@@V2
EOF
cat > versions.map <<'EOF'
V1 { global: answer; };
V2 { global: answer; } V1;
EOF
as --64 -o lib.o lib.s
ld -shared -soname libver.so --version-script versions.map -o libver.so lib.o
cat > use.s <<'EOF'
.symver answer_v1,answer@V1
.text
.globl _start
_start:
  call answer_v1@PLT
  mov %eax,%edi
  mov $60,%eax
  syscall
EOF
as --64 -o use.o use.s
$AFS --dynamic-linker "$RTLD" -o afs-ver use.o ./libver.so
ld --dynamic-linker "$RTLD" -o gnu-ver use.o ./libver.so
```

**Actual behavior:** `answer@V1` cannot match the provider key `answer`, so afs reports that the symbol is not exported.

**Intended behavior:** Match the exact V1 definition and emit the corresponding `VERNEED`/VERSYM requirement; GNU links the older ABI.

**Consequence:** Consumers that intentionally pin a backward-compatible DSO symbol version cannot link.

**Confidence:** High; provider parsing erases the identity that import lookup requires.

### D1. Mach-O parse diagnostics omit the input path

**Severity:** Minor

**Source:** `afs-ld/src/lib.rs:1099-1110` forwards `ReadError` from object parsing without attaching the `InputSpec` path; the error type/display contains only the low-level message.

**Reproduction:**

```sh
tmp=$(mktemp -d); cd "$tmp"
cat > good.s <<'EOF'
.text
.globl _main
_main: ret
EOF
xcrun as -arch arm64 good.s -o good.o
printf '\xcf\xfa' > bad.o
$AFS -arch arm64 good.o bad.o -o bad.out
```

**Actual behavior:** The diagnostic says only that `mach_header_64` is truncated (with needed/available byte counts), without identifying `bad.o`.

**Intended behavior:** Prefix the parse error with the input path and, where available, the failing offset/context.

**Consequence:** With many objects, users cannot identify the corrupt producer without manually bisecting inputs.

**Confidence:** High; path context is discarded at the error-conversion boundary.

## Deterministic parallel behavior

No source-level nondeterminism was confirmed. Initial object load results are sorted back into load order at `afs-ld/src/lib.rs:1041-1053`; archive-member parse results are sorted by member/index order at `afs-ld/src/resolve.rs:1234-1257`; parallel relocation chunks are consumed in deterministic handle order at `afs-ld/src/reloc/arm64.rs:293-302`; and code-signature hash chunks are joined in creation order at `afs-ld/src/synth/code_sig.rs:168-182`. Hash-map iteration encountered in the inspected output paths is generally followed by explicit ordering or does not choose semantic winners.

This is a positive source assessment, not execution proof: the disk-space failure prevented repeat-link byte comparisons. Failure diagnostics, ELF output, archives, custom segments, and different thread counts are not covered by the existing determinism test surface noted below.

## Unconfirmed concerns

These items have credible source evidence but were not promoted to confirmed discrepancies because the exact ABI consequence or minimal differential reproduction still needs validation.

- `afs-ld/src/resolve.rs:1646-1652` implements `-undefined dynamic_lookup`, `warning`, and `suppress` by promoting strong undefined references with `weak_import: true`; this may incorrectly turn required flat lookups into optional zero-valued references.
- `afs-ld/src/resolve.rs:1081-1089` derives import weakness from the provider's weak-definition flag rather than the consuming reference or `-weak_framework`, potentially reversing weak/strong bind semantics.
- `afs-ld/src/macho/writer.rs:748-802` does not appear to set `MH_WEAK_DEFINES`/`MH_BINDS_TO_WEAK`, while `afs-ld/src/macho/writer.rs:2381-2395` emits no weak-bind stream. A mixed weak-definition/interposition fixture should establish the dyld result.
- Dyld-info opcode construction in `afs-ld/src/synth/dyld_info.rs` masks segment indices into four bits. More than 15 output segments may therefore bind/rebase against the wrong segment rather than fail; a loader acceptance test is needed.
- Output symbol section ordinals are cast to `u8` near `afs-ld/src/macho/writer.rs:2174`; more than 255 output sections may corrupt `n_sect` instead of diagnosing the format limit.
- `afs-ld/src/layout.rs:407` may assign VM/file order inconsistently when a regular custom `__DATA` section follows zero-fill input, independently of M6. A loader-mapping comparison should confirm the exact swapped/zeroed bytes.
- `afs-ld/src/resolve.rs:1055-1061` silently skips Mach-O symbols with invalid string indices instead of rejecting the malformed object.
- The default dylib identity generated at `afs-ld/src/macho/writer.rs:763-773` is `@rpath/<basename>` even when no install name was supplied; compatibility with Darwin `ld`'s default output-path identity should be tested.
- ELF `SHT_GROUP`/COMDAT and `st_other` visibility are not modeled. Common C++/Rust COMDAT and hidden/protected-symbol corpora should establish duplicate-selection and preemption failures.
- `R_X86_64_DTPOFF64` uses the TP-relative `tls_offset` calculation at `afs-ld/src/elf.rs:2026-2028` and `afs-ld/src/elf.rs:3236-3238`; DTPOFF is normally module-base-relative, but a TLS runtime fixture is needed before classifying the exact emitted error.
- `-Bstatic`/`-Bdynamic` are accepted and ignored at `afs-ld/src/main.rs:199-200`, while dynamic `-l` lookup prefers `.so` at `afs-ld/src/elf.rs:526-528`. A paired `.so`/`.a` provider fixture should confirm selection.
- Shared-object parsing at `afs-ld/src/elf.rs:514-528` does not validate `e_machine` and relies on section headers rather than `PT_DYNAMIC`; test foreign-machine and section-header-stripped DSOs before assigning separate severities.
- Mach-O atom/section sizes narrow several `u64` values to `u32` in `afs-ld/src/atom.rs`; a sparse section larger than 4 GiB is likely to truncate but was impractical to validate in this environment.

## Maintainability and performance observations

- ELF static and dynamic writers duplicate most section collection, layout, TLS, symbol, and relocation logic across roughly two thousand lines in `afs-ld/src/elf.rs`. E7 and E8 demonstrate feature drift between paths, while the suspected DTPOFF issue is duplicated in both.
- ELF format detection reads each candidate file in full and processing reads it again (`afs-ld/src/main.rs:215-239`). Archive scanning reparses the complete archive (`afs-ld/src/elf.rs:1231`), and symbol/member lookup is linear (`afs-ld/src/archive.rs:246-249`, `afs-ld/src/archive.rs:534-538`). Large scientific static archives will amplify both I/O and CPU costs.
- Mach-O `ObjectInput` retains raw bytes while `ObjectFile` owns copied section, relocation, and string data (`afs-ld/src/resolve.rs:140-148`, `afs-ld/src/input.rs:20-33`), increasing peak memory during large links.
- Archive members are reopened/reparsed on extraction at `afs-ld/src/resolve.rs:1267-1279`; caching validated archive structure and external thin-member handles would reduce repeated work.
- ICF rebuilds input/object lookup state per atom and fixed-point iteration in `afs-ld/src/icf.rs`; unwind processing performs repeated linear atom/address searches. These patterns are likely superlinear on heavily atomized Fortran/C++ programs.
- Parallel relocation and signing create scoped worker threads for individual phases rather than use a reusable pool. The deterministic joins are good, but thread startup and many small chunks can dominate small/medium links.
- Mach-O finalization recalculates linkedit/code-signature plans through repeated layout/finalization passes (`afs-ld/src/macho/writer.rs:259-285`, `afs-ld/src/lib.rs:799-834`) with a fixed four-pass unwind cap and no explicit non-convergence diagnostic.
- Production does not call the available relocation validation consistently. Moving width, PC-relative, referent, and cross-atom checks to ingestion would improve both diagnostics and safety without complicating patchers.

## Test gaps

- Existing Mach-O determinism coverage should compare bytes and diagnostics across thread counts for archives, dylibs, dead strip/ICF, custom segments, malformed inputs, and both successful and failed links. Equivalent ELF determinism coverage is absent.
- No integration fixtures cover real `llvm-ar` thin archives, extensionless framework/dylib binaries, `N_INDR`, common allocation, unresolved weak binds, external absolute symbols, or standalone `-force_load`.
- CLI tests need interleaved positional/`-l`/framework inputs and provider collisions so that preserving one ordered token stream becomes an asserted contract.
- Mach-O relocation tests need custom-segment rebases, implicit bind addends, PC-relative pointer-to-GOT fields, and loader-time ASLR checks rather than only structural output checks.
- Dead-strip/ICF tests need initializer/terminator sections, section retention flags, equal-address aliases, and cross-object local section referents with different contents.
- Writer tests should assert `LC_MAIN` failure without an entry, CodeDirectory executable-segment fields against final `__TEXT`, and UUID changes when only content bytes change.
- ELF tests need semantic `--gc-sections`, conflicting section flags, `--no-as-needed`, archive-only target dispatch, bounded malformed relocations, executable-stack requests, dynamic init/fini arrays, executable-defined IFUNC, and explicit non-default version references.
- Add large-archive, high-atom-count, and repeated-incremental corpus benchmarks with peak RSS, total bytes read, archive rescans, thread count, and wall-clock baselines. Current tests do not guard the performance observations above.

## Review disposition

The most urgent cluster is silent runtime correctness: input-order reconstruction, custom-segment rebases, imported pointer addends, ICF identity, dead-strip roots, incompatible-section merging, same-address aliases, ELF DSO retention, ELF dynamic constructor metadata, and local dynamic IFUNC handling. These should block using `afs-ld` for production binaries until fixed and covered by loader-executed regression tests. The remaining major findings substantially constrain ordinary compiler-produced inputs and should be handled before broad compatibility or performance work.
