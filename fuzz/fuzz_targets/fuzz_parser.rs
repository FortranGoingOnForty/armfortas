#![no_main]

use libfuzzer_sys::fuzz_target;
use armfortas::lexer::{self, SourceForm};
use armfortas::parser::Parser;

fuzz_target!(|data: &[u8]| {
    // Feed arbitrary bytes through lexer → parser. Neither may panic.
    if let Ok(src) = std::str::from_utf8(data) {
        if let Ok(tokens) = lexer::tokenize(src, 0, SourceForm::FreeForm) {
            let mut parser = Parser::new(&tokens);
            let _ = parser.parse_file();
        }
    }
});
