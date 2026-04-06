// Library surface for ARMFORTAS.
//
// The binary remains the user-facing compiler entrypoint, while the library
// exposes internal pipeline stages for the bench harness and other tooling.
#![allow(dead_code)]

pub mod ast;
pub mod codegen;
pub mod driver;
pub mod ir;
pub mod lexer;
pub mod opt;
pub mod parser;
pub mod preprocess;
pub mod runtime;
pub mod sema;
pub mod testing;
