# Sprint 26: Runtime — Intrinsics (Math, Array, System)

## Prerequisites
Sprint 22 (array descriptors), Sprint 23 (strings)

## Goals
Implement all remaining intrinsic procedures: mathematical functions, array manipulation, system queries, and numeric inquiry. After this sprint, Fortran programs can use the full standard library of built-in functions.

## Deliverables

### 1. Mathematical Intrinsics
**Inlined by codegen** (emit ARM64 FPU instructions directly):
- `abs`, `sign`, `max`, `min`, `mod`, `modulo`
- `int`, `nint`, `real`, `dble`, `cmplx`
- `aimag`, `conjg` (complex)
- ARM64 FPU: `fsqrt`, `fabs`, `fneg` → direct instruction

**Runtime library** (call libm or our own implementation):
```rust
// Trigonometric
__afs_sin_f32, __afs_sin_f64
__afs_cos_f32, __afs_cos_f64
__afs_tan_f32, __afs_tan_f64
__afs_asin_f64, __afs_acos_f64, __afs_atan_f64, __afs_atan2_f64

// Hyperbolic
__afs_sinh_f64, __afs_cosh_f64, __afs_tanh_f64
__afs_asinh_f64, __afs_acosh_f64, __afs_atanh_f64

// Exponential/Logarithmic
__afs_exp_f64, __afs_log_f64, __afs_log10_f64

// Power
__afs_pow_f64_f64          // real ** real
__afs_pow_f64_i64          // real ** integer (special case, more precise)

// Bessel functions (F2008)
__afs_bessel_j0, __afs_bessel_j1, __afs_bessel_jn
__afs_bessel_y0, __afs_bessel_y1, __afs_bessel_yn

// Error function
__afs_erf_f64, __afs_erfc_f64

// Gamma
__afs_gamma_f64, __afs_log_gamma_f64

// Hypotenuse (F2008)
__afs_hypot_f64
```

For complex math: `sin(z) = sin(x)cosh(y) + i*cos(x)sinh(y)`, etc. Implement complex variants of all trig/exp/log functions.

### 2. Bit Manipulation Intrinsics
**All inlined** — these map directly to ARM64 instructions:
```
iand(a, b)   → AND
ior(a, b)    → ORR
ieor(a, b)   → EOR
not(a)       → MVN
ishft(a, n)  → LSL/LSR (depending on sign of n)
ishftc(a, n, size) → rotate (ARM64: ROR/EXTR)
btest(a, n)  → TST bit n, CSET
ibset(a, n)  → ORR with mask
ibclr(a, n)  → BIC (bit clear)
ibits(a, pos, len) → UBFX (bit field extract)
mvbits(from, frompos, len, to, topos) → BFI
leadz(a)     → CLZ
trailz(a)    → RBIT + CLZ
popcount(a)  → CNT (SIMD) or Hamming weight sequence
```

### 3. Array Intrinsics (Runtime)
These operate on array descriptors and are substantial runtime functions:

```rust
// MATMUL: matrix multiplication
__afs_matmul_real4(a: &ArrayDesc, b: &ArrayDesc, result: &mut ArrayDesc)
__afs_matmul_real8(...)
__afs_matmul_int4(...)
// Implementation: triple loop (optimize later with SIMD in Sprint 29)

// RESHAPE: change array shape
__afs_reshape(source: &ArrayDesc, shape: &[i64], pad: Option<&ArrayDesc>, order: Option<&[i64]>, result: &mut ArrayDesc)

// PACK: gather elements by mask
__afs_pack(array: &ArrayDesc, mask: &ArrayDesc, vector: Option<&ArrayDesc>, result: &mut ArrayDesc)

// UNPACK: scatter elements by mask
__afs_unpack(vector: &ArrayDesc, mask: &ArrayDesc, field: &ArrayDesc, result: &mut ArrayDesc)

// SPREAD: replicate array along dimension
__afs_spread(source: &ArrayDesc, dim: i32, ncopies: i64, result: &mut ArrayDesc)

// TRANSPOSE: matrix transpose
__afs_transpose(source: &ArrayDesc, result: &mut ArrayDesc)

// CSHIFT: circular shift
__afs_cshift(array: &ArrayDesc, shift: i64, dim: i32, result: &mut ArrayDesc)

// EOSHIFT: end-off shift
__afs_eoshift(array: &ArrayDesc, shift: i64, boundary: Option<*const u8>, dim: i32, result: &mut ArrayDesc)

// MERGE: element-wise select
__afs_merge(tsource: &ArrayDesc, fsource: &ArrayDesc, mask: &ArrayDesc, result: &mut ArrayDesc)
```

### 4. Array Reduction Intrinsics
```rust
// SUM, PRODUCT, MAXVAL, MINVAL — with optional DIM and MASK
__afs_sum_real8(array: &ArrayDesc, dim: i32, mask: Option<&ArrayDesc>) -> f64  // scalar result
__afs_sum_real8_dim(array: &ArrayDesc, dim: i32, mask: Option<&ArrayDesc>, result: &mut ArrayDesc)  // array result

// COUNT
__afs_count(mask: &ArrayDesc, dim: i32) -> i64

// ANY, ALL
__afs_any(mask: &ArrayDesc, dim: i32) -> i32
__afs_all(mask: &ArrayDesc, dim: i32) -> i32

// MAXLOC, MINLOC — return index of max/min element
__afs_maxloc(array: &ArrayDesc, dim: i32, mask: Option<&ArrayDesc>, back: bool, result: &mut ArrayDesc)
__afs_minloc(...)

// DOT_PRODUCT
__afs_dot_product_real8(a: &ArrayDesc, b: &ArrayDesc) -> f64
```

### 5. Array Query Intrinsics
**Inlined** (read from descriptor):
```
size(a)        → read descriptor bounds, compute product
size(a, dim)   → upper - lower + 1 for that dim
shape(a)       → array of sizes per dimension
lbound(a)      → array of lower bounds
ubound(a)      → array of upper bounds
rank(a)        → read descriptor rank field
allocated(a)   → read descriptor flag
```

### 6. Numeric Inquiry Intrinsics
**Compile-time constants** (inlined):
```
huge(x)       → largest representable value for x's kind
tiny(x)       → smallest positive normal value
epsilon(x)    → smallest value such that 1.0 + epsilon /= 1.0
precision(x)  → decimal digits of precision
range(x)      → decimal exponent range
digits(x)     → number of significant binary digits
radix(x)      → always 2
bit_size(x)   → 8, 16, 32, or 64
kind(x)       → kind parameter value
```

### 7. System Intrinsics (Runtime)
```rust
// SYSTEM_CLOCK
__afs_system_clock(count: *mut i64, count_rate: *mut i64, count_max: *mut i64)
// Uses clock_gettime(CLOCK_MONOTONIC) on macOS

// CPU_TIME
__afs_cpu_time(time: *mut f64)
// Uses clock() or getrusage()

// DATE_AND_TIME
__afs_date_and_time(
    date: *mut u8, date_len: i64,          // YYYYMMDD
    time: *mut u8, time_len: i64,          // hhmmss.sss
    zone: *mut u8, zone_len: i64,          // +hhmm
    values: *mut i32,                       // 8-element array
)

// COMMAND_ARGUMENT_COUNT
__afs_command_argument_count() -> i32

// GET_COMMAND_ARGUMENT
__afs_get_command_argument(number: i32, value: *mut u8, value_len: i64, length: *mut i32, status: *mut i32)

// GET_COMMAND
__afs_get_command(command: *mut u8, cmd_len: i64, length: *mut i32, status: *mut i32)

// GET_ENVIRONMENT_VARIABLE
__afs_get_environment_variable(name: *const u8, name_len: i64, value: *mut u8, value_len: i64, length: *mut i32, status: *mut i32, trim_name: i32)

// EXECUTE_COMMAND_LINE (F2008)
__afs_execute_command_line(command: *const u8, cmd_len: i64, wait: i32, exitstat: *mut i32, cmdstat: *mut i32, cmdmsg: *mut u8, cmdmsg_len: i64)
```

## Testing Strategy

### Math Tests
For each math function, test against known values:
- `sin(0) = 0`, `sin(pi/2) = 1`
- `exp(0) = 1`, `log(1) = 0`
- `atan2(1, 1) = pi/4`
- Edge cases: `sin(huge)`, `exp(big)`, `log(0)`, `sqrt(-1)`

### Bit Manipulation Tests
- Verify against manual bit calculations
- `iand(B'1100', B'1010') = B'1000'`
- `leadz(1) = 63` (for 64-bit integer)
- `popcount(B'1010_1010') = 4`

### Array Intrinsic Tests
- `matmul` against hand-calculated 3x3 matrix product
- `reshape` with various shape/pad combinations
- `sum`, `product`, `maxval` with and without DIM
- `pack`/`unpack` round-trip
- `transpose` of rectangular matrix

### System Intrinsic Tests
- `system_clock` returns increasing values
- `cpu_time` before/after computation shows elapsed time
- `date_and_time` returns valid date/time
- `command_argument_count` matches argc

## Definition of Done
- All F2018 mathematical intrinsics implemented
- All bit manipulation intrinsics implemented (inlined)
- All array transformation intrinsics implemented
- All array reduction intrinsics implemented (with DIM and MASK)
- All numeric inquiry intrinsics implemented (compile-time)
- All system intrinsics implemented
- Comprehensive test coverage per intrinsic
- `cargo test` intrinsic tests pass
