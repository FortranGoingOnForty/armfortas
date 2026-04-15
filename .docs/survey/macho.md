# Mach-O Object File Format Overview

Reference: Apple's Mach-O documentation, /usr/include/mach-o/*.h

## Structure

A Mach-O object file (MH_OBJECT) has this layout:
```
┌─────────────────────┐
│  Mach-O Header      │  28 bytes (32-bit) or 32 bytes (64-bit)
├─────────────────────┤
│  Load Commands      │  variable size, describes file structure
├─────────────────────┤
│  Section Data       │  code bytes, data bytes
├─────────────────────┤
│  Relocation Entries │  fixups for the linker
├─────────────────────┤
│  Symbol Table       │  nlist_64 entries
├─────────────────────┤
│  String Table       │  null-terminated symbol names
└─────────────────────┘
```

## Mach-O Header (mach_header_64)
```c
struct mach_header_64 {
    uint32_t magic;       // 0xFEEDFACF (64-bit)
    int32_t  cputype;     // CPU_TYPE_ARM64 = 0x0100000C
    int32_t  cpusubtype;  // CPU_SUBTYPE_ARM64_ALL = 0
    uint32_t filetype;    // MH_OBJECT = 1
    uint32_t ncmds;       // number of load commands
    uint32_t sizeofcmds;  // total size of load commands
    uint32_t flags;       // MH_SUBSECTIONS_VIA_SYMBOLS = 0x2000
    uint32_t reserved;    // 0
};
```

## Load Commands We Need

### LC_SEGMENT_64 (0x19)
Contains the sections:
```c
struct segment_command_64 {
    uint32_t cmd;         // LC_SEGMENT_64
    uint32_t cmdsize;     // sizeof this + sizeof sections
    char     segname[16]; // "" for object files
    uint64_t vmaddr;      // 0 for object files
    uint64_t vmsize;
    uint64_t fileoff;     // offset to section data
    uint64_t filesize;
    // ...
    uint32_t nsects;      // number of sections
};
```

### Sections (section_64)
```c
struct section_64 {
    char     sectname[16];  // "__text", "__data", etc.
    char     segname[16];   // "__TEXT", "__DATA", etc.
    uint64_t addr;          // 0 for object files
    uint64_t size;
    uint32_t offset;        // file offset to section data
    uint32_t align;         // power of 2
    uint32_t reloff;        // file offset to relocations
    uint32_t nreloc;        // number of relocations
    uint32_t flags;         // section type + attributes
    // ...
};
```

**Sections we emit:**
- `__TEXT,__text` — machine code (flags: S_REGULAR | S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS)
- `__DATA,__data` — initialized data (flags: S_REGULAR)
- `__DATA,__bss` — uninitialized data (flags: S_ZEROFILL)
- `__TEXT,__cstring` — C string literals
- `__DATA,__const` — constant data (float literals, etc.)

### LC_SYMTAB (0x2)
Points to symbol table and string table:
```c
struct symtab_command {
    uint32_t cmd;
    uint32_t cmdsize;
    uint32_t symoff;    // offset to nlist_64 array
    uint32_t nsyms;     // number of symbols
    uint32_t stroff;    // offset to string table
    uint32_t strsize;   // string table size
};
```

### LC_DYSYMTAB (0xB)
Dynamic symbol table info. Required even for static linking on macOS:
```c
struct dysymtab_command {
    uint32_t cmd;
    uint32_t cmdsize;
    uint32_t ilocalsym;   // index of first local symbol
    uint32_t nlocalsym;
    uint32_t iextdefsym;  // index of first externally defined symbol
    uint32_t nextdefsym;
    uint32_t iundefsym;   // index of first undefined symbol
    uint32_t nundefsym;
    // ... rest can be 0 for object files
};
```

### LC_BUILD_VERSION (0x32)
Required on modern macOS:
```c
struct build_version_command {
    uint32_t cmd;
    uint32_t cmdsize;
    uint32_t platform;   // PLATFORM_MACOS = 1
    uint32_t minos;      // minimum OS version (e.g., 14.0.0)
    uint32_t sdk;        // SDK version
    uint32_t ntools;     // 0 for us
};
```

## Symbol Table (nlist_64)
```c
struct nlist_64 {
    uint32_t n_strx;    // offset into string table
    uint8_t  n_type;    // type flags
    uint8_t  n_sect;    // section number (1-based) or NO_SECT
    uint16_t n_desc;    // additional info
    uint64_t n_value;   // value (usually address/offset)
};
```

**n_type flags:**
- `N_UNDF` (0x0) — undefined (external reference)
- `N_ABS` (0x2) — absolute symbol
- `N_SECT` (0xE) — defined in section n_sect
- `N_EXT` (0x1) — external (global) — OR'd with above

## ARM64 Relocations
```
ARM64_RELOC_UNSIGNED        = 0  // absolute 64-bit pointer
ARM64_RELOC_SUBTRACTOR      = 1  // paired with UNSIGNED for relative refs
ARM64_RELOC_BRANCH26        = 2  // B and BL instructions (26-bit offset)
ARM64_RELOC_PAGE21          = 3  // ADRP instruction (21-bit page offset)
ARM64_RELOC_PAGEOFF12       = 4  // ADD/LDR instruction (12-bit page offset)
ARM64_RELOC_GOT_LOAD_PAGE21 = 5  // GOT entry via ADRP
ARM64_RELOC_GOT_LOAD_PAGEOFF12 = 6  // GOT entry via LDR
ARM64_RELOC_POINTER_TO_GOT  = 7  // 32-bit GOT delta
ARM64_RELOC_TLVP_LOAD_PAGE21 = 8  // TLV page
ARM64_RELOC_TLVP_LOAD_PAGEOFF12 = 9  // TLV offset
ARM64_RELOC_ADDEND          = 10 // addend for next relocation
```

**Most common pattern for global access:**
```
ADRP X0, _symbol@PAGE        → ARM64_RELOC_PAGE21
ADD  X0, X0, _symbol@PAGEOFF → ARM64_RELOC_PAGEOFF12
```

**For function calls:**
```
BL _function                  → ARM64_RELOC_BRANCH26
```

## Relocation Entry Format
```c
struct relocation_info {
    int32_t  r_address;    // offset in section
    uint32_t r_symbolnum:24,  // symbol table index (if r_extern=1)
             r_pcrel:1,       // PC-relative?
             r_length:2,      // 0=byte, 1=word, 2=long, 3=quad
             r_extern:1,      // 1=symbol, 0=section
             r_type:4;        // relocation type
};
```

## Verification Tools
- `otool -l file.o` — dump load commands
- `otool -t file.o` — dump text section (code bytes)
- `otool -tv file.o` — disassemble text section
- `otool -r file.o` — dump relocations
- `nm file.o` — dump symbol table
- `size file.o` — section sizes
