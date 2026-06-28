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

fn scratch_filename(unit: i32) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir();
    dir.join(format!(
        "afs_scratch_{pid}_{}_{seq}.tmp",
        unit.unsigned_abs()
    ))
    .to_string_lossy()
    .into_owned()
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

#[inline]
fn write_i64_ptr(dst: *mut i64, value: i64) {
    if !dst.is_null() {
        unsafe { std::ptr::write_unaligned(dst, value) };
    }
}

#[inline]
fn write_i32_ptr(dst: *mut i32, value: i32) {
    if !dst.is_null() {
        unsafe { std::ptr::write_unaligned(dst, value) };
    }
}

#[inline]
fn write_i16_ptr(dst: *mut i16, value: i16) {
    if !dst.is_null() {
        unsafe { std::ptr::write_unaligned(dst, value) };
    }
}

#[inline]
fn write_i8_ptr(dst: *mut i8, value: i8) {
    if !dst.is_null() {
        unsafe { std::ptr::write_unaligned(dst, value) };
    }
}

#[inline]
fn write_f64_ptr(dst: *mut f64, value: f64) {
    if !dst.is_null() {
        unsafe { std::ptr::write_unaligned(dst, value) };
    }
}

#[inline]
fn write_f32_ptr(dst: *mut f32, value: f32) {
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

#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// True for STATUS='SCRATCH' units: backing file is deleted on close or exit.
    scratch: bool,
    /// Connection-level LEADING_ZERO= mode (F2023). Seeds the format
    /// engine's leading-zero state at the start of each formatted WRITE
    /// unless the statement carries its own LEADING_ZERO= override.
    leading_zero: LeadingZeroMode,
    /// In-flight sequential-unformatted record buffer. Set by
    /// `afs_list_write_begin` and drained by `afs_list_write_end`.
    /// While Some, list-directed write helpers append raw bytes here
    /// instead of writing ASCII or directly to the file stream — the
    /// whole statement materializes as one record [len][data][len].
    pending_record: Option<Vec<u8>>,
    /// In-flight sequential-unformatted record being consumed by a
    /// list-directed READ statement. Set by `afs_list_read_begin`
    /// (which reads a full [len][data][len] record into memory) and
    /// cleared by `afs_list_read_end`. The cursor tracks how many
    /// bytes the per-item helpers have consumed so far.
    pending_read: Option<(Vec<u8>, usize)>,
}

impl Unit {
    fn is_unformatted(&self) -> bool {
        self.form == Form::Unformatted
    }

    /// Append raw bytes to the in-flight unformatted record buffer if
    /// one is open, otherwise write directly to the stream. Returns
    /// `true` when bytes were buffered.
    fn raw_or_buffer(&mut self, bytes: &[u8]) -> bool {
        if let Some(buf) = self.pending_record.as_mut() {
            buf.extend_from_slice(bytes);
            true
        } else {
            let _ = self.write_raw(bytes);
            false
        }
    }

    /// Consume `n` bytes from the in-flight unformatted read record
    /// (advancing the cursor). Returns `Some(slice)` when the record
    /// has enough bytes; `None` when the record is exhausted, when no
    /// pending record is open, or when fewer than `n` bytes remain.
    fn read_buffer_take(&mut self, n: usize) -> Option<Vec<u8>> {
        let (buf, cur) = self.pending_read.as_mut()?;
        if *cur + n > buf.len() {
            return None;
        }
        let out = buf[*cur..*cur + n].to_vec();
        *cur += n;
        Some(out)
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

fn normalize_fortran_real_input(token: &str, strip_commas: bool) -> String {
    let mut normalized = String::with_capacity(token.len() + 1);
    for ch in token.trim().chars() {
        if strip_commas && ch == ',' {
            continue;
        }
        match ch {
            'd' => normalized.push('e'),
            'D' => normalized.push('E'),
            _ => normalized.push(ch),
        }
    }

    if normalized.bytes().any(|b| matches!(b, b'e' | b'E')) {
        return normalized;
    }

    let bytes = normalized.as_bytes();
    for idx in 1..bytes.len() {
        let b = bytes[idx];
        if !matches!(b, b'+' | b'-') {
            continue;
        }
        if idx + 1 >= bytes.len() || !bytes[idx + 1].is_ascii_digit() {
            continue;
        }
        if !mantissa_allows_implicit_exponent(&bytes[..idx]) {
            continue;
        }

        let mut with_exp = String::with_capacity(normalized.len() + 1);
        with_exp.push_str(&normalized[..idx]);
        with_exp.push('e');
        with_exp.push_str(&normalized[idx..]);
        return with_exp;
    }

    normalized
}

fn mantissa_allows_implicit_exponent(prefix: &[u8]) -> bool {
    let mut saw_digit = false;
    for (idx, b) in prefix.iter().copied().enumerate() {
        match b {
            b'+' | b'-' if idx == 0 => {}
            b'.' => {}
            b'0'..=b'9' => saw_digit = true,
            _ => return false,
        }
    }
    saw_digit
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
                scratch: false,
                leading_zero: LeadingZeroMode::Default,
                pending_record: None,
                pending_read: None,
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
                scratch: false,
                leading_zero: LeadingZeroMode::Default,
                pending_record: None,
                pending_read: None,
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
                scratch: false,
                leading_zero: LeadingZeroMode::Default,
                pending_record: None,
                pending_read: None,
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
    /// LEADING_ZERO= specifier (F2023). Appended after `position_len`;
    /// the lowering writes the matching offsets (see stmt.rs OPEN). Empty
    /// (null/0) when the OPEN carried no LEADING_ZERO=.
    pub leading_zero: *const u8,
    pub leading_zero_len: i64,
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
        leading_zero: std::ptr::null(),
        leading_zero_len: 0,
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
    let fname = fortran_file_name(cb.filename, cb.filename_len);
    let status_str = unsafe_str(cb.status, cb.status_len).to_lowercase();
    let status = status_str.trim();
    let is_scratch = status == "scratch";
    let action_str = unsafe_str(cb.action, cb.action_len).to_lowercase();
    let access_str = unsafe_str(cb.access, cb.access_len).to_lowercase();
    let form_str = unsafe_str(cb.form, cb.form_len).to_lowercase();
    let position_str = unsafe_str(cb.position, cb.position_len).to_lowercase();
    let leading_zero_str = unsafe_str(cb.leading_zero, cb.leading_zero_len);
    let leading_zero_specified = !leading_zero_str.trim().is_empty();
    let leading_zero_mode = LeadingZeroMode::from_specifier(&leading_zero_str);
    let recl = cb.recl;
    let iostat = cb.iostat;
    let newunit = cb.newunit;
    let missing_filename = fname.trim().is_empty();

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

    let existing_unit = state.units.get(&actual_unit).map(|u| {
        (
            u.filename.clone(),
            u.access,
            u.form.clone(),
            u.action,
            u.recl,
        )
    });
    let fname = if missing_filename {
        if is_scratch {
            // STATUS='SCRATCH': F2018 §12.5.6.13 — implementation chooses the
            // backing path; file must not be FILE=, must be deleted on close.
            scratch_filename(actual_unit)
        } else {
            existing_unit
                .as_ref()
                .map(|(filename, _, _, _, _)| filename.clone())
                .unwrap_or(fname)
        }
    } else {
        fname
    };

    let update_existing_in_place = missing_filename
        && existing_unit.is_some()
        && status.is_empty()
        && action_str.trim().is_empty()
        && access_str.trim().is_empty()
        && form_str.trim().is_empty()
        && recl <= 0
        && newunit.is_null();
    if update_existing_in_place {
        if let Some(unit) = state.get_unit(actual_unit) {
            match position_str.trim() {
                "append" => match &mut unit.stream {
                    UnitStream::FileRaw(f) => {
                        let _ = f.seek(SeekFrom::End(0));
                    }
                    UnitStream::FileRead(r) => {
                        let _ = r.seek(SeekFrom::End(0));
                    }
                    UnitStream::FileWrite(w) => {
                        let _ = w.seek(SeekFrom::End(0));
                    }
                    _ => {}
                },
                "rewind" => match &mut unit.stream {
                    UnitStream::FileRaw(f) => {
                        let _ = f.seek(SeekFrom::Start(0));
                    }
                    UnitStream::FileRead(r) => {
                        let _ = r.seek(SeekFrom::Start(0));
                    }
                    UnitStream::FileWrite(w) => {
                        let _ = w.seek(SeekFrom::Start(0));
                    }
                    _ => {}
                },
                _ => {}
            }
            if leading_zero_specified {
                unit.leading_zero = leading_zero_mode;
            }
        }
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        }
        return;
    }

    // Build OpenOptions based on status/action.
    let mut opts = OpenOptions::new();
    match status {
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
        "" => existing_unit
            .as_ref()
            .map(|(_, _, _, action, _)| match action {
                Action::Read => "read",
                Action::Write => "write",
                Action::ReadWrite => "readwrite",
            })
            .unwrap_or_else(|| match status {
                "old" => "read",
                "new" | "replace" => "write",
                _ => "readwrite",
            }),
        _ => "readwrite",
    };

    match effective_action {
        "read" => {
            opts.read(true);
        }
        "write" => {
            opts.write(true);
            if status != "old" {
                opts.create(true);
            }
        }
        _ => {
            opts.read(true).write(true);
            if status != "old" {
                opts.create(true);
            }
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
                "" => existing_unit
                    .as_ref()
                    .map(|(_, access, _, _, _)| *access)
                    .unwrap_or(Access::Sequential),
                _ => Access::Sequential,
            };
            let file_form = match form_str.trim() {
                "unformatted" => Form::Unformatted,
                "" => existing_unit
                    .as_ref()
                    .map(|(_, _, form, _, _)| form.clone())
                    .unwrap_or_else(|| {
                        if file_access == Access::Stream {
                            Form::Unformatted
                        } else {
                            Form::Formatted
                        }
                    }),
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
                    recl: if recl > 0 {
                        Some(recl)
                    } else {
                        existing_unit
                            .as_ref()
                            .and_then(|(_, _, _, _, existing_recl)| *existing_recl)
                    },
                    read_tokens: Vec::new(),
                    formatted_read_record: None,
                    formatted_read_cursor: 0,
                    scratch: is_scratch,
                    leading_zero: leading_zero_mode,
                    pending_record: None,
                    pending_read: None,
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
                // Release the io_state mutex before exit. process::exit invokes
                // libc atexit handlers — including afs_io_finalize, which locks
                // io_state to flush units. Holding the lock here while exiting
                // deadlocked on macOS where the atexit thread re-entered the
                // same mutex (sample-trace: pthread_mutex_firstfit_lock_wait
                // → __psynch_mutexwait, hangs forever).
                drop(state);
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
        // STATUS='SCRATCH' units always delete on close (F2018 §12.5.6.13).
        let delete = delete_on_close || u.scratch;
        drop(u);

        let mut close_status = 0;
        if delete
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
            // Release lock before exit (afs_io_finalize atexit re-locks). See afs_open.
            drop(state);
            eprintln!(
                "CLOSE: {}: {}",
                filename,
                io::Error::from_raw_os_error(close_status)
            );
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

/// Write an 8-bit integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int8(unit: i32, val: i8) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_unformatted() {
            u.raw_or_buffer(&val.to_ne_bytes());
        } else {
            let _ = u.write_str(&format!(" {}", val));
        }
    }
}

/// Write a 16-bit integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int16(unit: i32, val: i16) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_unformatted() {
            u.raw_or_buffer(&val.to_ne_bytes());
        } else {
            let _ = u.write_str(&format!(" {}", val));
        }
    }
}

/// Write an integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int(unit: i32, val: i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.is_unformatted() {
            u.raw_or_buffer(&val.to_ne_bytes());
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
        if u.is_unformatted() {
            u.raw_or_buffer(&val.to_ne_bytes());
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
        if u.is_unformatted() {
            u.raw_or_buffer(&val.to_ne_bytes());
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
        if u.is_unformatted() {
            u.raw_or_buffer(&val.to_ne_bytes());
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
        if u.is_unformatted() {
            u.raw_or_buffer(&val.to_ne_bytes());
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
        if u.is_unformatted() {
            u.raw_or_buffer(&re.to_ne_bytes());
            u.raw_or_buffer(&im.to_ne_bytes());
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
        if u.is_unformatted() {
            u.raw_or_buffer(&re.to_ne_bytes());
            u.raw_or_buffer(&im.to_ne_bytes());
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
        if u.is_unformatted() {
            if !ptr.is_null() && len > 0 {
                let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                u.raw_or_buffer(slice);
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
        if u.is_unformatted() {
            u.raw_or_buffer(&val.to_ne_bytes());
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
        if u.is_unformatted() {
            // Sequential unformatted: a pending record buffer means
            // afs_list_write_end was not called — flush nothing here.
            // Stream unformatted has no record terminator.
            let _ = u.flush();
            return;
        }
        let _ = u.write_str("\n");
        let _ = u.flush();
    }
}

/// Like `afs_write_newline` but no-ops when `advance == 0`. The
/// lowering uses this when `advance=` is a runtime-evaluated string
/// (e.g. `advance=optval(adv, 'YES')`) — `advance` is precomputed by
/// `afs_advance_eval` to 0 (no advance) or 1 (advance).
#[no_mangle]
pub extern "C" fn afs_write_newline_if(unit: i32, advance: i32) {
    if advance == 0 {
        let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(u) = state.get_unit(unit) {
            let _ = u.flush();
        }
        return;
    }
    afs_write_newline(unit);
}

/// Evaluate an `advance=` string at runtime. Returns 0 when the
/// trimmed, case-folded string equals "no", else 1. The lowering
/// uses this for non-literal advance expressions so that
/// `advance=optval(adv, 'YES')` produces the correct behavior
/// (current lowering only honors string-literal advance values).
#[no_mangle]
pub extern "C" fn afs_advance_eval(ptr: *const u8, len: i64) -> i32 {
    if ptr.is_null() || len <= 0 {
        return 1;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let s = std::str::from_utf8(bytes).unwrap_or("");
    if s.trim().eq_ignore_ascii_case("no") {
        0
    } else {
        1
    }
}

/// Read a formatted character item with runtime advance dispatch.
/// `advance == 0` selects the non-advancing path
/// (`afs_fmt_read_string_noadvance`); any other value uses
/// `afs_fmt_read_string` which advances past the record. Used by the
/// lowering when `advance=` is a non-literal expression and the bool
/// path can't be statically chosen.
#[no_mangle]
pub extern "C" fn afs_fmt_read_string_dyn(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    dest: *mut u8,
    dest_len: i64,
    size_out: *mut i32,
    iostat: *mut i32,
    advance: i32,
) {
    if advance == 0 {
        afs_fmt_read_string_noadvance(unit, fmt_str, fmt_len, dest, dest_len, size_out, iostat);
    } else {
        afs_fmt_read_string(
            unit, fmt_str, fmt_len, data_index, dest, dest_len, size_out, iostat,
        );
    }
}

/// Begin a list-directed write statement. Mandatory before the first
/// per-item helper when iostat=/iomsg= are requested or when the unit
/// might be sequential-unformatted (which needs record-buffered emit).
///
/// For formatted units this only resets iostat. For sequential
/// unformatted units it opens a fresh per-statement record buffer that
/// the per-item helpers will append into. Stream-unformatted units skip
/// the buffer (each helper writes raw bytes directly).
#[no_mangle]
pub extern "C" fn afs_list_write_begin(
    unit: i32,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
    if !iomsg.is_null() && iomsg_len > 0 {
        let buf = unsafe { std::slice::from_raw_parts_mut(iomsg, iomsg_len as usize) };
        for b in buf.iter_mut() {
            *b = b' ';
        }
    }
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if u.form == Form::Unformatted && u.access == Access::Sequential {
            u.pending_record = Some(Vec::new());
        }
    } else if !iostat.is_null() {
        unsafe {
            *iostat = 1;
        }
    }
}

/// End a list-directed write statement. For sequential unformatted
/// units this drains the per-statement record buffer and writes
/// `[len][bytes][len]` to the stream. For formatted units the trailing
/// newline is left to the per-item path's `afs_write_newline` so we
/// don't double-newline; this only flushes and forwards iostat/iomsg.
/// `advance` is accepted for symmetry with `afs_fmt_end` but is unused
/// by the formatted path here.
#[no_mangle]
pub extern "C" fn afs_list_write_end(
    unit: i32,
    _advance: i32,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let Some(u) = state.get_unit(unit) else {
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
        }
        return;
    };
    let mut err: Option<String> = None;
    if let Some(buf) = u.pending_record.take() {
        let len_bytes = (buf.len() as u32).to_ne_bytes();
        let r1 = u.write_raw(&len_bytes);
        let r2 = if !buf.is_empty() {
            u.write_raw(&buf)
        } else {
            Ok(())
        };
        let r3 = u.write_raw(&len_bytes);
        if let Err(e) = r1.or(r2).or(r3) {
            err = Some(e.to_string());
        }
    }
    let _ = u.flush();
    if let Some(msg) = err {
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
        }
        if !iomsg.is_null() && iomsg_len > 0 {
            let buf = unsafe { std::slice::from_raw_parts_mut(iomsg, iomsg_len as usize) };
            let bytes = msg.as_bytes();
            let n = bytes.len().min(buf.len());
            buf[..n].copy_from_slice(&bytes[..n]);
            for b in buf[n..].iter_mut() {
                *b = b' ';
            }
        }
    }
}

// ---- Public C API: List-directed READ ----

/// Read an i8 value (list-directed) from a unit.
#[no_mangle]
pub extern "C" fn afs_read_int8(unit: i32, val: *mut i8, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if let Some(bytes) = u.read_buffer_take(1) {
            write_i8_ptr(val, i8::from_ne_bytes([bytes[0]]));
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
            return;
        }
        if u.form == Form::Unformatted && u.access == Access::Stream {
            let mut b = [0u8; 1];
            if read_stream_unformatted_exact(u, &mut b, iostat) == Some(true) {
                write_i8_ptr(val, i8::from_ne_bytes(b));
            }
            return;
        }
        match u.next_read_token() {
            Ok(Some(token)) => match token.parse::<i8>() {
                Ok(v) => {
                    write_i8_ptr(val, v);
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

/// Read an i16 value (list-directed) from a unit.
#[no_mangle]
pub extern "C" fn afs_read_int16(unit: i32, val: *mut i16, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if let Some(bytes) = u.read_buffer_take(2) {
            let mut b = [0u8; 2];
            b.copy_from_slice(&bytes);
            write_i16_ptr(val, i16::from_ne_bytes(b));
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
            return;
        }
        if u.form == Form::Unformatted && u.access == Access::Stream {
            let mut b = [0u8; 2];
            if read_stream_unformatted_exact(u, &mut b, iostat) == Some(true) {
                write_i16_ptr(val, i16::from_ne_bytes(b));
            }
            return;
        }
        match u.next_read_token() {
            Ok(Some(token)) => match token.parse::<i16>() {
                Ok(v) => {
                    write_i16_ptr(val, v);
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

/// Read an i32 value (list-directed) from a unit.
/// Uses token buffer: multiple values on one line are consumed left-to-right.
#[no_mangle]
pub extern "C" fn afs_read_int(unit: i32, val: *mut i32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        if let Some(bytes) = u.read_buffer_take(4) {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bytes);
            write_i32_ptr(val, i32::from_ne_bytes(b));
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
            return;
        }
        if u.form == Form::Unformatted && u.access == Access::Stream {
            let mut b = [0u8; 4];
            if read_stream_unformatted_exact(u, &mut b, iostat) == Some(true) {
                write_i32_ptr(val, i32::from_ne_bytes(b));
            }
            return;
        }
        match u.next_read_token() {
            Ok(Some(token)) => match token.parse::<i32>() {
                Ok(v) => {
                    write_i32_ptr(val, v);
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
        if let Some(bytes) = u.read_buffer_take(8) {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes);
            write_i64_ptr(val, i64::from_ne_bytes(b));
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
            return;
        }
        if u.form == Form::Unformatted && u.access == Access::Stream {
            let mut b = [0u8; 8];
            if read_stream_unformatted_exact(u, &mut b, iostat) == Some(true) {
                write_i64_ptr(val, i64::from_ne_bytes(b));
            }
            return;
        }
        match u.next_read_token() {
            Ok(Some(token)) => match token.parse::<i64>() {
                Ok(v) => {
                    write_i64_ptr(val, v);
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
        if let Some(bytes) = u.read_buffer_take(16) {
            let mut b = [0u8; 16];
            b.copy_from_slice(&bytes);
            write_i128_ptr(val, i128::from_ne_bytes(b));
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
            return;
        }
        if u.form == Form::Unformatted && u.access == Access::Stream {
            let mut b = [0u8; 16];
            if read_stream_unformatted_exact(u, &mut b, iostat) == Some(true) {
                write_i128_ptr(val, i128::from_ne_bytes(b));
            }
            return;
        }
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
        if let Some(bytes) = u.read_buffer_take(4) {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bytes);
            write_f32_ptr(val, f32::from_ne_bytes(b));
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
            return;
        }
        if u.form == Form::Unformatted && u.access == Access::Stream {
            let mut b = [0u8; 4];
            if read_stream_unformatted_exact(u, &mut b, iostat) == Some(true) {
                write_f32_ptr(val, f32::from_ne_bytes(b));
            }
            return;
        }
        match u.next_read_token() {
            Ok(Some(token)) => {
                let normalized = normalize_fortran_real_input(&token, false);
                match normalized.parse::<f32>() {
                    Ok(v) => {
                        write_f32_ptr(val, v);
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
        if let Some(bytes) = u.read_buffer_take(8) {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes);
            write_f64_ptr(val, f64::from_ne_bytes(b));
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
            return;
        }
        if u.form == Form::Unformatted && u.access == Access::Stream {
            let mut b = [0u8; 8];
            if read_stream_unformatted_exact(u, &mut b, iostat) == Some(true) {
                write_f64_ptr(val, f64::from_ne_bytes(b));
            }
            return;
        }
        match u.next_read_token() {
            Ok(Some(token)) => {
                let normalized = normalize_fortran_real_input(&token, false);
                match normalized.parse::<f64>() {
                    Ok(v) => {
                        write_f64_ptr(val, v);
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

/// Begin a list-directed READ statement. Mandatory before per-item
/// helpers when iostat=/iomsg= are requested or when the unit may be
/// sequential-unformatted (which needs the leading record marker
/// consumed and the data slurped into a buffer for typed take-bytes).
///
/// For formatted units this only resets iostat. For sequential
/// unformatted units it reads `[u32 len][len bytes][u32 trailer]`,
/// stashes the data in `pending_read`, and the per-item helpers will
/// consume from there. Stream-unformatted reads continue using their
/// existing per-helper raw-byte path.
#[no_mangle]
pub extern "C" fn afs_list_read_begin(unit: i32, iostat: *mut i32, iomsg: *mut u8, iomsg_len: i64) {
    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
    if !iomsg.is_null() && iomsg_len > 0 {
        let buf = unsafe { std::slice::from_raw_parts_mut(iomsg, iomsg_len as usize) };
        for b in buf.iter_mut() {
            *b = b' ';
        }
    }
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let Some(u) = state.get_unit(unit) else {
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
        }
        return;
    };
    if !(u.form == Form::Unformatted && u.access == Access::Sequential) {
        return;
    }
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
    let mut data = vec![0u8; record_len];
    if record_len > 0 && u.read_raw(&mut data).is_err() && !iostat.is_null() {
        unsafe {
            *iostat = 1;
        }
        return;
    }
    let mut trailer = [0u8; 4];
    let _ = u.read_raw(&mut trailer);
    u.pending_read = Some((data, 0));
}

/// End a list-directed READ statement. Drops any unread bytes left in
/// the in-flight unformatted record buffer (the standard does not
/// require the program to consume the entire record).
#[no_mangle]
pub extern "C" fn afs_list_read_end(
    unit: i32,
    _iostat: *mut i32,
    _iomsg: *mut u8,
    _iomsg_len: i64,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        u.pending_read = None;
    }
}

/// Advance the file position past one record on a list-directed READ
/// statement that has no input items: `read(unit, *)` (no items) is
/// defined by F2018 §12.6.4.5 to position the unit at the next record.
/// stdlib's `number_of_rows(s)` counts rows by repeating exactly that
/// statement until a nonzero iostat — without this helper the loop is
/// infinite because the unit never advances and iostat is never set.
#[no_mangle]
pub extern "C" fn afs_read_skip_record(unit: i32, iostat: *mut i32) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let Some(u) = state.get_unit(unit) else {
        if !iostat.is_null() {
            unsafe { *iostat = 1 };
        }
        return;
    };
    // Drain any pre-tokenized values from the previous list-directed
    // read so the next iteration genuinely consumes a new record.
    u.read_tokens.clear();
    match u.read_line() {
        Ok(s) if s.is_empty() => {
            if !iostat.is_null() {
                unsafe { *iostat = IOSTAT_END };
            }
        }
        Ok(_) => {
            if !iostat.is_null() {
                unsafe { *iostat = 0 };
            }
        }
        Err(_) => {
            if !iostat.is_null() {
                unsafe { *iostat = 1 };
            }
        }
    }
}

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

    if let Some(bytes) = u.read_buffer_take(dest_len as usize) {
        crate::string::afs_assign_char_fixed(dest, dest_len, bytes.as_ptr(), dest_len);
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
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

fn fortran_file_name(ptr: *const u8, len: i64) -> String {
    unsafe_str(ptr, len).trim_end_matches(' ').to_string()
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

fn set_read_iostat_or_exit(iostat: *mut i32, status: i32, message: &str) {
    if !iostat.is_null() {
        unsafe {
            *iostat = status;
        }
    } else {
        eprintln!("READ: {}", message);
        std::process::exit(1);
    }
}

fn read_stream_unformatted_exact(u: &mut Unit, buf: &mut [u8], iostat: *mut i32) -> Option<bool> {
    if !(u.form == Form::Unformatted && u.access == Access::Stream) {
        return None;
    }

    let mut offset = 0usize;
    while offset < buf.len() {
        match u.read_raw(&mut buf[offset..]) {
            Ok(0) if offset == 0 => {
                set_read_iostat_or_exit(iostat, IOSTAT_END, "end of file");
                return Some(false);
            }
            Ok(0) => {
                set_read_iostat_or_exit(iostat, 1, "unexpected end of stream item");
                return Some(false);
            }
            Ok(n) => offset += n,
            Err(e) => {
                set_read_iostat_or_exit(iostat, 1, &e.to_string());
                return Some(false);
            }
        }
    }

    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
    Some(true)
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
    if pos < 1 {
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
        }
        return;
    }
    let offset = (pos - 1) as u64;
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(u) = state.get_unit(unit) {
        match &mut u.stream {
            UnitStream::FileRaw(f) => match f.seek(SeekFrom::Start(offset)) {
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
    } else if !iostat.is_null() {
        unsafe {
            *iostat = 1;
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
    pub data_type: i32, // 0=int, 1=real, 2=fixed string, 3=i32 logical, 4=StringDescriptor, 5=bool logical
    pub data_len: i64,  // fixed string length for type 2
    pub elem_count: i64,
}

fn quote_namelist_char(s: &str) -> String {
    format!("'{}'", s.trim_end().replace('\'', "''"))
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
                        let elem_len = entry.data_len.max(0) as usize;
                        let elem_count = entry.elem_count.max(1) as usize;
                        let mut values = Vec::with_capacity(elem_count);
                        for elem in 0..elem_count {
                            let ptr = unsafe { entry.data.add(elem * elem_len) };
                            let s = unsafe_str(ptr, entry.data_len);
                            values.push(quote_namelist_char(&s));
                        }
                        values.join(",")
                    }
                    3 => {
                        // logical
                        let v = unsafe { *(entry.data as *const i32) };
                        (if v != 0 { ".TRUE." } else { ".FALSE." }).to_string()
                    }
                    4 => {
                        // deferred-length string descriptor
                        let desc =
                            unsafe { &*(entry.data as *const crate::descriptor::StringDescriptor) };
                        let s = unsafe_str(desc.data, desc.len);
                        quote_namelist_char(&s)
                    }
                    5 => {
                        // bool-backed logical
                        let v = unsafe { *(entry.data as *const u8) } != 0;
                        (if v { ".TRUE." } else { ".FALSE." }).to_string()
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

        let _ = namelist_assign_from_text(&all_lines, &gname, entries, n_entries);
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        }
    }
}

/// Read a NAMELIST group from an internal character buffer.
#[no_mangle]
pub extern "C" fn afs_read_namelist_internal(
    buf: *const u8,
    buf_len: i64,
    group_name: *const u8,
    group_name_len: i64,
    entries: *const NamelistEntry,
    n_entries: i32,
    iostat: *mut i32,
) {
    let gname = unsafe_str(group_name, group_name_len).to_lowercase();
    let text = unsafe_str(buf, buf_len);
    let found = namelist_assign_from_text(&text, &gname, entries, n_entries);
    if !iostat.is_null() {
        unsafe {
            *iostat = if found { 0 } else { IOSTAT_END };
        }
    }
}

fn namelist_content<'a>(text: &'a str, group_name: &str) -> Option<&'a str> {
    let lower = text.to_lowercase();
    let marker = format!("&{}", group_name.to_lowercase());
    let start = lower.find(&marker)?;
    let after_start = start + marker.len();
    let after_name = &text[after_start..];
    if let Some(end) = after_name.find('/') {
        Some(&after_name[..end])
    } else {
        Some(after_name)
    }
}

fn namelist_assign_from_text(
    text: &str,
    group_name: &str,
    entries: *const NamelistEntry,
    n_entries: i32,
) -> bool {
    let Some(content) = namelist_content(text, group_name) else {
        return false;
    };
    if entries.is_null() || n_entries <= 0 {
        return true;
    }
    let entries_slice = unsafe { std::slice::from_raw_parts(entries, n_entries as usize) };

    // Parse var=val pairs. Supports:
    //   var=val            — simple scalar assignment
    //   var(index)=val     — array element assignment (1-based)
    //   var=n*val          — repeat notation (set n consecutive elements)
    enum Continuation {
        Array {
            entry_index: usize,
            next_index: usize,
        },
        Components {
            entry_indices: Vec<usize>,
            next_component: usize,
        },
    }

    fn component_entries(entries: &[NamelistEntry], aggregate_name: &str) -> Vec<usize> {
        let prefix = format!("{}%", aggregate_name);
        entries
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                if entry.data.is_null() {
                    return None;
                }
                let ename = unsafe_str(entry.name, entry.name_len).to_lowercase();
                let suffix = ename.strip_prefix(&prefix)?;
                if suffix.is_empty() || suffix.contains('%') {
                    None
                } else {
                    Some(idx)
                }
            })
            .collect()
    }

    fn assign_component_values(
        entries: &[NamelistEntry],
        entry_indices: &[usize],
        start_component: usize,
        val_str: &str,
        repeat: usize,
    ) -> usize {
        let mut next = start_component;
        for _ in 0..repeat.max(1) {
            let Some(entry_index) = entry_indices.get(next).copied() else {
                break;
            };
            if let Some(entry) = entries.get(entry_index) {
                namelist_assign_value(entry, val_str, None, 1);
            }
            next += 1;
        }
        next
    }

    let mut continuation: Option<Continuation> = None;
    for pair in split_namelist_fields(content) {
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
            let (repeat_count, actual_val) = parse_namelist_repeat(val_str);

            // Find the matching entry.
            continuation = None;
            let mut matched = false;
            for (entry_index, entry) in entries_slice.iter().enumerate() {
                if entry.data.is_null() {
                    continue;
                }
                let ename = unsafe_str(entry.name, entry.name_len).to_lowercase();
                if ename == var_name {
                    namelist_assign_value(entry, actual_val, array_index, repeat_count);
                    let next_index = array_index.unwrap_or(1).saturating_add(repeat_count);
                    if entry.data_type == 2 && next_index <= entry.elem_count.max(1) as usize {
                        continuation = Some(Continuation::Array {
                            entry_index,
                            next_index,
                        });
                    }
                    matched = true;
                    break;
                }
            }
            if !matched && array_index.is_none() {
                let entry_indices = component_entries(entries_slice, &var_name);
                if !entry_indices.is_empty() {
                    let next_component = assign_component_values(
                        entries_slice,
                        &entry_indices,
                        0,
                        actual_val,
                        repeat_count,
                    );
                    if next_component < entry_indices.len() {
                        continuation = Some(Continuation::Components {
                            entry_indices,
                            next_component,
                        });
                    }
                }
            }
        } else if let Some(cont) = continuation.take() {
            let (repeat_count, actual_val) = parse_namelist_repeat(pair);
            match cont {
                Continuation::Array {
                    entry_index,
                    next_index,
                } => {
                    if let Some(entry) = entries_slice.get(entry_index) {
                        namelist_assign_value(entry, actual_val, Some(next_index), repeat_count);
                        let next_index = next_index.saturating_add(repeat_count);
                        continuation = if next_index <= entry.elem_count.max(1) as usize {
                            Some(Continuation::Array {
                                entry_index,
                                next_index,
                            })
                        } else {
                            None
                        };
                    }
                }
                Continuation::Components {
                    entry_indices,
                    next_component,
                } => {
                    let next_component = assign_component_values(
                        entries_slice,
                        &entry_indices,
                        next_component,
                        actual_val,
                        repeat_count,
                    );
                    continuation = if next_component < entry_indices.len() {
                        Some(Continuation::Components {
                            entry_indices,
                            next_component,
                        })
                    } else {
                        None
                    };
                }
            }
        }
    }
    true
}

fn split_namelist_fields(content: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut chars = content.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if let Some(q) = quote {
            if ch == q {
                if chars.peek().is_some_and(|(_, next)| *next == q) {
                    let _ = chars.next();
                } else {
                    quote = None;
                }
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch == ',' {
            fields.push(&content[start..idx]);
            start = idx + ch.len_utf8();
        }
    }
    fields.push(&content[start..]);
    fields
}

fn parse_namelist_repeat(val_str: &str) -> (usize, &str) {
    if let Some(star) = val_str.find('*') {
        // Make sure * is preceded by digits (not part of a number like 1.5E*).
        let before = val_str[..star].trim();
        if let Ok(n) = before.parse::<usize>() {
            return (n, val_str[star + 1..].trim());
        }
    }
    (1, val_str)
}

fn parse_namelist_char_value(raw: &str) -> String {
    let s = raw.trim();
    let Some(first) = s.as_bytes().first().copied() else {
        return String::new();
    };
    if first != b'\'' && first != b'"' {
        return s.to_string();
    }
    if s.as_bytes().last().copied() != Some(first) || s.len() < 2 {
        return s.to_string();
    }

    let quote = first as char;
    let mut out = String::with_capacity(s.len().saturating_sub(2));
    let mut chars = s[1..s.len() - 1].chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == quote && chars.peek().copied() == Some(quote) {
            let _ = chars.next();
        }
        out.push(ch);
    }
    out
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
        2 => entry.data_len.max(1) as usize, // fixed string element
        3 => 4, // logical (i32)
        5 => 1, // logical (bool/i8)
        _ => 1, // string
    };
    let start_index = index.unwrap_or(1).max(1);
    let max_elems = entry.elem_count.max(1) as usize;

    for r in 0..repeat {
        let elem_index = start_index.saturating_add(r);
        if elem_index > max_elems {
            break;
        }
        let offset = (elem_index - 1) * elem_size;
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
                let normalized = normalize_fortran_real_input(val_str, false);
                if let Ok(v) = normalized.parse::<f64>() {
                    unsafe {
                        *(ptr as *mut f64) = v;
                    }
                }
            }
            2 => {
                // fixed-length string scalar or element
                let s = parse_namelist_char_value(val_str);
                let bytes = s.as_bytes();
                let slot_len = entry.data_len.max(0) as usize;
                let copy_len = bytes.len().min(slot_len);
                if slot_len > 0 {
                    unsafe {
                        if copy_len > 0 {
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, copy_len);
                        }
                        if copy_len < slot_len {
                            std::ptr::write_bytes(
                                ptr.add(copy_len),
                                b' ',
                                slot_len - copy_len,
                            );
                        }
                    }
                }
            }
            3 => {
                // logical
                let lower = val_str.to_lowercase();
                let v = lower.starts_with(".t") || lower.starts_with("t");
                unsafe {
                    *(ptr as *mut i32) = v as i32;
                }
            }
            4 => {
                // deferred-length string descriptor
                let s = parse_namelist_char_value(val_str);
                crate::string::afs_assign_char_deferred(
                    entry.data as *mut crate::descriptor::StringDescriptor,
                    s.as_ptr(),
                    s.len() as i64,
                );
                return;
            }
            5 => {
                // bool-backed logical
                let lower = val_str.to_lowercase();
                let v = lower.starts_with(".t") || lower.starts_with("t");
                unsafe {
                    *ptr = v as u8;
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
                write_i32_ptr(val, v);
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

/// Read a list-directed character token from a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_read_internal_string(
    buf: *const u8,
    buf_len: i64,
    pos: *mut i64,
    dest: *mut u8,
    dest_len: i64,
    iostat: *mut i32,
) {
    if dest.is_null() || dest_len <= 0 {
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
        }
        return;
    }

    let dest_slice = unsafe { std::slice::from_raw_parts_mut(dest, dest_len as usize) };
    dest_slice.fill(b' ');

    if let Some(token) = next_internal_token(buf, buf_len, pos) {
        let bytes = token.as_bytes();
        let n = bytes.len().min(dest_slice.len());
        dest_slice[..n].copy_from_slice(&bytes[..n]);
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        }
    } else if !iostat.is_null() {
        unsafe {
            *iostat = -1;
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
                write_i64_ptr(val, v);
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
        let normalized = normalize_fortran_real_input(&token, true);
        match normalized.parse::<f64>() {
            Ok(v) => {
                write_f64_ptr(val, v);
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

fn unit_current_fortran_pos(u: &mut Unit) -> i64 {
    let pos = match &mut u.stream {
        UnitStream::FileRaw(f) => f.stream_position(),
        UnitStream::FileRead(r) => r.stream_position(),
        UnitStream::FileWrite(w) => {
            let _ = w.flush();
            w.stream_position()
        }
        _ => return -1,
    };
    pos.map(|p| p as i64 + 1).unwrap_or(-1)
}

/// INQUIRE LEADING_ZERO= readback (F2023 12.10.2.15). A formatted
/// connection reports its current mode (`PRINT`/`SUPPRESS`/
/// `PROCESSOR_DEFINED`); no connection or an unformatted connection is
/// `UNDEFINED` — not `PROCESSOR_DEFINED`.
fn write_leading_zero_capability(unit: Option<&Unit>, buf: *mut u8, buf_len: i64) {
    let s = match unit {
        Some(u) if u.form == Form::Formatted => u.leading_zero.inquire_str(),
        _ => "UNDEFINED",
    };
    write_inquire_string(buf, buf_len, s);
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
    pos_out: *mut i64,
    read_buf: *mut u8,
    read_buf_len: i64,
    write_buf: *mut u8,
    write_buf_len: i64,
    readwrite_buf: *mut u8,
    readwrite_buf_len: i64,
    sequential_buf: *mut u8,
    sequential_buf_len: i64,
    direct_buf: *mut u8,
    direct_buf_len: i64,
    stream_buf: *mut u8,
    stream_buf_len: i64,
    formatted_buf: *mut u8,
    formatted_buf_len: i64,
    unformatted_buf: *mut u8,
    unformatted_buf_len: i64,
    leading_zero_buf: *mut u8,
    leading_zero_buf_len: i64,
) {
    let fname = fortran_file_name(filename, filename_len);

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
        write_action_capabilities(
            Some(u.action),
            read_buf,
            read_buf_len,
            write_buf,
            write_buf_len,
            readwrite_buf,
            readwrite_buf_len,
        );
        write_access_capabilities(
            Some(u.access),
            sequential_buf,
            sequential_buf_len,
            direct_buf,
            direct_buf_len,
            stream_buf,
            stream_buf_len,
        );
        write_form_capabilities(
            Some(&u.form),
            formatted_buf,
            formatted_buf_len,
            unformatted_buf,
            unformatted_buf_len,
        );
        write_leading_zero_capability(Some(u), leading_zero_buf, leading_zero_buf_len);
    } else {
        write_inquire_string(access_buf, access_buf_len, "UNDEFINED");
        write_inquire_string(form_buf, form_buf_len, "UNDEFINED");
        write_inquire_string(action_buf, action_buf_len, "UNDEFINED");
        write_action_capabilities(
            None,
            read_buf,
            read_buf_len,
            write_buf,
            write_buf_len,
            readwrite_buf,
            readwrite_buf_len,
        );
        write_access_capabilities(
            None,
            sequential_buf,
            sequential_buf_len,
            direct_buf,
            direct_buf_len,
            stream_buf,
            stream_buf_len,
        );
        write_form_capabilities(
            None,
            formatted_buf,
            formatted_buf_len,
            unformatted_buf,
            unformatted_buf_len,
        );
        write_leading_zero_capability(None, leading_zero_buf, leading_zero_buf_len);
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
    if !pos_out.is_null() {
        unsafe {
            *pos_out = -1;
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
    pos_out: *mut i64,
    read_buf: *mut u8,
    read_buf_len: i64,
    write_buf: *mut u8,
    write_buf_len: i64,
    readwrite_buf: *mut u8,
    readwrite_buf_len: i64,
    sequential_buf: *mut u8,
    sequential_buf_len: i64,
    direct_buf: *mut u8,
    direct_buf_len: i64,
    stream_buf: *mut u8,
    stream_buf_len: i64,
    formatted_buf: *mut u8,
    formatted_buf_len: i64,
    unformatted_buf: *mut u8,
    unformatted_buf_len: i64,
    leading_zero_buf: *mut u8,
    leading_zero_buf_len: i64,
    pos_out: *mut i64,
) {
    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
    let unit_entry = state.units.get_mut(&unit);

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
        write_action_capabilities(
            Some(u.action),
            read_buf,
            read_buf_len,
            write_buf,
            write_buf_len,
            readwrite_buf,
            readwrite_buf_len,
        );
        write_access_capabilities(
            Some(u.access),
            sequential_buf,
            sequential_buf_len,
            direct_buf,
            direct_buf_len,
            stream_buf,
            stream_buf_len,
        );
        write_form_capabilities(
            Some(&u.form),
            formatted_buf,
            formatted_buf_len,
            unformatted_buf,
            unformatted_buf_len,
        );
        write_leading_zero_capability(Some(u), leading_zero_buf, leading_zero_buf_len);

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
        if !pos_out.is_null() {
            unsafe {
                *pos_out = unit_current_fortran_pos(u);
            }
        }
    } else {
        write_inquire_string(name_buf, name_buf_len, "");
        write_inquire_string(access_buf, access_buf_len, "UNDEFINED");
        write_inquire_string(form_buf, form_buf_len, "UNDEFINED");
        write_inquire_string(action_buf, action_buf_len, "UNDEFINED");
        write_action_capabilities(
            None,
            read_buf,
            read_buf_len,
            write_buf,
            write_buf_len,
            readwrite_buf,
            readwrite_buf_len,
        );
        write_access_capabilities(
            None,
            sequential_buf,
            sequential_buf_len,
            direct_buf,
            direct_buf_len,
            stream_buf,
            stream_buf_len,
        );
        write_form_capabilities(
            None,
            formatted_buf,
            formatted_buf_len,
            unformatted_buf,
            unformatted_buf_len,
        );
        write_leading_zero_capability(None, leading_zero_buf, leading_zero_buf_len);
        if !size_out.is_null() {
            unsafe {
                *size_out = -1;
            }
        }
        if !pos_out.is_null() {
            unsafe {
                *pos_out = -1;
            }
        }
    }

    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
}

/// Fill READ=, WRITE=, READWRITE= INQUIRE specifiers based on a unit's
/// declared `Action`.  Per F2018 §12.10.2, READ returns YES if the unit
/// can be read, NO otherwise, and similarly for WRITE.  Disconnected
/// units (`action = None`) report UNKNOWN for all three.
fn write_action_capabilities(
    action: Option<Action>,
    read_buf: *mut u8,
    read_buf_len: i64,
    write_buf: *mut u8,
    write_buf_len: i64,
    readwrite_buf: *mut u8,
    readwrite_buf_len: i64,
) {
    let (read_cap, write_cap, rw_cap) = match action {
        Some(Action::Read) => ("YES", "NO", "NO"),
        Some(Action::Write) => ("NO", "YES", "NO"),
        Some(Action::ReadWrite) => ("YES", "YES", "YES"),
        None => ("UNKNOWN", "UNKNOWN", "UNKNOWN"),
    };
    write_inquire_string(read_buf, read_buf_len, read_cap);
    write_inquire_string(write_buf, write_buf_len, write_cap);
    write_inquire_string(readwrite_buf, readwrite_buf_len, rw_cap);
}

/// Fill SEQUENTIAL=, DIRECT=, STREAM= INQUIRE specifiers based on a
/// connected unit's access mode. Disconnected files/units report UNKNOWN.
fn write_access_capabilities(
    access: Option<Access>,
    sequential_buf: *mut u8,
    sequential_buf_len: i64,
    direct_buf: *mut u8,
    direct_buf_len: i64,
    stream_buf: *mut u8,
    stream_buf_len: i64,
) {
    let (sequential_cap, direct_cap, stream_cap) = match access {
        Some(Access::Sequential) => ("YES", "NO", "NO"),
        Some(Access::Direct) => ("NO", "YES", "NO"),
        Some(Access::Stream) => ("NO", "NO", "YES"),
        None => ("UNKNOWN", "UNKNOWN", "UNKNOWN"),
    };
    write_inquire_string(sequential_buf, sequential_buf_len, sequential_cap);
    write_inquire_string(direct_buf, direct_buf_len, direct_cap);
    write_inquire_string(stream_buf, stream_buf_len, stream_cap);
}

/// Fill FORMATTED= and UNFORMATTED= INQUIRE specifiers based on a
/// connected unit's form. Disconnected files/units report UNKNOWN.
fn write_form_capabilities(
    form: Option<&Form>,
    formatted_buf: *mut u8,
    formatted_buf_len: i64,
    unformatted_buf: *mut u8,
    unformatted_buf_len: i64,
) {
    let (formatted_cap, unformatted_cap) = match form {
        Some(Form::Formatted) => ("YES", "NO"),
        Some(Form::Unformatted) => ("NO", "YES"),
        None => ("UNKNOWN", "UNKNOWN"),
    };
    write_inquire_string(formatted_buf, formatted_buf_len, formatted_cap);
    write_inquire_string(unformatted_buf, unformatted_buf_len, unformatted_cap);
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
    // Use try_lock instead of lock: process::exit invokes libc atexit handlers,
    // and any I/O routine that exited while holding io_state would deadlock here.
    // If the caller is already holding the lock during exit, their drop has already
    // unwound or process::exit released their mutex first; in the rare case where
    // the lock is genuinely contested, skip flush rather than hang the program.
    if let Ok(mut state) = io_state().try_lock() {
        for (_, unit) in state.units.iter_mut() {
            let _ = unit.flush();
        }
        // Delete any STATUS='SCRATCH' backing files left open at exit.
        let scratch_paths: Vec<String> = state
            .units
            .values()
            .filter(|u| u.scratch && !u.filename.is_empty())
            .map(|u| u.filename.clone())
            .collect();
        for path in scratch_paths {
            let _ = std::fs::remove_file(&path);
        }
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

use crate::format::{parse_format, FormatDesc, FormatEngine, IoValue, LeadingZeroMode};
use std::cell::RefCell;

enum FmtSink {
    Unit(i32),
    Internal { buf: *mut u8, buf_len: usize },
    /// Internal write whose target is a deferred-length allocatable
    /// `character(:), allocatable` scalar. An already allocated target is
    /// treated as a fixed internal file; an unallocated target is allocated
    /// to the formatted record length.
    InternalAlloc { desc: *mut u8 },
}

/// Thread-local state for active formatted I/O operations.
struct FmtContext {
    sink: FmtSink,
    format_str: String,
    values: Vec<IoValue>,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
    /// Per-statement LEADING_ZERO= override (F2023). When set it seeds the
    /// format engine's leading-zero mode for this statement, beating the
    /// connection-level mode; format LZ/LZS/LZP descriptors still override
    /// it mid-string.
    stmt_leading_zero: Option<LeadingZeroMode>,
}

// SAFETY: FmtContext only lives inside a thread-local; the raw
// pointers are written by the same thread that begins the I/O.
unsafe impl Send for FmtContext {}

thread_local! {
    static FMT_CTX: RefCell<Vec<FmtContext>> = const { RefCell::new(Vec::new()) };
}

/// Begin a formatted write operation. Parses the format string and prepares
/// to accumulate values.
#[no_mangle]
pub extern "C" fn afs_fmt_begin(unit: i32, fmt_str: *const u8, fmt_len: i64) {
    afs_fmt_begin_ex(
        unit,
        fmt_str,
        fmt_len,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        0,
    );
}

/// Extended formatted-write begin: accepts iostat and iomsg pointers
/// captured from the WRITE-statement specs. Either pointer may be null.
#[no_mangle]
pub extern "C" fn afs_fmt_begin_ex(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    let fmt = unsafe_str(fmt_str, fmt_len);
    FMT_CTX.with(|ctx| {
        ctx.borrow_mut().push(FmtContext {
            sink: FmtSink::Unit(unit),
            format_str: fmt,
            values: Vec::new(),
            iostat,
            iomsg,
            iomsg_len,
            stmt_leading_zero: None,
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
    afs_fmt_begin_internal_ex(
        buf,
        buf_len,
        fmt_str,
        fmt_len,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        0,
    );
}

/// Extended internal-write begin with iostat/iomsg plumbing.
#[no_mangle]
pub extern "C" fn afs_fmt_begin_internal_ex(
    buf: *mut u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    let fmt = unsafe_str(fmt_str, fmt_len);
    FMT_CTX.with(|ctx| {
        ctx.borrow_mut().push(FmtContext {
            sink: FmtSink::Internal {
                buf,
                buf_len: buf_len.max(0) as usize,
            },
            format_str: fmt,
            values: Vec::new(),
            iostat,
            iomsg,
            iomsg_len,
            stmt_leading_zero: None,
        });
    });
}

extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

/// Store formatted internal-write bytes into a deferred-length
/// StringDescriptor; returns false on allocation failure.
///
/// If the descriptor is already allocated, a formatted internal WRITE treats
/// the existing allocation as the fixed internal file: copy/truncate the
/// record into the current length and space-pad the remainder. If the
/// descriptor is unallocated, allocate exactly enough storage for the record.
/// Storage is malloc'd to match `afs_dealloc_string`'s free.
fn store_internal_alloc_record(
    desc: *mut crate::descriptor::StringDescriptor,
    bytes: &[u8],
) -> bool {
    use crate::descriptor::{STR_ALLOCATED, STR_DEFERRED};
    if desc.is_null() {
        return true;
    }
    let d = unsafe { &mut *desc };
    let n = bytes.len() as i64;
    if d.is_allocated() && !d.data.is_null() {
        write_to_buffer(
            d.data,
            d.len.max(0) as usize,
            0,
            bytes,
            std::ptr::null_mut(),
        );
        d.flags |= STR_DEFERRED;
        return true;
    }
    if n <= 0 {
        d.len = 0;
        d.flags |= STR_ALLOCATED | STR_DEFERRED;
        return true;
    }
    if n > d.capacity || d.data.is_null() {
        let newp = unsafe { malloc(n as usize) };
        if newp.is_null() {
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), newp, n as usize);
            if d.is_allocated() && !d.data.is_null() {
                free(d.data);
            }
        }
        d.data = newp;
        d.capacity = n;
    } else {
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), d.data, n as usize);
        }
    }
    d.len = n;
    d.flags |= STR_ALLOCATED | STR_DEFERRED;
    true
}

/// Begin a formatted internal write whose target is a deferred-length
/// allocatable `character(:), allocatable` scalar. `desc` points at the
/// 32-byte StringDescriptor; at `afs_fmt_end` the formatted record is
/// written into the existing allocation when present, or allocated when
/// absent.
#[no_mangle]
pub extern "C" fn afs_fmt_begin_internal_alloc(
    desc: *mut u8,
    fmt_str: *const u8,
    fmt_len: i64,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    let fmt = unsafe_str(fmt_str, fmt_len);
    FMT_CTX.with(|ctx| {
        ctx.borrow_mut().push(FmtContext {
            sink: FmtSink::InternalAlloc { desc },
            format_str: fmt,
            values: Vec::new(),
            iostat,
            iomsg,
            iomsg_len,
            stmt_leading_zero: None,
        });
    });
}

/// Set the per-statement LEADING_ZERO= override for the in-flight
/// formatted write. Called between `afs_fmt_begin*` and `afs_fmt_end`
/// when the WRITE statement carries a LEADING_ZERO= specifier. The
/// string is the specifier value (`'PRINT'`/`'SUPPRESS'`/
/// `'PROCESSOR_DEFINED'`); it overrides the connection-level mode for
/// this statement only.
#[no_mangle]
pub extern "C" fn afs_fmt_set_leading_zero(ptr: *const u8, len: i64) {
    let mode = LeadingZeroMode::from_specifier(&unsafe_str(ptr, len));
    FMT_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.stmt_leading_zero = Some(mode);
        }
    });
}

/// Push an integer value for formatted output.
#[no_mangle]
pub extern "C" fn afs_fmt_push_int(val: i64) {
    FMT_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.values.push(IoValue::Integer(val as i128));
        }
    });
}

/// Push an integer(16) value for formatted output.
#[no_mangle]
pub extern "C" fn afs_fmt_push_int128(val: *const i128) {
    FMT_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
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
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.values.push(IoValue::Real(val));
        }
    });
}

/// Push a logical value for formatted output.
#[no_mangle]
pub extern "C" fn afs_fmt_push_logical(val: i32) {
    FMT_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
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
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.values.push(IoValue::Character(bytes));
        }
    });
}

/// End the formatted write: apply the format engine and write the result.
/// If advance is true (nonzero), appends a newline. If false (zero), no newline.
#[no_mangle]
pub extern "C" fn afs_fmt_end(advance: i32) {
    FMT_CTX.with(|ctx| {
        let context = ctx.borrow_mut().pop();
        if let Some(c) = context {
            let descriptors = parse_format(&c.format_str);
            let mut engine = FormatEngine::new(descriptors);

            // Track success across the sink branches. List-directed and
            // scalar formatted writes both leave `iostat` untouched on
            // older builds; stdlib's savetxt loops `if (ios/=0) error_stop`
            // on the post-write value, so a write that silently leaves the
            // pre-call sentinel in place trips the error-stop branch every
            // iteration. Set `*iostat = 0` on success and stash an empty
            // iomsg so callers see a clean state.
            let mut io_status: i32 = 0;
            let mut io_msg: Option<&'static str> = None;

            match c.sink {
                FmtSink::Unit(unit) => {
                    let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
                    // Seed the leading-zero mode: the statement override
                    // (LEADING_ZERO= on WRITE) beats the connection mode
                    // (LEADING_ZERO= on OPEN); format LZ/LZS/LZP descriptors
                    // still override mid-string via apply_descriptors.
                    let conn_mode = state
                        .units
                        .get(&unit)
                        .map(|u| u.leading_zero)
                        .unwrap_or(LeadingZeroMode::Default);
                    engine.set_leading_zero(c.stmt_leading_zero.unwrap_or(conn_mode));
                    match engine.format_values_reverting_checked(&c.values) {
                    Ok(output) => {
                        if let Some(u) = state.get_unit(unit) {
                            if u.write_str(&output).is_err() {
                                io_status = 1;
                                io_msg = Some("write failed");
                            }
                            if io_status == 0 && advance != 0 && u.write_str("\n").is_err() {
                                io_status = 1;
                                io_msg = Some("write failed");
                            }
                        } else {
                            io_status = 1;
                            io_msg = Some("unit not connected");
                        }
                    }
                    Err(_) => {
                        io_status = 1;
                        io_msg = Some("format error");
                    }
                    }
                }
                FmtSink::Internal { buf, buf_len } => {
                    if let Some(mode) = c.stmt_leading_zero {
                        engine.set_leading_zero(mode);
                    }
                    match engine.format_values_checked(&c.values) {
                        Ok(output) => {
                            write_to_buffer(
                                buf,
                                buf_len,
                                0,
                                output.as_bytes(),
                                std::ptr::null_mut(),
                            );
                        }
                        Err(_) => {
                            io_status = 1;
                            io_msg = Some("format error");
                        }
                    }
                }
                FmtSink::InternalAlloc { desc } => {
                    if let Some(mode) = c.stmt_leading_zero {
                        engine.set_leading_zero(mode);
                    }
                    match engine.format_values_checked(&c.values) {
                        Ok(output) => {
                            if !store_internal_alloc_record(
                                desc as *mut crate::descriptor::StringDescriptor,
                                output.as_bytes(),
                            ) {
                                io_status = 1;
                                io_msg = Some("out of memory");
                            }
                        }
                        Err(_) => {
                            io_status = 1;
                            io_msg = Some("format error");
                        }
                    }
                }
            }

            if !c.iostat.is_null() {
                unsafe { *c.iostat = io_status };
            }
            if !c.iomsg.is_null() && c.iomsg_len > 0 {
                let msg = io_msg.unwrap_or("");
                let cap = c.iomsg_len as usize;
                let bytes = msg.as_bytes();
                let copy = bytes.len().min(cap);
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), c.iomsg, copy);
                    if copy < cap {
                        // Pad remainder with spaces per Fortran character semantics.
                        std::ptr::write_bytes(c.iomsg.add(copy), b' ', cap - copy);
                    }
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

fn read_nonadvancing_formatted_field(
    desc: &FormatDesc,
    input: &[u8],
    cursor: &mut usize,
    dest_len: i64,
) -> Option<String> {
    match desc {
        FormatDesc::Character { width: None } => {
            let start = (*cursor).min(input.len());
            let n = dest_len.max(0) as usize;
            let end = start.saturating_add(n).min(input.len());
            *cursor = end;
            Some(String::from_utf8_lossy(&input[start..end]).into_owned())
        }
        _ => read_formatted_field(desc, input, cursor),
    }
}

fn extract_nth_nonadvancing_formatted_field(
    descs: &[FormatDesc],
    input: &[u8],
    cursor: &mut usize,
    remaining_data_index: &mut usize,
    dest_len: i64,
) -> Option<(FormatDesc, String)> {
    for desc in descs {
        match desc {
            FormatDesc::Group {
                repeat,
                descriptors,
            } => {
                for _ in 0..*repeat {
                    if let Some(found) = extract_nth_nonadvancing_formatted_field(
                        descriptors,
                        input,
                        cursor,
                        remaining_data_index,
                        dest_len,
                    ) {
                        return Some(found);
                    }
                }
            }
            FormatDesc::UnlimitedRepeat { descriptors } => {
                let mut loop_guard = 0usize;
                while *cursor < input.len() && loop_guard < input.len().saturating_add(1) {
                    let before = *cursor;
                    if let Some(found) = extract_nth_nonadvancing_formatted_field(
                        descriptors,
                        input,
                        cursor,
                        remaining_data_index,
                        dest_len,
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
                if let Some(field) =
                    read_nonadvancing_formatted_field(desc, input, cursor, dest_len)
                {
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

fn nonadvancing_char_field_hit_eor(desc: &FormatDesc, field: &str, dest_len: i64) -> bool {
    match desc {
        FormatDesc::Character { width: Some(width) } => field.len() < *width,
        FormatDesc::Character { width: None } => field.len() < dest_len.max(0) as usize,
        _ => true,
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
    // If a prior `read(...,advance='NO')` left a partial in-flight
    // record with cursor in the middle, consume from that cursor first
    // and then mark the record as fully consumed (advancing past it
    // for the next statement). Without this, switching from
    // non-advancing to advancing — exactly what stdlib's
    // `read_bitset_unit_64` does on its final-bit read — would call
    // `read_line` and discard the cursor, returning the wrong char or
    // EOF for files with a single physical record.
    {
        let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(u) = state.get_unit(unit) {
            // Only treat the in-flight record as partial when the
            // cursor has actually advanced past 0 — i.e. a previous
            // `read(...,advance='NO')` consumed some chars. A cursor
            // of 0 means the record was set up by `read_line` for an
            // advancing read but never partially consumed; in that
            // case we must let the next advancing read pull a fresh
            // record (otherwise we'd re-deliver the same line).
            let has_partial_record = u.formatted_read_cursor > 0
                && u.formatted_read_record.is_some()
                && u.formatted_read_cursor
                    < u.formatted_read_record
                        .as_ref()
                        .map(|r| r.len())
                        .unwrap_or(0);
            if has_partial_record {
                let fmt = unsafe_str(fmt_str, fmt_len);
                let descs = parse_format(&fmt);
                let input = u
                    .formatted_read_record
                    .as_ref()
                    .map(|l| l.as_bytes().to_vec())
                    .unwrap_or_default();
                let mut cursor = u.formatted_read_cursor;
                let mut remaining = 0usize;
                let outcome =
                    extract_nth_formatted_field(&descs, &input, &mut cursor, &mut remaining);
                u.formatted_read_record = None;
                u.formatted_read_cursor = 0;
                drop(state);
                match outcome {
                    Some((FormatDesc::Character { .. }, field)) => {
                        store_formatted_char_result(&field, dest, dest_len, size_out, iostat);
                    }
                    _ => {
                        store_formatted_char_error(dest, dest_len, size_out, IOSTAT_EOR);
                        if !iostat.is_null() {
                            unsafe {
                                *iostat = IOSTAT_EOR;
                            }
                        }
                    }
                }
                return;
            }
        }
    }
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

    match extract_nth_nonadvancing_formatted_field(
        &descs,
        &input,
        &mut cursor,
        &mut remaining,
        dest_len,
    ) {
        Some((desc @ FormatDesc::Character { .. }, field)) => {
            store_formatted_char_result(&field, dest, dest_len, size_out, iostat);
            if cursor >= input.len() {
                if nonadvancing_char_field_hit_eor(&desc, &field, dest_len) {
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = IOSTAT_EOR;
                        }
                    }
                    u.formatted_read_record = None;
                    u.formatted_read_cursor = 0;
                } else {
                    u.formatted_read_cursor = cursor;
                }
            } else {
                u.formatted_read_cursor = cursor;
            }
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
                    write_i32_ptr(val, v);
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
                    write_i64_ptr(val, v);
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
            let normalized = normalize_fortran_real_input(&field, true);
            match normalized.parse::<f64>() {
                Ok(v) => {
                    write_f64_ptr(val, v);
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
                    write_i32_ptr(val, v);
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
                    write_i64_ptr(val, v);
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
            let normalized = normalize_fortran_real_input(&field, true);
            match normalized.parse::<f64>() {
                Ok(v) => {
                    write_f64_ptr(val, v);
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
    fn normalize_real_input_accepts_implicit_signed_exponents() {
        assert_eq!(normalize_fortran_real_input("1.-3", false), "1.e-3");
        assert_eq!(normalize_fortran_real_input("1.+3", false), "1.e+3");
        assert_eq!(
            normalize_fortran_real_input("1234567890-9", false),
            "1234567890e-9"
        );
        assert_eq!(
            normalize_fortran_real_input("-123456.789+2", false),
            "-123456.789e+2"
        );
    }

    #[test]
    fn normalize_real_input_preserves_explicit_exponents() {
        assert_eq!(normalize_fortran_real_input("1.0d-3", false), "1.0e-3");
        assert_eq!(normalize_fortran_real_input("1.0D+3", false), "1.0E+3");
        assert_eq!(normalize_fortran_real_input("-Inf", false), "-Inf");
        assert_eq!(normalize_fortran_real_input("NaN", false), "NaN");
    }

    #[test]
    fn internal_real_read_accepts_implicit_signed_exponents() {
        let buf = b"1.-3 1.+3 1234567890-9";
        let mut pos = 0i64;
        let mut first = 0.0f64;
        let mut second = 0.0f64;
        let mut third = 0.0f64;
        let mut iostat = -99i32;

        afs_read_internal_real(
            buf.as_ptr(),
            buf.len() as i64,
            &mut pos,
            &mut first,
            &mut iostat,
        );
        assert_eq!(iostat, 0, "expected first internal real read to succeed");
        afs_read_internal_real(
            buf.as_ptr(),
            buf.len() as i64,
            &mut pos,
            &mut second,
            &mut iostat,
        );
        assert_eq!(iostat, 0, "expected second internal real read to succeed");
        afs_read_internal_real(
            buf.as_ptr(),
            buf.len() as i64,
            &mut pos,
            &mut third,
            &mut iostat,
        );
        assert_eq!(iostat, 0, "expected third internal real read to succeed");

        assert!((first - 1.0e-3).abs() < 1.0e-15, "first={first}");
        assert!((second - 1.0e3).abs() < 1.0e-12, "second={second}");
        assert!((third - 1.234567890).abs() < 1.0e-12, "third={third}");
    }

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
    fn status_old_missing_file_reports_iostat_without_creating() {
        let path = format!(
            "/tmp/afs_open_status_old_missing_{}_{}.dat",
            std::process::id(),
            line!()
        );
        let _ = std::fs::remove_file(&path);

        let mut iostat = -99i32;
        let cb = OpenControlBlock {
            unit: 781,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "old".as_ptr(),
            status_len: 3,
            action: "write".as_ptr(),
            action_len: 5,
            access: std::ptr::null(),
            access_len: 0,
            form: std::ptr::null(),
            form_len: 0,
            recl: 0,
            iostat: &mut iostat,
            newunit: std::ptr::null_mut(),
            position: "append".as_ptr(),
            position_len: 6,
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
        };

        afs_open(&cb);
        assert_ne!(iostat, 0, "STATUS='old' must fail for missing files");
        assert!(
            !std::path::Path::new(&path).exists(),
            "STATUS='old' OPEN must not create the missing file"
        );

        iostat = -99;
        let cb = OpenControlBlock {
            unit: 782,
            action: "read".as_ptr(),
            action_len: 4,
            position: "asis".as_ptr(),
            position_len: 4,
            iostat: &mut iostat,
            ..cb
        };

        afs_open(&cb);
        assert_ne!(iostat, 0, "STATUS='old' read must fail for missing files");
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
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
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
    fn stream_seek_uses_fortran_one_based_position() {
        let path = format!(
            "/tmp/afs_stream_seek_one_based_{}_{}.dat",
            std::process::id(),
            line!()
        );
        let _ = std::fs::remove_file(&path);
        let mut iostat = -99i32;

        let write_cb = OpenControlBlock {
            unit: 784,
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
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
        };
        afs_open(&write_cb);
        assert_eq!(iostat, 0, "expected stream OPEN for writing to succeed");

        afs_write_string(784, "Hello".as_ptr(), 5);
        afs_close(784, &mut iostat);
        assert_eq!(iostat, 0, "expected stream write close to succeed");

        iostat = -99;
        let read_cb = OpenControlBlock {
            unit: 785,
            action: "read".as_ptr(),
            action_len: 4,
            status: "old".as_ptr(),
            status_len: 3,
            iostat: &mut iostat,
            ..write_cb
        };
        afs_open(&read_cb);
        assert_eq!(iostat, 0, "expected stream OPEN for reading to succeed");

        afs_seek_stream(785, 5, &mut iostat);
        assert_eq!(iostat, 0, "expected POS=5 seek to succeed");
        let mut tail = [0u8; 1];
        afs_read_string(785, tail.as_mut_ptr(), tail.len() as i64, &mut iostat);
        assert_eq!(iostat, 0, "expected tail character read to succeed");
        assert_eq!(&tail, b"o");

        afs_seek_stream(785, 1, &mut iostat);
        assert_eq!(iostat, 0, "expected POS=1 seek to succeed");
        let mut text = [0u8; 5];
        afs_read_string(785, text.as_mut_ptr(), text.len() as i64, &mut iostat);
        assert_eq!(iostat, 0, "expected rewind read to succeed");
        assert_eq!(&text, b"Hello");

        afs_seek_stream(785, 0, &mut iostat);
        assert_ne!(iostat, 0, "POS=0 is invalid in Fortran stream I/O");

        afs_close_ex(785, "delete".as_ptr(), 6, &mut iostat);
        assert_eq!(iostat, 0, "expected stream close/delete to succeed");
    }

    #[test]
    fn stream_open_defaults_to_unformatted() {
        let path = format!(
            "/tmp/afs_stream_default_unformatted_{}_{}.dat",
            std::process::id(),
            line!()
        );
        let _ = std::fs::remove_file(&path);
        let mut iostat = -99i32;
        let cb = OpenControlBlock {
            unit: 786,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "replace".as_ptr(),
            status_len: 7,
            action: "write".as_ptr(),
            action_len: 5,
            access: "stream".as_ptr(),
            access_len: 6,
            form: std::ptr::null(),
            form_len: 0,
            recl: 0,
            iostat: &mut iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
        };

        afs_open(&cb);
        assert_eq!(iostat, 0, "expected default stream OPEN to succeed");
        afs_write_string(786, "alpha".as_ptr(), 5);
        afs_write_newline(786);
        afs_close(786, &mut iostat);
        assert_eq!(iostat, 0, "expected stream close to succeed");

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, b"alpha", "stream default must be unformatted");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_leading_zero_connection_mode_and_inquire() {
        // OPEN(...,LEADING_ZERO='SUPPRESS') seeds the connection mode; a
        // plain (F6.3) write to that unit drops the leading zero. A WRITE
        // statement override beats the connection mode; INQUIRE reads the
        // connection's current mode back.
        let path = format!(
            "/tmp/afs_lz_conn_{}_{}.txt",
            std::process::id(),
            line!()
        );
        let _ = std::fs::remove_file(&path);
        let mut iostat = -99i32;
        let cb = OpenControlBlock {
            unit: 821,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "replace".as_ptr(),
            status_len: 7,
            action: "write".as_ptr(),
            action_len: 5,
            access: std::ptr::null(),
            access_len: 0,
            form: std::ptr::null(),
            form_len: 0,
            recl: 0,
            iostat: &mut iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
            leading_zero: "SUPPRESS".as_ptr(),
            leading_zero_len: 8,
        };
        afs_open(&cb);
        assert_eq!(iostat, 0, "expected formatted OPEN to succeed");

        // INQUIRE reads back the connection mode.
        let mut lz = [b'?'; 16];
        afs_inquire_unit(
            821,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            lz.as_mut_ptr(),
            lz.len() as i64,
        );
        assert_eq!(&lz[..8], b"SUPPRESS");

        // Connection mode applies to a plain format with no LZ descriptor.
        afs_fmt_begin(821, "(F6.3)".as_ptr(), 6);
        afs_fmt_push_real(0.25);
        afs_fmt_end(1);

        // Statement override (PRINT) beats the SUPPRESS connection mode.
        afs_fmt_begin(821, "(F6.3)".as_ptr(), 6);
        afs_fmt_set_leading_zero("PRINT".as_ptr(), 5);
        afs_fmt_push_real(0.25);
        afs_fmt_end(1);

        afs_close(821, &mut iostat);
        assert_eq!(iostat, 0, "expected close to succeed");

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[0].trim(), ".250", "connection SUPPRESS drops zero");
        assert_eq!(lines[1].trim(), "0.250", "statement PRINT keeps zero");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn internal_write_allocates_unallocated_and_preserves_allocated_len() {
        use crate::descriptor::StringDescriptor;
        let mut desc = StringDescriptor::zeroed();
        let dptr = &mut desc as *mut StringDescriptor as *mut u8;

        // Grow from unallocated: write 'val=42!' (7 chars).
        afs_fmt_begin_internal_alloc(
            dptr,
            "(A,I0,A)".as_ptr(),
            8,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        afs_fmt_push_string("val=".as_ptr(), 4);
        afs_fmt_push_int(42);
        afs_fmt_push_string("!".as_ptr(), 1);
        afs_fmt_end(0);
        assert_eq!(desc.len, 7);
        let bytes = unsafe { std::slice::from_raw_parts(desc.data, desc.len as usize) };
        assert_eq!(bytes, b"val=42!");
        let grown_cap = desc.capacity;

        // Already allocated: behave like a fixed internal file, preserving
        // length and padding the remaining buffer with spaces.
        afs_fmt_begin_internal_alloc(
            dptr,
            "(A)".as_ptr(),
            3,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        afs_fmt_push_string("x".as_ptr(), 1);
        afs_fmt_end(0);
        assert_eq!(desc.len, 7);
        assert_eq!(desc.capacity, grown_cap);
        let bytes = unsafe { std::slice::from_raw_parts(desc.data, desc.len as usize) };
        assert_eq!(bytes, b"x      ");

        crate::string::afs_dealloc_string(dptr as *mut StringDescriptor);
    }

    #[test]
    fn formatted_context_restores_outer_write_after_nested_internal_write() {
        use crate::descriptor::StringDescriptor;
        let path = "/tmp/afs_fmt_nested_context_test.dat";
        let mut iostat = 0;
        afs_open_simple(
            822,
            path.as_ptr(),
            path.len() as i64,
            "replace".as_ptr(),
            7,
            std::ptr::null(),
            0,
        );

        let mut desc = StringDescriptor::zeroed();
        let dptr = &mut desc as *mut StringDescriptor as *mut u8;
        afs_fmt_begin(822, "(A)".as_ptr(), 3);
        afs_fmt_begin_internal_alloc(
            dptr,
            "(A)".as_ptr(),
            3,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        afs_fmt_push_string("inner".as_ptr(), 5);
        afs_fmt_end(0);

        afs_fmt_push_string(desc.data, desc.len);
        afs_fmt_end(1);
        afs_close(822, &mut iostat);

        assert_eq!(iostat, 0, "expected close to succeed");
        let content = std::fs::read_to_string(path).unwrap();
        assert_eq!(content, "inner\n");
        crate::string::afs_dealloc_string(dptr as *mut StringDescriptor);
        let _ = std::fs::remove_file(path);
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
    fn formatted_unit_write_reverts_format_across_records() {
        let path = "/tmp/afs_fmt_reversion_test.dat";
        afs_open_simple(
            88,
            path.as_ptr(),
            path.len() as i64,
            "replace".as_ptr(),
            7,
            std::ptr::null(),
            0,
        );

        afs_fmt_begin(88, "(A)".as_ptr(), 3);
        afs_fmt_push_string("abc".as_ptr(), 3);
        afs_fmt_push_string("def".as_ptr(), 3);
        afs_fmt_push_string("ghi".as_ptr(), 3);
        afs_fmt_end(1);

        afs_close(88, std::ptr::null_mut());

        let content = std::fs::read_to_string(path).unwrap();
        assert_eq!(content, "abc\ndef\nghi\n");
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
    fn formatted_noadvance_read_returns_eor_with_final_chunk() {
        let path = "/tmp/afs_fmt_noadvance_read_records_test.dat";
        std::fs::write(path, "abc\nwxyz\n").unwrap();

        afs_open_simple(
            91,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let mut first = [b' '; 8];
        let mut second = [b' '; 8];
        let mut first_size = -99i32;
        let mut second_size = -99i32;
        let mut iostat = -99i32;

        afs_fmt_read_string_noadvance(
            91,
            "(a)".as_ptr(),
            3,
            first.as_mut_ptr(),
            first.len() as i64,
            &mut first_size,
            &mut iostat,
        );
        assert_eq!(iostat, IOSTAT_EOR);
        assert_eq!(first_size, 3);
        assert_eq!(&first[..3], b"abc");

        afs_fmt_read_string_noadvance(
            91,
            "(a)".as_ptr(),
            3,
            second.as_mut_ptr(),
            second.len() as i64,
            &mut second_size,
            &mut iostat,
        );
        afs_close(91, std::ptr::null_mut());

        assert_eq!(iostat, IOSTAT_EOR);
        assert_eq!(second_size, 4);
        assert_eq!(&second[..4], b"wxyz");
    }

    #[test]
    fn formatted_noadvance_unbounded_a_chunks_long_record() {
        let path = "/tmp/afs_fmt_noadvance_unbounded_a_long_record_test.dat";
        std::fs::write(path, format!("{}\n", "x".repeat(5000))).unwrap();

        afs_open_simple(
            89,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let mut first = vec![b' '; 4096];
        let mut second = vec![b' '; 4096];
        let mut first_size = -99i32;
        let mut second_size = -99i32;
        let mut iostat = -99i32;

        afs_fmt_read_string_noadvance(
            89,
            "(a)".as_ptr(),
            3,
            first.as_mut_ptr(),
            first.len() as i64,
            &mut first_size,
            &mut iostat,
        );
        assert_eq!(iostat, 0);
        assert_eq!(first_size, 4096);
        assert!(first.iter().all(|&b| b == b'x'));

        afs_fmt_read_string_noadvance(
            89,
            "(a)".as_ptr(),
            3,
            second.as_mut_ptr(),
            second.len() as i64,
            &mut second_size,
            &mut iostat,
        );
        afs_close(89, std::ptr::null_mut());

        assert_eq!(iostat, IOSTAT_EOR);
        assert_eq!(second_size, 904);
        assert!(second[..904].iter().all(|&b| b == b'x'));
    }

    #[test]
    fn formatted_noadvance_a1_returns_exact_final_byte_before_eor() {
        let path = "/tmp/afs_fmt_noadvance_a1_nul_test.dat";
        std::fs::write(path, b"A\0B\n").unwrap();

        afs_open_simple(
            90,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let mut ch = [b' '; 1];
        let mut size = -99i32;
        let mut iostat = -99i32;
        for expected in [b'A', 0, b'B'] {
            afs_fmt_read_string_noadvance(
                90,
                "(A1)".as_ptr(),
                4,
                ch.as_mut_ptr(),
                ch.len() as i64,
                &mut size,
                &mut iostat,
            );
            assert_eq!(iostat, 0);
            assert_eq!(size, 1);
            assert_eq!(ch[0], expected);
        }

        afs_fmt_read_string_noadvance(
            90,
            "(A1)".as_ptr(),
            4,
            ch.as_mut_ptr(),
            ch.len() as i64,
            &mut size,
            &mut iostat,
        );
        afs_close(90, std::ptr::null_mut());

        assert_eq!(iostat, IOSTAT_EOR);
        assert_eq!(size, 0);
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
