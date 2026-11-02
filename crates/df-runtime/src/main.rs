//! df-runtime CLI entrypoint. Command handling lives in `cli.rs`.

mod cli;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::execute(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{failure}");
            ExitCode::from(failure.code)
        }
    }
}
