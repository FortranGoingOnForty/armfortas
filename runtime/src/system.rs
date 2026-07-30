//! Fortran system intrinsics: clock, timing, command-line, environment.

#[cfg(target_os = "freebsd")]
type ProcessClockT = i32;
#[cfg(not(target_os = "freebsd"))]
type ProcessClockT = i64;

#[cfg(target_os = "freebsd")]
const PROCESS_CLOCKS_PER_SECOND: ProcessClockT = 128;
#[cfg(not(target_os = "freebsd"))]
const PROCESS_CLOCKS_PER_SECOND: ProcessClockT = 1_000_000;

extern "C" {
    fn clock() -> ProcessClockT;
}

fn process_clock_seconds(ticks: ProcessClockT) -> f64 {
    ticks as f64 / PROCESS_CLOCKS_PER_SECOND as f64
}

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
    let ticks = unsafe { clock() };
    unsafe {
        *time = process_clock_seconds(ticks);
    }
}

/// Capture DATE_AND_TIME once, write any present character results, and
/// return the eight integer values from the same snapshot.
fn date_and_time_snapshot(
    date_buf: *mut u8,
    date_len: i64,
    time_buf: *mut u8,
    time_len: i64,
    zone_buf: *mut u8,
    zone_len: i64,
) -> [i32; 8] {
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

    [
        year,
        month,
        day,
        tz_offset_min as i32,
        hour,
        minute,
        second,
        millis,
    ]
}

unsafe fn write_date_and_time_integer(addr: *mut u8, elem_size: i64, value: i32) {
    match elem_size {
        1 => ptr::write_unaligned(addr as *mut i8, value as i8),
        2 => ptr::write_unaligned(addr as *mut i16, value as i16),
        4 => ptr::write_unaligned(addr as *mut i32, value),
        8 => ptr::write_unaligned(addr as *mut i64, value as i64),
        16 => ptr::write_unaligned(addr as *mut i128, value as i128),
        _ => unreachable!("DATE_AND_TIME integer kind was validated before storage"),
    }
}

fn write_date_and_time_values_descriptor(
    values: *mut ArrayDescriptor,
    snapshot: &[i32; 8],
) -> Result<(), &'static str> {
    if values.is_null() {
        return Ok(());
    }

    let descriptor = unsafe { &*values };
    if descriptor.rank != 1 {
        return Err("VALUES must be a rank-one array");
    }
    if !matches!(descriptor.elem_size, 1 | 2 | 4 | 8 | 16) {
        return Err("VALUES has an unsupported integer kind");
    }
    if descriptor.base_addr.is_null() {
        return Err("VALUES has no storage");
    }

    let dim = descriptor.dims[0];
    let extent = if dim.upper_bound < dim.lower_bound {
        0
    } else {
        dim.upper_bound
            .checked_sub(dim.lower_bound)
            .and_then(|span| span.checked_add(1))
            .ok_or("VALUES extent overflows the descriptor ABI")?
    };
    if extent < snapshot.len() as i64 {
        return Err("VALUES must contain at least eight elements");
    }
    if dim.stride == 0 {
        return Err("VALUES has a zero element stride");
    }

    let byte_stride = dim
        .stride
        .checked_mul(descriptor.elem_size)
        .and_then(|stride| isize::try_from(stride).ok())
        .ok_or("VALUES stride overflows the address space")?;
    for (index, value) in snapshot.iter().copied().enumerate() {
        let byte_offset = byte_stride
            .checked_mul(index as isize)
            .ok_or("VALUES offset overflows the address space")?;
        let destination = unsafe { descriptor.base_addr.offset(byte_offset) };
        unsafe {
            write_date_and_time_integer(destination, descriptor.elem_size, value);
        }
    }
    Ok(())
}

/// DATE_AND_TIME compatibility entry point for objects whose VALUES actual is
/// a contiguous default-INTEGER array. New code uses the descriptor ABI below.
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
    let snapshot =
        date_and_time_snapshot(date_buf, date_len, time_buf, time_len, zone_buf, zone_len);
    if !values.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(snapshot.as_ptr(), values, snapshot.len());
        }
    }
}

/// DATE_AND_TIME with descriptor-aware VALUES storage. Element kind and
/// positive or negative section strides are honored without temporary
/// contiguous writes or raw-address copyback.
#[no_mangle]
pub extern "C" fn afs_date_and_time_desc(
    date_buf: *mut u8,
    date_len: i64,
    time_buf: *mut u8,
    time_len: i64,
    zone_buf: *mut u8,
    zone_len: i64,
    values: *mut ArrayDescriptor,
) {
    let snapshot =
        date_and_time_snapshot(date_buf, date_len, time_buf, time_len, zone_buf, zone_len);
    if let Err(message) = write_date_and_time_values_descriptor(values, &snapshot) {
        eprintln!("Fortran runtime error: DATE_AND_TIME {message}");
        std::process::exit(1);
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
// them here; std::env::args_os stays as the fallback (macOS reads argv
// via _NSGetArgv and never needed the handoff).
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

static STORED_ARGC: AtomicI32 = AtomicI32::new(-1);
static STORED_ARGV: AtomicPtr<*const u8> = AtomicPtr::new(std::ptr::null_mut());

pub(crate) fn store_args(argc: i32, argv: *const *const u8) {
    if argc >= 0 && !argv.is_null() {
        STORED_ARGV.store(argv as *mut *const u8, Ordering::Release);
        STORED_ARGC.store(argc, Ordering::Release);
    }
}

fn os_string_into_bytes(value: OsString) -> Vec<u8> {
    #[cfg(unix)]
    {
        value.into_vec()
    }
    #[cfg(not(unix))]
    {
        value.to_string_lossy().into_owned().into_bytes()
    }
}

/// The program's arguments as owned bytes, from the stored argv when
/// the entry wrapper provided one, else from std.
fn program_args() -> Vec<Vec<u8>> {
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
            out.push(cstr.to_bytes().to_vec());
        }
        return out;
    }
    std::env::args_os().map(os_string_into_bytes).collect()
}

fn store_optional_i32(target: *mut i32, value: i32) {
    if !target.is_null() {
        unsafe {
            *target = value;
        }
    }
}

fn write_character_result(target: *mut u8, target_len: i64, bytes: &[u8]) -> bool {
    if target.is_null() {
        return false;
    }
    let capacity = usize::try_from(target_len).unwrap_or(0);
    let copied = bytes.len().min(capacity);
    unsafe {
        if copied > 0 {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, copied);
        }
        if copied < capacity {
            std::ptr::write_bytes(target.add(copied), b' ', capacity - copied);
        }
    }
    bytes.len() > capacity
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
    let args = program_args();
    if number < 0 || number as usize >= args.len() {
        store_optional_i32(length, 0);
        write_character_result(value, value_len, &[]);
        store_optional_i32(status, 1);
        return;
    }

    let bytes = &args[number as usize];
    store_optional_i32(length, bytes.len() as i32);
    let truncated = write_character_result(value, value_len, bytes);
    store_optional_i32(status, if truncated { -1 } else { 0 });
}

/// GET_COMMAND: retrieve the full command line.
#[no_mangle]
pub extern "C" fn afs_get_command(
    command: *mut u8,
    cmd_len: i64,
    length: *mut i32,
    status: *mut i32,
) {
    let bytes = program_args().join(&b" "[..]);
    store_optional_i32(length, bytes.len() as i32);
    let truncated = write_character_result(command, cmd_len, &bytes);
    store_optional_i32(status, if truncated { -1 } else { 0 });
}

#[cfg(unix)]
fn environment_value(name: &[u8]) -> Option<Vec<u8>> {
    std::env::var_os(std::ffi::OsStr::from_bytes(name)).map(OsStringExt::into_vec)
}

#[cfg(not(unix))]
fn environment_value(name: &[u8]) -> Option<Vec<u8>> {
    std::env::var_os(String::from_utf8_lossy(name).as_ref()).map(os_string_into_bytes)
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
        let end = slice
            .iter()
            .rposition(|byte| *byte != b' ')
            .map_or(0, |index| index + 1);
        &slice[..end]
    } else {
        store_optional_i32(length, 0);
        write_character_result(value, value_len, &[]);
        store_optional_i32(status, 1);
        return;
    };

    match environment_value(var_name) {
        Some(bytes) => {
            store_optional_i32(length, bytes.len() as i32);
            let truncated = write_character_result(value, value_len, &bytes);
            store_optional_i32(status, if truncated { -1 } else { 0 });
        }
        None => {
            store_optional_i32(length, 0);
            write_character_result(value, value_len, &[]);
            store_optional_i32(status, 1);
        }
    }
}

/// EXECUTE_COMMAND_LINE: run a shell command.
const ASYNC_COMMAND_REAP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
const ASYNC_COMMAND_RECEIVE_BATCH: usize = 64;

type AsyncCommandSender = std::sync::mpsc::Sender<std::process::Child>;

fn async_command_sender() -> Option<&'static AsyncCommandSender> {
    static SENDER: std::sync::OnceLock<AsyncCommandSender> = std::sync::OnceLock::new();

    if let Some(sender) = SENDER.get() {
        return Some(sender);
    }

    let (sender, receiver) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("afs-command-reaper".to_string())
        .spawn(move || reap_async_commands(receiver));
    if spawned.is_err() {
        return SENDER.get();
    }

    let _ = SENDER.set(sender);
    SENDER.get()
}

fn reap_async_commands(receiver: std::sync::mpsc::Receiver<std::process::Child>) {
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};

    let mut children = Vec::new();
    let mut connected = true;

    while connected || !children.is_empty() {
        if !connected {
            std::thread::sleep(ASYNC_COMMAND_REAP_INTERVAL);
        } else if children.is_empty() {
            match receiver.recv() {
                Ok(child) => children.push(child),
                Err(_) => connected = false,
            }
        } else {
            match receiver.recv_timeout(ASYNC_COMMAND_REAP_INTERVAL) {
                Ok(child) => children.push(child),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => connected = false,
            }
        }

        if connected {
            for _ in 1..ASYNC_COMMAND_RECEIVE_BATCH {
                match receiver.try_recv() {
                    Ok(child) => children.push(child),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        connected = false;
                        break;
                    }
                }
            }
        }

        children.retain_mut(|child| match child.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(error) => error.kind() == std::io::ErrorKind::Interrupted,
        });
    }
}

fn spawn_async_command(command: &str) -> std::io::Result<u32> {
    let sender = async_command_sender()
        .ok_or_else(|| std::io::Error::other("could not start asynchronous command reaper"))?;
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .spawn()?;
    let pid = child.id();

    if let Err(error) = sender.send(child) {
        let mut child = error.0;
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "asynchronous command reaper stopped",
        ));
    }

    Ok(pid)
}

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
        match spawn_async_command(&cmd) {
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

    #[cfg(unix)]
    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }

    #[cfg(unix)]
    fn process_exists(pid: u32) -> bool {
        unsafe { kill(pid as i32, 0) == 0 }
    }

    #[cfg(unix)]
    fn wait_for_processes_to_disappear(pids: &[u32], timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if pids.iter().all(|pid| !process_exists(*pid)) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    struct ProcessCleanup {
        pid: Option<u32>,
    }

    #[cfg(unix)]
    impl ProcessCleanup {
        fn new(pid: u32) -> Self {
            Self { pid: Some(pid) }
        }

        fn pid(&self) -> u32 {
            self.pid.expect("process cleanup should still be armed")
        }

        fn terminate_and_reap(&mut self, timeout: std::time::Duration) -> bool {
            let Some(pid) = self.pid else {
                return true;
            };
            if !process_exists(pid) {
                self.pid = None;
                return true;
            }

            unsafe {
                kill(pid as i32, 15);
            }
            if wait_for_processes_to_disappear(std::slice::from_ref(&pid), timeout) {
                self.pid = None;
                return true;
            }

            unsafe {
                kill(pid as i32, 9);
            }
            let reaped = wait_for_processes_to_disappear(std::slice::from_ref(&pid), timeout);
            if reaped {
                self.pid = None;
            }
            reaped
        }
    }

    #[cfg(unix)]
    impl Drop for ProcessCleanup {
        fn drop(&mut self) {
            let _ = self.terminate_and_reap(std::time::Duration::from_secs(3));
        }
    }

    #[cfg(unix)]
    fn wait_for_pid_files(
        paths: &[std::path::PathBuf],
        timeout: std::time::Duration,
    ) -> Option<Vec<u32>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let pids = paths
                .iter()
                .map(|path| std::fs::read_to_string(path).ok()?.trim().parse().ok())
                .collect::<Option<Vec<_>>>();
            if pids.is_some() {
                return pids;
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn execute_async_command(command: &str) {
        let mut cmdstat = i32::MIN;
        afs_execute_command_line(
            command.as_ptr(),
            command.len() as i64,
            0,
            std::ptr::null_mut(),
            &mut cmdstat,
        );
        assert_eq!(cmdstat, 0, "asynchronous command should start");
    }

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
    fn cpu_time_converts_one_second_of_native_ticks() {
        assert_eq!(process_clock_seconds(PROCESS_CLOCKS_PER_SECOND), 1.0);
    }

    #[test]
    fn date_and_time_values_honor_integer_kind_and_negative_stride() {
        let snapshot = [2026, 7, 30, -240, 12, 34, 56, 789];
        let sentinel = i64::MIN;
        let mut storage = [sentinel; 17];
        let mut descriptor = ArrayDescriptor::zeroed();
        descriptor.base_addr = unsafe { storage.as_mut_ptr().add(15) }.cast();
        descriptor.elem_size = std::mem::size_of::<i64>() as i64;
        descriptor.rank = 1;
        descriptor.dims[0] = crate::descriptor::DimDescriptor {
            lower_bound: 1,
            upper_bound: 8,
            stride: -2,
        };

        write_date_and_time_values_descriptor(&mut descriptor, &snapshot).unwrap();

        for (index, expected) in snapshot.iter().copied().enumerate() {
            assert_eq!(storage[15 - index * 2], expected as i64);
        }
        for index in (0..storage.len()).filter(|index| index % 2 == 0) {
            assert_eq!(storage[index], sentinel);
        }
    }

    #[test]
    fn date_and_time_values_reject_short_descriptors_without_writing() {
        let snapshot = [2026, 7, 30, -240, 12, 34, 56, 789];
        let mut storage = [i32::MIN; 7];
        let mut descriptor = ArrayDescriptor::zeroed();
        descriptor.base_addr = storage.as_mut_ptr().cast();
        descriptor.elem_size = std::mem::size_of::<i32>() as i64;
        descriptor.rank = 1;
        descriptor.dims[0] = crate::descriptor::DimDescriptor {
            lower_bound: 1,
            upper_bound: 7,
            stride: 1,
        };

        assert_eq!(
            write_date_and_time_values_descriptor(&mut descriptor, &snapshot),
            Err("VALUES must contain at least eight elements")
        );
        assert_eq!(storage, [i32::MIN; 7]);
    }

    #[cfg(target_os = "freebsd")]
    #[test]
    fn cpu_time_matches_freebsd_clock_scale() {
        extern "C" {
            fn clock() -> i32;
        }

        let before = loop {
            let ticks = unsafe { clock() };
            assert!(ticks >= 0, "clock() failed");
            if ticks > 0 {
                break ticks;
            }
            std::hint::spin_loop();
        };
        let mut actual = -1.0f64;
        afs_cpu_time(&mut actual);
        let after = unsafe { clock() };
        assert!(after >= before, "clock() moved backwards or failed");

        let lower = before as f64 / 128.0;
        let upper = after as f64 / 128.0;
        assert!(
            (lower..=upper).contains(&actual),
            "CPU_TIME {actual} was outside native clock range {lower}..={upper}"
        );
    }

    #[test]
    fn command_argument_count_nonneg() {
        let c = afs_command_argument_count();
        assert!(c >= 0);
    }

    #[test]
    fn character_result_copies_pads_and_reports_truncation() {
        let mut target = [b'?'; 4];
        assert!(!write_character_result(
            target.as_mut_ptr(),
            target.len() as i64,
            b"ab"
        ));
        assert_eq!(&target, b"ab  ");

        assert!(write_character_result(target.as_mut_ptr(), 2, b"abcd"));
        assert_eq!(&target[..2], b"ab");

        assert!(write_character_result(target.as_mut_ptr(), 0, b"abcd"));
        assert!(!write_character_result(std::ptr::null_mut(), 0, b"abcd"));
    }

    #[cfg(unix)]
    #[test]
    fn process_cleanup_reaps_during_unwind() {
        let blocker = spawn_async_command("while :; do :; done")
            .expect("long-running asynchronous command should start");
        let unwind = std::panic::catch_unwind(|| {
            let _cleanup = ProcessCleanup::new(blocker);
            panic!("exercise process cleanup during unwind");
        });

        assert!(unwind.is_err());
        assert!(
            !process_exists(blocker),
            "process cleanup should reap the child during unwind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn asynchronous_commands_are_reaped_without_head_of_line_blocking() {
        let blocker = spawn_async_command("while :; do :; done")
            .expect("long-running asynchronous command should start");
        let mut blocker = ProcessCleanup::new(blocker);
        let pid_paths = (0..64)
            .map(|index| {
                std::path::PathBuf::from(format!(
                    "/tmp/afs-command-reaper-{}-{index}.pid",
                    std::process::id()
                ))
            })
            .collect::<Vec<_>>();
        for path in &pid_paths {
            let _ = std::fs::remove_file(path);
            execute_async_command(&format!("echo $$ > {}; exit 0", path.display()));
        }

        let quick = wait_for_pid_files(&pid_paths, std::time::Duration::from_secs(3));
        let quick_reaped = quick
            .as_ref()
            .map(|pids| wait_for_processes_to_disappear(pids, std::time::Duration::from_secs(3)))
            .unwrap_or(false);
        let blocker_was_running = process_exists(blocker.pid());
        let blocker_reaped = blocker.terminate_and_reap(std::time::Duration::from_secs(3));
        for path in pid_paths {
            let _ = std::fs::remove_file(path);
        }

        assert!(
            quick.is_some(),
            "asynchronous commands did not publish PIDs"
        );
        assert!(
            quick_reaped,
            "completed asynchronous commands were not reaped"
        );
        assert!(
            blocker_was_running,
            "later commands were not tested behind a running command"
        );
        assert!(
            blocker_reaped,
            "terminated asynchronous command was not reaped"
        );
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
