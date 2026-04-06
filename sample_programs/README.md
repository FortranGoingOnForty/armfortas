# sample_programs

Sixteen ARM64 assembly programs for `afs-as`, collected in one root-level directory so it is easy to poke at the assembler without hunting through test fixtures.

## Build `afs-as`

Run this from the repo root:

```bash
cargo build -p afs-as
```

## Assemble, Link, And Run One Sample

This example assembles and runs `02_hello_world.s` without leaving generated files in the repo:

```bash
SDKROOT=$(xcrun --show-sdk-path)
target/debug/afs-as sample_programs/02_hello_world.s -o /tmp/02_hello_world.o
ld /tmp/02_hello_world.o -o /tmp/02_hello_world -lSystem -syslibroot "$SDKROOT" -e _main
/tmp/02_hello_world
```

For samples that report their result through the process exit code, check it with:

```bash
echo $?
```

## Assemble And Link Any Sample

Replace `NAME` with a file stem like `09_array_sum` or `15_fp_sqrt`:

```bash
SDKROOT=$(xcrun --show-sdk-path)
NAME=09_array_sum
target/debug/afs-as "sample_programs/$NAME.s" -o "/tmp/$NAME.o"
ld "/tmp/$NAME.o" -o "/tmp/$NAME" -lSystem -syslibroot "$SDKROOT" -e _main
"/tmp/$NAME"
echo $?
```

`16_snake_live.s` is a live terminal program, so run it directly in an interactive terminal instead of expecting a useful exit-code script.

## Assemble Everything

```bash
mkdir -p /tmp/afs-samples
for src in sample_programs/*.s; do
  name=$(basename "${src%.s}")
  target/debug/afs-as "$src" -o "/tmp/afs-samples/$name.o"
done
```

## Assemble, Link, And Run Everything

```bash
mkdir -p /tmp/afs-samples
SDKROOT=$(xcrun --show-sdk-path)

for src in sample_programs/*.s; do
  name=$(basename "${src%.s}")
  target/debug/afs-as "$src" -o "/tmp/afs-samples/$name.o"
  ld "/tmp/afs-samples/$name.o" -o "/tmp/afs-samples/$name" -lSystem -syslibroot "$SDKROOT" -e _main
  printf "%s: " "$name"
  "/tmp/afs-samples/$name"
  status=$?
  printf "exit=%s\n" "$status"
done
```

## Live Snake

```bash
SDKROOT=$(xcrun --show-sdk-path)
target/debug/afs-as sample_programs/16_snake_live.s -o /tmp/16_snake_live.o
ld /tmp/16_snake_live.o -o /tmp/16_snake_live -lSystem -syslibroot "$SDKROOT" -e _main
/tmp/16_snake_live
```

Controls:

```text
W A S D move
q quits
```

## Sample Index

| File | What It Shows | Expected Result |
|------|----------------|-----------------|
| `01_exit_0.s` | Smallest possible `_main` with `exit(0)` | exit `0` |
| `02_hello_world.s` | Direct `write` syscall with a `.asciz` string | stdout `Hello, World!\n`, exit `0` |
| `03_arithmetic_exit_42.s` | Integer multiply and exit status | exit `42` |
| `04_sum_1_to_10.s` | Numeric local labels and a counted loop | exit `55` |
| `05_factorial_5.s` | Repeated multiply in a loop | exit `120` |
| `06_fibonacci_10.s` | Iterative Fibonacci with `cbz` | exit `55` |
| `07_dot_product.s` | Two arrays, memory loads, multiply-accumulate | exit `70` |
| `08_gcd_48_18.s` | Euclid by repeated subtraction | exit `6` |
| `09_array_sum.s` | Walking a `.quad` array in memory | exit `36` |
| `10_string_length.s` | `ldrb` over a C string in `__cstring` | exit `9` |
| `11_copy_qword_and_write.s` | Copying 8 bytes into `__bss` and writing them | stdout `Copy OK\n`, exit `0` |
| `12_puts_call.s` | External call relocation against `_puts` | stdout `Hello from puts()\n`, exit `0` |
| `13_helper_function.s` | Internal `bl`, stack frame, and `ret` | exit `21` |
| `14_literal_pool.s` | `ldr` literal load from an inline `.quad` | exit `77` |
| `15_fp_sqrt.s` | `scvtf`, `fadd`, `fsqrt`, and `fcvtzs` | exit `5` |
| `16_snake_live.s` | Bonus live snake with raw terminal input, redraws, and nonblocking reads | interactive game in a real terminal |
