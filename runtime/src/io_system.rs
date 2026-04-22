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
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

// ---- Global I/O state ----

use std::sync::OnceLock;

fn io_state() -> &'static Mutex<IoState> {
    static STATE: OnceLock<Mutex<IoState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(IoState::new()))
}

#[inline]
fn read_i128_ptr(src: *const i128) -> Option<i128> {
    if src.is_null() {
        None
    } else {
        Some(unsafe { std::ptr::read_unaligned(src) })
    }
}

#[inline]
fn write_i128_ptr(dst: *mut i128, value: i128) {
    if !dst.is_null() {
        unsafe { std::ptr::write_unaligned(dst, value) };
    }
}

// ---- Unit status types ----

#[derive(Debug, Clone, PartialEq)]
enum UnitStatus {
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// Raw file handle for direct/stream access (supports both read and write + seeking).
    FileRaw(File),
}

struct Unit {
    _number: i32,
    stream: UnitStream,
    filename: String,
    _status: UnitStatus,
    access: Access,
    form: Form,
    action: Action,
    /// Record length for direct access (in bytes). None for sequential/stream.
    recl: Option<i64>,
    /// Buffered tokens from the current input record for list-directed READ.
    read_tokens: Vec<String>,
    /// Cached formatted input record for the current READ statement.
    formatted_read_record: Option<String>,
    /// Cursor within a cached formatted input record for ADVANCE='NO' reads.
    formatted_read_cursor: usize,
}

impl Unit {
    fn is_stream_unformatted(&self) -> bool {
        self.form == Form::Unformatted && self.access == Access::Stream
    }

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
            UnitStream::FileRaw(f) => {
                f.write_all(data)?;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unit not open for writing",
                ))
            }
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
            UnitStream::FileRaw(f) => {
                let mut byte = [0u8; 1];
                loop {
                    match f.read(&mut byte)? {
                        0 => break,
                        1 => {
                            line.push(byte[0] as char);
                            if byte[0] == b'\n' {
                                break;
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unit not open for reading",
                ))
            }
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
            UnitStream::FileRaw(f) => f.flush(),
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
        units.insert(
            5,
            Unit {
                _number: 5,
                stream: UnitStream::Stdin,
                filename: "stdin".into(),
                _status: UnitStatus::Open,
                access: Access::Sequential,
                form: Form::Formatted,
                action: Action::Read,
                recl: None,
                read_tokens: Vec::new(),
                formatted_read_record: None,
                formatted_read_cursor: 0,
            },
        );
        units.insert(
            6,
            Unit {
                _number: 6,
                stream: UnitStream::Stdout,
                filename: "stdout".into(),
                _status: UnitStatus::Open,
                access: Access::Sequential,
                form: Form::Formatted,
                action: Action::Write,
                recl: None,
                read_tokens: Vec::new(),
                formatted_read_record: None,
                formatted_read_cursor: 0,
            },
        );
        units.insert(
            0,
            Unit {
                _number: 0,
                stream: UnitStream::Stderr,
                filename: "stderr".into(),
                _status: UnitStatus::Open,
                access: Access::Sequential,
                form: Form::Formatted,
                action: Action::Write,
                recl: None,
                read_tokens: Vec::new(),
                formatted_read_record: None,
                formatted_read_cursor: 0,
            },
        );

        Self {
            units,
            next_newunit: -10,
        }
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
/// OPEN control block — packed struct for passing all OPEN specifiers in one pointer.
#[repr(C)]
pub struct OpenControlBlock {
    pub unit: i32,
    pub filename: *const u8,
    pub filename_len: i64,
    pub status: *const u8,
    pub status_len: i64,
    pub action: *const u8,
    pub action_len: i64,
    pub access: *const u8,
    pub access_len: i64,
    pub form: *const u8,
    pub form_len: i64,
    pub recl: i64,
    pub iostat: *mut i32,
    pub newunit: *mut i32,
    pub position: *const u8,
    pub position_len: i64,
}

/// Simple OPEN with the most common specifiers (fits in 8 registers).
/// Used by the IR lowering for basic OPEN statements.
#[no_mangle]
pub extern "C" fn afs_open_simple(
    unit: i32,
    filename: *const u8,
    filename_len: i64,
    status: *const u8,
    status_len: i64,
    action: *const u8,
    action_len: i64,
) {
    let cb = OpenControlBlock {
        unit,
        filename,
        filename_len,
        status,
        status_len,
        action,
        action_len,
        access: std::ptr::null(),
        access_len: 0,
        form: std::ptr::null(),
        form_len: 0,
        recl: 0,
        iostat: std::ptr::null_mut(),
        newunit: std::ptr::null_mut(),
        position: std::ptr::null(),
        position_len: 0,
    };
    afs_open(&cb);
}

/// Open a file unit. Takes a pointer to an OpenControlBlock to avoid
/// exceeding the 8-register ARM64 calling convention limit.
#[no_mangle]
pub extern "C" fn afs_open(cb: *const OpenControlBlock) {
    if cb.is_null() {
        return;
    }
    let cb = unsafe { &*cb };
    let unit = cb.unit;
    let fname = unsafe_str(cb.filename, cb.filename_len);
    let status_str = unsafe_str(cb.status, cb.status_len).to_lowercase();
    let action_str = unsafe_str(cb.action, cb.action_len).to_lowercase();
    let access_str = unsafe_str(cb.access, cb.access_len).to_lowercase();
    let form_str = unsafe_str(cb.form, cb.form_len).to_lowercase();
    let position_str = unsafe_str(cb.position, cb.position_len).to_lowercase();
    let recl = cb.recl;
    let iostat = cb.iostat;
    let newunit = cb.newunit;

    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());

    // NEWUNIT: allocate a new unit number.
    let actual_unit = if !newunit.is_null() {
        let u = state.alloc_newunit();
        unsafe {
            *newunit = u;
        }
        u
    } else {
        unit
    };

    // Build OpenOptions based on status/action.
    let mut opts = OpenOptions::new();
    match status_str.trim() {
        "old" => {
            opts.read(true);
        }
        "new" => {
            opts.write(true).create_new(true);
        }
        "replace" => {
            opts.write(true).create(true).truncate(true);
        }
        "scratch" | "unknown" | "" => {
            opts.read(true).write(true).create(true);
        }
        _ => {
            opts.read(true).write(true).create(true);
        }
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
        "read" => {
            opts.read(true);
        }
        "write" => {
            opts.write(true).create(true);
        }
        _ => {
            opts.read(true).write(true).create(true);
        }
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
            let file_access = match access_str.trim() {
                "direct" => Access::Direct,
                "stream" => Access::Stream,
                _ => Access::Sequential,
            };
            let file_form = match form_str.trim() {
                "unformatted" => Form::Unformatted,
                _ => Form::Formatted,
            };

            // Direct, stream, and readwrite access use raw file handle for seeking.
            // ReadWrite needs FileRaw because BufWriter can't read and BufReader can't write.
            let stream = match file_access {
                Access::Direct | Access::Stream => UnitStream::FileRaw(file),
                Access::Sequential => match file_action {
                    Action::Read => UnitStream::FileRead(BufReader::new(file)),
                    Action::Write => UnitStream::FileWrite(BufWriter::new(file)),
                    Action::ReadWrite => UnitStream::FileRaw(file),
                },
            };
            state.units.insert(
                actual_unit,
                Unit {
                    _number: actual_unit,
                    stream,
                    filename: fname,
                    _status: UnitStatus::Open,
                    access: file_access,
                    form: file_form,
                    action: file_action,
                    recl: if recl > 0 { Some(recl) } else { None },
                    read_tokens: Vec::new(),
                    formatted_read_record: None,
                    formatted_read_cursor: 0,
                },
            );

            // Apply POSITION specifier.
            // Default: REWIND for sequential, ASIS for direct/stream.
            let pos = match position_str.trim() {
                "append" => Some(SeekFrom::End(0)),
                "rewind" => Some(SeekFrom::Start(0)),
                "asis" => None,
                "" => match file_access {
                    Access::Sequential => Some(SeekFrom::Start(0)),
                    _ => None,
                },
                _ => None,
            };
            if let Some(seek) = pos {
                if let Some(u) = state.get_unit(actual_unit) {
                    match &mut u.stream {
                        UnitStream::FileRaw(f) => {
                            let _ = f.seek(seek);
                        }
                        UnitStream::FileRead(r) => {
                            let _ = r.seek(seek);
                        }
                        UnitStream::FileWrite(w) => {
                            let _ = w.seek(seek);
                        }
                        _ => {}
                    }
                }
            }

            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
        }
        Err(e) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = e.raw_os_error().unwrap_or(1);
                }
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
    afs_close_ex(unit, std::ptr::null(), 0, iostat);
}

/// Close a unit with optional STATUS= semantics.
#[no_mangle]
pub extern "C" fn afs_close_ex(unit: i32, status: *const u8, status_len: i64, iostat: *mut i32) {
    let delete_on_close = if status.is_null() || status_len <= 0 {
        false
    } else {
        let raw = unsafe { std::slice::from_raw_parts(status, status_len as usize) };
        std::str::from_utf8(raw)
            .map(|s| s.trim().eq_ignore_ascii_case("delete"))
            .unwrap_or(false)
    };

    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut u) = state.units.remove(&unit) {
        let _ = u.flush();
        let filename = u.filename.clone();
        drop(u);

        let mut close_status = 0;
        if delete_on_close
            && !matches!(filename.as_str(), "stdin" | "stdout" | "stderr")
            && !filename.is_empty()
        {
            if let Err(e) = std::fs::remove_file(&filename) {
                close_status = e.raw_os_error().unwrap_or(1);
            }
        }

        if !iostat.is_null() {
            unsafe { *iostat = close_status };
        } else if close_status != 0 {
            eprintln!("CLOSE: {}: {}", filename, io::Error::from_raw_os_error(close_status));
            std::process::exit(1);
        }
    } else {
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        } // closing unopen unit is not an error
    }
}

// ---- Public C API: List-directed WRITE ----

/// Write an integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int(unit: i32, val: i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            let _ = u.write_raw(&val.to_ne_bytes());
        } else {
            let _ = u.write_str(&format!(" {}", val));
        }
    }
}

/// Write a 64-bit integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int64(unit: i32, val: i64) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            let _ = u.write_raw(&val.to_ne_bytes());
        } else {
            let _ = u.write_str(&format!(" {}", val));
        }
    }
}

/// Write a 128-bit integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int128(unit: i32, val: i128) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            let _ = u.write_raw(&val.to_ne_bytes());
        } else {
            let _ = u.write_str(&format!(" {}", val));
        }
    }
}

/// Write a real value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_real(unit: i32, val: f32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            let _ = u.write_raw(&val.to_ne_bytes());
        } else {
            let _ = u.write_str(&format!("  {:14.7E}", val));
        }
    }
}

/// Write a double value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_real64(unit: i32, val: f64) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            let _ = u.write_raw(&val.to_ne_bytes());
        } else {
            let _ = u.write_str(&format!("  {:22.15E}", val));
        }
    }
}

/// Write a complex(4) value (list-directed): " (re,im)".
/// `ptr` points to a two-element f32 array [real, imag].
#[no_mangle]
pub extern "C" fn afs_write_complex_f32(unit: i32, ptr: *const f32) {
    let (re, im) = unsafe { (*ptr, *ptr.add(1)) };
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            let _ = u.write_raw(&re.to_ne_bytes());
            let _ = u.write_raw(&im.to_ne_bytes());
        } else {
            let _ = u.write_str(&format!(" ({:14.7E},{:14.7E})", re, im));
        }
    }
}

/// Write a complex(8) value (list-directed): " (re,im)".
/// `ptr` points to a two-element f64 array [real, imag].
#[no_mangle]
pub extern "C" fn afs_write_complex_f64(unit: i32, ptr: *const f64) {
    let (re, im) = unsafe { (*ptr, *ptr.add(1)) };
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            let _ = u.write_raw(&re.to_ne_bytes());
            let _ = u.write_raw(&im.to_ne_bytes());
        } else {
            let _ = u.write_str(&format!(" ({:22.15E},{:22.15E})", re, im));
        }
    }
}

/// Write a character string (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_string(unit: i32, ptr: *const u8, len: i64) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            if !ptr.is_null() && len > 0 {
                let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                let _ = u.write_raw(slice);
            }
        } else {
            let _ = u.write_str(" ");
            if !ptr.is_null() && len > 0 {
                let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                let _ = u.write_bytes(slice);
            }
        }
    }
}

/// Write a logical value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_logical(unit: i32, val: i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            let _ = u.write_raw(&val.to_ne_bytes());
        } else {
            let _ = u.write_str(if val != 0 { " T" } else { " F" });
        }
    }
}

/// End a write statement (newline).
#[no_mangle]
pub extern "C" fn afs_write_newline(unit: i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_stream_unformatted() {
            let _ = u.flush();
            return;
        }
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
            Ok(Some(token)) => match token.parse::<i32>() {
                Ok(v) => {
                    if !val.is_null() {
                        unsafe {
                            *val = v;
                        }
                    }
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                Err(_) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    } else {
                        eprintln!("READ: cannot parse integer from '{}'", token);
                        std::process::exit(1);
                    }
                }
            },
            Ok(None) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = IOSTAT_END;
                    }
                } else {
                    eprintln!("READ: end of file");
                    std::process::exit(1);
                }
            }
            Err(e) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                } else {
                    eprintln!("READ: {}", e);
                    std::process::exit(1);
                }
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
            Ok(Some(token)) => match token.parse::<i64>() {
                Ok(v) => {
                    if !val.is_null() {
                        unsafe {
                            *val = v;
                        }
                    }
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                Err(_) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    } else {
                        eprintln!("READ: cannot parse integer from '{}'", token);
                        std::process::exit(1);
                    }
                }
            },
            Ok(None) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = IOSTAT_END;
                    }
                }
            }
            Err(_) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
            }
        }
    }
}

/// Read an i128 value (list-directed).
#[no_mangle]
pub extern "C" fn afs_read_int128(unit: i32, val: *mut i128, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match u.next_read_token() {
            Ok(Some(token)) => match token.parse::<i128>() {
                Ok(v) => {
                    write_i128_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                Err(_) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    } else {
                        eprintln!("READ: cannot parse integer from '{}'", token);
                        std::process::exit(1);
                    }
                }
            },
            Ok(None) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = IOSTAT_END;
                    }
                }
            }
            Err(_) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
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
                        if !val.is_null() {
                            unsafe {
                                *val = v;
                            }
                        }
                        if !iostat.is_null() {
                            unsafe {
                                *iostat = 0;
                            }
                        }
                    }
                    Err(_) => {
                        if !iostat.is_null() {
                            unsafe {
                                *iostat = 1;
                            }
                        } else {
                            eprintln!("READ: cannot parse real from '{}'", token);
                            std::process::exit(1);
                        }
                    }
                }
            }
            Ok(None) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = IOSTAT_END;
                    }
                } else {
                    eprintln!("READ: end of file");
                    std::process::exit(1);
                }
            }
            Err(_) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
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
                        if !val.is_null() {
                            unsafe {
                                *val = v;
                            }
                        }
                        if !iostat.is_null() {
                            unsafe {
                                *iostat = 0;
                            }
                        }
                    }
                    Err(_) => {
                        if !iostat.is_null() {
                            unsafe {
                                *iostat = 1;
                            }
                        } else {
                            eprintln!("READ: cannot parse real from '{}'", token);
                            std::process::exit(1);
                        }
                    }
                }
            }
            Ok(None) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = IOSTAT_END;
                    }
                }
            }
            Err(_) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
            }
        }
    }
}

/// Read a character value from an external unit.
///
/// For formatted/list-directed units this consumes the next token. For
/// stream-unformatted units it performs a raw byte read into the caller's
/// fixed-length character storage.
#[no_mangle]
pub extern "C" fn afs_read_string(unit: i32, dest: *mut u8, dest_len: i64, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let Some(u) = state.get_unit(unit) else {
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
        }
        return;
    };

    if dest_len < 0 {
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
        }
        return;
    }

    if u.form == Form::Unformatted && u.access == Access::Stream {
        let mut bytes = vec![b' '; dest_len as usize];
        match u.read_raw(&mut bytes) {
            Ok(0) => {
                crate::string::afs_assign_char_fixed(dest, dest_len, std::ptr::null(), 0);
                if !iostat.is_null() {
                    unsafe {
                        *iostat = IOSTAT_END;
                    }
                }
            }
            Ok(n) => {
                crate::string::afs_assign_char_fixed(dest, dest_len, bytes.as_ptr(), n as i64);
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
            Err(_) => {
                crate::string::afs_assign_char_fixed(dest, dest_len, std::ptr::null(), 0);
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
            }
        }
        return;
    }

    match u.next_read_token() {
        Ok(Some(token)) => {
            crate::string::afs_assign_char_fixed(
                dest,
                dest_len,
                token.as_ptr(),
                token.len() as i64,
            );
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
        }
        Ok(None) => {
            crate::string::afs_assign_char_fixed(dest, dest_len, std::ptr::null(), 0);
            if !iostat.is_null() {
                unsafe {
                    *iostat = IOSTAT_END;
                }
            }
        }
        Err(_) => {
            crate::string::afs_assign_char_fixed(dest, dest_len, std::ptr::null(), 0);
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
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

// ---- Direct access helpers ----

impl Unit {
    /// Seek to a specific record for direct access.
    /// Record numbers are 1-based. Returns Ok(()) or Err on failure.
    fn seek_to_record(&mut self, rec: i64) -> io::Result<()> {
        if rec < 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "direct access record number must be >= 1",
            ));
        }
        let recl = self.recl.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "direct access requires RECL")
        })?;
        let offset = (rec - 1)
            .checked_mul(recl)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "record offset overflow"))?;
        if offset < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative record offset",
            ));
        }
        match &mut self.stream {
            UnitStream::FileRaw(f) => {
                f.seek(SeekFrom::Start(offset as u64))?;
                Ok(())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unit not opened for direct access",
            )),
        }
    }

    /// Write raw bytes at the current file position (for unformatted/stream I/O).
    fn write_raw(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.stream {
            UnitStream::FileRaw(f) => f.write_all(data),
            UnitStream::FileWrite(w) => w.write_all(data),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unit not open for writing",
            )),
        }
    }

    /// Read raw bytes at the current file position (for unformatted/stream I/O).
    fn read_raw(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.stream {
            UnitStream::FileRaw(f) => f.read(buf),
            UnitStream::FileRead(r) => r.read(buf),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unit not open for reading",
            )),
        }
    }
}

/// Write a direct-access record (formatted string padded to recl).
#[no_mangle]
pub extern "C" fn afs_write_direct(
    unit: i32,
    rec: i64,
    data: *const u8,
    data_len: i64,
    iostat: *mut i32,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if let Err(e) = u.seek_to_record(rec) {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            } else {
                eprintln!("WRITE direct: {}", e);
                std::process::exit(1);
            }
            return;
        }
        let recl = u.recl.unwrap_or(0) as usize;
        let mut record = vec![b' '; recl]; // space-padded
        let copy_len = (data_len as usize).min(recl);
        if !data.is_null() && copy_len > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(data, record.as_mut_ptr(), copy_len);
            }
        }
        if let Err(e) = u.write_raw(&record) {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            } else {
                eprintln!("WRITE direct: {}", e);
                std::process::exit(1);
            }
            return;
        }
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        }
    }
}

/// Read a direct-access record.
#[no_mangle]
pub extern "C" fn afs_read_direct(
    unit: i32,
    rec: i64,
    data: *mut u8,
    data_len: i64,
    iostat: *mut i32,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if let Err(e) = u.seek_to_record(rec) {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            } else {
                eprintln!("READ direct: {}", e);
                std::process::exit(1);
            }
            return;
        }
        let recl = u.recl.unwrap_or(0) as usize;
        let mut record = vec![0u8; recl];
        match u.read_raw(&mut record) {
            Ok(n) => {
                let copy_len = n.min(data_len as usize);
                if !data.is_null() && copy_len > 0 {
                    unsafe {
                        std::ptr::copy_nonoverlapping(record.as_ptr(), data, copy_len);
                    }
                }
                // Pad remainder with spaces.
                if copy_len < data_len as usize {
                    unsafe {
                        std::ptr::write_bytes(
                            data.add(copy_len),
                            b' ',
                            data_len as usize - copy_len,
                        );
                    }
                }
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
            Err(e) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                } else {
                    eprintln!("READ direct: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}

// ---- Unformatted sequential I/O ----

/// Write an unformatted record with 4-byte length markers (gfortran-compatible).
#[no_mangle]
pub extern "C" fn afs_write_unformatted(
    unit: i32,
    data: *const u8,
    data_len: i64,
    iostat: *mut i32,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if data_len < 0 || data_len > u32::MAX as i64 {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            } else {
                eprintln!(
                    "WRITE unformatted: record length {} exceeds 4GB limit",
                    data_len
                );
                std::process::exit(1);
            }
            return;
        }
        let len_bytes = (data_len as u32).to_ne_bytes();
        // Write: [len][data][len]
        let r1 = u.write_raw(&len_bytes);
        let r2 = if !data.is_null() && data_len > 0 {
            let slice = unsafe { std::slice::from_raw_parts(data, data_len as usize) };
            u.write_raw(slice)
        } else {
            Ok(())
        };
        let r3 = u.write_raw(&len_bytes);
        if r1.is_err() || r2.is_err() || r3.is_err() {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        } else {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
        }
    }
}

/// Read an unformatted record with 4-byte length markers.
#[no_mangle]
pub extern "C" fn afs_read_unformatted(
    unit: i32,
    data: *mut u8,
    max_len: i64,
    actual_len: *mut i64,
    iostat: *mut i32,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        // Read leading length marker.
        let mut len_buf = [0u8; 4];
        match u.read_raw(&mut len_buf) {
            Ok(4) => {}
            Ok(0) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = IOSTAT_END;
                    }
                }
                return;
            }
            _ => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
                return;
            }
        }
        let record_len = u32::from_ne_bytes(len_buf) as usize;
        if !actual_len.is_null() {
            unsafe {
                *actual_len = record_len as i64;
            }
        }

        // Read record data.
        let read_len = record_len.min(max_len as usize);
        if !data.is_null() && read_len > 0 {
            let slice = unsafe { std::slice::from_raw_parts_mut(data, read_len) };
            let _ = u.read_raw(slice);
        }
        // Skip remaining if record is longer than buffer.
        if record_len > max_len as usize {
            let skip = record_len - max_len as usize;
            let mut trash = vec![0u8; skip];
            let _ = u.read_raw(&mut trash);
        }

        // Read trailing length marker (and discard).
        let _ = u.read_raw(&mut len_buf);
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        }
    }
}

// ---- Stream access helpers ----

/// Write raw bytes at the current stream position.
#[no_mangle]
pub extern "C" fn afs_write_stream(unit: i32, data: *const u8, data_len: i64, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if !data.is_null() && data_len > 0 {
            let slice = unsafe { std::slice::from_raw_parts(data, data_len as usize) };
            match u.write_raw(slice) {
                Ok(()) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                Err(_) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            }
        }
    }
}

/// Seek to an absolute byte position in a stream unit.
#[no_mangle]
pub extern "C" fn afs_seek_stream(unit: i32, pos: i64, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match &mut u.stream {
            UnitStream::FileRaw(f) => match f.seek(SeekFrom::Start(pos as u64)) {
                Ok(_) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                Err(_) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            },
            _ => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
            }
        }
    }
}

// ---- NAMELIST I/O ----

/// A NAMELIST entry describing one variable in a namelist group.
#[repr(C)]
pub struct NamelistEntry {
    pub name: *const u8,
    pub name_len: i64,
    pub data: *mut u8,
    pub data_type: i32, // 0=int, 1=real, 2=string, 3=logical
    pub data_len: i64,  // string length for type 2
}

/// Write a NAMELIST group to a unit.
/// Format: &GROUPNAME var=val, var=val, ... /
#[no_mangle]
pub extern "C" fn afs_write_namelist(
    unit: i32,
    group_name: *const u8,
    group_name_len: i64,
    entries: *const NamelistEntry,
    n_entries: i32,
) {
    let gname = unsafe_str(group_name, group_name_len);
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let _ = u.write_str(&format!(" &{}", gname.to_uppercase()));

        if !entries.is_null() && n_entries > 0 {
            let slice = unsafe { std::slice::from_raw_parts(entries, n_entries as usize) };
            for (i, entry) in slice.iter().enumerate() {
                let name = unsafe_str(entry.name, entry.name_len);
                let sep = if i > 0 { "," } else { "" };
                let val_str = match entry.data_type {
                    0 => {
                        // integer
                        let v = unsafe { *(entry.data as *const i32) };
                        format!("{}", v)
                    }
                    1 => {
                        // real
                        let v = unsafe { *(entry.data as *const f64) };
                        format!("{}", v)
                    }
                    2 => {
                        // string
                        let s = unsafe_str(entry.data, entry.data_len);
                        format!("'{}'", s.trim_end())
                    }
                    3 => {
                        // logical
                        let v = unsafe { *(entry.data as *const i32) };
                        (if v != 0 { ".TRUE." } else { ".FALSE." }).to_string()
                    }
                    _ => "???".to_string(),
                };
                let _ = u.write_str(&format!("{} {}={}", sep, name.to_uppercase(), val_str));
            }
        }
        let _ = u.write_str(" /\n");
        let _ = u.flush();
    }
}

/// Read a NAMELIST group from a unit.
/// Parses &GROUPNAME var=val, ... / format.
#[no_mangle]
pub extern "C" fn afs_read_namelist(
    unit: i32,
    group_name: *const u8,
    group_name_len: i64,
    entries: *const NamelistEntry,
    n_entries: i32,
    iostat: *mut i32,
) {
    let gname = unsafe_str(group_name, group_name_len).to_lowercase();
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        // Read lines until we find &groupname.
        let mut all_lines = String::new();
        loop {
            match u.read_line() {
                Ok(line) => {
                    let trimmed = line.trim().to_lowercase();
                    if trimmed.starts_with('&') && trimmed[1..].starts_with(&gname) {
                        all_lines.push_str(&line);
                        // Keep reading until we find '/'.
                        while !all_lines.contains('/') {
                            match u.read_line() {
                                Ok(cont) => all_lines.push_str(&cont),
                                Err(_) => break,
                            }
                        }
                        break;
                    }
                }
                Err(_) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = IOSTAT_END;
                        }
                    }
                    return;
                }
            }
        }

        // Parse assignments from the namelist text.
        if !entries.is_null() && n_entries > 0 {
            let entries_slice = unsafe { std::slice::from_raw_parts(entries, n_entries as usize) };
            // Extract the content between & and /.
            let content = if let Some(start) = all_lines.find(&gname) {
                let after_name = &all_lines[start + gname.len()..];
                if let Some(end) = after_name.find('/') {
                    &after_name[..end]
                } else {
                    after_name
                }
            } else {
                ""
            };

            // Parse var=val pairs. Supports:
            //   var=val            — simple scalar assignment
            //   var(index)=val     — array element assignment (1-based)
            //   var=n*val          — repeat notation (set n consecutive elements)
            for pair in content.split(',') {
                let pair = pair.trim();
                if let Some(eq_pos) = pair.find('=') {
                    let lhs = pair[..eq_pos].trim().to_lowercase();
                    let val_str = pair[eq_pos + 1..].trim();

                    // Parse array index from "var(idx)" syntax.
                    let (var_name, array_index) = if let Some(paren) = lhs.find('(') {
                        let name = lhs[..paren].trim();
                        let idx_str = lhs[paren + 1..].trim_end_matches(')').trim();
                        let idx = idx_str.parse::<usize>().unwrap_or(1);
                        (name.to_string(), Some(idx))
                    } else {
                        (lhs, None)
                    };

                    // Parse repeat notation "n*val".
                    let (repeat_count, actual_val) = if let Some(star) = val_str.find('*') {
                        // Make sure * is preceded by digits (not part of a number like 1.5E*).
                        let before = val_str[..star].trim();
                        if let Ok(n) = before.parse::<usize>() {
                            (n, val_str[star + 1..].trim())
                        } else {
                            (1, val_str)
                        }
                    } else {
                        (1, val_str)
                    };

                    // Find the matching entry.
                    for entry in entries_slice {
                        if entry.data.is_null() {
                            continue;
                        }
                        let ename = unsafe_str(entry.name, entry.name_len).to_lowercase();
                        if ename == var_name {
                            namelist_assign_value(entry, actual_val, array_index, repeat_count);
                            break;
                        }
                    }
                }
            }
        }
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        }
    }
}

/// Assign a parsed NAMELIST value to an entry, handling array indexing and repeat.
fn namelist_assign_value(
    entry: &NamelistEntry,
    val_str: &str,
    index: Option<usize>,
    repeat: usize,
) {
    // For array elements, compute byte offset from 1-based index.
    let elem_size = match entry.data_type {
        0 => 4, // integer (i32)
        1 => 8, // real (f64)
        3 => 4, // logical (i32)
        _ => 1, // string
    };
    let base_offset = index
        .map(|i| (i.saturating_sub(1)) * elem_size)
        .unwrap_or(0);

    for r in 0..repeat {
        let offset = base_offset + r * elem_size;
        let ptr = unsafe { entry.data.add(offset) };
        match entry.data_type {
            0 => {
                // integer
                if let Ok(v) = val_str.parse::<i32>() {
                    unsafe {
                        *(ptr as *mut i32) = v;
                    }
                }
            }
            1 => {
                // real
                let normalized = val_str.replace('d', "e").replace('D', "E");
                if let Ok(v) = normalized.parse::<f64>() {
                    unsafe {
                        *(ptr as *mut f64) = v;
                    }
                }
            }
            2 => {
                // string (only first element for repeat, no array stride for strings)
                let s = val_str.trim_matches('\'').trim_matches('"');
                let bytes = s.as_bytes();
                let copy_len = bytes.len().min(entry.data_len as usize);
                if copy_len > 0 {
                    unsafe {
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), entry.data, copy_len);
                        if copy_len < entry.data_len as usize {
                            std::ptr::write_bytes(
                                entry.data.add(copy_len),
                                b' ',
                                entry.data_len as usize - copy_len,
                            );
                        }
                    }
                }
                return; // string repeat doesn't make sense
            }
            3 => {
                // logical
                let lower = val_str.to_lowercase();
                let v = lower.starts_with(".t") || lower.starts_with("t");
                unsafe {
                    *(ptr as *mut i32) = v as i32;
                }
            }
            _ => {}
        }
    }
}

// ---- Internal I/O (read/write to character variables) ----

/// Write a formatted integer to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_int(
    buf: *mut u8,
    buf_len: i64,
    val: i32,
    pos: *mut i64, // current write position, updated after write
) {
    if buf.is_null() || buf_len <= 0 {
        return;
    }
    let s = format!(" {}", val);
    let start = if !pos.is_null() {
        (unsafe { *pos }) as usize
    } else {
        0
    };
    write_to_buffer(buf, buf_len as usize, start, s.as_bytes(), pos);
}

/// Write a formatted i64 to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_int64(buf: *mut u8, buf_len: i64, val: i64, pos: *mut i64) {
    if buf.is_null() || buf_len <= 0 {
        return;
    }
    let s = format!(" {}", val);
    let start = if !pos.is_null() {
        (unsafe { *pos }) as usize
    } else {
        0
    };
    write_to_buffer(buf, buf_len as usize, start, s.as_bytes(), pos);
}

/// Write a formatted integer(16) to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_int128(buf: *mut u8, buf_len: i64, val: i128, pos: *mut i64) {
    if buf.is_null() || buf_len <= 0 {
        return;
    }
    let s = format!(" {}", val);
    let start = if !pos.is_null() {
        (unsafe { *pos }) as usize
    } else {
        0
    };
    write_to_buffer(buf, buf_len as usize, start, s.as_bytes(), pos);
}

/// Write a formatted real to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_real64(buf: *mut u8, buf_len: i64, val: f64, pos: *mut i64) {
    if buf.is_null() || buf_len <= 0 {
        return;
    }
    let s = format!(" {}", val);
    let start = if !pos.is_null() {
        (unsafe { *pos }) as usize
    } else {
        0
    };
    write_to_buffer(buf, buf_len as usize, start, s.as_bytes(), pos);
}

/// Write a formatted string to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_string(
    buf: *mut u8,
    buf_len: i64,
    src: *const u8,
    src_len: i64,
    pos: *mut i64,
) {
    if buf.is_null() || buf_len <= 0 {
        return;
    }
    let start = if !pos.is_null() {
        (unsafe { *pos }) as usize
    } else {
        0
    };
    let mut data = vec![b' '];
    if !src.is_null() && src_len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(src, src_len as usize) };
        data.extend_from_slice(slice);
    }
    write_to_buffer(buf, buf_len as usize, start, &data, pos);
}

fn next_internal_token(buf: *const u8, buf_len: i64, pos: *mut i64) -> Option<String> {
    if buf.is_null() || buf_len <= 0 {
        return None;
    }

    let slice = unsafe { std::slice::from_raw_parts(buf, buf_len as usize) };
    let mut idx = if !pos.is_null() {
        unsafe { (*pos).clamp(0, buf_len) as usize }
    } else {
        0
    };

    while idx < slice.len() && (slice[idx].is_ascii_whitespace() || slice[idx] == b',') {
        idx += 1;
    }

    if idx >= slice.len() {
        if !pos.is_null() {
            unsafe {
                *pos = idx as i64;
            }
        }
        return None;
    }

    let start = idx;
    while idx < slice.len() && !slice[idx].is_ascii_whitespace() && slice[idx] != b',' {
        idx += 1;
    }

    if !pos.is_null() {
        unsafe {
            *pos = idx as i64;
        }
    }

    Some(String::from_utf8_lossy(&slice[start..idx]).into_owned())
}

/// Read an integer from a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_read_internal_int(
    buf: *const u8,
    buf_len: i64,
    pos: *mut i64,
    val: *mut i32,
    iostat: *mut i32,
) {
    if let Some(token) = next_internal_token(buf, buf_len, pos) {
        match token.replace(',', "").parse::<i32>() {
            Ok(v) => {
                if !val.is_null() {
                    unsafe {
                        *val = v;
                    }
                }
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
            Err(_) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
            }
        }
    } else {
        if !iostat.is_null() {
            unsafe {
                *iostat = -1;
            }
        }
    }
}

/// Read an i64 from a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_read_internal_int64(
    buf: *const u8,
    buf_len: i64,
    pos: *mut i64,
    val: *mut i64,
    iostat: *mut i32,
) {
    if let Some(token) = next_internal_token(buf, buf_len, pos) {
        match token.replace(',', "").parse::<i64>() {
            Ok(v) => {
                if !val.is_null() {
                    unsafe {
                        *val = v;
                    }
                }
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
            Err(_) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
            }
        }
    } else {
        if !iostat.is_null() {
            unsafe {
                *iostat = -1;
            }
        }
    }
}

/// Read an integer(16) from a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_read_internal_int128(
    buf: *const u8,
    buf_len: i64,
    pos: *mut i64,
    val: *mut i128,
    iostat: *mut i32,
) {
    if let Some(token) = next_internal_token(buf, buf_len, pos) {
        match token.replace(',', "").parse::<i128>() {
            Ok(v) => {
                write_i128_ptr(val, v);
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
            Err(_) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
            }
        }
    } else {
        if !iostat.is_null() {
            unsafe {
                *iostat = -1;
            }
        }
    }
}

/// Read a real from a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_read_internal_real(
    buf: *const u8,
    buf_len: i64,
    pos: *mut i64,
    val: *mut f64,
    iostat: *mut i32,
) {
    if let Some(token) = next_internal_token(buf, buf_len, pos) {
        let normalized = token.replace('d', "e").replace('D', "E").replace(',', "");
        match normalized.parse::<f64>() {
            Ok(v) => {
                if !val.is_null() {
                    unsafe {
                        *val = v;
                    }
                }
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
            Err(_) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
            }
        }
    } else {
        if !iostat.is_null() {
            unsafe {
                *iostat = -1;
            }
        }
    }
}

/// Helper: write bytes into a buffer at a given position, space-pad remainder.
fn write_to_buffer(buf: *mut u8, buf_len: usize, start: usize, data: &[u8], pos: *mut i64) {
    let copy_len = data.len().min(buf_len.saturating_sub(start));
    if copy_len > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf.add(start), copy_len);
        }
    }
    // Space-pad remaining buffer.
    let end = start + copy_len;
    if end < buf_len {
        unsafe {
            std::ptr::write_bytes(buf.add(end), b' ', buf_len - end);
        }
    }
    if !pos.is_null() {
        unsafe {
            *pos = end as i64;
        }
    }
}

// ---- BACKSPACE / ENDFILE ----

/// Backspace one record on a sequential unit.
/// For formatted: seek backwards past the previous newline.
#[no_mangle]
pub extern "C" fn afs_backspace(unit: i32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match &mut u.stream {
            UnitStream::FileRaw(f) => {
                // Simple approach: seek backwards byte-by-byte to find newline.
                let pos = f.stream_position().unwrap_or(0);
                if pos <= 1 {
                    let _ = f.seek(SeekFrom::Start(0));
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                    // Clear stale read tokens.
                    u.read_tokens.clear();
                    return;
                }
                // Skip the current newline at pos-1.
                let mut search_pos = pos - 2;
                loop {
                    f.seek(SeekFrom::Start(search_pos)).ok();
                    let mut byte = [0u8; 1];
                    if f.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    if byte[0] == b'\n' {
                        f.seek(SeekFrom::Start(search_pos + 1)).ok();
                        break;
                    }
                    if search_pos == 0 {
                        f.seek(SeekFrom::Start(0)).ok();
                        break;
                    }
                    search_pos -= 1;
                }
                // Clear stale read tokens after repositioning.
                u.read_tokens.clear();
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
            _ => {
                // Sequential buffered files: backspace is not well-supported.
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
        }
    }
}

/// Write an end-of-file marker and truncate.
#[no_mangle]
pub extern "C" fn afs_endfile(unit: i32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let _ = u.flush();
        if let UnitStream::FileRaw(f) = &mut u.stream {
            let pos = f.stream_position().unwrap_or(0);
            let _ = f.set_len(pos); // truncate at current position
        }
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        }
    }
}

// ---- IOSTAT constants (iso_fortran_env) ----

/// IOSTAT_END: end-of-file encountered during input.
pub const IOSTAT_END: i32 = -1;
/// IOSTAT_EOR: end-of-record encountered during non-advancing input.
pub const IOSTAT_EOR: i32 = -2;

// ---- INQUIRE ----

/// Write a Fortran-style string result into a caller-provided buffer.
/// Pads with spaces to buf_len (Fortran CHARACTER semantics).
fn write_inquire_string(buf: *mut u8, buf_len: i64, value: &str) {
    if buf.is_null() || buf_len <= 0 {
        return;
    }
    let n = buf_len as usize;
    let val_bytes = value.as_bytes();
    let copy_len = val_bytes.len().min(n);
    unsafe {
        std::ptr::copy_nonoverlapping(val_bytes.as_ptr(), buf, copy_len);
        // Pad remainder with spaces.
        if copy_len < n {
            std::ptr::write_bytes(buf.add(copy_len), b' ', n - copy_len);
        }
    }
}

/// INQUIRE by file: check if a file exists, report its properties.
#[no_mangle]
pub extern "C" fn afs_inquire_file(
    filename: *const u8,
    filename_len: i64,
    exist: *mut i32,
    opened: *mut i32,
    iostat: *mut i32,
    // Extended specifiers — pass null for any not needed.
    name_buf: *mut u8,
    name_buf_len: i64,
    access_buf: *mut u8,
    access_buf_len: i64,
    form_buf: *mut u8,
    form_buf_len: i64,
    action_buf: *mut u8,
    action_buf_len: i64,
    recl_out: *mut i64,
    size_out: *mut i64,
) {
    let fname = unsafe_str(filename, filename_len);

    let file_exists = std::path::Path::new(&fname).exists();
    if !exist.is_null() {
        unsafe {
            *exist = file_exists as i32;
        }
    }

    // Find unit connected to this file (if any).
    let state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let connected_unit = state.units.values().find(|u| u.filename == fname);

    if !opened.is_null() {
        unsafe {
            *opened = connected_unit.is_some() as i32;
        }
    }

    write_inquire_string(name_buf, name_buf_len, &fname);

    if let Some(u) = connected_unit {
        write_unit_properties(
            u,
            access_buf,
            access_buf_len,
            form_buf,
            form_buf_len,
            action_buf,
            action_buf_len,
            recl_out,
        );
    } else {
        write_inquire_string(access_buf, access_buf_len, "UNDEFINED");
        write_inquire_string(form_buf, form_buf_len, "UNDEFINED");
        write_inquire_string(action_buf, action_buf_len, "UNDEFINED");
    }

    // File size via metadata.
    if !size_out.is_null() {
        let sz = std::fs::metadata(&fname)
            .map(|m| m.len() as i64)
            .unwrap_or(-1);
        unsafe {
            *size_out = sz;
        }
    }

    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
}

/// INQUIRE by unit: check if a unit is connected, report its properties.
#[no_mangle]
pub extern "C" fn afs_inquire_unit(
    unit: i32,
    exist: *mut i32,
    opened: *mut i32,
    iostat: *mut i32,
    // Extended specifiers.
    name_buf: *mut u8,
    name_buf_len: i64,
    access_buf: *mut u8,
    access_buf_len: i64,
    form_buf: *mut u8,
    form_buf_len: i64,
    action_buf: *mut u8,
    action_buf_len: i64,
    recl_out: *mut i64,
    size_out: *mut i64,
) {
    let state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let unit_entry = state.units.get(&unit);

    if !exist.is_null() {
        unsafe {
            *exist = unit_entry.is_some() as i32;
        }
    }
    if !opened.is_null() {
        unsafe {
            *opened = unit_entry.is_some() as i32;
        }
    }

    if let Some(u) = unit_entry {
        write_inquire_string(name_buf, name_buf_len, &u.filename);
        write_unit_properties(
            u,
            access_buf,
            access_buf_len,
            form_buf,
            form_buf_len,
            action_buf,
            action_buf_len,
            recl_out,
        );

        if !size_out.is_null() {
            let sz = if !u.filename.is_empty() {
                std::fs::metadata(&u.filename)
                    .map(|m| m.len() as i64)
                    .unwrap_or(-1)
            } else {
                -1
            };
            unsafe {
                *size_out = sz;
            }
        }
    } else {
        write_inquire_string(name_buf, name_buf_len, "");
        write_inquire_string(access_buf, access_buf_len, "UNDEFINED");
        write_inquire_string(form_buf, form_buf_len, "UNDEFINED");
        write_inquire_string(action_buf, action_buf_len, "UNDEFINED");
        if !size_out.is_null() {
            unsafe {
                *size_out = -1;
            }
        }
    }

    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
}

/// Write ACCESS, FORM, ACTION, RECL for a connected unit.
fn write_unit_properties(
    u: &Unit,
    access_buf: *mut u8,
    access_buf_len: i64,
    form_buf: *mut u8,
    form_buf_len: i64,
    action_buf: *mut u8,
    action_buf_len: i64,
    recl_out: *mut i64,
) {
    let access_str = match u.access {
        Access::Sequential => "SEQUENTIAL",
        Access::Direct => "DIRECT",
        Access::Stream => "STREAM",
    };
    write_inquire_string(access_buf, access_buf_len, access_str);

    let form_str = match u.form {
        Form::Formatted => "FORMATTED",
        Form::Unformatted => "UNFORMATTED",
    };
    write_inquire_string(form_buf, form_buf_len, form_str);

    let action_str = match u.action {
        Action::Read => "READ",
        Action::Write => "WRITE",
        Action::ReadWrite => "READWRITE",
    };
    write_inquire_string(action_buf, action_buf_len, action_str);

    if !recl_out.is_null() {
        unsafe {
            *recl_out = u.recl.unwrap_or(-1);
        }
    }
}

// ---- FLUSH ----

/// Flush a unit's output buffer.
#[no_mangle]
pub extern "C" fn afs_flush(unit: i32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match u.flush() {
            Ok(()) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
            Err(e) => {
                if !iostat.is_null() {
                    unsafe {
                        *iostat = e.raw_os_error().unwrap_or(1);
                    }
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
            UnitStream::FileRaw(f) => {
                let _ = f.seek(SeekFrom::Start(0));
            }
            _ => {}
        }
        // Clear stale read tokens so subsequent reads come from file start.
        u.read_tokens.clear();
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        }
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

// ---- Formatted I/O (push-based API) ----
//
// The format engine (format.rs) parses format strings and applies descriptors
// to values. This API lets compiled code push values one at a time, then flush
// the formatted output in one call.
//
// Usage from codegen:
//   afs_fmt_begin(unit, fmt_str, fmt_len)
//   afs_fmt_push_int(val) / afs_fmt_push_int128(&val) / afs_fmt_push_real(val) / ...
//   afs_fmt_end()

use crate::format::{parse_format, FormatDesc, FormatEngine, IoValue};
use std::cell::RefCell;

enum FmtSink {
    Unit(i32),
    Internal { buf: *mut u8, buf_len: usize },
}

/// Thread-local state for the current formatted I/O operation.
struct FmtContext {
    sink: FmtSink,
    format_str: String,
    values: Vec<IoValue>,
}

thread_local! {
    static FMT_CTX: RefCell<Option<FmtContext>> = const { RefCell::new(None) };
}

/// Begin a formatted write operation. Parses the format string and prepares
/// to accumulate values.
#[no_mangle]
pub extern "C" fn afs_fmt_begin(unit: i32, fmt_str: *const u8, fmt_len: i64) {
    let fmt = unsafe_str(fmt_str, fmt_len);
    FMT_CTX.with(|ctx| {
        *ctx.borrow_mut() = Some(FmtContext {
            sink: FmtSink::Unit(unit),
            format_str: fmt,
            values: Vec::new(),
        });
    });
}

/// Begin a formatted internal write operation targeting a character buffer.
#[no_mangle]
pub extern "C" fn afs_fmt_begin_internal(
    buf: *mut u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
) {
    let fmt = unsafe_str(fmt_str, fmt_len);
    FMT_CTX.with(|ctx| {
        *ctx.borrow_mut() = Some(FmtContext {
            sink: FmtSink::Internal {
                buf,
                buf_len: buf_len.max(0) as usize,
            },
            format_str: fmt,
            values: Vec::new(),
        });
    });
}

/// Push an integer value for formatted output.
#[no_mangle]
pub extern "C" fn afs_fmt_push_int(val: i64) {
    FMT_CTX.with(|ctx| {
        if let Some(ref mut c) = *ctx.borrow_mut() {
            c.values.push(IoValue::Integer(val as i128));
        }
    });
}

/// Push an integer(16) value for formatted output.
#[no_mangle]
pub extern "C" fn afs_fmt_push_int128(val: *const i128) {
    FMT_CTX.with(|ctx| {
        if let Some(ref mut c) = *ctx.borrow_mut() {
            if let Some(wide) = read_i128_ptr(val) {
                c.values.push(IoValue::Integer(wide));
            }
        }
    });
}

/// Push a real (f64) value for formatted output.
#[no_mangle]
pub extern "C" fn afs_fmt_push_real(val: f64) {
    FMT_CTX.with(|ctx| {
        if let Some(ref mut c) = *ctx.borrow_mut() {
            c.values.push(IoValue::Real(val));
        }
    });
}

/// Push a logical value for formatted output.
#[no_mangle]
pub extern "C" fn afs_fmt_push_logical(val: i32) {
    FMT_CTX.with(|ctx| {
        if let Some(ref mut c) = *ctx.borrow_mut() {
            c.values.push(IoValue::Logical(val != 0));
        }
    });
}

/// Push a character string value for formatted output.
#[no_mangle]
pub extern "C" fn afs_fmt_push_string(ptr: *const u8, len: i64) {
    let bytes = if !ptr.is_null() && len > 0 {
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec()
    } else {
        Vec::new()
    };
    FMT_CTX.with(|ctx| {
        if let Some(ref mut c) = *ctx.borrow_mut() {
            c.values.push(IoValue::Character(bytes));
        }
    });
}

/// End the formatted write: apply the format engine and write the result.
/// If advance is true (nonzero), appends a newline. If false (zero), no newline.
#[no_mangle]
pub extern "C" fn afs_fmt_end(advance: i32) {
    FMT_CTX.with(|ctx| {
        let context = ctx.borrow_mut().take();
        if let Some(c) = context {
            let descriptors = parse_format(&c.format_str);
            let mut engine = FormatEngine::new(descriptors);
            let output = engine.format_values(&c.values);

            match c.sink {
                FmtSink::Unit(unit) => {
                    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(u) = state.get_unit(unit) {
                        let _ = u.write_str(&output);
                        if advance != 0 {
                            let _ = u.write_str("\n");
                        }
                    }
                }
                FmtSink::Internal { buf, buf_len } => {
                    write_to_buffer(buf, buf_len, 0, output.as_bytes(), std::ptr::null_mut());
                }
            }
        }
    });
}

fn advance_formatted_cursor(desc: &FormatDesc, input: &[u8], cursor: &mut usize) {
    match desc {
        FormatDesc::Skip { count } => {
            *cursor = (*cursor).saturating_add(*count).min(input.len());
        }
        FormatDesc::TabTo { position } => {
            *cursor = position.saturating_sub(1).min(input.len());
        }
        FormatDesc::TabLeft { count } => {
            *cursor = cursor.saturating_sub(*count);
        }
        FormatDesc::TabRight { count } => {
            *cursor = (*cursor).saturating_add(*count).min(input.len());
        }
        FormatDesc::LiteralString(text) => {
            *cursor = (*cursor).saturating_add(text.len()).min(input.len());
        }
        FormatDesc::Newline => {
            while *cursor < input.len() && input[*cursor] != b'\n' {
                *cursor += 1;
            }
            if *cursor < input.len() {
                *cursor += 1;
            }
        }
        _ => {}
    }
}

fn read_formatted_field(desc: &FormatDesc, input: &[u8], cursor: &mut usize) -> Option<String> {
    let take_width = |cursor: &mut usize, width: usize| {
        let start = (*cursor).min(input.len());
        let end = start.saturating_add(width).min(input.len());
        *cursor = end;
        String::from_utf8_lossy(&input[start..end]).into_owned()
    };

    match desc {
        FormatDesc::IntegerI { width, .. }
        | FormatDesc::IntegerB { width, .. }
        | FormatDesc::IntegerO { width, .. }
        | FormatDesc::IntegerZ { width, .. }
        | FormatDesc::RealF { width, .. }
        | FormatDesc::RealE { width, .. }
        | FormatDesc::RealEN { width, .. }
        | FormatDesc::RealES { width, .. }
        | FormatDesc::RealEX { width, .. }
        | FormatDesc::RealD { width, .. }
        | FormatDesc::RealG { width, .. }
        | FormatDesc::Logical { width } => Some(take_width(cursor, *width)),
        FormatDesc::Character { width: Some(width) } => Some(take_width(cursor, *width)),
        FormatDesc::Character { width: None } => {
            let start = *cursor;
            *cursor = input.len();
            Some(String::from_utf8_lossy(&input[start..]).into_owned())
        }
        _ => None,
    }
}

fn extract_nth_formatted_field(
    descs: &[FormatDesc],
    input: &[u8],
    cursor: &mut usize,
    remaining_data_index: &mut usize,
) -> Option<(FormatDesc, String)> {
    for desc in descs {
        match desc {
            FormatDesc::Group {
                repeat,
                descriptors,
            } => {
                for _ in 0..*repeat {
                    if let Some(found) = extract_nth_formatted_field(
                        descriptors,
                        input,
                        cursor,
                        remaining_data_index,
                    ) {
                        return Some(found);
                    }
                }
            }
            FormatDesc::UnlimitedRepeat { descriptors } => {
                let mut loop_guard = 0usize;
                while *cursor < input.len() && loop_guard < input.len().saturating_add(1) {
                    let before = *cursor;
                    if let Some(found) = extract_nth_formatted_field(
                        descriptors,
                        input,
                        cursor,
                        remaining_data_index,
                    ) {
                        return Some(found);
                    }
                    if *cursor == before {
                        break;
                    }
                    loop_guard += 1;
                }
            }
            _ => {
                if let Some(field) = read_formatted_field(desc, input, cursor) {
                    if *remaining_data_index == 0 {
                        return Some((desc.clone(), field));
                    }
                    *remaining_data_index -= 1;
                } else {
                    advance_formatted_cursor(desc, input, cursor);
                }
            }
        }
    }

    None
}

fn parse_nth_formatted_record(
    input: &[u8],
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
) -> Result<(FormatDesc, String), i32> {
    let fmt = unsafe_str(fmt_str, fmt_len);
    let descs = parse_format(&fmt);
    let mut cursor = 0usize;
    let mut remaining = data_index.max(0) as usize;

    extract_nth_formatted_field(&descs, input, &mut cursor, &mut remaining).ok_or(-1)
}

fn parse_nth_formatted_internal_field(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
) -> Result<(FormatDesc, String), i32> {
    if buf.is_null() || buf_len <= 0 {
        return Err(-1);
    }

    let input = unsafe { std::slice::from_raw_parts(buf, buf_len as usize) };
    parse_nth_formatted_record(input, fmt_str, fmt_len, data_index)
}

fn formatted_read_record_for_unit(unit: i32, data_index: i64) -> Result<String, i32> {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let Some(u) = state.get_unit(unit) else {
        return Err(1);
    };

    if data_index <= 0 || u.formatted_read_record.is_none() {
        match u.read_line() {
            Ok(line) if !line.is_empty() => {
                u.formatted_read_record = Some(line);
            }
            Ok(_) => return Err(IOSTAT_END),
            Err(_) => return Err(1),
        }
    }

    u.formatted_read_record
        .as_ref()
        .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
        .ok_or(IOSTAT_END)
}

fn store_formatted_char_result(
    field: &str,
    dest: *mut u8,
    dest_len: i64,
    size_out: *mut i32,
    iostat: *mut i32,
) {
    crate::string::afs_assign_char_fixed(dest, dest_len, field.as_ptr(), field.len() as i64);
    if !size_out.is_null() {
        unsafe {
            *size_out = field.len().min(i32::MAX as usize) as i32;
        }
    }
    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
}

fn store_formatted_char_error(dest: *mut u8, dest_len: i64, size_out: *mut i32, code: i32) {
    crate::string::afs_assign_char_fixed(dest, dest_len, std::ptr::null(), 0);
    if !size_out.is_null() {
        unsafe {
            *size_out = 0;
        }
    }
    if code != 0 {
        // Caller writes IOSTAT when it passed a non-null pointer.
    }
}

fn parse_formatted_integer_field(desc: &FormatDesc, field: &str) -> Option<i128> {
    let trimmed = field.trim().replace(',', "");
    if trimmed.is_empty() {
        return None;
    }
    let (negative, digits) = if let Some(rest) = trimmed.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (false, rest)
    } else {
        (false, trimmed.as_str())
    };
    let radix = match desc {
        FormatDesc::IntegerI { .. } => 10,
        FormatDesc::IntegerB { .. } => 2,
        FormatDesc::IntegerO { .. } => 8,
        FormatDesc::IntegerZ { .. } => 16,
        _ => return None,
    };
    let parsed = i128::from_str_radix(digits, radix).ok()?;
    Some(if negative { -parsed } else { parsed })
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_string(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    dest: *mut u8,
    dest_len: i64,
    size_out: *mut i32,
    iostat: *mut i32,
) {
    match formatted_read_record_for_unit(unit, data_index)
        .and_then(|line| parse_nth_formatted_record(line.as_bytes(), fmt_str, fmt_len, data_index))
    {
        Ok((FormatDesc::Character { .. }, field)) => {
            store_formatted_char_result(&field, dest, dest_len, size_out, iostat);
        }
        Ok(_) => {
            store_formatted_char_error(dest, dest_len, size_out, 1);
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            store_formatted_char_error(dest, dest_len, size_out, code);
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_string_noadvance(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    dest: *mut u8,
    dest_len: i64,
    size_out: *mut i32,
    iostat: *mut i32,
) {
    let fmt = unsafe_str(fmt_str, fmt_len);
    let descs = parse_format(&fmt);

    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let Some(u) = state.get_unit(unit) else {
        store_formatted_char_error(dest, dest_len, size_out, 1);
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
        }
        return;
    };

    if u.formatted_read_record.is_none() {
        match u.read_line() {
            Ok(line) if !line.is_empty() => {
                u.formatted_read_record = Some(line.trim_end_matches(['\r', '\n']).to_string());
                u.formatted_read_cursor = 0;
            }
            Ok(_) => {
                store_formatted_char_error(dest, dest_len, size_out, IOSTAT_END);
                if !iostat.is_null() {
                    unsafe {
                        *iostat = IOSTAT_END;
                    }
                }
                return;
            }
            Err(_) => {
                store_formatted_char_error(dest, dest_len, size_out, 1);
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 1;
                    }
                }
                return;
            }
        }
    }

    let input = u
        .formatted_read_record
        .as_ref()
        .map(|line| line.as_bytes().to_vec())
        .unwrap_or_default();
    if u.formatted_read_cursor >= input.len() {
        store_formatted_char_error(dest, dest_len, size_out, IOSTAT_EOR);
        if !iostat.is_null() {
            unsafe {
                *iostat = IOSTAT_EOR;
            }
        }
        u.formatted_read_record = None;
        u.formatted_read_cursor = 0;
        return;
    }
    let mut cursor = u.formatted_read_cursor;
    let mut remaining = 0usize;

    match extract_nth_formatted_field(&descs, &input, &mut cursor, &mut remaining) {
        Some((FormatDesc::Character { .. }, field)) => {
            store_formatted_char_result(&field, dest, dest_len, size_out, iostat);
            u.formatted_read_cursor = cursor;
        }
        Some(_) => {
            store_formatted_char_error(dest, dest_len, size_out, 1);
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        None => {
            store_formatted_char_error(dest, dest_len, size_out, IOSTAT_EOR);
            if !iostat.is_null() {
                unsafe {
                    *iostat = IOSTAT_EOR;
                }
            }
            u.formatted_read_record = None;
            u.formatted_read_cursor = 0;
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_int(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut i32,
    iostat: *mut i32,
) {
    match formatted_read_record_for_unit(unit, data_index)
        .and_then(|line| parse_nth_formatted_record(line.as_bytes(), fmt_str, fmt_len, data_index))
    {
        Ok((desc @ FormatDesc::IntegerI { .. }, field))
        | Ok((desc @ FormatDesc::IntegerB { .. }, field))
        | Ok((desc @ FormatDesc::IntegerO { .. }, field))
        | Ok((desc @ FormatDesc::IntegerZ { .. }, field)) => {
            match parse_formatted_integer_field(&desc, &field).and_then(|v| i32::try_from(v).ok()) {
                Some(v) => {
                    if !val.is_null() {
                        unsafe {
                            *val = v;
                        }
                    }
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            }
        }
        Ok(_) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_int64(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut i64,
    iostat: *mut i32,
) {
    match formatted_read_record_for_unit(unit, data_index)
        .and_then(|line| parse_nth_formatted_record(line.as_bytes(), fmt_str, fmt_len, data_index))
    {
        Ok((desc @ FormatDesc::IntegerI { .. }, field))
        | Ok((desc @ FormatDesc::IntegerB { .. }, field))
        | Ok((desc @ FormatDesc::IntegerO { .. }, field))
        | Ok((desc @ FormatDesc::IntegerZ { .. }, field)) => {
            match parse_formatted_integer_field(&desc, &field).and_then(|v| i64::try_from(v).ok()) {
                Some(v) => {
                    if !val.is_null() {
                        unsafe {
                            *val = v;
                        }
                    }
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            }
        }
        Ok(_) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_int128(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut i128,
    iostat: *mut i32,
) {
    match formatted_read_record_for_unit(unit, data_index)
        .and_then(|line| parse_nth_formatted_record(line.as_bytes(), fmt_str, fmt_len, data_index))
    {
        Ok((desc @ FormatDesc::IntegerI { .. }, field))
        | Ok((desc @ FormatDesc::IntegerB { .. }, field))
        | Ok((desc @ FormatDesc::IntegerO { .. }, field))
        | Ok((desc @ FormatDesc::IntegerZ { .. }, field)) => {
            match parse_formatted_integer_field(&desc, &field) {
                Some(v) => {
                    write_i128_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            }
        }
        Ok(_) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_real(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut f64,
    iostat: *mut i32,
) {
    match formatted_read_record_for_unit(unit, data_index)
        .and_then(|line| parse_nth_formatted_record(line.as_bytes(), fmt_str, fmt_len, data_index))
    {
        Ok((FormatDesc::RealF { .. }, field))
        | Ok((FormatDesc::RealE { .. }, field))
        | Ok((FormatDesc::RealEN { .. }, field))
        | Ok((FormatDesc::RealES { .. }, field))
        | Ok((FormatDesc::RealEX { .. }, field))
        | Ok((FormatDesc::RealD { .. }, field))
        | Ok((FormatDesc::RealG { .. }, field)) => {
            let normalized = field
                .trim()
                .replace('d', "e")
                .replace('D', "E")
                .replace(',', "");
            match normalized.parse::<f64>() {
                Ok(v) => {
                    if !val.is_null() {
                        unsafe {
                            *val = v;
                        }
                    }
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                Err(_) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            }
        }
        Ok(_) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_string_internal(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    dest: *mut u8,
    dest_len: i64,
    size_out: *mut i32,
    iostat: *mut i32,
) {
    match parse_nth_formatted_internal_field(buf, buf_len, fmt_str, fmt_len, data_index) {
        Ok((FormatDesc::Character { .. }, field)) => {
            store_formatted_char_result(&field, dest, dest_len, size_out, iostat);
        }
        Ok(_) => {
            store_formatted_char_error(dest, dest_len, size_out, 1);
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            store_formatted_char_error(dest, dest_len, size_out, code);
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_int_internal(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut i32,
    iostat: *mut i32,
) {
    match parse_nth_formatted_internal_field(buf, buf_len, fmt_str, fmt_len, data_index) {
        Ok((desc @ FormatDesc::IntegerI { .. }, field))
        | Ok((desc @ FormatDesc::IntegerB { .. }, field))
        | Ok((desc @ FormatDesc::IntegerO { .. }, field))
        | Ok((desc @ FormatDesc::IntegerZ { .. }, field)) => {
            match parse_formatted_integer_field(&desc, &field).and_then(|v| i32::try_from(v).ok()) {
                Some(v) => {
                    if !val.is_null() {
                        unsafe {
                            *val = v;
                        }
                    }
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            }
        }
        Ok(_) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_int64_internal(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut i64,
    iostat: *mut i32,
) {
    match parse_nth_formatted_internal_field(buf, buf_len, fmt_str, fmt_len, data_index) {
        Ok((desc @ FormatDesc::IntegerI { .. }, field))
        | Ok((desc @ FormatDesc::IntegerB { .. }, field))
        | Ok((desc @ FormatDesc::IntegerO { .. }, field))
        | Ok((desc @ FormatDesc::IntegerZ { .. }, field)) => {
            match parse_formatted_integer_field(&desc, &field).and_then(|v| i64::try_from(v).ok()) {
                Some(v) => {
                    if !val.is_null() {
                        unsafe {
                            *val = v;
                        }
                    }
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            }
        }
        Ok(_) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_int128_internal(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut i128,
    iostat: *mut i32,
) {
    match parse_nth_formatted_internal_field(buf, buf_len, fmt_str, fmt_len, data_index) {
        Ok((desc @ FormatDesc::IntegerI { .. }, field))
        | Ok((desc @ FormatDesc::IntegerB { .. }, field))
        | Ok((desc @ FormatDesc::IntegerO { .. }, field))
        | Ok((desc @ FormatDesc::IntegerZ { .. }, field)) => {
            match parse_formatted_integer_field(&desc, &field) {
                Some(v) => {
                    write_i128_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            }
        }
        Ok(_) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_real_internal(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut f64,
    iostat: *mut i32,
) {
    match parse_nth_formatted_internal_field(buf, buf_len, fmt_str, fmt_len, data_index) {
        Ok((FormatDesc::RealF { .. }, field))
        | Ok((FormatDesc::RealE { .. }, field))
        | Ok((FormatDesc::RealEN { .. }, field))
        | Ok((FormatDesc::RealES { .. }, field))
        | Ok((FormatDesc::RealEX { .. }, field))
        | Ok((FormatDesc::RealD { .. }, field))
        | Ok((FormatDesc::RealG { .. }, field)) => {
            let normalized = field
                .trim()
                .replace('d', "e")
                .replace('D', "E")
                .replace(',', "");
            match normalized.parse::<f64>() {
                Ok(v) => {
                    if !val.is_null() {
                        unsafe {
                            *val = v;
                        }
                    }
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                Err(_) => {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 1;
                        }
                    }
                }
            }
        }
        Ok(_) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
        }
        Err(code) => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = code;
                }
            }
        }
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

    #[test]
    fn stream_unformatted_string_write_preserves_exact_bytes() {
        let path = "/tmp/afs_stream_unformatted_string_write.dat";
        let mut iostat = -99i32;
        let cb = OpenControlBlock {
            unit: 94,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "replace".as_ptr(),
            status_len: 7,
            action: "write".as_ptr(),
            action_len: 5,
            access: "stream".as_ptr(),
            access_len: 6,
            form: "unformatted".as_ptr(),
            form_len: 11,
            recl: 0,
            iostat: &mut iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
        };

        afs_open(&cb);
        assert_eq!(iostat, 0, "expected stream-unformatted OPEN to succeed");

        afs_write_string(94, "alpha".as_ptr(), 5);
        afs_write_newline(94);
        afs_close(94, std::ptr::null_mut());

        let content = std::fs::read(path).unwrap();
        assert_eq!(content, b"alpha", "expected exact stream bytes");
    }

    #[test]
    fn write_i128_to_file() {
        let path = "/tmp/afs_write_i128_test.dat";
        afs_open_simple(
            97,
            path.as_ptr(),
            path.len() as i64,
            "replace".as_ptr(),
            7,
            std::ptr::null(),
            0,
        );

        afs_write_int128(97, 170141183460469231731687303715884105727i128);
        afs_write_newline(97);
        afs_close(97, std::ptr::null_mut());

        let content = std::fs::read_to_string(path).unwrap();
        assert!(
            content.contains("170141183460469231731687303715884105727"),
            "expected full i128 decimal rendering in: {}",
            content
        );
    }

    #[test]
    fn read_i128_from_file() {
        let path = "/tmp/afs_read_i128_test.dat";
        std::fs::write(path, "170141183460469231731687303715884105727\n").unwrap();

        afs_open_simple(
            95,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let mut value = 0i128;
        let mut iostat = -99i32;
        afs_read_int128(95, &mut value, &mut iostat);
        afs_close(95, std::ptr::null_mut());

        assert_eq!(iostat, 0, "expected successful i128 read");
        assert_eq!(value, 170141183460469231731687303715884105727i128);
    }

    #[test]
    fn internal_i128_roundtrip_tracks_position() {
        let mut buf = [b' '; 96];
        let mut write_pos = 0i64;

        afs_write_internal_int128(
            buf.as_mut_ptr(),
            buf.len() as i64,
            170141183460469231731687303715884105727i128,
            &mut write_pos,
        );
        afs_write_internal_int128(
            buf.as_mut_ptr(),
            buf.len() as i64,
            -170141183460469231731687303715884105727i128,
            &mut write_pos,
        );

        let mut read_pos = 0i64;
        let mut first = 0i128;
        let mut second = 0i128;
        let mut iostat = -99i32;

        afs_read_internal_int128(
            buf.as_ptr(),
            buf.len() as i64,
            &mut read_pos,
            &mut first,
            &mut iostat,
        );
        assert_eq!(iostat, 0, "expected first internal i128 read to succeed");

        afs_read_internal_int128(
            buf.as_ptr(),
            buf.len() as i64,
            &mut read_pos,
            &mut second,
            &mut iostat,
        );
        assert_eq!(iostat, 0, "expected second internal i128 read to succeed");
        assert_eq!(first, 170141183460469231731687303715884105727i128);
        assert_eq!(second, -170141183460469231731687303715884105727i128);
    }

    #[test]
    fn internal_i128_read_accepts_unaligned_destination() {
        let buf = b"170141183460469231731687303715884105727";
        let mut pos = 0i64;
        let mut raw = [0u8; 32];
        let ptr = unsafe { raw.as_mut_ptr().add(1) as *mut i128 };
        let mut iostat = -99i32;

        afs_read_internal_int128(buf.as_ptr(), buf.len() as i64, &mut pos, ptr, &mut iostat);

        assert_eq!(iostat, 0, "expected internal i128 read to succeed");
        let value = unsafe { std::ptr::read_unaligned(ptr) };
        assert_eq!(value, 170141183460469231731687303715884105727i128);
    }

    #[test]
    fn formatted_write_to_file() {
        let path = "/tmp/afs_fmt_test.dat";
        afs_open_simple(
            99,
            path.as_ptr(),
            path.len() as i64,
            "replace".as_ptr(),
            7,
            std::ptr::null(),
            0,
        );

        afs_fmt_begin(99, "(I5, F8.2)".as_ptr(), 10);
        afs_fmt_push_int(42);
        afs_fmt_push_real(3.14);
        afs_fmt_end(1); // with newline

        afs_close(99, std::ptr::null_mut());

        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("42"), "expected 42 in: {}", content);
        assert!(content.contains("3.14"), "expected 3.14 in: {}", content);
    }

    #[test]
    fn formatted_write_integer16_to_file() {
        let path = "/tmp/afs_fmt_i128_test.dat";
        let wide = 170141183460469231731687303715884105727i128;
        afs_open_simple(
            96,
            path.as_ptr(),
            path.len() as i64,
            "replace".as_ptr(),
            7,
            std::ptr::null(),
            0,
        );

        afs_fmt_begin(96, "(I40)".as_ptr(), 5);
        afs_fmt_push_int128(&wide);
        afs_fmt_end(1);

        afs_close(96, std::ptr::null_mut());

        let content = std::fs::read_to_string(path).unwrap();
        assert!(
            content.contains("170141183460469231731687303715884105727"),
            "expected full formatted i128 rendering in: {}",
            content
        );
    }

    #[test]
    fn formatted_write_integer16_accepts_unaligned_pointer() {
        let mut rendered = [b' '; 64];
        let mut raw = [0u8; 32];
        let wide = 170141183460469231731687303715884105727i128;
        let ptr = unsafe { raw.as_mut_ptr().add(1) as *mut i128 };

        unsafe { std::ptr::write_unaligned(ptr, wide) };

        afs_fmt_begin_internal(
            rendered.as_mut_ptr(),
            rendered.len() as i64,
            "(I40)".as_ptr(),
            5,
        );
        afs_fmt_push_int128(ptr);
        afs_fmt_end(0);

        let text = String::from_utf8_lossy(&rendered).into_owned();
        assert!(
            text.contains("170141183460469231731687303715884105727"),
            "expected formatted internal write to accept unaligned i128 pointer: {:?}",
            text
        );
    }

    #[test]
    fn formatted_internal_write_pads_buffer() {
        let mut buf = [b'?'; 48];

        afs_fmt_begin_internal(buf.as_mut_ptr(), buf.len() as i64, "(I6)".as_ptr(), 4);
        afs_fmt_push_int(42);
        afs_fmt_end(0);

        let rendered = String::from_utf8_lossy(&buf).into_owned();
        assert!(
            rendered.starts_with("    42"),
            "expected formatted internal output at start of buffer: {:?}",
            rendered
        );
        assert!(
            rendered[6..].bytes().all(|b| b == b' '),
            "expected remaining internal buffer to be space padded: {:?}",
            rendered
        );
    }

    #[test]
    fn formatted_internal_read_i128_field() {
        let buf = b" 170141183460469231731687303715884105727";
        let mut value = 0i128;
        let mut iostat = -99i32;

        afs_fmt_read_int128_internal(
            buf.as_ptr(),
            buf.len() as i64,
            "(I40)".as_ptr(),
            5,
            0,
            &mut value,
            &mut iostat,
        );

        assert_eq!(
            iostat, 0,
            "expected formatted internal i128 read to succeed"
        );
        assert_eq!(value, 170141183460469231731687303715884105727i128);
    }

    #[test]
    fn formatted_internal_read_i128_accepts_unaligned_destination() {
        let buf = b" 170141183460469231731687303715884105727";
        let mut raw = [0u8; 32];
        let ptr = unsafe { raw.as_mut_ptr().add(1) as *mut i128 };
        let mut iostat = -99i32;

        afs_fmt_read_int128_internal(
            buf.as_ptr(),
            buf.len() as i64,
            "(I40)".as_ptr(),
            5,
            0,
            ptr,
            &mut iostat,
        );

        assert_eq!(
            iostat, 0,
            "expected formatted internal i128 read to succeed"
        );
        let value = unsafe { std::ptr::read_unaligned(ptr) };
        assert_eq!(value, 170141183460469231731687303715884105727i128);
    }

    #[test]
    fn formatted_internal_read_tracks_descriptor_index() {
        let buf = b"  42 7";
        let mut first = 0i32;
        let mut second = 0i32;
        let mut iostat = -99i32;

        afs_fmt_read_int_internal(
            buf.as_ptr(),
            buf.len() as i64,
            "(I4,1X,I1)".as_ptr(),
            10,
            0,
            &mut first,
            &mut iostat,
        );
        assert_eq!(iostat, 0);

        afs_fmt_read_int_internal(
            buf.as_ptr(),
            buf.len() as i64,
            "(I4,1X,I1)".as_ptr(),
            10,
            1,
            &mut second,
            &mut iostat,
        );
        assert_eq!(iostat, 0);
        assert_eq!(first, 42);
        assert_eq!(second, 7);
    }

    #[test]
    fn formatted_internal_read_supports_octal_descriptor() {
        let buf = b"077 22";
        let mut first = 0i32;
        let mut second = 0i32;
        let mut iostat = -99i32;

        afs_fmt_read_int_internal(
            buf.as_ptr(),
            buf.len() as i64,
            "(O3,1X,O2)".as_ptr(),
            10,
            0,
            &mut first,
            &mut iostat,
        );
        assert_eq!(iostat, 0);

        afs_fmt_read_int_internal(
            buf.as_ptr(),
            buf.len() as i64,
            "(O3,1X,O2)".as_ptr(),
            10,
            1,
            &mut second,
            &mut iostat,
        );
        assert_eq!(iostat, 0);
        assert_eq!(first, 63);
        assert_eq!(second, 18);
    }

    #[test]
    fn formatted_unit_read_i128_field() {
        let path = "/tmp/afs_fmt_read_i128_test.dat";
        std::fs::write(path, " 170141183460469231731687303715884105727\n").unwrap();

        afs_open_simple(
            94,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let mut value = 0i128;
        let mut iostat = -99i32;
        afs_fmt_read_int128(94, "(I40)".as_ptr(), 5, 0, &mut value, &mut iostat);
        afs_close(94, std::ptr::null_mut());

        assert_eq!(iostat, 0, "expected formatted unit i128 read to succeed");
        assert_eq!(value, 170141183460469231731687303715884105727i128);
    }

    #[test]
    fn formatted_unit_read_tracks_descriptor_index() {
        let path = "/tmp/afs_fmt_read_multi_test.dat";
        std::fs::write(path, " 170141183460469231731687303715884105727  42\n").unwrap();

        afs_open_simple(
            93,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let mut first = 0i128;
        let mut second = 0i32;
        let mut iostat = -99i32;
        afs_fmt_read_int128(93, "(I40,1X,I4)".as_ptr(), 10, 0, &mut first, &mut iostat);
        assert_eq!(iostat, 0);

        afs_fmt_read_int(93, "(I40,1X,I4)".as_ptr(), 10, 1, &mut second, &mut iostat);
        afs_close(93, std::ptr::null_mut());

        assert_eq!(iostat, 0);
        assert_eq!(first, 170141183460469231731687303715884105727i128);
        assert_eq!(second, 42);
    }

    #[test]
    fn formatted_readwrite_unit_advances_across_records() {
        let path = "/tmp/afs_fmt_read_records_rw_test.dat";
        std::fs::write(
            path,
            " 170141183460469231731687303715884105727\n-170141183460469231731687303715884105727\n",
        )
        .unwrap();

        afs_open_simple(
            92,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "readwrite".as_ptr(),
            9,
        );

        let mut first = 0i128;
        let mut second = 0i128;
        let mut iostat = -99i32;

        afs_fmt_read_int128(92, "(I40)".as_ptr(), 5, 0, &mut first, &mut iostat);
        assert_eq!(
            iostat, 0,
            "expected first formatted readwrite-unit read to succeed"
        );

        afs_fmt_read_int128(92, "(I40)".as_ptr(), 5, 0, &mut second, &mut iostat);
        afs_close(92, std::ptr::null_mut());

        assert_eq!(
            iostat, 0,
            "expected second formatted readwrite-unit read to succeed"
        );
        assert_eq!(first, 170141183460469231731687303715884105727i128);
        assert_eq!(second, -170141183460469231731687303715884105727i128);
    }

    #[test]
    fn formatted_write_no_advance() {
        let path = "/tmp/afs_fmt_noadv_test.dat";
        afs_open_simple(
            98,
            path.as_ptr(),
            path.len() as i64,
            "replace".as_ptr(),
            7,
            std::ptr::null(),
            0,
        );

        afs_fmt_begin(98, "('hello')".as_ptr(), 9);
        afs_fmt_end(0); // no newline

        afs_fmt_begin(98, "(' world')".as_ptr(), 10);
        afs_fmt_end(1); // with newline

        afs_close(98, std::ptr::null_mut());

        let content = std::fs::read_to_string(path).unwrap();
        assert_eq!(content.trim(), "hello world");
    }
}
