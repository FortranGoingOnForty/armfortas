# Sprint 18: HELLO WORLD MILESTONE (End-to-End Pipeline)

## Prerequisites
Sprint 17 (instruction selection), Sprint 3 (assembler), Sprint 4 (preprocessor), Sprint 5 (lexer), Sprint 11 (parser complete), Sprint 14 (semantic analysis), Sprint 16 (IR lowering)

All prior sprints converge here.

## Goals
Wire the entire pipeline together and compile our first program:

```fortran
program hello
    print *, 'Hello, World!'
end program
```

```bash
$ afs hello.f90 -o hello
$ ./hello
Hello, World!
```

This is the first time source code goes in one end and a running binary comes out the other. Every component we've built — preprocessor, lexer, parser, semantic analyzer, IR, codegen, assembler — works together for the first time.

## Deliverables

### 1. Driver (Compilation Orchestrator)
The driver connects all phases:

```rust
fn compile(input: &Path, output: &Path, opts: &Options) -> Result<()> {
    // 1. Read source
    let source = fs::read_to_string(input)?;
    
    // 2. Preprocess
    let preprocessed = preprocess(&source, input, &opts.preproc)?;
    
    // 3. Lex
    let tokens = lex(&preprocessed, opts.source_form)?;
    
    // 4. Parse
    let ast = parse(&tokens)?;
    
    // 5. Semantic analysis
    let typed_ast = analyze(&ast, &opts.std)?;
    
    // 6. Lower to IR
    let ir = lower_to_ir(&typed_ast)?;
    verify_ir(&ir)?;
    
    // 7. Instruction selection
    let mir = select_instructions(&ir)?;
    
    // 8. Register allocation (naive for now)
    let allocated = allocate_registers_naive(&mir)?;
    
    // 9. Emit machine code
    let object = assemble_from_mir(&allocated)?;
    
    // 10. Write object file
    write_macho(&object, &obj_path)?;
    
    // 11. Link
    link(&obj_path, output)?;
    
    Ok(())
}
```

### 2. Minimal Runtime Stub
For "Hello, World!" we need exactly one runtime function:

```rust
// In libarmfortas_rt:
// List-directed PRINT for a character string
#[no_mangle]
pub extern "C" fn __afs_print_star_string(ptr: *const u8, len: i64) {
    let s = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let s = std::str::from_utf8(s).unwrap_or("<invalid utf8>");
    println!(" {}", s);  // Fortran list-directed output has leading space
}

// And the Fortran program needs an entry point:
#[no_mangle]
pub extern "C" fn __afs_program_init() {
    // nothing yet — will set up I/O units, signal handlers, etc.
}

#[no_mangle]
pub extern "C" fn __afs_program_finalize() {
    // nothing yet — will flush I/O, run finalizers
}
```

The generated `_main` calls `__afs_program_init`, then the user's program body, then `__afs_program_finalize`.

### 3. Linker Integration
```bash
# What the driver runs internally:
ld hello.o \
   -L/path/to/libarmfortas_rt \
   -larmfortas_rt \
   -lSystem \
   -syslibroot $(xcrun --show-sdk-path) \
   -e _main \
   -o hello
```

The runtime is a static library. We link it into every binary.

### 4. CLI (Minimal)
```bash
afs hello.f90 -o hello         # compile + link
afs -c hello.f90               # compile to .o only
afs -S hello.f90               # emit assembly
afs -E hello.f90               # preprocess only
afs --emit-ir hello.f90        # emit our IR (for debugging)
afs --emit-ast hello.f90       # emit AST (for debugging)
```

### 5. Test Programs
Beyond "Hello, World!", compile and run:

```fortran
! test_arithmetic.f90
program arithmetic
    integer :: a, b, c
    a = 10
    b = 20
    c = a + b
    print *, c        ! should print 30
end program

! test_real.f90
program real_math
    real :: x, y
    x = 3.14
    y = x * 2.0
    print *, y        ! should print ~6.28
end program

! test_if.f90
program test_if
    integer :: x
    x = 42
    if (x > 0) then
        print *, 'positive'
    else
        print *, 'non-positive'
    end if
end program
```

### 6. Error Handling
When compilation fails at any stage, print a clear error and exit with non-zero status:
```
hello.f90:3:5: error: undefined variable 'xyz'
  3 |     print *, xyz
    |              ^^^
```

## Testing Strategy

### The Big Test
```bash
echo 'program hello; print *, "Hello, World!"; end program' > /tmp/hello.f90
afs /tmp/hello.f90 -o /tmp/hello
/tmp/hello
# Output: Hello, World!
```

If this works, we have a compiler.

### Progressive Test Suite
Compile and run each test program, verify output:
1. Hello World → prints "Hello, World!"
2. Integer arithmetic → prints correct result
3. Real arithmetic → prints correct result (within tolerance)
4. If/else → prints correct branch
5. DO loop → prints correct iterations

### Phase Isolation Tests
- `-E` produces correct preprocessed output
- `-S` produces valid assembly (assembleable by both `as` and `afs-as`)
- `-c` produces valid object file (verifiable with `otool`)
- `--emit-ir` produces readable IR
- `--emit-ast` produces readable AST dump

### Negative Tests
- Syntax errors produce helpful messages
- Type errors caught
- Undeclared variables caught (with `implicit none`)

## Key Technical Notes

### Entry Point on macOS
macOS ARM64 binaries need `_main` as the entry point. Our generated code:
```asm
.global _main
_main:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    bl __afs_program_init
    bl __afs_user_main         ; the user's program body
    bl __afs_program_finalize
    mov x0, #0                 ; exit code 0
    ldp x29, x30, [sp], #16
    ret
```

### Runtime Library Build
The runtime is built as part of `cargo build` — it's a Rust static library that gets archived into `libarmfortas_rt.a`. The compiler knows where to find it (embedded path or `-L` flag).

## Definition of Done
- `afs hello.f90 -o hello && ./hello` prints "Hello, World!"
- At least 5 test programs compile and run correctly
- All pipeline phases work end-to-end
- `-E`, `-S`, `-c`, `--emit-ir`, `--emit-ast` flags work
- Error messages are clear with source locations
- Runtime library builds and links correctly
- **This is a working compiler.** Everything after this is making it handle more Fortran.
