//! Fortran system intrinsics: clock, timing, command-line, environment.

/// SYSTEM_CLOCK: returns monotonic clock count, rate, and max.
/// count_rate is nanoseconds (1_000_000_000 ticks per second).
#[no_mangle]
pub extern "C" fn afs_system_clock(count: *mut i64, count_rate: *mut i64, count_max: *mut i64) {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let nanos = now.as_nanos() as i64;

    if !count.is_null() {
        unsafe {
            *count = nanos;
        }
    }
    if !count_rate.is_null() {
        unsafe {
            *count_rate = 1_000_000_000;
        }
    }
    if !count_max.is_null() {
        unsafe {
            *count_max = i64::MAX;
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

/// COMMAND_ARGUMENT_COUNT: returns argc - 1.
#[no_mangle]
pub extern "C" fn afs_command_argument_count() -> i32 {
    std::env::args().count() as i32 - 1
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
    let args: Vec<String> = std::env::args().collect();
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
    let full: String = std::env::args().collect::<Vec<_>>().join(" ");
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
use std::cell::Cell;
thread_local! {
    static RNG_SEED: Cell<u64> = const { Cell::new(12345678901234567) };
}

fn next_random_u64() -> u64 {
    RNG_SEED.with(|s| {
        let mut x = s.get();
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        s.set(x);
        x
    })
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

/// RANDOM_SEED: seed the random number generator.
#[no_mangle]
pub extern "C" fn afs_random_seed(seed_val: i64) {
    RNG_SEED.with(|s| s.set(seed_val as u64));
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
            assert!(x >= 0.0 && x < 1.0, "random out of range: {}", x);
        }
    }
}
