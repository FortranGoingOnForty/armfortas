//! Smoke-level fuzz tests for the lexer and parser.
//!
//! These don't use libfuzzer — they run deterministic random inputs
//! through the lexer and parser to verify no panics. For real fuzzing,
//! use `cargo fuzz run fuzz_lexer` / `cargo fuzz run fuzz_parser`.

use armfortas::lexer::{self, SourceForm};
use armfortas::parser::Parser;

/// Feed N random-ish strings through the lexer.
fn fuzz_lexer_deterministic(seed: u64, count: usize) {
    let mut state = seed;
    for _ in 0..count {
        // Simple xorshift64 PRNG.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;

        let len = (state % 256) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|i| {
                let mut s = state.wrapping_add(i as u64);
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s & 0x7F) as u8 // ASCII range
            })
            .collect();

        if let Ok(src) = std::str::from_utf8(&bytes) {
            let _ = lexer::tokenize(src, 0, SourceForm::FreeForm);
        }
    }
}

/// Feed N random-ish strings through lexer → parser.
fn fuzz_parser_deterministic(seed: u64, count: usize) {
    let mut state = seed;
    for _ in 0..count {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;

        let len = (state % 512) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|i| {
                let mut s = state.wrapping_add(i as u64);
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s & 0x7F) as u8
            })
            .collect();

        if let Ok(src) = std::str::from_utf8(&bytes) {
            if let Ok(tokens) = lexer::tokenize(src, 0, SourceForm::FreeForm) {
                let mut parser = Parser::new(&tokens);
                let _ = parser.parse_file();
            }
        }
    }
}

/// Feed malformed but plausible Fortran through the parser.
/// Runs without threads — each input goes directly through the parser.
fn fuzz_parser_with_fortran_fragments(count: usize) {
    let fragments = [
        "program p\nend program\n", "module m\nend module\n",
        "integer :: x\n", "real, allocatable :: a(:,:)\n",
        "do i = 1, 10\nend do\n", "if (x > 0) then\nend if\n",
        "select case (x)\ncase (1)\nend select\n",
        "type :: t\n  integer :: f\nend type\n",
        "interface\nend interface\n",
        "goto 100\n", "100 continue\n",
        "use iso_c_binding, only: c_int\n",
        "", "!\n", "! comment\n",
        "end\n",
    ];

    let mut state: u64 = 0xDEADBEEF;
    for _ in 0..count {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;

        // Pick 1-3 random fragments and concatenate.
        let n_frags = 1 + (state % 3) as usize;
        let mut src = String::new();
        for _ in 0..n_frags {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let idx = (state as usize) % fragments.len();
            src.push_str(fragments[idx]);
        }

        if let Ok(tokens) = lexer::tokenize(&src, 0, SourceForm::FreeForm) {
            if tokens.len() > 100 { continue; }
            let mut parser = Parser::new(&tokens);
            let _ = parser.parse_file();
        }
    }
}

#[test]
fn lexer_no_panic_on_random_ascii() {
    fuzz_lexer_deterministic(0x12345678, 10_000);
}

#[test]
fn parser_no_panic_on_random_ascii() {
    fuzz_parser_deterministic(0x87654321, 5_000);
}

#[test]
fn parser_no_panic_on_fortran_fragments() {
    fuzz_parser_with_fortran_fragments(5_000);
}
