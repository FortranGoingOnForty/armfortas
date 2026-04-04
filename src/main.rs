// Modules under active development — suppress dead_code warnings until wired up.
#![allow(dead_code)]

use std::env;
use std::process;

mod preprocess;
mod lexer;
mod parser;
mod ast;
mod sema;
mod ir;
mod opt;
mod codegen;
mod driver;
mod runtime;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: armfortas [options] <file.f90>");
        eprintln!("       afs [options] <file.f90>");
        eprintln!();
        eprintln!("options:");
        eprintln!("  -o <file>    output file");
        eprintln!("  -S           emit assembly");
        eprintln!("  -c           compile to object file");
        eprintln!("  -E           preprocess only");
        eprintln!("  --emit-ir    emit IR");
        process::exit(1);
    }

    let opts = match driver::Options::from_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("armfortas: {}", e);
            process::exit(1);
        }
    };

    if let Err(e) = driver::compile(&opts) {
        eprintln!("armfortas: {}", e);
        process::exit(1);
    }
}
