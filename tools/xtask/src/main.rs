//! Repository automation that behaves the same on Windows and Linux:
//! `cargo run -p xtask -- <task>`.

mod coverage;

use std::io::Write as _;
use std::process::ExitCode;

use anyhow::{Result, bail};

const USAGE: &str =
    "usage: cargo run -p xtask -- coverage-gate [--report <llvm-cov.json>] [--bump]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (code, text) = match run(&args) {
        Ok(outcome) if outcome.passed => (ExitCode::SUCCESS, outcome.text),
        Ok(outcome) => (ExitCode::FAILURE, outcome.text),
        Err(e) => (ExitCode::from(2), format!("xtask: {e:#}\n")),
    };
    if std::io::stdout().lock().write_all(text.as_bytes()).is_err() {
        return ExitCode::FAILURE;
    }
    code
}

fn run(args: &[String]) -> Result<coverage::Outcome> {
    match args.split_first() {
        Some((task, rest)) if task == "coverage-gate" => {
            coverage::gate(&coverage::Options::parse(rest)?)
        }
        _ => bail!("{USAGE}"),
    }
}
