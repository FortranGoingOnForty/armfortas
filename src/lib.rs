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
pub mod target;
pub mod testing;

/// CLI entry point shared by both the `armfortas` and `afs` binaries.
/// Both binaries are built from the same source path; this function
/// holds the actual logic so the bin files are one-liners and Cargo
/// stops warning about a duplicated build target.
///
/// Exit codes (sprint 32):
///   0 success
///   1 compile error (syntax, type, semantic)
///   2 linker error
///   3 I/O error (cannot read input, cannot write output)
///   4 internal compiler error (panic / assertion)
pub fn cli_entry() -> ! {
    use std::env;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::process;
    const EXIT_COMPILE: i32 = 1;
    const EXIT_LINK: i32 = 2;
    const EXIT_IO: i32 = 3;
    const EXIT_ICE: i32 = 4;
    let program_name = driver::program_name();

    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print!("{}", driver::HELP_TEXT);
        process::exit(0);
    }

    let parsed = match driver::parse_cli(&args) {
        Ok(p) => p,
        Err(e) => {
            if e == "no input file" {
                eprintln!("{}: {}", program_name, e);
                print!("{}", driver::HELP_TEXT);
                process::exit(0);
            }
            eprintln!("{}: {}", program_name, e);
            process::exit(classify_compile_error(&e, EXIT_COMPILE, EXIT_LINK, EXIT_IO));
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
            if emit_cli_warnings(&program_name, &opts) {
                process::exit(EXIT_COMPILE);
            }
            // Capture the input path before opts is moved into the
            // compile thunk so we can include it in any ICE report.
            let input_for_ice = opts.input.display().to_string();
            install_ice_hook();
            let result = catch_unwind(AssertUnwindSafe(|| driver::execute(&opts)));
            match result {
                Ok(Ok(())) => process::exit(0),
                Ok(Err(e)) => {
                    eprintln!("{}: {}", program_name, e);
                    process::exit(classify_compile_error(&e, EXIT_COMPILE, EXIT_LINK, EXIT_IO));
                }
                Err(_panic) => {
                    print_ice_report(&input_for_ice);
                    process::exit(EXIT_ICE);
                }
            }
        }
    }
}

fn emit_cli_warnings(program_name: &str, opts: &driver::Options) -> bool {
    if opts.cli_warnings.is_empty() {
        return false;
    }

    let label = if opts.warn_as_error {
        "error"
    } else {
        "warning"
    };
    for warning in &opts.cli_warnings {
        eprintln!("{}: {}: {}", program_name, label, warning);
    }
    opts.warn_as_error
}

/// Map a compile error message to the appropriate exit code.  Today
/// this is heuristic on the string content; a future structured
/// error type would let us be precise.
fn classify_compile_error(msg: &str, compile: i32, link: i32, io: i32) -> i32 {
    if msg.contains("cannot read")
        || msg.contains("cannot write")
        || msg.contains("No such file")
        || msg.contains("cannot open")
    {
        io
    } else if msg.contains("linker failed") || msg.contains("ld:") {
        link
    } else {
        compile
    }
}

/// Install a panic hook that prints a compact ICE banner BEFORE the
/// stack trace.  catch_unwind in `cli_entry` then formats the
/// "please report this" footer once the panic is caught.  Splitting
/// it this way means the user sees the bug-report template even when
/// `RUST_BACKTRACE` is unset (which suppresses the default hook
/// output).
fn install_ice_hook() {
    let prior = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!();
        eprintln!("INTERNAL COMPILER ERROR");
        if let Some(loc) = info.location() {
            eprintln!("  at {}:{}:{}", loc.file(), loc.line(), loc.column());
        }
        if let Some(msg) = info.payload().downcast_ref::<&'static str>() {
            eprintln!("  message: {}", msg);
        } else if let Some(msg) = info.payload().downcast_ref::<String>() {
            eprintln!("  message: {}", msg);
        }
        // Defer to the prior hook for the backtrace (only emitted
        // when RUST_BACKTRACE=1, but we want to keep that behaviour).
        prior(info);
    }));
}

fn print_ice_report(input: &str) {
    eprintln!();
    eprintln!("This is a bug in ARMFORTAS.  Please report it at:");
    eprintln!("  https://github.com/FortranGoingOnForty/armfortas/issues");
    eprintln!();
    eprintln!("Include this information:");
    eprintln!("  - ARMFORTAS version: {}", env!("CARGO_PKG_VERSION"));
    eprintln!(
        "  - Platform: {}-{}-{}",
        std::env::consts::ARCH,
        std::env::consts::FAMILY,
        std::env::consts::OS
    );
    eprintln!("  - Source file: {}", input);
    eprintln!("  - Re-run with RUST_BACKTRACE=1 for a stack trace.");
}
