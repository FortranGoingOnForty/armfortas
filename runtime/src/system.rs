//! Fortran system intrinsics: clock, timing, command-line, environment.

/// SYSTEM_CLOCK: returns monotonic clock count, rate, and max
/// (kind-8 resolution; see afs_system_clock_k).
#[no_mangle]
pub extern "C" fn afs_system_clock(count: *mut i64, count_rate: *mut i64, count_max: *mut i64) {
    afs_system_clock_k(count, count_rate, count_max, 8);
}

/// SYSTEM_CLOCK keyed by the smallest integer kind among the present
/// arguments (gfortran's behavior): kind >= 8 gets the nanosecond
/// clock, kind 4 a millisecond clock wrapped at HUGE(int32) so COUNT
/// and COUNT_MAX fit, and kinds 1/2 report "no clock" per F2018
/// 16.9.202 (COUNT = -HUGE, RATE = 0, MAX = 0). Values are written as
/// i64; the lowering truncates to each argument's declared kind,
/// which is lossless for the ranges chosen here.
#[no_mangle]
pub extern "C" fn afs_system_clock_k(
    count: *mut i64,
    count_rate: *mut i64,
    count_max: *mut i64,
    kind: i32,
) {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();

    let (count_val, rate_val, max_val) = match kind {
        k if k >= 8 => (now.as_nanos() as i64, 1_000_000_000i64, i64::MAX),
        4 => {
            let max = i32::MAX as i64;
            ((now.as_millis() as i64) % (max + 1), 1_000, max)
        }
        2 => (-(i16::MAX as i64), 0, 0),
        _ => (-(i8::MAX as i64), 0, 0),
    };

    if !count.is_null() {
        unsafe {
            *count = count_val;
        }
    }
    if !count_rate.is_null() {
        unsafe {
            *count_rate = rate_val;
        }
    }
    if !count_max.is_null() {
        unsafe {
            *count_max = max_val;
        }
    }
}

/// CPU_TIME: returns processor time in seconds.
#[no_mangle]
pub extern "C" fn afs_cpu_time(time: *mut f64) {
    if time.is_null() {
        return;
    }
    extern "C" {
        fn clock() -> i64;
    }
    const CLOCKS_PER_SEC: i64 = 1_000_000; // POSIX value on macOS
    let ticks = unsafe { clock() };
    unsafe {
        *time = ticks as f64 / CLOCKS_PER_SEC as f64;
    }
}

/// DATE_AND_TIME: returns date, time, timezone, and 8-element values array.
#[no_mangle]
pub extern "C" fn afs_date_and_time(
    date_buf: *mut u8,
    date_len: i64,
    time_buf: *mut u8,
    time_len: i64,
    zone_buf: *mut u8,
    zone_len: i64,
    values: *mut i32,
) {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = (now.subsec_millis()) as i32;

    // Use POSIX localtime_r to decompose.
    #[repr(C)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: i64, // macOS: long (8 bytes on ARM64)
        tm_zone: *const u8,
    }
    extern "C" {
        fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
    }
    let mut tm = unsafe { std::mem::zeroed::<Tm>() };
    let time_t = secs;
    unsafe {
        localtime_r(&time_t, &mut tm);
    }

    let year = tm.tm_year + 1900;
    let month = tm.tm_mon + 1;
    let day = tm.tm_mday;
    let hour = tm.tm_hour;
    let minute = tm.tm_min;
    let second = tm.tm_sec;
    let tz_offset_min = tm.tm_gmtoff / 60;

    // DATE: YYYYMMDD
    if !date_buf.is_null() && date_len >= 8 {
        let s = format!("{:04}{:02}{:02}", year, month, day);
        let bytes = s.as_bytes();
        let n = bytes.len().min(date_len as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), date_buf, n);
        }
        if n < date_len as usize {
            unsafe {
                std::ptr::write_bytes(date_buf.add(n), b' ', date_len as usize - n);
            }
        }
    }

    // TIME: hhmmss.sss
    if !time_buf.is_null() && time_len >= 10 {
        let s = format!("{:02}{:02}{:02}.{:03}", hour, minute, second, millis);
        let bytes = s.as_bytes();
        let n = bytes.len().min(time_len as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), time_buf, n);
        }
        if n < time_len as usize {
            unsafe {
                std::ptr::write_bytes(time_buf.add(n), b' ', time_len as usize - n);
            }
        }
    }

    // ZONE: +hhmm or -hhmm
    if !zone_buf.is_null() && zone_len >= 5 {
        let sign = if tz_offset_min >= 0 { '+' } else { '-' };
        let abs_min = tz_offset_min.unsigned_abs();
        let s = format!("{}{:02}{:02}", sign, abs_min / 60, abs_min % 60);
        let bytes = s.as_bytes();
        let n = bytes.len().min(zone_len as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), zone_buf, n);
        }
        if n < zone_len as usize {
            unsafe {
                std::ptr::write_bytes(zone_buf.add(n), b' ', zone_len as usize - n);
            }
        }
    }

    // VALUES(8): year, month, day, tz_minutes, hour, minute, second, milliseconds
    if !values.is_null() {
        unsafe {
            *values.add(0) = year;
            *values.add(1) = month;
            *values.add(2) = day;
            *values.add(3) = tz_offset_min as i32;
            *values.add(4) = hour;
            *values.add(5) = minute;
            *values.add(6) = second;
            *values.add(7) = millis;
        }
    }
}

// ---- argv plumbing (x07) ----
//
// `std::env::args()` captures argv through an `.init_array`
// constructor inside Rust std. Linking libarmfortas_rt.a under a
// NON-Rust `main` (the compiler's entry wrapper) lets the linker drop
// the archive member holding that constructor — nothing references
// it — so std's argv comes back EMPTY on ELF targets and
// command_argument_count() returned -1. The entry wrappers therefore
// forward main's (argc, argv) into afs_program_init, which stores
// them here; std::env::args stays as the fallback (macOS reads argv
// via _NSGetArgv and never needed the handoff).
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

static STORED_ARGC: AtomicI32 = AtomicI32::new(-1);
static STORED_ARGV: AtomicPtr<*const u8> = AtomicPtr::new(std::ptr::null_mut());

pub(crate) fn store_args(argc: i32, argv: *const *const u8) {
    if argc >= 0 && !argv.is_null() {
        STORED_ARGV.store(argv as *mut *const u8, Ordering::Release);
        STORED_ARGC.store(argc, Ordering::Release);
    }
}

/// The program's arguments as owned strings, from the stored argv
/// when the entry wrapper provided one, else from std.
fn program_args() -> Vec<String> {
    let argc = STORED_ARGC.load(Ordering::Acquire);
    let argv = STORED_ARGV.load(Ordering::Acquire);
    if argc >= 0 && !argv.is_null() {
        let mut out = Vec::with_capacity(argc as usize);
        for i in 0..argc as usize {
            let p = unsafe { *argv.add(i) };
            if p.is_null() {
                break;
            }
            let cstr = unsafe { std::ffi::CStr::from_ptr(p as *const std::ffi::c_char) };
            out.push(cstr.to_string_lossy().into_owned());
        }
        return out;
    }
    std::env::args().collect()
}

/// COMMAND_ARGUMENT_COUNT: returns argc - 1.
#[no_mangle]
pub extern "C" fn afs_command_argument_count() -> i32 {
    program_args().len() as i32 - 1
}

/// GET_COMMAND_ARGUMENT: retrieve the nth command-line argument.
#[no_mangle]
pub extern "C" fn afs_get_command_argument(
    number: i32,
    value: *mut u8,
    value_len: i64,
    length: *mut i32,
    status: *mut i32,
) {
    let args: Vec<String> = program_args();
    if number < 0 || number as usize >= args.len() {
        if !status.is_null() {
            unsafe {
                *status = 1;
            }
        }
        if !length.is_null() {
            unsafe {
                *length = 0;
            }
        }
        return;
    }

    let arg = &args[number as usize];
    let bytes = arg.as_bytes();

    if !length.is_null() {
        unsafe {
            *length = bytes.len() as i32;
        }
    }

    if !value.is_null() && value_len > 0 {
        let n = bytes.len().min(value_len as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), value, n);
            if n < value_len as usize {
                std::ptr::write_bytes(value.add(n), b' ', value_len as usize - n);
            }
        }
    }

    if !status.is_null() {
        unsafe {
            *status = 0;
        }
    }
}

/// GET_COMMAND: retrieve the full command line.
#[no_mangle]
pub extern "C" fn afs_get_command(
    command: *mut u8,
    cmd_len: i64,
    length: *mut i32,
    status: *mut i32,
) {
    let full: String = program_args().join(" ");
    let bytes = full.as_bytes();

    if !length.is_null() {
        unsafe {
            *length = bytes.len() as i32;
        }
    }
    if !command.is_null() && cmd_len > 0 {
        let n = bytes.len().min(cmd_len as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), command, n);
            if n < cmd_len as usize {
                std::ptr::write_bytes(command.add(n), b' ', cmd_len as usize - n);
            }
        }
    }
    if !status.is_null() {
        unsafe {
            *status = 0;
        }
    }
}

/// GET_ENVIRONMENT_VARIABLE: retrieve an environment variable by name.
#[no_mangle]
pub extern "C" fn afs_get_environment_variable(
    name: *const u8,
    name_len: i64,
    value: *mut u8,
    value_len: i64,
    length: *mut i32,
    status: *mut i32,
) {
    let var_name = if !name.is_null() && name_len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(name, name_len as usize) };
        String::from_utf8_lossy(slice).trim().to_string()
    } else {
        if !status.is_null() {
            unsafe {
                *status = 1;
            }
        }
        return;
    };

    match std::env::var(&var_name) {
        Ok(val) => {
            let bytes = val.as_bytes();
            if !length.is_null() {
                unsafe {
                    *length = bytes.len() as i32;
                }
            }
            if !value.is_null() && value_len > 0 {
                let n = bytes.len().min(value_len as usize);
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), value, n);
                    if n < value_len as usize {
                        std::ptr::write_bytes(value.add(n), b' ', value_len as usize - n);
                    }
                }
            }
            if !status.is_null() {
                unsafe {
                    *status = 0;
                }
            }
        }
        Err(_) => {
            if !length.is_null() {
                unsafe {
                    *length = 0;
                }
            }
            if !status.is_null() {
                unsafe {
                    *status = 1;
                }
            }
        }
    }
}

/// EXECUTE_COMMAND_LINE: run a shell command.
#[no_mangle]
pub extern "C" fn afs_execute_command_line(
    command: *const u8,
    cmd_len: i64,
    wait: i32,
    exitstat: *mut i32,
    cmdstat: *mut i32,
) {
    let cmd = if !command.is_null() && cmd_len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(command, cmd_len as usize) };
        String::from_utf8_lossy(slice).trim().to_string()
    } else {
        if !cmdstat.is_null() {
            unsafe {
                *cmdstat = 1;
            }
        }
        return;
    };

    use std::process::Command;
    if wait != 0 {
        match Command::new("sh").arg("-c").arg(&cmd).status() {
            Ok(status) => {
                if !exitstat.is_null() {
                    unsafe {
                        *exitstat = status.code().unwrap_or(-1);
                    }
                }
                if !cmdstat.is_null() {
                    unsafe {
                        *cmdstat = 0;
                    }
                }
            }
            Err(_) => {
                if !cmdstat.is_null() {
                    unsafe {
                        *cmdstat = -1;
                    }
                }
            }
        }
    } else {
        match Command::new("sh").arg("-c").arg(&cmd).spawn() {
            Ok(_) => {
                if !cmdstat.is_null() {
                    unsafe {
                        *cmdstat = 0;
                    }
                }
            }
            Err(_) => {
                if !cmdstat.is_null() {
                    unsafe {
                        *cmdstat = -1;
                    }
                }
            }
        }
    }
}

// Shared RNG state for RANDOM_NUMBER / RANDOM_SEED.
use crate::descriptor::ArrayDescriptor;
use std::cell::Cell;
use std::ptr;
thread_local! {
    static RNG_SEED: Cell<u64> = const { Cell::new(0) };
    static RNG_INITIALIZED: Cell<bool> = const { Cell::new(false) };
}

const RANDOM_SEED_VECTOR_SIZE: i64 = 1;

fn default_random_seed() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = (std::process::id() as u64) << 32;
    let stack_probe = 0u8;
    let addr = (&stack_probe as *const u8 as usize) as u64;
    let mut seed = now ^ pid ^ addr ^ 0x9e37_79b9_7f4a_7c15;
    if seed == 0 {
        seed = 0x0123_4567_89ab_cdef;
    }
    seed
}

fn set_random_seed(seed: u64) {
    RNG_SEED.with(|s| s.set(seed));
    RNG_INITIALIZED.with(|initialized| initialized.set(true));
}

fn current_random_seed() -> u64 {
    RNG_SEED.with(|s| {
        RNG_INITIALIZED.with(|initialized| {
            if !initialized.get() {
                let seed = default_random_seed();
                s.set(seed);
                initialized.set(true);
                seed
            } else {
                s.get()
            }
        })
    })
}

fn next_random_u64() -> u64 {
    RNG_SEED.with(|s| {
        RNG_INITIALIZED.with(|initialized| {
            if !initialized.get() {
                s.set(default_random_seed());
                initialized.set(true);
            }
        });
        let mut x = s.get();
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        s.set(x);
        x
    })
}

unsafe fn read_seed_element(addr: *const u8, elem_size: i64) -> u64 {
    match elem_size {
        1 => ptr::read_unaligned(addr as *const i8) as i64 as u64,
        2 => ptr::read_unaligned(addr as *const i16) as i64 as u64,
        4 => ptr::read_unaligned(addr as *const i32) as i64 as u64,
        8 => ptr::read_unaligned(addr as *const i64) as u64,
        n if n > 0 => {
            let mut value = 0u64;
            let bytes = n.min(8) as usize;
            for i in 0..bytes {
                value |= (ptr::read(addr.add(i)) as u64) << (i * 8);
            }
            value
        }
        _ => 0,
    }
}

unsafe fn write_seed_element(addr: *mut u8, elem_size: i64, seed: u64) {
    match elem_size {
        1 => ptr::write_unaligned(addr as *mut i8, seed as i8),
        2 => ptr::write_unaligned(addr as *mut i16, seed as i16),
        4 => ptr::write_unaligned(addr as *mut i32, seed as i32),
        8 => ptr::write_unaligned(addr as *mut i64, seed as i64),
        n if n > 0 => {
            let bytes = n.min(8) as usize;
            for i in 0..bytes {
                ptr::write(addr.add(i), ((seed >> (i * 8)) & 0xff) as u8);
            }
        }
        _ => {}
    }
}

/// RANDOM_NUMBER: fill a scalar single-precision real with a random value in [0, 1).
#[no_mangle]
pub extern "C" fn afs_random_number_f32(harvest: *mut f32) {
    if harvest.is_null() {
        return;
    }
    let x = next_random_u64();
    unsafe {
        *harvest = ((x >> 40) as f32) / (1u32 << 24) as f32;
    }
}

/// RANDOM_NUMBER: fill a scalar with a random value in [0, 1).
#[no_mangle]
pub extern "C" fn afs_random_number_f64(harvest: *mut f64) {
    if harvest.is_null() {
        return;
    }
    let x = next_random_u64();
    unsafe {
        *harvest = (x >> 11) as f64 / (1u64 << 53) as f64;
    }
}

/// RANDOM_NUMBER on an N-element f32 array: every element gets an
/// independent draw in [0, 1).  The scalar entry only fills one slot,
/// so the IR dispatches to this when HARVEST is an array — without it,
/// LAPACK / QR / EIG run on uninitialized stack data and segfault
/// nondeterministically.
#[no_mangle]
pub extern "C" fn afs_random_number_array_f32(harvest: *mut f32, n: i64) {
    if harvest.is_null() || n <= 0 {
        return;
    }
    for i in 0..n {
        let x = next_random_u64();
        let v = ((x >> 40) as f32) / (1u32 << 24) as f32;
        unsafe {
            *harvest.offset(i as isize) = v;
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_random_number_array_f64(harvest: *mut f64, n: i64) {
    if harvest.is_null() || n <= 0 {
        return;
    }
    for i in 0..n {
        let x = next_random_u64();
        let v = (x >> 11) as f64 / (1u64 << 53) as f64;
        unsafe {
            *harvest.offset(i as isize) = v;
        }
    }
}

/// RANDOM_SEED: seed the random number generator.
#[no_mangle]
pub extern "C" fn afs_random_seed(seed_val: i64) {
    set_random_seed(seed_val as u64);
}

#[no_mangle]
pub extern "C" fn afs_random_seed_init() {
    set_random_seed(default_random_seed());
}

#[no_mangle]
pub extern "C" fn afs_random_seed_size(size: *mut i64) {
    if size.is_null() {
        return;
    }
    unsafe {
        *size = RANDOM_SEED_VECTOR_SIZE;
    }
}

#[no_mangle]
pub extern "C" fn afs_random_seed_put(seed_desc: *const ArrayDescriptor) {
    if seed_desc.is_null() {
        return;
    }
    let desc = unsafe { &*seed_desc };
    if desc.base_addr.is_null() || desc.total_elements() <= 0 {
        return;
    }
    let seed = unsafe { read_seed_element(desc.base_addr as *const u8, desc.elem_size) };
    set_random_seed(seed);
}

#[no_mangle]
pub extern "C" fn afs_random_seed_get(seed_desc: *mut ArrayDescriptor) {
    if seed_desc.is_null() {
        return;
    }
    let desc = unsafe { &mut *seed_desc };
    if desc.base_addr.is_null() || desc.total_elements() <= 0 {
        return;
    }
    let seed = current_random_seed();
    unsafe {
        write_seed_element(desc.base_addr, desc.elem_size, seed);
    }
}

/// POPCOUNT: count set bits in an integer (Hamming weight).
#[no_mangle]
pub extern "C" fn afs_popcount(val: u64) -> i32 {
    val.count_ones() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_increases() {
        let mut c1 = 0i64;
        let mut c2 = 0i64;
        let mut rate = 0i64;
        afs_system_clock(&mut c1, &mut rate, std::ptr::null_mut());
        // Busy loop to ensure time passes.
        let mut sum = 0u64;
        for i in 0..100000 {
            sum = sum.wrapping_add(i);
        }
        let _ = sum;
        afs_system_clock(&mut c2, std::ptr::null_mut(), std::ptr::null_mut());
        assert!(c2 >= c1, "clock should not go backwards: {} vs {}", c1, c2);
        assert!(rate > 0);
    }

    #[test]
    fn cpu_time_positive() {
        let mut t = 0.0f64;
        afs_cpu_time(&mut t);
        assert!(t >= 0.0);
    }

    #[test]
    fn command_argument_count_nonneg() {
        let c = afs_command_argument_count();
        assert!(c >= 0);
    }

    #[test]
    fn random_number_range() {
        for _ in 0..100 {
            let mut x = 0.0f64;
            afs_random_number_f64(&mut x);
            assert!((0.0..1.0).contains(&x), "random out of range: {}", x);
        }
    }

    #[test]
    fn random_seed_keeps_explicit_sequences_reproducible() {
        let mut first = 0.0f64;
        let mut second = 0.0f64;
        afs_random_seed(42);
        afs_random_number_f64(&mut first);
        afs_random_seed(42);
        afs_random_number_f64(&mut second);
        assert_eq!(first, second);
    }
}

/// NEXT/PREVIOUS for enumeration values (F2023 16.9.148/16.9.161):
/// step a 1-based ordinal by +-1 within [1, count]. Out of range:
/// with a STAT argument, STAT=1 and the value is returned unchanged;
/// without one, a loud runtime error (exit 1).
#[no_mangle]
pub extern "C" fn afs_enum_step(v: i32, count: i32, step: i32, stat: *mut i32) -> i32 {
    let next = v + step;
    let ok = next >= 1 && next <= count;
    if !stat.is_null() {
        unsafe { *stat = if ok { 0 } else { 1 } };
        return if ok { next } else { v };
    }
    if !ok {
        eprintln!(
            "Fortran runtime error: {} of enumeration ordinal {} is out of range 1..{}",
            if step > 0 { "NEXT" } else { "PREVIOUS" },
            v,
            count
        );
        std::process::exit(1);
    }
    next
}
