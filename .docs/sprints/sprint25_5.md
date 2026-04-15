# Sprint 25.5: I/O Pipeline Completeness

## Prerequisites
Sprint 25 (Advanced I/O), Sprint 16 (IR Complex Lowering)

## Goals
Connect the format engine and non-advancing I/O through the full pipeline (parser → IR → codegen → runtime). The runtime pieces exist; this sprint wires them to the frontend.

## Deliverables

### 1. Formatted I/O Integration
The FORMAT engine (`runtime/src/format.rs`) parses and applies format strings. The parser handles FORMAT statements. What's missing is the IR→runtime connection.

**Pipeline work:**
- IR: Add `WriteFormatted` / `ReadFormatted` instructions that carry a format string reference
- Lowering: When `WRITE(unit, fmt)` has a format spec (integer label, character expression, or `*`), lower to `afs_write_formatted()` instead of individual `afs_write_int/real/string` calls
- Runtime: Implement `afs_write_formatted(unit, fmt_str, fmt_len, values, n_values)` that constructs IoValue list and calls FormatEngine
- Runtime: Implement `afs_read_formatted(unit, fmt_str, fmt_len, ...)` for formatted input

### 2. Non-Advancing I/O (ADVANCE='NO')
Used by fortsh for prompt output (`WRITE(*, '(A)', ADVANCE='NO') 'prompt> '`).

**Pipeline work:**
- Parser: Recognize `ADVANCE=` specifier in WRITE/READ io-control-list
- IR: Add `advance` flag to I/O instructions
- Lowering: Pass advance flag through to runtime calls
- Runtime: Implement line-buffered output that doesn't emit newline when ADVANCE='NO'
- Runtime: Track column position per unit for subsequent ADVANCE='NO' writes
- Runtime: Detect EOR condition on non-advancing READ, set IOSTAT=IOSTAT_EOR

## When to Schedule
After Sprint 26 (Intrinsics) — the format engine may benefit from intrinsic support for type conversions. Before Sprint 30 (Module System) since formatted I/O is used heavily in fortsh.

## Definition of Done
- `WRITE(*, '(I5,F10.3)') 42, 3.14` produces correctly formatted output
- `WRITE(*, '(A)', ADVANCE='NO') 'hello'` does not emit trailing newline
- Formatted READ parses fixed-width fields according to format descriptors
- All existing end-to-end tests still pass
