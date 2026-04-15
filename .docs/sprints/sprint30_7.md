# Sprint 30.7: Intrinsic Module Completeness

## Prerequisites
Sprint 30 (module system), Sprint 27 (iso_c_binding basics)

## Context
Sprint 30 shipped multi-file compilation. The intrinsic modules (iso_c_binding, iso_fortran_env, ieee_*) were partially implemented in earlier sprints but have gaps that block real-world Fortran code. Sprint 30.5 focuses on test infrastructure. Sprint 30.7 fills the intrinsic module gaps between 30 and 31.

## Deliverables

### 1. iso_c_binding Procedure Lowering
**Currently:** c_loc, c_funloc, c_f_pointer, c_f_procpointer, c_associated, c_sizeof are registered as IntrinsicProc symbols but calling them falls through to an undefined external symbol.

**Needed:**
- `c_loc(x)` → returns the C address of a Fortran variable (type(c_ptr))
- `c_funloc(f)` → returns the C address of a Fortran procedure (type(c_funptr))
- `c_f_pointer(cptr, fptr [, shape])` → associates a Fortran pointer with a C pointer
- `c_associated(c_ptr_1 [, c_ptr_2])` → tests pointer association status
- `c_sizeof(x)` → returns size in bytes

**Impact:** fortsh uses iso_c_binding extensively (3 C interop files).

### 2. iso_fortran_env Inquiry Functions
**Currently:** Missing `compiler_version()` and `compiler_options()`.

**Needed:**
- `compiler_version()` → returns "armfortas 0.1.0" as a deferred-length character string
- `compiler_options()` → returns the --std= and -O flags used for this compilation

**Impact:** Standard conformance. Some programs print compiler info.

### 3. Array-Valued Kind Parameters
**Currently:** `integer_kinds`, `real_kinds`, `logical_kinds`, `character_kinds` are scalar integers. Per the standard, they should be rank-1 arrays listing all supported kinds.

**Needed:**
- `integer_kinds = [1, 2, 4, 8, 16]` (i8, i16, i32, i64, i128)
- `real_kinds = [4, 8]` (f32, f64)
- `logical_kinds = [1, 4]`
- `character_kinds = [1]`

**Impact:** Programs that iterate over `integer_kinds` to select the best kind.

### 4. IEEE Modules
**Currently:** Not implemented. `USE ieee_arithmetic` errors.

**Needed (minimal):**
- `ieee_arithmetic`: ieee_is_nan, ieee_is_finite, ieee_value, ieee_selected_real_kind, derived types ieee_class_type and ieee_round_type
- `ieee_exceptions`: ieee_get_flag, ieee_set_flag, ieee_get_halting_mode
- `ieee_features`: ieee_datatype, ieee_denormal, ieee_sqrt (feature flag constants)

**Impact:** Scientific codes that check for NaN/Inf. fortsh doesn't use these heavily but real Fortran code does.

### 5. c_char Character Constant Values
**Currently:** c_null_char, c_alert, c_backspace, etc. are registered as parameters with no runtime value. They should resolve to their ASCII byte values.

**Needed:**
- c_null_char = ACHAR(0)
- c_alert = ACHAR(7)
- c_backspace = ACHAR(8)
- c_new_line = ACHAR(10)
- c_carriage_return = ACHAR(13)
- c_horizontal_tab = ACHAR(9)
- c_vertical_tab = ACHAR(11)
- c_form_feed = ACHAR(12)

## Definition of Done
- `call c_f_pointer(cptr, fptr)` compiles and links
- `compiler_version()` returns a non-empty string
- `integer_kinds` is a rank-1 array with 5 elements
- `USE ieee_arithmetic` compiles (stub scope, no procedure bodies yet)
- `c_null_char` resolves to ACHAR(0)
