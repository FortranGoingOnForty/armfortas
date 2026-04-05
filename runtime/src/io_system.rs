//! Fortran I/O subsystem — unit management, list-directed and formatted I/O.
//!
//! The I/O state is global (Fortran I/O units are program-wide). Access
//! is protected by a mutex for future thread safety (DO CONCURRENT).
//!
//! Preconnected units:
//! - Unit 5 → stdin
//! - Unit 6 → stdout
//! - Unit 0 → stderr
//! - * in I/O statements → unit 5 (read) or 6 (write)

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write, BufRead, BufReader, BufWriter, Seek, SeekFrom};
use std::sync::Mutex;
use std::ptr;

// ---- Global I/O state ----

use std::sync::OnceLock;

fn io_state() -> &'static Mutex<IoState> {
    static STATE: OnceLock<Mutex<IoState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(IoState::new()))
}

// ---- Unit status types ----

#[derive(Debug, Clone, PartialEq)]
enum UnitStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq)]
enum Access {
    Sequential,
    Direct,
    Stream,
}

#[derive(Debug, Clone, PartialEq)]
enum Form {
    Formatted,
    Unformatted,
}

#[derive(Debug, Clone, PartialEq)]
enum Action {
    Read,
    Write,
    ReadWrite,
}

// ---- Unit ----

enum UnitStream {
    Stdin,
    Stdout,
    Stderr,
    FileRead(BufReader<File>),
    FileWrite(BufWriter<File>),
}

struct Unit {
    number: i32,
    stream: UnitStream,
    filename: String,
    status: UnitStatus,
    access: Access,
    form: Form,
    action: Action,
    /// Buffered tokens from the current input record for list-directed READ.
    /// Consumed left-to-right. Refilled when empty by reading the next line.
    read_tokens: Vec<String>,
}

impl Unit {
    fn write_bytes(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.stream {
            UnitStream::Stdout => {
                io::stdout().write_all(data)?;
            }
            UnitStream::Stderr => {
                io::stderr().write_all(data)?;
            }
            UnitStream::FileWrite(w) => {
                w.write_all(data)?;
            }
            _ => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unit not open for writing")),
        }
        Ok(())
    }

    fn write_str(&mut self, s: &str) -> io::Result<()> {
        self.write_bytes(s.as_bytes())
    }

    fn read_line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        match &mut self.stream {
            UnitStream::Stdin => {
                io::stdin().lock().read_line(&mut line)?;
            }
            UnitStream::FileRead(r) => {
                r.read_line(&mut line)?;
            }
            _ => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unit not open for reading")),
        }
        Ok(line)
    }

    /// Get the next token for list-directed READ.
    /// Reads a new line if the token buffer is empty.
    fn next_read_token(&mut self) -> io::Result<Option<String>> {
        // Consume from buffer first.
        if !self.read_tokens.is_empty() {
            return Ok(Some(self.read_tokens.remove(0)));
        }
        // Read a new line and tokenize.
        let line = self.read_line()?;
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            return Ok(None); // EOF or blank line
        }
        // Split on whitespace and commas.
        let tokens: Vec<String> = trimmed
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if tokens.is_empty() {
            return Ok(None);
        }
        self.read_tokens = tokens;
        Ok(Some(self.read_tokens.remove(0)))
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.stream {
            UnitStream::Stdout => io::stdout().flush(),
            UnitStream::Stderr => io::stderr().flush(),
            UnitStream::FileWrite(w) => w.flush(),
            _ => Ok(()),
        }
    }
}

// ---- I/O State ----

struct IoState {
    units: HashMap<i32, Unit>,
    next_newunit: i32,
}

impl IoState {
    fn new() -> Self {
        let mut units = HashMap::new();

        // Preconnected units.
        units.insert(5, Unit {
            number: 5,
            stream: UnitStream::Stdin,
            filename: "stdin".into(),
            status: UnitStatus::Open,
            access: Access::Sequential,
            form: Form::Formatted,
            action: Action::Read,
            read_tokens: Vec::new(),
        });
        units.insert(6, Unit {
            number: 6,
            stream: UnitStream::Stdout,
            filename: "stdout".into(),
            status: UnitStatus::Open,
            access: Access::Sequential,
            form: Form::Formatted,
            action: Action::Write,
            read_tokens: Vec::new(),
        });
        units.insert(0, Unit {
            number: 0,
            stream: UnitStream::Stderr,
            filename: "stderr".into(),
            status: UnitStatus::Open,
            access: Access::Sequential,
            form: Form::Formatted,
            action: Action::Write,
            read_tokens: Vec::new(),
        });

        Self { units, next_newunit: -10 }
    }

    fn get_unit(&mut self, unit_num: i32) -> Option<&mut Unit> {
        self.units.get_mut(&unit_num)
    }

    fn alloc_newunit(&mut self) -> i32 {
        let u = self.next_newunit;
        self.next_newunit -= 1;
        u
    }
}

// ---- Public C API: OPEN/CLOSE ----

/// Open a file and associate it with a unit number.
#[no_mangle]
pub extern "C" fn afs_open(
    unit: i32,
    filename: *const u8, filename_len: i64,
    status: *const u8, status_len: i64,
    action: *const u8, action_len: i64,
    iostat: *mut i32,
    newunit: *mut i32,
) {
    let fname = unsafe_str(filename, filename_len);
    let status_str = unsafe_str(status, status_len).to_lowercase();
    let action_str = unsafe_str(action, action_len).to_lowercase();

    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());

    // NEWUNIT: allocate a new unit number.
    let actual_unit = if !newunit.is_null() {
        let u = state.alloc_newunit();
        unsafe { *newunit = u; }
        u
    } else {
        unit
    };

    // Build OpenOptions based on status/action.
    let mut opts = OpenOptions::new();
    match status_str.trim() {
        "old" => { opts.read(true); }
        "new" => { opts.write(true).create_new(true); }
        "replace" => { opts.write(true).create(true).truncate(true); }
        "scratch" | "unknown" | "" => { opts.read(true).write(true).create(true); }
        _ => { opts.read(true).write(true).create(true); }
    }

    // Determine action. Default depends on status:
    // old → read, new/replace → write, scratch/unknown → readwrite.
    let effective_action = match action_str.trim() {
        "read" => "read",
        "write" => "write",
        "readwrite" => "readwrite",
        "" => match status_str.trim() {
            "old" => "read",
            "new" | "replace" => "write",
            _ => "readwrite",
        },
        _ => "readwrite",
    };

    match effective_action {
        "read" => { opts.read(true); }
        "write" => { opts.write(true).create(true); }
        _ => { opts.read(true).write(true).create(true); }
    }

    // Flush and close existing unit if re-opening.
    if let Some(mut existing) = state.units.remove(&actual_unit) {
        let _ = existing.flush();
        // Drop closes the file handle.
    }

    match opts.open(&fname) {
        Ok(file) => {
            let file_action = match effective_action {
                "read" => Action::Read,
                "write" => Action::Write,
                _ => Action::ReadWrite,
            };
            let stream = match file_action {
                Action::Read => UnitStream::FileRead(BufReader::new(file)),
                Action::Write | Action::ReadWrite => UnitStream::FileWrite(BufWriter::new(file)),
            };
            state.units.insert(actual_unit, Unit {
                number: actual_unit,
                stream,
                filename: fname,
                status: UnitStatus::Open,
                access: Access::Sequential,
                form: Form::Formatted,
                action: file_action,
                read_tokens: Vec::new(),
            });
            if !iostat.is_null() { unsafe { *iostat = 0; } }
        }
        Err(e) => {
            if !iostat.is_null() {
                unsafe { *iostat = e.raw_os_error().unwrap_or(1); }
            } else {
                eprintln!("OPEN: {}: {}", fname, e);
                std::process::exit(1);
            }
        }
    }
}

/// Close a unit.
#[no_mangle]
pub extern "C" fn afs_close(unit: i32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut u) = state.units.remove(&unit) {
        let _ = u.flush();
        // File is dropped here, closing the handle.
        if !iostat.is_null() { unsafe { *iostat = 0; } }
    } else {
        if !iostat.is_null() { unsafe { *iostat = 0; } } // closing unopen unit is not an error
    }
}

// ---- Public C API: List-directed WRITE ----

/// Write an integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int(unit: i32, val: i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let _ = u.write_str(&format!(" {}", val));
    }
}

/// Write a 64-bit integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int64(unit: i32, val: i64) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let _ = u.write_str(&format!(" {}", val));
    }
}

/// Write a real value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_real(unit: i32, val: f32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let _ = u.write_str(&format!("  {:14.7E}", val));
    }
}

/// Write a double value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_real64(unit: i32, val: f64) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let _ = u.write_str(&format!("  {:22.15E}", val));
    }
}

/// Write a character string (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_string(unit: i32, ptr: *const u8, len: i64) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let _ = u.write_str(" ");
        if !ptr.is_null() && len > 0 {
            let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
            let _ = u.write_bytes(slice);
        }
    }
}

/// Write a logical value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_logical(unit: i32, val: i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let _ = u.write_str(if val != 0 { " T" } else { " F" });
    }
}

/// End a write statement (newline).
#[no_mangle]
pub extern "C" fn afs_write_newline(unit: i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let _ = u.write_str("\n");
        let _ = u.flush();
    }
}

// ---- Public C API: List-directed READ ----

/// Read an i32 value (list-directed) from a unit.
/// Uses token buffer: multiple values on one line are consumed left-to-right.
#[no_mangle]
pub extern "C" fn afs_read_int(unit: i32, val: *mut i32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match u.next_read_token() {
            Ok(Some(token)) => {
                match token.parse::<i32>() {
                    Ok(v) => {
                        if !val.is_null() { unsafe { *val = v; } }
                        if !iostat.is_null() { unsafe { *iostat = 0; } }
                    }
                    Err(_) => {
                        if !iostat.is_null() { unsafe { *iostat = 1; } }
                        else { eprintln!("READ: cannot parse integer from '{}'", token); std::process::exit(1); }
                    }
                }
            }
            Ok(None) => {
                if !iostat.is_null() { unsafe { *iostat = IOSTAT_END; } }
                else { eprintln!("READ: end of file"); std::process::exit(1); }
            }
            Err(e) => {
                if !iostat.is_null() { unsafe { *iostat = 1; } }
                else { eprintln!("READ: {}", e); std::process::exit(1); }
            }
        }
    }
}

/// Read an i64 value (list-directed).
#[no_mangle]
pub extern "C" fn afs_read_int64(unit: i32, val: *mut i64, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match u.next_read_token() {
            Ok(Some(token)) => {
                match token.parse::<i64>() {
                    Ok(v) => {
                        if !val.is_null() { unsafe { *val = v; } }
                        if !iostat.is_null() { unsafe { *iostat = 0; } }
                    }
                    Err(_) => {
                        if !iostat.is_null() { unsafe { *iostat = 1; } }
                        else { eprintln!("READ: cannot parse integer from '{}'", token); std::process::exit(1); }
                    }
                }
            }
            Ok(None) => {
                if !iostat.is_null() { unsafe { *iostat = IOSTAT_END; } }
            }
            Err(_) => {
                if !iostat.is_null() { unsafe { *iostat = 1; } }
            }
        }
    }
}

/// Read an f32 value (list-directed).
#[no_mangle]
pub extern "C" fn afs_read_real(unit: i32, val: *mut f32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match u.next_read_token() {
            Ok(Some(token)) => {
                // Handle Fortran D-exponent: replace D with E for parsing.
                let normalized = token.replace('d', "e").replace('D', "E");
                match normalized.parse::<f32>() {
                    Ok(v) => {
                        if !val.is_null() { unsafe { *val = v; } }
                        if !iostat.is_null() { unsafe { *iostat = 0; } }
                    }
                    Err(_) => {
                        if !iostat.is_null() { unsafe { *iostat = 1; } }
                        else { eprintln!("READ: cannot parse real from '{}'", token); std::process::exit(1); }
                    }
                }
            }
            Ok(None) => {
                if !iostat.is_null() { unsafe { *iostat = IOSTAT_END; } }
                else { eprintln!("READ: end of file"); std::process::exit(1); }
            }
            Err(_) => {
                if !iostat.is_null() { unsafe { *iostat = 1; } }
            }
        }
    }
}

/// Read an f64 value (list-directed).
#[no_mangle]
pub extern "C" fn afs_read_real64(unit: i32, val: *mut f64, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match u.next_read_token() {
            Ok(Some(token)) => {
                let normalized = token.replace('d', "e").replace('D', "E");
                match normalized.parse::<f64>() {
                    Ok(v) => {
                        if !val.is_null() { unsafe { *val = v; } }
                        if !iostat.is_null() { unsafe { *iostat = 0; } }
                    }
                    Err(_) => {
                        if !iostat.is_null() { unsafe { *iostat = 1; } }
                        else { eprintln!("READ: cannot parse real from '{}'", token); std::process::exit(1); }
                    }
                }
            }
            Ok(None) => {
                if !iostat.is_null() { unsafe { *iostat = IOSTAT_END; } }
            }
            Err(_) => {
                if !iostat.is_null() { unsafe { *iostat = 1; } }
            }
        }
    }
}

// ---- Helpers ----

fn unsafe_str(ptr: *const u8, len: i64) -> String {
    if ptr.is_null() || len <= 0 {
        String::new()
    } else {
        let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        String::from_utf8_lossy(slice).into_owned()
    }
}

// ---- IOSTAT constants (iso_fortran_env) ----

/// IOSTAT_END: end-of-file encountered during input.
pub const IOSTAT_END: i32 = -1;
/// IOSTAT_EOR: end-of-record encountered during non-advancing input.
pub const IOSTAT_EOR: i32 = -2;

// ---- INQUIRE ----

/// INQUIRE by file: check if a file exists.
/// Sets exist to 1 (true) or 0 (false).
#[no_mangle]
pub extern "C" fn afs_inquire_file(
    filename: *const u8, filename_len: i64,
    exist: *mut i32,
    opened: *mut i32,
    iostat: *mut i32,
) {
    let fname = unsafe_str(filename, filename_len);

    let file_exists = std::path::Path::new(&fname).exists();
    if !exist.is_null() {
        unsafe { *exist = file_exists as i32; }
    }

    if !opened.is_null() {
        // Check if any unit has this file open.
        let state = io_state().lock().unwrap_or_else(|e| e.into_inner());
        let is_opened = state.units.values().any(|u| u.filename == fname);
        unsafe { *opened = is_opened as i32; }
    }

    if !iostat.is_null() {
        unsafe { *iostat = 0; }
    }
}

/// INQUIRE by unit: check if a unit is connected.
#[no_mangle]
pub extern "C" fn afs_inquire_unit(
    unit: i32,
    exist: *mut i32,
    opened: *mut i32,
    iostat: *mut i32,
) {
    let state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let unit_exists = state.units.contains_key(&unit);

    if !exist.is_null() {
        unsafe { *exist = unit_exists as i32; }
    }
    if !opened.is_null() {
        unsafe { *opened = unit_exists as i32; }
    }
    if !iostat.is_null() {
        unsafe { *iostat = 0; }
    }
}

// ---- FLUSH ----

/// Flush a unit's output buffer.
#[no_mangle]
pub extern "C" fn afs_flush(unit: i32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match u.flush() {
            Ok(()) => { if !iostat.is_null() { unsafe { *iostat = 0; } } }
            Err(e) => {
                if !iostat.is_null() {
                    unsafe { *iostat = e.raw_os_error().unwrap_or(1); }
                }
            }
        }
    }
}

// ---- REWIND / BACKSPACE / ENDFILE ----

/// Rewind a unit to the beginning.
#[no_mangle]
pub extern "C" fn afs_rewind(unit: i32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match &mut u.stream {
            UnitStream::FileRead(r) => {
                let _ = r.seek(SeekFrom::Start(0));
            }
            UnitStream::FileWrite(w) => {
                let _ = w.flush();
                let _ = w.seek(SeekFrom::Start(0));
            }
            _ => {}
        }
        if !iostat.is_null() { unsafe { *iostat = 0; } }
    }
}

// ---- Program lifecycle integration ----

/// Initialize the I/O subsystem. Called from afs_program_init.
#[no_mangle]
pub extern "C" fn afs_io_init() {
    // Force initialization of the global state.
    drop(io_state().lock());
}

/// Finalize the I/O subsystem. Flush and close all open units.
#[no_mangle]
pub extern "C" fn afs_io_finalize() {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    for (_, unit) in state.units.iter_mut() {
        let _ = unit.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preconnected_units() {
        let state = io_state().lock().unwrap_or_else(|e| e.into_inner());
        assert!(state.units.contains_key(&0)); // stderr
        assert!(state.units.contains_key(&5)); // stdin
        assert!(state.units.contains_key(&6)); // stdout
    }

    #[test]
    fn newunit_allocation() {
        let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
        let u1 = state.alloc_newunit();
        let u2 = state.alloc_newunit();
        assert!(u1 < 0); // negative unit numbers
        assert_ne!(u1, u2);
    }

    #[test]
    fn write_to_stdout() {
        // This test just verifies no panic — output goes to test runner's stdout.
        afs_write_int(6, 42);
        afs_write_newline(6);
    }
}
