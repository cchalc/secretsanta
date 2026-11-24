//! Binary entry point: builds the real environment and hands it to the CLI.

use std::io;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::Parser;
use santa::cli::{Cli, Io, run};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stdout();
    let mut err = io::stderr();
    let env = |k: &str| std::env::var(k).ok();
    let mut io = Io {
        out: &mut out,
        err: &mut err,
        input: &mut input,
        now: SystemTime::now(),
        env: &env,
    };
    match run(cli, &mut io) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}
