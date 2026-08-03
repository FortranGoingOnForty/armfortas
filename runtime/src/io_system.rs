//! Fortran I/O subsystem — unit management, list-directed and formatted I/O.
//!
//! The I/O registry is global (Fortran I/O units are program-wide).
//! Registry changes and individual unit operations have separate locks,
//! so a blocking operation cannot stall unrelated units.
//!
//! Preconnected units:
//! - Unit 5 → stdin
//! - Unit 6 → stdout
//! - Unit 0 → stderr
//! - * in I/O statements → unit 5 (read) or 6 (write)

use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::ffi::c_void;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, IsTerminal, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
extern "C" {
    #[link_name = "read"]
    fn libc_read(fd: i32, buf: *mut c_void, count: usize) -> isize;
}

// ---- Global I/O state ----

use std::sync::OnceLock;

fn io_state() -> &'static Mutex<IoState> {
    static STATE: OnceLock<Mutex<IoState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(IoState::new()))
}

fn os_string_to_bytes(s: OsString) -> Vec<u8> {
    #[cfg(unix)]
    {
        s.into_vec()
    }
    #[cfg(not(unix))]
    {
        s.to_string_lossy().into_owned().into_bytes()
    }
}

fn path_from_filename(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn display_filename(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn scratch_filename(unit: i32) -> Vec<u8> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir();
    os_string_to_bytes(
        dir.join(format!(
            "afs_scratch_{pid}_{}_{seq}.tmp",
            unit.unsigned_abs()
        ))
        .into_os_string(),
    )
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

fn assign_iomsg(dst: *mut u8, dst_len: i64, msg: &str) {
    if dst.is_null() || dst_len <= 0 {
        return;
    }
    let cap = dst_len as usize;
    let bytes = msg.as_bytes();
    let copy = bytes.len().min(cap);
    unsafe {
        if copy > 0 {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, copy);
        }
        if copy < cap {
            std::ptr::write_bytes(dst.add(copy), b' ', cap - copy);
        }
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
    #[cfg(test)]
    TestRead(Box<dyn BufRead + Send>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ListReadToken {
    Null,
    Value(String),
}

struct Unit {
    _number: i32,
    stream: UnitStream,
    filename: Vec<u8>,
    _status: UnitStatus,
    access: Access,
    form: Form,
    action: Action,
    /// Record length for direct access (in bytes). None for sequential/stream.
    recl: Option<i64>,
    /// Buffered tokens from the current input record for list-directed READ.
    read_tokens: VecDeque<ListReadToken>,
    /// Cached formatted input record for the current READ statement.
    formatted_read_record: Option<Vec<u8>>,
    /// Cursor within a cached formatted input record for ADVANCE='NO' reads.
    formatted_read_cursor: usize,
    /// True after a non-advancing terminal read returned bytes without
    /// seeing a newline. If the next terminal read hits EOF, report EOR
    /// once for that open record before reporting END.
    terminal_nonadvancing_open_record: bool,
    /// True when the most recent formatted list-directed output item was
    /// character. Adjacent character items concatenate without another
    /// separator; any non-character item breaks the run.
    last_list_output_char: bool,
    /// True between `afs_list_write_begin` and `afs_list_write_end`.
    list_write_active: bool,
    /// Nesting depth for child data transfers on the same unit. Defined I/O
    /// procedures must append to their parent's record rather than replacing
    /// or prematurely draining it.
    list_write_depth: usize,
    /// First error raised while emitting the current list-directed WRITE.
    list_write_error: Option<String>,
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
    /// Nesting depth for sequential-unformatted child reads sharing
    /// `pending_read` with their parent transfer statement.
    list_read_depth: usize,
}

fn scan_list_directed_token(input: &[u8], start: usize) -> Option<(ListReadToken, usize)> {
    let mut cursor = start.min(input.len());
    while cursor < input.len() && input[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor == input.len() {
        return None;
    }

    if input[cursor] == b',' {
        return Some((ListReadToken::Null, cursor + 1));
    }

    let token_start = cursor;
    if matches!(input[cursor], b'\'' | b'"') {
        let delimiter = input[cursor];
        cursor += 1;
        while cursor < input.len() {
            if input[cursor] != delimiter {
                cursor += 1;
                continue;
            }
            if cursor + 1 < input.len() && input[cursor + 1] == delimiter {
                cursor += 2;
                continue;
            }
            cursor += 1;
            break;
        }
        // Keep any non-separator suffix in the same raw token. The
        // character decoder will reject it instead of silently treating
        // the suffix as a second list item.
        while cursor < input.len() && !input[cursor].is_ascii_whitespace() && input[cursor] != b','
        {
            cursor += 1;
        }
    } else {
        while cursor < input.len() && !input[cursor].is_ascii_whitespace() && input[cursor] != b','
        {
            cursor += 1;
        }
    }
    let token_end = cursor;

    while cursor < input.len() && input[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor < input.len() && input[cursor] == b',' {
        cursor += 1;
    }

    Some((
        ListReadToken::Value(String::from_utf8_lossy(&input[token_start..token_end]).into_owned()),
        cursor,
    ))
}

fn tokenize_list_directed_record(line: &str) -> VecDeque<ListReadToken> {
    let mut tokens = VecDeque::new();
    let mut cursor = 0usize;
    while let Some((token, next_cursor)) = scan_list_directed_token(line.as_bytes(), cursor) {
        tokens.push_back(token);
        cursor = next_cursor;
    }
    tokens
}

fn decode_list_directed_character_value(token: &str) -> Result<String, ()> {
    let bytes = token.as_bytes();
    let Some(&delimiter) = bytes.first() else {
        return Ok(String::new());
    };
    if !matches!(delimiter, b'\'' | b'"') {
        return Ok(token.to_string());
    }
    if bytes.len() < 2 || bytes.last().copied() != Some(delimiter) {
        return Err(());
    }

    let mut decoded = Vec::with_capacity(bytes.len().saturating_sub(2));
    let mut cursor = 1usize;
    let content_end = bytes.len() - 1;
    while cursor < content_end {
        if bytes[cursor] != delimiter {
            decoded.push(bytes[cursor]);
            cursor += 1;
            continue;
        }
        if cursor + 1 >= content_end || bytes[cursor + 1] != delimiter {
            return Err(());
        }
        decoded.push(delimiter);
        cursor += 2;
    }

    String::from_utf8(decoded).map_err(|_| ())
}

impl Unit {
    fn is_unformatted(&self) -> bool {
        self.form == Form::Unformatted
    }

    fn reset_read_state_after_positioning(&mut self) {
        self.read_tokens.clear();
        self.formatted_read_record = None;
        self.formatted_read_cursor = 0;
        self.terminal_nonadvancing_open_record = false;
        self.pending_read = None;
        self.list_read_depth = 0;
    }

    fn remember_list_write_result(&mut self, result: io::Result<()>) {
        if self.list_write_active && self.list_write_error.is_none() {
            if let Err(err) = result {
                self.list_write_error = Some(err.to_string());
            }
        }
    }

    /// Append raw bytes to the in-flight unformatted record buffer if
    /// one is open, otherwise write directly to the stream.
    fn list_write_raw_or_buffer(&mut self, bytes: &[u8]) {
        if self.list_write_active && self.list_write_error.is_some() {
            return;
        }
        let result = if let Some(buf) = self.pending_record.as_mut() {
            buf.extend_from_slice(bytes);
            Ok(())
        } else {
            self.write_raw(bytes)
        };
        self.remember_list_write_result(result);
    }

    fn list_write_bytes(&mut self, data: &[u8]) {
        if self.list_write_active && self.list_write_error.is_some() {
            return;
        }
        let result = self.write_bytes(data);
        self.remember_list_write_result(result);
    }

    fn list_write_str(&mut self, value: &str) {
        self.list_write_bytes(value.as_bytes());
    }

    fn list_write_flush(&mut self) {
        let result = self.flush();
        self.remember_list_write_result(result);
    }

    /// Consume `n` bytes from the in-flight unformatted read record
    /// (advancing the cursor). Returns `Some(slice)` when the record
    /// has enough bytes. When this returns `None` and `pending_read`
    /// is still open, the active record is too short for the item.
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

    fn read_line_bytes(&mut self) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        match &mut self.stream {
            UnitStream::Stdin => {
                io::stdin().lock().read_until(b'\n', &mut line)?;
            }
            UnitStream::FileRead(r) => {
                r.read_until(b'\n', &mut line)?;
            }
            #[cfg(test)]
            UnitStream::TestRead(r) => {
                r.read_until(b'\n', &mut line)?;
            }
            UnitStream::FileRaw(f) => {
                let mut buf = [0u8; 8192];
                loop {
                    match f.read(&mut buf)? {
                        0 => break,
                        n => {
                            let newline_pos = buf[..n].iter().position(|&b| b == b'\n');
                            let take = newline_pos.map_or(n, |pos| pos + 1);
                            line.extend_from_slice(&buf[..take]);
                            if let Some(pos) = newline_pos {
                                let unread = n - (pos + 1);
                                if unread > 0 {
                                    f.seek(SeekFrom::Current(-(unread as i64)))?;
                                }
                                break;
                            }
                        }
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

    fn read_line(&mut self) -> io::Result<String> {
        self.read_line_bytes()
            .map(|line| String::from_utf8_lossy(&line).into_owned())
    }

    fn is_terminal(&self) -> bool {
        match &self.stream {
            UnitStream::Stdin => io::stdin().is_terminal(),
            UnitStream::FileRead(r) => r.get_ref().is_terminal(),
            UnitStream::FileRaw(f) => f.is_terminal(),
            _ => false,
        }
    }

    fn read_nonadvancing_bytes(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.stream {
            UnitStream::Stdin => read_stdin_unbuffered(buf),
            UnitStream::FileRead(r) => r.read(buf),
            UnitStream::FileRaw(f) => f.read(buf),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unit not open for reading",
            )),
        }
    }

    /// Get the next token for list-directed READ.
    /// Reads a new line if the token buffer is empty.
    fn next_read_token(&mut self) -> io::Result<Option<ListReadToken>> {
        // Consume from buffer first.
        if let Some(token) = self.read_tokens.pop_front() {
            return Ok(Some(token));
        }

        loop {
            let line = self.read_line()?;
            if line.is_empty() {
                return Ok(None);
            }
            let tokens = tokenize_list_directed_record(&line);
            if tokens.is_empty() {
                continue;
            }
            self.read_tokens = tokens;
            return Ok(self.read_tokens.pop_front());
        }
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

struct UnitConnection {
    // CLOSE/OPEN removes the handle from the registry, then takes this value.
    // An operation that cloned the handle before removal therefore either
    // finishes first or observes a disconnected connection after taking the lock.
    unit: Mutex<Option<Unit>>,
    filename: Vec<u8>,
    scratch_path: Option<Vec<u8>>,
}

impl UnitConnection {
    fn new(unit: Unit) -> Arc<Self> {
        let filename = unit.filename.clone();
        let scratch_path =
            (unit.scratch && !unit.filename.is_empty()).then(|| unit.filename.clone());
        Arc::new(Self {
            unit: Mutex::new(Some(unit)),
            filename,
            scratch_path,
        })
    }
}

type SharedUnit = Arc<UnitConnection>;

struct IoState {
    units: HashMap<i32, SharedUnit>,
    next_newunit: i32,
}

fn connected_unit(unit_num: i32) -> Option<SharedUnit> {
    io_state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .units
        .get(&unit_num)
        .cloned()
}

fn with_unit<R>(unit_num: i32, operation: impl FnOnce(&mut Unit) -> R) -> Option<R> {
    let connection = connected_unit(unit_num)?;
    let mut guard = connection
        .unit
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    guard.as_mut().map(operation)
}

fn disconnect_unit(connection: SharedUnit) -> Option<Unit> {
    connection
        .unit
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
}

impl IoState {
    fn new() -> Self {
        let mut units = HashMap::new();

        // Preconnected units.
        units.insert(
            5,
            UnitConnection::new(Unit {
                _number: 5,
                stream: UnitStream::Stdin,
                filename: b"stdin".to_vec(),
                _status: UnitStatus::Open,
                access: Access::Sequential,
                form: Form::Formatted,
                action: Action::Read,
                recl: None,
                read_tokens: VecDeque::new(),
                formatted_read_record: None,
                formatted_read_cursor: 0,
                terminal_nonadvancing_open_record: false,
                last_list_output_char: false,
                list_write_active: false,
                list_write_depth: 0,
                list_write_error: None,
                scratch: false,
                leading_zero: LeadingZeroMode::Default,
                pending_record: None,
                pending_read: None,
                list_read_depth: 0,
            }),
        );
        units.insert(
            6,
            UnitConnection::new(Unit {
                _number: 6,
                stream: UnitStream::Stdout,
                filename: b"stdout".to_vec(),
                _status: UnitStatus::Open,
                access: Access::Sequential,
                form: Form::Formatted,
                action: Action::Write,
                recl: None,
                read_tokens: VecDeque::new(),
                formatted_read_record: None,
                formatted_read_cursor: 0,
                terminal_nonadvancing_open_record: false,
                last_list_output_char: false,
                list_write_active: false,
                list_write_depth: 0,
                list_write_error: None,
                scratch: false,
                leading_zero: LeadingZeroMode::Default,
                pending_record: None,
                pending_read: None,
                list_read_depth: 0,
            }),
        );
        units.insert(
            0,
            UnitConnection::new(Unit {
                _number: 0,
                stream: UnitStream::Stderr,
                filename: b"stderr".to_vec(),
                _status: UnitStatus::Open,
                access: Access::Sequential,
                form: Form::Formatted,
                action: Action::Write,
                recl: None,
                read_tokens: VecDeque::new(),
                formatted_read_record: None,
                formatted_read_cursor: 0,
                terminal_nonadvancing_open_record: false,
                last_list_output_char: false,
                list_write_active: false,
                list_write_depth: 0,
                list_write_error: None,
                scratch: false,
                leading_zero: LeadingZeroMode::Default,
                pending_record: None,
                pending_read: None,
                list_read_depth: 0,
            }),
        );

        Self {
            units,
            next_newunit: -10,
        }
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
    pub iomsg: *mut u8,
    pub iomsg_len: i64,
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
        iomsg: std::ptr::null_mut(),
        iomsg_len: 0,
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
    let iomsg = cb.iomsg;
    let iomsg_len = cb.iomsg_len;
    let missing_filename = fname.is_empty();

    if is_scratch && !cb.filename.is_null() {
        let message = "OPEN: FILE= must not be specified with STATUS='SCRATCH'";
        assign_iomsg(iomsg, iomsg_len, message);
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
            return;
        }
        eprintln!("{message}");
        std::process::exit(1);
    }

    if access_str.trim() == "direct" {
        let message = "OPEN: ACCESS='DIRECT' is not implemented";
        assign_iomsg(iomsg, iomsg_len, message);
        if !iostat.is_null() {
            unsafe {
                *iostat = 1;
            }
            return;
        }
        eprintln!("{message}");
        std::process::exit(1);
    }

    let (actual_unit, existing_connection) = {
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
        (actual_unit, state.units.get(&actual_unit).cloned())
    };

    let existing_unit = existing_connection.as_ref().and_then(|connection| {
        let guard = connection
            .unit
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        guard.as_ref().map(|unit| {
            (
                unit.filename.clone(),
                unit.access,
                unit.form.clone(),
                unit.action,
                unit.recl,
            )
        })
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
        with_unit(actual_unit, |unit| {
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
        });
        if !iostat.is_null() {
            unsafe {
                *iostat = 0;
            }
        }
        assign_iomsg(iomsg, iomsg_len, "");
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
    // old -> read, new/replace/scratch/unknown -> readwrite.
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

    // Disconnect before waiting for an in-flight operation on this same unit.
    // New operations then fail the registry lookup while unrelated units remain free.
    let displaced = io_state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .units
        .remove(&actual_unit);
    if let Some(mut existing) = displaced.and_then(disconnect_unit) {
        let _ = existing.flush();
        // Drop closes the file handle.
    }

    let open_path = path_from_filename(&fname);
    match opts.open(&open_path) {
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
            let mut opened_unit = Unit {
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
                read_tokens: VecDeque::new(),
                formatted_read_record: None,
                formatted_read_cursor: 0,
                terminal_nonadvancing_open_record: false,
                last_list_output_char: false,
                list_write_active: false,
                list_write_depth: 0,
                list_write_error: None,
                scratch: is_scratch,
                leading_zero: leading_zero_mode,
                pending_record: None,
                pending_read: None,
                list_read_depth: 0,
            };

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
                match &mut opened_unit.stream {
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
            let replaced = io_state()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .units
                .insert(actual_unit, UnitConnection::new(opened_unit));
            if let Some(mut replaced) = replaced.and_then(disconnect_unit) {
                let _ = replaced.flush();
            }

            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
            assign_iomsg(iomsg, iomsg_len, "");
        }
        Err(e) => {
            let message = format!("OPEN: {}: {}", display_filename(&fname), e);
            if !iostat.is_null() {
                unsafe {
                    *iostat = e.raw_os_error().unwrap_or(1);
                }
                assign_iomsg(iomsg, iomsg_len, &message);
            } else {
                eprintln!("{message}");
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

    let connection = io_state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .units
        .remove(&unit);
    if let Some(mut u) = connection.and_then(disconnect_unit) {
        let _ = u.flush();
        let filename = u.filename.clone();
        // STATUS='SCRATCH' units always delete on close (F2018 §12.5.6.13).
        let delete = delete_on_close || u.scratch;
        drop(u);

        let mut close_status = 0;
        if delete
            && !matches!(filename.as_slice(), b"stdin" | b"stdout" | b"stderr")
            && !filename.is_empty()
        {
            if let Err(e) = std::fs::remove_file(path_from_filename(&filename)) {
                close_status = e.raw_os_error().unwrap_or(1);
            }
        }

        if !iostat.is_null() {
            unsafe { *iostat = close_status };
        } else if close_status != 0 {
            eprintln!(
                "CLOSE: {}: {}",
                display_filename(&filename),
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

const LIST_INT8_WIDTH: usize = 5;
const LIST_INT16_WIDTH: usize = 7;
const LIST_INT32_WIDTH: usize = 12;
const LIST_INT64_WIDTH: usize = 21;
const LIST_INT128_WIDTH: usize = 41;

fn list_directed_integer_field<T: std::fmt::Display>(val: T, width: usize) -> String {
    format!("{:>width$}", val, width = width)
}

fn mark_list_output_nonchar(u: &mut Unit) {
    u.last_list_output_char = false;
}

/// Write an 8-bit integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int8(unit: i32, val: i8) {
    with_unit(unit, |u| {
        if u.is_unformatted() {
            u.list_write_raw_or_buffer(&val.to_ne_bytes());
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(&list_directed_integer_field(val, LIST_INT8_WIDTH));
        }
    });
}

/// Write a 16-bit integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int16(unit: i32, val: i16) {
    with_unit(unit, |u| {
        if u.is_unformatted() {
            u.list_write_raw_or_buffer(&val.to_ne_bytes());
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(&list_directed_integer_field(val, LIST_INT16_WIDTH));
        }
    });
}

/// Write an integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int(unit: i32, val: i32) {
    with_unit(unit, |u| {
        if u.is_unformatted() {
            u.list_write_raw_or_buffer(&val.to_ne_bytes());
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(&list_directed_integer_field(val, LIST_INT32_WIDTH));
        }
    });
}

/// Write a 64-bit integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int64(unit: i32, val: i64) {
    with_unit(unit, |u| {
        if u.is_unformatted() {
            u.list_write_raw_or_buffer(&val.to_ne_bytes());
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(&list_directed_integer_field(val, LIST_INT64_WIDTH));
        }
    });
}

/// Write a 128-bit integer value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_int128(unit: i32, val: i128) {
    with_unit(unit, |u| {
        if u.is_unformatted() {
            u.list_write_raw_or_buffer(&val.to_ne_bytes());
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(&list_directed_integer_field(val, LIST_INT128_WIDTH));
        }
    });
}

/// Write a real value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_real(unit: i32, val: f32) {
    with_unit(unit, |u| {
        if u.is_unformatted() {
            u.list_write_raw_or_buffer(&val.to_ne_bytes());
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(&format!("  {:14.7E}", val));
        }
    });
}

/// Write a double value (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_real64(unit: i32, val: f64) {
    with_unit(unit, |u| {
        if u.is_unformatted() {
            u.list_write_raw_or_buffer(&val.to_ne_bytes());
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(&format!("  {:22.15E}", val));
        }
    });
}

/// Write a complex(4) value (list-directed): " (re,im)".
/// `ptr` points to a two-element f32 array [real, imag].
#[no_mangle]
pub extern "C" fn afs_write_complex_f32(unit: i32, ptr: *const f32) {
    let (re, im) = unsafe { (*ptr, *ptr.add(1)) };
    with_unit(unit, |u| {
        if u.is_unformatted() {
            u.list_write_raw_or_buffer(&re.to_ne_bytes());
            u.list_write_raw_or_buffer(&im.to_ne_bytes());
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(&format!(" ({:14.7E},{:14.7E})", re, im));
        }
    });
}

/// Write a complex(8) value (list-directed): " (re,im)".
/// `ptr` points to a two-element f64 array [real, imag].
#[no_mangle]
pub extern "C" fn afs_write_complex_f64(unit: i32, ptr: *const f64) {
    let (re, im) = unsafe { (*ptr, *ptr.add(1)) };
    with_unit(unit, |u| {
        if u.is_unformatted() {
            u.list_write_raw_or_buffer(&re.to_ne_bytes());
            u.list_write_raw_or_buffer(&im.to_ne_bytes());
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(&format!(" ({:22.15E},{:22.15E})", re, im));
        }
    });
}

/// Write a character string (list-directed).
#[no_mangle]
pub extern "C" fn afs_write_string(unit: i32, ptr: *const u8, len: i64) {
    with_unit(unit, |u| {
        if u.is_unformatted() {
            if !ptr.is_null() && len > 0 {
                let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                u.list_write_raw_or_buffer(slice);
            }
        } else {
            if !u.last_list_output_char {
                u.list_write_str(" ");
            }
            if !ptr.is_null() && len > 0 {
                let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                u.list_write_bytes(slice);
            }
            u.last_list_output_char = true;
        }
    });
}

fn logical_transfer_width(kind_bytes: i32) -> Option<usize> {
    match kind_bytes {
        1 | 2 | 4 | 8 | 16 => Some(kind_bytes as usize),
        _ => None,
    }
}

fn write_unformatted_logical(u: &mut Unit, val: i32, width: usize) {
    let truth = val != 0;
    match width {
        1 => u.list_write_raw_or_buffer(&(truth as i8).to_ne_bytes()),
        2 => u.list_write_raw_or_buffer(&(truth as i16).to_ne_bytes()),
        4 => u.list_write_raw_or_buffer(&(truth as i32).to_ne_bytes()),
        8 => u.list_write_raw_or_buffer(&(truth as i64).to_ne_bytes()),
        16 => u.list_write_raw_or_buffer(&(truth as i128).to_ne_bytes()),
        _ => unreachable!("validated logical transfer width"),
    }
}

/// Write a logical value using its declared storage width.
#[no_mangle]
pub extern "C" fn afs_write_logical_kind(unit: i32, val: i32, kind_bytes: i32) {
    let Some(width) = logical_transfer_width(kind_bytes) else {
        eprintln!("WRITE: invalid logical kind width {kind_bytes}");
        std::process::exit(1);
    };
    with_unit(unit, |u| {
        if u.is_unformatted() {
            write_unformatted_logical(u, val, width);
        } else {
            mark_list_output_nonchar(u);
            u.list_write_str(if val != 0 { " T" } else { " F" });
        }
    });
}

/// Write a default-kind logical value (list-directed or unformatted).
#[no_mangle]
pub extern "C" fn afs_write_logical(unit: i32, val: i32) {
    afs_write_logical_kind(unit, val, 4);
}

/// End a write statement (newline).
#[no_mangle]
pub extern "C" fn afs_write_newline(unit: i32) {
    with_unit(unit, |u| {
        if u.is_unformatted() {
            // Sequential unformatted: a pending record buffer means
            // afs_list_write_end was not called — flush nothing here.
            // Stream unformatted has no record terminator.
            u.list_write_flush();
            return;
        }
        u.list_write_str("\n");
        u.list_write_flush();
        u.last_list_output_char = false;
    });
}

/// Like `afs_write_newline` but no-ops when `advance == 0`. The
/// lowering uses this when `advance=` is a runtime-evaluated string
/// (e.g. `advance=optval(adv, 'YES')`) — `advance` is precomputed by
/// `afs_advance_eval` to 0 (no advance) or 1 (advance).
#[no_mangle]
pub extern "C" fn afs_write_newline_if(unit: i32, advance: i32) {
    if advance == 0 {
        with_unit(unit, |u| {
            u.list_write_flush();
        });
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
/// the per-item helpers will append into. A nested child transfer on the
/// same unit shares the parent's buffer. Stream-unformatted units skip
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
    let err = with_unit(unit, |u| {
        if u.list_write_depth > 0 {
            let Some(depth) = u.list_write_depth.checked_add(1) else {
                let message = "defined I/O child write nesting depth overflow".to_string();
                if u.list_write_error.is_none() {
                    u.list_write_error = Some(message.clone());
                }
                return Some(message);
            };
            u.list_write_depth = depth;
            return None;
        }

        u.last_list_output_char = false;
        u.list_write_active = true;
        u.list_write_depth = 1;
        u.list_write_error = None;
        if u.form == Form::Unformatted && u.access == Access::Sequential {
            u.pending_record = Some(Vec::new());
        }
        None
    })
    .unwrap_or_else(|| Some("unit not connected".to_string()));

    if let Some(message) = err {
        write_i32_ptr(iostat, 1);
        assign_iomsg(iomsg, iomsg_len, &message);
    }
}

/// Report an error accumulated by the current WRITE without closing its
/// record. Mixed intrinsic/defined transfer lists use this between items so
/// an intrinsic runtime failure prevents later defined-I/O calls.
#[no_mangle]
pub extern "C" fn afs_list_write_check(
    unit: i32,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    let err = with_unit(unit, |u| u.list_write_error.clone())
        .unwrap_or_else(|| Some("unit not connected".to_string()));
    if let Some(message) = err {
        write_i32_ptr(iostat, 1);
        assign_iomsg(iomsg, iomsg_len, &message);
    }
}

/// End a list-directed write statement. For sequential unformatted
/// units this drains the per-statement record buffer and writes
/// `[len][bytes][len]` to the stream. For formatted units the trailing
/// newline is left to the per-item path's `afs_write_newline` so we
/// don't double-newline; this only flushes and forwards iostat/iomsg.
/// A nested child transfer only releases its depth and reports the
/// current statement error; the outermost transfer owns record drain.
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
    let err = with_unit(unit, |u| {
        if u.list_write_depth > 1 {
            u.list_write_depth -= 1;
            return u.list_write_error.clone();
        }
        u.list_write_depth = 0;

        if let Some(buf) = u.pending_record.take() {
            let len_bytes = (buf.len() as u32).to_ne_bytes();
            u.list_write_raw_or_buffer(&len_bytes);
            if !buf.is_empty() {
                u.list_write_raw_or_buffer(&buf);
            }
            u.list_write_raw_or_buffer(&len_bytes);
        }
        u.list_write_flush();
        let err = u.list_write_error.take();
        u.list_write_active = false;
        err
    })
    .unwrap_or_else(|| Some("unit not connected".to_string()));

    if let Some(msg) = err {
        write_i32_ptr(iostat, 1);
        assign_iomsg(iomsg, iomsg_len, &msg);
        if iostat.is_null() {
            eprintln!("Fortran runtime error: {msg}");
            std::process::exit(2);
        }
    }
}

// ---- Public C API: List-directed READ ----

fn set_read_success(iostat: *mut i32) {
    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
}

fn parse_logical_token(token: &str) -> Option<bool> {
    let token = token.trim();
    let token = token
        .strip_prefix('.')
        .and_then(|value| value.strip_suffix('.'))
        .unwrap_or(token);

    if token.eq_ignore_ascii_case("t") || token.eq_ignore_ascii_case("true") {
        Some(true)
    } else if token.eq_ignore_ascii_case("f") || token.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn store_logical_token(token: &str, val: *mut i32, iostat: *mut i32) -> bool {
    let Some(value) = parse_logical_token(token) else {
        return false;
    };
    write_i32_ptr(val, i32::from(value));
    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
    true
}

fn store_formatted_logical_result(
    result: Result<(FormatDesc, Vec<u8>), i32>,
    val: *mut i32,
    iostat: *mut i32,
) {
    match result {
        Ok((FormatDesc::Logical { .. }, field)) => {
            let field_text = String::from_utf8_lossy(&field);
            if !store_logical_token(&field_text, val, iostat) {
                set_read_status_or_exit(iostat, 1);
            }
        }
        Ok(_) => set_read_status_or_exit(iostat, 1),
        Err(code) => set_read_status_or_exit(iostat, code),
    }
}

/// Read a logical value using its declared storage width.
#[no_mangle]
pub extern "C" fn afs_read_logical_kind(
    unit: i32,
    val: *mut i32,
    iostat: *mut i32,
    kind_bytes: i32,
) {
    let Some(width) = logical_transfer_width(kind_bytes) else {
        set_read_iostat_or_exit(iostat, 1, "invalid logical kind width");
        return;
    };
    with_unit(unit, |u| {
        if let Some(bytes) = u.read_buffer_take(width) {
            write_i32_ptr(val, i32::from(bytes.iter().any(|byte| *byte != 0)));
            set_read_success(iostat);
            return;
        }
        if report_short_pending_read_record(u, iostat) {
            return;
        }
        if u.form == Form::Unformatted && u.access == Access::Stream {
            let mut raw = vec![0u8; width];
            if read_stream_unformatted_exact(u, &mut raw, iostat) == Some(true) {
                write_i32_ptr(val, i32::from(raw.iter().any(|byte| *byte != 0)));
            }
            return;
        }
        match u.next_read_token() {
            Ok(Some(ListReadToken::Value(token))) if store_logical_token(&token, val, iostat) => {}
            Ok(Some(ListReadToken::Value(token))) => {
                set_read_iostat_or_exit(
                    iostat,
                    1,
                    &format!("cannot parse logical from '{}'", token),
                );
            }
            Ok(Some(ListReadToken::Null)) => set_read_success(iostat),
            Ok(None) => {
                set_read_iostat_or_exit(iostat, IOSTAT_END, "end of file");
            }
            Err(e) => {
                set_read_iostat_or_exit(iostat, 1, &e.to_string());
            }
        }
    });
}

/// Read a default-kind logical value (list-directed or unformatted).
#[no_mangle]
pub extern "C" fn afs_read_logical(unit: i32, val: *mut i32, iostat: *mut i32) {
    afs_read_logical_kind(unit, val, iostat, 4);
}

/// Read an i8 value (list-directed) from a unit.
#[no_mangle]
pub extern "C" fn afs_read_int8(unit: i32, val: *mut i8, iostat: *mut i32) {
    with_unit(unit, |u| {
        if let Some(bytes) = u.read_buffer_take(1) {
            write_i8_ptr(val, i8::from_ne_bytes([bytes[0]]));
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
            return;
        }
        if report_short_pending_read_record(u, iostat) {
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
            Ok(Some(ListReadToken::Value(token))) => match token.parse::<i8>() {
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
            Ok(Some(ListReadToken::Null)) => set_read_success(iostat),
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
    });
}

/// Read an i16 value (list-directed) from a unit.
#[no_mangle]
pub extern "C" fn afs_read_int16(unit: i32, val: *mut i16, iostat: *mut i32) {
    with_unit(unit, |u| {
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
        if report_short_pending_read_record(u, iostat) {
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
            Ok(Some(ListReadToken::Value(token))) => match token.parse::<i16>() {
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
            Ok(Some(ListReadToken::Null)) => set_read_success(iostat),
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
    });
}

/// Read an i32 value (list-directed) from a unit.
/// Uses token buffer: multiple values on one line are consumed left-to-right.
#[no_mangle]
pub extern "C" fn afs_read_int(unit: i32, val: *mut i32, iostat: *mut i32) {
    with_unit(unit, |u| {
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
        if report_short_pending_read_record(u, iostat) {
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
            Ok(Some(ListReadToken::Value(token))) => match token.parse::<i32>() {
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
            Ok(Some(ListReadToken::Null)) => set_read_success(iostat),
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
    });
}

/// Read an i64 value (list-directed).
#[no_mangle]
pub extern "C" fn afs_read_int64(unit: i32, val: *mut i64, iostat: *mut i32) {
    with_unit(unit, |u| {
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
        if report_short_pending_read_record(u, iostat) {
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
            Ok(Some(ListReadToken::Value(token))) => match token.parse::<i64>() {
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
            Ok(Some(ListReadToken::Null)) => set_read_success(iostat),
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
    });
}

/// Read an i128 value (list-directed).
#[no_mangle]
pub extern "C" fn afs_read_int128(unit: i32, val: *mut i128, iostat: *mut i32) {
    with_unit(unit, |u| {
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
        if report_short_pending_read_record(u, iostat) {
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
            Ok(Some(ListReadToken::Value(token))) => match token.parse::<i128>() {
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
            Ok(Some(ListReadToken::Null)) => set_read_success(iostat),
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
    });
}

/// Read an f32 value (list-directed).
#[no_mangle]
pub extern "C" fn afs_read_real(unit: i32, val: *mut f32, iostat: *mut i32) {
    with_unit(unit, |u| {
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
        if report_short_pending_read_record(u, iostat) {
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
            Ok(Some(ListReadToken::Value(token))) => {
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
            Ok(Some(ListReadToken::Null)) => set_read_success(iostat),
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
    });
}

/// Read an f64 value (list-directed).
#[no_mangle]
pub extern "C" fn afs_read_real64(unit: i32, val: *mut f64, iostat: *mut i32) {
    with_unit(unit, |u| {
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
        if report_short_pending_read_record(u, iostat) {
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
            Ok(Some(ListReadToken::Value(token))) => {
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
            Ok(Some(ListReadToken::Null)) => set_read_success(iostat),
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
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExactRawRead {
    Complete,
    EndOfFile,
    Truncated,
}

fn read_raw_exact(u: &mut Unit, buf: &mut [u8]) -> io::Result<ExactRawRead> {
    let mut offset = 0usize;
    while offset < buf.len() {
        match u.read_raw(&mut buf[offset..]) {
            Ok(0) if offset == 0 => return Ok(ExactRawRead::EndOfFile),
            Ok(0) => return Ok(ExactRawRead::Truncated),
            Ok(n) => offset += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(ExactRawRead::Complete)
}

/// Begin a list-directed READ statement. Mandatory before per-item
/// helpers when iostat=/iomsg= are requested or when the unit may be
/// sequential-unformatted (which needs the leading record marker
/// consumed and the data slurped into a buffer for typed take-bytes).
///
/// For formatted units this only resets iostat. For sequential
/// unformatted units it reads `[u32 len][len bytes][u32 trailer]`,
/// stashes the data in `pending_read`, and the per-item helpers will
/// consume from there. A nested child transfer on the same unit shares
/// the parent's buffer and cursor. Stream-unformatted reads continue
/// using their existing per-helper raw-byte path.
#[no_mangle]
pub extern "C" fn afs_list_read_begin(unit: i32, iostat: *mut i32, iomsg: *mut u8, iomsg_len: i64) {
    if !iostat.is_null() && unsafe { *iostat != 0 } {
        return;
    }
    if !iomsg.is_null() && iomsg_len > 0 {
        let buf = unsafe { std::slice::from_raw_parts_mut(iomsg, iomsg_len as usize) };
        for b in buf.iter_mut() {
            *b = b' ';
        }
    }
    if with_unit(unit, |u| {
        if !(u.form == Form::Unformatted && u.access == Access::Sequential) {
            return;
        }
        if u.list_read_depth > 0 {
            let Some(depth) = u.list_read_depth.checked_add(1) else {
                set_read_status_or_exit(iostat, 1);
                return;
            };
            u.list_read_depth = depth;
            return;
        }

        u.pending_read = None;
        u.list_read_depth = 1;

        let mut len_buf = [0u8; 4];
        match read_raw_exact(u, &mut len_buf) {
            Ok(ExactRawRead::Complete) => {}
            Ok(ExactRawRead::EndOfFile) => {
                u.list_read_depth = 0;
                set_read_status_or_exit(iostat, IOSTAT_END);
                return;
            }
            _ => {
                u.list_read_depth = 0;
                set_read_status_or_exit(iostat, 1);
                return;
            }
        }
        let record_len = u32::from_ne_bytes(len_buf) as usize;
        let mut data = Vec::new();
        if data.try_reserve_exact(record_len).is_err() {
            u.list_read_depth = 0;
            set_read_status_or_exit(iostat, 1);
            return;
        }
        data.resize(record_len, 0);
        if !matches!(read_raw_exact(u, &mut data), Ok(ExactRawRead::Complete)) {
            u.list_read_depth = 0;
            set_read_status_or_exit(iostat, 1);
            return;
        }

        let mut trailer = [0u8; 4];
        if !matches!(read_raw_exact(u, &mut trailer), Ok(ExactRawRead::Complete))
            || u32::from_ne_bytes(trailer) as usize != record_len
        {
            u.list_read_depth = 0;
            set_read_status_or_exit(iostat, 1);
            return;
        }
        u.pending_read = Some((data, 0));
    })
    .is_none()
    {
        write_i32_ptr(iostat, 1);
    }
}

/// End a list-directed READ statement. The outermost sequential
/// unformatted transfer drops any unread bytes left in the in-flight
/// record buffer (the standard does not require the program to consume
/// the entire record). Nested child transfers only release their depth.
#[no_mangle]
pub extern "C" fn afs_list_read_end(
    unit: i32,
    _iostat: *mut i32,
    _iomsg: *mut u8,
    _iomsg_len: i64,
) {
    with_unit(unit, |u| {
        if !(u.form == Form::Unformatted && u.access == Access::Sequential) {
            return;
        }
        if u.list_read_depth > 1 {
            u.list_read_depth -= 1;
            return;
        }
        u.list_read_depth = 0;
        u.pending_read = None;
    });
}

/// Advance the file position past one record on a list-directed READ
/// statement that has no input items: `read(unit, *)` (no items) is
/// defined by F2018 §12.6.4.5 to position the unit at the next record.
/// stdlib's `number_of_rows(s)` counts rows by repeating exactly that
/// statement until a nonzero iostat — without this helper the loop is
/// infinite because the unit never advances and iostat is never set.
#[no_mangle]
pub extern "C" fn afs_read_skip_record(unit: i32, iostat: *mut i32) {
    if with_unit(unit, |u| {
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
    })
    .is_none()
    {
        write_i32_ptr(iostat, 1);
    }
}

#[no_mangle]
pub extern "C" fn afs_read_string(unit: i32, dest: *mut u8, dest_len: i64, iostat: *mut i32) {
    if with_unit(unit, |u| {
        if dest_len < 0 {
            set_read_status_or_exit(iostat, 1);
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

        if report_short_pending_read_record(u, iostat) {
            return;
        }

        if u.form == Form::Unformatted && u.access == Access::Stream {
            let mut bytes = vec![b' '; dest_len as usize];
            match u.read_raw(&mut bytes) {
                Ok(0) => {
                    crate::string::afs_assign_char_fixed(dest, dest_len, std::ptr::null(), 0);
                    set_read_status_or_exit(iostat, IOSTAT_END);
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
                    set_read_status_or_exit(iostat, 1);
                }
            }
            return;
        }

        match u.next_read_token() {
            Ok(Some(ListReadToken::Value(token))) => {
                let Ok(value) = decode_list_directed_character_value(&token) else {
                    set_read_status_or_exit(iostat, 1);
                    return;
                };
                crate::string::afs_assign_char_fixed(
                    dest,
                    dest_len,
                    value.as_ptr(),
                    value.len() as i64,
                );
                if !iostat.is_null() {
                    unsafe {
                        *iostat = 0;
                    }
                }
            }
            Ok(Some(ListReadToken::Null)) => set_read_success(iostat),
            Ok(None) => {
                crate::string::afs_assign_char_fixed(dest, dest_len, std::ptr::null(), 0);
                set_read_status_or_exit(iostat, IOSTAT_END);
            }
            Err(_) => {
                crate::string::afs_assign_char_fixed(dest, dest_len, std::ptr::null(), 0);
                set_read_status_or_exit(iostat, 1);
            }
        }
    })
    .is_none()
    {
        set_read_status_or_exit(iostat, 1);
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

fn fortran_file_name(ptr: *const u8, len: i64) -> Vec<u8> {
    if ptr.is_null() || len <= 0 {
        return Vec::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let end = slice
        .iter()
        .rposition(|&b| b != b' ')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    slice[..end].to_vec()
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

fn read_status_message(status: i32) -> &'static str {
    match status {
        IOSTAT_END => "end of file",
        IOSTAT_EOR => "end of record",
        _ => "input/output error",
    }
}

#[no_mangle]
pub extern "C" fn afs_read_assign_iomsg(status: i32, iomsg: *mut u8, iomsg_len: i64) {
    let message = if status == 0 {
        ""
    } else {
        read_status_message(status)
    };
    assign_iomsg(iomsg, iomsg_len, message);
}

fn set_read_status_or_exit(iostat: *mut i32, status: i32) {
    set_read_iostat_or_exit(iostat, status, read_status_message(status));
}

fn report_short_pending_read_record(u: &Unit, iostat: *mut i32) -> bool {
    if u.pending_read.is_none() {
        return false;
    }
    set_read_iostat_or_exit(iostat, 1, "unexpected end of unformatted record");
    true
}

#[no_mangle]
pub extern "C" fn afs_read_unhandled_iostat(status: i32) {
    eprintln!("READ: {}", read_status_message(status));
    std::process::exit(1);
}

#[no_mangle]
pub extern "C" fn afs_write_unhandled_iostat(status: i32, iomsg: *const u8, iomsg_len: i64) {
    let message = unsafe_str(iomsg, iomsg_len);
    let message = message.trim();
    if message.is_empty() {
        eprintln!("Fortran runtime error: WRITE failed with IOSTAT={status}");
    } else {
        eprintln!("Fortran runtime error: {message}");
    }
    std::process::exit(2);
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
    with_unit(unit, |u| {
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
    });
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
    with_unit(unit, |u| {
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
    });
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
    with_unit(unit, |u| {
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
    });
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
    with_unit(unit, |u| {
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
    });
}

// ---- Stream access helpers ----

/// Write raw bytes at the current stream position.
#[no_mangle]
pub extern "C" fn afs_write_stream(unit: i32, data: *const u8, data_len: i64, iostat: *mut i32) {
    with_unit(unit, |u| {
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
    });
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
    if with_unit(unit, |u| {
        if u.access != Access::Stream {
            if !iostat.is_null() {
                unsafe {
                    *iostat = 1;
                }
            }
            return;
        }
        match &mut u.stream {
            UnitStream::FileRaw(f) => match f.seek(SeekFrom::Start(offset)) {
                Ok(_) => {
                    u.reset_read_state_after_positioning();
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
    })
    .is_none()
        && !iostat.is_null()
    {
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

fn remember_io_status(
    status: &mut i32,
    error_message: &mut Option<String>,
    result: io::Result<()>,
) {
    if *status != 0 {
        return;
    }
    if let Err(e) = result {
        *status = e.raw_os_error().unwrap_or(1);
        *error_message = Some(e.to_string());
    }
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
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    let gname = unsafe_str(group_name, group_name_len);
    let mut status = 0;
    let mut error_message = None;
    if with_unit(unit, |u| {
        remember_io_status(
            &mut status,
            &mut error_message,
            u.write_str(&format!(" &{}", gname.to_uppercase())),
        );

        if !entries.is_null() && n_entries > 0 {
            let slice = unsafe { std::slice::from_raw_parts(entries, n_entries as usize) };
            for (i, entry) in slice.iter().enumerate() {
                let name = unsafe_str(entry.name, entry.name_len);
                let sep = if i > 0 { "," } else { "" };
                let val_str = match entry.data_type {
                    0 => {
                        // integer
                        let elem_count = entry.elem_count.max(1) as usize;
                        let mut values = Vec::with_capacity(elem_count);
                        for elem in 0..elem_count {
                            let ptr = unsafe { entry.data.add(elem * std::mem::size_of::<i32>()) };
                            let v = unsafe { *(ptr as *const i32) };
                            values.push(format!("{}", v));
                        }
                        values.join(",")
                    }
                    1 => {
                        // real
                        let elem_count = entry.elem_count.max(1) as usize;
                        let mut values = Vec::with_capacity(elem_count);
                        for elem in 0..elem_count {
                            let ptr = unsafe { entry.data.add(elem * std::mem::size_of::<f64>()) };
                            let v = unsafe { *(ptr as *const f64) };
                            values.push(format!("{}", v));
                        }
                        values.join(",")
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
                        let elem_count = entry.elem_count.max(1) as usize;
                        let mut values = Vec::with_capacity(elem_count);
                        for elem in 0..elem_count {
                            let ptr = unsafe { entry.data.add(elem * std::mem::size_of::<i32>()) };
                            let v = unsafe { *(ptr as *const i32) };
                            values.push((if v != 0 { ".TRUE." } else { ".FALSE." }).to_string());
                        }
                        values.join(",")
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
                        let elem_count = entry.elem_count.max(1) as usize;
                        let mut values = Vec::with_capacity(elem_count);
                        for elem in 0..elem_count {
                            let v = unsafe { *entry.data.add(elem) } != 0;
                            values.push((if v { ".TRUE." } else { ".FALSE." }).to_string());
                        }
                        values.join(",")
                    }
                    _ => "???".to_string(),
                };
                remember_io_status(
                    &mut status,
                    &mut error_message,
                    u.write_str(&format!("{} {}={}", sep, name.to_uppercase(), val_str)),
                );
            }
        }
        remember_io_status(&mut status, &mut error_message, u.write_str(" /\n"));
        remember_io_status(&mut status, &mut error_message, u.flush());
    })
    .is_none()
    {
        status = 1;
        error_message = Some("unit not open for writing".to_string());
    }
    if !iostat.is_null() {
        unsafe {
            *iostat = status;
        }
    }
    assign_iomsg(
        iomsg,
        iomsg_len,
        error_message.as_deref().unwrap_or_default(),
    );
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
    let status = with_unit(unit, |u| {
        // Read lines until we find &groupname.
        let mut all_lines = String::new();
        'find_group: loop {
            match u.read_line() {
                Ok(line) if line.is_empty() => break 'find_group IOSTAT_END,
                Ok(line) => {
                    let trimmed = line.trim().to_lowercase();
                    if trimmed.starts_with('&') && trimmed[1..].starts_with(&gname) {
                        all_lines.push_str(&line);
                        // Keep reading until we find a group terminator outside
                        // a character literal.
                        let mut terminal_status = 0;
                        while find_unquoted_namelist_terminator(&all_lines).is_none() {
                            match u.read_line() {
                                Ok(cont) if cont.is_empty() => {
                                    terminal_status = IOSTAT_END;
                                    break;
                                }
                                Ok(cont) => all_lines.push_str(&cont),
                                Err(error) => {
                                    terminal_status = error.raw_os_error().unwrap_or(1);
                                    break;
                                }
                            }
                        }
                        let assignment_status =
                            match namelist_assign_from_text(&all_lines, &gname, entries, n_entries)
                            {
                                Ok(true) => 0,
                                Ok(false) => IOSTAT_END,
                                Err(_) => 1,
                            };
                        break 'find_group if terminal_status == 0 {
                            assignment_status
                        } else {
                            terminal_status
                        };
                    }
                }
                Err(error) => break 'find_group error.raw_os_error().unwrap_or(1),
            }
        }
    })
    .unwrap_or(1);
    if status == 0 {
        write_i32_ptr(iostat, 0);
    } else {
        set_read_status_or_exit(iostat, status);
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
    match namelist_assign_from_text(&text, &gname, entries, n_entries) {
        Ok(true) => write_i32_ptr(iostat, 0),
        Ok(false) => set_read_status_or_exit(iostat, IOSTAT_END),
        Err(_) => set_read_status_or_exit(iostat, 1),
    }
}

fn find_unquoted_namelist_terminator(text: &str) -> Option<usize> {
    let mut quote = None;
    let mut chars = text.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if let Some(delimiter) = quote {
            if ch == delimiter {
                if chars.peek().is_some_and(|(_, next)| *next == delimiter) {
                    let _ = chars.next();
                } else {
                    quote = None;
                }
            }
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch == '/' {
            return Some(idx);
        }
    }
    None
}

fn namelist_content<'a>(text: &'a str, group_name: &str) -> Option<&'a str> {
    let lower = text.to_lowercase();
    let marker = format!("&{}", group_name.to_lowercase());
    let start = lower.find(&marker)?;
    let after_start = start + marker.len();
    let after_name = &text[after_start..];
    if let Some(end) = find_unquoted_namelist_terminator(after_name) {
        Some(&after_name[..end])
    } else {
        Some(after_name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamelistAssignError {
    InvalidValue,
    UnsupportedEntryType,
}

fn namelist_assign_from_text(
    text: &str,
    group_name: &str,
    entries: *const NamelistEntry,
    n_entries: i32,
) -> Result<bool, NamelistAssignError> {
    let Some(content) = namelist_content(text, group_name) else {
        return Ok(false);
    };
    if entries.is_null() || n_entries <= 0 {
        return Ok(true);
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
    ) -> Result<usize, NamelistAssignError> {
        let mut next = start_component;
        for _ in 0..repeat.max(1) {
            let Some(entry_index) = entry_indices.get(next).copied() else {
                break;
            };
            if let Some(entry) = entries.get(entry_index) {
                namelist_assign_value(entry, val_str, None, 1)?;
            }
            next += 1;
        }
        Ok(next)
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
                    namelist_assign_value(entry, actual_val, array_index, repeat_count)?;
                    let next_index = array_index.unwrap_or(1).saturating_add(repeat_count);
                    if next_index <= entry.elem_count.max(1) as usize {
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
                    )?;
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
                        namelist_assign_value(entry, actual_val, Some(next_index), repeat_count)?;
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
                    )?;
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
    Ok(true)
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
        } else if ch == ',' || ch == '\n' || ch == '\r' {
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
) -> Result<(), NamelistAssignError> {
    // For array elements, compute byte offset from 1-based index.
    let elem_size = match entry.data_type {
        0 => 4,                              // integer (i32)
        1 => 8,                              // real (f64)
        2 => entry.data_len.max(1) as usize, // fixed string element
        3 => 4,                              // logical (i32)
        5 => 1,                              // logical (bool/i8)
        _ => 1,                              // string
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
                let value = val_str
                    .parse::<i32>()
                    .map_err(|_| NamelistAssignError::InvalidValue)?;
                unsafe {
                    *(ptr as *mut i32) = value;
                }
            }
            1 => {
                // real
                let normalized = normalize_fortran_real_input(val_str, false);
                let value = normalized
                    .parse::<f64>()
                    .map_err(|_| NamelistAssignError::InvalidValue)?;
                unsafe {
                    *(ptr as *mut f64) = value;
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
                            std::ptr::write_bytes(ptr.add(copy_len), b' ', slot_len - copy_len);
                        }
                    }
                }
            }
            3 => {
                // logical
                let value =
                    parse_logical_token(val_str).ok_or(NamelistAssignError::InvalidValue)?;
                unsafe {
                    *(ptr as *mut i32) = value as i32;
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
                return Ok(());
            }
            5 => {
                // bool-backed logical
                let value =
                    parse_logical_token(val_str).ok_or(NamelistAssignError::InvalidValue)?;
                unsafe {
                    *ptr = value as u8;
                }
            }
            _ => return Err(NamelistAssignError::UnsupportedEntryType),
        }
    }
    Ok(())
}

// ---- Internal I/O (read/write to character variables) ----

struct FixedInternalListContext {
    buf: *mut u8,
    buf_len: usize,
    original: Vec<u8>,
    unavailable: bool,
    overflowed: bool,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
}

unsafe impl Send for FixedInternalListContext {}

thread_local! {
    static FIXED_INTERNAL_LIST_CTX: RefCell<Vec<FixedInternalListContext>> =
        const { RefCell::new(Vec::new()) };
}

#[no_mangle]
pub extern "C" fn afs_lst_begin_internal_fixed(
    buf: *mut u8,
    buf_len: i64,
    record_count: i64,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    let buf_len = buf_len.max(0) as usize;
    let unavailable = buf.is_null() || record_count <= 0;
    let original = if unavailable || buf_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(buf, buf_len) }.to_vec()
    };
    FIXED_INTERNAL_LIST_CTX.with(|ctx| {
        ctx.borrow_mut().push(FixedInternalListContext {
            buf,
            buf_len,
            original,
            unavailable,
            overflowed: false,
            iostat,
            iomsg,
            iomsg_len,
        });
    });
}

#[no_mangle]
pub extern "C" fn afs_lst_end_internal_fixed() {
    FIXED_INTERNAL_LIST_CTX.with(|ctx| {
        let Some(context) = ctx.borrow_mut().pop() else {
            return;
        };

        if context.overflowed && !context.unavailable && !context.original.is_empty() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    context.original.as_ptr(),
                    context.buf,
                    context.original.len(),
                );
            }
        }

        let (status, message) = if context.unavailable {
            (
                1,
                "internal WRITE to an unallocated or zero-size character array",
            )
        } else if context.overflowed {
            (IOSTAT_EOR, "end of record")
        } else {
            (0, "")
        };
        assign_iomsg(context.iomsg, context.iomsg_len, message);
        if !context.iostat.is_null() {
            unsafe {
                *context.iostat = status;
            }
        } else if context.unavailable || context.overflowed {
            if context.unavailable {
                eprintln!("ERROR: internal WRITE to an unallocated or zero-size character array");
            } else {
                eprintln!("ERROR: list-directed internal WRITE exceeded its record");
            }
            std::process::exit(2);
        }
    });
}

fn fixed_internal_list_transfer_blocked(
    buf: *mut u8,
    buf_len: usize,
    start: usize,
    data_len: usize,
) -> bool {
    FIXED_INTERNAL_LIST_CTX.with(|ctx| {
        if let Some(context) = ctx
            .borrow_mut()
            .iter_mut()
            .rev()
            .find(|context| context.buf == buf && context.buf_len == buf_len)
        {
            if context.unavailable {
                return true;
            }
            if start > buf_len || data_len > buf_len - start {
                context.overflowed = true;
            }
        }
        false
    })
}

fn write_internal_list_to_buffer(
    buf: *mut u8,
    buf_len: usize,
    start: usize,
    data: &[u8],
    pos: *mut i64,
) {
    if fixed_internal_list_transfer_blocked(buf, buf_len, start, data.len()) || buf.is_null() {
        return;
    }
    write_to_buffer(buf, buf_len, start, data, pos);
}

fn write_internal_list_directed_integer<T: std::fmt::Display>(
    buf: *mut u8,
    buf_len: i64,
    val: T,
    width: usize,
    pos: *mut i64,
) {
    let buf_len = buf_len.max(0) as usize;
    let s = list_directed_integer_field(val, width);
    let start = if !pos.is_null() {
        (unsafe { *pos }) as usize
    } else {
        0
    };
    write_internal_list_to_buffer(buf, buf_len, start, s.as_bytes(), pos);
}

/// Write a formatted i8 to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_int8(buf: *mut u8, buf_len: i64, val: i8, pos: *mut i64) {
    write_internal_list_directed_integer(buf, buf_len, val, LIST_INT8_WIDTH, pos);
}

/// Write a formatted i16 to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_int16(buf: *mut u8, buf_len: i64, val: i16, pos: *mut i64) {
    write_internal_list_directed_integer(buf, buf_len, val, LIST_INT16_WIDTH, pos);
}

/// Write a formatted integer to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_int(
    buf: *mut u8,
    buf_len: i64,
    val: i32,
    pos: *mut i64, // current write position, updated after write
) {
    write_internal_list_directed_integer(buf, buf_len, val, LIST_INT32_WIDTH, pos);
}

/// Write a formatted i64 to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_int64(buf: *mut u8, buf_len: i64, val: i64, pos: *mut i64) {
    write_internal_list_directed_integer(buf, buf_len, val, LIST_INT64_WIDTH, pos);
}

/// Write a formatted integer(16) to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_int128(buf: *mut u8, buf_len: i64, val: i128, pos: *mut i64) {
    write_internal_list_directed_integer(buf, buf_len, val, LIST_INT128_WIDTH, pos);
}

/// Write a list-directed logical value to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_logical(buf: *mut u8, buf_len: i64, val: i32, pos: *mut i64) {
    let buf_len = buf_len.max(0) as usize;
    let data = if val != 0 { b" T" } else { b" F" };
    let start = if !pos.is_null() {
        unsafe { *pos as usize }
    } else {
        0
    };
    write_internal_list_to_buffer(buf, buf_len, start, data, pos);
}

/// Write a formatted real to a character buffer (internal I/O).
#[no_mangle]
pub extern "C" fn afs_write_internal_real64(buf: *mut u8, buf_len: i64, val: f64, pos: *mut i64) {
    let buf_len = buf_len.max(0) as usize;
    let s = format!(" {}", val);
    let start = if !pos.is_null() {
        (unsafe { *pos }) as usize
    } else {
        0
    };
    write_internal_list_to_buffer(buf, buf_len, start, s.as_bytes(), pos);
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
    let buf_len = buf_len.max(0) as usize;
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
    write_internal_list_to_buffer(buf, buf_len, start, &data, pos);
}

fn next_internal_token(buf: *const u8, buf_len: i64, pos: *mut i64) -> Option<ListReadToken> {
    if buf.is_null() || buf_len <= 0 {
        return None;
    }

    let slice = unsafe { std::slice::from_raw_parts(buf, buf_len as usize) };
    let cursor = if !pos.is_null() {
        unsafe { (*pos).clamp(0, buf_len) as usize }
    } else {
        0
    };

    match scan_list_directed_token(slice, cursor) {
        Some((token, next_cursor)) => {
            if !pos.is_null() {
                unsafe {
                    *pos = next_cursor as i64;
                }
            }
            Some(token)
        }
        None => {
            if !pos.is_null() {
                unsafe {
                    *pos = slice.len() as i64;
                }
            }
            None
        }
    }
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
    match next_internal_token(buf, buf_len, pos) {
        Some(ListReadToken::Value(token)) => match token.replace(',', "").parse::<i32>() {
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
        },
        Some(ListReadToken::Null) => set_read_success(iostat),
        None => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = -1;
                }
            }
        }
    }
}

/// Read a list-directed logical value from a character buffer.
#[no_mangle]
pub extern "C" fn afs_read_internal_logical(
    buf: *const u8,
    buf_len: i64,
    pos: *mut i64,
    val: *mut i32,
    iostat: *mut i32,
) {
    match next_internal_token(buf, buf_len, pos) {
        Some(ListReadToken::Value(token)) if store_logical_token(&token, val, iostat) => {}
        Some(ListReadToken::Value(token)) => {
            set_read_iostat_or_exit(iostat, 1, &format!("cannot parse logical from '{}'", token));
        }
        Some(ListReadToken::Null) => set_read_success(iostat),
        None => {
            set_read_iostat_or_exit(iostat, IOSTAT_END, "end of record");
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

    match next_internal_token(buf, buf_len, pos) {
        Some(ListReadToken::Value(token)) => {
            let Ok(value) = decode_list_directed_character_value(&token) else {
                set_read_status_or_exit(iostat, 1);
                return;
            };
            dest_slice.fill(b' ');
            let bytes = value.as_bytes();
            let n = bytes.len().min(dest_slice.len());
            dest_slice[..n].copy_from_slice(&bytes[..n]);
            if !iostat.is_null() {
                unsafe {
                    *iostat = 0;
                }
            }
        }
        Some(ListReadToken::Null) => set_read_success(iostat),
        None => {
            dest_slice.fill(b' ');
            if !iostat.is_null() {
                unsafe {
                    *iostat = -1;
                }
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
    match next_internal_token(buf, buf_len, pos) {
        Some(ListReadToken::Value(token)) => match token.replace(',', "").parse::<i64>() {
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
        },
        Some(ListReadToken::Null) => set_read_success(iostat),
        None => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = -1;
                }
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
    match next_internal_token(buf, buf_len, pos) {
        Some(ListReadToken::Value(token)) => match token.replace(',', "").parse::<i128>() {
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
        },
        Some(ListReadToken::Null) => set_read_success(iostat),
        None => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = -1;
                }
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
    match next_internal_token(buf, buf_len, pos) {
        Some(ListReadToken::Value(token)) => {
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
        }
        Some(ListReadToken::Null) => set_read_success(iostat),
        None => {
            if !iostat.is_null() {
                unsafe {
                    *iostat = -1;
                }
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

fn invalid_positioning_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn backspace_formatted_stream<S: Read + Seek>(stream: &mut S) -> io::Result<()> {
    let position = stream.stream_position()?;
    if position == 0 {
        return Ok(());
    }

    let mut search_position = position - 1;
    let mut byte = [0u8; 1];
    stream.seek(SeekFrom::Start(search_position))?;
    stream.read_exact(&mut byte)?;

    if byte[0] == b'\n' {
        if search_position == 0 {
            stream.seek(SeekFrom::Start(0))?;
            return Ok(());
        }
        search_position -= 1;
    }

    loop {
        stream.seek(SeekFrom::Start(search_position))?;
        stream.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            stream.seek(SeekFrom::Start(search_position + 1))?;
            return Ok(());
        }
        if search_position == 0 {
            stream.seek(SeekFrom::Start(0))?;
            return Ok(());
        }
        search_position -= 1;
    }
}

fn backspace_unformatted_stream<S: Read + Seek>(stream: &mut S) -> io::Result<()> {
    let position = stream.stream_position()?;
    if position == 0 {
        return Ok(());
    }
    if position < 8 {
        return Err(invalid_positioning_error(
            "invalid sequential unformatted record",
        ));
    }

    let mut marker = [0u8; 4];
    stream.seek(SeekFrom::Start(position - 4))?;
    stream.read_exact(&mut marker)?;
    let record_len = u32::from_ne_bytes(marker) as u64;
    let record_size = record_len
        .checked_add(8)
        .ok_or_else(|| invalid_positioning_error("unformatted record length overflow"))?;
    let record_start = position
        .checked_sub(record_size)
        .ok_or_else(|| invalid_positioning_error("invalid sequential unformatted record"))?;

    stream.seek(SeekFrom::Start(record_start))?;
    stream.read_exact(&mut marker)?;
    if u32::from_ne_bytes(marker) as u64 != record_len {
        return Err(invalid_positioning_error(
            "mismatched sequential unformatted record markers",
        ));
    }
    stream.seek(SeekFrom::Start(record_start))?;
    Ok(())
}

fn finish_positioning_result(
    operation: &str,
    result: io::Result<()>,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    match result {
        Ok(()) => {
            if !iostat.is_null() {
                unsafe { *iostat = 0 };
            }
            assign_iomsg(iomsg, iomsg_len, "");
        }
        Err(error) => {
            let status = error.raw_os_error().unwrap_or(1);
            let message = format!("{}: {}", operation, error);
            assign_iomsg(iomsg, iomsg_len, &message);
            if !iostat.is_null() {
                unsafe { *iostat = status };
            } else {
                eprintln!("{}", message);
                std::process::exit(1);
            }
        }
    }
}

impl Unit {
    fn rewind(&mut self) -> io::Result<()> {
        if self.access == Access::Direct {
            return Err(invalid_positioning_error(
                "REWIND is not valid for direct access",
            ));
        }

        match &mut self.stream {
            UnitStream::FileRead(reader) => reader.seek(SeekFrom::Start(0)).map(|_| ()),
            UnitStream::FileWrite(writer) => writer
                .flush()
                .and_then(|()| writer.seek(SeekFrom::Start(0)).map(|_| ())),
            UnitStream::FileRaw(file) => file.seek(SeekFrom::Start(0)).map(|_| ()),
            _ => Err(invalid_positioning_error("unit does not support REWIND")),
        }?;
        self.reset_read_state_after_positioning();
        Ok(())
    }

    fn backspace(&mut self) -> io::Result<()> {
        if self.access == Access::Direct {
            return Err(invalid_positioning_error(
                "BACKSPACE is not valid for direct access",
            ));
        }
        if self.access == Access::Stream {
            return Err(invalid_positioning_error(
                "BACKSPACE is not valid for stream access",
            ));
        }

        let formatted = self.form == Form::Formatted;
        match &mut self.stream {
            UnitStream::FileRaw(file) => {
                if formatted {
                    backspace_formatted_stream(file)?;
                } else {
                    backspace_unformatted_stream(file)?;
                }
            }
            UnitStream::FileRead(reader) => {
                if formatted {
                    backspace_formatted_stream(reader)?;
                } else {
                    backspace_unformatted_stream(reader)?;
                }
            }
            UnitStream::FileWrite(writer) => {
                writer.flush()?;
                if formatted {
                    backspace_formatted_stream(writer.get_mut())?;
                } else {
                    backspace_unformatted_stream(writer.get_mut())?;
                }
            }
            _ => return Err(invalid_positioning_error("unit is not seekable")),
        }
        self.reset_read_state_after_positioning();
        Ok(())
    }

    fn endfile(&mut self) -> io::Result<()> {
        if self.access == Access::Direct {
            return Err(invalid_positioning_error(
                "ENDFILE is not valid for direct access",
            ));
        }
        if self.action == Action::Read {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unit is not open for writing",
            ));
        }

        match &mut self.stream {
            UnitStream::FileRaw(file) => {
                file.flush()?;
                let position = file.stream_position()?;
                file.set_len(position)?;
            }
            UnitStream::FileWrite(writer) => {
                writer.flush()?;
                let position = writer.stream_position()?;
                writer.get_ref().set_len(position)?;
            }
            _ => return Err(invalid_positioning_error("unit is not seekable")),
        }
        self.reset_read_state_after_positioning();
        Ok(())
    }
}

/// Backspace one record on a sequential unit.
#[no_mangle]
pub extern "C" fn afs_backspace(unit: i32, iostat: *mut i32) {
    afs_backspace_ex(unit, iostat, std::ptr::null_mut(), 0);
}

#[no_mangle]
pub extern "C" fn afs_backspace_ex(unit: i32, iostat: *mut i32, iomsg: *mut u8, iomsg_len: i64) {
    let result = with_unit(unit, Unit::backspace)
        .unwrap_or_else(|| Err(invalid_positioning_error("unit is not connected")));
    finish_positioning_result("BACKSPACE", result, iostat, iomsg, iomsg_len);
}

/// Write an end-of-file marker and truncate.
#[no_mangle]
pub extern "C" fn afs_endfile(unit: i32, iostat: *mut i32) {
    afs_endfile_ex(unit, iostat, std::ptr::null_mut(), 0);
}

#[no_mangle]
pub extern "C" fn afs_endfile_ex(unit: i32, iostat: *mut i32, iomsg: *mut u8, iomsg_len: i64) {
    let result = with_unit(unit, Unit::endfile)
        .unwrap_or_else(|| Err(invalid_positioning_error("unit is not connected")));
    finish_positioning_result("ENDFILE", result, iostat, iomsg, iomsg_len);
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
    write_inquire_bytes(buf, buf_len, value.as_bytes());
}

fn write_inquire_bytes(buf: *mut u8, buf_len: i64, value: &[u8]) {
    if buf.is_null() || buf_len <= 0 {
        return;
    }
    let n = buf_len as usize;
    let copy_len = value.len().min(n);
    unsafe {
        std::ptr::copy_nonoverlapping(value.as_ptr(), buf, copy_len);
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
fn write_leading_zero_capability(
    unit: Option<(&Form, LeadingZeroMode)>,
    buf: *mut u8,
    buf_len: i64,
) {
    let s = match unit {
        Some((Form::Formatted, mode)) => mode.inquire_str(),
        _ => "UNDEFINED",
    };
    write_inquire_string(buf, buf_len, s);
}

struct UnitInquiry {
    access: Access,
    form: Form,
    action: Action,
    recl: Option<i64>,
    leading_zero: LeadingZeroMode,
}

impl UnitInquiry {
    fn from_unit(unit: &Unit) -> Self {
        Self {
            access: unit.access,
            form: unit.form.clone(),
            action: unit.action,
            recl: unit.recl,
            leading_zero: unit.leading_zero,
        }
    }
}

fn find_connected_file(filename: &[u8]) -> Option<UnitInquiry> {
    let connections: Vec<SharedUnit> = io_state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .units
        .values()
        .cloned()
        .collect();
    connections
        .into_iter()
        .filter(|connection| connection.filename == filename)
        .find_map(|connection| {
            let guard = connection
                .unit
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            guard.as_ref().map(UnitInquiry::from_unit)
        })
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

    let path = path_from_filename(&fname);
    let file_exists = path.exists();
    if !exist.is_null() {
        unsafe {
            *exist = file_exists as i32;
        }
    }

    // Find unit connected to this file (if any).
    let connected_unit = find_connected_file(&fname);

    if !opened.is_null() {
        unsafe {
            *opened = connected_unit.is_some() as i32;
        }
    }

    write_inquire_bytes(name_buf, name_buf_len, &fname);

    if let Some(u) = connected_unit.as_ref() {
        write_unit_properties(
            u.access,
            &u.form,
            u.action,
            u.recl,
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
        write_leading_zero_capability(
            Some((&u.form, u.leading_zero)),
            leading_zero_buf,
            leading_zero_buf_len,
        );
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
        let sz = std::fs::metadata(&path)
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
) {
    let connection = connected_unit(unit);
    let mut unit_guard = connection.as_ref().map(|connection| {
        connection
            .unit
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    });
    let unit_entry = unit_guard.as_mut().and_then(|guard| guard.as_mut());

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
        write_inquire_bytes(name_buf, name_buf_len, &u.filename);
        write_unit_properties(
            u.access,
            &u.form,
            u.action,
            u.recl,
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
        write_leading_zero_capability(
            Some((&u.form, u.leading_zero)),
            leading_zero_buf,
            leading_zero_buf_len,
        );

        if !size_out.is_null() {
            let sz = if !u.filename.is_empty() {
                std::fs::metadata(path_from_filename(&u.filename))
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
#[allow(clippy::too_many_arguments)]
fn write_unit_properties(
    access: Access,
    form: &Form,
    action: Action,
    recl: Option<i64>,
    access_buf: *mut u8,
    access_buf_len: i64,
    form_buf: *mut u8,
    form_buf_len: i64,
    action_buf: *mut u8,
    action_buf_len: i64,
    recl_out: *mut i64,
) {
    let access_str = match access {
        Access::Sequential => "SEQUENTIAL",
        Access::Direct => "DIRECT",
        Access::Stream => "STREAM",
    };
    write_inquire_string(access_buf, access_buf_len, access_str);

    let form_str = match form {
        Form::Formatted => "FORMATTED",
        Form::Unformatted => "UNFORMATTED",
    };
    write_inquire_string(form_buf, form_buf_len, form_str);

    let action_str = match action {
        Action::Read => "READ",
        Action::Write => "WRITE",
        Action::ReadWrite => "READWRITE",
    };
    write_inquire_string(action_buf, action_buf_len, action_str);

    if !recl_out.is_null() {
        unsafe {
            *recl_out = recl.unwrap_or(-1);
        }
    }
}

// ---- FLUSH ----

/// Flush a unit's output buffer.
#[no_mangle]
pub extern "C" fn afs_flush(unit: i32, iostat: *mut i32) {
    let status = with_unit(unit, |u| {
        u.flush().err().map_or(0, |e| e.raw_os_error().unwrap_or(1))
    })
    .unwrap_or(1);
    if !iostat.is_null() {
        unsafe {
            *iostat = status;
        }
    }
}

// ---- REWIND / BACKSPACE / ENDFILE ----

/// Rewind a unit to the beginning.
#[no_mangle]
pub extern "C" fn afs_rewind(unit: i32, iostat: *mut i32) {
    let result = with_unit(unit, Unit::rewind)
        .unwrap_or_else(|| Err(invalid_positioning_error("unit is not connected")));
    finish_positioning_result("REWIND", result, iostat, std::ptr::null_mut(), 0);
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
    let connections: Vec<SharedUnit> = io_state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .units
        .values()
        .cloned()
        .collect();

    // A connection may be inside an input system call at process exit. Never let
    // that one unit suppress flushing and scratch cleanup for every other unit.
    for connection in &connections {
        let mut guard = match connection.unit.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => continue,
        };
        if let Some(unit) = guard.as_mut() {
            let _ = unit.flush();
        }
    }

    // Cleanup identity is immutable connection metadata, so it remains available
    // even when the unit itself is occupied by a blocking read.
    for path in connections
        .iter()
        .filter_map(|connection| connection.scratch_path.as_ref())
    {
        let _ = std::fs::remove_file(path_from_filename(path));
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
//   afs_fmt_push_int(val) / afs_fmt_push_int128(&val) / afs_fmt_push_real*(val) / ...
//   afs_fmt_end()

use crate::format::{
    format_reversion_descriptors, parse_format, BlankInterpretation, DecimalSep, FormatDesc,
    FormatEngine, FormatError, IoValue, LeadingZeroMode, RoundMode,
};
use std::cell::RefCell;

enum FmtSink {
    Unit(i32),
    Internal {
        buf: *mut u8,
        buf_len: usize,
    },
    /// Internal write whose target is a deferred-length allocatable
    /// `character(:), allocatable` scalar. The target is (re)allocated to the
    /// formatted record length whether or not it was already allocated
    /// (F2023 §12.4).
    InternalAlloc {
        desc: *mut u8,
    },
    /// Internal write whose target is a whole character array: each
    /// formatted record goes into one element (blank-padded to the
    /// element length). Records longer than an element are rejected.
    /// Elements after the last record
    /// written are left unchanged (F2023 12.6.4.8.3 leaves them
    /// undefined). Overflowing the array or targeting an unallocated
    /// one is an error — loud when no IOSTAT= is present.
    InternalArray {
        buf: *mut u8,
        elem_len: i64,
        nelems: i64,
    },
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

const FORMAT_CACHE_LIMIT: usize = 128;

thread_local! {
    static FMT_CTX: RefCell<Vec<FmtContext>> = const { RefCell::new(Vec::new()) };
    static FORMAT_CACHE: RefCell<HashMap<String, Arc<[FormatDesc]>>> =
        RefCell::new(HashMap::new());
}

fn cached_format_descriptors(fmt: &str) -> Result<Arc<[FormatDesc]>, FormatError> {
    FORMAT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(descriptors) = cache.get(fmt) {
            return Ok(Arc::clone(descriptors));
        }

        let descriptors = Arc::from(parse_format(fmt)?.into_boxed_slice());
        if cache.len() >= FORMAT_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(fmt.to_owned(), Arc::clone(&descriptors));
        Ok(descriptors)
    })
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
            values: Vec::with_capacity(1),
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
            values: Vec::with_capacity(1),
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
/// F2023 §12.4: when an internal file is a deferred-length allocatable
/// character scalar, the record is assigned by intrinsic assignment,
/// allocating or reallocating the variable to have length equal to the number
/// of characters written. An already-allocated target is NOT treated as a
/// fixed internal file (that is the "otherwise" case in §12.4, for
/// non-deferred-length units) — it is reallocated to the record length,
/// growing or shrinking as needed. Storage is malloc'd to match
/// `afs_dealloc_string`'s free.
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
            values: Vec::with_capacity(1),
            iostat,
            iomsg,
            iomsg_len,
            stmt_leading_zero: None,
        });
    });
}

/// Begin a formatted internal WRITE whose unit is a whole character
/// array: record-per-element semantics via FmtSink::InternalArray.
#[no_mangle]
pub extern "C" fn afs_fmt_begin_internal_array(
    buf: *mut u8,
    elem_len: i64,
    nelems: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    let fmt = unsafe_str(fmt_str, fmt_len);
    FMT_CTX.with(|ctx| {
        ctx.borrow_mut().push(FmtContext {
            sink: FmtSink::InternalArray {
                buf,
                elem_len,
                nelems,
            },
            format_str: fmt,
            values: Vec::with_capacity(1),
            iostat,
            iomsg,
            iomsg_len,
            stmt_leading_zero: None,
        });
    });
}

/// List-directed internal WRITE to a deferred-length allocatable
/// character scalar. The fixed-buffer writers can't serve this target
/// (an unallocated descriptor presents a len-0 view), so the record
/// is collected here and stored in one shot through
/// `store_internal_alloc_record` — the same F2023 §12.4 reallocate-to-record-
/// length semantics as the formatted path. Item rendering matches
/// `afs_write_internal_*` exactly (leading blank + `{}`).
struct LstIaContext {
    desc: *mut crate::descriptor::StringDescriptor,
    record: Vec<u8>,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
}

unsafe impl Send for LstIaContext {}

thread_local! {
    static LST_IA_CTX: RefCell<Vec<LstIaContext>> = const { RefCell::new(Vec::new()) };
}

#[no_mangle]
pub extern "C" fn afs_lst_ia_begin(
    desc: *mut u8,
    iostat: *mut i32,
    iomsg: *mut u8,
    iomsg_len: i64,
) {
    LST_IA_CTX.with(|ctx| {
        ctx.borrow_mut().push(LstIaContext {
            desc: desc as *mut crate::descriptor::StringDescriptor,
            record: Vec::new(),
            iostat,
            iomsg,
            iomsg_len,
        });
    });
}

#[no_mangle]
pub extern "C" fn afs_lst_ia_int(val: i64) {
    LST_IA_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.record.extend_from_slice(format!(" {}", val).as_bytes());
        }
    });
}

#[no_mangle]
pub extern "C" fn afs_lst_ia_int128(val: i128) {
    LST_IA_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.record.extend_from_slice(format!(" {}", val).as_bytes());
        }
    });
}

#[no_mangle]
pub extern "C" fn afs_lst_ia_logical(val: i32) {
    LST_IA_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.record
                .extend_from_slice(if val != 0 { b" T" } else { b" F" });
        }
    });
}

#[no_mangle]
pub extern "C" fn afs_lst_ia_real(val: f64) {
    LST_IA_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.record.extend_from_slice(format!(" {}", val).as_bytes());
        }
    });
}

#[no_mangle]
pub extern "C" fn afs_lst_ia_string(ptr: *const u8, len: i64) {
    LST_IA_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.record.push(b' ');
            if !ptr.is_null() && len > 0 {
                let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                c.record.extend_from_slice(slice);
            }
        }
    });
}

#[no_mangle]
pub extern "C" fn afs_lst_ia_end() {
    let Some(c) = LST_IA_CTX.with(|ctx| ctx.borrow_mut().pop()) else {
        return;
    };
    let mut io_status = 0i32;
    let mut io_msg = "";
    if !store_internal_alloc_record(c.desc, &c.record) {
        io_status = 1;
        io_msg = "out of memory";
    }
    if !c.iostat.is_null() {
        unsafe { *c.iostat = io_status };
    }
    if !c.iomsg.is_null() && c.iomsg_len > 0 {
        let cap = c.iomsg_len as usize;
        let bytes = io_msg.as_bytes();
        let copy = bytes.len().min(cap);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), c.iomsg, copy);
            if copy < cap {
                std::ptr::write_bytes(c.iomsg.add(copy), b' ', cap - copy);
            }
        }
    }
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

/// Push a widened real(4) value for formatted output.
#[no_mangle]
pub extern "C" fn afs_fmt_push_real32(val: f64) {
    FMT_CTX.with(|ctx| {
        if let Some(c) = ctx.borrow_mut().last_mut() {
            c.values.push(IoValue::Real32(val));
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

fn is_simple_character_format(fmt: &str) -> bool {
    let trimmed = fmt.trim();
    let inner = trimmed
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(trimmed)
        .trim();
    inner.eq_ignore_ascii_case("A")
}

/// End the formatted write: apply the format engine and write the result.
/// If advance is true (nonzero), appends a newline. If false (zero), no newline.
#[no_mangle]
pub extern "C" fn afs_fmt_end(advance: i32) {
    FMT_CTX.with(|ctx| {
        let context = ctx.borrow_mut().pop();
        if let Some(c) = context {
            // Track success across the sink branches. List-directed and
            // scalar formatted writes both leave `iostat` untouched on
            // older builds; stdlib's savetxt loops `if (ios/=0) error_stop`
            // on the post-write value, so a write that silently leaves the
            // pre-call sentinel in place trips the error-stop branch every
            // iteration. Set `*iostat = 0` on success and stash an empty
            // iomsg so callers see a clean state.
            let mut io_status: i32 = 0;
            let mut io_msg: Option<&'static str> = None;

            match cached_format_descriptors(&c.format_str) {
                Err(_) => {
                    io_status = 1;
                    io_msg = Some("invalid format");
                }
                Ok(descriptors) => {
                    match c.sink {
                    FmtSink::Unit(unit) => {
                    let fast_character = if c.stmt_leading_zero.is_none()
                        && is_simple_character_format(&c.format_str)
                    {
                        match c.values.as_slice() {
                            [IoValue::Character(bytes)] => Some(bytes.as_slice()),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    if with_unit(unit, |u| {
                        if let Some(bytes) = fast_character {
                            if u.write_bytes(bytes).is_err()
                                || (advance != 0 && u.write_bytes(b"\n").is_err())
                            {
                                io_status = 1;
                                io_msg = Some("write failed");
                            }
                        } else {
                            let mut engine = FormatEngine::from_shared(Arc::clone(&descriptors));
                            // The statement override beats the connection mode;
                            // format descriptors can still override it mid-string.
                            engine.set_leading_zero(
                                c.stmt_leading_zero.unwrap_or(u.leading_zero),
                            );
                            match engine.format_values_reverting_bytes_checked(&c.values) {
                                Ok(mut output) => {
                                    if advance != 0 {
                                        output.push(b'\n');
                                    }
                                    if u.write_bytes(&output).is_err() {
                                        io_status = 1;
                                        io_msg = Some("write failed");
                                    }
                                }
                                Err(_) => {
                                    io_status = 1;
                                    io_msg = Some("format error");
                                }
                            }
                        }
                    })
                    .is_none()
                    {
                        io_status = 1;
                        io_msg = Some("unit not connected");
                    }
                    }
                    FmtSink::Internal { buf, buf_len } => {
                    let mut engine = FormatEngine::from_shared(Arc::clone(&descriptors));
                    if let Some(mode) = c.stmt_leading_zero {
                        engine.set_leading_zero(mode);
                    }
                    // Reverting scans: a scalar internal file has exactly
                    // one record, so a second scan (or an explicit '/')
                    // is an overflow — previously the excess values were
                    // silently dropped.
                    match engine.format_values_reverting_bytes_checked(&c.values) {
                        Ok(output) => {
                            if output.contains(&b'\n') {
                                io_status = 1;
                                io_msg = Some(
                                    "internal WRITE of more than one record into a character scalar",
                                );
                                if c.iostat.is_null() {
                                    eprintln!(
                                        "ERROR: internal WRITE of more than one record into a character scalar"
                                    );
                                    std::process::exit(2);
                                }
                            } else if output.len() > buf_len {
                                io_status = IOSTAT_EOR;
                                io_msg = Some("end of record");
                            } else {
                                write_to_buffer(
                                    buf,
                                    buf_len,
                                    0,
                                    &output,
                                    std::ptr::null_mut(),
                                );
                            }
                        }
                        Err(_) => {
                            io_status = 1;
                            io_msg = Some("format error");
                        }
                    }
                    }
                    FmtSink::InternalAlloc { desc } => {
                    let mut engine = FormatEngine::from_shared(Arc::clone(&descriptors));
                    if let Some(mode) = c.stmt_leading_zero {
                        engine.set_leading_zero(mode);
                    }
                    // Same one-record rule as FmtSink::Internal.
                    match engine.format_values_reverting_bytes_checked(&c.values) {
                        Ok(output) => {
                            if output.contains(&b'\n') {
                                io_status = 1;
                                io_msg = Some(
                                    "internal WRITE of more than one record into a character scalar",
                                );
                                if c.iostat.is_null() {
                                    eprintln!(
                                        "ERROR: internal WRITE of more than one record into a character scalar"
                                    );
                                    std::process::exit(2);
                                }
                            } else if !store_internal_alloc_record(
                                desc as *mut crate::descriptor::StringDescriptor,
                                &output,
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
                    FmtSink::InternalArray {
                        buf,
                        elem_len,
                        nelems,
                    } => {
                    let mut engine = FormatEngine::from_shared(descriptors);
                    if let Some(mode) = c.stmt_leading_zero {
                        engine.set_leading_zero(mode);
                    }
                    // Reverting: each new format scan starts a new record,
                    // i.e. the next array element.
                    match engine.format_values_reverting_bytes_checked(&c.values) {
                        Ok(output) => {
                            if buf.is_null() || elem_len <= 0 || nelems <= 0 {
                                io_status = 1;
                                io_msg = Some(
                                    "internal WRITE to an unallocated or zero-size character array",
                                );
                                if c.iostat.is_null() {
                                    eprintln!(
                                        "ERROR: internal WRITE to an unallocated or zero-size character array"
                                    );
                                    std::process::exit(2);
                                }
                            } else {
                                let records: Vec<&[u8]> =
                                    output.split(|&b| b == b'\n').collect();
                                if records.len() as i64 > nelems {
                                    io_status = 1;
                                    io_msg = Some("write exceeds internal file size");
                                    if c.iostat.is_null() {
                                        eprintln!(
                                            "ERROR: internal WRITE of {} records into a {}-element character array",
                                            records.len(),
                                            nelems
                                        );
                                        std::process::exit(2);
                                    }
                                } else if records
                                    .iter()
                                    .any(|record| record.len() > elem_len as usize)
                                {
                                    io_status = IOSTAT_EOR;
                                    io_msg = Some("end of record");
                                } else {
                                    for (i, rec) in records.iter().enumerate() {
                                        write_to_buffer(
                                            unsafe { buf.add(i * elem_len as usize) },
                                            elem_len as usize,
                                            0,
                                            rec,
                                            std::ptr::null_mut(),
                                        );
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            io_status = 1;
                            io_msg = Some("format error");
                        }
                    }
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
            if io_status != 0 && c.iostat.is_null() {
                eprintln!(
                    "Fortran runtime error: {}",
                    io_msg.unwrap_or("formatted I/O error")
                );
                std::process::exit(2);
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

fn read_formatted_field(desc: &FormatDesc, input: &[u8], cursor: &mut usize) -> Option<Vec<u8>> {
    let take_width = |cursor: &mut usize, width: usize| {
        let start = (*cursor).min(input.len());
        let end = start.saturating_add(width).min(input.len());
        *cursor = end;
        input[start..end].to_vec()
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
            Some(input[start..].to_vec())
        }
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct FormattedInputState {
    blank_mode: BlankInterpretation,
    scale_factor: i32,
    decimal_sep: DecimalSep,
    round_mode: RoundMode,
}

impl Default for FormattedInputState {
    fn default() -> Self {
        Self {
            blank_mode: BlankInterpretation::Null,
            scale_factor: 0,
            decimal_sep: DecimalSep::Point,
            round_mode: RoundMode::ProcessorDefined,
        }
    }
}

fn update_formatted_input_state(desc: &FormatDesc, state: &mut FormattedInputState) {
    match desc {
        FormatDesc::BlankMode(mode) => state.blank_mode = *mode,
        FormatDesc::ScaleFactor(scale) => state.scale_factor = *scale,
        FormatDesc::DecimalMode(sep) => state.decimal_sep = *sep,
        FormatDesc::RoundingMode(mode) => state.round_mode = *mode,
        _ => {}
    }
}

fn finite_formatted_data_count(descs: &[FormatDesc]) -> Result<Option<usize>, ()> {
    let mut total = 0usize;
    for desc in descs {
        let count = match desc {
            FormatDesc::IntegerI { .. }
            | FormatDesc::IntegerB { .. }
            | FormatDesc::IntegerO { .. }
            | FormatDesc::IntegerZ { .. }
            | FormatDesc::RealF { .. }
            | FormatDesc::RealE { .. }
            | FormatDesc::RealEN { .. }
            | FormatDesc::RealES { .. }
            | FormatDesc::RealEX { .. }
            | FormatDesc::RealD { .. }
            | FormatDesc::RealG { .. }
            | FormatDesc::Logical { .. }
            | FormatDesc::Character { .. }
            | FormatDesc::CharTrimmed
            | FormatDesc::DerivedType { .. } => 1,
            FormatDesc::Group {
                repeat,
                descriptors,
                ..
            } => {
                let Some(nested) = finite_formatted_data_count(descriptors)? else {
                    return Ok(None);
                };
                repeat.checked_mul(nested).ok_or(())?
            }
            FormatDesc::UnlimitedRepeat { .. } => return Ok(None),
            _ => 0,
        };
        total = total.checked_add(count).ok_or(())?;
    }
    Ok(Some(total))
}

fn apply_formatted_input_state_scan(descs: &[FormatDesc], state: &mut FormattedInputState) {
    for desc in descs {
        match desc {
            FormatDesc::Group {
                repeat,
                descriptors,
                ..
            } if *repeat > 0 => {
                // Every input-state descriptor assigns a mode rather than
                // incrementally mutating it, so one traversal has the same
                // final state as any positive repeat count.
                apply_formatted_input_state_scan(descriptors, state);
            }
            FormatDesc::UnlimitedRepeat { descriptors } => {
                apply_formatted_input_state_scan(descriptors, state);
            }
            _ => update_formatted_input_state(desc, state),
        }
    }
}

struct FormattedInputPlan<'a> {
    descriptors: &'a [FormatDesc],
    local_data_index: usize,
    state: FormattedInputState,
    starts_new_record: bool,
}

fn plan_formatted_input(descs: &[FormatDesc], data_index: i64) -> Option<FormattedInputPlan<'_>> {
    let data_index = usize::try_from(data_index.max(0)).ok()?;
    let Some(initial_count) = finite_formatted_data_count(descs).ok()? else {
        return Some(FormattedInputPlan {
            descriptors: descs,
            local_data_index: data_index,
            state: FormattedInputState::default(),
            starts_new_record: data_index == 0,
        });
    };
    if initial_count == 0 {
        return None;
    }
    if data_index < initial_count {
        return Some(FormattedInputPlan {
            descriptors: descs,
            local_data_index: data_index,
            state: FormattedInputState::default(),
            starts_new_record: data_index == 0,
        });
    }

    let reversion_descs = format_reversion_descriptors(descs);
    let reversion_count = finite_formatted_data_count(reversion_descs).ok()??;
    if reversion_count == 0 {
        return None;
    }

    let reverted_index = data_index - initial_count;
    let completed_reversion_scans = reverted_index / reversion_count;
    let local_data_index = reverted_index % reversion_count;
    let mut state = FormattedInputState::default();
    apply_formatted_input_state_scan(descs, &mut state);
    if completed_reversion_scans > 0 {
        apply_formatted_input_state_scan(reversion_descs, &mut state);
    }

    Some(FormattedInputPlan {
        descriptors: reversion_descs,
        local_data_index,
        state,
        starts_new_record: local_data_index == 0,
    })
}

fn extract_nth_formatted_field_with_state(
    descs: &[FormatDesc],
    input: &[u8],
    cursor: &mut usize,
    remaining_data_index: &mut usize,
    state: &mut FormattedInputState,
) -> Option<(FormatDesc, Vec<u8>, FormattedInputState)> {
    for desc in descs {
        match desc {
            FormatDesc::Group {
                repeat,
                descriptors,
                ..
            } => {
                for _ in 0..*repeat {
                    if let Some(found) = extract_nth_formatted_field_with_state(
                        descriptors,
                        input,
                        cursor,
                        remaining_data_index,
                        state,
                    ) {
                        return Some(found);
                    }
                }
            }
            FormatDesc::UnlimitedRepeat { descriptors } => {
                let mut loop_guard = 0usize;
                while *cursor < input.len() && loop_guard < input.len().saturating_add(1) {
                    let before = *cursor;
                    if let Some(found) = extract_nth_formatted_field_with_state(
                        descriptors,
                        input,
                        cursor,
                        remaining_data_index,
                        state,
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
                        return Some((desc.clone(), field, *state));
                    }
                    *remaining_data_index -= 1;
                } else {
                    update_formatted_input_state(desc, state);
                    advance_formatted_cursor(desc, input, cursor);
                }
            }
        }
    }

    None
}

fn extract_nth_formatted_field(
    descs: &[FormatDesc],
    input: &[u8],
    cursor: &mut usize,
    remaining_data_index: &mut usize,
) -> Option<(FormatDesc, Vec<u8>)> {
    let mut state = FormattedInputState::default();
    extract_nth_formatted_field_with_state(descs, input, cursor, remaining_data_index, &mut state)
        .map(|(desc, field, _)| (desc, field))
}

fn read_nonadvancing_formatted_field(
    desc: &FormatDesc,
    input: &[u8],
    cursor: &mut usize,
    dest_len: i64,
) -> Option<Vec<u8>> {
    match desc {
        FormatDesc::Character { width: None } => {
            let start = (*cursor).min(input.len());
            let n = dest_len.max(0) as usize;
            let end = start.saturating_add(n).min(input.len());
            *cursor = end;
            Some(input[start..end].to_vec())
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
) -> Option<(FormatDesc, Vec<u8>)> {
    for desc in descs {
        match desc {
            FormatDesc::Group {
                repeat,
                descriptors,
                ..
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
) -> Result<(FormatDesc, Vec<u8>), i32> {
    parse_nth_formatted_record_with_state(input, fmt_str, fmt_len, data_index)
        .map(|(desc, field, _)| (desc, field))
}

fn parse_nth_formatted_record_with_state(
    input: &[u8],
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
) -> Result<(FormatDesc, Vec<u8>, FormattedInputState), i32> {
    let fmt = unsafe_str(fmt_str, fmt_len);
    let descs = parse_format(&fmt).map_err(|_| 1)?;
    let mut cursor = 0usize;
    let mut remaining = data_index.max(0) as usize;
    let mut state = FormattedInputState::default();

    extract_nth_formatted_field_with_state(&descs, input, &mut cursor, &mut remaining, &mut state)
        .ok_or(-1)
}

fn parse_nth_formatted_internal_field(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
) -> Result<(FormatDesc, Vec<u8>), i32> {
    if buf.is_null() || buf_len <= 0 {
        return Err(-1);
    }

    let input = unsafe { std::slice::from_raw_parts(buf, buf_len as usize) };
    parse_nth_formatted_record(input, fmt_str, fmt_len, data_index)
}

fn parse_nth_formatted_internal_field_with_state(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
) -> Result<(FormatDesc, Vec<u8>, FormattedInputState), i32> {
    if buf.is_null() || buf_len <= 0 {
        return Err(-1);
    }

    let input = unsafe { std::slice::from_raw_parts(buf, buf_len as usize) };
    parse_nth_formatted_record_with_state(input, fmt_str, fmt_len, data_index)
}

fn trim_record_newline(mut line: Vec<u8>) -> Vec<u8> {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    line
}

fn nonadvancing_read_len(descs: &[FormatDesc], dest_len: i64) -> usize {
    for desc in descs {
        if let FormatDesc::Character { width } = desc {
            return width.unwrap_or_else(|| dest_len.max(1) as usize).max(1);
        }
    }
    dest_len.max(1) as usize
}

#[cfg(unix)]
fn read_stdin_unbuffered(buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc_read(0, buf.as_mut_ptr().cast::<c_void>(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

#[cfg(not(unix))]
fn read_stdin_unbuffered(buf: &mut [u8]) -> io::Result<usize> {
    io::stdin().read(buf)
}

fn read_nonadvancing_chunk(
    unit: &mut Unit,
    descs: &[FormatDesc],
    dest_len: i64,
) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; nonadvancing_read_len(descs, dest_len)];
    let n = unit.read_nonadvancing_bytes(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

fn formatted_read_record_for_unit(unit: i32, starts_new_record: bool) -> Result<Vec<u8>, i32> {
    with_unit(unit, |u| {
        if starts_new_record || u.formatted_read_record.is_none() {
            u.formatted_read_record = None;
            u.formatted_read_cursor = 0;
            match u.read_line_bytes() {
                Ok(line) if !line.is_empty() => {
                    u.formatted_read_record = Some(line);
                }
                Ok(_) => return Err(IOSTAT_END),
                Err(_) => return Err(1),
            }
        }

        u.formatted_read_record
            .as_ref()
            .map(|line| trim_record_newline(line.clone()))
            .ok_or(IOSTAT_END)
    })
    .unwrap_or(Err(1))
}

fn parse_nth_formatted_unit_field_with_state(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
) -> Result<(FormatDesc, Vec<u8>, FormattedInputState), i32> {
    let fmt = unsafe_str(fmt_str, fmt_len);
    let descs = parse_format(&fmt).map_err(|_| 1)?;
    let plan = plan_formatted_input(&descs, data_index).ok_or(1)?;
    let input = formatted_read_record_for_unit(unit, plan.starts_new_record)?;
    let mut cursor = 0usize;
    let mut remaining = plan.local_data_index;
    let mut state = plan.state;

    extract_nth_formatted_field_with_state(
        plan.descriptors,
        &input,
        &mut cursor,
        &mut remaining,
        &mut state,
    )
    .ok_or(IOSTAT_END)
}

fn parse_nth_formatted_unit_field(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
) -> Result<(FormatDesc, Vec<u8>), i32> {
    parse_nth_formatted_unit_field_with_state(unit, fmt_str, fmt_len, data_index)
        .map(|(desc, field, _)| (desc, field))
}

fn store_formatted_char_result(
    field: &[u8],
    dest: *mut u8,
    dest_len: i64,
    size_out: *mut i32,
    iostat: *mut i32,
) {
    crate::string::afs_assign_char_fixed(dest, dest_len, field.as_ptr(), field.len() as i64);
    if !size_out.is_null() {
        unsafe {
            let transferred = field.len().min(i32::MAX as usize) as i32;
            *size_out = (*size_out).saturating_add(transferred);
        }
    }
    if !iostat.is_null() {
        unsafe {
            *iostat = 0;
        }
    }
}

fn store_formatted_char_error(
    dest: *mut u8,
    dest_len: i64,
    _size_out: *mut i32,
    code: i32,
    iostat: *mut i32,
) {
    crate::string::afs_assign_char_fixed(dest, dest_len, std::ptr::null(), 0);
    if code != 0 {
        set_read_status_or_exit(iostat, code);
    }
}

fn nonadvancing_char_field_hit_eor(desc: &FormatDesc, field: &[u8], dest_len: i64) -> bool {
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

fn formatted_real_decimals(desc: &FormatDesc) -> Option<usize> {
    match desc {
        FormatDesc::RealF { decimals, .. }
        | FormatDesc::RealE { decimals, .. }
        | FormatDesc::RealEN { decimals, .. }
        | FormatDesc::RealES { decimals, .. }
        | FormatDesc::RealEX { decimals, .. }
        | FormatDesc::RealD { decimals, .. }
        | FormatDesc::RealG { decimals, .. } => Some(*decimals),
        _ => None,
    }
}

fn parse_formatted_real_field(
    desc: &FormatDesc,
    field: &[u8],
    state: FormattedInputState,
) -> Option<f64> {
    let (normalized, decimals) = normalize_formatted_real_field(desc, field, state)?;
    crate::decimal_input::parse_f64(&normalized, decimals, state.scale_factor, state.round_mode)
}

fn parse_formatted_real32_field(
    desc: &FormatDesc,
    field: &[u8],
    state: FormattedInputState,
) -> Option<f32> {
    let (normalized, decimals) = normalize_formatted_real_field(desc, field, state)?;
    crate::decimal_input::parse_f32(&normalized, decimals, state.scale_factor, state.round_mode)
}

fn normalize_formatted_real_field(
    desc: &FormatDesc,
    field: &[u8],
    state: FormattedInputState,
) -> Option<(String, usize)> {
    let decimals = formatted_real_decimals(desc)?;
    let field = String::from_utf8_lossy(field);
    let mut numeric = match state.blank_mode {
        BlankInterpretation::Null => field.chars().filter(|&ch| ch != ' ').collect::<String>(),
        BlankInterpretation::Zero => field
            .trim_start_matches(' ')
            .chars()
            .map(|ch| if ch == ' ' { '0' } else { ch })
            .collect::<String>(),
    };

    match state.decimal_sep {
        DecimalSep::Point => {
            if numeric.contains(',') {
                return None;
            }
        }
        DecimalSep::Comma => {
            if numeric.contains('.') {
                return None;
            }
            numeric = numeric.replace(',', ".");
        }
    }

    let normalized = normalize_fortran_real_input(&numeric, false);
    Some((normalized, decimals))
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
        let connection = connected_unit(unit);
        let mut unit_guard = connection.as_ref().map(|connection| {
            connection
                .unit
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        if let Some(u) = unit_guard.as_mut().and_then(|guard| guard.as_mut()) {
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
                let descs = match parse_format(&fmt) {
                    Ok(descs) => descs,
                    Err(_) => {
                        drop(unit_guard);
                        store_formatted_char_error(dest, dest_len, size_out, 1, iostat);
                        return;
                    }
                };
                let input = u
                    .formatted_read_record
                    .as_ref()
                    .cloned()
                    .unwrap_or_default();
                let mut cursor = u.formatted_read_cursor;
                let mut remaining = 0usize;
                let outcome =
                    extract_nth_formatted_field(&descs, &input, &mut cursor, &mut remaining);
                u.formatted_read_record = None;
                u.formatted_read_cursor = 0;
                drop(unit_guard);
                match outcome {
                    Some((FormatDesc::Character { .. }, field)) => {
                        store_formatted_char_result(&field, dest, dest_len, size_out, iostat);
                    }
                    _ => {
                        store_formatted_char_error(dest, dest_len, size_out, IOSTAT_EOR, iostat);
                    }
                }
                return;
            }
        }
    }
    match parse_nth_formatted_unit_field(unit, fmt_str, fmt_len, data_index) {
        Ok((FormatDesc::Character { .. }, field)) => {
            store_formatted_char_result(&field, dest, dest_len, size_out, iostat);
        }
        Ok(_) => {
            store_formatted_char_error(dest, dest_len, size_out, 1, iostat);
        }
        Err(code) => {
            store_formatted_char_error(dest, dest_len, size_out, code, iostat);
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
    let descs = match parse_format(&fmt) {
        Ok(descs) => descs,
        Err(_) => {
            store_formatted_char_error(dest, dest_len, size_out, 1, iostat);
            return;
        }
    };

    if with_unit(unit, |u| {
        let mut terminal_chunk_without_newline = false;
        if u.formatted_read_record.is_none() {
            let bounded_nonadvancing = matches!(&u.stream, UnitStream::Stdin) || u.is_terminal();
            let read_result = if bounded_nonadvancing {
                read_nonadvancing_chunk(u, &descs, dest_len)
            } else {
                u.read_line_bytes()
            };
            match read_result {
                Ok(line) if !line.is_empty() => {
                    terminal_chunk_without_newline =
                        bounded_nonadvancing && !line.iter().any(|&b| matches!(b, b'\n' | b'\r'));
                    if bounded_nonadvancing {
                        u.terminal_nonadvancing_open_record = terminal_chunk_without_newline;
                    }
                    u.formatted_read_record = Some(trim_record_newline(line));
                    u.formatted_read_cursor = 0;
                }
                Ok(_) => {
                    let code = if bounded_nonadvancing && u.terminal_nonadvancing_open_record {
                        u.terminal_nonadvancing_open_record = false;
                        IOSTAT_EOR
                    } else {
                        IOSTAT_END
                    };
                    store_formatted_char_error(dest, dest_len, size_out, code, iostat);
                    return;
                }
                Err(_) => {
                    store_formatted_char_error(dest, dest_len, size_out, 1, iostat);
                    return;
                }
            }
        }

        let input = u
            .formatted_read_record
            .as_ref()
            .cloned()
            .unwrap_or_default();
        if u.formatted_read_cursor >= input.len() {
            store_formatted_char_error(dest, dest_len, size_out, IOSTAT_EOR, iostat);
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
                    } else if terminal_chunk_without_newline {
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
                store_formatted_char_error(dest, dest_len, size_out, 1, iostat);
            }
            None => {
                store_formatted_char_error(dest, dest_len, size_out, IOSTAT_EOR, iostat);
                u.formatted_read_record = None;
                u.formatted_read_cursor = 0;
            }
        }
    })
    .is_none()
    {
        store_formatted_char_error(dest, dest_len, size_out, 1, iostat);
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
    match parse_nth_formatted_unit_field(unit, fmt_str, fmt_len, data_index) {
        Ok((desc @ FormatDesc::IntegerI { .. }, field))
        | Ok((desc @ FormatDesc::IntegerB { .. }, field))
        | Ok((desc @ FormatDesc::IntegerO { .. }, field))
        | Ok((desc @ FormatDesc::IntegerZ { .. }, field)) => {
            let field_text = String::from_utf8_lossy(&field);
            match parse_formatted_integer_field(&desc, &field_text)
                .and_then(|v| i32::try_from(v).ok())
            {
                Some(v) => {
                    write_i32_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_logical(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut i32,
    iostat: *mut i32,
) {
    let result = parse_nth_formatted_unit_field(unit, fmt_str, fmt_len, data_index);
    store_formatted_logical_result(result, val, iostat);
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
    match parse_nth_formatted_unit_field(unit, fmt_str, fmt_len, data_index) {
        Ok((desc @ FormatDesc::IntegerI { .. }, field))
        | Ok((desc @ FormatDesc::IntegerB { .. }, field))
        | Ok((desc @ FormatDesc::IntegerO { .. }, field))
        | Ok((desc @ FormatDesc::IntegerZ { .. }, field)) => {
            let field_text = String::from_utf8_lossy(&field);
            match parse_formatted_integer_field(&desc, &field_text)
                .and_then(|v| i64::try_from(v).ok())
            {
                Some(v) => {
                    write_i64_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
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
    match parse_nth_formatted_unit_field(unit, fmt_str, fmt_len, data_index) {
        Ok((desc @ FormatDesc::IntegerI { .. }, field))
        | Ok((desc @ FormatDesc::IntegerB { .. }, field))
        | Ok((desc @ FormatDesc::IntegerO { .. }, field))
        | Ok((desc @ FormatDesc::IntegerZ { .. }, field)) => {
            let field_text = String::from_utf8_lossy(&field);
            match parse_formatted_integer_field(&desc, &field_text) {
                Some(v) => {
                    write_i128_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
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
    match parse_nth_formatted_unit_field_with_state(unit, fmt_str, fmt_len, data_index) {
        Ok((desc, field, input_state)) if formatted_real_decimals(&desc).is_some() => {
            match parse_formatted_real_field(&desc, &field, input_state) {
                Some(v) => {
                    write_f64_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_real32(
    unit: i32,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut f32,
    iostat: *mut i32,
) {
    match parse_nth_formatted_unit_field_with_state(unit, fmt_str, fmt_len, data_index) {
        Ok((desc, field, input_state)) if formatted_real_decimals(&desc).is_some() => {
            match parse_formatted_real32_field(&desc, &field, input_state) {
                Some(v) => {
                    write_f32_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
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
            store_formatted_char_error(dest, dest_len, size_out, 1, iostat);
        }
        Err(code) => {
            store_formatted_char_error(dest, dest_len, size_out, code, iostat);
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
            let field_text = String::from_utf8_lossy(&field);
            match parse_formatted_integer_field(&desc, &field_text)
                .and_then(|v| i32::try_from(v).ok())
            {
                Some(v) => {
                    write_i32_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_logical_internal(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut i32,
    iostat: *mut i32,
) {
    let result = parse_nth_formatted_internal_field(buf, buf_len, fmt_str, fmt_len, data_index);
    store_formatted_logical_result(result, val, iostat);
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
            let field_text = String::from_utf8_lossy(&field);
            match parse_formatted_integer_field(&desc, &field_text)
                .and_then(|v| i64::try_from(v).ok())
            {
                Some(v) => {
                    write_i64_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
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
            let field_text = String::from_utf8_lossy(&field);
            match parse_formatted_integer_field(&desc, &field_text) {
                Some(v) => {
                    write_i128_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
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
    match parse_nth_formatted_internal_field_with_state(buf, buf_len, fmt_str, fmt_len, data_index)
    {
        Ok((desc, field, input_state)) if formatted_real_decimals(&desc).is_some() => {
            match parse_formatted_real_field(&desc, &field, input_state) {
                Some(v) => {
                    write_f64_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_fmt_read_real32_internal(
    buf: *const u8,
    buf_len: i64,
    fmt_str: *const u8,
    fmt_len: i64,
    data_index: i64,
    val: *mut f32,
    iostat: *mut i32,
) {
    match parse_nth_formatted_internal_field_with_state(buf, buf_len, fmt_str, fmt_len, data_index)
    {
        Ok((desc, field, input_state)) if formatted_real_decimals(&desc).is_some() => {
            match parse_formatted_real32_field(&desc, &field, input_state) {
                Some(v) => {
                    write_f32_ptr(val, v);
                    if !iostat.is_null() {
                        unsafe {
                            *iostat = 0;
                        }
                    }
                }
                None => {
                    set_read_status_or_exit(iostat, 1);
                }
            }
        }
        Ok(_) => {
            set_read_status_or_exit(iostat, 1);
        }
        Err(code) => {
            set_read_status_or_exit(iostat, code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    struct BlockingReader {
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<Vec<u8>>,
        buffer: Vec<u8>,
        position: usize,
    }

    impl Read for BlockingReader {
        fn read(&mut self, dest: &mut [u8]) -> io::Result<usize> {
            let available = self.fill_buf()?;
            let count = available.len().min(dest.len());
            dest[..count].copy_from_slice(&available[..count]);
            self.consume(count);
            Ok(count)
        }
    }

    impl BufRead for BlockingReader {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.position == self.buffer.len() {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).map_err(|_| {
                        io::Error::new(io::ErrorKind::BrokenPipe, "test reader lost observer")
                    })?;
                }
                self.buffer = self.release.recv().map_err(|_| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "test reader was not released")
                })?;
                self.position = 0;
            }
            Ok(&self.buffer[self.position..])
        }

        fn consume(&mut self, amount: usize) {
            self.position = (self.position + amount).min(self.buffer.len());
        }
    }

    fn test_unit(number: i32, stream: UnitStream, filename: Vec<u8>, scratch: bool) -> Unit {
        Unit {
            _number: number,
            stream,
            filename,
            _status: UnitStatus::Open,
            access: Access::Sequential,
            form: Form::Formatted,
            action: Action::ReadWrite,
            recl: None,
            read_tokens: VecDeque::new(),
            formatted_read_record: None,
            formatted_read_cursor: 0,
            terminal_nonadvancing_open_record: false,
            last_list_output_char: false,
            list_write_active: false,
            list_write_depth: 0,
            list_write_error: None,
            scratch,
            leading_zero: LeadingZeroMode::Default,
            pending_record: None,
            pending_read: None,
            list_read_depth: 0,
        }
    }

    #[test]
    fn blocked_read_does_not_stall_unrelated_flush_or_finalizer_cleanup() {
        const READ_UNIT: i32 = 901;
        const SCRATCH_UNIT: i32 = 902;
        const DEADLINE: Duration = Duration::from_secs(1);

        let scratch_path = std::env::temp_dir().join(format!(
            "afs_blocked_read_scratch_{}_{}.tmp",
            std::process::id(),
            line!()
        ));
        let scratch_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&scratch_path)
            .expect("create scratch witness");
        let scratch_name = os_string_to_bytes(scratch_path.clone().into_os_string());

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let reader = BlockingReader {
            entered: Some(entered_tx),
            release: release_rx,
            buffer: Vec::new(),
            position: 0,
        };

        {
            let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
            state.units.insert(
                READ_UNIT,
                UnitConnection::new(test_unit(
                    READ_UNIT,
                    UnitStream::TestRead(Box::new(reader)),
                    b"blocking-reader".to_vec(),
                    false,
                )),
            );
            state.units.insert(
                SCRATCH_UNIT,
                UnitConnection::new(test_unit(
                    SCRATCH_UNIT,
                    UnitStream::FileWrite(BufWriter::new(scratch_file)),
                    scratch_name,
                    true,
                )),
            );
        }

        let read_thread = std::thread::spawn(|| {
            let mut status = -99;
            afs_read_skip_record(READ_UNIT, &mut status);
            status
        });
        entered_rx
            .recv_timeout(DEADLINE)
            .expect("reader never reached the blocking operation");

        let (flush_tx, flush_rx) = mpsc::channel();
        let flush_thread = std::thread::spawn(move || {
            let mut status = -99;
            afs_flush(SCRATCH_UNIT, &mut status);
            let _ = flush_tx.send(status);
        });

        let (finalize_tx, finalize_rx) = mpsc::channel();
        let finalize_thread = std::thread::spawn(move || {
            afs_io_finalize();
            let _ = finalize_tx.send(());
        });

        let flush_before_release = flush_rx.recv_timeout(DEADLINE);
        let finalize_before_release = finalize_rx.recv_timeout(DEADLINE);
        let scratch_removed_before_release = !scratch_path.exists();

        let (close_started_tx, close_started_rx) = mpsc::channel();
        let (close_tx, close_rx) = mpsc::channel();
        let close_thread = std::thread::spawn(move || {
            let _ = close_started_tx.send(());
            let mut status = -99;
            afs_close(READ_UNIT, &mut status);
            let _ = close_tx.send(status);
        });
        close_started_rx
            .recv_timeout(DEADLINE)
            .expect("close thread did not start");
        let registry_deadline = std::time::Instant::now() + DEADLINE;
        while connected_unit(READ_UNIT).is_some() && std::time::Instant::now() < registry_deadline {
            std::thread::yield_now();
        }
        let disconnected_before_release = connected_unit(READ_UNIT).is_none();
        let close_before_release = close_rx.try_recv();

        release_tx
            .send(b"released\n".to_vec())
            .expect("release blocking reader");
        let read_status = read_thread.join().expect("join reader");
        flush_thread.join().expect("join flush");
        finalize_thread.join().expect("join finalizer");
        close_thread.join().expect("join close");

        {
            let mut state = io_state().lock().unwrap_or_else(|e| e.into_inner());
            state.units.remove(&SCRATCH_UNIT);
        }
        let _ = std::fs::remove_file(&scratch_path);

        assert_eq!(read_status, 0, "released record read must succeed");
        assert_eq!(
            flush_before_release,
            Ok(0),
            "FLUSH on an unrelated unit exceeded {DEADLINE:?}"
        );
        assert!(
            finalize_before_release.is_ok(),
            "I/O finalization exceeded {DEADLINE:?}"
        );
        assert!(
            scratch_removed_before_release,
            "finalization skipped unrelated scratch cleanup"
        );
        assert_eq!(
            close_before_release,
            Err(mpsc::TryRecvError::Empty),
            "same-unit CLOSE bypassed the in-flight read"
        );
        assert!(
            disconnected_before_release,
            "same-unit CLOSE did not detach the connection before waiting"
        );
        assert_eq!(
            close_rx.recv_timeout(DEADLINE),
            Ok(0),
            "same-unit CLOSE did not complete after the read"
        );
    }

    #[test]
    fn flush_reports_an_unconnected_unit() {
        let mut status = 0;

        afs_flush(i32::MAX, &mut status);

        assert_ne!(status, 0);
    }

    #[test]
    fn rewind_reports_an_unconnected_unit() {
        let mut status = 0;

        afs_rewind(i32::MAX, &mut status);

        assert_ne!(status, 0);
    }

    #[test]
    fn positioning_rejects_invalid_access_without_repositioning() {
        let path = format!(
            "/tmp/afs_backspace_unformatted_stream_{}.dat",
            std::process::id()
        );
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        for value in [4i32, 42, 4] {
            file.write_all(&value.to_ne_bytes()).unwrap();
        }
        let before = file.stream_position().unwrap();
        let mut unit = Unit {
            _number: 97,
            stream: UnitStream::FileRaw(file),
            filename: path.as_bytes().to_vec(),
            _status: UnitStatus::Open,
            access: Access::Stream,
            form: Form::Unformatted,
            action: Action::ReadWrite,
            recl: None,
            read_tokens: VecDeque::new(),
            formatted_read_record: None,
            formatted_read_cursor: 0,
            terminal_nonadvancing_open_record: false,
            last_list_output_char: false,
            list_write_active: false,
            list_write_depth: 0,
            list_write_error: None,
            scratch: false,
            leading_zero: LeadingZeroMode::Default,
            pending_record: None,
            pending_read: None,
            list_read_depth: 0,
        };

        let error = unit.backspace().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "BACKSPACE is not valid for stream access"
        );
        let after = match &mut unit.stream {
            UnitStream::FileRaw(file) => file.stream_position().unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(after, before);

        unit.form = Form::Formatted;
        let error = unit.backspace().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "BACKSPACE is not valid for stream access"
        );

        unit.access = Access::Direct;
        let error = unit.rewind().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "REWIND is not valid for direct access");
        let after = match &mut unit.stream {
            UnitStream::FileRaw(file) => file.stream_position().unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(after, before);

        drop(unit);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn list_directed_tokenizer_preserves_null_positions() {
        let tokens: Vec<_> = tokenize_list_directed_record("  , 42, , 7 8,  \n")
            .into_iter()
            .collect();
        assert_eq!(
            tokens,
            vec![
                ListReadToken::Null,
                ListReadToken::Value("42".into()),
                ListReadToken::Null,
                ListReadToken::Value("7".into()),
                ListReadToken::Value("8".into()),
            ]
        );
    }

    #[test]
    fn list_directed_tokenizer_preserves_delimited_character_values() {
        let tokens: Vec<_> =
            tokenize_list_directed_record(" 'alpha beta', \"gamma,delta\", 'don''t', plain\n")
                .into_iter()
                .collect();
        assert_eq!(
            tokens,
            vec![
                ListReadToken::Value("'alpha beta'".into()),
                ListReadToken::Value("\"gamma,delta\"".into()),
                ListReadToken::Value("'don''t'".into()),
                ListReadToken::Value("plain".into()),
            ]
        );
    }

    #[test]
    fn list_directed_character_read_unquotes_and_unescapes_delimiters() {
        let path = format!(
            "/tmp/afs_list_character_quotes_{}_{}.dat",
            std::process::id(),
            line!()
        );
        std::fs::write(
            &path,
            " 'alpha beta', \"gamma,delta\", 'don''t', ' spaced ', '', plain\n",
        )
        .expect("create quoted list-directed input");
        afs_open_simple(
            807,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let mut iostat = -99;
        for expected in [
            "alpha beta",
            "gamma,delta",
            "don't",
            " spaced ",
            "",
            "plain",
        ] {
            let mut actual = [b'?'; 16];
            afs_read_string(807, actual.as_mut_ptr(), actual.len() as i64, &mut iostat);
            assert_eq!(iostat, 0, "failed to read {expected:?}");

            let mut padded = [b' '; 16];
            padded[..expected.len()].copy_from_slice(expected.as_bytes());
            assert_eq!(actual, padded, "incorrect value for {expected:?}");
        }

        afs_close(807, &mut iostat);
        assert_eq!(iostat, 0, "CLOSE failed");
        std::fs::remove_file(path).expect("remove quoted list-directed input");
    }

    #[test]
    fn list_directed_character_decoder_rejects_malformed_delimiters() {
        assert_eq!(
            decode_list_directed_character_value("\"say \"\"hi\"\"\""),
            Ok("say \"hi\"".into())
        );
        assert_eq!(
            decode_list_directed_character_value("''"),
            Ok(String::new())
        );
        assert_eq!(
            decode_list_directed_character_value("plain"),
            Ok("plain".into())
        );
        assert!(decode_list_directed_character_value("'unterminated").is_err());
        assert!(decode_list_directed_character_value("'a'b'").is_err());
    }

    #[test]
    fn internal_list_directed_character_read_uses_quote_aware_cursor() {
        let input = b"'left,right', \"two words\", 'say ''hello'''";
        let mut position = 0;
        let mut iostat = -99;

        for expected in ["left,right", "two words", "say 'hello'"] {
            let mut actual = [b'?'; 16];
            afs_read_internal_string(
                input.as_ptr(),
                input.len() as i64,
                &mut position,
                actual.as_mut_ptr(),
                actual.len() as i64,
                &mut iostat,
            );
            assert_eq!(iostat, 0, "failed to read {expected:?}");

            let mut padded = [b' '; 16];
            padded[..expected.len()].copy_from_slice(expected.as_bytes());
            assert_eq!(actual, padded, "incorrect value for {expected:?}");
        }
    }

    #[test]
    fn internal_list_cursor_preserves_null_positions() {
        let buf = b",42, ,7";
        let mut pos = 0i64;

        assert_eq!(
            next_internal_token(buf.as_ptr(), buf.len() as i64, &mut pos),
            Some(ListReadToken::Null)
        );
        assert_eq!(
            next_internal_token(buf.as_ptr(), buf.len() as i64, &mut pos),
            Some(ListReadToken::Value("42".into()))
        );
        assert_eq!(
            next_internal_token(buf.as_ptr(), buf.len() as i64, &mut pos),
            Some(ListReadToken::Null)
        );
        assert_eq!(
            next_internal_token(buf.as_ptr(), buf.len() as i64, &mut pos),
            Some(ListReadToken::Value("7".into()))
        );
        assert_eq!(
            next_internal_token(buf.as_ptr(), buf.len() as i64, &mut pos),
            None
        );
    }

    #[test]
    fn logical_input_accepts_supported_spellings() {
        for token in ["T", "true", ".TRUE.", " .t. "] {
            assert_eq!(parse_logical_token(token), Some(true), "token={token:?}");
        }
        for token in ["F", "false", ".FALSE.", " .f. "] {
            assert_eq!(parse_logical_token(token), Some(false), "token={token:?}");
        }
        for token in ["", ".", "1", ".MAYBE."] {
            assert_eq!(parse_logical_token(token), None, "token={token:?}");
        }
    }

    #[test]
    fn internal_logical_read_consumes_each_token() {
        let buf = b"T, .FALSE.";
        let mut pos = 0i64;
        let mut first = 0i32;
        let mut second = 1i32;
        let mut iostat = -99i32;

        afs_read_internal_logical(
            buf.as_ptr(),
            buf.len() as i64,
            &mut pos,
            &mut first,
            &mut iostat,
        );
        assert_eq!((first, iostat), (1, 0));
        afs_read_internal_logical(
            buf.as_ptr(),
            buf.len() as i64,
            &mut pos,
            &mut second,
            &mut iostat,
        );
        assert_eq!((second, iostat), (0, 0));
    }

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
    fn formatted_real_input_applies_descriptor_state() {
        let read = |input: &[u8], fmt: &str, index: i64| {
            let (desc, field, state) =
                parse_nth_formatted_record_with_state(input, fmt.as_ptr(), fmt.len() as i64, index)
                    .expect("formatted field");
            parse_formatted_real_field(&desc, &field, state).expect("formatted real")
        };

        let cases = [
            (b"00123".as_slice(), "(F5.2)", 0, 1.23),
            (b"00123".as_slice(), "(1P,F5.2)", 0, 0.123),
            (b"00123".as_slice(), "(-1P,F5.2)", 0, 12.3),
            (b"1.23E2".as_slice(), "(1P,E6.2)", 0, 123.0),
            (b"1.23".as_slice(), "(1P,F4.2)", 0, 0.123),
            (b"00123E2".as_slice(), "(1P,E7.2)", 0, 123.0),
            (b"1 2".as_slice(), "(BN,F3.0)", 0, 12.0),
            (b"1 2".as_slice(), "(BZ,F3.0)", 0, 102.0),
            (b" 12".as_slice(), "(BZ,F3.0)", 0, 12.0),
            (b"12 ".as_slice(), "(BZ,F3.0)", 0, 120.0),
            (b"1,25".as_slice(), "(DC,F4.2)", 0, 1.25),
            (b"0012300123".as_slice(), "(2(1P,F5.2))", 1, 0.123),
            (b"1,251.25".as_slice(), "(DC,F4.2,DP,F4.2)", 1, 1.25),
        ];

        for (input, fmt, index, expected) in cases {
            let actual = read(input, fmt, index);
            assert!(
                (actual - expected).abs() < 1.0e-12,
                "input={input:?} fmt={fmt:?} index={index} actual={actual} expected={expected}"
            );
        }
    }

    #[test]
    fn formatted_real_input_applies_directed_rounding_state() {
        let read = |input: &[u8], fmt: &str| {
            let (desc, field, state) =
                parse_nth_formatted_record_with_state(input, fmt.as_ptr(), fmt.len() as i64, 0)
                    .expect("formatted field");
            parse_formatted_real_field(&desc, &field, state).expect("formatted real")
        };

        let positive = b"1.00000000000000011102230246251565404236316680908203125";
        let negative = b"-1.00000000000000011102230246251565404236316680908203125";

        assert_eq!(
            read(positive, "(RU,F55.53)").to_bits(),
            0x3ff0_0000_0000_0001
        );
        assert_eq!(
            read(positive, "(RD,F55.53)").to_bits(),
            0x3ff0_0000_0000_0000
        );
        assert_eq!(
            read(negative, "(RU,F56.53)").to_bits(),
            0xbff0_0000_0000_0000
        );
        assert_eq!(
            read(negative, "(RD,F56.53)").to_bits(),
            0xbff0_0000_0000_0001
        );
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
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
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
    fn scratch_status_rejects_file_without_touching_path() {
        let path = format!(
            "/tmp/afs_open_scratch_file_{}_{}.dat",
            std::process::id(),
            line!()
        );
        std::fs::write(&path, b"keep\n").expect("create scratch FILE sentinel");

        let mut iostat = -99i32;
        let mut iomsg = [b'?'; 128];
        let cb = OpenControlBlock {
            unit: 792,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "scratch".as_ptr(),
            status_len: 7,
            action: std::ptr::null(),
            action_len: 0,
            access: std::ptr::null(),
            access_len: 0,
            form: std::ptr::null(),
            form_len: 0,
            recl: 0,
            iostat: &mut iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
            iomsg: iomsg.as_mut_ptr(),
            iomsg_len: iomsg.len() as i64,
        };

        afs_open(&cb);

        assert_ne!(iostat, 0, "STATUS='SCRATCH' with FILE= must fail");
        let msg = String::from_utf8_lossy(&iomsg);
        assert!(
            msg.contains("OPEN:"),
            "expected OPEN context in iomsg: {msg:?}"
        );
        assert!(
            msg.contains("FILE="),
            "expected FILE context in iomsg: {msg:?}"
        );
        assert!(
            msg.contains("STATUS='SCRATCH'"),
            "expected STATUS context in iomsg: {msg:?}"
        );
        assert_eq!(
            std::fs::read(&path).expect("read scratch FILE sentinel"),
            b"keep\n",
            "rejected OPEN must not alter the named file"
        );
        let state = io_state().lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            !state.units.contains_key(&792),
            "rejected OPEN must not connect the unit"
        );
        drop(state);

        std::fs::remove_file(path).expect("remove scratch FILE sentinel");
    }

    #[test]
    fn failed_open_assigns_iomsg() {
        let path = format!(
            "/tmp/afs_open_iomsg_missing_{}_{}.dat",
            std::process::id(),
            line!()
        );
        let _ = std::fs::remove_file(&path);

        let mut iostat = -99i32;
        let mut iomsg = [b'X'; 128];
        let cb = OpenControlBlock {
            unit: 788,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "old".as_ptr(),
            status_len: 3,
            action: "read".as_ptr(),
            action_len: 4,
            access: std::ptr::null(),
            access_len: 0,
            form: std::ptr::null(),
            form_len: 0,
            recl: 0,
            iostat: &mut iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
            iomsg: iomsg.as_mut_ptr(),
            iomsg_len: iomsg.len() as i64,
        };

        afs_open(&cb);
        let msg = String::from_utf8_lossy(&iomsg);
        assert_ne!(iostat, 0, "missing file OPEN must fail");
        assert!(
            msg.contains("OPEN:"),
            "expected OPEN context in iomsg: {msg:?}"
        );
        assert!(msg.contains(&path), "expected filename in iomsg: {msg:?}");
    }

    // Darwin rejects arbitrary invalid UTF-8 path bytes before the runtime can
    // exercise its byte-preservation contract. Other Unix filesystems accept
    // this witness and still verify that OPEN does not transcode the name.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn open_preserves_non_utf8_filename_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let root = std::env::temp_dir().join(format!(
            "afs_latin1_open_{}_{}",
            std::process::id(),
            line!()
        ));
        let parent = root.join(std::ffi::OsString::from_vec(b"d_\xe9".to_vec()));
        std::fs::create_dir_all(&parent).expect("create non-UTF8 parent");
        let filename = os_string_to_bytes(parent.join("f.txt").into_os_string());

        let mut iostat = -99i32;
        let cb = OpenControlBlock {
            unit: 789,
            filename: filename.as_ptr(),
            filename_len: filename.len() as i64,
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
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
        };

        afs_open(&cb);
        assert_eq!(iostat, 0, "OPEN should preserve raw filename bytes");
        afs_close_ex(789, "delete".as_ptr(), 6, &mut iostat);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn direct_access_open_reports_iostat_without_creating() {
        let path = format!(
            "/tmp/afs_direct_access_reject_{}_{}.dat",
            std::process::id(),
            line!()
        );
        let _ = std::fs::remove_file(&path);

        let mut iostat = -99i32;
        let cb = OpenControlBlock {
            unit: 783,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "replace".as_ptr(),
            status_len: 7,
            action: "readwrite".as_ptr(),
            action_len: 9,
            access: "direct".as_ptr(),
            access_len: 6,
            form: "unformatted".as_ptr(),
            form_len: 11,
            recl: 4,
            iostat: &mut iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
        };

        afs_open(&cb);
        assert_ne!(iostat, 0, "direct access OPEN must be rejected");
        assert!(
            !std::path::Path::new(&path).exists(),
            "rejected direct access OPEN must not create a file"
        );
    }

    #[test]
    fn status_replace_defaults_to_readwrite() {
        let path = format!(
            "/tmp/afs_replace_readwrite_{}_{}.dat",
            std::process::id(),
            line!()
        );
        let _ = std::fs::remove_file(&path);

        let mut iostat = -99i32;
        let cb = OpenControlBlock {
            unit: 787,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "replace".as_ptr(),
            status_len: 7,
            action: std::ptr::null(),
            action_len: 0,
            access: std::ptr::null(),
            access_len: 0,
            form: std::ptr::null(),
            form_len: 0,
            recl: 0,
            iostat: &mut iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
        };

        afs_open(&cb);
        assert_eq!(iostat, 0, "expected replace OPEN to succeed");
        let properties = with_unit(787, |unit| {
            (unit.action, matches!(unit.stream, UnitStream::FileRaw(_)))
        })
        .expect("unit must be connected");
        assert_eq!(properties, (Action::ReadWrite, true));

        afs_close_ex(787, "delete".as_ptr(), 6, &mut iostat);
        assert_eq!(iostat, 0, "expected close/delete to succeed");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_to_stdout() {
        // This test just verifies no panic — output goes to test runner's stdout.
        afs_write_int(6, 42);
        afs_write_newline(6);
    }

    #[test]
    fn sequential_unformatted_read_rejects_incomplete_or_mismatched_framing() {
        let payload = 1234i32.to_ne_bytes();
        let marker4 = 4u32.to_ne_bytes();
        let marker8 = 8u32.to_ne_bytes();
        let cases = [
            (
                "truncated_payload",
                [marker8.as_slice(), payload.as_slice()].concat(),
            ),
            (
                "truncated_trailer",
                [marker4.as_slice(), payload.as_slice(), &marker4[..2]].concat(),
            ),
            (
                "mismatched_trailer",
                [marker4.as_slice(), payload.as_slice(), marker8.as_slice()].concat(),
            ),
        ];

        for (index, (name, bytes)) in cases.into_iter().enumerate() {
            let path = format!(
                "/tmp/afs_seq_unformatted_bad_frame_{}_{}_{}.dat",
                std::process::id(),
                line!(),
                index
            );
            std::fs::write(&path, bytes).expect("create malformed unformatted record");

            let unit = 810 + index as i32;
            let mut iostat = -99;
            let cb = OpenControlBlock {
                unit,
                filename: path.as_ptr(),
                filename_len: path.len() as i64,
                status: "old".as_ptr(),
                status_len: 3,
                action: "read".as_ptr(),
                action_len: 4,
                access: "sequential".as_ptr(),
                access_len: 10,
                form: "unformatted".as_ptr(),
                form_len: 11,
                recl: 0,
                iostat: &mut iostat,
                newunit: std::ptr::null_mut(),
                position: std::ptr::null(),
                position_len: 0,
                leading_zero: std::ptr::null(),
                leading_zero_len: 0,
                iomsg: std::ptr::null_mut(),
                iomsg_len: 0,
            };
            afs_open(&cb);
            assert_eq!(iostat, 0, "{name}: OPEN failed");

            afs_list_read_begin(unit, &mut iostat, std::ptr::null_mut(), 0);
            assert_ne!(iostat, 0, "{name}: malformed record was accepted");
            let pending_read = with_unit(unit, |connected| connected.pending_read.clone())
                .expect("test unit must remain open");
            assert!(
                pending_read.is_none(),
                "{name}: malformed record payload was published"
            );

            afs_close(unit, &mut iostat);
            assert_eq!(iostat, 0, "{name}: CLOSE failed");
            std::fs::remove_file(path).expect("remove malformed unformatted record");
        }
    }

    #[test]
    fn sequential_unformatted_read_accepts_exact_empty_and_payload_records() {
        let path = format!(
            "/tmp/afs_seq_unformatted_valid_frame_{}_{}.dat",
            std::process::id(),
            line!()
        );
        let marker0 = 0u32.to_ne_bytes();
        let marker4 = 4u32.to_ne_bytes();
        let payload = 2468i32.to_ne_bytes();
        std::fs::write(
            &path,
            [
                marker0.as_slice(),
                marker0.as_slice(),
                marker4.as_slice(),
                payload.as_slice(),
                marker4.as_slice(),
            ]
            .concat(),
        )
        .expect("create valid unformatted records");

        let mut iostat = -99;
        let cb = OpenControlBlock {
            unit: 809,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "old".as_ptr(),
            status_len: 3,
            action: "read".as_ptr(),
            action_len: 4,
            access: "sequential".as_ptr(),
            access_len: 10,
            form: "unformatted".as_ptr(),
            form_len: 11,
            recl: 0,
            iostat: &mut iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
        };
        afs_open(&cb);
        assert_eq!(iostat, 0, "OPEN failed");

        afs_list_read_begin(809, &mut iostat, std::ptr::null_mut(), 0);
        assert_eq!(iostat, 0, "empty record framing must be accepted");
        let pending_read = with_unit(809, |connected| connected.pending_read.clone())
            .expect("test unit must remain open");
        assert_eq!(pending_read, Some((Vec::new(), 0)));
        afs_list_read_end(809, &mut iostat, std::ptr::null_mut(), 0);

        afs_list_read_begin(809, &mut iostat, std::ptr::null_mut(), 0);
        assert_eq!(iostat, 0, "payload record framing must be accepted");
        let mut value = -1;
        afs_read_int(809, &mut value, &mut iostat);
        assert_eq!(iostat, 0, "exact payload read must succeed");
        assert_eq!(value, 2468);
        afs_list_read_end(809, &mut iostat, std::ptr::null_mut(), 0);

        afs_list_read_begin(809, &mut iostat, std::ptr::null_mut(), 0);
        assert_eq!(iostat, IOSTAT_END, "clean record boundary EOF expected");
        let pending_read = with_unit(809, |connected| connected.pending_read.clone())
            .expect("test unit must remain open");
        assert!(
            pending_read.is_none(),
            "EOF must not retain a completed record"
        );

        afs_close(809, &mut iostat);
        assert_eq!(iostat, 0, "CLOSE failed");
        std::fs::remove_file(path).expect("remove valid unformatted records");
    }

    #[test]
    fn nested_sequential_unformatted_write_shares_parent_record() {
        let path = format!(
            "/tmp/afs_seq_unformatted_nested_write_{}_{}.dat",
            std::process::id(),
            line!()
        );
        let mut outer_iostat = -99i32;
        let cb = OpenControlBlock {
            unit: 830,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "replace".as_ptr(),
            status_len: 7,
            action: "readwrite".as_ptr(),
            action_len: 9,
            access: "sequential".as_ptr(),
            access_len: 10,
            form: "unformatted".as_ptr(),
            form_len: 11,
            recl: 0,
            iostat: &mut outer_iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
        };
        afs_open(&cb);
        assert_eq!(outer_iostat, 0, "OPEN failed");

        afs_list_write_begin(830, &mut outer_iostat, std::ptr::null_mut(), 0);
        afs_write_int(830, 11);

        let mut child_iostat = -99i32;
        afs_list_write_begin(830, &mut child_iostat, std::ptr::null_mut(), 0);
        afs_write_int(830, 22);
        afs_list_write_end(830, 1, &mut child_iostat, std::ptr::null_mut(), 0);
        assert_eq!(child_iostat, 0, "nested child WRITE failed");
        let nested_state = with_unit(830, |unit| {
            (
                unit.list_write_depth,
                unit.pending_record.as_ref().map(Vec::len),
            )
        })
        .expect("test unit must remain open");
        assert_eq!(
            nested_state,
            (1, Some(8)),
            "child WRITE must leave the parent record open"
        );

        afs_write_int(830, 33);
        afs_list_write_end(830, 1, &mut outer_iostat, std::ptr::null_mut(), 0);
        assert_eq!(outer_iostat, 0, "outer WRITE failed");
        let final_state = with_unit(830, |unit| {
            (
                unit.list_write_active,
                unit.list_write_depth,
                unit.pending_record.is_none(),
            )
        })
        .expect("test unit must remain open");
        assert_eq!(final_state, (false, 0, true));

        afs_close(830, &mut outer_iostat);
        assert_eq!(outer_iostat, 0, "CLOSE failed");
        let marker = 12u32.to_ne_bytes();
        let expected = [
            marker.as_slice(),
            11i32.to_ne_bytes().as_slice(),
            22i32.to_ne_bytes().as_slice(),
            33i32.to_ne_bytes().as_slice(),
            marker.as_slice(),
        ]
        .concat();
        assert_eq!(
            std::fs::read(&path).expect("read nested WRITE output"),
            expected
        );
        std::fs::remove_file(path).expect("remove nested WRITE output");
    }

    #[test]
    fn nested_sequential_unformatted_read_shares_parent_cursor() {
        let path = format!(
            "/tmp/afs_seq_unformatted_nested_read_{}_{}.dat",
            std::process::id(),
            line!()
        );
        let marker = 12u32.to_ne_bytes();
        std::fs::write(
            &path,
            [
                marker.as_slice(),
                11i32.to_ne_bytes().as_slice(),
                22i32.to_ne_bytes().as_slice(),
                33i32.to_ne_bytes().as_slice(),
                marker.as_slice(),
            ]
            .concat(),
        )
        .expect("create nested READ input");

        let mut outer_iostat = -99i32;
        let cb = OpenControlBlock {
            unit: 831,
            filename: path.as_ptr(),
            filename_len: path.len() as i64,
            status: "old".as_ptr(),
            status_len: 3,
            action: "read".as_ptr(),
            action_len: 4,
            access: "sequential".as_ptr(),
            access_len: 10,
            form: "unformatted".as_ptr(),
            form_len: 11,
            recl: 0,
            iostat: &mut outer_iostat,
            newunit: std::ptr::null_mut(),
            position: std::ptr::null(),
            position_len: 0,
            leading_zero: std::ptr::null(),
            leading_zero_len: 0,
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
        };
        afs_open(&cb);
        assert_eq!(outer_iostat, 0, "OPEN failed");

        afs_list_read_begin(831, &mut outer_iostat, std::ptr::null_mut(), 0);
        let mut first = -1i32;
        afs_read_int(831, &mut first, &mut outer_iostat);
        assert_eq!((outer_iostat, first), (0, 11));

        let mut child_iostat = 0i32;
        afs_list_read_begin(831, &mut child_iostat, std::ptr::null_mut(), 0);
        let mut second = -1i32;
        afs_read_int(831, &mut second, &mut child_iostat);
        afs_list_read_end(831, &mut child_iostat, std::ptr::null_mut(), 0);
        assert_eq!((child_iostat, second), (0, 22));
        let nested_state = with_unit(831, |unit| {
            (
                unit.list_read_depth,
                unit.pending_read.as_ref().map(|(_, cursor)| *cursor),
            )
        })
        .expect("test unit must remain open");
        assert_eq!(
            nested_state,
            (1, Some(8)),
            "child READ must preserve the parent record cursor"
        );

        let mut third = -1i32;
        afs_read_int(831, &mut third, &mut outer_iostat);
        afs_list_read_end(831, &mut outer_iostat, std::ptr::null_mut(), 0);
        assert_eq!((outer_iostat, third), (0, 33));
        let final_state = with_unit(831, |unit| {
            (unit.list_read_depth, unit.pending_read.is_none())
        })
        .expect("test unit must remain open");
        assert_eq!(final_state, (0, true));

        afs_close_ex(831, "delete".as_ptr(), 6, &mut outer_iostat);
        assert_eq!(outer_iostat, 0, "CLOSE failed");
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
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
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
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
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
    fn namelist_content_ignores_slashes_inside_character_literals() {
        let text = concat!(
            "&cfg ",
            "single='left/right', ",
            "double=\"up/down\", ",
            "escaped='say ''/'' now', ",
            "value=42 /\n",
        );

        assert_eq!(
            namelist_content(text, "cfg"),
            Some(
                " single='left/right', double=\"up/down\", \
                 escaped='say ''/'' now', value=42 "
            ),
        );
    }

    fn read_internal_namelist_test_entry(
        record: &[u8],
        name: &[u8],
        data: *mut u8,
        data_type: i32,
    ) -> i32 {
        let entry = NamelistEntry {
            name: name.as_ptr(),
            name_len: name.len() as i64,
            data,
            data_type,
            data_len: 0,
            elem_count: 1,
        };
        let mut iostat = -99;
        afs_read_namelist_internal(
            record.as_ptr(),
            record.len() as i64,
            "cfg".as_ptr(),
            3,
            &entry,
            1,
            &mut iostat,
        );
        iostat
    }

    #[test]
    fn namelist_conversion_failures_report_error_without_overwriting_values() {
        let mut integer = 17i32;
        let integer_status = read_internal_namelist_test_entry(
            b"&cfg integer_value=not_an_integer /",
            b"integer_value",
            (&mut integer as *mut i32).cast(),
            0,
        );

        let mut real = 2.5f64;
        let real_status = read_internal_namelist_test_entry(
            b"&cfg real_value=not_a_real /",
            b"real_value",
            (&mut real as *mut f64).cast(),
            1,
        );

        let mut logical = 1i32;
        let logical_status = read_internal_namelist_test_entry(
            b"&cfg logical_value=maybe /",
            b"logical_value",
            (&mut logical as *mut i32).cast(),
            3,
        );

        let mut bool_logical = 1u8;
        let bool_status = read_internal_namelist_test_entry(
            b"&cfg bool_value=perhaps /",
            b"bool_value",
            &mut bool_logical,
            5,
        );

        assert!(
            [integer_status, real_status, logical_status, bool_status]
                .into_iter()
                .all(|status| status != 0),
            "invalid NAMELIST values reported statuses: integer={integer_status}, \
             real={real_status}, logical={logical_status}, bool={bool_status}"
        );
        assert_eq!(integer, 17);
        assert_eq!(real, 2.5);
        assert_eq!(logical, 1);
        assert_eq!(bool_logical, 1);

        let retry_status = read_internal_namelist_test_entry(
            b"&cfg integer_value=42 /",
            b"integer_value",
            (&mut integer as *mut i32).cast(),
            0,
        );
        assert_eq!(retry_status, 0);
        assert_eq!(integer, 42);
    }

    #[test]
    fn namelist_external_read_collects_past_a_quoted_slash() {
        let path = format!(
            "/tmp/afs_namelist_quoted_slash_{}_{}.dat",
            std::process::id(),
            line!()
        );
        std::fs::write(&path, b"&cfg\n path='left/right'\n value=42\n/\n")
            .expect("create quoted-slash NAMELIST input");
        afs_open_simple(
            1794,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let path_name = b"path";
        let value_name = b"value";
        let mut path_value = [b'?'; 16];
        let mut value = -7i32;
        let entries = [
            NamelistEntry {
                name: path_name.as_ptr(),
                name_len: path_name.len() as i64,
                data: path_value.as_mut_ptr(),
                data_type: 2,
                data_len: path_value.len() as i64,
                elem_count: 1,
            },
            NamelistEntry {
                name: value_name.as_ptr(),
                name_len: value_name.len() as i64,
                data: (&mut value as *mut i32).cast(),
                data_type: 0,
                data_len: 0,
                elem_count: 1,
            },
        ];
        let mut iostat = -99;

        afs_read_namelist(
            1794,
            "cfg".as_ptr(),
            3,
            entries.as_ptr(),
            entries.len() as i32,
            &mut iostat,
        );

        assert_eq!(iostat, 0);
        assert_eq!(
            std::str::from_utf8(&path_value).unwrap().trim_end(),
            "left/right"
        );
        assert_eq!(value, 42);
        afs_close_ex(1794, "delete".as_ptr(), 6, &mut iostat);
        assert_eq!(iostat, 0);
    }

    #[test]
    fn namelist_write_assigns_first_error_to_iomsg() {
        let path = format!(
            "/tmp/afs_namelist_write_iomsg_{}_{}.dat",
            std::process::id(),
            line!()
        );
        std::fs::write(&path, b"").unwrap();
        afs_open_simple(
            1793,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let mut iostat = -99;
        let mut iomsg = [b'?'; 64];
        afs_write_namelist(
            1793,
            "group".as_ptr(),
            5,
            std::ptr::null(),
            0,
            &mut iostat,
            iomsg.as_mut_ptr(),
            iomsg.len() as i64,
        );

        assert_ne!(iostat, 0);
        assert_eq!(
            std::str::from_utf8(&iomsg).unwrap().trim_end(),
            "unit not open for writing"
        );
        afs_close_ex(1793, "delete".as_ptr(), 6, &mut iostat);
        assert_eq!(iostat, 0);
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
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
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
        let path = format!("/tmp/afs_lz_conn_{}_{}.txt", std::process::id(), line!());
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
            iomsg: std::ptr::null_mut(),
            iomsg_len: 0,
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
    fn internal_write_reallocates_deferred_length_target_to_record_length() {
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

        // Already allocated (len 7): F2023 §12.4 reallocates to the new record
        // length. Writing 'x' (1 char) shrinks len to 1 — it is NOT a fixed
        // internal file that pads to the old length.
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
        assert_eq!(desc.len, 1);
        let bytes = unsafe { std::slice::from_raw_parts(desc.data, desc.len as usize) };
        assert_eq!(bytes, b"x");

        // Re-grow past the current length: 'hello, world #100' (17 chars).
        afs_fmt_begin_internal_alloc(
            dptr,
            "(A,I0)".as_ptr(),
            6,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        afs_fmt_push_string("hello, world #".as_ptr(), 14);
        afs_fmt_push_int(100);
        afs_fmt_end(0);
        assert_eq!(desc.len, 17);
        let bytes = unsafe { std::slice::from_raw_parts(desc.data, desc.len as usize) };
        assert_eq!(bytes, b"hello, world #100");

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
    fn list_directed_integer_kinds_use_gfortran_widths() {
        let path = format!("/tmp/afs_list_widths_{}.dat", std::process::id());
        let expected = format!(
            "{:>5}{:>7}{:>12}{:>21}{:>41}\n",
            0i8, 0i16, 0i32, 0i64, 0i128
        );

        afs_open_simple(
            909,
            path.as_ptr(),
            path.len() as i64,
            "replace".as_ptr(),
            7,
            std::ptr::null(),
            0,
        );
        afs_write_int8(909, 0);
        afs_write_int16(909, 0);
        afs_write_int(909, 0);
        afs_write_int64(909, 0);
        afs_write_int128(909, 0);
        afs_write_newline(909);
        afs_close(909, std::ptr::null_mut());

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, expected);
        let _ = std::fs::remove_file(path);
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
    fn internal_list_directed_integer_kinds_use_gfortran_widths() {
        let expected = format!("{:>5}{:>7}{:>12}{:>21}{:>41}", 0i8, 0i16, 0i32, 0i64, 0i128);
        let mut buf = [b'.'; 100];
        let mut write_pos = 0i64;

        afs_write_internal_int8(buf.as_mut_ptr(), buf.len() as i64, 0, &mut write_pos);
        afs_write_internal_int16(buf.as_mut_ptr(), buf.len() as i64, 0, &mut write_pos);
        afs_write_internal_int(buf.as_mut_ptr(), buf.len() as i64, 0, &mut write_pos);
        afs_write_internal_int64(buf.as_mut_ptr(), buf.len() as i64, 0, &mut write_pos);
        afs_write_internal_int128(buf.as_mut_ptr(), buf.len() as i64, 0, &mut write_pos);

        assert_eq!(write_pos as usize, expected.len());
        assert_eq!(&buf[..expected.len()], expected.as_bytes());
        assert_eq!(buf[expected.len()], b' ');
    }

    #[test]
    fn internal_list_directed_logicals_use_letter_fields() {
        let mut buf = [b'.'; 8];
        let mut write_pos = 0i64;

        afs_write_internal_logical(buf.as_mut_ptr(), buf.len() as i64, 1, &mut write_pos);
        afs_write_internal_logical(buf.as_mut_ptr(), buf.len() as i64, 0, &mut write_pos);

        assert_eq!(write_pos, 4);
        assert_eq!(&buf[..4], b" T F");
        assert_eq!(buf[4], b' ');
    }

    #[test]
    fn deferred_internal_list_write_collects_logical_fields() {
        use crate::descriptor::StringDescriptor;

        let mut desc = StringDescriptor::zeroed();
        let desc_ptr = &mut desc as *mut StringDescriptor as *mut u8;
        let mut iostat = 77;

        afs_lst_ia_begin(desc_ptr, &mut iostat, std::ptr::null_mut(), 0);
        afs_lst_ia_logical(1);
        afs_lst_ia_logical(0);
        afs_lst_ia_end();

        assert_eq!(iostat, 0);
        assert_eq!(desc.len, 4);
        let bytes = unsafe { std::slice::from_raw_parts(desc.data, desc.len as usize) };
        assert_eq!(bytes, b" T F");

        crate::string::afs_dealloc_string(desc_ptr as *mut StringDescriptor);
    }

    #[test]
    fn fixed_internal_list_overflow_restores_target() {
        let mut buf = *b"???";
        let mut iostat = 77;
        let mut iomsg = [b'?'; 32];
        let mut pos = 0;
        let value = b"abcdef";

        afs_lst_begin_internal_fixed(
            buf.as_mut_ptr(),
            buf.len() as i64,
            1,
            &mut iostat,
            iomsg.as_mut_ptr(),
            iomsg.len() as i64,
        );
        afs_write_internal_string(
            buf.as_mut_ptr(),
            buf.len() as i64,
            value.as_ptr(),
            value.len() as i64,
            &mut pos,
        );
        afs_lst_end_internal_fixed();

        assert_eq!(buf, *b"???");
        assert_eq!(iostat, IOSTAT_EOR);
        assert_eq!(&iomsg[..13], b"end of record");
        assert!(iomsg[13..].iter().all(|byte| *byte == b' '));
    }

    #[test]
    fn fixed_internal_list_zero_length_items_overflow() {
        let mut buf = [];
        let mut iostat = 77;
        let mut pos = 0;

        afs_lst_begin_internal_fixed(buf.as_mut_ptr(), 0, 1, &mut iostat, std::ptr::null_mut(), 0);
        afs_write_internal_int(buf.as_mut_ptr(), 0, 1, &mut pos);
        afs_lst_end_internal_fixed();
        assert_eq!(iostat, IOSTAT_EOR);

        iostat = 77;
        afs_lst_begin_internal_fixed(buf.as_mut_ptr(), 0, 1, &mut iostat, std::ptr::null_mut(), 0);
        afs_write_internal_real64(buf.as_mut_ptr(), 0, 1.0, &mut pos);
        afs_lst_end_internal_fixed();
        assert_eq!(iostat, IOSTAT_EOR);

        iostat = 77;
        afs_lst_begin_internal_fixed(buf.as_mut_ptr(), 0, 1, &mut iostat, std::ptr::null_mut(), 0);
        afs_write_internal_string(buf.as_mut_ptr(), 0, b"a".as_ptr(), 1, &mut pos);
        afs_lst_end_internal_fixed();
        assert_eq!(iostat, IOSTAT_EOR);
    }

    #[test]
    fn fixed_internal_list_rejects_zero_record_array() {
        let mut buf = *b"???";
        let mut iostat = 77;
        let mut iomsg = [b'?'; 80];
        let mut pos = 0;

        afs_lst_begin_internal_fixed(
            buf.as_mut_ptr(),
            buf.len() as i64,
            0,
            &mut iostat,
            iomsg.as_mut_ptr(),
            iomsg.len() as i64,
        );
        afs_write_internal_string(
            buf.as_mut_ptr(),
            buf.len() as i64,
            b"a".as_ptr(),
            1,
            &mut pos,
        );
        afs_lst_end_internal_fixed();

        assert_eq!(buf, *b"???");
        assert_eq!(iostat, 1);
        assert_eq!(
            std::str::from_utf8(&iomsg).unwrap().trim_end(),
            "internal WRITE to an unallocated or zero-size character array"
        );
    }

    #[test]
    fn fixed_internal_list_contexts_are_nested() {
        let mut outer = *b"????";
        let mut inner = *b"!!";
        let mut outer_iostat = 77;
        let mut inner_iostat = 77;
        let mut outer_pos = 0;
        let mut inner_pos = 0;

        afs_lst_begin_internal_fixed(
            outer.as_mut_ptr(),
            outer.len() as i64,
            1,
            &mut outer_iostat,
            std::ptr::null_mut(),
            0,
        );
        afs_lst_begin_internal_fixed(
            inner.as_mut_ptr(),
            inner.len() as i64,
            1,
            &mut inner_iostat,
            std::ptr::null_mut(),
            0,
        );
        afs_write_internal_string(
            inner.as_mut_ptr(),
            inner.len() as i64,
            b"abc".as_ptr(),
            3,
            &mut inner_pos,
        );
        afs_lst_end_internal_fixed();
        afs_write_internal_string(
            outer.as_mut_ptr(),
            outer.len() as i64,
            b"x".as_ptr(),
            1,
            &mut outer_pos,
        );
        afs_lst_end_internal_fixed();

        assert_eq!(inner, *b"!!");
        assert_eq!(inner_iostat, IOSTAT_EOR);
        assert_eq!(outer, *b" x  ");
        assert_eq!(outer_iostat, 0);
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
        afs_fmt_push_real(1.23);
        afs_fmt_end(1); // with newline

        afs_close(99, std::ptr::null_mut());

        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("42"), "expected 42 in: {}", content);
        assert!(content.contains("1.23"), "expected 1.23 in: {}", content);
    }

    #[test]
    fn malformed_dynamic_format_sets_write_status_without_touching_target() {
        let mut buffer = [b'?'; 16];
        let mut iostat = -99;
        let mut iomsg = [b'?'; 32];
        let format = "(F8)";

        afs_fmt_begin_internal_ex(
            buffer.as_mut_ptr(),
            buffer.len() as i64,
            format.as_ptr(),
            format.len() as i64,
            &mut iostat,
            iomsg.as_mut_ptr(),
            iomsg.len() as i64,
        );
        afs_fmt_push_real(1.25);
        afs_fmt_end(0);

        assert_eq!(iostat, 1);
        assert_eq!(buffer, [b'?'; 16]);
        assert_eq!(
            std::str::from_utf8(&iomsg).unwrap().trim_end(),
            "invalid format"
        );
    }

    #[test]
    fn malformed_dynamic_format_sets_read_status_without_touching_value() {
        let input = b" 42";
        let format = "(I)";
        let mut value = 1234;
        let mut iostat = -99;

        afs_fmt_read_int_internal(
            input.as_ptr(),
            input.len() as i64,
            format.as_ptr(),
            format.len() as i64,
            0,
            &mut value,
            &mut iostat,
        );

        assert_eq!(iostat, 1);
        assert_eq!(value, 1234);
    }

    #[test]
    fn malformed_dynamic_format_does_not_consume_external_input() {
        let path = "/tmp/afs_malformed_dynamic_format_read.dat";
        std::fs::write(path, " 42\n").unwrap();
        afs_open_simple(
            829,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let mut value = 1234;
        let mut iostat = -99;
        let malformed = "(I)";
        afs_fmt_read_int(
            829,
            malformed.as_ptr(),
            malformed.len() as i64,
            0,
            &mut value,
            &mut iostat,
        );
        assert_eq!((value, iostat), (1234, 1));

        let valid = "(I3)";
        afs_fmt_read_int(
            829,
            valid.as_ptr(),
            valid.len() as i64,
            0,
            &mut value,
            &mut iostat,
        );
        assert_eq!((value, iostat), (42, 0));

        afs_close(829, std::ptr::null_mut());
        let _ = std::fs::remove_file(path);
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
        let format = "(I40,1X,I4)";
        afs_fmt_read_int128(
            93,
            format.as_ptr(),
            format.len() as i64,
            0,
            &mut first,
            &mut iostat,
        );
        assert_eq!(iostat, 0);

        afs_fmt_read_int(
            93,
            format.as_ptr(),
            format.len() as i64,
            1,
            &mut second,
            &mut iostat,
        );
        afs_close(93, std::ptr::null_mut());

        assert_eq!(iostat, 0);
        assert_eq!(first, 170141183460469231731687303715884105727i128);
        assert_eq!(second, 42);
    }

    #[test]
    fn formatted_unit_read_reverts_to_rightmost_group_and_preserves_state() {
        let path = "/tmp/afs_fmt_read_reversion_group_test.dat";
        std::fs::write(path, " 1 2\n3 4\n").unwrap();

        afs_open_simple(
            91,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let format = "(BZ,1X,(F3.0))";
        let mut first = -1.0;
        let mut second = -1.0;
        let mut iostat = -99;
        afs_fmt_read_real(
            91,
            format.as_ptr(),
            format.len() as i64,
            0,
            &mut first,
            &mut iostat,
        );
        assert_eq!(iostat, 0);
        afs_fmt_read_real(
            91,
            format.as_ptr(),
            format.len() as i64,
            1,
            &mut second,
            &mut iostat,
        );
        afs_close(91, std::ptr::null_mut());
        let _ = std::fs::remove_file(path);

        assert_eq!(iostat, 0);
        assert_eq!(first, 102.0);
        assert_eq!(second, 304.0);
    }

    #[test]
    fn formatted_unit_read_reversion_reports_missing_next_record() {
        let path = "/tmp/afs_fmt_read_reversion_eof_test.dat";
        std::fs::write(path, "01\n02\n").unwrap();

        afs_open_simple(
            90,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let format = "(I2)";
        let mut values = [-1; 3];
        let mut iostat = -99;
        for (index, value) in values.iter_mut().enumerate() {
            afs_fmt_read_int(
                90,
                format.as_ptr(),
                format.len() as i64,
                index as i64,
                value,
                &mut iostat,
            );
            if index < 2 {
                assert_eq!(iostat, 0);
            }
        }
        afs_close(90, std::ptr::null_mut());
        let _ = std::fs::remove_file(path);

        assert_eq!(values, [1, 2, -1]);
        assert_eq!(iostat, IOSTAT_END);
    }

    #[test]
    fn formatted_unit_read_unlimited_repeat_stays_on_one_record() {
        let path = "/tmp/afs_fmt_read_unlimited_test.dat";
        std::fs::write(path, "010203\n").unwrap();

        afs_open_simple(
            89,
            path.as_ptr(),
            path.len() as i64,
            "old".as_ptr(),
            3,
            "read".as_ptr(),
            4,
        );

        let format = "(*(I2))";
        let mut values = [-1; 3];
        let mut iostat = -99;
        for (index, value) in values.iter_mut().enumerate() {
            afs_fmt_read_int(
                89,
                format.as_ptr(),
                format.len() as i64,
                index as i64,
                value,
                &mut iostat,
            );
            assert_eq!(iostat, 0);
        }
        afs_close(89, std::ptr::null_mut());
        let _ = std::fs::remove_file(path);

        assert_eq!(values, [1, 2, 3]);
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
        let mut first_size = 0i32;
        let mut second_size = 0i32;
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
        let mut first_size = 0i32;
        let mut second_size = 0i32;
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
        let mut size = 0i32;
        let mut iostat = -99i32;
        for (index, expected) in [b'A', 0, b'B'].into_iter().enumerate() {
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
            assert_eq!(size, (index + 1) as i32);
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
        assert_eq!(size, 3);
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
