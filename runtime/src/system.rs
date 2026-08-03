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

    // Character assignment truncates or blank-pads to the actual length.
    let date = format!("{:04}{:02}{:02}", year, month, day);
    write_character_result(date_buf, date_len, date.as_bytes());

    let time = format!("{:02}{:02}{:02}.{:03}", hour, minute, second, millis);
    write_character_result(time_buf, time_len, time.as_bytes());

    let zone_sign = if tz_offset_min >= 0 { '+' } else { '-' };
    let zone_minutes = tz_offset_min.unsigned_abs();
    let zone = format!(
        "{}{:02}{:02}",
        zone_sign,
        zone_minutes / 60,
        zone_minutes % 60
    );
    write_character_result(zone_buf, zone_len, zone.as_bytes());

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

fn environment_variable_name(name: &[u8], trim_name: bool) -> &[u8] {
    if !trim_name {
        return name;
    }
    let end = name
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |index| index + 1);
    &name[..end]
}

fn get_environment_variable(
    name: *const u8,
    name_len: i64,
    value: *mut u8,
    value_len: i64,
    length: *mut i32,
    status: *mut i32,
    trim_name: bool,
) {
    let var_name = if !name.is_null() && name_len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(name, name_len as usize) };
        environment_variable_name(slice, trim_name)
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

/// Compatibility entry point for compiler output predating TRIM_NAME support.
#[no_mangle]
pub extern "C" fn afs_get_environment_variable(
    name: *const u8,
    name_len: i64,
    value: *mut u8,
    value_len: i64,
    length: *mut i32,
    status: *mut i32,
) {
    get_environment_variable(name, name_len, value, value_len, length, status, true);
}

/// GET_ENVIRONMENT_VARIABLE with the Fortran 2018 TRIM_NAME argument.
#[no_mangle]
pub extern "C" fn afs_get_environment_variable_trim(
    name: *const u8,
    name_len: i64,
    value: *mut u8,
    value_len: i64,
    length: *mut i32,
    status: *mut i32,
    trim_name: i32,
) {
    get_environment_variable(
        name,
        name_len,
        value,
        value_len,
        length,
        status,
        trim_name != 0,
    );
}

/// EXECUTE_COMMAND_LINE: run a shell command.
const ASYNC_COMMAND_REAP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
const ASYNC_COMMAND_RECEIVE_BATCH: usize = 64;
// Launch the Unix command processor independently of the caller's PATH.
#[cfg(unix)]
const COMMAND_PROCESSOR: &str = "/bin/sh";
#[cfg(not(unix))]
const COMMAND_PROCESSOR: &str = "sh";

type AsyncCommandSender = std::sync::mpsc::Sender<std::process::Child>;

fn command_process(command: &str) -> std::process::Command {
    let mut process = std::process::Command::new(COMMAND_PROCESSOR);
    process.arg("-c").arg(command);
    process
}

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
    let child = command_process(command).spawn()?;
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

fn store_execute_command_failure(
    cmdstat: *mut i32,
    cmdmsg: *mut u8,
    cmdmsg_len: i64,
    status: i32,
    message: &str,
) {
    store_optional_i32(cmdstat, status);
    write_character_result(cmdmsg, cmdmsg_len, message.as_bytes());
}

fn execute_command_line(
    command: *const u8,
    cmd_len: i64,
    wait: i32,
    exitstat: *mut i32,
    cmdstat: *mut i32,
    cmdmsg: *mut u8,
    cmdmsg_len: i64,
) {
    let cmd = if !command.is_null() && cmd_len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(command, cmd_len as usize) };
        String::from_utf8_lossy(slice).trim().to_string()
    } else {
        store_execute_command_failure(
            cmdstat,
            cmdmsg,
            cmdmsg_len,
            1,
            "command argument is absent or has zero length",
        );
        return;
    };

    if wait != 0 {
        match command_process(&cmd).status() {
            Ok(status) => {
                if !exitstat.is_null() {
                    unsafe {
                        *exitstat = status.code().unwrap_or(-1);
                    }
                }
                store_optional_i32(cmdstat, 0);
            }
            Err(error) => {
                store_execute_command_failure(
                    cmdstat,
                    cmdmsg,
                    cmdmsg_len,
                    -1,
                    &format!("could not execute command: {error}"),
                );
            }
        }
    } else {
        match spawn_async_command(&cmd) {
            Ok(_) => {
                store_optional_i32(cmdstat, 0);
            }
            Err(error) => {
                store_execute_command_failure(
                    cmdstat,
                    cmdmsg,
                    cmdmsg_len,
                    -1,
                    &format!("could not start asynchronous command: {error}"),
                );
            }
        }
    }
}

/// Compatibility entry point for compiler output predating CMDMSG support.
#[no_mangle]
pub extern "C" fn afs_execute_command_line(
    command: *const u8,
    cmd_len: i64,
    wait: i32,
    exitstat: *mut i32,
    cmdstat: *mut i32,
) {
    execute_command_line(
        command,
        cmd_len,
        wait,
        exitstat,
        cmdstat,
        std::ptr::null_mut(),
        0,
    );
}

#[no_mangle]
pub extern "C" fn afs_execute_command_line_cmdmsg(
    command: *const u8,
    cmd_len: i64,
    wait: i32,
    exitstat: *mut i32,
    cmdstat: *mut i32,
    cmdmsg: *mut u8,
    cmdmsg_len: i64,
) {
    execute_command_line(
        command, cmd_len, wait, exitstat, cmdstat, cmdmsg, cmdmsg_len,
    );
}

// Shared RNG state for RANDOM_NUMBER / RANDOM_SEED.
use crate::descriptor::{ArrayDescriptor, MAX_RANK};
use std::cell::Cell;
use std::ptr;
thread_local! {
    static RNG_SEED: Cell<u64> = const { Cell::new(0) };
    static RNG_INITIALIZED: Cell<bool> = const { Cell::new(false) };
}

const RANDOM_SEED_VECTOR_SIZE: i64 = 1;
const RANDOM_INIT_REPEATABLE_SEED: u64 = 0x243f_6a88_85a3_08d3;

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

fn random_unit_f32(bits: u64) -> f32 {
    ((bits >> 40) as f32) / (1u32 << 24) as f32
}

fn random_unit_f64(bits: u64) -> f64 {
    (bits >> 11) as f64 / (1u64 << 53) as f64
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
        *harvest = random_unit_f32(x);
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
        *harvest = random_unit_f64(x);
    }
}

/// Compatibility entry for older objects whose f32 HARVEST is contiguous.
/// New compiler output uses the descriptor entry below.
#[no_mangle]
pub extern "C" fn afs_random_number_array_f32(harvest: *mut f32, n: i64) {
    if harvest.is_null() || n <= 0 {
        return;
    }
    for i in 0..n {
        let value = random_unit_f32(next_random_u64());
        unsafe {
            *harvest.offset(i as isize) = value;
        }
    }
}

/// Compatibility entry for older objects whose f64 HARVEST is contiguous.
/// New compiler output uses the descriptor entry below.
#[no_mangle]
pub extern "C" fn afs_random_number_array_f64(harvest: *mut f64, n: i64) {
    if harvest.is_null() || n <= 0 {
        return;
    }
    for i in 0..n {
        let value = random_unit_f64(next_random_u64());
        unsafe {
            *harvest.offset(i as isize) = value;
        }
    }
}

fn fill_random_number_descriptor(harvest: *mut ArrayDescriptor) -> Result<(), &'static str> {
    if harvest.is_null() {
        return Err("HARVEST descriptor is null");
    }

    let descriptor = unsafe { &*harvest };
    let rank = usize::try_from(descriptor.rank)
        .ok()
        .filter(|rank| (1..=MAX_RANK).contains(rank))
        .ok_or("HARVEST descriptor rank is invalid")?;
    if !matches!(descriptor.elem_size, 4 | 8) {
        return Err("HARVEST has an unsupported REAL kind");
    }

    let mut extents = [0_i64; MAX_RANK];
    let mut total = 1_usize;
    let mut min_element_offset = 0_i128;
    let mut max_element_offset = 0_i128;
    for (index, dim) in descriptor.dims.iter().copied().take(rank).enumerate() {
        let extent = if dim.upper_bound < dim.lower_bound {
            0
        } else {
            dim.upper_bound
                .checked_sub(dim.lower_bound)
                .and_then(|span| span.checked_add(1))
                .ok_or("HARVEST extent overflows the descriptor ABI")?
        };
        extents[index] = extent;
        total = total
            .checked_mul(
                usize::try_from(extent).map_err(|_| "HARVEST extent exceeds the address space")?,
            )
            .ok_or("HARVEST element count exceeds the address space")?;
        if extent > 1 && dim.stride == 0 {
            return Err("HARVEST has a zero element stride");
        }

        let last_offset = i128::from(extent.saturating_sub(1))
            .checked_mul(i128::from(dim.stride))
            .ok_or("HARVEST stride span overflows the descriptor ABI")?;
        if last_offset < 0 {
            min_element_offset = min_element_offset
                .checked_add(last_offset)
                .ok_or("HARVEST offset overflows the descriptor ABI")?;
        } else {
            max_element_offset = max_element_offset
                .checked_add(last_offset)
                .ok_or("HARVEST offset overflows the descriptor ABI")?;
        }
    }

    if total == 0 {
        return Ok(());
    }
    if descriptor.base_addr.is_null() {
        return Err("HARVEST has no storage");
    }

    for boundary in [min_element_offset, max_element_offset] {
        let byte_offset = boundary
            .checked_mul(i128::from(descriptor.elem_size))
            .ok_or("HARVEST byte offset overflows the descriptor ABI")?;
        isize::try_from(byte_offset)
            .map_err(|_| "HARVEST byte offset exceeds the address space")?;
    }

    let mut byte_strides = [0_isize; MAX_RANK];
    let mut byte_rewinds = [0_isize; MAX_RANK];
    for dimension in 0..rank {
        if extents[dimension] <= 1 {
            continue;
        }
        let byte_stride = i128::from(descriptor.dims[dimension].stride)
            .checked_mul(i128::from(descriptor.elem_size))
            .ok_or("HARVEST byte stride overflows the descriptor ABI")?;
        byte_strides[dimension] = isize::try_from(byte_stride)
            .map_err(|_| "HARVEST byte stride exceeds the address space")?;
        let byte_rewind = byte_stride
            .checked_mul(i128::from(extents[dimension] - 1))
            .ok_or("HARVEST byte rewind overflows the descriptor ABI")?;
        byte_rewinds[dimension] = isize::try_from(byte_rewind)
            .map_err(|_| "HARVEST byte rewind exceeds the address space")?;
    }

    let mut indices = [0_i64; MAX_RANK];
    let mut byte_offset = 0_isize;
    for linear in 0..total {
        let destination = descriptor.base_addr.wrapping_offset(byte_offset);
        let bits = next_random_u64();
        unsafe {
            match descriptor.elem_size {
                4 => ptr::write_unaligned(destination as *mut f32, random_unit_f32(bits)),
                8 => ptr::write_unaligned(destination as *mut f64, random_unit_f64(bits)),
                _ => unreachable!("RANDOM_NUMBER element size was validated before storage"),
            }
        }

        if linear + 1 == total {
            break;
        }
        for dimension in 0..rank {
            indices[dimension] += 1;
            if indices[dimension] < extents[dimension] {
                byte_offset += byte_strides[dimension];
                break;
            }
            indices[dimension] = 0;
            byte_offset -= byte_rewinds[dimension];
        }
    }
    Ok(())
}

/// RANDOM_NUMBER on an array descriptor. New compiler output uses this entry
/// so noncontiguous and reverse sections retain every dimension's memory
/// stride. The raw pointer-and-count entries remain for older objects.
#[no_mangle]
pub extern "C" fn afs_random_number_array_desc(harvest: *mut ArrayDescriptor) {
    if let Err(message) = fill_random_number_descriptor(harvest) {
        eprintln!("Fortran runtime error: RANDOM_NUMBER {message}");
        std::process::exit(1);
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

/// RANDOM_INIT: reset RANDOM_NUMBER's generator.
///
/// ARMFORTAS currently executes a single Fortran image, so
/// IMAGE_DISTINCT cannot distinguish any peer image. REPEATABLE uses a
/// fixed processor seed; the nonrepeatable path retains RANDOM_SEED's
/// process-dependent initialization.
#[no_mangle]
pub extern "C" fn afs_random_init(repeatable: i32, _image_distinct: i32) {
    let seed = if repeatable != 0 {
        RANDOM_INIT_REPEATABLE_SEED
    } else {
        default_random_seed()
    };
    set_random_seed(seed);
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
    fn date_and_time_character_results_truncate_without_overwriting_guards() {
        const GUARD: u8 = 0xa5;

        let mut date = [GUARD; 9];
        let mut time = [GUARD; 11];
        let mut zone = [GUARD; 6];
        let snapshot = date_and_time_snapshot(
            unsafe { date.as_mut_ptr().add(1) },
            7,
            unsafe { time.as_mut_ptr().add(1) },
            9,
            unsafe { zone.as_mut_ptr().add(1) },
            4,
        );

        let expected_date = format!("{:04}{:02}{:02}", snapshot[0], snapshot[1], snapshot[2]);
        let expected_time = format!(
            "{:02}{:02}{:02}.{:03}",
            snapshot[4], snapshot[5], snapshot[6], snapshot[7]
        );
        let zone_minutes = snapshot[3].unsigned_abs();
        let expected_zone = format!(
            "{}{:02}{:02}",
            if snapshot[3] >= 0 { '+' } else { '-' },
            zone_minutes / 60,
            zone_minutes % 60
        );

        assert_eq!(&date[1..8], &expected_date.as_bytes()[..7]);
        assert_eq!(&time[1..10], &expected_time.as_bytes()[..9]);
        assert_eq!(&zone[1..5], &expected_zone.as_bytes()[..4]);
        assert_eq!((date[0], date[8]), (GUARD, GUARD));
        assert_eq!((time[0], time[10]), (GUARD, GUARD));
        assert_eq!((zone[0], zone[5]), (GUARD, GUARD));

        let mut zero_length = [GUARD; 3];
        date_and_time_snapshot(
            unsafe { zero_length.as_mut_ptr().add(1) },
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
        );
        assert_eq!(zero_length, [GUARD; 3]);
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
    fn execute_command_line_cmdmsg_reports_padded_and_truncated_failures() {
        let invalid_command = [0u8];
        let mut cmdstat = 0;
        let mut full_message = [b'?'; 96];
        afs_execute_command_line_cmdmsg(
            std::ptr::null(),
            0,
            1,
            std::ptr::null_mut(),
            &mut cmdstat,
            full_message.as_mut_ptr(),
            full_message.len() as i64,
        );
        assert_ne!(cmdstat, 0);
        assert_ne!(full_message[0], b'?');
        assert_eq!(full_message[full_message.len() - 1], b' ');

        let mut short_message = [b'?'; 8];
        afs_execute_command_line_cmdmsg(
            invalid_command.as_ptr(),
            invalid_command.len() as i64,
            0,
            std::ptr::null_mut(),
            &mut cmdstat,
            short_message.as_mut_ptr(),
            short_message.len() as i64,
        );
        assert_ne!(cmdstat, 0);
        assert!(!short_message.contains(&b'?'));
    }

    #[test]
    fn execute_command_line_cmdmsg_is_unchanged_after_successful_start() {
        let command = b"exit 0";
        let mut exitstat = i32::MIN;
        let mut cmdstat = i32::MIN;
        let mut message = *b"unchanged";
        afs_execute_command_line_cmdmsg(
            command.as_ptr(),
            command.len() as i64,
            1,
            &mut exitstat,
            &mut cmdstat,
            message.as_mut_ptr(),
            message.len() as i64,
        );
        assert_eq!(exitstat, 0);
        assert_eq!(cmdstat, 0);
        assert_eq!(&message, b"unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn command_processor_does_not_depend_on_path() {
        const MISSING_PATH: &str = "/armfortas-command-path-does-not-exist";

        let sync_status = command_process("exit 0")
            .env("PATH", MISSING_PATH)
            .status()
            .expect("synchronous command processor should start outside PATH");
        assert!(sync_status.success());

        let mut child = command_process("exit 0")
            .env("PATH", MISSING_PATH)
            .spawn()
            .expect("asynchronous command processor should start outside PATH");
        let async_status = child
            .wait()
            .expect("asynchronous command processor should be waitable");
        assert!(async_status.success());
    }

    #[test]
    fn environment_variable_name_honors_trim_name() {
        assert_eq!(environment_variable_name(b"PATH   ", true), b"PATH");
        assert_eq!(environment_variable_name(b"PATH   ", false), b"PATH   ");
        assert_eq!(environment_variable_name(b"   ", true), b"");
        assert_eq!(environment_variable_name(b"   ", false), b"   ");
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
    fn random_number_descriptor_walks_signed_multidimensional_strides() {
        let mut forward = [-1.0_f32; 16];
        let mut forward_desc = ArrayDescriptor::zeroed();
        forward_desc.base_addr = forward.as_mut_ptr() as *mut u8;
        forward_desc.elem_size = std::mem::size_of::<f32>() as i64;
        forward_desc.rank = 2;
        forward_desc.dims[0] = crate::descriptor::DimDescriptor {
            lower_bound: 1,
            upper_bound: 3,
            stride: 2,
        };
        forward_desc.dims[1] = crate::descriptor::DimDescriptor {
            lower_bound: 1,
            upper_bound: 2,
            stride: 8,
        };

        set_random_seed(11);
        fill_random_number_descriptor(&mut forward_desc)
            .expect("valid positive-stride descriptor should be filled");
        for (index, value) in forward.iter().copied().enumerate() {
            if [0, 2, 4, 8, 10, 12].contains(&index) {
                assert!((0.0..1.0).contains(&value), "forward[{index}]={value}");
            } else {
                assert_eq!(value, -1.0, "forward guard {index} was overwritten");
            }
        }

        let mut reverse = [-1.0_f64; 18];
        let mut reverse_desc = ArrayDescriptor::zeroed();
        reverse_desc.base_addr = unsafe { reverse.as_mut_ptr().add(14) as *mut u8 };
        reverse_desc.elem_size = std::mem::size_of::<f64>() as i64;
        reverse_desc.rank = 2;
        reverse_desc.dims[0] = crate::descriptor::DimDescriptor {
            lower_bound: 1,
            upper_bound: 2,
            stride: -2,
        };
        reverse_desc.dims[1] = crate::descriptor::DimDescriptor {
            lower_bound: 1,
            upper_bound: 2,
            stride: -5,
        };

        set_random_seed(17);
        fill_random_number_descriptor(&mut reverse_desc)
            .expect("valid negative-stride descriptor should be filled");
        for (index, value) in reverse.iter().copied().enumerate() {
            if [7, 9, 12, 14].contains(&index) {
                assert!((0.0..1.0).contains(&value), "reverse[{index}]={value}");
            } else {
                assert_eq!(value, -1.0, "reverse guard {index} was overwritten");
            }
        }
    }

    #[test]
    fn empty_random_number_descriptor_consumes_no_random_values() {
        let mut empty = ArrayDescriptor::zeroed();
        empty.elem_size = std::mem::size_of::<f64>() as i64;
        empty.rank = 1;
        empty.dims[0] = crate::descriptor::DimDescriptor {
            lower_bound: 1,
            upper_bound: 0,
            stride: 1,
        };

        set_random_seed(23);
        fill_random_number_descriptor(&mut empty)
            .expect("zero-extent HARVEST should be a successful no-op");
        let after_empty = next_random_u64();
        set_random_seed(23);
        let without_empty = next_random_u64();
        assert_eq!(after_empty, without_empty);
    }

    #[test]
    fn random_number_descriptor_rejects_repeated_storage() {
        let mut values = [-1.0_f64; 2];
        let mut descriptor = ArrayDescriptor::zeroed();
        descriptor.base_addr = values.as_mut_ptr() as *mut u8;
        descriptor.elem_size = std::mem::size_of::<f64>() as i64;
        descriptor.rank = 1;
        descriptor.dims[0] = crate::descriptor::DimDescriptor {
            lower_bound: 1,
            upper_bound: 2,
            stride: 0,
        };

        assert_eq!(
            fill_random_number_descriptor(&mut descriptor),
            Err("HARVEST has a zero element stride")
        );
        assert_eq!(values, [-1.0, -1.0]);
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

    #[test]
    fn random_init_repeatable_restarts_the_same_sequence() {
        let mut first = [0.0f64; 3];
        let mut advanced = [0.0f64; 3];
        let mut repeated = [0.0f64; 3];

        afs_random_init(1, 0);
        for value in &mut first {
            afs_random_number_f64(value);
        }
        for value in &mut advanced {
            afs_random_number_f64(value);
        }

        afs_random_init(1, 1);
        for value in &mut repeated {
            afs_random_number_f64(value);
        }

        assert_eq!(first, repeated);
        assert_ne!(first, advanced);
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
