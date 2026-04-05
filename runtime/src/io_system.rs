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
    /// Raw file handle for direct/stream access (supports both read and write + seeking).
    FileRaw(File),
}

struct Unit {
    number: i32,
    stream: UnitStream,
    filename: String,
    status: UnitStatus,
    access: Access,
    form: Form,
    action: Action,
    /// Record length for direct access (in bytes). None for sequential/stream.
    recl: Option<i64>,
    /// Buffered tokens from the current input record for list-directed READ.
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
            UnitStream::FileRaw(f) => {
                f.write_all(data)?;
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
            UnitStream::FileRaw(f) => {
                let mut br = BufReader::new(&mut *f);
                br.read_line(&mut line)?;
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
        units.insert(5, Unit {
            number: 5,
            stream: UnitStream::Stdin,
            filename: "stdin".into(),
            status: UnitStatus::Open,
            access: Access::Sequential,
            form: Form::Formatted,
            action: Action::Read,
            recl: None,
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
            recl: None,
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
            recl: None,
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
}

/// Simple OPEN with the most common specifiers (fits in 8 registers).
/// Used by the IR lowering for basic OPEN statements.
#[no_mangle]
pub extern "C" fn afs_open_simple(
    unit: i32,
    filename: *const u8, filename_len: i64,
    status: *const u8, status_len: i64,
    action: *const u8, action_len: i64,
) {
    let cb = OpenControlBlock {
        unit, filename, filename_len,
        status, status_len,
        action, action_len,
        access: std::ptr::null(), access_len: 0,
        form: std::ptr::null(), form_len: 0,
        recl: 0,
        iostat: std::ptr::null_mut(),
        newunit: std::ptr::null_mut(),
    };
    afs_open(&cb);
}

/// Open a file unit. Takes a pointer to an OpenControlBlock to avoid
/// exceeding the 8-register ARM64 calling convention limit.
#[no_mangle]
pub extern "C" fn afs_open(cb: *const OpenControlBlock) {
    if cb.is_null() { return; }
    let cb = unsafe { &*cb };
    let unit = cb.unit;
    let fname = unsafe_str(cb.filename, cb.filename_len);
    let status_str = unsafe_str(cb.status, cb.status_len).to_lowercase();
    let action_str = unsafe_str(cb.action, cb.action_len).to_lowercase();
    let access_str = unsafe_str(cb.access, cb.access_len).to_lowercase();
    let form_str = unsafe_str(cb.form, cb.form_len).to_lowercase();
    let recl = cb.recl;
    let iostat = cb.iostat;
    let newunit = cb.newunit;

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
            let file_access = match access_str.trim() {
                "direct" => Access::Direct,
                "stream" => Access::Stream,
                _ => Access::Sequential,
            };
            let file_form = match form_str.trim() {
                "unformatted" => Form::Unformatted,
                _ => Form::Formatted,
            };

            // Direct and stream access use raw file handle for seeking.
            let stream = match file_access {
                Access::Direct | Access::Stream => UnitStream::FileRaw(file),
                Access::Sequential => match file_action {
                    Action::Read => UnitStream::FileRead(BufReader::new(file)),
                    Action::Write | Action::ReadWrite => UnitStream::FileWrite(BufWriter::new(file)),
                },
            };
            state.units.insert(actual_unit, Unit {
                number: actual_unit,
                stream,
                filename: fname,
                status: UnitStatus::Open,
                access: file_access,
                form: file_form,
                action: file_action,
                recl: if recl > 0 { Some(recl) } else { None },
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

// ---- Direct access helpers ----

impl Unit {
    /// Seek to a specific record for direct access.
    /// Record numbers are 1-based. Returns Ok(()) or Err on failure.
    fn seek_to_record(&mut self, rec: i64) -> io::Result<()> {
        let recl = self.recl.ok_or_else(||
            io::Error::new(io::ErrorKind::InvalidInput, "direct access requires RECL"))?;
        let offset = (rec - 1) * recl;
        match &mut self.stream {
            UnitStream::FileRaw(f) => {
                f.seek(SeekFrom::Start(offset as u64))?;
                Ok(())
            }
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "unit not opened for direct access")),
        }
    }

    /// Write raw bytes at the current file position (for unformatted/stream I/O).
    fn write_raw(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.stream {
            UnitStream::FileRaw(f) => f.write_all(data),
            UnitStream::FileWrite(w) => w.write_all(data),
            _ => Err(io::Error::new(io::ErrorKind::PermissionDenied, "unit not open for writing")),
        }
    }

    /// Read raw bytes at the current file position (for unformatted/stream I/O).
    fn read_raw(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.stream {
            UnitStream::FileRaw(f) => f.read(buf),
            UnitStream::FileRead(r) => r.read(buf),
            _ => Err(io::Error::new(io::ErrorKind::PermissionDenied, "unit not open for reading")),
        }
    }
}

/// Write a direct-access record (formatted string padded to recl).
#[no_mangle]
pub extern "C" fn afs_write_direct(
    unit: i32, rec: i64,
    data: *const u8, data_len: i64,
    iostat: *mut i32,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if let Err(e) = u.seek_to_record(rec) {
            if !iostat.is_null() { unsafe { *iostat = 1; } }
            else { eprintln!("WRITE direct: {}", e); std::process::exit(1); }
            return;
        }
        let recl = u.recl.unwrap_or(0) as usize;
        let mut record = vec![b' '; recl]; // space-padded
        let copy_len = (data_len as usize).min(recl);
        if !data.is_null() && copy_len > 0 {
            unsafe { std::ptr::copy_nonoverlapping(data, record.as_mut_ptr(), copy_len); }
        }
        if let Err(e) = u.write_raw(&record) {
            if !iostat.is_null() { unsafe { *iostat = 1; } }
            else { eprintln!("WRITE direct: {}", e); std::process::exit(1); }
            return;
        }
        if !iostat.is_null() { unsafe { *iostat = 0; } }
    }
}

/// Read a direct-access record.
#[no_mangle]
pub extern "C" fn afs_read_direct(
    unit: i32, rec: i64,
    data: *mut u8, data_len: i64,
    iostat: *mut i32,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if let Err(e) = u.seek_to_record(rec) {
            if !iostat.is_null() { unsafe { *iostat = 1; } }
            else { eprintln!("READ direct: {}", e); std::process::exit(1); }
            return;
        }
        let recl = u.recl.unwrap_or(0) as usize;
        let mut record = vec![0u8; recl];
        match u.read_raw(&mut record) {
            Ok(n) => {
                let copy_len = n.min(data_len as usize);
                if !data.is_null() && copy_len > 0 {
                    unsafe { std::ptr::copy_nonoverlapping(record.as_ptr(), data, copy_len); }
                }
                // Pad remainder with spaces.
                if copy_len < data_len as usize {
                    unsafe { std::ptr::write_bytes(data.add(copy_len), b' ', data_len as usize - copy_len); }
                }
                if !iostat.is_null() { unsafe { *iostat = 0; } }
            }
            Err(e) => {
                if !iostat.is_null() { unsafe { *iostat = 1; } }
                else { eprintln!("READ direct: {}", e); std::process::exit(1); }
            }
        }
    }
}

// ---- Unformatted sequential I/O ----

/// Write an unformatted record with 4-byte length markers (gfortran-compatible).
#[no_mangle]
pub extern "C" fn afs_write_unformatted(
    unit: i32,
    data: *const u8, data_len: i64,
    iostat: *mut i32,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        let len_bytes = (data_len as u32).to_ne_bytes();
        // Write: [len][data][len]
        let r1 = u.write_raw(&len_bytes);
        let r2 = if !data.is_null() && data_len > 0 {
            let slice = unsafe { std::slice::from_raw_parts(data, data_len as usize) };
            u.write_raw(slice)
        } else { Ok(()) };
        let r3 = u.write_raw(&len_bytes);
        if r1.is_err() || r2.is_err() || r3.is_err() {
            if !iostat.is_null() { unsafe { *iostat = 1; } }
        } else {
            if !iostat.is_null() { unsafe { *iostat = 0; } }
        }
    }
}

/// Read an unformatted record with 4-byte length markers.
#[no_mangle]
pub extern "C" fn afs_read_unformatted(
    unit: i32,
    data: *mut u8, max_len: i64,
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
                if !iostat.is_null() { unsafe { *iostat = IOSTAT_END; } }
                return;
            }
            _ => {
                if !iostat.is_null() { unsafe { *iostat = 1; } }
                return;
            }
        }
        let record_len = u32::from_ne_bytes(len_buf) as usize;
        if !actual_len.is_null() { unsafe { *actual_len = record_len as i64; } }

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
        if !iostat.is_null() { unsafe { *iostat = 0; } }
    }
}

// ---- Stream access helpers ----

/// Write raw bytes at the current stream position.
#[no_mangle]
pub extern "C" fn afs_write_stream(
    unit: i32,
    data: *const u8, data_len: i64,
    iostat: *mut i32,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if !data.is_null() && data_len > 0 {
            let slice = unsafe { std::slice::from_raw_parts(data, data_len as usize) };
            match u.write_raw(slice) {
                Ok(()) => { if !iostat.is_null() { unsafe { *iostat = 0; } } }
                Err(_) => { if !iostat.is_null() { unsafe { *iostat = 1; } } }
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
            UnitStream::FileRaw(f) => {
                match f.seek(SeekFrom::Start(pos as u64)) {
                    Ok(_) => { if !iostat.is_null() { unsafe { *iostat = 0; } } }
                    Err(_) => { if !iostat.is_null() { unsafe { *iostat = 1; } } }
                }
            }
            _ => { if !iostat.is_null() { unsafe { *iostat = 1; } } }
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
    pub data_type: i32,  // 0=int, 1=real, 2=string, 3=logical
    pub data_len: i64,   // string length for type 2
}

/// Write a NAMELIST group to a unit.
/// Format: &GROUPNAME var=val, var=val, ... /
#[no_mangle]
pub extern "C" fn afs_write_namelist(
    unit: i32,
    group_name: *const u8, group_name_len: i64,
    entries: *const NamelistEntry, n_entries: i32,
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
                    0 => { // integer
                        let v = unsafe { *(entry.data as *const i32) };
                        format!("{}", v)
                    }
                    1 => { // real
                        let v = unsafe { *(entry.data as *const f64) };
                        format!("{}", v)
                    }
                    2 => { // string
                        let s = unsafe_str(entry.data, entry.data_len);
                        format!("'{}'", s.trim_end())
                    }
                    3 => { // logical
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
    group_name: *const u8, group_name_len: i64,
    entries: *const NamelistEntry, n_entries: i32,
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
                    if !iostat.is_null() { unsafe { *iostat = IOSTAT_END; } }
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
                } else { after_name }
            } else { "" };

            // Parse var=val pairs.
            for pair in content.split(',') {
                let pair = pair.trim();
                if let Some(eq_pos) = pair.find('=') {
                    let var_name = pair[..eq_pos].trim().to_lowercase();
                    let val_str = pair[eq_pos+1..].trim();

                    // Find the matching entry.
                    for entry in entries_slice {
                        let ename = unsafe_str(entry.name, entry.name_len).to_lowercase();
                        if ename == var_name {
                            match entry.data_type {
                                0 => { // integer
                                    if let Ok(v) = val_str.parse::<i32>() {
                                        unsafe { *(entry.data as *mut i32) = v; }
                                    }
                                }
                                1 => { // real
                                    let normalized = val_str.replace('d', "e").replace('D', "E");
                                    if let Ok(v) = normalized.parse::<f64>() {
                                        unsafe { *(entry.data as *mut f64) = v; }
                                    }
                                }
                                2 => { // string
                                    let s = val_str.trim_matches('\'').trim_matches('"');
                                    let bytes = s.as_bytes();
                                    let copy_len = bytes.len().min(entry.data_len as usize);
                                    if !entry.data.is_null() && copy_len > 0 {
                                        unsafe {
                                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), entry.data, copy_len);
                                            if copy_len < entry.data_len as usize {
                                                std::ptr::write_bytes(entry.data.add(copy_len), b' ',
                                                    entry.data_len as usize - copy_len);
                                            }
                                        }
                                    }
                                }
                                3 => { // logical
                                    let lower = val_str.to_lowercase();
                                    let v = lower.starts_with(".t") || lower.starts_with("t");
                                    unsafe { *(entry.data as *mut i32) = v as i32; }
                                }
                                _ => {}
                            }
                            break;
                        }
                    }
                }
            }
        }
        if !iostat.is_null() { unsafe { *iostat = 0; } }
    }
}

// ---- Internal I/O (read/write to character variables) ----

/// Write a formatted integer to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_int(
    buf: *mut u8, buf_len: i64,
    val: i32,
    pos: *mut i64, // current write position, updated after write
) {
    if buf.is_null() || buf_len <= 0 { return; }
    let s = format!(" {}", val);
    let start = if !pos.is_null() { (unsafe { *pos }) as usize } else { 0 };
    write_to_buffer(buf, buf_len as usize, start, s.as_bytes(), pos);
}

/// Write a formatted string to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_string(
    buf: *mut u8, buf_len: i64,
    src: *const u8, src_len: i64,
    pos: *mut i64,
) {
    if buf.is_null() || buf_len <= 0 { return; }
    let start = if !pos.is_null() { (unsafe { *pos }) as usize } else { 0 };
    let mut data = vec![b' '];
    if !src.is_null() && src_len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(src, src_len as usize) };
        data.extend_from_slice(slice);
    }
    write_to_buffer(buf, buf_len as usize, start, &data, pos);
}

/// Read an integer from a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_read_internal_int(
    buf: *const u8, buf_len: i64,
    val: *mut i32,
    iostat: *mut i32,
) {
    if buf.is_null() || buf_len <= 0 {
        if !iostat.is_null() { unsafe { *iostat = -1; } }
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(buf, buf_len as usize) };
    let s = std::str::from_utf8(slice).unwrap_or("");
    if let Some(token) = s.trim().split_whitespace().next() {
        match token.replace(',', "").parse::<i32>() {
            Ok(v) => {
                if !val.is_null() { unsafe { *val = v; } }
                if !iostat.is_null() { unsafe { *iostat = 0; } }
            }
            Err(_) => {
                if !iostat.is_null() { unsafe { *iostat = 1; } }
            }
        }
    } else {
        if !iostat.is_null() { unsafe { *iostat = -1; } }
    }
}

/// Read a real from a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_read_internal_real(
    buf: *const u8, buf_len: i64,
    val: *mut f64,
    iostat: *mut i32,
) {
    if buf.is_null() || buf_len <= 0 {
        if !iostat.is_null() { unsafe { *iostat = -1; } }
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(buf, buf_len as usize) };
    let s = std::str::from_utf8(slice).unwrap_or("");
    if let Some(token) = s.trim().split_whitespace().next() {
        let normalized = token.replace('d', "e").replace('D', "E").replace(',', "");
        match normalized.parse::<f64>() {
            Ok(v) => {
                if !val.is_null() { unsafe { *val = v; } }
                if !iostat.is_null() { unsafe { *iostat = 0; } }
            }
            Err(_) => {
                if !iostat.is_null() { unsafe { *iostat = 1; } }
            }
        }
    } else {
        if !iostat.is_null() { unsafe { *iostat = -1; } }
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
        unsafe { std::ptr::write_bytes(buf.add(end), b' ', buf_len - end); }
    }
    if !pos.is_null() {
        unsafe { *pos = end as i64; }
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
                let pos = f.seek(SeekFrom::Current(0)).unwrap_or(0);
                if pos <= 1 {
                    if !iostat.is_null() { unsafe { *iostat = 0; } }
                    return;
                }
                // Skip the current newline at pos-1.
                let mut search_pos = pos - 2;
                loop {
                    f.seek(SeekFrom::Start(search_pos)).ok();
                    let mut byte = [0u8; 1];
                    if f.read(&mut byte).unwrap_or(0) == 0 { break; }
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
                if !iostat.is_null() { unsafe { *iostat = 0; } }
            }
            _ => {
                // Sequential buffered files: backspace is not well-supported.
                if !iostat.is_null() { unsafe { *iostat = 0; } }
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
        match &mut u.stream {
            UnitStream::FileRaw(f) => {
                let pos = f.seek(SeekFrom::Current(0)).unwrap_or(0);
                let _ = f.set_len(pos); // truncate at current position
            }
            _ => {}
        }
        if !iostat.is_null() { unsafe { *iostat = 0; } }
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
            UnitStream::FileRaw(f) => {
                let _ = f.seek(SeekFrom::Start(0));
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
