# Sprint 2: afs-as — Assembly Text Parser

## Prerequisites
Sprint 1 (instruction encoding works)

## Goals
Parse ARM64 assembly text (`.s` files) into our instruction representation. After this sprint, `afs-as` can read assembly files and convert them to an in-memory instruction stream — the bridge between text and the encoder from Sprint 1.

## Deliverables

### 1. Assembly Lexer
Tokenize ARM64 assembly text:
- **Labels**: `_main:`, `.Lloop:`
- **Instructions**: `add`, `ldr`, `b.eq`, `stp`
- **Registers**: `x0`-`x30`, `w0`-`w30`, `sp`, `xzr`, `wzr`, `d0`-`d31`, `s0`-`s31`
- **Immediates**: `#42`, `#0xff`, `#-16`
- **Addressing modes**: `[x0]`, `[x0, #16]`, `[x0, #16]!`, `[x0], #16`
- **Directives**: `.text`, `.data`, `.global`, `.align`, `.byte`, `.word`, `.quad`, `.ascii`, `.asciz`, `.space`, `.section`, `.p2align`
- **Shifts/extends**: `lsl #2`, `uxtw`, `sxtx`
- **Comments**: `//` and `/* */`
- **Separators**: `,`, newlines

### 2. Assembly Parser
Recursive descent parser for assembly syntax:

```
line       = label? (instruction | directive)? comment?
instruction = mnemonic operand (',' operand)*
operand    = register | immediate | address | label_ref | shift_expr
address    = '[' register (',' offset)? ']' ('!')?   // pre-index
           | '[' register ']' (',' offset)?           // post-index
```

Produce a structured representation:
```rust
enum AsmStatement {
    Label(String),
    Instruction(Instruction),
    Directive(Directive),
}

enum Directive {
    Text,
    Data,
    Global(String),
    Align(u32),
    Byte(Vec<u8>),
    Word(Vec<u32>),
    Quad(Vec<u64>),
    Ascii(Vec<u8>),
    Asciz(Vec<u8>),  // null-terminated
    Space(usize),
    Section(String, String),  // segment, section
}
```

### 3. Symbol Table (Assembly Level)
Track labels and their locations:
- Forward references (branch to label not yet seen)
- Local labels (`.Lxxx` — used extensively by compilers)
- Global vs local symbol distinction
- Record section membership

### 4. Error Reporting
Clear error messages with line numbers:
```
hello.s:12:5: error: unknown register 'x32'
hello.s:24:1: error: undefined label '.Lmissing'
```

## Testing Strategy

### Unit Tests
- Lex individual tokens and verify
- Parse individual instructions and verify they produce correct `Instruction` variants
- Parse addressing modes (all forms)
- Parse directives

### Round-Trip Tests
- Compile C programs with `clang -S` to produce `.s` files
- Parse them with our parser
- Verify no parse errors on real compiler output

### Integration with Sprint 1
- Parse `.s` file → instruction stream → encode each instruction → compare bytes with `as` output

### Error Tests
- Intentionally malformed assembly → verify clear error messages
- Unknown mnemonics, bad register names, missing operands

## Key Technical Notes

### ARM64 Assembly Syntax Variants
There are two main syntax styles:
- **GAS (GNU) syntax**: What `gcc -S` and `clang -S` produce on Linux
- **Apple syntax**: Slightly different directive names (`.subsections_via_symbols`, Mach-O section syntax)

We need to handle Apple syntax since we target macOS. Key differences:
- Section directives: `.section __TEXT,__text` instead of `.text` (though `.text` is usually accepted)
- Symbol attributes: `.globl` vs `.global` (accept both)
- Alignment: `.p2align` (power-of-2) vs `.align` (platform-dependent meaning)

### Instruction Mnemonics
ARM64 mnemonics are mostly unambiguous, but some have variants:
- `ldr x0, [x1]` vs `ldr x0, =label` (literal pool load — PC-relative)
- `mov x0, x1` is actually an alias for `orr x0, xzr, x1`
- Many aliases: `cmp` = `subs xzr`, `tst` = `ands xzr`, `neg` = `sub from xzr`

We should resolve aliases to canonical forms during parsing.

## Definition of Done
- Parser handles all instruction forms from Sprint 1
- Parser handles all listed directives
- Parses real `.s` files produced by `clang -S` on macOS ARM64 without errors
- Symbol table tracks all labels with forward reference resolution
- Clear error messages for malformed input
- `cargo test -p afs-as` parser tests pass
