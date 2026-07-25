//! Fortran-aware C-style preprocessor.
//!
//! Text-to-text transformation that runs before lexing. Handles #define,
//! #ifdef/#ifndef/#if/#elif/#else/#endif, #include, Fortran INCLUDE, #undef,
//! #error, #warning, #line/GNU linemarkers, null directives, and #pragma.
//! Aware of Fortran string literals and comments — won't expand macros inside them.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::lexer::{Position, Span};

/// Configuration for the preprocessor.
#[derive(Debug, Clone)]
pub struct PreprocConfig {
    /// Predefined macros from -D flags and built-in definitions.
    pub defines: HashMap<String, MacroDef>,
    /// Include search paths from -I flags.
    pub include_paths: Vec<PathBuf>,
    /// The source filename (for __FILE__ and error messages).
    pub filename: String,
    /// If true, source is fixed-form Fortran (C/* in column 1 = comment).
    pub fixed_form: bool,
    /// If true, strip C block comments from source lines for -cpp compatibility.
    pub cpp_compat: bool,
}

impl PreprocConfig {
    /// Predefined macros for a target.
    pub fn for_target(target: &crate::target::TargetSpec) -> Self {
        use crate::target::{Arch, Os};

        let mut defines = HashMap::new();
        defines.insert("__ARMFORTAS__".into(), MacroDef::object("1"));
        defines.insert("__ARMFORTAS_MAJOR__".into(), MacroDef::object("0"));
        defines.insert("__ARMFORTAS_MINOR__".into(), MacroDef::object("1"));
        // Build systems such as CMake key GNU-compatible Fortran
        // behavior off these predefines before they know our compiler
        // name. Advertise the build-system compatibility surface, not
        // the armfortas release version: GCC 10 is new enough to avoid
        // legacy project gates/workarounds while keeping CMake on the
        // GNU module-dir and dependency-file paths we intentionally
        // accept in the driver.
        defines.insert("__GNUC__".into(), MacroDef::object("10"));
        defines.insert("__GNUC_MINOR__".into(), MacroDef::object("0"));
        defines.insert("__GNUC_PATCHLEVEL__".into(), MacroDef::object("0"));
        match target.arch {
            Arch::Arm64 => {
                defines.insert("__aarch64__".into(), MacroDef::object("1"));
                defines.insert("__arm64__".into(), MacroDef::object("1"));
            }
            Arch::X86_64 => {
                defines.insert("__x86_64__".into(), MacroDef::object("1"));
                defines.insert("__amd64__".into(), MacroDef::object("1"));
            }
        }
        match target.os {
            Os::MacOs => {
                defines.insert("__APPLE__".into(), MacroDef::object("1"));
            }
            Os::FreeBsd => {
                // System compilers define __FreeBSD__ to the OS major
                // version. TargetSpec carries no OS version, so we define 1;
                // revisit in x06 if a campaign project version-checks it.
                defines.insert("__FreeBSD__".into(), MacroDef::object("1"));
            }
            Os::Linux => {
                defines.insert("__linux__".into(), MacroDef::object("1"));
            }
        }

        Self {
            defines,
            include_paths: Vec::new(),
            filename: "<input>".into(),
            fixed_form: false,
            cpp_compat: false,
        }
    }
}

impl Default for PreprocConfig {
    fn default() -> Self {
        Self::for_target(&crate::target::TargetSpec::host())
    }
}

/// A macro definition.
#[derive(Debug, Clone)]
pub struct MacroDef {
    /// For object-like macros: the replacement text.
    /// For function-like macros: the replacement text with parameter placeholders.
    pub body: String,
    /// Parameter names (empty for object-like macros).
    pub params: Vec<String>,
    /// Whether this is a function-like macro.
    pub is_function: bool,
    /// Whether this is a variadic macro (accepts `...` / `__VA_ARGS__`).
    pub is_variadic: bool,
}

impl MacroDef {
    pub fn object(body: &str) -> Self {
        Self {
            body: body.into(),
            params: Vec::new(),
            is_function: false,
            is_variadic: false,
        }
    }

    pub fn function(params: Vec<String>, body: &str) -> Self {
        Self {
            body: body.into(),
            params,
            is_function: true,
            is_variadic: false,
        }
    }
}

/// Preprocessor output.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PreprocOutput {
    /// The preprocessed text.
    pub text: String,
    /// Maps output line numbers (1-based) to original (filename, line) pairs.
    pub source_map: Vec<SourceLoc>,
    /// Resolved include files in first-seen order, including nested includes.
    pub included_files: Vec<PathBuf>,
    source_view: bool,
    eof_origin: SourceOrigin,
}

/// A reported source line before preprocessing.
///
/// This is an output record; its private provenance data intentionally keeps
/// callers from constructing locations that cannot resolve diagnostics.
#[derive(Clone)]
#[non_exhaustive]
pub struct SourceLoc {
    pub filename: String,
    pub line: u32,
    text: String,
    source: Arc<SourceFile>,
    source_line: u32,
    source_col: u32,
    runs: Vec<SourceRun>,
}

struct SourceFile {
    filename: Arc<str>,
    text: String,
    display_text: OnceLock<String>,
    line_starts: Vec<usize>,
    source_view: bool,
}

impl SourceFile {
    fn new(filename: Arc<str>, mut text: String, source_view: bool) -> Self {
        // Normalize the stream marker before physical-line dispatch and source mapping.
        if text.starts_with('\u{feff}') {
            text.drain(..'\u{feff}'.len_utf8());
        }
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset + 1)),
        );
        Self {
            filename,
            text,
            display_text: OnceLock::new(),
            line_starts,
            source_view,
        }
    }

    fn display_text(&self) -> &str {
        if self.source_view {
            self.display_text
                .get_or_init(|| crate::source_bytes::display_source_view(&self.text))
        } else {
            &self.text
        }
    }

    fn line(&self, line: u32) -> Option<&str> {
        let index = line.checked_sub(1)? as usize;
        let start = *self.line_starts.get(index)?;
        let end = self
            .line_starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.text.len());
        Some(self.text[start..end].trim_end_matches(['\r', '\n']))
    }

    fn display_col(&self, line: u32, source_col: u32) -> u32 {
        let Some(line) = self.line(line) else {
            return source_col;
        };
        let source_offset = source_col.saturating_sub(1) as usize;
        let display_offset = if self.source_view {
            crate::source_bytes::display_column(line, source_offset)
        } else {
            line.char_indices()
                .take_while(|(offset, _)| *offset < source_offset)
                .count()
        };
        display_offset.saturating_add(1).min(u32::MAX as usize) as u32
    }
}

impl fmt::Debug for SourceFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceFile")
            .field("filename", &self.filename)
            .field("text_len", &self.text.len())
            .finish()
    }
}

impl fmt::Debug for SourceLoc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceLoc")
            .field("filename", &self.filename)
            .field("line", &self.line)
            .finish()
    }
}

#[derive(Debug, Clone)]
struct SourceOrigin {
    source: Arc<SourceFile>,
    source_line: u32,
    source_col: u32,
    filename: Arc<str>,
    line: u32,
}

impl SourceOrigin {
    fn advanced(&self, source_bytes: usize) -> Self {
        let mut origin = self.clone();
        origin.source_col = origin.source_col.saturating_add(source_bytes as u32);
        origin
    }
}

fn source_text_width(text: &str, range: Range<usize>, source_view: bool) -> usize {
    if source_view {
        crate::source_bytes::source_byte_range_len(text, range)
    } else {
        range.end.saturating_sub(range.start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceRunKind {
    Linear,
    Anchor,
}

#[derive(Debug, Clone)]
struct SourceRun {
    output: Range<usize>,
    source_len: usize,
    origin: SourceOrigin,
    kind: SourceRunKind,
}

fn source_run_prefix_width(text: &str, run: &SourceRun, end: usize) -> usize {
    let end = end.max(run.output.start).min(run.output.end);
    let encoded_len = end - run.output.start;
    if run.source_len == run.output.len() {
        encoded_len
    } else {
        source_text_width(text, run.output.start..end, run.origin.source.source_view)
    }
}

#[derive(Debug, Clone)]
struct MappedText {
    text: String,
    runs: Vec<SourceRun>,
    fallback: SourceOrigin,
}

impl MappedText {
    fn empty(fallback: SourceOrigin) -> Self {
        Self {
            text: String::new(),
            runs: Vec::new(),
            fallback,
        }
    }

    fn source_line(text: &str, origin: SourceOrigin) -> Self {
        let mut mapped = Self::empty(origin.clone());
        mapped.push_text(text, origin, SourceRunKind::Linear);
        mapped
    }

    fn anchored(text: &str, origin: SourceOrigin) -> Self {
        let mut mapped = Self::empty(origin.clone());
        mapped.push_text(text, origin, SourceRunKind::Anchor);
        mapped
    }

    fn append(&mut self, other: &Self) {
        self.append_slice(other, 0..other.text.len());
    }

    fn append_slice(&mut self, other: &Self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let base = self.text.len();
        self.text.push_str(&other.text[range.clone()]);
        let first = other
            .runs
            .partition_point(|run| run.output.end <= range.start);
        for run in &other.runs[first..] {
            if run.output.start >= range.end {
                break;
            }
            let start = run.output.start.max(range.start);
            let end = run.output.end.min(range.end);
            if start >= end {
                continue;
            }
            let origin = match run.kind {
                SourceRunKind::Linear => run.origin.advanced(source_text_width(
                    &other.text,
                    run.output.start..start,
                    run.origin.source.source_view,
                )),
                SourceRunKind::Anchor => run.origin.clone(),
            };
            self.push_run(SourceRun {
                output: base + start - range.start..base + end - range.start,
                source_len: source_text_width(
                    &other.text,
                    start..end,
                    run.origin.source.source_view,
                ),
                origin,
                kind: run.kind,
            });
        }
    }

    fn push_text(&mut self, text: &str, origin: SourceOrigin, kind: SourceRunKind) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.push_run(SourceRun {
            output: start..self.text.len(),
            source_len: source_text_width(
                &self.text,
                start..self.text.len(),
                origin.source.source_view,
            ),
            origin,
            kind,
        });
    }

    fn push_run(&mut self, run: SourceRun) {
        if let Some(previous) = self.runs.last() {
            let contiguous_origin = match run.kind {
                SourceRunKind::Linear => {
                    previous
                        .origin
                        .source_col
                        .saturating_add(previous.source_len as u32)
                        == run.origin.source_col
                }
                SourceRunKind::Anchor => previous.origin.source_col == run.origin.source_col,
            };
            if previous.output.end == run.output.start
                && previous.kind == run.kind
                && Arc::ptr_eq(&previous.origin.source, &run.origin.source)
                && previous.origin.source_line == run.origin.source_line
                && previous.origin.filename == run.origin.filename
                && previous.origin.line == run.origin.line
                && contiguous_origin
            {
                let previous = self
                    .runs
                    .last_mut()
                    .expect("the previous source run disappeared");
                previous.output.end = run.output.end;
                previous.source_len += run.source_len;
                return;
            }
        }
        self.runs.push(run);
    }

    fn truncate(&mut self, len: usize) {
        self.text.truncate(len);
        let keep = self.runs.partition_point(|run| run.output.start < len);
        self.runs.truncate(keep);
        if let Some(last) = self.runs.last_mut() {
            last.output.end = last.output.end.min(len);
            last.source_len = source_text_width(
                &self.text,
                last.output.clone(),
                last.origin.source.source_view,
            );
        }
    }

    fn slice(&self, range: Range<usize>) -> Self {
        let fallback = self.origin_at(range.start);
        let mut mapped = Self::empty(fallback);
        mapped.append_slice(self, range);
        mapped
    }

    fn origin_at(&self, offset: usize) -> SourceOrigin {
        let run = source_run_at_or_before(&self.runs, offset);
        let Some(run) = run else {
            return self.fallback.clone();
        };
        match run.kind {
            SourceRunKind::Linear => run
                .origin
                .advanced(source_run_prefix_width(&self.text, run, offset)),
            SourceRunKind::Anchor => run.origin.clone(),
        }
    }

    fn into_source_loc(self) -> SourceLoc {
        SourceLoc {
            filename: self.fallback.filename.to_string(),
            line: self.fallback.line,
            text: self.text,
            source: self.fallback.source,
            source_line: self.fallback.source_line,
            source_col: self.fallback.source_col,
            runs: self.runs,
        }
    }
}

struct ResolvedPoint<'a> {
    filename: &'a str,
    source: &'a SourceFile,
    line: u32,
    col: u32,
    source_line: u32,
    source_col: u32,
}

impl SourceLoc {
    fn origin_at(&self, offset: usize) -> SourceOrigin {
        if let Some(run) = source_run_at_or_before(&self.runs, offset) {
            return match run.kind {
                SourceRunKind::Linear => run
                    .origin
                    .advanced(source_run_prefix_width(&self.text, run, offset)),
                SourceRunKind::Anchor => run.origin.clone(),
            };
        }
        let offset = offset.min(self.text.len());
        let delta = source_text_width(&self.text, 0..offset, self.source.source_view) as u32;
        SourceOrigin {
            source: self.source.clone(),
            source_line: self.source_line,
            source_col: self.source_col.saturating_add(delta),
            filename: Arc::from(self.filename.as_str()),
            line: self.line,
        }
    }

    fn resolve(&self, col: u32) -> ResolvedPoint<'_> {
        let offset = col.saturating_sub(1) as usize;
        let run = source_run_at_or_before(&self.runs, offset);
        if let Some(run) = run {
            let delta = match run.kind {
                SourceRunKind::Linear => source_run_prefix_width(&self.text, run, offset),
                SourceRunKind::Anchor => 0,
            };
            return ResolvedPoint {
                filename: &run.origin.filename,
                source: &run.origin.source,
                line: run.origin.line,
                col: run.origin.source_col.saturating_add(delta as u32),
                source_line: run.origin.source_line,
                source_col: run.origin.source_col.saturating_add(delta as u32),
            };
        }
        let offset = offset.min(self.text.len());
        let delta = source_text_width(&self.text, 0..offset, self.source.source_view) as u32;
        ResolvedPoint {
            filename: &self.filename,
            source: &self.source,
            line: self.line,
            col: self.source_col.saturating_add(delta),
            source_line: self.source_line,
            source_col: self.source_col.saturating_add(delta),
        }
    }

    fn resolve_end(&self, col: u32) -> ResolvedPoint<'_> {
        let exclusive = col.saturating_sub(1) as usize;
        let Some(last_byte) = exclusive.checked_sub(1) else {
            return self.resolve(col);
        };
        let run = source_run_at_or_before(&self.runs, last_byte);
        if let Some(run) = run {
            let delta = match run.kind {
                SourceRunKind::Linear => source_run_prefix_width(&self.text, run, exclusive),
                SourceRunKind::Anchor => 1,
            };
            return ResolvedPoint {
                filename: &run.origin.filename,
                source: &run.origin.source,
                line: run.origin.line,
                col: run.origin.source_col.saturating_add(delta as u32),
                source_line: run.origin.source_line,
                source_col: run.origin.source_col.saturating_add(delta as u32),
            };
        }
        let exclusive = exclusive.min(self.text.len());
        let delta = source_text_width(&self.text, 0..exclusive, self.source.source_view) as u32;
        ResolvedPoint {
            filename: &self.filename,
            source: &self.source,
            line: self.line,
            col: self.source_col.saturating_add(delta),
            source_line: self.source_line,
            source_col: self.source_col.saturating_add(delta),
        }
    }
}

fn source_run_at_or_before(runs: &[SourceRun], offset: usize) -> Option<&SourceRun> {
    let next = runs.partition_point(|run| run.output.end <= offset);
    runs.get(next)
        .filter(|run| run.output.contains(&offset))
        .or_else(|| next.checked_sub(1).and_then(|index| runs.get(index)))
}

pub(crate) struct ResolvedSpan<'a> {
    pub filename: &'a str,
    pub source: &'a str,
    pub display_span: Span,
    pub source_span: Span,
}

impl PreprocOutput {
    pub(crate) fn bytes(&self) -> Vec<u8> {
        if self.source_view {
            crate::source_bytes::from_source_view(&self.text)
        } else {
            self.text.as_bytes().to_vec()
        }
    }

    pub(crate) fn resolve_span(&self, span: Span) -> ResolvedSpan<'_> {
        let location = span
            .start
            .line
            .checked_sub(1)
            .and_then(|line| self.source_map.get(line as usize));

        let Some(location) = location else {
            let start = ResolvedPoint {
                filename: &self.eof_origin.filename,
                source: &self.eof_origin.source,
                line: self.eof_origin.line,
                col: self.eof_origin.source_col,
                source_line: self.eof_origin.source_line,
                source_col: self.eof_origin.source_col,
            };
            return resolved_span_from_points(span.file_id, start, None);
        };

        let start = location.resolve(span.start.col);
        let end = if span.end.line == span.start.line && span.end.col > span.start.col {
            Some(location.resolve_end(span.end.col))
        } else {
            None
        };
        resolved_span_from_points(span.file_id, start, end)
    }
}

fn resolved_span_from_points<'a>(
    file_id: u32,
    start: ResolvedPoint<'a>,
    end: Option<ResolvedPoint<'a>>,
) -> ResolvedSpan<'a> {
    let same_origin = end.as_ref().is_some_and(|end| {
        std::ptr::eq(start.source, end.source)
            && start.filename == end.filename
            && start.line == end.line
            && start.source_line == end.source_line
    });
    let display_end = if same_origin {
        let end = end.as_ref().unwrap();
        Position {
            line: end.line,
            col: end.col.max(start.col.saturating_add(1)),
        }
    } else {
        Position {
            line: start.line,
            col: start.col.saturating_add(1),
        }
    };
    let source_start_col = start
        .source
        .display_col(start.source_line, start.source_col);
    let source_end = if same_origin {
        let end = end.as_ref().unwrap();
        let end_col = end.source.display_col(end.source_line, end.source_col);
        Position {
            line: end.source_line,
            col: end_col.max(source_start_col.saturating_add(1)),
        }
    } else {
        Position {
            line: start.source_line,
            col: source_start_col.saturating_add(1),
        }
    };

    ResolvedSpan {
        filename: start.filename,
        source: start.source.display_text(),
        display_span: Span {
            file_id,
            start: Position {
                line: start.line,
                col: start.col,
            },
            end: display_end,
        },
        source_span: Span {
            file_id,
            start: Position {
                line: start.source_line,
                col: source_start_col,
            },
            end: source_end,
        },
    }
}

/// Preprocessor error.
#[derive(Debug, Clone)]
pub struct PreprocError {
    pub filename: String,
    pub line: u32,
    pub msg: String,
}

impl fmt::Display for PreprocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: error: {}", self.filename, self.line, self.msg)
    }
}

impl std::error::Error for PreprocError {}

/// Preprocess Fortran source text with given configuration.
pub fn preprocess(source: &str, config: &PreprocConfig) -> Result<PreprocOutput, PreprocError> {
    let mut pp = Preprocessor::new(config, true, false);
    pp.process(source, &config.filename)
}

pub(crate) fn preprocess_bytes(
    source: &[u8],
    config: &PreprocConfig,
) -> Result<PreprocOutput, PreprocError> {
    let mut pp = Preprocessor::new(config, true, true);
    let source = crate::source_bytes::to_source_view(source);
    pp.process(&source, &config.filename)
}

/// Preprocess for dependency discovery without emitting user-facing warnings.
pub(crate) fn preprocess_for_dependency_scan(
    source: &str,
    config: &PreprocConfig,
) -> Result<PreprocOutput, PreprocError> {
    let mut pp = Preprocessor::new(config, false, false);
    pp.process(source, &config.filename)
}

pub(crate) fn preprocess_bytes_for_dependency_scan(
    source: &[u8],
    config: &PreprocConfig,
) -> Result<PreprocOutput, PreprocError> {
    let mut pp = Preprocessor::new(config, false, true);
    let source = crate::source_bytes::to_source_view(source);
    pp.process(&source, &config.filename)
}

#[derive(Debug, Clone)]
struct ReportedLocation {
    filename: Arc<str>,
    line: u32,
}

impl ReportedLocation {
    fn advance(&mut self, physical_lines: usize) {
        self.line = self.line.saturating_add(physical_lines as u32);
    }

    fn at_physical_offset(&self, physical_lines: u32) -> Self {
        Self {
            filename: self.filename.clone(),
            line: self.line.saturating_add(physical_lines),
        }
    }
}

struct ProcessEnd {
    next: ReportedLocation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MacroContext {
    Source,
    Condition,
}

/// Condition stack state for nested #if/#ifdef blocks.
#[derive(Debug, Clone, Copy)]
enum CondState {
    /// Currently in a true branch, emitting output.
    Active,
    /// No arm has matched yet, so the current arm is skipped.
    Skipping,
    /// A prior arm matched, so later alternatives are skipped.
    Done,
    /// Parent was skipping, so everything at this level is skipped regardless.
    ParentSkipping,
}

#[derive(Debug, Clone, Copy)]
struct ConditionalFrame {
    state: CondState,
    seen_else: bool,
}

struct Preprocessor {
    defines: HashMap<String, MacroDef>,
    include_paths: Vec<PathBuf>,
    cond_stack: Vec<ConditionalFrame>,
    /// O(1) counter: number of non-Active levels on the stack.
    /// `is_emitting()` is just `skip_depth == 0`.
    skip_depth: u32,
    /// Include depth for recursion guard.
    include_depth: u32,
    /// Fixed-form source mode.
    fixed_form: bool,
    /// GNU-style preprocessing compatibility mode.
    cpp_compat: bool,
    /// Whether active #warning directives should be printed.
    emit_warnings: bool,
    /// Whether text uses the reversible source-byte representation.
    source_view: bool,
    /// Whether source stripping is currently inside a C-style block comment.
    in_c_block_comment: bool,
    included_files: Vec<PathBuf>,
}

impl Preprocessor {
    fn new(config: &PreprocConfig, emit_warnings: bool, source_view: bool) -> Self {
        let defines = config
            .defines
            .iter()
            .map(|(name, definition)| {
                let mut definition = definition.clone();
                if source_view {
                    definition.body = crate::source_bytes::escape_utf8(&definition.body);
                }
                (name.clone(), definition)
            })
            .collect();
        Self {
            defines,
            include_paths: config.include_paths.clone(),
            fixed_form: config.fixed_form,
            cpp_compat: config.cpp_compat,
            emit_warnings,
            source_view,
            in_c_block_comment: false,
            cond_stack: Vec::new(),
            skip_depth: 0,
            include_depth: 0,
            included_files: Vec::new(),
        }
    }

    fn make_source_origin(
        source: &Arc<SourceFile>,
        source_line: u32,
        source_col: u32,
        reported: &ReportedLocation,
    ) -> SourceOrigin {
        SourceOrigin {
            source: source.clone(),
            source_line,
            source_col,
            filename: reported.filename.clone(),
            line: reported.line,
        }
    }

    fn is_emitting(&self) -> bool {
        self.skip_depth == 0
    }

    fn encode_generated_text(&self, text: &str) -> String {
        if self.source_view {
            crate::source_bytes::escape_utf8(text)
        } else {
            text.to_string()
        }
    }

    fn display_source_text(&self, text: &str) -> String {
        if self.source_view {
            crate::source_bytes::display_source_view(text)
        } else {
            text.to_string()
        }
    }

    fn display_directive_name(&self, directive: &str) -> String {
        const MAX_CHARS: usize = 80;

        let Some((end, _)) = directive.char_indices().nth(MAX_CHARS) else {
            return self.display_source_text(directive);
        };
        let mut displayed = self.display_source_text(&directive[..end]);
        displayed.push_str("...");
        displayed
    }

    fn set_location_macros(&mut self, location: &ReportedLocation) {
        self.defines.insert(
            "__LINE__".into(),
            MacroDef::object(&location.line.to_string()),
        );
        self.defines.insert(
            "__FILE__".into(),
            MacroDef::object(&format!(
                "\"{}\"",
                self.encode_generated_text(&location.filename)
            )),
        );
    }

    fn process(&mut self, source: &str, filename: &str) -> Result<PreprocOutput, PreprocError> {
        let mut output = String::new();
        let mut source_map = Vec::new();
        let root_source = Arc::new(SourceFile::new(
            Arc::from(filename),
            source.into(),
            self.source_view,
        ));
        let process_end = self.process_into(root_source.clone(), &mut output, &mut source_map)?;
        let final_reported = process_end.next;

        let mut eof_origin = None;
        for (index, line) in output.lines().enumerate() {
            if !line.trim().is_empty() {
                eof_origin = source_map
                    .get(index)
                    .map(|location| location.origin_at(line.len()));
            }
        }
        let eof_origin = eof_origin.unwrap_or_else(|| {
            let eof_line = root_source.text.lines().count() as u32 + 1;
            SourceOrigin {
                source: root_source.clone(),
                source_line: eof_line,
                source_col: 1,
                filename: final_reported.filename,
                line: final_reported.line,
            }
        });

        Ok(PreprocOutput {
            text: output,
            source_map,
            included_files: self.included_files.clone(),
            source_view: self.source_view,
            eof_origin,
        })
    }

    fn process_into(
        &mut self,
        source: Arc<SourceFile>,
        output: &mut String,
        source_map: &mut Vec<SourceLoc>,
    ) -> Result<ProcessEnd, PreprocError> {
        let filename = source.filename.as_ref();
        let mut reported = ReportedLocation {
            filename: source.filename.clone(),
            line: 1,
        };
        let mut last_reported = None;
        let conditional_floor = self.cond_stack.len();

        let now = current_datetime();
        self.defines.insert(
            "__DATE__".into(),
            MacroDef::object(&format!("\"{}\"", now.0)),
        );
        self.defines.insert(
            "__TIME__".into(),
            MacroDef::object(&format!("\"{}\"", now.1)),
        );

        // Process lines with inline backslash continuation joining,
        // tracking original line numbers so __LINE__ and source_map are correct.
        let raw_lines: Vec<&str> = source.text.lines().collect();
        let mut i = 0;
        while i < raw_lines.len() {
            let physical_start = i;
            let orig_line_num = (i + 1) as u32; // 1-based, tracks original source line
            let reported_start = reported.clone();
            let mut logical_line = MappedText::empty(Self::make_source_origin(
                &source,
                orig_line_num,
                1,
                &reported_start,
            ));

            // Join backslash-continued lines (C-style).
            while i < raw_lines.len() && raw_lines[i].ends_with('\\') {
                let line_num = (i + 1) as u32;
                let piece_reported = reported_start.at_physical_offset(line_num - orig_line_num);
                let piece = MappedText::source_line(
                    &raw_lines[i][..raw_lines[i].len() - 1],
                    Self::make_source_origin(&source, line_num, 1, &piece_reported),
                );
                logical_line.append(&piece);
                i += 1;
            }
            if i < raw_lines.len() {
                let line_num = (i + 1) as u32;
                let piece_reported = reported_start.at_physical_offset(line_num - orig_line_num);
                let piece = MappedText::source_line(
                    raw_lines[i],
                    Self::make_source_origin(&source, line_num, 1, &piece_reported),
                );
                logical_line.append(&piece);
                i += 1;
            }

            // Also join Fortran &-continued lines (free-form).
            // A line ending with & in the code portion (not inside strings or after !)
            // continues on the next line.
            // Skip for preprocessor directives (#if, #define, etc.) where ! and &
            // have C semantics, not Fortran semantics.
            let mut joined_fortran_continuation = false;
            if !self.fixed_form && !logical_line.text.trim_start().starts_with('#') {
                // Incremental: scan only the newly appended piece per
                // join, carrying string state — the full-line rescan
                // was O(N^2) on many-continuation statements.
                let mut scan = scan_trailing_ampersand(&logical_line.text, 0, None);
                while let Some(amp_pos) = scan.amp {
                    if i < raw_lines.len() {
                        let raw_next = raw_lines[i];
                        let next = raw_next.trim_start();
                        if next.starts_with('#') {
                            break;
                        }
                        // F2018 6.3.2.4: comment lines and blank lines
                        // may appear between a continuation line and
                        // its successor. Skip them without breaking
                        // the continuation.
                        if next.starts_with('!') || next.is_empty() {
                            i += 1;
                            continue;
                        }
                        logical_line.truncate(amp_pos);
                        let leading = raw_next.len() - next.len();
                        let content_start = leading + usize::from(next.starts_with('&'));
                        let physical_line = (i + 1) as u32;
                        let piece_reported =
                            reported_start.at_physical_offset(physical_line - orig_line_num);
                        let next_line = MappedText::source_line(
                            raw_next,
                            Self::make_source_origin(&source, physical_line, 1, &piece_reported),
                        );
                        let next_piece = next_line.slice(content_start..raw_next.len());
                        let base = logical_line.text.len();
                        logical_line.append(&next_piece);
                        joined_fortran_continuation = true;
                        i += 1;
                        scan = scan_trailing_ampersand(
                            &logical_line.text[base..],
                            base,
                            scan.in_string,
                        );
                    } else {
                        break;
                    }
                }
            }

            let physical_lines = i - physical_start;
            last_reported =
                Some(reported_start.at_physical_offset(physical_lines.saturating_sub(1) as u32));
            self.set_location_macros(&reported_start);

            let trimmed = logical_line.text.trim_start();
            if self.cpp_compat || self.in_c_block_comment || trimmed.starts_with("/*") {
                logical_line = strip_c_block_comments_from_mapped_line(
                    &logical_line,
                    &mut self.in_c_block_comment,
                );
            }

            // Fixed-form: C, c, or * in column 1 is a comment line.
            if self.fixed_form {
                let first = logical_line.text.as_bytes().first().copied().unwrap_or(0);
                if first == b'C' || first == b'c' || first == b'*' {
                    // Comment line — emit as-is without expansion.
                    if self.is_emitting() {
                        output.push_str(&logical_line.text);
                    }
                    output.push('\n');
                    source_map.push(logical_line.into_source_loc());
                    reported.advance(physical_lines);
                    continue;
                }
            }

            let trimmed = logical_line.text.trim_start();

            // Preprocessor directives: # in column 1 (or after whitespace in free-form).
            if trimmed.starts_with('#') {
                let directive_start = logical_line.text.len() - trimmed.len();
                let directive_line = logical_line.slice(directive_start..logical_line.text.len());
                let reset_location = self.process_directive(
                    &directive_line,
                    filename,
                    output,
                    source_map,
                    &mut reported,
                    conditional_floor,
                )?;
                output.push('\n');
                source_map.push(logical_line.into_source_loc());
                if !reset_location {
                    reported.advance(physical_lines);
                }
                continue;
            }

            let emitted = if self.is_emitting() {
                let expanded = self.expand_mapped_macros(&logical_line)?;
                match parse_fortran_include_path(&expanded.text, self.fixed_form) {
                    Ok(Some(path)) => {
                        if joined_fortran_continuation {
                            return Err(PreprocError {
                                filename: reported_start.filename.to_string(),
                                line: reported_start.line,
                                msg: "Fortran INCLUDE line cannot be continued".into(),
                            });
                        }
                        let path = self.display_source_text(&path);
                        self.include_file(
                            &path,
                            filename,
                            false,
                            &reported_start.filename,
                            reported_start.line,
                            output,
                            source_map,
                        )?;
                        MappedText::empty(expanded.fallback)
                    }
                    Ok(None) => {
                        output.push_str(&expanded.text);
                        expanded
                    }
                    Err(msg) => {
                        return Err(PreprocError {
                            filename: reported_start.filename.to_string(),
                            line: reported_start.line,
                            msg: self.display_source_text(&msg),
                        });
                    }
                }
            } else {
                MappedText::empty(logical_line.fallback.clone())
            };
            output.push('\n');
            source_map.push(emitted.into_source_loc());
            reported.advance(physical_lines);
        }

        if self.cond_stack.len() > conditional_floor {
            let open_levels = self.cond_stack.len() - conditional_floor;
            while self.cond_stack.len() > conditional_floor {
                self.pop_cond();
            }
            let location = last_reported.as_ref().unwrap_or(&reported);
            let scope = if self.include_depth > 0 {
                " in include file"
            } else {
                ""
            };
            return Err(PreprocError {
                filename: location.filename.to_string(),
                line: location.line,
                msg: format!("unterminated #if/#ifdef{scope} ({open_levels} level(s) still open)"),
            });
        }

        Ok(ProcessEnd { next: reported })
    }

    fn process_directive(
        &mut self,
        line: &MappedText,
        source_filename: &str,
        output: &mut String,
        source_map: &mut Vec<SourceLoc>,
        reported: &mut ReportedLocation,
        conditional_floor: usize,
    ) -> Result<bool, PreprocError> {
        let diagnostic_filename = reported.filename.clone();
        let diagnostic_line = reported.line;
        let line = strip_c_directive_comments(line);
        let bytes = line.text.as_bytes();
        let mut directive_start = 1; // skip '#'
        while directive_start < bytes.len() && bytes[directive_start].is_ascii_whitespace() {
            directive_start += 1;
        }
        let mut directive_end = directive_start;
        while directive_end < bytes.len() && !bytes[directive_end].is_ascii_whitespace() {
            directive_end += 1;
        }
        let directive = &line.text[directive_start..directive_end];
        let mut args_start = directive_end;
        while args_start < bytes.len() && bytes[args_start].is_ascii_whitespace() {
            args_start += 1;
        }
        let args = line.slice(args_start..line.text.len());
        let args_text = args.text.as_str();

        // Conditionals must be processed even when skipping.
        match directive {
            "ifdef" => self.do_ifdef(args_text, false)?,
            "ifndef" => self.do_ifdef(args_text, true)?,
            "if" => self.do_if(&args, &diagnostic_filename, diagnostic_line)?,
            "elif" => self.do_elif(
                &args,
                &diagnostic_filename,
                diagnostic_line,
                conditional_floor,
            )?,
            "else" => self.do_else(&diagnostic_filename, diagnostic_line, conditional_floor)?,
            "endif" => self.do_endif(&diagnostic_filename, diagnostic_line, conditional_floor)?,
            _ => {}
        }
        if matches!(
            directive,
            "ifdef" | "ifndef" | "if" | "elif" | "else" | "endif"
        ) {
            return Ok(false);
        }

        // All other directives are only processed when emitting.
        if !self.is_emitting() {
            return Ok(false);
        }

        match directive {
            "define" => self.do_define(args_text)?,
            "undef" => self.do_undef(args_text)?,
            "include" => self.do_include(
                args_text,
                source_filename,
                &diagnostic_filename,
                diagnostic_line,
                output,
                source_map,
            )?,
            "error" => {
                return Err(PreprocError {
                    filename: diagnostic_filename.to_string(),
                    line: diagnostic_line,
                    msg: format!("#error {}", self.display_source_text(args_text)),
                });
            }
            "warning" => {
                if self.emit_warnings {
                    eprintln!(
                        "{}:{}: warning: #warning {}",
                        diagnostic_filename,
                        diagnostic_line,
                        self.display_source_text(args_text)
                    );
                }
            }
            "line" => {
                return Ok(self.do_line(args_text, reported));
            }
            "pragma" => {} // implementations may ignore unrecognized pragmas
            "" => {}       // bare # is allowed (null directive)
            _ if directive.parse::<u32>().is_ok() => {
                let marker = if args_text.is_empty() {
                    directive.to_string()
                } else {
                    format!("{directive} {args_text}")
                };
                return Ok(self.do_line(&marker, reported));
            }
            _ => {
                return Err(PreprocError {
                    filename: diagnostic_filename.to_string(),
                    line: diagnostic_line,
                    msg: format!(
                        "unknown preprocessing directive #{}",
                        self.display_directive_name(directive)
                    ),
                });
            }
        }
        Ok(false)
    }

    // ---- Conditional directive helpers (maintain skip_depth counter) ----

    fn push_cond(&mut self, state: CondState) {
        if !matches!(state, CondState::Active) {
            self.skip_depth += 1;
        }
        self.cond_stack.push(ConditionalFrame {
            state,
            seen_else: false,
        });
    }

    fn set_top_cond(&mut self, new: CondState) {
        let old = self.cond_stack.last().unwrap().state;
        let was_skip = !matches!(old, CondState::Active);
        let now_skip = !matches!(new, CondState::Active);
        match (was_skip, now_skip) {
            (false, true) => self.skip_depth += 1,
            (true, false) => self.skip_depth -= 1,
            _ => {}
        }
        self.cond_stack.last_mut().unwrap().state = new;
    }

    fn pop_cond(&mut self) -> Option<CondState> {
        let popped = self.cond_stack.pop()?.state;
        if !matches!(popped, CondState::Active) {
            self.skip_depth -= 1;
        }
        Some(popped)
    }

    fn require_file_local_conditional(
        &self,
        directive: &str,
        filename: &str,
        line_num: u32,
        conditional_floor: usize,
    ) -> Result<(), PreprocError> {
        if conditional_floor > 0 && self.cond_stack.len() == conditional_floor {
            return Err(PreprocError {
                filename: filename.into(),
                line: line_num,
                msg: format!(
                    "{directive} cannot match a conditional opened outside this include file"
                ),
            });
        }
        Ok(())
    }

    // ---- Conditional directives ----

    fn do_ifdef(&mut self, args: &str, negate: bool) -> Result<(), PreprocError> {
        let name = args.split_whitespace().next().unwrap_or("");
        if !self.is_emitting() {
            self.push_cond(CondState::ParentSkipping);
            return Ok(());
        }
        let defined = self.defines.contains_key(name);
        let condition = if negate { !defined } else { defined };
        self.push_cond(if condition {
            CondState::Active
        } else {
            CondState::Skipping
        });
        Ok(())
    }

    fn do_if(
        &mut self,
        args: &MappedText,
        filename: &str,
        line_num: u32,
    ) -> Result<(), PreprocError> {
        if !self.is_emitting() {
            self.push_cond(CondState::ParentSkipping);
            return Ok(());
        }
        let val = self.eval_condition(args, filename, line_num)?;
        self.push_cond(if val {
            CondState::Active
        } else {
            CondState::Skipping
        });
        Ok(())
    }

    fn do_elif(
        &mut self,
        args: &MappedText,
        filename: &str,
        line_num: u32,
        conditional_floor: usize,
    ) -> Result<(), PreprocError> {
        self.require_file_local_conditional("#elif", filename, line_num, conditional_floor)?;
        let Some(frame) = self.cond_stack.last().copied() else {
            return Err(PreprocError {
                filename: filename.into(),
                line: line_num,
                msg: "#elif without matching #if".into(),
            });
        };
        if frame.seen_else {
            return Err(PreprocError {
                filename: filename.into(),
                line: line_num,
                msg: "#elif after #else".into(),
            });
        }
        match frame.state {
            CondState::ParentSkipping => Ok(()),
            CondState::Active => {
                self.set_top_cond(CondState::Done);
                Ok(())
            }
            CondState::Done => Ok(()),
            CondState::Skipping => {
                let val = self.eval_condition(args, filename, line_num)?;
                self.set_top_cond(if val {
                    CondState::Active
                } else {
                    CondState::Skipping
                });
                Ok(())
            }
        }
    }

    fn do_else(
        &mut self,
        filename: &str,
        line_num: u32,
        conditional_floor: usize,
    ) -> Result<(), PreprocError> {
        self.require_file_local_conditional("#else", filename, line_num, conditional_floor)?;
        let Some(frame) = self.cond_stack.last().copied() else {
            return Err(PreprocError {
                filename: filename.into(),
                line: line_num,
                msg: "#else without matching #if".into(),
            });
        };
        if frame.seen_else {
            return Err(PreprocError {
                filename: filename.into(),
                line: line_num,
                msg: "#else after #else".into(),
            });
        }
        self.cond_stack.last_mut().unwrap().seen_else = true;
        match frame.state {
            CondState::ParentSkipping => Ok(()),
            CondState::Active => {
                self.set_top_cond(CondState::Done);
                Ok(())
            }
            CondState::Done => Ok(()),
            CondState::Skipping => {
                self.set_top_cond(CondState::Active);
                Ok(())
            }
        }
    }

    fn do_endif(
        &mut self,
        filename: &str,
        line_num: u32,
        conditional_floor: usize,
    ) -> Result<(), PreprocError> {
        self.require_file_local_conditional("#endif", filename, line_num, conditional_floor)?;
        if self.pop_cond().is_none() {
            return Err(PreprocError {
                filename: filename.into(),
                line: line_num,
                msg: "#endif without matching #if".into(),
            });
        }
        Ok(())
    }

    // ---- #define / #undef ----

    fn do_define(&mut self, args: &str) -> Result<(), PreprocError> {
        let args = args.trim();
        if args.is_empty() {
            return Ok(());
        }

        // Check for function-like macro: NAME(params...) body
        if let Some(paren_pos) = args.find('(') {
            let name = &args[..paren_pos];
            if !name.contains(' ') {
                // Function-like macro.
                let rest = &args[paren_pos + 1..];
                if let Some(close) = rest.find(')') {
                    let params_str = &rest[..close];
                    let mut params: Vec<String> = params_str
                        .split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect();
                    // Handle variadic: last param is "..." → replace with __VA_ARGS__
                    let is_variadic = params.last().is_some_and(|p| p == "...");
                    if is_variadic {
                        params.pop();
                    }
                    let body = rest[close + 1..].trim();
                    let mut def = MacroDef::function(params, body);
                    def.is_variadic = is_variadic;
                    self.defines.insert(name.into(), def);
                    return Ok(());
                }
            }
        }

        // Object-like macro: NAME body  or  NAME (empty body = "1")
        let (name, body) = split_first_word(args);
        // Empty #define has empty body (not "1"). #ifdef uses contains_key, not body value.
        self.defines.insert(name.into(), MacroDef::object(body));
        Ok(())
    }

    fn do_undef(&mut self, args: &str) -> Result<(), PreprocError> {
        let name = args.split_whitespace().next().unwrap_or("");
        self.defines.remove(name);
        Ok(())
    }

    // ---- #line ----

    fn do_line(&self, args: &str, reported: &mut ReportedLocation) -> bool {
        let args = args.trim();
        let (line_str, rest) = split_first_word(args);
        if let Ok(line_num) = line_str.parse::<u32>() {
            reported.line = line_num;
            let rest = rest.trim();
            if let Some(quoted) = rest.strip_prefix('"') {
                if let Some(end) = quoted.find('"') {
                    reported.filename = Arc::from(self.display_source_text(&quoted[..end]));
                }
            }
            true
        } else {
            false
        }
    }

    // ---- #include ----

    fn do_include(
        &mut self,
        args: &str,
        source_filename: &str,
        diagnostic_filename: &str,
        diagnostic_line: u32,
        output: &mut String,
        source_map: &mut Vec<SourceLoc>,
    ) -> Result<(), PreprocError> {
        let args = args.trim();
        let (path_str, search_system) = if let Some(rest) = args.strip_prefix('"') {
            let end = rest.find('"').ok_or_else(|| PreprocError {
                filename: diagnostic_filename.into(),
                line: diagnostic_line,
                msg: "unterminated #include string".into(),
            })?;
            (&rest[..end], false)
        } else if let Some(rest) = args.strip_prefix('<') {
            let end = rest.find('>').ok_or_else(|| PreprocError {
                filename: diagnostic_filename.into(),
                line: diagnostic_line,
                msg: "unterminated #include <path>".into(),
            })?;
            (&rest[..end], true)
        } else {
            return Err(PreprocError {
                filename: diagnostic_filename.into(),
                line: diagnostic_line,
                msg: format!(
                    "expected \"file\" or <file> after #include, got: {}",
                    self.display_source_text(args)
                ),
            });
        };
        let path = self.display_source_text(path_str);

        self.include_file(
            &path,
            source_filename,
            search_system,
            diagnostic_filename,
            diagnostic_line,
            output,
            source_map,
        )
    }

    fn include_file(
        &mut self,
        path: &str,
        source_filename: &str,
        search_system: bool,
        diagnostic_filename: &str,
        diagnostic_line: u32,
        output: &mut String,
        source_map: &mut Vec<SourceLoc>,
    ) -> Result<(), PreprocError> {
        if self.include_depth >= 64 {
            return Err(PreprocError {
                filename: diagnostic_filename.into(),
                line: diagnostic_line,
                msg: "include depth limit exceeded (possible recursion)".into(),
            });
        }

        // Search for the file.
        let resolved = self
            .resolve_include(path, source_filename, search_system)
            .ok_or_else(|| PreprocError {
                filename: diagnostic_filename.into(),
                line: diagnostic_line,
                msg: format!("cannot find include file: {}", path),
            })?;

        let content = if self.source_view {
            let bytes = std::fs::read(&resolved).map_err(|e| PreprocError {
                filename: diagnostic_filename.into(),
                line: diagnostic_line,
                msg: format!("reading {}: {}", resolved.display(), e),
            })?;
            crate::source_bytes::to_source_view(&bytes)
        } else {
            std::fs::read_to_string(&resolved).map_err(|e| PreprocError {
                filename: diagnostic_filename.into(),
                line: diagnostic_line,
                msg: format!("reading {}: {}", resolved.display(), e),
            })?
        };
        if !self.included_files.contains(&resolved) {
            self.included_files.push(resolved.clone());
        }

        // Built-ins are dynamically scoped to the included source.
        let saved_file = self.defines.get("__FILE__").cloned();
        let saved_line = self.defines.get("__LINE__").cloned();

        self.include_depth += 1;
        let inc_filename = resolved.to_string_lossy().into_owned();
        let included_source = Arc::new(SourceFile::new(
            Arc::from(inc_filename),
            content,
            self.source_view,
        ));
        let conditional_depth = self.cond_stack.len();
        let include_result = self.process_into(included_source, output, source_map);
        while self.cond_stack.len() > conditional_depth {
            self.pop_cond();
        }
        debug_assert_eq!(
            self.cond_stack.len(),
            conditional_depth,
            "included file crossed its conditional-stack floor"
        );
        self.include_depth -= 1;

        restore_macro(&mut self.defines, "__FILE__", saved_file);
        restore_macro(&mut self.defines, "__LINE__", saved_line);
        include_result?;
        Ok(())
    }

    fn resolve_include(&self, path: &str, current_file: &str, system: bool) -> Option<PathBuf> {
        // #include "file" — search relative to current file first, then include paths.
        // #include <file> — search include paths only (system = true).
        if !system {
            let current_dir = Path::new(current_file).parent().unwrap_or(Path::new("."));
            let candidate = current_dir.join(path);
            if candidate.exists() {
                return Some(candidate);
            }
        }

        // Search include paths.
        for dir in &self.include_paths {
            let candidate = dir.join(path);
            if candidate.exists() {
                return Some(candidate);
            }
        }

        None
    }

    // ---- Condition expression evaluator ----

    fn eval_condition(
        &self,
        expr: &MappedText,
        filename: &str,
        line_num: u32,
    ) -> Result<bool, PreprocError> {
        // Expand macros in the expression first.
        let expanded = self.expand_condition_macros(expr)?;
        // Parse and evaluate the expression.
        eval_expr(&expanded).map_err(|msg| PreprocError {
            filename: filename.into(),
            line: line_num,
            msg: format!("in #if expression: {}", self.display_source_text(&msg)),
        })
    }

    /// Expand macros and `defined(NAME)` / `defined NAME` in a condition expression.
    ///
    /// Condition expressions share the same recursive macro engine as
    /// ordinary source lines, but apply condition-specific semantics:
    /// `defined` is resolved during the walk and any remaining identifiers
    /// are rewritten to `0` at the end.
    fn expand_condition_macros(&self, expr: &MappedText) -> Result<String, PreprocError> {
        let expanding = std::collections::HashSet::new();
        let expanded =
            self.expand_mapped_macros_inner(expr, &expanding, MacroContext::Condition)?;
        Ok(replace_undefined_idents(&expanded.text))
    }

    // ---- Macro expansion in source lines ----

    fn expand_mapped_macros(&self, line: &MappedText) -> Result<MappedText, PreprocError> {
        let expanding = std::collections::HashSet::new();
        self.expand_mapped_macros_inner(line, &expanding, MacroContext::Source)
    }

    fn expand_mapped_macros_inner(
        &self,
        line: &MappedText,
        expanding: &std::collections::HashSet<String>,
        context: MacroContext,
    ) -> Result<MappedText, PreprocError> {
        if self.defines.is_empty() && context == MacroContext::Source {
            return Ok(line.clone());
        }

        let mut result = MappedText::empty(line.fallback.clone());
        let bytes = line.text.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if context == MacroContext::Source && bytes[i] == b'!' {
                result.append_slice(line, i..line.text.len());
                break;
            }

            if bytes[i] == b'\'' || bytes[i] == b'"' {
                let start = i;
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        i += 1;
                        if i < bytes.len() && bytes[i] == quote {
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        i += line.text[i..].chars().next().unwrap().len_utf8();
                    }
                }
                result.append_slice(line, start..i);
                continue;
            }

            if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let ident = &line.text[start..i];

                if context == MacroContext::Condition && ident == "defined" {
                    let (name, new_i) = parse_defined_operand(&line.text, i);
                    let invocation = line.origin_at(start);
                    let replacement = if self.defines.contains_key(name) {
                        "1"
                    } else {
                        "0"
                    };
                    result.append(&MappedText::anchored(replacement, invocation));
                    i = new_i;
                    continue;
                }

                if expanding.contains(ident) {
                    result.append_slice(line, start..i);
                    continue;
                }

                if ident == "__LINE__" || ident == "__FILE__" {
                    let invocation = line.origin_at(start);
                    let replacement = if ident == "__LINE__" {
                        invocation.line.to_string()
                    } else {
                        format!("\"{}\"", self.encode_generated_text(&invocation.filename))
                    };
                    result.append(&MappedText::anchored(&replacement, invocation));
                    continue;
                }

                if let Some(def) = self.defines.get(ident) {
                    let invocation = line.origin_at(start);
                    if def.is_function {
                        let mut paren_start = i;
                        while paren_start < bytes.len() && bytes[paren_start].is_ascii_whitespace()
                        {
                            paren_start += 1;
                        }
                        if paren_start < bytes.len() && bytes[paren_start] == b'(' {
                            if let Some((expanded, new_i)) = self.expand_mapped_function_macro(
                                ident,
                                def,
                                line,
                                paren_start,
                                invocation,
                                expanding,
                                context,
                            )? {
                                let mut next_expanding = expanding.clone();
                                next_expanding.insert(ident.to_string());
                                result.append(&self.expand_mapped_macros_inner(
                                    &expanded,
                                    &next_expanding,
                                    context,
                                )?);
                                i = new_i;
                                continue;
                            }
                        }
                        result.append_slice(line, start..i);
                    } else {
                        let mut next_expanding = expanding.clone();
                        next_expanding.insert(ident.to_string());
                        let body = MappedText::anchored(&def.body, invocation);
                        result.append(&self.expand_mapped_macros_inner(
                            &body,
                            &next_expanding,
                            context,
                        )?);
                    }
                } else {
                    result.append_slice(line, start..i);
                }
                continue;
            }

            let end = i + line.text[i..].chars().next().unwrap().len_utf8();
            result.append_slice(line, i..end);
            i = end;
        }

        Ok(result)
    }

    fn expand_mapped_function_macro(
        &self,
        name: &str,
        def: &MacroDef,
        line: &MappedText,
        paren_start: usize,
        invocation: SourceOrigin,
        expanding: &std::collections::HashSet<String>,
        context: MacroContext,
    ) -> Result<Option<(MappedText, usize)>, PreprocError> {
        let bytes = line.text.as_bytes();
        let mut i = paren_start + 1;
        let mut arg_start = i;
        let mut arg_ranges = Vec::new();
        let mut depth = 1;
        let mut quote = None;

        while i < bytes.len() && depth > 0 {
            if let Some(delimiter) = quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                    i += line.text[i..].chars().next().unwrap().len_utf8();
                    continue;
                }
                if bytes[i] == delimiter {
                    if i + 1 < bytes.len() && bytes[i + 1] == delimiter {
                        i += 2;
                        continue;
                    }
                    quote = None;
                    i += 1;
                    continue;
                }
                i += line.text[i..].chars().next().unwrap().len_utf8();
                continue;
            }

            match bytes[i] {
                delimiter @ (b'\'' | b'"') => {
                    quote = Some(delimiter);
                    i += 1;
                }
                b'(' => {
                    depth += 1;
                    i += 1;
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        arg_ranges.push(trim_text_range(&line.text, arg_start..i));
                    }
                    i += 1;
                }
                b',' if depth == 1 => {
                    arg_ranges.push(trim_text_range(&line.text, arg_start..i));
                    i += 1;
                    arg_start = i;
                }
                _ => i += line.text[i..].chars().next().unwrap().len_utf8(),
            }
        }

        if depth != 0 {
            return Ok(None);
        }

        let mut args: Vec<MappedText> = arg_ranges
            .into_iter()
            .map(|range| line.slice(range))
            .collect();
        let provided = if def.params.is_empty() && args.len() == 1 && args[0].text.is_empty() {
            0
        } else {
            args.len()
        };
        let required = def.params.len();
        let too_few = provided < required;
        let too_many = !def.is_variadic && provided > required;
        if too_few || too_many {
            let msg = if too_few {
                let minimum = if def.is_variadic {
                    format!("at least {required}")
                } else {
                    required.to_string()
                };
                format!(
                    "macro \"{name}\" requires {minimum} argument{}, but only {provided} given",
                    if required == 1 { "" } else { "s" }
                )
            } else {
                format!("macro \"{name}\" passed {provided} arguments, but takes just {required}")
            };
            return Err(PreprocError {
                filename: invocation.filename.to_string(),
                line: invocation.line,
                msg,
            });
        }
        if provided == 0 {
            args.clear();
        }

        let expanded_args: Vec<MappedText> = args
            .iter()
            .map(|arg| self.expand_mapped_macros_inner(arg, expanding, context))
            .collect::<Result<_, _>>()?;

        let mut param_map: HashMap<&str, usize> = HashMap::new();
        for (pi, param) in def.params.iter().enumerate() {
            param_map.insert(param.as_str(), pi);
        }

        let va_args_raw = if def.is_variadic {
            join_mapped_args(
                args.get(def.params.len()..).unwrap_or(&[]),
                invocation.clone(),
            )
        } else {
            MappedText::empty(invocation.clone())
        };
        let va_args_expanded = if def.is_variadic {
            join_mapped_args(
                expanded_args.get(def.params.len()..).unwrap_or(&[]),
                invocation.clone(),
            )
        } else {
            MappedText::empty(invocation.clone())
        };

        let body_bytes = def.body.as_bytes();
        let mut body = MappedText::empty(invocation.clone());
        let mut bi = 0;

        while bi < body_bytes.len() {
            if body_bytes[bi] == b'#' && bi + 1 < body_bytes.len() && body_bytes[bi + 1] != b'#' {
                let mut id_start = bi + 1;
                while id_start < body_bytes.len() && body_bytes[id_start] == b' ' {
                    id_start += 1;
                }
                let mut id_end = id_start;
                while id_end < body_bytes.len()
                    && (body_bytes[id_end].is_ascii_alphanumeric() || body_bytes[id_end] == b'_')
                {
                    id_end += 1;
                }
                if id_end > id_start {
                    let id = std::str::from_utf8(&body_bytes[id_start..id_end]).unwrap_or("");
                    if let Some(&pi) = param_map.get(id) {
                        let raw = args.get(pi).map(|arg| arg.text.as_str()).unwrap_or("");
                        body.push_text(
                            &format!("\"{}\"", raw),
                            invocation.clone(),
                            SourceRunKind::Anchor,
                        );
                        bi = id_end;
                        continue;
                    }
                }
            }

            if bi + 1 < body_bytes.len() && body_bytes[bi] == b'#' && body_bytes[bi + 1] == b'#' {
                let trimmed_len = body.text.trim_end().len();
                body.truncate(trimmed_len);
                bi += 2;
                while bi < body_bytes.len() && body_bytes[bi] == b' ' {
                    bi += 1;
                }
                continue;
            }

            if body_bytes[bi].is_ascii_alphabetic() || body_bytes[bi] == b'_' {
                let id_start = bi;
                while bi < body_bytes.len()
                    && (body_bytes[bi].is_ascii_alphanumeric() || body_bytes[bi] == b'_')
                {
                    bi += 1;
                }
                let id = std::str::from_utf8(&body_bytes[id_start..bi]).unwrap_or("");
                let is_pasted = macro_param_is_pasted_left(body_bytes, id_start)
                    || macro_param_is_pasted_right(body_bytes, bi);

                if id == "__VA_ARGS__" && def.is_variadic {
                    body.append(if is_pasted {
                        &va_args_raw
                    } else {
                        &va_args_expanded
                    });
                } else if let Some(&pi) = param_map.get(id) {
                    let replacement = if is_pasted {
                        args.get(pi)
                    } else {
                        expanded_args.get(pi)
                    };
                    if let Some(replacement) = replacement {
                        body.append(replacement);
                    }
                } else {
                    body.push_text(id, invocation.clone(), SourceRunKind::Anchor);
                }
                continue;
            }

            let ch = def.body[bi..]
                .chars()
                .next()
                .expect("macro body index must stay on a character boundary");
            let end = bi + ch.len_utf8();
            body.push_text(
                &def.body[bi..end],
                invocation.clone(),
                SourceRunKind::Anchor,
            );
            bi = end;
        }

        Ok(Some((body, i)))
    }
}

fn trim_text_range(text: &str, range: Range<usize>) -> Range<usize> {
    let slice = &text[range.clone()];
    let trimmed_start = slice.trim_start();
    let start = range.start + slice.len() - trimmed_start.len();
    let trimmed = trimmed_start.trim_end();
    start..start + trimmed.len()
}

fn restore_macro(defines: &mut HashMap<String, MacroDef>, name: &str, saved: Option<MacroDef>) {
    if let Some(saved) = saved {
        defines.insert(name.into(), saved);
    } else {
        defines.remove(name);
    }
}

fn join_mapped_args(args: &[MappedText], fallback: SourceOrigin) -> MappedText {
    let mut joined = MappedText::empty(fallback.clone());
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            joined.push_text(", ", fallback.clone(), SourceRunKind::Anchor);
        }
        joined.append(arg);
    }
    joined
}

fn macro_param_is_pasted_left(body: &[u8], start: usize) -> bool {
    let mut i = start;
    while i > 0 && body[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    i >= 2 && body[i - 2] == b'#' && body[i - 1] == b'#'
}

fn macro_param_is_pasted_right(body: &[u8], end: usize) -> bool {
    let mut i = end;
    while i < body.len() && body[i].is_ascii_whitespace() {
        i += 1;
    }
    i + 1 < body.len() && body[i] == b'#' && body[i + 1] == b'#'
}

fn parse_defined_operand(expr: &str, start: usize) -> (&str, usize) {
    let bytes = expr.as_bytes();
    let mut i = start;

    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }

    let has_paren = i < bytes.len() && bytes[i] == b'(';
    if has_paren {
        i += 1;
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
    }

    let name_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    let name = &expr[name_start..i];

    if has_paren {
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b')' {
            i += 1;
        }
    }

    (name, i)
}

// ---- Expression evaluator for #if ----

fn eval_expr(expr: &str) -> Result<bool, String> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Err("empty expression".into());
    }
    Ok(ConditionExprParser::new(trimmed).parse()? != 0)
}

struct ConditionExprParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> ConditionExprParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn parse(mut self) -> Result<i64, String> {
        let value = self.parse_logical_or(true)?;
        self.skip_whitespace();
        if self.pos != self.input.len() {
            return Err(self.unexpected_token());
        }
        Ok(value)
    }

    fn parse_logical_or(&mut self, evaluate: bool) -> Result<i64, String> {
        let mut left = self.parse_logical_and(evaluate)?;
        while self.consume("||") {
            let right = self.parse_logical_and(evaluate && left == 0)?;
            if evaluate {
                left = i64::from(left != 0 || right != 0);
            }
        }
        Ok(left)
    }

    fn parse_logical_and(&mut self, evaluate: bool) -> Result<i64, String> {
        let mut left = self.parse_equality(evaluate)?;
        while self.consume("&&") {
            let right = self.parse_equality(evaluate && left != 0)?;
            if evaluate {
                left = i64::from(left != 0 && right != 0);
            }
        }
        Ok(left)
    }

    fn parse_equality(&mut self, evaluate: bool) -> Result<i64, String> {
        let mut left = self.parse_relational(evaluate)?;
        loop {
            if self.consume("==") {
                let right = self.parse_relational(evaluate)?;
                if evaluate {
                    left = i64::from(left == right);
                }
            } else if self.consume("!=") {
                let right = self.parse_relational(evaluate)?;
                if evaluate {
                    left = i64::from(left != right);
                }
            } else {
                return Ok(left);
            }
        }
    }

    fn parse_relational(&mut self, evaluate: bool) -> Result<i64, String> {
        let mut left = self.parse_additive(evaluate)?;
        loop {
            if self.consume("<=") {
                let right = self.parse_additive(evaluate)?;
                if evaluate {
                    left = i64::from(left <= right);
                }
            } else if self.consume(">=") {
                let right = self.parse_additive(evaluate)?;
                if evaluate {
                    left = i64::from(left >= right);
                }
            } else if self.consume("<") {
                let right = self.parse_additive(evaluate)?;
                if evaluate {
                    left = i64::from(left < right);
                }
            } else if self.consume(">") {
                let right = self.parse_additive(evaluate)?;
                if evaluate {
                    left = i64::from(left > right);
                }
            } else {
                return Ok(left);
            }
        }
    }

    fn parse_additive(&mut self, evaluate: bool) -> Result<i64, String> {
        let mut left = self.parse_multiplicative(evaluate)?;
        loop {
            if self.consume("+") {
                let right = self.parse_multiplicative(evaluate)?;
                if evaluate {
                    left = left.wrapping_add(right);
                }
            } else if self.consume("-") {
                let right = self.parse_multiplicative(evaluate)?;
                if evaluate {
                    left = left.wrapping_sub(right);
                }
            } else {
                return Ok(left);
            }
        }
    }

    fn parse_multiplicative(&mut self, evaluate: bool) -> Result<i64, String> {
        let mut left = self.parse_unary(evaluate)?;
        loop {
            if self.consume("*") {
                let right = self.parse_unary(evaluate)?;
                if evaluate {
                    left = left.wrapping_mul(right);
                }
            } else if self.consume("/") {
                let right = self.parse_unary(evaluate)?;
                if evaluate {
                    if right == 0 {
                        return Err("division by zero in #if expression".into());
                    }
                    left = if left == i64::MIN && right == -1 {
                        i64::MIN
                    } else {
                        left / right
                    };
                }
            } else if self.consume("%") {
                let right = self.parse_unary(evaluate)?;
                if evaluate {
                    if right == 0 {
                        return Err("modulo by zero in #if expression".into());
                    }
                    left = if left == i64::MIN && right == -1 {
                        0
                    } else {
                        left % right
                    };
                }
            } else {
                return Ok(left);
            }
        }
    }

    fn parse_unary(&mut self, evaluate: bool) -> Result<i64, String> {
        if self.consume("!") {
            let value = self.parse_unary(evaluate)?;
            return Ok(if evaluate { i64::from(value == 0) } else { 0 });
        }
        if self.consume("-") {
            let value = self.parse_unary(evaluate)?;
            return Ok(if evaluate { value.wrapping_neg() } else { 0 });
        }
        if self.consume("+") {
            return self.parse_unary(evaluate);
        }
        self.parse_primary(evaluate)
    }

    fn parse_primary(&mut self, evaluate: bool) -> Result<i64, String> {
        self.skip_whitespace();
        if self.consume("(") {
            let value = self.parse_logical_or(evaluate)?;
            if !self.consume(")") {
                return Err("unmatched parenthesis in #if expression".into());
            }
            return Ok(value);
        }

        let bytes = self.input.as_bytes();
        if self.pos >= bytes.len() {
            return Err("unexpected end of expression".into());
        }

        if bytes[self.pos].is_ascii_digit() {
            let start = self.pos;
            if bytes[self.pos] == b'0'
                && bytes
                    .get(self.pos + 1)
                    .is_some_and(|next| matches!(next, b'x' | b'X'))
            {
                self.pos += 2;
                let digits = self.pos;
                while self
                    .input
                    .as_bytes()
                    .get(self.pos)
                    .is_some_and(u8::is_ascii_hexdigit)
                {
                    self.pos += 1;
                }
                if self.pos == digits {
                    return Err("invalid hex in #if: missing digits".into());
                }
                let value = i64::from_str_radix(&self.input[digits..self.pos], 16)
                    .map_err(|err| format!("invalid hex in #if: {}", err))?;
                return Ok(if evaluate { value } else { 0 });
            }

            while self
                .input
                .as_bytes()
                .get(self.pos)
                .is_some_and(u8::is_ascii_digit)
            {
                self.pos += 1;
            }
            let value = self.input[start..self.pos]
                .parse::<i64>()
                .map_err(|err| format!("invalid integer in #if: {}", err))?;
            return Ok(if evaluate { value } else { 0 });
        }

        if bytes[self.pos].is_ascii_alphabetic() || bytes[self.pos] == b'_' {
            self.pos += 1;
            while self
                .input
                .as_bytes()
                .get(self.pos)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                self.pos += 1;
            }
            return Ok(0);
        }

        Err(self.unexpected_token())
    }

    fn consume(&mut self, token: &str) -> bool {
        self.skip_whitespace();
        if self.input[self.pos..].starts_with(token) {
            self.pos += token.len();
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .input
            .as_bytes()
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }

    fn unexpected_token(&self) -> String {
        format!(
            "unexpected token in #if expression: '{}'",
            self.input[self.pos..].trim()
        )
    }
}

/// Replace remaining identifiers with "0" in a #if expression.
/// After macro expansion, any identifier that's still present is undefined
/// and evaluates to 0 per the cpp standard.
fn replace_undefined_idents(expr: &str) -> String {
    let mut result = String::new();
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Preserve numeric literals (including hex).
        if bytes[i].is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                result.push(bytes[i] as char);
                i += 1;
            }
            continue;
        }
        // Identifiers -> "0".
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let _ident = &expr[start..i];
            result.push('0');
            continue;
        }
        let ch = expr[i..]
            .chars()
            .next()
            .expect("condition expression index must stay on a character boundary");
        result.push(ch);
        i += ch.len_utf8();
    }
    result
}

/// Find the position of a trailing `&` continuation marker on this line.
/// Recognises both code continuations (`&` outside strings) and the string
/// continuation case where the `&` sits inside an unterminated literal —
/// `'hello &\n      &world'` is one logical literal and the line still
/// needs to be joined to the next.  Returns None if no `&` qualifies.
fn find_code_trailing_ampersand(line: &str) -> Option<usize> {
    scan_trailing_ampersand(line, 0, None).amp
}

/// Result of scanning one piece of a logical line: the trailing-`&`
/// position (absolute, via `base`) and the string state at the end of
/// the piece, so the NEXT appended piece can be scanned alone. The
/// join loop used to rescan the whole accumulated line per appended
/// continuation — O(N^2) on long statements (a 100k-continuation
/// statement preprocessed in ~25s, all of it in this rescan).
struct AmpScan {
    amp: Option<usize>,
    in_string: Option<u8>,
}

/// Scan `piece` (a suffix of the logical line starting at byte
/// `base`) with `in_string` carried from the previous piece. A
/// trailing `&` is only ever separated from the end by whitespace, so
/// the string state at the reported `&` equals the state at the end
/// of the piece — truncating there and appending the next piece keeps
/// the carried state valid.
fn scan_trailing_ampersand(piece: &str, base: usize, carried: Option<u8>) -> AmpScan {
    let line = piece;
    let bytes = line.as_bytes();
    let mut in_string: Option<u8> = carried;
    let mut last_amp: Option<usize> = None;

    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];

        // Track string state.  Inside a string, `&` followed only by
        // whitespace until end-of-line is also a continuation, and `!`
        // is a literal character (not the start of a comment).
        if let Some(quote) = in_string {
            if ch == quote {
                if i + 1 < bytes.len() && bytes[i + 1] == quote {
                    i += 2; // doubled quote escape
                    continue;
                }
                in_string = None;
                last_amp = None; // any `&` we saw was inside the now-closed string
                i += 1;
                continue;
            }
            if ch == b'&' {
                last_amp = Some(i);
            } else if !ch.is_ascii_whitespace() {
                last_amp = None;
            }
            i += 1;
            continue;
        }

        if ch == b'\'' || ch == b'"' {
            in_string = Some(ch);
            i += 1;
            continue;
        }

        // Comment — everything after ! is not code.
        if ch == b'!' {
            break;
        }

        if ch == b'&' {
            last_amp = Some(i);
        } else if !ch.is_ascii_whitespace() {
            // Non-whitespace after the & means it's not trailing.
            last_amp = None;
        }

        i += 1;
    }

    AmpScan {
        amp: last_amp.map(|pos| base + pos),
        in_string,
    }
}

fn parse_fortran_include_path(line: &str, fixed_form: bool) -> Result<Option<String>, String> {
    let statement = if fixed_form && !line.starts_with('\t') {
        let end = line
            .char_indices()
            .nth(72)
            .map_or(line.len(), |(offset, _)| offset);
        &line[..end]
    } else {
        line
    };
    let bytes = statement.as_bytes();
    let mut pos = 0;
    while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos += 1;
    }

    for expected in b"include" {
        if fixed_form {
            while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
                pos += 1;
            }
        }
        let Some(actual) = bytes.get(pos) else {
            return Ok(None);
        };
        if actual.to_ascii_lowercase() != *expected {
            return Ok(None);
        }
        pos += 1;
    }

    while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos += 1;
    }
    let Some(&quote) = bytes.get(pos) else {
        return Ok(None);
    };
    if !matches!(quote, b'\'' | b'"') {
        return Ok(None);
    }
    pos += 1;

    let mut path = String::new();
    let mut piece_start = pos;
    let closed_at = loop {
        let Some(&byte) = bytes.get(pos) else {
            return Err("unterminated Fortran INCLUDE string".into());
        };
        if byte == quote {
            path.push_str(&statement[piece_start..pos]);
            if bytes.get(pos + 1) == Some(&quote) {
                path.push(quote as char);
                pos += 2;
                piece_start = pos;
                continue;
            }
            break pos + 1;
        }
        pos += statement[pos..]
            .chars()
            .next()
            .expect("INCLUDE path index must stay on a character boundary")
            .len_utf8();
    };

    pos = closed_at;
    while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos += 1;
    }
    if pos == bytes.len() || bytes[pos] == b'!' {
        return Ok(Some(path));
    }

    Err(format!(
        "unexpected text after Fortran INCLUDE path: {}",
        statement[pos..].trim()
    ))
}

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    if let Some(pos) = s.find(|c: char| c.is_whitespace()) {
        (&s[..pos], s[pos..].trim_start())
    } else {
        (s, "")
    }
}

fn strip_c_directive_comments(line: &MappedText) -> MappedText {
    let bytes = line.text.as_bytes();
    let mut result = MappedText::empty(line.fallback.clone());
    let mut i = 0;
    let mut copy_start = 0;

    while i < bytes.len() {
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == quote {
                    if i + 1 < bytes.len() && bytes[i + 1] == quote {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += line.text[i..].chars().next().unwrap().len_utf8();
            }
            continue;
        }

        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            result.append_slice(line, copy_start..i);
            result.push_text(" ", line.origin_at(i), SourceRunKind::Anchor);
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += line.text[i..].chars().next().unwrap().len_utf8();
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                i = bytes.len();
            }
            copy_start = i;
            continue;
        }

        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            result.append_slice(line, copy_start..i);
            return result;
        }

        i += line.text[i..].chars().next().unwrap().len_utf8();
    }

    result.append_slice(line, copy_start..line.text.len());
    result
}

fn strip_c_block_comments_from_mapped_line(line: &MappedText, in_block: &mut bool) -> MappedText {
    let bytes = line.text.as_bytes();
    let mut result = MappedText::empty(line.fallback.clone());
    let mut i = 0;

    while i < bytes.len() {
        if *in_block {
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                *in_block = false;
                i += 2;
            } else {
                i = bytes.len();
            }
            continue;
        }

        if bytes[i] == b'!' {
            result.append_slice(line, i..line.text.len());
            break;
        }

        if bytes[i] == b'\'' || bytes[i] == b'"' {
            let start = i;
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == quote {
                    if i + 1 < bytes.len() && bytes[i + 1] == quote {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += line.text[i..].chars().next().unwrap().len_utf8();
            }
            result.append_slice(line, start..i);
            continue;
        }

        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            result.push_text(" ", line.origin_at(i), SourceRunKind::Anchor);
            *in_block = true;
            i += 2;
            continue;
        }

        let end = i + line.text[i..].chars().next().unwrap().len_utf8();
        result.append_slice(line, i..end);
        i = end;
    }

    result
}

/// Get current date and time strings for __DATE__ and __TIME__.
fn current_datetime() -> (String, String) {
    use std::time::SystemTime;

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Convert epoch seconds to date/time components.
    // Simple conversion without external crates.
    let secs_per_day = 86400u64;
    let days = now / secs_per_day;
    let time_of_day = now % secs_per_day;

    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since epoch to year/month/day (simplified Gregorian).
    let (year, month, day) = epoch_days_to_date(days);

    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month_name = months.get(month as usize).unwrap_or(&"???");

    let date = format!("{} {:2} {}", month_name, day, year);
    let time = format!("{:02}:{:02}:{:02}", hours, minutes, seconds);
    (date, time)
}

fn epoch_days_to_date(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from Howard Hinnant's date library (public domain).
    days += 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m - 1, d) // month is 0-based for array indexing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pp(src: &str) -> String {
        let config = PreprocConfig::default();
        preprocess(src, &config).unwrap().text
    }

    fn pp_with(src: &str, defines: &[(&str, &str)]) -> String {
        let mut config = PreprocConfig::default();
        for (k, v) in defines {
            config.defines.insert(k.to_string(), MacroDef::object(v));
        }
        preprocess(src, &config).unwrap().text
    }

    fn pp_cpp(src: &str) -> String {
        let config = PreprocConfig {
            cpp_compat: true,
            ..PreprocConfig::default()
        };
        preprocess(src, &config).unwrap().text
    }

    fn pp_bytes(src: &[u8]) -> Vec<u8> {
        preprocess_bytes(src, &PreprocConfig::default())
            .unwrap()
            .bytes()
    }

    fn pp_err(src: &str) -> PreprocError {
        let config = PreprocConfig::default();
        preprocess(src, &config).unwrap_err()
    }

    fn lines(s: &str) -> Vec<&str> {
        s.lines().filter(|l| !l.is_empty()).collect()
    }

    // ---- Object-like macros ----

    #[test]
    fn define_and_expand_object_macro() {
        let out = pp("#define FOO 42\nx = FOO\n");
        assert!(out.contains("x = 42"));
    }

    #[test]
    fn utf8_bom_does_not_hide_first_line_directive() {
        let output = pp_bytes(b"\xef\xbb\xbf#define FOO 42\nx = FOO\n");
        assert!(!output.starts_with(b"\xef\xbb\xbf"), "got: {output:?}");
        assert!(
            !output
                .windows(b"#define".len())
                .any(|text| text == b"#define"),
            "got: {output:?}"
        );
        assert!(
            output
                .windows(b"x = 42".len())
                .any(|text| text == b"x = 42"),
            "got: {output:?}"
        );
    }

    #[test]
    fn utf8_bom_is_removed_from_included_source_before_directive_dispatch() {
        let dir = std::env::temp_dir();
        let name = format!("afs-bom-include-{}.inc", std::process::id());
        let path = dir.join(&name);
        std::fs::write(&path, "\u{feff}#define INCLUDED_VALUE 73\n").unwrap();
        let config = PreprocConfig {
            include_paths: vec![dir],
            ..PreprocConfig::default()
        };
        let source = format!("#include \"{name}\"\nx = INCLUDED_VALUE\n");
        let output = preprocess(&source, &config).unwrap().text;
        assert!(!output.contains('\u{feff}'), "got: {output:?}");
        assert!(!output.contains("#define"), "got: {output:?}");
        assert!(output.contains("x = 73"), "got: {output:?}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn utf8_bom_is_only_removed_at_the_start_of_each_file() {
        let source = "x = 1\n\u{feff}#define HIDDEN 2\ny = HIDDEN\n";
        let output = pp(source);
        assert!(
            output.contains("\u{feff}#define HIDDEN 2"),
            "got: {output:?}"
        );
        assert!(output.contains("y = HIDDEN"), "got: {output:?}");
    }

    #[test]
    fn utf8_bom_is_removed_before_source_map_construction() {
        let result =
            preprocess("\u{feff}value = @\nsecond = 1\n", &PreprocConfig::default()).unwrap();
        assert_eq!(result.text, "value = @\nsecond = 1\n");
        let column = result.text.lines().next().unwrap().find('@').unwrap() as u32 + 1;
        let resolved = result.resolve_span(Span {
            file_id: 0,
            start: Position {
                line: 1,
                col: column,
            },
            end: Position {
                line: 1,
                col: column + 1,
            },
        });
        assert_eq!(resolved.source_span.start.line, 1);
        assert_eq!(resolved.source_span.start.col, column);
        assert!(!resolved.source.starts_with('\u{feff}'));
    }

    #[test]
    fn function_macro_body_preserves_non_utf8_bytes() {
        let out = pp_bytes(b"#define PAYLOAD() 'A\xffB'\nx = PAYLOAD()\n");
        assert!(out
            .windows(b"x = 'A\xffB'".len())
            .any(|bytes| bytes == b"x = 'A\xffB'"));
    }

    #[test]
    fn function_macro_body_preserves_utf8_text() {
        let out = pp("#define PAYLOAD() 'é'\nx = PAYLOAD()\n");
        assert!(out.contains("x = 'é'"), "got: {out:?}");
    }

    #[test]
    fn public_preprocess_preserves_reserved_unicode_scalars() {
        let source = "x = '\u{f0000}\u{f01ff}' @\n";
        let result = preprocess(source, &PreprocConfig::default()).unwrap();
        assert_eq!(result.text, source);
        assert_eq!(result.bytes(), source.as_bytes());
        let col = source.find('@').unwrap() as u32 + 1;
        let resolved = result.resolve_span(Span {
            file_id: 0,
            start: Position { line: 1, col },
            end: Position {
                line: 1,
                col: col + 1,
            },
        });
        assert_eq!(resolved.display_span.start.col, col);
    }

    #[test]
    fn byte_preprocess_preserves_reserved_unicode_scalars() {
        let source = "x = '\u{f0000}\u{f01ff}'\n";
        let result = preprocess_bytes(source.as_bytes(), &PreprocConfig::default()).unwrap();
        assert_eq!(result.bytes(), source.as_bytes());
    }

    #[test]
    fn define_empty_macro() {
        let out = pp("#define ENABLED\n#ifdef ENABLED\nyes\n#endif\n");
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn define_empty_macro_expands_to_nothing() {
        // Per cpp standard: #define GUARD with no body expands to empty, not "1".
        let out = pp("#define GUARD\nx = GUARD end\n");
        assert!(out.contains("x =  end"), "got: {:?}", out);
    }

    #[test]
    fn undef_removes_macro() {
        let out = pp("#define FOO 1\n#undef FOO\n#ifdef FOO\nyes\n#else\nno\n#endif\n");
        assert!(lines(&out).contains(&"no"));
    }

    #[test]
    fn macro_expands_to_macro() {
        // A expands to B, B expands to 42. Recursive expansion required.
        let out = pp("#define A B\n#define B 42\nx = A\n");
        assert!(out.contains("x = 42"), "got: {:?}", out);
    }

    #[test]
    fn recursive_expansion_three_levels() {
        let out = pp("#define X Y\n#define Y Z\n#define Z 99\nval = X\n");
        assert!(out.contains("val = 99"), "got: {:?}", out);
    }

    #[test]
    fn self_referencing_macro_stops() {
        // A macro referencing itself must not infinite-loop.
        // Blue paint: FOO is marked as expanding, so inner FOO is not re-expanded.
        let out = pp("#define FOO FOO + 1\nx = FOO\n");
        assert!(out.contains("x = FOO + 1"), "got: {:?}", out);
    }

    #[test]
    fn mutual_recursion_stops() {
        // A → B, B → A. Must not infinite-loop.
        let out = pp("#define A B\n#define B A\nx = A\n");
        // A→B→A(blocked), so result is "A"
        assert!(out.contains("x = A"), "got: {:?}", out);
    }

    // ---- Function-like macros ----

    #[test]
    fn define_and_expand_function_macro() {
        let out = pp("#define MAX(a, b) merge((a), (b), (a) > (b))\nx = MAX(foo, bar)\n");
        assert!(out.contains("x = merge((foo), (bar), (foo) > (bar))"));
    }

    #[test]
    fn function_macro_expands_after_preprocessing_whitespace() {
        let out = pp("#define DOUBLE(x) ((x) * 2)\ny = DOUBLE \t (21)\n");
        assert!(
            out.contains("y = ((21) * 2)"),
            "spaced function-like invocation remained unexpanded: {out:?}"
        );
    }

    #[test]
    fn function_macro_no_parens_not_expanded() {
        let out = pp("#define FOO(x) (x+1)\ny = FOO \t + 2\n");
        // No invocation parens after the preprocessing whitespace: preserve
        // both the identifier and the intervening bytes.
        assert!(out.contains("y = FOO \t + 2"), "got: {out:?}");
    }

    #[test]
    fn function_macro_nested_parens() {
        let out = pp("#define F(x) (x)\ny = F(a(b, c))\n");
        assert!(out.contains("y = (a(b, c))"));
    }

    #[test]
    fn function_macro_rejects_missing_arguments() {
        let err = pp_err("#define PAIR(a, b) ((a) + (b))\ny = PAIR(1)\n");
        assert_eq!(err.line, 2);
        assert!(
            err.msg.contains("PAIR")
                && err.msg.contains("requires 2 arguments")
                && err.msg.contains("only 1 given"),
            "unexpected diagnostic: {err}"
        );
    }

    #[test]
    fn function_macro_rejects_extra_arguments() {
        let err = pp_err("#define ID(x) x\ny = ID(1, 2)\n");
        assert_eq!(err.line, 2);
        assert!(
            err.msg.contains("ID")
                && err.msg.contains("passed 2 arguments")
                && err.msg.contains("takes just 1"),
            "unexpected diagnostic: {err}"
        );
    }

    #[test]
    fn function_macro_accepts_zero_and_empty_arguments() {
        let out = pp(concat!(
            "#define ZERO() 7\n",
            "#define EMPTY(x) [x]\n",
            "a = ZERO()\n",
            "b = EMPTY()\n",
        ));
        assert!(out.contains("a = 7"), "got: {out:?}");
        assert!(out.contains("b = []"), "got: {out:?}");
    }

    #[test]
    fn variadic_macro_allows_extra_arguments_but_requires_fixed_parameters() {
        let out = pp("#define V(first, ...) first + __VA_ARGS__\ny = V(1, 2, 3)\n");
        assert!(out.contains("y = 1 + 2, 3"), "got: {out:?}");

        let err = pp_err("#define V(first, second, ...) first + second\ny = V(1)\n");
        assert!(
            err.msg.contains("requires at least 2 arguments") && err.msg.contains("only 1 given"),
            "unexpected diagnostic: {err}"
        );
    }

    #[test]
    fn nested_function_macro_arity_error_uses_invocation_location() {
        let err = pp_err(concat!(
            "#define PAIR(a, b) ((a) + (b))\n",
            "#define WRAP(x) PAIR(x)\n",
            "#line 40 \"virtual.f90\"\n",
            "y = WRAP(1)\n",
        ));
        assert_eq!(err.filename, "virtual.f90");
        assert_eq!(err.line, 40);
        assert!(err.msg.contains("PAIR"), "unexpected diagnostic: {err}");
    }

    #[test]
    fn condition_function_macro_arity_error_is_not_discarded() {
        let err = pp_err("#define PAIR(a, b) ((a) + (b))\n#if PAIR(1)\n#endif\n");
        assert_eq!(err.line, 2);
        assert!(err.msg.contains("PAIR"), "unexpected diagnostic: {err}");
    }

    #[test]
    fn function_macro_large_extra_argument_list_is_rejected_iteratively() {
        let arguments = std::iter::repeat_n("1", 20_000)
            .collect::<Vec<_>>()
            .join(",");
        let err = pp_err(&format!("#define ID(x) x\nvalue = ID({arguments})\n"));
        assert!(
            err.msg.contains("passed 20000 arguments"),
            "unexpected diagnostic: {err}"
        );
    }

    #[test]
    fn function_macro_keeps_comma_inside_double_quoted_argument() {
        let out = pp("#define ID(x) x\nprint *, ID(\"a,b\")\n");
        assert!(out.contains("print *, \"a,b\""), "got: {out:?}");
    }

    #[test]
    fn function_macro_keeps_parenthesis_inside_single_quoted_argument() {
        let out = pp("#define ID(x) x\nprint *, ID('a)b')\n");
        assert!(out.contains("print *, 'a)b'"), "got: {out:?}");
    }

    #[test]
    fn function_macro_honors_doubled_quotes_while_splitting_arguments() {
        let out = pp("#define FIRST(a, b) a\nprint *, FIRST('it''s a,b', 9)\n");
        assert!(out.contains("print *, 'it''s a,b'"), "got: {out:?}");
    }

    #[test]
    fn function_macro_splits_after_quoted_argument() {
        let out = pp("#define SECOND(a, b) b\nprint *, SECOND(\"a,b)\", 42)\n");
        assert!(out.contains("print *, 42"), "got: {out:?}");
    }

    #[test]
    fn function_macro_honors_backslash_escaped_quote_while_splitting_arguments() {
        let out = pp(r#"#define FIRST(a, b) a
print *, FIRST("a\",b", 9)
"#);
        assert!(out.contains(r#"print *, "a\",b""#), "got: {out:?}");
    }

    #[test]
    fn function_macro_preserves_utf8_quoted_argument() {
        let out = pp("#define ID(x) x\nprint *, ID(\"\u{03bb},)\")\n");
        assert!(out.contains("print *, \"\u{03bb},)\""), "got: {out:?}");
    }

    #[test]
    fn function_macro_leaves_unterminated_quoted_invocation_unexpanded() {
        let out = pp("#define ID(x) x\nprint *, ID(\"a,b)\n");
        assert!(out.contains("print *, ID(\"a,b)"), "got: {out:?}");
    }

    #[test]
    fn param_substitution_word_boundary() {
        // Parameter "a" must not match inside "abcdef".
        let out = pp("#define F(a) abcdef + a\ny = F(99)\n");
        assert!(out.contains("y = abcdef + 99"), "got: {:?}", out);
    }

    #[test]
    fn param_no_double_substitution() {
        // F(a, b) with body "a + b": calling F(b, a) should give "b + a", not "a + a".
        let out = pp("#define F(a, b) a + b\ny = F(b, a)\n");
        assert!(out.contains("y = b + a"), "got: {:?}", out);
    }

    #[test]
    fn param_name_as_substring_of_another() {
        // Parameter "x" should not match inside "x_extra".
        let out = pp("#define F(x) x_extra + x\ny = F(42)\n");
        assert!(out.contains("y = x_extra + 42"), "got: {:?}", out);
    }

    // ---- Conditionals ----

    #[test]
    fn ifdef_true() {
        let out = pp_with("#ifdef FEAT\nyes\n#endif\n", &[("FEAT", "1")]);
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn ifdef_false() {
        let out = pp("#ifdef FEAT\nyes\n#endif\n");
        assert!(!lines(&out).contains(&"yes"));
    }

    #[test]
    fn ifdef_else() {
        let out = pp("#ifdef FEAT\nyes\n#else\nno\n#endif\n");
        assert!(lines(&out).contains(&"no"));
        assert!(!lines(&out).contains(&"yes"));
    }

    #[test]
    fn ifndef() {
        let out = pp("#ifndef FEAT\nyes\n#endif\n");
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn ifndef_false() {
        let out = pp_with("#ifndef FEAT\nyes\n#endif\n", &[("FEAT", "1")]);
        assert!(!lines(&out).contains(&"yes"));
    }

    #[test]
    fn nested_ifdef() {
        let out = pp_with(
            "#ifdef A\n#ifdef B\nboth\n#else\nonly_a\n#endif\n#endif\n",
            &[("A", "1"), ("B", "1")],
        );
        assert!(lines(&out).contains(&"both"));
    }

    #[test]
    fn nested_ifdef_outer_false() {
        let out = pp("#ifdef A\n#ifdef B\nboth\n#endif\n#endif\n");
        assert!(!lines(&out).contains(&"both"));
    }

    #[test]
    fn if_true() {
        let out = pp("#if 1\nyes\n#endif\n");
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn if_false() {
        let out = pp("#if 0\nyes\n#endif\n");
        assert!(!lines(&out).contains(&"yes"));
    }

    #[test]
    fn if_defined() {
        let out = pp_with("#if defined(FEAT)\nyes\n#endif\n", &[("FEAT", "1")]);
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn if_not_defined() {
        let out = pp("#if defined(FEAT)\nyes\n#else\nno\n#endif\n");
        assert!(lines(&out).contains(&"no"));
    }

    #[test]
    fn if_arithmetic() {
        let out = pp_with(
            "#if MAX > 512\nbig\n#else\nsmall\n#endif\n",
            &[("MAX", "1024")],
        );
        assert!(lines(&out).contains(&"big"));
    }

    #[test]
    fn if_and() {
        let out = pp_with(
            "#if defined(A) && defined(B)\nboth\n#endif\n",
            &[("A", "1"), ("B", "1")],
        );
        assert!(lines(&out).contains(&"both"));
    }

    #[test]
    fn elif() {
        let out = pp_with(
            "#ifdef LINUX\nlinux\n#elif defined(__APPLE__)\napple\n#else\nother\n#endif\n",
            &[], // __APPLE__ is predefined on macOS
        );
        // On macOS, should get "apple".
        #[cfg(target_os = "macos")]
        assert!(lines(&out).contains(&"apple"));
        #[cfg(not(target_os = "macos"))]
        assert!(lines(&out).contains(&"other"));
    }

    #[test]
    fn if_zero_skips_content() {
        let out = pp("#if 0\nskipped code\n#define SHOULD_NOT_EXIST\n#endif\nx\n");
        assert!(!lines(&out).contains(&"skipped code"));
        assert!(lines(&out).contains(&"x"));
    }

    // ---- Predefined macros ----

    #[test]
    fn predefined_armfortas() {
        let out = pp("x = __ARMFORTAS__\n");
        assert!(out.contains("x = 1"));
    }

    #[test]
    fn predefined_aarch64() {
        // Target-pinned since x00: the assertion is about the arm64-macos
        // define set, not about whatever host runs the test suite.
        let target = crate::target::TargetSpec::parse("arm64-macos").unwrap();
        let config = PreprocConfig::for_target(&target);
        let out = preprocess("x = __aarch64__\n", &config).unwrap().text;
        assert!(out.contains("x = 1"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn predefined_apple() {
        let out = pp("#ifdef __APPLE__\nyes\n#endif\n");
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn predefined_line() {
        let out = pp("a\nb\nc = __LINE__\n");
        assert!(out.contains("c = 3"));
    }

    // ---- Fortran-aware behavior ----

    #[test]
    fn no_expansion_in_string_single() {
        let out = pp_with("x = 'FOO is great'\n", &[("FOO", "BAR")]);
        assert!(out.contains("'FOO is great'"));
    }

    #[test]
    fn no_expansion_in_string_double() {
        let out = pp_with("x = \"FOO is great\"\n", &[("FOO", "BAR")]);
        assert!(out.contains("\"FOO is great\""));
    }

    #[test]
    fn no_expansion_in_doubled_quote_string() {
        // Fortran doubled-quote escape: 'it''s' should be preserved intact.
        let out = pp_with("x = 'it''s a FOO'\n", &[("FOO", "BAR")]);
        assert!(out.contains("'it''s a FOO'"), "got: {:?}", out);
    }

    #[test]
    fn doubled_quote_does_not_end_string_early() {
        // Regression test: the '' must not cause early string termination.
        let out = pp_with("x = 'he said ''hello'' there' + FOO\n", &[("FOO", "1")]);
        assert!(out.contains("'he said ''hello'' there'"), "got: {:?}", out);
        assert!(
            out.contains("+ 1"),
            "FOO after string should expand, got: {:?}",
            out
        );
    }

    #[test]
    fn no_expansion_in_comment() {
        let out = pp_with("x = 1 ! FOO comment\n", &[("FOO", "BAR")]);
        assert!(out.contains("! FOO comment"));
    }

    #[test]
    fn c_block_marker_inside_fortran_comment_does_not_hide_following_source() {
        let out = pp("! Handle delimiter /* in prose\ninteger :: still_here\n");
        assert!(
            out.contains("! Handle delimiter /* in prose"),
            "Fortran comment was corrupted: {:?}",
            out
        );
        assert!(
            out.contains("integer :: still_here"),
            "source after Fortran comment was hidden: {:?}",
            out
        );
    }

    #[test]
    fn expansion_before_comment() {
        let out = pp_with("x = FOO ! comment\n", &[("FOO", "42")]);
        assert!(out.contains("x = 42 ! comment"));
    }

    // ---- Error cases ----

    #[test]
    fn error_unterminated_if() {
        let err = pp_err("#ifdef FOO\nstuff\n");
        assert!(err.msg.contains("unterminated"));
    }

    #[test]
    fn error_else_without_if() {
        let err = pp_err("#else\n");
        assert!(err.msg.contains("without matching"));
    }

    #[test]
    fn duplicate_else_is_rejected_for_taken_and_untaken_arms() {
        for condition in ["0", "1"] {
            let source = format!("#if {condition}\nfirst\n#else\nsecond\n#else\nthird\n#endif\n");
            let error = pp_err(&source);
            assert_eq!(error.line, 5);
            assert_eq!(error.msg, "#else after #else");
        }
    }

    #[test]
    fn elif_after_else_is_rejected_before_evaluating_its_expression() {
        for condition in ["0", "1"] {
            let source =
                format!("#if {condition}\nfirst\n#else\nsecond\n#elif 1 / 0\nthird\n#endif\n");
            let error = pp_err(&source);
            assert_eq!(error.line, 5);
            assert_eq!(error.msg, "#elif after #else");
        }
    }

    #[test]
    fn repeated_alternatives_are_rejected_inside_skipped_parents() {
        for (directive, expected) in [
            ("#else", "#else after #else"),
            ("#elif 1", "#elif after #else"),
        ] {
            let source =
                format!("#if 0\n#if 1\nfirst\n#else\nsecond\n{directive}\nthird\n#endif\n#endif\n");
            let error = pp_err(&source);
            assert_eq!(error.line, 6);
            assert_eq!(error.msg, expected);
        }
    }

    #[test]
    fn single_else_inside_skipped_parent_preserves_following_emission() {
        let output =
            pp("#if 0\n#if 1\ndead_first\n#else\ndead_second\n#endif\n#endif\nafter = 1\n");
        assert!(!output.contains("dead_first"), "got: {output:?}");
        assert!(!output.contains("dead_second"), "got: {output:?}");
        assert!(output.contains("after = 1"), "got: {output:?}");
    }

    #[test]
    fn large_conditional_chain_accepts_one_final_else() {
        let mut source = String::with_capacity(20_000 * 8);
        source.push_str("#if 0\n");
        for _ in 0..20_000 {
            source.push_str("#elif 0\n");
        }
        source.push_str("#else\nselected = 1\n#endif\nafter = 1\n");

        let output = pp(&source);
        assert!(output.contains("selected = 1"), "got no selected arm");
        assert!(output.contains("after = 1"), "lost post-conditional source");
    }

    #[test]
    fn repeated_alternative_in_include_reports_included_location() {
        let dir = std::env::temp_dir();
        let name = format!("afs-repeated-else-{}.inc", std::process::id());
        let path = dir.join(&name);
        std::fs::write(
            &path,
            "#if 1\nselected = 1\n#else\nfirst_else = 1\n#else\nsecond_else = 1\n#endif\n",
        )
        .unwrap();
        let config = PreprocConfig {
            include_paths: vec![dir],
            ..PreprocConfig::default()
        };
        let error = preprocess(&format!("#include \"{name}\"\n"), &config).unwrap_err();
        assert_eq!(error.filename, path.to_string_lossy());
        assert_eq!(error.line, 5);
        assert_eq!(error.msg, "#else after #else");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn error_endif_without_if() {
        let err = pp_err("#endif\n");
        assert!(err.msg.contains("without matching"));
    }

    #[test]
    fn error_directive() {
        let err = pp_err("#error something went wrong\n");
        assert!(err.msg.contains("something went wrong"));
    }

    // ---- Practical Fortran patterns ----

    #[test]
    fn fortsh_style_ifdef() {
        let src = "\
module test
#ifdef USE_C_STRINGS
    use c_string_module
#else
    ! pure Fortran strings
#endif
    implicit none
end module
";
        let out = pp(src);
        // USE_C_STRINGS not defined, should get the else branch.
        assert!(out.contains("! pure Fortran strings"));
        assert!(!out.contains("use c_string_module"));
    }

    #[test]
    fn fortsh_style_apple_guard() {
        let src = "\
#ifdef __APPLE__
    call macos_specific()
#else
    call linux_specific()
#endif
";
        let out = pp(src);
        #[cfg(target_os = "macos")]
        {
            assert!(out.contains("call macos_specific()"));
            assert!(!out.contains("call linux_specific()"));
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(!out.contains("call macos_specific()"));
            assert!(out.contains("call linux_specific()"));
        }
    }

    #[test]
    fn directive_between_free_form_continuations_stays_a_directive() {
        let src = "\
program p
  integer :: x
  x = 1 + &
#if FLAG
    2 + &
#endif
    3
end program
";
        let out = pp(src);
        assert!(
            out.contains("x = 1 + &"),
            "continued line head should remain intact: {:?}",
            out
        );
        assert!(
            out.contains("    3"),
            "continued line tail should remain after the stripped directive block: {:?}",
            out
        );
        assert!(
            !out.contains("#if FLAG") && !out.contains("2 + &") && !out.contains("#endif"),
            "false branch should remain removed: {:?}",
            out
        );
    }

    #[test]
    fn source_map_preserves_line_numbers() {
        let config = PreprocConfig::default();
        let result = preprocess("a\n#define X 1\nb\nc\n", &config).unwrap();
        // Line 1 is "a", line 2 is #define (blank), line 3 is "b", line 4 is "c".
        assert_eq!(result.source_map.len(), 4);
        assert_eq!(result.source_map[0].line, 1);
        assert_eq!(result.source_map[2].line, 3);
    }

    fn diagnostic_span(line: u32, col: u32) -> Span {
        Span {
            file_id: 0,
            start: Position { line, col },
            end: Position { line, col: col + 1 },
        }
    }

    #[test]
    fn source_map_resolves_free_form_continuation_fragments() {
        let config = PreprocConfig {
            filename: "continuation.f90".into(),
            ..PreprocConfig::default()
        };
        let result = preprocess("x = 1 + &\n    @\n", &config).unwrap();
        let generated_col = result.text.lines().next().unwrap().find('@').unwrap() as u32 + 1;
        let resolved = result.resolve_span(diagnostic_span(1, generated_col));

        assert_eq!(resolved.filename, "continuation.f90");
        assert_eq!(resolved.display_span.start, Position { line: 2, col: 5 });
        assert_eq!(resolved.source_span.start, Position { line: 2, col: 5 });
        assert_eq!(resolved.source.lines().nth(1), Some("    @"));
    }

    #[test]
    fn source_map_preserves_columns_after_macro_expansion() {
        let config = PreprocConfig {
            filename: "macro.f90".into(),
            ..PreprocConfig::default()
        };
        let result = preprocess("#define LONG 123456\nx = LONG + @\n", &config).unwrap();
        let generated_col = result.text.lines().nth(1).unwrap().find('@').unwrap() as u32 + 1;
        let resolved = result.resolve_span(diagnostic_span(2, generated_col));

        assert_eq!(resolved.display_span.start, Position { line: 2, col: 12 });
        assert_eq!(resolved.source_span.start, Position { line: 2, col: 12 });
    }

    #[test]
    fn source_map_preserves_function_macro_argument_origins() {
        let config = PreprocConfig {
            filename: "macro-arg.f90".into(),
            ..PreprocConfig::default()
        };
        let result = preprocess("#define ID(x) x\nv = ID \t (   @ )\n", &config).unwrap();
        let generated_col = result.text.lines().nth(1).unwrap().find('@').unwrap() as u32 + 1;
        let resolved = result.resolve_span(diagnostic_span(2, generated_col));

        assert_eq!(resolved.display_span.start, Position { line: 2, col: 14 });
        assert_eq!(resolved.source_span.start, Position { line: 2, col: 14 });
    }

    #[test]
    fn source_map_anchors_macro_body_text_to_the_invocation() {
        let config = PreprocConfig {
            filename: "macro-body.f90".into(),
            ..PreprocConfig::default()
        };
        let result = preprocess("#define BAD @\nv = BAD\n", &config).unwrap();
        let generated_col = result.text.lines().nth(1).unwrap().find('@').unwrap() as u32 + 1;
        let resolved = result.resolve_span(diagnostic_span(2, generated_col));

        assert_eq!(resolved.display_span.start, Position { line: 2, col: 5 });
        assert_eq!(resolved.source_span.start, Position { line: 2, col: 5 });
    }

    #[test]
    fn source_map_resolves_exclusive_end_before_a_continuation_boundary() {
        let result = preprocess("ab&\n  &cd\n", &PreprocConfig::default()).unwrap();
        let resolved = result.resolve_span(Span {
            file_id: 0,
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 3 },
        });

        assert_eq!(resolved.source_span.start, Position { line: 1, col: 1 });
        assert_eq!(resolved.source_span.end, Position { line: 1, col: 3 });
    }

    #[test]
    fn source_map_scales_across_many_continuation_fragments() {
        let fragments = 2048;
        let mut source = String::from("x = &\n");
        for _ in 0..fragments {
            source.push_str("  &a&\n");
        }
        source.push_str("  &@\n");

        let result = preprocess(&source, &PreprocConfig::default()).unwrap();
        let generated_col = result.text.lines().next().unwrap().find('@').unwrap() as u32 + 1;
        let resolved = result.resolve_span(diagnostic_span(1, generated_col));

        assert_eq!(resolved.source_span.start.line, fragments + 2);
        assert_eq!(resolved.source_span.start.col, 4);
        assert!(result.source_map[0].runs.len() <= fragments as usize + 2);
    }

    // ---- Expression evaluator ----

    #[test]
    fn eval_simple_true() {
        assert!(eval_expr("1").unwrap());
    }

    #[test]
    fn eval_simple_false() {
        assert!(!eval_expr("0").unwrap());
    }

    #[test]
    fn eval_comparison_gt() {
        assert!(eval_expr("1024 > 512").unwrap());
    }

    #[test]
    fn eval_comparison_eq() {
        assert!(eval_expr("42 == 42").unwrap());
    }

    #[test]
    fn eval_logical_and() {
        assert!(eval_expr("1 && 1").unwrap());
        assert!(!eval_expr("1 && 0").unwrap());
    }

    #[test]
    fn eval_logical_or() {
        assert!(eval_expr("0 || 1").unwrap());
        assert!(!eval_expr("0 || 0").unwrap());
    }

    #[test]
    fn eval_logical_operators_short_circuit_arithmetic_faults() {
        assert_eq!(ConditionExprParser::new("0 && (1 / 0)").parse().unwrap(), 0);
        assert_eq!(ConditionExprParser::new("1 || (1 % 0)").parse().unwrap(), 1);
        assert_eq!(
            ConditionExprParser::new("0 && (1 / 0) || 1")
                .parse()
                .unwrap(),
            1
        );
        assert_eq!(
            ConditionExprParser::new("1 || 0 && (1 / 0)")
                .parse()
                .unwrap(),
            1
        );
        assert_eq!(
            ConditionExprParser::new("1 || 1 / 0 && 0").parse().unwrap(),
            1
        );
        assert_eq!(
            ConditionExprParser::new("0 && (1 / 0) || (1 / 0)")
                .parse()
                .unwrap_err(),
            "division by zero in #if expression"
        );
    }

    #[test]
    fn eval_short_circuited_operands_still_require_valid_syntax() {
        assert_eq!(
            ConditionExprParser::new("0 && (1 / )").parse().unwrap_err(),
            "unexpected token in #if expression: ')'"
        );
        assert_eq!(
            ConditionExprParser::new("1 || (1 % 0").parse().unwrap_err(),
            "unmatched parenthesis in #if expression"
        );
    }

    #[test]
    fn eval_large_short_circuit_chain_is_iterative() {
        let mut expression = String::from("1");
        for _ in 0..20_000 {
            expression.push_str(" || (1 / 0)");
        }
        assert_eq!(ConditionExprParser::new(&expression).parse().unwrap(), 1);
    }

    #[test]
    fn if_logical_operators_short_circuit_dead_arithmetic() {
        let output = preprocess(
            "#if 0 && (1 / 0)\ndead_and\n#else\nand_ok\n#endif\n\
             #if 1 || (1 % 0)\nor_ok\n#else\ndead_or\n#endif\n",
            &PreprocConfig::default(),
        )
        .unwrap()
        .text;
        let output_lines = lines(&output);
        assert!(output_lines.contains(&"and_ok"), "got: {output:?}");
        assert!(output_lines.contains(&"or_ok"), "got: {output:?}");
        assert!(!output.contains("dead_and"), "got: {output:?}");
        assert!(!output.contains("dead_or"), "got: {output:?}");
    }

    #[test]
    fn eval_not() {
        assert!(eval_expr("!0").unwrap());
        assert!(!eval_expr("!1").unwrap());
    }

    #[test]
    fn eval_parenthesized() {
        assert!(eval_expr("(1 || 0) && 1").unwrap());
        assert!(!eval_expr("1 && (0 || 0)").unwrap());
    }

    #[test]
    fn eval_hex() {
        assert!(eval_expr("0xFF > 200").unwrap());
    }

    // Arithmetic
    #[test]
    fn eval_addition() {
        assert!(eval_expr("3 + 4").unwrap()); // 7 != 0
        assert!(eval_expr("100 + 200 > 250").unwrap());
    }

    #[test]
    fn eval_subtraction() {
        assert!(eval_expr("10 - 5 > 0").unwrap());
        assert!(!eval_expr("5 - 10 > 0").unwrap());
    }

    #[test]
    fn eval_multiplication() {
        assert!(eval_expr("6 * 7 == 42").unwrap());
    }

    #[test]
    fn eval_division() {
        assert!(eval_expr("42 / 6 == 7").unwrap());
    }

    #[test]
    fn eval_modulo() {
        assert!(eval_expr("10 % 3 == 1").unwrap());
    }

    #[test]
    fn eval_unary_minus() {
        assert!(eval_expr("-1 < 0").unwrap());
        assert!(eval_expr("-(-1) > 0").unwrap());
    }

    #[test]
    fn eval_complex_arithmetic() {
        // (1024 + 1) > 512
        assert!(eval_expr("1024 + 1 > 512").unwrap());
    }

    #[test]
    fn eval_precedence() {
        // 2 + 3 * 4 = 14 (not 20)
        assert!(eval_expr("2 + 3 * 4 == 14").unwrap());
    }

    #[test]
    fn eval_mixed_arithmetic_is_left_associative() {
        assert!(eval_expr("10 + 5 - 2 == 13").unwrap());
        assert!(eval_expr("48 / 4 * 2 % 5 == 4").unwrap());
        assert!(eval_expr("1 - -2 == 3").unwrap());
    }

    #[test]
    fn eval_relational_operators_bind_before_equality() {
        assert!(eval_expr("1 == 2 < 3").unwrap());
        assert!(!eval_expr("3 < 4 == 0").unwrap());
    }

    #[test]
    fn eval_integer_overflow_wraps_without_panicking() {
        assert_eq!(
            ConditionExprParser::new("9223372036854775807 + 1")
                .parse()
                .unwrap(),
            i64::MIN
        );
        assert_eq!(
            ConditionExprParser::new("-9223372036854775807 - 2")
                .parse()
                .unwrap(),
            i64::MAX
        );
        assert_eq!(
            ConditionExprParser::new("3037000500 * 3037000500")
                .parse()
                .unwrap(),
            -9_223_372_036_709_301_616
        );
        assert_eq!(
            ConditionExprParser::new("(-9223372036854775807 - 1) / -1")
                .parse()
                .unwrap(),
            i64::MIN
        );
        assert_eq!(
            ConditionExprParser::new("(-9223372036854775807 - 1) % -1")
                .parse()
                .unwrap(),
            0
        );
        assert_eq!(
            ConditionExprParser::new("-(-9223372036854775807 - 1)")
                .parse()
                .unwrap(),
            i64::MIN
        );
    }

    #[test]
    fn eval_reports_arithmetic_and_syntax_errors() {
        assert_eq!(
            ConditionExprParser::new("1 / 0").parse().unwrap_err(),
            "division by zero in #if expression"
        );
        assert_eq!(
            ConditionExprParser::new("1 % 0").parse().unwrap_err(),
            "modulo by zero in #if expression"
        );
        assert_eq!(
            ConditionExprParser::new("(1 + 2").parse().unwrap_err(),
            "unmatched parenthesis in #if expression"
        );
        assert_eq!(
            ConditionExprParser::new("1 + 2 trailing")
                .parse()
                .unwrap_err(),
            "unexpected token in #if expression: 'trailing'"
        );
    }

    #[test]
    fn if_with_arithmetic() {
        let out = pp_with(
            "#if MAX + 1 > 512\nbig\n#else\nsmall\n#endif\n",
            &[("MAX", "1024")],
        );
        assert!(lines(&out).contains(&"big"));
    }

    #[test]
    fn if_with_mixed_arithmetic_selects_the_true_branch() {
        let out = pp(
            "#if 10 + 5 - 2 == 13\nselected\n#else\n#error arithmetic precedence broken\n#endif\n",
        );
        assert!(lines(&out).contains(&"selected"));
    }

    #[test]
    fn if_overflow_remains_evaluable() {
        let out = pp("#if 9223372036854775807 + 1\nselected\n#endif\n");
        assert!(lines(&out).contains(&"selected"));
    }

    #[test]
    fn if_chained_macros() {
        // A -> 1, B -> A. #if B should expand B->A->1, evaluating to true.
        let out = pp_with("#if B\nyes\n#endif\n", &[("A", "1"), ("B", "A")]);
        assert!(
            lines(&out).contains(&"yes"),
            "chained macro in #if failed, got: {:?}",
            lines(&out)
        );
    }

    #[test]
    fn if_chained_three_levels() {
        let out = pp_with(
            "#if C > 10\nyes\n#endif\n",
            &[("A", "42"), ("B", "A"), ("C", "B")],
        );
        assert!(
            lines(&out).contains(&"yes"),
            "3-level chain in #if failed, got: {:?}",
            lines(&out)
        );
    }

    #[test]
    fn if_defined_and_value() {
        // Common real-world pattern: #if defined(FOO) && FOO > 5
        let out = pp_with(
            "#if defined(FOO) && FOO > 5\nyes\n#endif\n",
            &[("FOO", "10")],
        );
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn if_function_macro_expands() {
        let out = pp("#define INC(x) ((x) + 1)\n#if INC(41) > 41\nyes\n#endif\n");
        assert!(
            lines(&out).contains(&"yes"),
            "function macro in #if failed, got: {:?}",
            lines(&out)
        );
    }

    #[test]
    fn if_function_macro_expands_after_preprocessing_whitespace() {
        let out = pp("#define INC(x) ((x) + 1)\n#if INC \t (41) > 41\nyes\n#endif\n");
        assert!(
            lines(&out).contains(&"yes"),
            "spaced function macro in #if failed, got: {:?}",
            lines(&out)
        );
    }

    #[test]
    fn if_object_macro_can_expand_into_function_macro() {
        let out = pp(
            "#define INC(x) ((x) + 1)\n#define WRAP(x) INC(x)\n#if WRAP(41) > 41\nyes\n#endif\n",
        );
        assert!(
            lines(&out).contains(&"yes"),
            "object->function macro chain in #if failed, got: {:?}",
            lines(&out)
        );
    }

    #[test]
    fn if_not_defined_with_bang() {
        let out = pp("#if !defined(NOPE)\nyes\n#endif\n");
        assert!(lines(&out).contains(&"yes"));
    }

    // ---- Variadic macros ----

    #[test]
    fn variadic_macro() {
        let out = pp("#define DBG(fmt, ...) write(0, fmt) __VA_ARGS__\nx = DBG(a, b, c)\n");
        assert!(out.contains("x = write(0, a) b, c"));
    }

    #[test]
    fn variadic_macro_no_extra_args() {
        let out = pp("#define DBG(fmt, ...) write(0, fmt) __VA_ARGS__\nx = DBG(a)\n");
        assert!(out.contains("x = write(0, a) "));
    }

    // ---- Stringification ----

    #[test]
    fn stringification() {
        let out = pp("#define STR(x) #x\ny = STR(hello)\n");
        assert!(out.contains("y = \"hello\""));
    }

    #[test]
    fn stringification_with_spaces() {
        let out = pp("#define STR(x) #x\ny = STR(a + b)\n");
        assert!(out.contains("y = \"a + b\""));
    }

    #[test]
    fn direct_stringification_uses_raw_argument() {
        let out = pp("#define VERSION 0.13.0\n#define STR(x) #x\ny = STR(VERSION)\n");
        assert!(out.contains("y = \"VERSION\""));
    }

    #[test]
    fn two_step_stringification_prescans_argument() {
        let out = pp("#define VERSION 0.13.0\n\
             #define STR_(x) #x\n\
             #define STR(x) STR_(x)\n\
             y = STR(VERSION)\n");
        assert!(out.contains("y = \"0.13.0\""), "got: {out}");
    }

    // ---- Token pasting ----

    #[test]
    fn token_pasting() {
        let out = pp("#define PASTE(a, b) a ## b\nx = PASTE(foo, bar)\n");
        assert!(out.contains("x = foobar"));
    }

    #[test]
    fn token_pasting_with_numbers() {
        let out = pp("#define VAR(n) var_ ## n\nx = VAR(42)\n");
        assert!(out.contains("x = var_42"));
    }

    // ---- Backslash continuation ----

    #[test]
    fn backslash_continuation_in_define() {
        let out = pp("#define LONG_MACRO \\\n    42\nx = LONG_MACRO\n");
        assert!(out.contains("x = 42"));
    }

    #[test]
    fn backslash_continuation_multiline() {
        let out = pp("#define M(a, b) \\\n    ((a) + \\\n     (b))\nx = M(1, 2)\n");
        // After continuation joining, the define body is "    ((a) +      (b))"
        // with leading/trailing whitespace from the continuation lines.
        // The body gets trimmed during define processing, so it becomes "((a) +      (b))".
        assert!(
            out.contains("((1) +"),
            "got: {:?}",
            out.lines().collect::<Vec<_>>()
        );
        assert!(out.contains("(2))"));
    }

    #[test]
    fn line_number_correct_after_continuation() {
        // Lines 1-3 are a continued #define. Line 4 should report __LINE__ = 4.
        let out = pp("#define M \\\n    42\na\nb = __LINE__\n");
        assert!(
            out.contains("b = 4"),
            "got: {:?}",
            out.lines().collect::<Vec<_>>()
        );
    }

    // ---- Self-referencing macro (no infinite loop) ----

    #[test]
    fn self_referencing_macro_no_loop() {
        // A macro that expands to its own name should not infinite-loop.
        // cpp standard: a macro is not re-expanded during its own expansion.
        // Our simple implementation does one pass, so this works naturally.
        let out = pp("#define FOO FOO + 1\nx = FOO\n");
        assert!(out.contains("x = FOO + 1") || out.contains("x = FOO"));
    }

    // ---- #if 0 does not process directives inside ----

    #[test]
    fn if_zero_does_not_include() {
        // #include inside #if 0 must not try to open the file.
        let out = pp("#if 0\n#include \"nonexistent_file.h\"\n#endif\nok\n");
        assert!(lines(&out).contains(&"ok"));
    }

    #[test]
    fn if_zero_does_not_define() {
        let out = pp("#if 0\n#define SECRET 42\n#endif\n#ifdef SECRET\nyes\n#else\nno\n#endif\n");
        assert!(lines(&out).contains(&"no"));
    }

    // ---- Deeply nested conditionals ----

    #[test]
    fn deeply_nested_conditionals() {
        let src = "\
#if 1
#if 1
#if 1
#if 1
deep
#endif
#endif
#endif
#endif
";
        let out = pp(src);
        assert!(lines(&out).contains(&"deep"));
    }

    // ---- Unknown directives ----

    #[test]
    fn unknown_directive_is_rejected_in_active_source() {
        for directive in ["incldue", "Include", "not_a_directive"] {
            let error = pp_err(&format!("before\n#{directive} \"missing.inc\"\nafter\n"));
            assert_eq!(error.filename, "<input>");
            assert_eq!(error.line, 2);
            assert_eq!(
                error.msg,
                format!("unknown preprocessing directive #{directive}")
            );
        }
    }

    #[test]
    fn unknown_directive_is_ignored_in_skipped_conditional_group() {
        let output = pp("#if 0\n#incldue \"missing.inc\"\n#endif\nafter = 1\n");
        assert!(output.contains("after = 1"), "got: {output:?}");
    }

    #[test]
    fn many_unknown_directives_in_skipped_group_remain_linear_and_inert() {
        let mut source = String::with_capacity(50_000 * 20);
        source.push_str("#if 0\n");
        for _ in 0..50_000 {
            source.push_str("#incldue \"missing\"\n");
        }
        source.push_str("#endif\nafter = 1\n");

        let output = pp(&source);
        assert!(output.contains("after = 1"), "lost following source");
    }

    #[test]
    fn huge_unknown_directive_has_a_bounded_diagnostic() {
        let directive = "x".repeat(256 * 1024);
        let error = pp_err(&format!("#{directive}\n"));
        assert_eq!(
            error.msg,
            format!("unknown preprocessing directive #{}...", "x".repeat(80))
        );
    }

    #[test]
    fn pragma_directive_remains_an_explicit_no_op() {
        let output = pp("#pragma once\nafter = 1\n");
        assert!(output.contains("after = 1"), "got: {output:?}");
    }

    #[test]
    fn gnu_numeric_linemarker_remains_a_recognized_directive() {
        let result = preprocess(
            "# 700 \"virtual.f90\" 1\nx = __LINE__\n",
            &PreprocConfig::default(),
        )
        .unwrap();
        assert_eq!(result.source_map[1].filename, "virtual.f90");
        assert_eq!(result.source_map[1].line, 700);
        assert!(result.text.contains("x = 700"), "got: {:?}", result.text);
    }

    #[test]
    fn unknown_directive_uses_reported_location() {
        let error = pp_err("#line 700 \"virtual.f90\"\n#incldue \"missing.inc\"\n");
        assert_eq!(error.filename, "virtual.f90");
        assert_eq!(error.line, 700);
        assert_eq!(error.msg, "unknown preprocessing directive #incldue");
    }

    #[test]
    fn unknown_directive_in_include_reports_included_location() {
        let dir = std::env::temp_dir();
        let name = format!("afs-unknown-directive-{}.inc", std::process::id());
        let path = dir.join(&name);
        std::fs::write(&path, "before\n#incldue \"missing.inc\"\nafter\n").unwrap();
        let config = PreprocConfig {
            include_paths: vec![dir],
            ..PreprocConfig::default()
        };

        let error = preprocess(&format!("#include \"{name}\"\n"), &config).unwrap_err();
        assert_eq!(error.filename, path.to_string_lossy());
        assert_eq!(error.line, 2);
        assert_eq!(error.msg, "unknown preprocessing directive #incldue");
        let _ = std::fs::remove_file(path);
    }

    // ---- Null directive ----

    #[test]
    fn null_directive() {
        // Bare # on a line is valid (null directive).
        let out = pp("#\nok\n");
        assert!(lines(&out).contains(&"ok"));
    }

    // ---- #include with actual file content ----

    #[test]
    fn include_injects_file_content() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let inc_path = dir.join("test_pp_include.inc");
        let mut f = std::fs::File::create(&inc_path).unwrap();
        writeln!(f, "integer :: included_var").unwrap();
        drop(f);

        let mut config = PreprocConfig::default();
        config.include_paths.push(dir);
        let src = "#include \"test_pp_include.inc\"\nreal :: x\n";
        let result = preprocess(src, &config).unwrap();
        assert!(
            result.text.contains("integer :: included_var"),
            "got: {:?}",
            result.text
        );
        assert!(result.text.contains("real :: x"));
    }

    #[test]
    fn fortran_include_injects_file_content_and_tracks_dependency() {
        let dir = std::env::temp_dir();
        let include_name = format!("afs-fortran-include-{}.inc", std::process::id());
        let include_path = dir.join(&include_name);
        std::fs::write(&include_path, "integer, parameter :: answer = 42\n").unwrap();

        let config = PreprocConfig {
            include_paths: vec![dir],
            ..PreprocConfig::default()
        };
        let source = format!("include '{include_name}' ! ordinary Fortran include\nreal :: x\n");
        let result = preprocess(&source, &config).unwrap();

        assert!(
            result.text.contains("integer, parameter :: answer = 42"),
            "INCLUDE content was not injected: {:?}",
            result.text
        );
        assert!(result.text.contains("real :: x"));
        assert_eq!(result.included_files, vec![include_path.clone()]);
        assert_eq!(result.source_map.len(), 3);
        assert_eq!(
            result.source_map[0].filename,
            include_path.to_string_lossy()
        );
        assert_eq!(result.source_map[0].line, 1);
        assert_eq!(result.source_map[2].filename, "<input>");
        assert_eq!(result.source_map[2].line, 2);

        let _ = std::fs::remove_file(include_path);
    }

    #[test]
    fn fortran_include_accepts_reference_free_and_fixed_forms() {
        let dir = std::env::temp_dir();
        let include_name = format!("afs-fortran-include-forms-{}.inc", std::process::id());
        let include_path = dir.join(&include_name);
        std::fs::write(
            &include_path,
            "      integer, parameter :: included_value = 42\n",
        )
        .unwrap();

        let free_config = PreprocConfig {
            include_paths: vec![dir.clone()],
            ..PreprocConfig::default()
        };
        for source in [
            format!("INCLUDE \"{include_name}\" ! trailing comment\n"),
            format!("include'{include_name}'\n"),
        ] {
            let result = preprocess(&source, &free_config).unwrap();
            assert!(
                result.text.contains("included_value = 42"),
                "INCLUDE content was not injected for {source:?}: {:?}",
                result.text
            );
            assert_eq!(result.included_files, vec![include_path.clone()]);
        }

        let fixed_config = PreprocConfig {
            include_paths: vec![dir],
            fixed_form: true,
            ..PreprocConfig::default()
        };
        let fixed_source = format!("      I N C L U D E '{include_name}'\n");
        let result = preprocess(&fixed_source, &fixed_config).unwrap();
        assert!(
            result.text.contains("included_value = 42"),
            "fixed-form INCLUDE content was not injected: {:?}",
            result.text
        );
        assert_eq!(result.included_files, vec![include_path.clone()]);

        let _ = std::fs::remove_file(include_path);
    }

    #[test]
    fn fortran_include_path_can_be_produced_by_a_macro() {
        let dir = std::env::temp_dir();
        let include_name = format!("afs-fortran-include-macro-{}.inc", std::process::id());
        let include_path = dir.join(&include_name);
        std::fs::write(&include_path, "integer, parameter :: macro_value = 7\n").unwrap();

        let mut config = PreprocConfig {
            include_paths: vec![dir],
            ..PreprocConfig::default()
        };
        config.defines.insert(
            "INCLUDE_FILE".into(),
            MacroDef::object(&format!("\"{include_name}\"")),
        );
        let result = preprocess("include INCLUDE_FILE\n", &config).unwrap();

        assert!(result.text.contains("macro_value = 7"));
        assert_eq!(result.included_files, vec![include_path.clone()]);

        let _ = std::fs::remove_file(include_path);
    }

    #[test]
    fn fortran_include_recognition_preserves_neighboring_source() {
        assert_eq!(
            parse_fortran_include_path("include_file = 3", false).unwrap(),
            None
        );
        assert_eq!(
            parse_fortran_include_path("10 include 'decl.inc'", false).unwrap(),
            None
        );
        assert_eq!(
            parse_fortran_include_path("12345 INCLUDE 'decl.inc'", true).unwrap(),
            None
        );
        assert_eq!(
            parse_fortran_include_path("     1INCLUDE 'decl.inc'", true).unwrap(),
            None
        );
        assert_eq!(
            parse_fortran_include_path("include 'a''b.inc'", false).unwrap(),
            Some("a'b.inc".into())
        );

        let result = preprocess(
            "#if 0\ninclude 'missing-inactive-file.inc'\n#endif\ninclude_file = 3\n",
            &PreprocConfig::default(),
        )
        .unwrap();
        assert!(result.text.contains("include_file = 3"));
        assert!(result.included_files.is_empty());
    }

    #[test]
    fn malformed_fortran_include_is_diagnosed_at_its_reported_location() {
        let error = preprocess(
            "#line 40 \"virtual.f90\"\ninclude 'decl.inc'; print *, 1\n",
            &PreprocConfig::default(),
        )
        .unwrap_err();
        assert_eq!(error.filename, "virtual.f90");
        assert_eq!(error.line, 40);
        assert_eq!(
            error.msg,
            "unexpected text after Fortran INCLUDE path: ; print *, 1"
        );

        let error =
            preprocess("include 'unterminated.inc\n", &PreprocConfig::default()).unwrap_err();
        assert_eq!(error.msg, "unterminated Fortran INCLUDE string");

        let error = preprocess(
            "include &\n  & 'continued.inc'\n",
            &PreprocConfig::default(),
        )
        .unwrap_err();
        assert_eq!(error.msg, "Fortran INCLUDE line cannot be continued");
    }

    #[test]
    fn include_defines_propagate() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let inc_path = dir.join("test_pp_define.inc");
        let mut f = std::fs::File::create(&inc_path).unwrap();
        writeln!(f, "#define INCLUDED_VAL 99").unwrap();
        drop(f);

        let mut config = PreprocConfig::default();
        config.include_paths.push(dir);
        let src = "#include \"test_pp_define.inc\"\nx = INCLUDED_VAL\n";
        let result = preprocess(src, &config).unwrap();
        assert!(result.text.contains("x = 99"), "got: {:?}", result.text);
    }

    #[test]
    fn include_files_cannot_close_or_mutate_parent_conditionals() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let cases = [
            (
                "endif",
                "#endif\n",
                "#if 1\n#include \"{name}\"\nparent = 1\n",
                "#endif",
            ),
            (
                "else",
                "#else\nchild_else = 1\n",
                "#if 1\n#include \"{name}\"\nparent = 1\n#endif\n",
                "#else",
            ),
            (
                "elif",
                "#elif 0\nchild_elif = 1\n",
                "#if 1\n#include \"{name}\"\nparent = 1\n#endif\n",
                "#elif",
            ),
        ];

        for (case, included, parent_template, directive) in cases {
            let name = format!("afs-cond-boundary-{case}-{pid}.inc");
            let path = dir.join(&name);
            std::fs::write(&path, included).unwrap();
            let config = PreprocConfig {
                include_paths: vec![dir.clone()],
                ..PreprocConfig::default()
            };
            let parent = parent_template.replace("{name}", &name);
            let error = preprocess(&parent, &config).unwrap_err();
            assert_eq!(error.filename, path.to_string_lossy());
            assert_eq!(error.line, 1);
            assert!(
                error.msg.contains(directive) && error.msg.contains("outside this include file"),
                "unexpected diagnostic: {error}"
            );
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn include_files_cannot_leave_conditionals_for_the_parent_to_close() {
        let dir = std::env::temp_dir();
        let name = format!("afs-cond-open-{}.inc", std::process::id());
        let path = dir.join(&name);
        std::fs::write(&path, "#if 1\nincluded = 1\n").unwrap();
        let config = PreprocConfig {
            include_paths: vec![dir],
            ..PreprocConfig::default()
        };
        let parent = format!("#include \"{name}\"\n#endif\nparent = 1\n");
        let error = preprocess(&parent, &config).unwrap_err();
        assert_eq!(error.filename, path.to_string_lossy());
        assert_eq!(error.line, 2);
        assert!(
            error.msg.contains("unterminated")
                && error.msg.contains("include file")
                && error.msg.contains("1 level"),
            "unexpected diagnostic: {error}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn balanced_include_conditionals_preserve_the_parent_state() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let name = format!("afs-cond-balanced-{pid}.inc");
        let nested_name = format!("afs-cond-balanced-nested-{pid}.inc");
        let path = dir.join(&name);
        let nested_path = dir.join(&nested_name);
        std::fs::write(
            &path,
            format!("#if 1\nincluded = 1\n#include \"{nested_name}\"\n#endif\n"),
        )
        .unwrap();
        std::fs::write(&nested_path, "#if 1\nnested = 1\n#endif\n").unwrap();
        let config = PreprocConfig {
            include_paths: vec![dir],
            ..PreprocConfig::default()
        };
        let parent = format!("#if 1\n#include \"{name}\"\nparent = 1\n#endif\nafter = 1\n");
        let output = preprocess(&parent, &config).unwrap().text;
        assert!(output.contains("included = 1"), "got: {output:?}");
        assert!(output.contains("nested = 1"), "got: {output:?}");
        assert!(output.contains("parent = 1"), "got: {output:?}");
        assert!(output.contains("after = 1"), "got: {output:?}");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(nested_path);
    }

    #[test]
    fn file_macro_restored_after_include() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let inc_path = dir.join("test_pp_file_restore.inc");
        let mut f = std::fs::File::create(&inc_path).unwrap();
        writeln!(f, "! included").unwrap();
        drop(f);

        let mut config = PreprocConfig::default();
        config.include_paths.push(dir);
        config.filename = "parent.f90".into();
        let src = "before = __FILE__\n#include \"test_pp_file_restore.inc\"\nafter = __FILE__\n";
        let result = preprocess(src, &config).unwrap();
        assert!(
            result.text.contains("before = \"parent.f90\""),
            "got: {:?}",
            result.text
        );
        assert!(
            result.text.contains("after = \"parent.f90\""),
            "__FILE__ not restored, got: {:?}",
            result.text
        );
    }

    // ---- Fixed-form awareness ----

    #[test]
    fn fixed_form_comment_not_expanded() {
        let mut config = PreprocConfig {
            fixed_form: true,
            ..PreprocConfig::default()
        };
        config.defines.insert("FOO".into(), MacroDef::object("BAR"));
        let result = preprocess("C     FOO is a comment\n      x = FOO\n", &config).unwrap();
        // C-line should not have FOO expanded.
        assert!(
            result.text.contains("C     FOO is a comment"),
            "got: {:?}",
            result.text
        );
        // Continuation line should expand FOO.
        assert!(result.text.contains("x = BAR"), "got: {:?}", result.text);
    }

    #[test]
    fn fixed_form_star_comment() {
        let mut config = PreprocConfig {
            fixed_form: true,
            ..PreprocConfig::default()
        };
        config.defines.insert("FOO".into(), MacroDef::object("BAR"));
        let result = preprocess("*     FOO is a comment\n", &config).unwrap();
        assert!(
            result.text.contains("*     FOO is a comment"),
            "got: {:?}",
            result.text
        );
    }

    // ---- Fortran & continuation ----

    #[test]
    fn fortran_ampersand_continuation() {
        let out = pp_with("x = FOO + &\n    BAR\n", &[("FOO", "1"), ("BAR", "2")]);
        assert!(
            out.contains("x = 1 + 2"),
            "got: {:?}",
            out.lines().collect::<Vec<_>>()
        );
    }

    #[test]
    fn ampersand_in_string_not_continued() {
        // & inside a string literal must NOT trigger continuation.
        let out = pp("x = 'hello &'\ny = 2\n");
        assert!(
            out.contains("'hello &'"),
            "string corrupted, got: {:?}",
            out.lines().collect::<Vec<_>>()
        );
        assert!(
            out.contains("y = 2"),
            "next line missing, got: {:?}",
            out.lines().collect::<Vec<_>>()
        );
    }

    #[test]
    fn ampersand_in_comment_not_continued() {
        // & after ! comment must NOT trigger continuation.
        let out = pp("x = 1 ! comment &\ny = 2\n");
        assert!(
            out.contains("! comment &"),
            "comment corrupted, got: {:?}",
            out.lines().collect::<Vec<_>>()
        );
        assert!(
            out.contains("y = 2"),
            "next line missing, got: {:?}",
            out.lines().collect::<Vec<_>>()
        );
    }

    // ---- __DATE__ and __TIME__ ----

    #[test]
    fn date_macro_not_empty() {
        let out = pp("x = __DATE__\n");
        // Should contain a quoted date string, not empty.
        assert!(out.contains("\""), "got: {:?}", out);
        assert!(!out.contains("__DATE__"), "macro not expanded: {:?}", out);
    }

    #[test]
    fn time_macro_has_colons() {
        let out = pp("x = __TIME__\n");
        assert!(out.contains(":"), "got: {:?}", out);
    }

    // ---- __FILE__ ----

    #[test]
    fn file_macro() {
        let config = PreprocConfig {
            filename: "test.f90".into(),
            ..PreprocConfig::default()
        };
        let result = preprocess("x = __FILE__\n", &config).unwrap();
        assert!(
            result.text.contains("\"test.f90\""),
            "got: {:?}",
            result.text
        );
    }

    // ---- defined without parens ----

    #[test]
    fn defined_without_parens() {
        let out = pp_with("#if defined FEAT\nyes\n#endif\n", &[("FEAT", "1")]);
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn not_defined_without_parens_ignores_trailing_c_comment() {
        let out = pp("#if !defined GUARD  /* include guard */\nyes\n#endif\n");
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn directive_c_comment_is_removed_from_macro_body() {
        let out = pp("#define FEATURE 1  /* enabled */\n#if FEATURE == 1\nyes\n#endif\n");
        assert!(lines(&out).contains(&"yes"));
    }

    #[test]
    fn c_block_comment_header_is_removed_before_directives_and_source() {
        let out = pp("/* header\n\
             * text.h\n\
             * #if 0\n\
             */\n\
             #if !defined GUARD  /* include guard */\n\
             #define GUARD\n\
             ok\n\
             #endif\n");
        assert!(lines(&out).contains(&"ok"));
        assert!(!out.contains("text.h"), "C block comment leaked: {:?}", out);
    }

    #[test]
    fn fortran_bang_comment_does_not_start_c_block_comment() {
        let out = pp("x = 1 ! /*\nx = x + 41\n! */\n");
        assert!(
            out.contains("x = x + 41"),
            "Fortran source after ! /* was stripped: {out:?}"
        );
    }

    #[test]
    fn source_string_preserves_c_block_markers() {
        let out = pp("print *, 'literal /* kept */'\n");
        assert!(
            out.contains("literal /* kept */"),
            "string was stripped: {out:?}"
        );
    }

    #[test]
    fn cpp_compat_strips_source_c_block_comment() {
        let out = pp_cpp("x = 1 /* c comment */ + 2\n");
        assert!(
            out.contains("x = 1   + 2"),
            "C block comment leaked: {out:?}"
        );
    }

    // ---- Multi-elif chain ----

    #[test]
    fn multi_elif_chain() {
        let out = pp_with(
            "#if X == 1\nfirst\n#elif X == 2\nsecond\n#elif X == 3\nthird\n#else\nother\n#endif\n",
            &[("X", "2")],
        );
        assert!(lines(&out).contains(&"second"));
        assert!(!lines(&out).contains(&"first"));
        assert!(!lines(&out).contains(&"third"));
        assert!(!lines(&out).contains(&"other"));
    }

    // ---- #error inside #if 0 should not trigger ----

    #[test]
    fn error_inside_if_zero_does_not_trigger() {
        let out = pp("#if 0\n#error this should not fire\n#endif\nok\n");
        assert!(lines(&out).contains(&"ok"));
    }

    // ---- #if with hex ----

    #[test]
    fn if_hex_comparison() {
        let out = pp("#if 0xFF > 200\nyes\n#endif\n");
        assert!(lines(&out).contains(&"yes"));
    }

    // ---- Object macro with space before paren is not function-like ----

    #[test]
    fn define_with_space_before_paren_is_object() {
        // #define FOO (x) should be object-like with body "(x)", not function-like.
        let out = pp("#define FOO (x)\ny = FOO\n");
        assert!(out.contains("y = (x)"), "got: {:?}", out);
    }

    // ---- #line directive ----

    #[test]
    fn line_directive_updates_source_map() {
        let config = PreprocConfig::default();
        let result = preprocess("a\n#line 100 \"other.f90\"\nb\nc\n", &config).unwrap();
        // Line 3 (b) should have source map entry pointing to other.f90:100.
        assert_eq!(result.source_map[2].line, 100);
        assert_eq!(result.source_map[2].filename, "other.f90");
        assert_eq!(result.source_map[3].line, 101);
    }

    #[test]
    fn source_map_counts_non_utf8_markers_as_source_bytes() {
        let source = b"x = 'A\xffB' @\n";
        let result = preprocess_bytes(source, &PreprocConfig::default()).unwrap();
        let generated_col = result.text.find('@').unwrap() as u32 + 1;
        let resolved = result.resolve_span(Span {
            file_id: 0,
            start: Position {
                line: 1,
                col: generated_col,
            },
            end: Position {
                line: 1,
                col: generated_col + 1,
            },
        });
        let expected_col = source.iter().position(|byte| *byte == b'@').unwrap() as u32 + 1;
        assert_eq!(resolved.display_span.start.col, expected_col);
        assert_eq!(resolved.display_span.end.col, expected_col + 1);
        assert_eq!(resolved.source, "x = 'A\u{fffd}B' @\n");
        assert_eq!(resolved.source_span.start.col, expected_col);
    }

    #[test]
    fn preprocessor_error_displays_non_utf8_bytes_without_markers() {
        let error = preprocess_bytes(b"#error A\xffB\n", &PreprocConfig::default()).unwrap_err();
        assert_eq!(error.msg, "#error A\u{fffd}B");
    }

    #[test]
    fn line_directive_without_filename() {
        let config = PreprocConfig::default();
        let result = preprocess("a\n#line 50\nb\n", &config).unwrap();
        assert_eq!(result.source_map[2].line, 50);
    }

    #[test]
    fn line_directive_updates_location_builtins() {
        let result = preprocess(
            "#line 100 \"virtual.f90\"\na = __LINE__\nb = __LINE__\nf = __FILE__\n",
            &PreprocConfig::default(),
        )
        .unwrap();

        assert!(result.text.contains("a = 100"), "got: {:?}", result.text);
        assert!(result.text.contains("b = 101"), "got: {:?}", result.text);
        assert!(
            result.text.contains("f = \"virtual.f90\""),
            "got: {:?}",
            result.text
        );
    }

    #[test]
    fn location_builtins_use_their_physical_continuation_lines() {
        let result = preprocess(
            "#line 10 \"virtual.f90\"\na = &\n  __LINE__\nb = \\\n__LINE__\n",
            &PreprocConfig::default(),
        )
        .unwrap();

        assert!(result.text.contains("a = 11"), "got: {:?}", result.text);
        assert!(result.text.contains("b = 13"), "got: {:?}", result.text);
    }

    #[test]
    fn condition_location_builtin_uses_its_continuation_line() {
        let result = preprocess(
            "#line 10 \"virtual.f90\"\n#if \\\n__LINE__ == 11\ndirect\n#endif\n",
            &PreprocConfig::default(),
        )
        .unwrap();

        assert!(lines(&result.text).contains(&"direct"));
    }

    #[test]
    fn condition_macro_body_uses_its_invocation_line() {
        let result = preprocess(
            "#define HERE __LINE__\n#line 20 \"virtual.f90\"\n#if \\\nHERE == 21\nbody\n#endif\n",
            &PreprocConfig::default(),
        )
        .unwrap();

        assert!(lines(&result.text).contains(&"body"));
    }

    #[test]
    fn condition_macro_argument_keeps_its_source_line() {
        let result = preprocess(
            "#define ID(x) x\n#line 30 \"virtual.f90\"\n#if \\\nID(__LINE__) == 31\nargument\n#endif\n",
            &PreprocConfig::default(),
        )
        .unwrap();

        assert!(lines(&result.text).contains(&"argument"));
    }

    #[test]
    fn line_directive_without_filename_preserves_the_reported_file() {
        let result = preprocess(
            "#line 100 \"virtual.f90\"\n#line 200\nx = __LINE__\nf = __FILE__\n",
            &PreprocConfig::default(),
        )
        .unwrap();

        assert_eq!(result.source_map[2].filename, "virtual.f90");
        assert_eq!(result.source_map[2].line, 200);
        assert!(result.text.contains("x = 200"), "got: {:?}", result.text);
        assert!(
            result.text.contains("f = \"virtual.f90\""),
            "got: {:?}",
            result.text
        );
    }

    #[test]
    fn line_directive_applies_to_preprocessor_errors() {
        let error = preprocess(
            "#line 9 \"virtual.f90\"\n#error nope\n",
            &PreprocConfig::default(),
        )
        .unwrap_err();

        assert_eq!(error.filename, "virtual.f90");
        assert_eq!(error.line, 9);
    }

    #[test]
    fn malformed_line_directive_advances_the_reported_line() {
        let result = preprocess(
            "#line 100 \"virtual.f90\"\n#line invalid\nx = __LINE__\n",
            &PreprocConfig::default(),
        )
        .unwrap();

        assert_eq!(result.source_map[2].filename, "virtual.f90");
        assert_eq!(result.source_map[2].line, 101);
        assert!(result.text.contains("x = 101"), "got: {:?}", result.text);
    }

    #[test]
    fn unterminated_conditional_uses_the_last_consumed_location() {
        let error = preprocess(
            "#if 1\n#line 100 \"virtual.f90\"\n",
            &PreprocConfig::default(),
        )
        .unwrap_err();

        assert_eq!(error.filename, "<input>");
        assert_eq!(error.line, 2);
    }

    #[test]
    fn include_locations_are_scoped_and_parent_locations_resume() {
        use std::io::Write;

        let dir = std::env::temp_dir();
        let include_name = format!("afs-line-scope-{}.inc", std::process::id());
        let include_path = dir.join(&include_name);
        let mut include = std::fs::File::create(&include_path).unwrap();
        writeln!(include, "inside_line = __LINE__").unwrap();
        writeln!(include, "inside_file = __FILE__").unwrap();
        writeln!(include, "#line 70 \"include.virtual\"").unwrap();
        writeln!(include, "inside_after = __LINE__").unwrap();
        writeln!(include, "inside_after_file = __FILE__").unwrap();
        drop(include);

        let config = PreprocConfig {
            include_paths: vec![dir],
            ..PreprocConfig::default()
        };
        let source = format!(
            "#line 40 \"parent.virtual\"\nparent_before = __LINE__\n#include \"{}\"\nparent_after = __LINE__\nparent_file = __FILE__\n",
            include_name
        );
        let result = preprocess(&source, &config).unwrap();

        assert!(result.text.contains("parent_before = 40"));
        assert!(result.text.contains("inside_line = 1"));
        assert!(result
            .text
            .contains(&format!("inside_file = \"{}\"", include_path.display())));
        assert!(result.text.contains("inside_after = 70"));
        assert!(result
            .text
            .contains("inside_after_file = \"include.virtual\""));
        assert!(result.text.contains("parent_after = 42"));
        assert!(result.text.contains("parent_file = \"parent.virtual\""));

        let _ = std::fs::remove_file(include_path);
    }

    // ---- Stringify with space ----

    #[test]
    fn stringify_with_space() {
        // # x (with space) should still stringify.
        let out = pp("#define STR(x) # x\ny = STR(hello)\n");
        assert!(out.contains("y = \"hello\""), "got: {:?}", out);
    }

    // ---- Variadic edge cases ----

    #[test]
    fn variadic_zero_args() {
        let out = pp("#define M(...) [__VA_ARGS__]\ny = M()\n");
        assert!(out.contains("y = []"), "got: {:?}", out);
    }

    // ---- #warning doesn't error ----

    #[test]
    fn warning_continues_processing() {
        let out = pp("#warning test warning\nok\n");
        assert!(lines(&out).contains(&"ok"));
    }

    // ---- Include recursion guard ----

    #[test]
    fn include_recursion_guard() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let path = dir.join("test_pp_recurse.inc");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#include \"test_pp_recurse.inc\"").unwrap();
        drop(f);

        let mut config = PreprocConfig::default();
        config.include_paths.push(dir);
        let result = preprocess("#include \"test_pp_recurse.inc\"\n", &config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.msg.contains("depth") || err.msg.contains("recursion"),
            "got: {}",
            err.msg
        );
    }

    /// Golden per-target define sets.
    #[test]
    fn target_define_sets_are_golden() {
        use crate::target::TargetSpec;

        let set = |triple: &str| -> Vec<String> {
            let target = TargetSpec::parse(triple).unwrap();
            let mut names: Vec<String> = PreprocConfig::for_target(&target)
                .defines
                .keys()
                .cloned()
                .collect();
            names.sort();
            names
        };
        let base = [
            "__ARMFORTAS_MAJOR__",
            "__ARMFORTAS_MINOR__",
            "__ARMFORTAS__",
            "__GNUC_MINOR__",
            "__GNUC_PATCHLEVEL__",
            "__GNUC__",
        ];

        let golden = |extra: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = base.iter().chain(extra).map(|s| s.to_string()).collect();
            v.sort();
            v
        };

        assert_eq!(
            set("arm64-macos"),
            golden(&["__APPLE__", "__aarch64__", "__arm64__"])
        );
        assert_eq!(
            set("x86_64-freebsd"),
            golden(&["__FreeBSD__", "__amd64__", "__x86_64__"])
        );
        assert_eq!(
            set("x86_64-linux-gnu"),
            golden(&["__amd64__", "__linux__", "__x86_64__"])
        );
        // libc flavor is deliberately not a predefine: __GLIBC__ etc. come
        // from libc headers, not the compiler.
        assert_eq!(set("x86_64-linux-musl"), set("x86_64-linux-gnu"));
    }

    #[test]
    fn target_defines_select_ifdef_branches() {
        use crate::target::TargetSpec;

        let src =
            "#ifdef __FreeBSD__\nx = 1\n#elif defined(__APPLE__)\nx = 2\n#else\nx = 3\n#endif\n";
        let run = |triple: &str| -> String {
            let target = TargetSpec::parse(triple).unwrap();
            let config = PreprocConfig::for_target(&target);
            preprocess(src, &config).unwrap().text
        };
        assert!(run("x86_64-freebsd").contains("x = 1"));
        assert!(run("arm64-macos").contains("x = 2"));
        assert!(run("x86_64-linux-gnu").contains("x = 3"));
    }

    #[test]
    fn gnu_compat_defines_select_cmake_compiler_id_branch() {
        let target = crate::target::TargetSpec::parse("arm64-macos").unwrap();
        let config = PreprocConfig::for_target(&target);
        let out = preprocess(
            "#if defined(__GNUC__)\nprint *, 'INFO:compiler[GNU]'\n#else\nprint *, 'INFO:compiler[]'\n#endif\n",
            &config,
        )
        .unwrap()
        .text;
        assert!(
            out.contains("INFO:compiler[GNU]"),
            "CMake compiler-id macro branch should be reachable: {out:?}"
        );
    }

    #[test]
    fn gnu_compat_defines_advertise_modern_cmake_version() {
        let target = crate::target::TargetSpec::parse("arm64-macos").unwrap();
        let config = PreprocConfig::for_target(&target);
        let out = preprocess(
            "major = __GNUC__\nminor = __GNUC_MINOR__\npatch = __GNUC_PATCHLEVEL__\n",
            &config,
        )
        .unwrap()
        .text;
        assert!(out.contains("major = 10"), "{out:?}");
        assert!(out.contains("minor = 0"), "{out:?}");
        assert!(out.contains("patch = 0"), "{out:?}");
    }
}
