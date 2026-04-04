//! Fortran-aware C-style preprocessor.
//!
//! Text-to-text transformation that runs before lexing. Handles #define,
//! #ifdef/#ifndef/#if/#elif/#else/#endif, #include, #undef, #error, #warning.
//! Aware of Fortran string literals and comments — won't expand macros inside them.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

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
}

impl Default for PreprocConfig {
    fn default() -> Self {
        let mut defines = HashMap::new();
        // Built-in predefined macros.
        defines.insert("__ARMFORTAS__".into(), MacroDef::object("1"));
        defines.insert("__ARMFORTAS_MAJOR__".into(), MacroDef::object("0"));
        defines.insert("__ARMFORTAS_MINOR__".into(), MacroDef::object("1"));
        defines.insert("__aarch64__".into(), MacroDef::object("1"));
        defines.insert("__arm64__".into(), MacroDef::object("1"));
        #[cfg(target_os = "macos")]
        defines.insert("__APPLE__".into(), MacroDef::object("1"));
        #[cfg(target_os = "linux")]
        defines.insert("__linux__".into(), MacroDef::object("1"));

        Self {
            defines,
            include_paths: Vec::new(),
            filename: "<input>".into(),
            fixed_form: false,
        }
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
        Self { body: body.into(), params: Vec::new(), is_function: false, is_variadic: false }
    }

    pub fn function(params: Vec<String>, body: &str) -> Self {
        Self { body: body.into(), params, is_function: true, is_variadic: false }
    }
}

/// Preprocessor output.
#[derive(Debug, Clone)]
pub struct PreprocOutput {
    /// The preprocessed text.
    pub text: String,
    /// Maps output line numbers (1-based) to original (filename, line) pairs.
    pub source_map: Vec<SourceLoc>,
}

/// A source location before preprocessing.
#[derive(Debug, Clone)]
pub struct SourceLoc {
    pub filename: String,
    pub line: u32,
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

/// Preprocess Fortran source text.
pub fn preprocess(source: &str, config: &PreprocConfig) -> Result<PreprocOutput, PreprocError> {
    let mut pp = Preprocessor::new(config);
    pp.process(source, &config.filename)
}

/// Condition stack state for nested #if/#ifdef blocks.
#[derive(Debug, Clone, Copy)]
enum CondState {
    /// Currently in a true branch, emitting output.
    Active,
    /// In a false branch, skipping output. Saw the directive at this level.
    Skipping,
    /// Already found a true branch at this level, skip rest (including #else).
    Done,
    /// Parent was skipping, so everything at this level is skipped regardless.
    ParentSkipping,
}

struct Preprocessor {
    defines: HashMap<String, MacroDef>,
    include_paths: Vec<PathBuf>,
    cond_stack: Vec<CondState>,
    /// Include depth for recursion guard.
    include_depth: u32,
    /// Fixed-form source mode.
    fixed_form: bool,
}

impl Preprocessor {
    fn new(config: &PreprocConfig) -> Self {
        Self {
            defines: config.defines.clone(),
            include_paths: config.include_paths.clone(),
            fixed_form: config.fixed_form,
            cond_stack: Vec::new(),
            include_depth: 0,
        }
    }

    fn is_emitting(&self) -> bool {
        self.cond_stack.iter().all(|s| matches!(s, CondState::Active))
    }

    fn process(&mut self, source: &str, filename: &str) -> Result<PreprocOutput, PreprocError> {
        let mut output = String::new();
        let mut source_map = Vec::new();
        self.process_into(source, filename, &mut output, &mut source_map)?;

        // Check for unterminated conditionals (only at top level).
        if self.include_depth == 0 && !self.cond_stack.is_empty() {
            return Err(PreprocError {
                filename: filename.into(),
                line: source.lines().count() as u32,
                msg: format!("unterminated #if/#ifdef ({} level(s) still open)", self.cond_stack.len()),
            });
        }

        Ok(PreprocOutput { text: output, source_map })
    }

    fn process_into(
        &mut self,
        source: &str,
        filename: &str,
        output: &mut String,
        source_map: &mut Vec<SourceLoc>,
    ) -> Result<(), PreprocError> {
        // Set dynamic macros.
        self.defines.insert("__FILE__".into(), MacroDef::object(&format!("\"{}\"", filename)));

        let now = current_datetime();
        self.defines.insert("__DATE__".into(), MacroDef::object(&format!("\"{}\"", now.0)));
        self.defines.insert("__TIME__".into(), MacroDef::object(&format!("\"{}\"", now.1)));

        // Process lines with inline backslash continuation joining,
        // tracking original line numbers so __LINE__ and source_map are correct.
        let raw_lines: Vec<&str> = source.lines().collect();
        let mut i = 0;
        while i < raw_lines.len() {
            let orig_line_num = (i + 1) as u32; // 1-based, tracks original source line
            let mut logical_line = String::new();

            // Join backslash-continued lines (C-style).
            while i < raw_lines.len() && raw_lines[i].ends_with('\\') {
                logical_line.push_str(&raw_lines[i][..raw_lines[i].len() - 1]);
                i += 1;
            }
            if i < raw_lines.len() {
                logical_line.push_str(raw_lines[i]);
                i += 1;
            }

            // Also join Fortran &-continued lines (free-form).
            // A line ending with & (before any comment) continues on the next line.
            if !self.fixed_form {
                while logical_line.trim_end().ends_with('&') {
                    // Remove the trailing &.
                    let trimmed_len = logical_line.trim_end().len();
                    logical_line.truncate(trimmed_len - 1);
                    // Consume the next line, skipping leading & if present.
                    if i < raw_lines.len() {
                        let next = raw_lines[i].trim_start();
                        let next = next.strip_prefix('&').unwrap_or(next);
                        logical_line.push_str(next);
                        i += 1;
                    } else {
                        break;
                    }
                }
            }

            // Update __LINE__ to the original starting line of this logical line.
            self.defines.insert("__LINE__".into(), MacroDef::object(&orig_line_num.to_string()));

            // Fixed-form: C, c, or * in column 1 is a comment line.
            if self.fixed_form {
                let first = logical_line.as_bytes().first().copied().unwrap_or(0);
                if first == b'C' || first == b'c' || first == b'*' {
                    // Comment line — emit as-is without expansion.
                    if self.is_emitting() {
                        output.push_str(&logical_line);
                    }
                    output.push('\n');
                    source_map.push(SourceLoc { filename: filename.into(), line: orig_line_num });
                    continue;
                }
            }

            let trimmed = logical_line.trim_start();

            // Preprocessor directives: # in column 1 (or after whitespace in free-form).
            if trimmed.starts_with('#') {
                self.process_directive(trimmed, filename, orig_line_num, output, source_map)?;
                output.push('\n');
                source_map.push(SourceLoc { filename: filename.into(), line: orig_line_num });
                continue;
            }

            if self.is_emitting() {
                let expanded = self.expand_macros(&logical_line, filename, orig_line_num);
                output.push_str(&expanded);
            }
            output.push('\n');
            source_map.push(SourceLoc { filename: filename.into(), line: orig_line_num });
        }

        Ok(())
    }

    fn process_directive(
        &mut self,
        line: &str,
        filename: &str,
        line_num: u32,
        output: &mut String,
        source_map: &mut Vec<SourceLoc>,
    ) -> Result<(), PreprocError> {
        let rest = line[1..].trim_start(); // skip '#' and whitespace
        let (directive, args) = split_first_word(rest);

        // Conditionals must be processed even when skipping.
        match directive {
            "ifdef" => return self.do_ifdef(args, false),
            "ifndef" => return self.do_ifdef(args, true),
            "if" => return self.do_if(args, filename, line_num),
            "elif" => return self.do_elif(args, filename, line_num),
            "else" => return self.do_else(filename, line_num),
            "endif" => return self.do_endif(filename, line_num),
            _ => {}
        }

        // All other directives are only processed when emitting.
        if !self.is_emitting() {
            return Ok(());
        }

        match directive {
            "define" => self.do_define(args),
            "undef" => self.do_undef(args),
            "include" => self.do_include(args, filename, line_num, output, source_map),
            "error" => Err(PreprocError {
                filename: filename.into(), line: line_num,
                msg: format!("#error {}", args),
            }),
            "warning" => {
                eprintln!("{}:{}: warning: #warning {}", filename, line_num, args);
                Ok(())
            }
            "line" => Ok(()), // TODO: update source location tracking
            "" => Ok(()), // bare # is allowed (null directive)
            _ => Ok(()), // unknown directives are ignored (like #pragma)
        }
    }

    // ---- Conditional directives ----

    fn do_ifdef(&mut self, args: &str, negate: bool) -> Result<(), PreprocError> {
        let name = args.split_whitespace().next().unwrap_or("");
        if !self.is_emitting() {
            self.cond_stack.push(CondState::ParentSkipping);
            return Ok(());
        }
        let defined = self.defines.contains_key(name);
        let condition = if negate { !defined } else { defined };
        self.cond_stack.push(if condition { CondState::Active } else { CondState::Skipping });
        Ok(())
    }

    fn do_if(&mut self, args: &str, filename: &str, line_num: u32) -> Result<(), PreprocError> {
        if !self.is_emitting() {
            self.cond_stack.push(CondState::ParentSkipping);
            return Ok(());
        }
        let val = self.eval_condition(args, filename, line_num)?;
        self.cond_stack.push(if val { CondState::Active } else { CondState::Skipping });
        Ok(())
    }

    fn do_elif(&mut self, args: &str, filename: &str, line_num: u32) -> Result<(), PreprocError> {
        match self.cond_stack.last().copied() {
            None => Err(PreprocError {
                filename: filename.into(), line: line_num,
                msg: "#elif without matching #if".into(),
            }),
            Some(CondState::ParentSkipping) => Ok(()), // stay as ParentSkipping
            Some(CondState::Active) => {
                // Was true, now done — skip rest.
                *self.cond_stack.last_mut().unwrap() = CondState::Done;
                Ok(())
            }
            Some(CondState::Done) => Ok(()), // already found true branch
            Some(CondState::Skipping) => {
                // Previous branch was false, evaluate this one.
                let val = self.eval_condition(args, filename, line_num)?;
                *self.cond_stack.last_mut().unwrap() = if val { CondState::Active } else { CondState::Skipping };
                Ok(())
            }
        }
    }

    fn do_else(&mut self, filename: &str, line_num: u32) -> Result<(), PreprocError> {
        match self.cond_stack.last().copied() {
            None => Err(PreprocError {
                filename: filename.into(), line: line_num,
                msg: "#else without matching #if".into(),
            }),
            Some(CondState::ParentSkipping) => Ok(()),
            Some(CondState::Active) => {
                *self.cond_stack.last_mut().unwrap() = CondState::Done;
                Ok(())
            }
            Some(CondState::Done) => Ok(()),
            Some(CondState::Skipping) => {
                *self.cond_stack.last_mut().unwrap() = CondState::Active;
                Ok(())
            }
        }
    }

    fn do_endif(&mut self, filename: &str, line_num: u32) -> Result<(), PreprocError> {
        if self.cond_stack.pop().is_none() {
            return Err(PreprocError {
                filename: filename.into(), line: line_num,
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
                    let mut params: Vec<String> = params_str.split(',')
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

    // ---- #include ----

    fn do_include(
        &mut self,
        args: &str,
        filename: &str,
        line_num: u32,
        output: &mut String,
        source_map: &mut Vec<SourceLoc>,
    ) -> Result<(), PreprocError> {
        let args = args.trim();
        let (path_str, search_system) = if let Some(rest) = args.strip_prefix('"') {
            let end = rest.find('"').ok_or_else(|| PreprocError {
                filename: filename.into(), line: line_num,
                msg: "unterminated #include string".into(),
            })?;
            (&rest[..end], false)
        } else if let Some(rest) = args.strip_prefix('<') {
            let end = rest.find('>').ok_or_else(|| PreprocError {
                filename: filename.into(), line: line_num,
                msg: "unterminated #include <path>".into(),
            })?;
            (&rest[..end], true)
        } else {
            return Err(PreprocError {
                filename: filename.into(), line: line_num,
                msg: format!("expected \"file\" or <file> after #include, got: {}", args),
            });
        };

        if self.include_depth >= 64 {
            return Err(PreprocError {
                filename: filename.into(), line: line_num,
                msg: "include depth limit exceeded (possible recursion)".into(),
            });
        }

        // Search for the file.
        let resolved = self.resolve_include(path_str, filename, search_system)
            .ok_or_else(|| PreprocError {
                filename: filename.into(), line: line_num,
                msg: format!("cannot find include file: {}", path_str),
            })?;

        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| PreprocError {
                filename: filename.into(), line: line_num,
                msg: format!("reading {}: {}", resolved.display(), e),
            })?;

        // Save __FILE__ so it's restored after the include returns.
        let saved_file = self.defines.get("__FILE__").cloned();

        self.include_depth += 1;
        let inc_filename = resolved.to_string_lossy().into_owned();
        self.process_into(&content, &inc_filename, output, source_map)?;
        self.include_depth -= 1;

        // Restore __FILE__ to the parent's filename.
        if let Some(saved) = saved_file {
            self.defines.insert("__FILE__".into(), saved);
        }
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

    fn eval_condition(&self, expr: &str, filename: &str, line_num: u32) -> Result<bool, PreprocError> {
        // Expand macros in the expression first.
        let expanded = self.expand_condition_macros(expr);
        // Parse and evaluate the expression.
        eval_expr(&expanded).map_err(|msg| PreprocError {
            filename: filename.into(), line: line_num,
            msg: format!("in #if expression: {}", msg),
        })
    }

    /// Expand macros and `defined(NAME)` / `defined NAME` in a condition expression.
    ///
    /// Three passes:
    /// 1. Replace `defined(X)` / `defined X` with "1" or "0"
    /// 2. Recursively expand macros (reusing expand_macros_inner)
    /// 3. Replace remaining undefined identifiers with "0" (per cpp standard)
    fn expand_condition_macros(&self, expr: &str) -> String {
        // Pass 1: resolve `defined(...)` and `defined ...` operators.
        let after_defined = self.resolve_defined_ops(expr);

        // Pass 2: recursively expand macros (same engine as source lines).
        let expanding = std::collections::HashSet::new();
        let after_macros = self.expand_macros_inner(&after_defined, &expanding);

        // Pass 3: replace remaining identifiers with 0 (undefined = 0 in #if).
        replace_undefined_idents(&after_macros)
    }

    /// Replace `defined(NAME)` and `defined NAME` with "1" or "0".
    fn resolve_defined_ops(&self, expr: &str) -> String {
        let mut result = String::new();
        let mut chars = expr.chars().peekable();

        while let Some(&ch) = chars.peek() {
            if ch.is_alphabetic() || ch == '_' {
                let mut ident = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' {
                        ident.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }

                if ident == "defined" {
                    while chars.peek() == Some(&' ') { chars.next(); }
                    let has_paren = chars.peek() == Some(&'(');
                    if has_paren { chars.next(); }
                    let mut name = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_alphanumeric() || c == '_' {
                            name.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if has_paren {
                        while chars.peek() == Some(&' ') { chars.next(); }
                        if chars.peek() == Some(&')') { chars.next(); }
                    }
                    result.push_str(if self.defines.contains_key(&name) { "1" } else { "0" });
                } else {
                    result.push_str(&ident);
                }
            } else {
                result.push(ch);
                chars.next();
            }
        }

        result
    }

    // ---- Macro expansion in source lines ----

    fn expand_macros(&self, line: &str, _filename: &str, _line_num: u32) -> String {
        let expanding = std::collections::HashSet::new();
        self.expand_macros_inner(line, &expanding)
    }

    fn expand_macros_inner(&self, line: &str, expanding: &std::collections::HashSet<String>) -> String {
        if self.defines.is_empty() {
            return line.to_string();
        }

        let mut result = String::with_capacity(line.len());
        let bytes = line.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            // Skip Fortran comment (! to end of line).
            if bytes[i] == b'!' {
                result.push_str(&line[i..]);
                break;
            }

            // Skip string literals.
            if bytes[i] == b'\'' || bytes[i] == b'"' {
                let quote = bytes[i];
                result.push(quote as char);
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            result.push(quote as char);
                            result.push(quote as char);
                            i += 2;
                        } else {
                            break;
                        }
                    } else {
                        result.push(bytes[i] as char);
                        i += 1;
                    }
                }
                if i < bytes.len() {
                    result.push(quote as char);
                    i += 1;
                }
                continue;
            }

            // Try to match an identifier.
            if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let ident = &line[start..i];

                // Skip if this macro is currently being expanded (blue paint).
                if expanding.contains(ident) {
                    result.push_str(ident);
                    continue;
                }

                if let Some(def) = self.defines.get(ident) {
                    if def.is_function {
                        if i < bytes.len() && bytes[i] == b'(' {
                            if let Some((expanded, new_i)) = self.expand_function_macro(def, line, i) {
                                // Re-expand the result with this macro marked as expanding.
                                let mut next_expanding = expanding.clone();
                                next_expanding.insert(ident.to_string());
                                result.push_str(&self.expand_macros_inner(&expanded, &next_expanding));
                                i = new_i;
                                continue;
                            }
                        }
                        result.push_str(ident);
                    } else {
                        // Re-expand object macro body with this macro marked as expanding.
                        let mut next_expanding = expanding.clone();
                        next_expanding.insert(ident.to_string());
                        result.push_str(&self.expand_macros_inner(&def.body, &next_expanding));
                    }
                } else {
                    result.push_str(ident);
                }
                continue;
            }

            result.push(bytes[i] as char);
            i += 1;
        }

        result
    }

    fn expand_function_macro(&self, def: &MacroDef, line: &str, paren_start: usize) -> Option<(String, usize)> {
        let bytes = line.as_bytes();
        let mut i = paren_start + 1; // skip '('
        let mut args: Vec<String> = Vec::new();
        let mut current_arg = String::new();
        let mut depth = 1;

        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'(' => { depth += 1; current_arg.push('('); }
                b')' => {
                    depth -= 1;
                    if depth > 0 { current_arg.push(')'); }
                }
                b',' if depth == 1 => {
                    args.push(current_arg.trim().to_string());
                    current_arg = String::new();
                }
                _ => current_arg.push(bytes[i] as char),
            }
            i += 1;
        }

        if depth != 0 { return None; }
        args.push(current_arg.trim().to_string());

        // Build parameter lookup table.
        let mut param_map: HashMap<&str, usize> = HashMap::new();
        for (pi, param) in def.params.iter().enumerate() {
            param_map.insert(param.as_str(), pi);
        }

        let va_args_str = if def.is_variadic {
            let va_start = def.params.len();
            args[va_start..].join(", ")
        } else {
            String::new()
        };

        // Single-pass word-boundary-aware substitution over the body.
        let body_bytes = def.body.as_bytes();
        let mut body = String::new();
        let mut bi = 0;

        while bi < body_bytes.len() {
            // Stringification: # followed by identifier
            if body_bytes[bi] == b'#' && bi + 1 < body_bytes.len() && body_bytes[bi + 1] != b'#' {
                let id_start = bi + 1;
                let mut id_end = id_start;
                while id_end < body_bytes.len() && (body_bytes[id_end].is_ascii_alphanumeric() || body_bytes[id_end] == b'_') {
                    id_end += 1;
                }
                if id_end > id_start {
                    let id = std::str::from_utf8(&body_bytes[id_start..id_end]).unwrap_or("");
                    if let Some(&pi) = param_map.get(id) {
                        body.push_str(&format!("\"{}\"", args.get(pi).map(|s| s.as_str()).unwrap_or("")));
                        bi = id_end;
                        continue;
                    }
                }
            }

            // Token pasting: ##
            if bi + 1 < body_bytes.len() && body_bytes[bi] == b'#' && body_bytes[bi + 1] == b'#' {
                // Trim trailing whitespace from what we've built, skip ## and leading whitespace.
                let trimmed = body.trim_end().to_string();
                body = trimmed;
                bi += 2;
                while bi < body_bytes.len() && body_bytes[bi] == b' ' {
                    bi += 1;
                }
                continue;
            }

            // Identifier: check if it's a parameter name.
            if body_bytes[bi].is_ascii_alphabetic() || body_bytes[bi] == b'_' {
                let id_start = bi;
                while bi < body_bytes.len() && (body_bytes[bi].is_ascii_alphanumeric() || body_bytes[bi] == b'_') {
                    bi += 1;
                }
                let id = std::str::from_utf8(&body_bytes[id_start..bi]).unwrap_or("");

                if id == "__VA_ARGS__" && def.is_variadic {
                    body.push_str(&va_args_str);
                } else if let Some(&pi) = param_map.get(id) {
                    body.push_str(args.get(pi).map(|s| s.as_str()).unwrap_or(""));
                } else {
                    body.push_str(id);
                }
                continue;
            }

            body.push(body_bytes[bi] as char);
            bi += 1;
        }

        Some((body, i))
    }
}

// ---- Expression evaluator for #if ----

fn eval_expr(expr: &str) -> Result<bool, String> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Err("empty expression".into());
    }
    let val = eval_or(trimmed)?;
    Ok(val != 0)
}

fn eval_or(expr: &str) -> Result<i64, String> {
    // Split on || (lowest precedence).
    if let Some(pos) = find_op(expr, "||") {
        let left = eval_or(&expr[..pos])?;
        let right = eval_or(&expr[pos + 2..])?;
        return Ok(if left != 0 || right != 0 { 1 } else { 0 });
    }
    eval_and(expr)
}

fn eval_and(expr: &str) -> Result<i64, String> {
    if let Some(pos) = find_op(expr, "&&") {
        let left = eval_and(&expr[..pos])?;
        let right = eval_and(&expr[pos + 2..])?;
        return Ok(if left != 0 && right != 0 { 1 } else { 0 });
    }
    eval_comparison(expr)
}

fn eval_comparison(expr: &str) -> Result<i64, String> {
    // Scan right-to-left for left-associative evaluation.
    for (op, op_len) in [("==", 2), ("!=", 2), (">=", 2), ("<=", 2), (">", 1), ("<", 1)] {
        if let Some(pos) = find_op_right(expr, op) {
            let left = eval_comparison(&expr[..pos])?;
            let right = eval_additive(&expr[pos + op_len..])?;
            let result = match op {
                "==" => left == right,
                "!=" => left != right,
                ">=" => left >= right,
                "<=" => left <= right,
                ">" => left > right,
                "<" => left < right,
                _ => unreachable!(),
            };
            return Ok(if result { 1 } else { 0 });
        }
    }
    eval_additive(expr)
}

fn eval_additive(expr: &str) -> Result<i64, String> {
    // Scan right-to-left for left-associative + and -.
    // But be careful: don't match unary minus (no left operand).
    if let Some(pos) = find_op_right(expr, "+") {
        let left = eval_additive(&expr[..pos])?;
        let right = eval_multiplicative(&expr[pos + 1..])?;
        return Ok(left + right);
    }
    // For minus, only match if there's a non-empty left side (not unary).
    if let Some(pos) = find_op_right(expr, "-") {
        if pos > 0 && !expr[..pos].trim().is_empty() {
            let left = eval_additive(&expr[..pos])?;
            let right = eval_multiplicative(&expr[pos + 1..])?;
            return Ok(left - right);
        }
    }
    eval_multiplicative(expr)
}

fn eval_multiplicative(expr: &str) -> Result<i64, String> {
    if let Some(pos) = find_op_right(expr, "*") {
        let left = eval_multiplicative(&expr[..pos])?;
        let right = eval_unary(&expr[pos + 1..])?;
        return Ok(left * right);
    }
    if let Some(pos) = find_op_right(expr, "/") {
        let left = eval_multiplicative(&expr[..pos])?;
        let right = eval_unary(&expr[pos + 1..])?;
        if right == 0 { return Err("division by zero in #if expression".into()); }
        return Ok(left / right);
    }
    if let Some(pos) = find_op_right(expr, "%") {
        let left = eval_multiplicative(&expr[..pos])?;
        let right = eval_unary(&expr[pos + 1..])?;
        if right == 0 { return Err("modulo by zero in #if expression".into()); }
        return Ok(left % right);
    }
    eval_unary(expr)
}

fn eval_unary(expr: &str) -> Result<i64, String> {
    let trimmed = expr.trim();
    if let Some(rest) = trimmed.strip_prefix('!') {
        let val = eval_unary(rest)?;
        return Ok(if val == 0 { 1 } else { 0 });
    }
    if let Some(rest) = trimmed.strip_prefix('-') {
        let val = eval_unary(rest)?;
        return Ok(-val);
    }
    if let Some(rest) = trimmed.strip_prefix('+') {
        return eval_unary(rest);
    }
    eval_primary(trimmed)
}

fn eval_primary(expr: &str) -> Result<i64, String> {
    let trimmed = expr.trim();

    if trimmed.is_empty() {
        return Err("unexpected end of expression".into());
    }

    // Parenthesized expression.
    if trimmed.starts_with('(') {
        let close = find_matching_paren(trimmed)
            .ok_or("unmatched parenthesis in #if expression")?;
        return eval_or(&trimmed[1..close]);
    }

    // Integer literal.
    if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
        return i64::from_str_radix(&trimmed[2..], 16)
            .map_err(|e| format!("invalid hex in #if: {}", e));
    }
    if let Ok(val) = trimmed.parse::<i64>() {
        return Ok(val);
    }

    // Identifier — should have been expanded already. Treat as 0.
    if trimmed.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Ok(0);
    }

    Err(format!("unexpected token in #if expression: '{}'", trimmed))
}

/// Find an operator at the top level (not inside parentheses).
fn find_op(expr: &str, op: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let op_bytes = op.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i + op_bytes.len() <= bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {
                if depth == 0 && &bytes[i..i + op_bytes.len()] == op_bytes {
                    // Make sure we don't match inside a longer operator.
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Find the rightmost occurrence of an operator at the top level (not inside parentheses).
/// Used for left-associative binary operators.
fn find_op_right(expr: &str, op: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let op_bytes = op.as_bytes();
    let mut depth = 0i32;
    let mut last_match = None;
    let mut i = 0;
    while i + op_bytes.len() <= bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {
                if depth == 0 && &bytes[i..i + op_bytes.len()] == op_bytes {
                    // Don't match multi-char operators as part of longer ones.
                    // e.g., don't match ">" inside ">=" or "!" inside "!=".
                    let after = i + op_bytes.len();
                    let is_part_of_longer = match op {
                        ">" => after < bytes.len() && bytes[after] == b'=',
                        "<" => after < bytes.len() && bytes[after] == b'=',
                        "!" => after < bytes.len() && bytes[after] == b'=',
                        _ => false,
                    };
                    if !is_part_of_longer {
                        last_match = Some(i);
                    }
                }
            }
        }
        i += 1;
    }
    last_match
}

fn find_matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, ch) in s.chars().enumerate() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 { return Some(i); }
            }
            _ => {}
        }
    }
    None
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
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    if let Some(pos) = s.find(|c: char| c.is_whitespace()) {
        (&s[..pos], s[pos..].trim_start())
    } else {
        (s, "")
    }
}

/// Join backslash-continued lines: a line ending with `\` is joined
/// with the following line (with the `\` and newline removed).
fn join_continuations(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.ends_with('\\') && lines.peek().is_some() {
            result.push_str(&line[..line.len() - 1]);
        } else {
            result.push_str(line);
            result.push('\n');
        }
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

    let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun",
                   "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
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
    fn function_macro_no_parens_not_expanded() {
        let out = pp("#define FOO(x) (x+1)\ny = FOO\n");
        // No parens after FOO — should not expand.
        assert!(out.contains("y = FOO"));
    }

    #[test]
    fn function_macro_nested_parens() {
        let out = pp("#define F(x) (x)\ny = F(a(b, c))\n");
        assert!(out.contains("y = (a(b, c))"));
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
        let out = pp_with("#if MAX > 512\nbig\n#else\nsmall\n#endif\n", &[("MAX", "1024")]);
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
        let out = pp("x = __aarch64__\n");
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
        assert!(out.contains("+ 1"), "FOO after string should expand, got: {:?}", out);
    }

    #[test]
    fn no_expansion_in_comment() {
        let out = pp_with("x = 1 ! FOO comment\n", &[("FOO", "BAR")]);
        assert!(out.contains("! FOO comment"));
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
        assert_eq!(eval_expr("3 + 4").unwrap(), true); // 7 != 0
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
    fn if_with_arithmetic() {
        let out = pp_with("#if MAX + 1 > 512\nbig\n#else\nsmall\n#endif\n", &[("MAX", "1024")]);
        assert!(lines(&out).contains(&"big"));
    }

    #[test]
    fn if_chained_macros() {
        // A -> 1, B -> A. #if B should expand B->A->1, evaluating to true.
        let out = pp_with("#if B\nyes\n#endif\n", &[("A", "1"), ("B", "A")]);
        assert!(lines(&out).contains(&"yes"), "chained macro in #if failed, got: {:?}", lines(&out));
    }

    #[test]
    fn if_chained_three_levels() {
        let out = pp_with("#if C > 10\nyes\n#endif\n", &[("A", "42"), ("B", "A"), ("C", "B")]);
        assert!(lines(&out).contains(&"yes"), "3-level chain in #if failed, got: {:?}", lines(&out));
    }

    #[test]
    fn if_defined_and_value() {
        // Common real-world pattern: #if defined(FOO) && FOO > 5
        let out = pp_with("#if defined(FOO) && FOO > 5\nyes\n#endif\n", &[("FOO", "10")]);
        assert!(lines(&out).contains(&"yes"));
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
        assert!(out.contains("((1) +"), "got: {:?}", out.lines().collect::<Vec<_>>());
        assert!(out.contains("(2))"));
    }

    #[test]
    fn line_number_correct_after_continuation() {
        // Lines 1-3 are a continued #define. Line 4 should report __LINE__ = 4.
        let out = pp("#define M \\\n    42\na\nb = __LINE__\n");
        assert!(out.contains("b = 4"), "got: {:?}", out.lines().collect::<Vec<_>>());
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
        assert!(result.text.contains("integer :: included_var"), "got: {:?}", result.text);
        assert!(result.text.contains("real :: x"));
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
        assert!(result.text.contains("before = \"parent.f90\""), "got: {:?}", result.text);
        assert!(result.text.contains("after = \"parent.f90\""), "__FILE__ not restored, got: {:?}", result.text);
    }

    // ---- Fixed-form awareness ----

    #[test]
    fn fixed_form_comment_not_expanded() {
        let mut config = PreprocConfig::default();
        config.fixed_form = true;
        config.defines.insert("FOO".into(), MacroDef::object("BAR"));
        let result = preprocess("C     FOO is a comment\n      x = FOO\n", &config).unwrap();
        // C-line should not have FOO expanded.
        assert!(result.text.contains("C     FOO is a comment"), "got: {:?}", result.text);
        // Continuation line should expand FOO.
        assert!(result.text.contains("x = BAR"), "got: {:?}", result.text);
    }

    #[test]
    fn fixed_form_star_comment() {
        let mut config = PreprocConfig::default();
        config.fixed_form = true;
        config.defines.insert("FOO".into(), MacroDef::object("BAR"));
        let result = preprocess("*     FOO is a comment\n", &config).unwrap();
        assert!(result.text.contains("*     FOO is a comment"), "got: {:?}", result.text);
    }

    // ---- Fortran & continuation ----

    #[test]
    fn fortran_ampersand_continuation() {
        let out = pp_with("x = FOO + &\n    BAR\n", &[("FOO", "1"), ("BAR", "2")]);
        assert!(out.contains("x = 1 + 2"), "got: {:?}", out.lines().collect::<Vec<_>>());
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
        let mut config = PreprocConfig::default();
        config.filename = "test.f90".into();
        let result = preprocess("x = __FILE__\n", &config).unwrap();
        assert!(result.text.contains("\"test.f90\""), "got: {:?}", result.text);
    }

    // ---- defined without parens ----

    #[test]
    fn defined_without_parens() {
        let out = pp_with("#if defined FEAT\nyes\n#endif\n", &[("FEAT", "1")]);
        assert!(lines(&out).contains(&"yes"));
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
}
