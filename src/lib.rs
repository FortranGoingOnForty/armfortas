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

/// CLI entry point shared by both the `armfortas` and `afs` binaries.
/// Both binaries are built from the same source path; this function
/// holds the actual logic so the bin files are one-liners and Cargo
/// stops warning about a duplicated build target.
///
/// Exit codes (sprint 32):
///   0 success, 1 compile error, 2 link error, 3 I/O error, 4 ICE.
pub fn cli_entry() -> ! {
    use std::env;
    use std::process;
    const EXIT_COMPILE: i32 = 1;
    const EXIT_IO: i32 = 3;

    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{}", driver::HELP_TEXT);
        process::exit(EXIT_COMPILE);
    }

    let parsed = match driver::parse_cli(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("armfortas: {}", e);
            process::exit(EXIT_COMPILE);
        }
    };

    match parsed {
        driver::ParsedCli::Info(driver::InfoAction::Help) => {
            print!("{}", driver::HELP_TEXT);
            process::exit(0);
        }
        driver::ParsedCli::Info(driver::InfoAction::Version) => {
            println!("{}", driver::version_string());
            process::exit(0);
        }
        driver::ParsedCli::Info(driver::InfoAction::DumpVersion) => {
            println!("{}", driver::dump_version_string());
            process::exit(0);
        }
        driver::ParsedCli::Compile(opts) => {
            let result = if opts.extra_inputs.is_empty() {
                driver::compile(&opts)
            } else {
                driver::compile_multi(&opts)
            };
            if let Err(e) = result {
                eprintln!("armfortas: {}", e);
                // Heuristic categorisation; sprint 32 #507 tracks
                // the proper structured error type.
                let exit_code = if e.contains("cannot read")
                    || e.contains("cannot write")
                    || e.contains("No such file")
                {
                    EXIT_IO
                } else {
                    EXIT_COMPILE
                };
                process::exit(exit_code);
            }
            process::exit(0);
        }
    }
}
