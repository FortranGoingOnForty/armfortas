use std::env;
use std::process;

use armfortas::driver;

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
        eprintln!("  -O0..-O3     optimization level");
        eprintln!("  -Os          optimize for code size");
        eprintln!("  -Ofast       maximum optimization");
        process::exit(1);
    }

    let opts = match driver::Options::from_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("armfortas: {}", e);
            process::exit(1);
        }
    };

    let result = if opts.extra_inputs.is_empty() {
        driver::compile(&opts)
    } else {
        driver::compile_multi(&opts)
    };
    if let Err(e) = result {
        eprintln!("armfortas: {}", e);
        process::exit(1);
    }
}
