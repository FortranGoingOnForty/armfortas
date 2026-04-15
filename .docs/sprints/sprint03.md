# Sprint 3: afs-as — Mach-O Object Emission

## Prerequisites
Sprint 1 (encoding), Sprint 2 (parsing)

## Goals
Complete the assembler by writing Mach-O 64-bit object files. After this sprint, `afs-as` is a functional standalone ARM64 assembler for macOS: `.s` in, `.o` out, linkable with `ld`.

## Deliverables

### 1. Mach-O Writer
Implement the Mach-O 64-bit object file format from scratch.

**File structure we need to emit:**
```
┌─────────────────────┐
│ Mach-O Header       │  (magic, cputype, filetype, ncmds, sizeofcmds)
├─────────────────────┤
│ LC_SEGMENT_64       │  (segment containing sections)
│  ├─ __TEXT,__text   │  (machine code)
│  ├─ __DATA,__data   │  (initialized data)
│  └─ __DATA,__bss    │  (uninitialized data, zero-fill)
├─────────────────────┤
│ LC_SYMTAB           │  (symbol table command)
├─────────────────────┤
│ LC_DYSYMTAB         │  (dynamic symbol table — required by ld)
├─────────────────────┤
│ Section contents    │  (actual bytes: code + data)
├─────────────────────┤
│ Relocation entries  │  (fixups for unresolved references)
├─────────────────────┤
│ Symbol table        │  (nlist_64 entries)
├─────────────────────┤
│ String table        │  (null-terminated symbol names)
└─────────────────────┘
```

**Key Mach-O constants:**
- Magic: `0xFEEDFACF` (64-bit)
- CPU type: `CPU_TYPE_ARM64` (0x0100000C)
- CPU subtype: `CPU_SUBTYPE_ARM64_ALL` (0x00000000)
- File type: `MH_OBJECT` (0x1)

### 2. Relocation Handling
ARM64 Mach-O relocations are critical for linking:

- `ARM64_RELOC_BRANCH26` — for B and BL to external symbols
- `ARM64_RELOC_PAGE21` — ADRP (page-relative addressing, upper 21 bits)
- `ARM64_RELOC_PAGEOFF12` — ADD/LDR offset within page (lower 12 bits)
- `ARM64_RELOC_GOT_LOAD_PAGE21` — GOT entry page
- `ARM64_RELOC_GOT_LOAD_PAGEOFF12` — GOT entry offset
- `ARM64_RELOC_SUBTRACTOR` + `ARM64_RELOC_UNSIGNED` — for data references

ADRP+ADD/LDR pairs are the standard ARM64 pattern for global data access. Getting relocations right is essential for linking.

### 3. Symbol Emission
Convert our symbol table to Mach-O `nlist_64` entries:
- External symbols (`.global` labels): `N_EXT | N_SECT`
- Local symbols: `N_SECT`
- Undefined symbols (references to external functions): `N_UNDF | N_EXT`
- Section ordinals (which section does the symbol live in)

### 4. CLI for afs-as
```
afs-as input.s -o output.o        # assemble
afs-as --help                      # usage
afs-as --version                   # version info
```

Minimal, focused. No flags we don't need yet.

## Testing Strategy

### The Ultimate Test
```bash
# Write a minimal ARM64 assembly program
cat > hello.s << 'EOF'
.global _main
.align 4

_main:
    // write(1, msg, 14)
    mov x0, #1          // stdout
    adrp x1, msg@PAGE
    add x1, x1, msg@PAGEOFF
    mov x2, #14          // length
    mov x16, #4          // write syscall
    svc #0x80

    // exit(0)
    mov x0, #0
    mov x16, #1          // exit syscall
    svc #0x80

.data
msg: .asciz "Hello, World!\n"
EOF

# Assemble with our assembler
afs-as hello.s -o hello.o

# Link with system linker
ld hello.o -o hello -lSystem -syslibroot $(xcrun --show-sdk-path) -e _main

# Run
./hello  # should print "Hello, World!"
```

### Verification Tests
- `otool -l hello.o` — verify Mach-O structure (segments, sections, load commands)
- `otool -t hello.o` — verify code bytes match our encoding
- `nm hello.o` — verify symbol table
- `otool -r hello.o` — verify relocations

### Comparison Tests
For a set of test `.s` files:
1. Assemble with `as` (Apple's assembler): `as test.s -o test_ref.o`
2. Assemble with `afs-as`: `afs-as test.s -o test_ours.o`
3. Compare code sections byte-for-byte
4. Compare symbol tables
5. Link both and verify identical behavior

### Edge Cases
- Empty sections
- Large immediates requiring ADRP+ADD pairs
- Data alignment requirements
- Multiple sections
- Forward references to labels in later sections

## Key Technical Notes

### macOS Linker Requirements
Apple's `ld` is picky:
- Requires `LC_DYSYMTAB` even for static linking
- Object files must have correct section alignment
- The `-lSystem` flag links libSystem (provides `_exit`, `_write`, etc. via syscall wrappers)
- Modern macOS requires `-syslibroot` pointing to the SDK

### Page Size
macOS on ARM64 uses 16KB pages (not 4KB like x86). ADRP instructions use 4KB page granularity in encoding but the OS uses 16KB pages. This doesn't affect object file emission but matters for understanding ADRP relocations.

### Assembler as Library
The `afs-as` crate exposes both:
- `pub fn assemble_file(path: &str) -> Result<ObjectFile>` — for CLI use
- `pub fn assemble_instructions(insts: &[Instruction], symbols: &SymbolTable) -> Result<ObjectFile>` — for compiler use (no text parsing needed)
- `pub fn write_macho(obj: &ObjectFile, path: &str) -> Result<()>` — emit to disk

The compiler will call the library API directly, skipping the text parser entirely.

## Definition of Done
- `afs-as hello.s -o hello.o && ld hello.o -o hello -lSystem && ./hello` prints "Hello, World!"
- Mach-O structure verified with `otool`
- Relocations correct for ADRP+ADD patterns
- Symbol table correct (verified with `nm`)
- Code bytes identical to Apple's `as` for test programs
- Library API usable without text parsing
- `cargo test -p afs-as` all pass
