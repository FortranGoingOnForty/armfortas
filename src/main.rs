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
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: armfortas [options] <files...>");
        eprintln!("       afs [options] <files...>");
        process::exit(1);
    }
    // TODO: parse args, dispatch to driver
    eprintln!("armfortas: not yet implemented");
    process::exit(1);
}
