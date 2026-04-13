#![no_main]

use libfuzzer_sys::fuzz_target;
use armfortas::lexer::{self, SourceForm};

fuzz_target!(|data: &[u8]| {
    // Feed arbitrary bytes to the lexer. It must never panic —
    // errors are fine, panics are bugs.
    if let Ok(src) = std::str::from_utf8(data) {
        let _ = lexer::tokenize(src, 0, SourceForm::FreeForm);
    }
});
