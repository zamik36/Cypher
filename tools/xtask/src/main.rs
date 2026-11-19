//! Repository automation that behaves the same on Windows and Linux:
//! `cargo run -p xtask -- <task>`.

mod coverage;
mod versions;

use std::io::Write as _;
use std::process::ExitCode;

use anyhow::{Result, bail};

const USAGE: &str = "usage: cargo run -p xtask -- <task>
  coverage-gate [--report <llvm-cov.json>] [--bump]
  versions [--tag <v0.3.0>]";

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
        Some((task, rest)) if task == "versions" => {
            let tag = match rest {
                [] => None,
                [flag, tag] if flag == "--tag" => Some(tag.as_str()),
                _ => bail!("{USAGE}"),
            };
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            versions::check(&root, tag)
        }
        _ => bail!("{USAGE}"),
    }
}
