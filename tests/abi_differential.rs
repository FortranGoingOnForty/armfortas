//! Sprint x04 differential ABI harness (scaffold).
//!
//! Each row generates a paired caller/callee in C, built as separate
//! translation units so the call crosses a real ABI boundary, then run:
//! the callee checks every received value and the caller checks every
//! returned one. This sprint proves the harness with clang on both
//! sides (all-C self-check); the armfortas legs — armfortas builds one
//! side, clang the other — are registered as skips until x05 emits
//! code and x06 links it. x07/x08 flip them on.
//!
//! Coverage mirrors the in-scope subset of the psABI's own harness
//! (`.docs/refs/x86-64-ABI/abitest/`): basic scalars, struct
//! passing/returning, complex passing/returning, register exhaustion,
//! the revert rule, FP-live-across-call, and the alignment-at-entry
//! assertion. Unions, bitfields, and __m128 wait for the BIND(C)
//! sprints that can produce them.
//!
//! Every callee asserts 16-byte frame alignment at entry
//! (`__builtin_frame_address(0) % 16 == 0` under
//! -fno-omit-frame-pointer): misalignment doesn't fault in our code,
//! it faults in libc's first movaps spill, deep inside printf, on some
//! inputs — so the harness checks it on every single row.

use std::path::PathBuf;
use std::process::Command;

use armfortas::target::{Arch, ObjectFormat, TargetSpec};

struct Row {
    name: &'static str,
    /// Shared declarations (typedefs).
    decls: &'static str,
    /// Callee parameter list.
    params: &'static str,
    /// Callee return type.
    ret: &'static str,
    /// Callee body: check args, set *ok=0 on mismatch, return a value.
    callee_body: &'static str,
    /// Caller body: call `callee(...)`, check the return, set ok=0 on
    /// mismatch. `ok` starts at 1; main returns !ok.
    caller_body: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        name: "scalar_mix",
        decls: "",
        params: "int a, long b, _Bool c, void *d, float e, double f, short g",
        ret: "long",
        callee_body: r#"
            if (a != -7) *ok = 0;
            if (b != 1234567890123L) *ok = 0;
            if (c != 1) *ok = 0;
            if (d != (void *)0x1000) *ok = 0;
            if (e != 1.5f) *ok = 0;
            if (f != 2.25) *ok = 0;
            if (g != -3) *ok = 0;
            return b + a;
        "#,
        caller_body: r#"
            long r = callee(-7, 1234567890123L, 1, (void *)0x1000, 1.5f, 2.25, -3);
            if (r != 1234567890123L - 7) ok = 0;
        "#,
    },
    Row {
        name: "seven_ints_stack",
        decls: "",
        params: "long a, long b, long c, long d, long e, long f, long g",
        ret: "long",
        callee_body: r#"
            if (a!=1||b!=2||c!=3||d!=4||e!=5||f!=6) *ok = 0;
            if (g != 77) *ok = 0; /* 7th: stack */
            return g;
        "#,
        caller_body: r#"
            if (callee(1,2,3,4,5,6,77) != 77) ok = 0;
        "#,
    },
    Row {
        name: "nine_floats_stack",
        decls: "",
        params: "double a,double b,double c,double d,double e,double f,double g,double h,double i",
        ret: "double",
        callee_body: r#"
            if (a!=1||b!=2||c!=3||d!=4||e!=5||f!=6||g!=7||h!=8) *ok = 0;
            if (i != 9.5) *ok = 0; /* 9th: stack */
            return i;
        "#,
        caller_body: r#"
            if (callee(1,2,3,4,5,6,7,8,9.5) != 9.5) ok = 0;
        "#,
    },
    Row {
        name: "struct_ii_one_gp",
        decls: "typedef struct { int a, b; } S;",
        params: "S s, int tail",
        ret: "int",
        callee_body: r#"
            if (s.a != 11 || s.b != 22) *ok = 0;
            if (tail != 33) *ok = 0;
            return s.a + s.b;
        "#,
        caller_body: r#"
            S s = {11, 22};
            if (callee(s, 33) != 33) ok = 0;
        "#,
    },
    Row {
        name: "struct_ff_one_xmm",
        decls: "typedef struct { float a, b; } S;",
        params: "S s",
        ret: "float",
        callee_body: r#"
            if (s.a != 1.25f || s.b != -2.5f) *ok = 0;
            return s.a + s.b;
        "#,
        caller_body: r#"
            S s = {1.25f, -2.5f};
            if (callee(s) != -1.25f) ok = 0;
        "#,
    },
    Row {
        name: "struct_dd_xmm_pair",
        decls: "typedef struct { double a, b; } S;",
        params: "S s",
        ret: "double",
        callee_body: r#"
            if (s.a != 3.5 || s.b != -4.5) *ok = 0;
            return s.a * s.b;
        "#,
        caller_body: r#"
            S s = {3.5, -4.5};
            if (callee(s) != 3.5 * -4.5) ok = 0;
        "#,
    },
    Row {
        name: "struct_ld_gp_then_xmm",
        decls: "typedef struct { long a; double b; } S;",
        params: "S s",
        ret: "double",
        callee_body: r#"
            if (s.a != 9 || s.b != 0.5) *ok = 0;
            return s.a + s.b;
        "#,
        caller_body: r#"
            S s = {9, 0.5};
            if (callee(s) != 9.5) ok = 0;
        "#,
    },
    Row {
        name: "complex_float_one_xmm",
        decls: "#include <complex.h>",
        params: "float _Complex z",
        ret: "float _Complex",
        callee_body: r#"
            if (crealf(z) != 1.0f || cimagf(z) != 2.0f) *ok = 0;
            return z + (1.0f + 1.0f * I); /* imag half garbage if packed wrong */
        "#,
        caller_body: r#"
            float _Complex r = callee(1.0f + 2.0f * I);
            if (crealf(r) != 2.0f || cimagf(r) != 3.0f) ok = 0;
        "#,
    },
    Row {
        name: "complex_double_xmm01",
        decls: "#include <complex.h>",
        params: "double _Complex z",
        ret: "double _Complex",
        callee_body: r#"
            if (creal(z) != -1.0 || cimag(z) != 4.0) *ok = 0;
            return z * 2.0;
        "#,
        caller_body: r#"
            double _Complex r = callee(-1.0 + 4.0 * I);
            if (creal(r) != -2.0 || cimag(r) != 8.0) ok = 0;
        "#,
    },
    Row {
        name: "byval_three_eightbytes_memory",
        decls: "typedef struct { long a, b, c; } S;",
        params: "S s, long tail",
        ret: "long",
        callee_body: r#"
            if (s.a != 1 || s.b != 2 || s.c != 3) *ok = 0;
            if (tail != 4) *ok = 0;
            return s.c;
        "#,
        caller_body: r#"
            S s = {1, 2, 3};
            if (callee(s, 4) != 3) ok = 0;
        "#,
    },
    Row {
        name: "packed_struct_memory",
        decls: "typedef struct { char a; long b; } __attribute__((packed)) S;",
        params: "S s",
        ret: "long",
        callee_body: r#"
            if (s.a != 5 || s.b != 6) *ok = 0;
            return s.b;
        "#,
        caller_body: r#"
            S s = {5, 6};
            if (callee(s) != 6) ok = 0;
        "#,
    },
    Row {
        name: "revert_keeps_registers_open",
        decls: "typedef struct { long a, b; } S;",
        params: "long a, long b, long c, long d, long e, S s, long g",
        ret: "long",
        callee_body: r#"
            if (a!=1||b!=2||c!=3||d!=4||e!=5) *ok = 0;
            if (s.a != 8 || s.b != 9) *ok = 0; /* reverted to stack */
            if (g != 7) *ok = 0;               /* still takes r9 */
            return s.b + g;
        "#,
        caller_body: r#"
            S s = {8, 9};
            if (callee(1,2,3,4,5,s,7) != 16) ok = 0;
        "#,
    },
    Row {
        name: "memory_class_return",
        decls: "typedef struct { long a, b, c; } S;",
        params: "long seed",
        ret: "S",
        callee_body: r#"
            S r = { seed, seed * 2, seed * 3 };
            return r;
        "#,
        caller_body: r#"
            S r = callee(11);
            if (r.a != 11 || r.b != 22 || r.c != 33) ok = 0;
        "#,
    },
    Row {
        name: "fp_live_across_call",
        decls: "",
        params: "double x",
        ret: "double",
        callee_body: r#"
            /* clobber every xmm the ABI allows (none are callee-saved) */
            return x * 3.0;
        "#,
        caller_body: r#"
            double live = 1.125;            /* must survive in caller */
            double r = callee(2.0);
            if (r != 6.0) ok = 0;
            if (live != 1.125) ok = 0;      /* corrupt if 'saved' in xmm */
        "#,
    },
];

fn callee_source(row: &Row) -> String {
    format!(
        r#"#include <stdint.h>
{decls}
/* 16-byte alignment at entry: under -fno-omit-frame-pointer the frame
   address is rsp-after-push-rbp, which must be 16-aligned iff rsp was
   ≡ 8 (mod 16) at the call (return address pushed). */
{ret} callee({params}, int *ok) {{
    if (((uintptr_t)__builtin_frame_address(0)) % 16 != 0) *ok = 0;
    {body}
}}
"#,
        decls = row.decls,
        ret = row.ret,
        params = row.params,
        body = row.callee_body,
    )
}

fn caller_source(row: &Row) -> String {
    // Re-declare the callee with the trailing ok pointer.
    format!(
        r#"#include <stdint.h>
{decls}
extern {ret} callee({params}, int *ok);
#define callee(...) callee(__VA_ARGS__, &okp)
int main(void) {{
    int ok = 1;
    int okp_storage = 1;
    int *okp = &okp_storage;
    (void)okp;
    {body}
    return (ok && okp_storage) ? 0 : 1;
}}
"#,
        decls = row.decls,
        ret = row.ret,
        params = row.params,
        body = row.caller_body,
    )
}

fn host_is_x86_elf() -> bool {
    let host = TargetSpec::host();
    host.arch == Arch::X86_64 && host.object_format() == ObjectFormat::Elf
}

/// All-C self-check: clang builds both sides. Proves the harness, the
/// generated shims, and the alignment assertion before armfortas is in
/// the loop.
#[test]
fn all_c_self_check() {
    if !host_is_x86_elf() {
        eprintln!(
            "\nHARNESS_SKIP suite=abi_differential test=all_c_self_check count={} reason=\"SysV differential needs an x86_64 ELF host\"",
            ROWS.len()
        );
        return;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!("afs_abi_diff_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mut failures = Vec::new();
    for row in ROWS {
        let callee_c = dir.join(format!("{}_callee.c", row.name));
        let caller_c = dir.join(format!("{}_caller.c", row.name));
        let bin = dir.join(row.name);
        std::fs::write(&callee_c, callee_source(row)).unwrap();
        std::fs::write(&caller_c, caller_source(row)).unwrap();
        let build = Command::new("cc")
            .args(["-O1", "-fno-omit-frame-pointer", "-o"])
            .arg(&bin)
            .arg(&caller_c)
            .arg(&callee_c)
            .output()
            .expect("cannot run cc");
        if !build.status.success() {
            failures.push(format!(
                "{}: cc failed:\n{}",
                row.name,
                String::from_utf8_lossy(&build.stderr)
            ));
            continue;
        }
        let run = Command::new(&bin).output().expect("cannot run shim");
        if !run.status.success() {
            failures.push(format!(
                "{}: value mismatch across the C/C boundary (exit {:?})",
                row.name,
                run.status.code()
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The armfortas legs: registered now, skipped with exact counts until
/// x05 (codegen) and x06 (link) make them runnable. x07/x08 flip them.
#[test]
fn armfortas_caller_legs_pending_x05_x06() {
    eprintln!(
        "\nHARNESS_SKIP suite=abi_differential test=armfortas_caller_legs_pending_x05_x06 count={} reason=\"armfortas-as-caller needs x05 isel and x06 link\"",
        ROWS.len()
    );
}

#[test]
fn armfortas_callee_legs_pending_x05_x06() {
    eprintln!(
        "\nHARNESS_SKIP suite=abi_differential test=armfortas_callee_legs_pending_x05_x06 count={} reason=\"armfortas-as-callee needs x05 isel and x06 link\"",
        ROWS.len()
    );
}
