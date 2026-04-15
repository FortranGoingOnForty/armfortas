# Sprint 15: IR Design & Basic Construction

## Prerequisites
Sprint 14 (semantic analysis complete — fully typed AST available)

## Goals
Design our intermediate representation (IR) and implement basic AST→IR lowering for simple programs. The IR is the hinge point of the compiler: the frontend produces it, the backend consumes it, and optimizations operate on it. Getting the design right here determines the quality of everything downstream.

## Deliverables

### 1. IR Design: SSA Form
We use Static Single Assignment (SSA) form: every value is defined exactly once, and phi nodes merge values at control flow join points.

```rust
struct Module {
    name: String,
    globals: Vec<Global>,
    functions: Vec<Function>,
    types: Vec<TypeDef>,
}

struct Function {
    name: String,
    params: Vec<Param>,
    return_type: IrType,
    blocks: Vec<BasicBlock>,
    entry: BlockId,
}

struct BasicBlock {
    id: BlockId,
    params: Vec<BlockParam>,   // instead of phi nodes — block parameters
    instructions: Vec<Inst>,
    terminator: Terminator,
}

struct Inst {
    id: ValueId,
    kind: InstKind,
    type_: IrType,
    span: Span,                // source location for debug info
}
```

### 2. IR Instruction Set
```rust
enum InstKind {
    // Constants
    ConstInt(i64, IntWidth),
    ConstFloat(f64, FloatWidth),
    ConstBool(bool),
    ConstString(Vec<u8>),
    Undef(IrType),

    // Arithmetic (integer)
    IAdd(ValueId, ValueId),
    ISub(ValueId, ValueId),
    IMul(ValueId, ValueId),
    IDiv(ValueId, ValueId),
    IMod(ValueId, ValueId),
    INeg(ValueId),
    // Arithmetic (float)
    FAdd(ValueId, ValueId),
    FSub(ValueId, ValueId),
    FMul(ValueId, ValueId),
    FDiv(ValueId, ValueId),
    FNeg(ValueId),
    FPow(ValueId, ValueId),

    // Comparison
    ICmp(CmpOp, ValueId, ValueId),
    FCmp(CmpOp, ValueId, ValueId),

    // Logic
    And(ValueId, ValueId),
    Or(ValueId, ValueId),
    Not(ValueId),

    // Bitwise
    BitAnd(ValueId, ValueId),
    BitOr(ValueId, ValueId),
    BitXor(ValueId, ValueId),
    Shl(ValueId, ValueId),
    Shr(ValueId, ValueId),     // arithmetic shift right

    // Conversions
    IntToFloat(ValueId, FloatWidth),
    FloatToInt(ValueId, IntWidth),
    FloatExtend(ValueId, FloatWidth),
    FloatTrunc(ValueId, FloatWidth),
    IntExtend(ValueId, IntWidth, bool),    // bool = signed?
    IntTrunc(ValueId, IntWidth),

    // Memory
    Alloca(IrType),                        // stack allocation
    Load(ValueId),                          // load from address
    Store(ValueId, ValueId),                // store value to address
    GetElementPtr(ValueId, Vec<ValueId>),   // address computation

    // Function calls
    Call(FuncRef, Vec<ValueId>),
    RuntimeCall(RuntimeFunc, Vec<ValueId>), // calls into libarmfortas_rt

    // Aggregate operations
    ExtractField(ValueId, u32),
    InsertField(ValueId, u32, ValueId),
}

enum Terminator {
    Return(Option<ValueId>),
    Branch(BlockId, Vec<ValueId>),              // unconditional, with block args
    CondBranch(ValueId, BlockId, Vec<ValueId>, BlockId, Vec<ValueId>),
    Switch(ValueId, Vec<(i64, BlockId)>, BlockId),  // select case
    Unreachable,
}
```

### 3. IR Type System
```rust
enum IrType {
    Void,
    Bool,
    Int(IntWidth),           // i8, i16, i32, i64
    Float(FloatWidth),       // f32, f64
    Ptr(Box<IrType>),        // pointer to T
    Array(Box<IrType>, u64), // [T; N] — fixed size
    Struct(StructId),        // named struct type
    FuncPtr(FuncSig),        // procedure pointer
}

// Fortran-specific compound types lowered to IR structs:
// - Array descriptor → struct { ptr, rank, dims: [{lower, upper, stride}] }
// - Character → struct { ptr, len }
// - Allocatable → descriptor with allocated flag
```

### 4. Basic AST → IR Lowering

Lower simple programs:
```fortran
program simple
    integer :: x, y, z
    x = 10
    y = 20
    z = x + y
    print *, z
end program
```

Becomes:
```
function @main() -> void {
entry:
    %x = alloca i32
    %y = alloca i32
    %z = alloca i32
    store i32 10, %x
    store i32 20, %y
    %t0 = load %x
    %t1 = load %y
    %t2 = iadd %t0, %t1
    store %t2, %z
    %t3 = load %z
    call @__afs_print_int(%t3)
    return
}
```

### 5. IR Printer
A textual representation for debugging — print IR to stdout with `-emit-ir` flag:
```
module simple
  func @main() -> void {
    bb0:
      %0 = const_int 10 : i32
      %1 = const_int 20 : i32
      %2 = iadd %0, %1 : i32
      call @__afs_print_int(%2)
      ret void
  }
```

### 6. IR Verifier
A pass that validates IR well-formedness:
- Every use of a value is dominated by its definition
- Block parameters match branch arguments
- Types consistent (can't `iadd` a float)
- Every block has exactly one terminator
- Entry block has no predecessors

Run the verifier after every IR transformation to catch bugs early.

## Testing Strategy

### IR Construction Tests
Build IR programmatically (not from AST), verify structure with the verifier.

### Lowering Tests
Lower simple Fortran programs to IR, print the IR, verify it matches expected output:
- Integer arithmetic
- Real arithmetic
- Variable assignment and use
- Simple print statements (via runtime call)

### Round-Trip: Parse → Sema → IR → Print
End-to-end from source to IR text for simple programs.

### Verifier Tests
- Construct deliberately invalid IR → verifier catches it
- Construct valid IR → verifier accepts it

## Key Technical Notes

### Why Block Parameters Instead of Phi Nodes
Traditional SSA uses phi nodes at the top of blocks. We use block parameters instead (inspired by MLIR and cranelift): when branching to a block, you pass values as arguments. This is equivalent to phi nodes but easier to work with during construction and transformation.

### Fortran-Specific IR Considerations
- **Array descriptors** are first-class — they're not just pointers. The IR must carry descriptor operations.
- **Character strings** have runtime-determined length — they lower to {pointer, length} pairs.
- **Implicit deallocation** — allocatable variables are deallocated when they go out of scope. The IR must insert deallocation calls at scope exits.

## Definition of Done
- IR data structures defined and documented
- IR printer produces readable text
- IR verifier catches malformed IR
- Simple Fortran programs (integer arithmetic, assignments, print) lower to valid IR
- `-emit-ir` flag works in the driver
- IR verifier passes on all generated IR
- `cargo test` IR tests pass
