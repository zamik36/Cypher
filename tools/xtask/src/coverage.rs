//! Per-crate line-coverage ratchet over `cargo llvm-cov --json` output.
//!
//! `.config/coverage.toml` holds a floor and a target per crate. The gate
//! fails when a crate drops below its floor; `--bump` raises floors to just
//! under the measured value and never lowers them, so every raise shows up
//! in review.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

const CONFIG_PATH: &str = ".config/coverage.toml";
const DEFAULT_REPORT: &str = "target/cov.json";
const HEADER: &str = "\
# Line-coverage ratchet, checked by `cargo run -p xtask -- coverage-gate`.
# floor: CI fails below it. `--bump` raises it to one point under the measured
# value (run-to-run noise) and never lowers it. target: where the floor has to
# end up. Floors come from the Linux CI job.
# Rewritten by `--bump`; edit targets and `excluded` by hand.
";

pub(crate) struct Options {
    report: PathBuf,
    bump: bool,
}

impl Options {
    pub(crate) fn parse(args: &[String]) -> Result<Self> {
        let mut report = PathBuf::from(DEFAULT_REPORT);
        let mut bump = false;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--bump" => bump = true,
                "--report" => report = args.next().context("--report needs a path")?.into(),
                other => bail!("unknown argument `{other}`"),
            }
        }
        Ok(Self { report, bump })
    }
}

pub(crate) struct Outcome {
    pub(crate) passed: bool,
    pub(crate) text: String,
}

pub(crate) fn gate(options: &Options) -> Result<Outcome> {
    let workspace = Workspace::load()?;
    let config_path = workspace.root.join(CONFIG_PATH);
    let config = Config::parse(&read(&config_path)?)?;
    let report: Export = serde_json::from_str(&read(&workspace.root.join(&options.report))?)
        .context("parsing the llvm-cov JSON report")?;
    let measured = tally(report.files(), &workspace.crates);
    let names: Vec<&str> = workspace.crates.iter().map(|c| c.name.as_str()).collect();
    let rows = evaluate(&config, &names, &measured)?;
    let mut text = table(&rows)?;
    if options.bump {
        let bumped = config.bumped(&measured);
        text.push_str(&bump_summary(&config, &bumped)?);
        fs::write(&config_path, bumped.render()?)
            .with_context(|| format!("writing {}", config_path.display()))?;
    }
    Ok(Outcome {
        passed: rows.iter().all(Row::passes),
        text,
    })
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    excluded: Vec<String>,
    crates: BTreeMap<String, Threshold>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Threshold {
    floor: f64,
    target: f64,
}

impl Config {
    fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("parsing .config/coverage.toml")
    }

    fn bumped(&self, measured: &BTreeMap<&str, Lines>) -> Self {
        let crates = self
            .crates
            .iter()
            .map(|(name, t)| {
                let floor = measured
                    .get(name.as_str())
                    .map_or(t.floor, |lines| t.floor.max(floor_for(lines.percent())));
                (name.clone(), Threshold { floor, ..*t })
            })
            .collect();
        Self {
            excluded: self.excluded.clone(),
            crates,
        }
    }

    fn render(&self) -> Result<String> {
        let mut out = String::from(HEADER);
        let excluded: Vec<String> = self.excluded.iter().map(|n| format!("{n:?}")).collect();
        writeln!(out, "\nexcluded = [{}]\n\n[crates]", excluded.join(", "))?;
        for (name, t) in &self.crates {
            writeln!(
                out,
                "{name} = {{ floor = {:.1}, target = {} }}",
                t.floor, t.target
            )?;
        }
        Ok(out)
    }
}

/// Timing-dependent paths (reconnects, retries) run in some test runs and
/// not others, moving a crate by about a point; the floor leaves that room.
const SLACK: f64 = 1.0;

/// The floor `--bump` sets for a measured percentage.
fn floor_for(percent: f64) -> f64 {
    ((percent - SLACK) * 10.0).floor() / 10.0
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
struct Lines {
    count: u32,
    covered: u32,
}

impl Lines {
    fn percent(self) -> f64 {
        if self.count == 0 {
            100.0
        } else {
            f64::from(self.covered) * 100.0 / f64::from(self.count)
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            count: self.count.saturating_add(other.count),
            covered: self.covered.saturating_add(other.covered),
        }
    }
}

/// The subset of `llvm-cov export --summary-only` the gate reads.
#[derive(Deserialize)]
struct Export {
    data: Vec<ExportData>,
}

#[derive(Deserialize)]
struct ExportData {
    files: Vec<FileEntry>,
}

#[derive(Deserialize)]
struct FileEntry {
    filename: String,
    summary: FileSummary,
}

#[derive(Deserialize)]
struct FileSummary {
    lines: Lines,
}

impl Export {
    fn files(&self) -> impl Iterator<Item = &FileEntry> {
        self.data.iter().flat_map(|d| &d.files)
    }
}

struct Workspace {
    root: PathBuf,
    crates: Vec<CrateDir>,
}

struct CrateDir {
    name: String,
    /// Normalised directory with a trailing `/`, see [`normalise`].
    dir: String,
}

impl Workspace {
    fn load() -> Result<Self> {
        #[derive(Deserialize)]
        struct Metadata {
            workspace_root: PathBuf,
            packages: Vec<Package>,
        }
        #[derive(Deserialize)]
        struct Package {
            name: String,
            manifest_path: PathBuf,
        }

        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = Command::new(cargo)
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .output()
            .context("running cargo metadata")?;
        if !output.status.success() {
            bail!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let meta: Metadata = serde_json::from_slice(&output.stdout)?;
        let crates = meta
            .packages
            .into_iter()
            .filter_map(|p| {
                let dir = p.manifest_path.parent()?;
                Some(CrateDir {
                    dir: format!("{}/", normalise(&dir.to_string_lossy())),
                    name: p.name,
                })
            })
            .collect();
        Ok(Self {
            root: meta.workspace_root,
            crates,
        })
    }
}

/// Forward slashes, and on Windows one case, so llvm-cov and cargo paths compare.
fn normalise(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    if cfg!(windows) {
        path.make_ascii_lowercase();
    }
    path
}

/// The innermost crate whose directory contains `file`.
fn crate_of<'a>(file: &str, crates: &'a [CrateDir]) -> Option<&'a str> {
    let file = normalise(file);
    crates
        .iter()
        .filter(|c| file.starts_with(&c.dir))
        .max_by_key(|c| c.dir.len())
        .map(|c| c.name.as_str())
}

fn tally<'a>(
    files: impl Iterator<Item = &'a FileEntry>,
    crates: &'a [CrateDir],
) -> BTreeMap<&'a str, Lines> {
    let mut totals = BTreeMap::new();
    for file in files {
        if let Some(name) = crate_of(&file.filename, crates) {
            let total: &mut Lines = totals.entry(name).or_default();
            *total = total.add(file.summary.lines);
        }
    }
    totals
}

#[derive(Debug)]
struct Row<'a> {
    name: &'a str,
    lines: Lines,
    threshold: Threshold,
}

impl Row<'_> {
    fn passes(&self) -> bool {
        self.lines.percent() >= self.threshold.floor
    }
}

/// Every workspace crate must be gated or explicitly excluded, so a new crate
/// cannot slip past the ratchet.
fn evaluate<'a>(
    config: &'a Config,
    workspace: &[&str],
    measured: &BTreeMap<&str, Lines>,
) -> Result<Vec<Row<'a>>> {
    for name in workspace {
        if !config.crates.contains_key(*name) && !config.excluded.iter().any(|e| e == name) {
            bail!("crate `{name}` is neither gated nor excluded in {CONFIG_PATH}");
        }
    }
    config
        .crates
        .iter()
        .map(|(name, &threshold)| {
            if !workspace.contains(&name.as_str()) {
                bail!("{CONFIG_PATH} gates `{name}`, which is not a workspace crate");
            }
            let lines = *measured
                .get(name.as_str())
                .with_context(|| format!("no coverage data for `{name}`"))?;
            Ok(Row {
                name,
                lines,
                threshold,
            })
        })
        .collect()
}

fn table(rows: &[Row<'_>]) -> Result<String> {
    let mut out = format!(
        "{:<20} {:>7} {:>8} {:>7} {:>7}\n",
        "crate", "lines", "covered", "floor", "target"
    );
    for row in rows {
        let status = if row.passes() { "ok" } else { "BELOW FLOOR" };
        writeln!(
            out,
            "{:<20} {:>7} {:>7.1}% {:>7.1} {:>7} {status}",
            row.name,
            row.lines.count,
            row.lines.percent(),
            row.threshold.floor,
            row.threshold.target,
        )?;
    }
    Ok(out)
}

fn bump_summary(before: &Config, after: &Config) -> Result<String> {
    let mut out = String::new();
    for (name, new) in &after.crates {
        if let Some(old) = before.crates.get(name)
            && new.floor > old.floor
        {
            writeln!(out, "floor {name}: {:.1} -> {:.1}", old.floor, new.floor)?;
        }
    }
    if out.is_empty() {
        out.push_str("no floor raised\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crates() -> Vec<CrateDir> {
        [
            ("core", "/ws/crates/core/"),
            ("desktop", "/ws/apps/desktop/"),
            ("tauri", "/ws/apps/desktop/src-tauri/"),
        ]
        .into_iter()
        .map(|(name, dir)| CrateDir {
            name: name.into(),
            dir: dir.into(),
        })
        .collect()
    }

    fn lines(count: u32, covered: u32) -> Lines {
        Lines { count, covered }
    }

    fn config(entries: &[(&str, f64, f64)], excluded: &[&str]) -> Config {
        Config {
            excluded: excluded.iter().map(|&e| e.into()).collect(),
            crates: entries
                .iter()
                .map(|&(name, floor, target)| (name.into(), Threshold { floor, target }))
                .collect(),
        }
    }

    #[test]
    fn files_map_to_the_innermost_crate() {
        let crates = crates();
        assert_eq!(
            crate_of("/ws/crates/core/src/lib.rs", &crates),
            Some("core")
        );
        assert_eq!(
            crate_of("/ws/apps/desktop/src-tauri/src/main.rs", &crates),
            Some("tauri")
        );
        assert_eq!(
            crate_of("/ws/apps/desktop/build.rs", &crates),
            Some("desktop")
        );
        assert_eq!(crate_of("/ws/crates/core-extra/src/lib.rs", &crates), None);
        assert_eq!(crate_of("/registry/serde/src/lib.rs", &crates), None);
    }

    #[test]
    fn report_is_tallied_per_crate() {
        let json = r#"{"data":[{"files":[
            {"filename":"/ws/crates/core/src/a.rs","summary":{"lines":{"count":10,"covered":9,"percent":90}}},
            {"filename":"/ws/crates/core/src/b.rs","summary":{"lines":{"count":30,"covered":15,"percent":50}}},
            {"filename":"/elsewhere/x.rs","summary":{"lines":{"count":5,"covered":0,"percent":0}}}
        ]}]}"#;
        let export: Export = serde_json::from_str(json).unwrap();
        let crates = crates();
        let totals = tally(export.files(), &crates);
        assert_eq!(
            totals.into_iter().collect::<Vec<_>>(),
            [("core", lines(40, 24))]
        );
    }

    #[test]
    fn gate_fails_only_below_the_floor() {
        let config = config(&[("core", 60.0, 90.0), ("tauri", 50.0, 80.0)], &["desktop"]);
        let measured = BTreeMap::from([("core", lines(10, 6)), ("tauri", lines(10, 4))]);
        let rows = evaluate(&config, &["core", "desktop", "tauri"], &measured).unwrap();
        let verdicts: Vec<(&str, bool)> = rows.iter().map(|r| (r.name, r.passes())).collect();
        assert_eq!(verdicts, [("core", true), ("tauri", false)]);
        assert!(table(&rows).unwrap().contains("BELOW FLOOR"));
    }

    #[test]
    fn every_crate_must_be_gated_or_excluded() {
        let measured = BTreeMap::from([("core", lines(1, 1))]);
        let error = |gated: &str, workspace: &[&str]| {
            let config = config(&[(gated, 0.0, 90.0)], &[]);
            evaluate(&config, workspace, &measured)
                .unwrap_err()
                .to_string()
        };
        assert!(error("core", &["core", "tauri"]).contains("`tauri` is neither gated"));
        assert!(error("gone", &[]).contains("`gone`, which is not"));
        assert!(error("tauri", &["tauri"]).contains("no coverage data"));
    }

    #[test]
    fn bump_raises_floors_and_never_lowers_them() {
        let before = config(&[("core", 50.0, 90.0), ("tauri", 70.0, 80.0)], &["desktop"]);
        let measured = BTreeMap::from([("core", lines(3, 2)), ("tauri", lines(10, 6))]);
        let after = before.bumped(&measured);
        assert_eq!(
            after,
            config(&[("core", 65.6, 90.0), ("tauri", 70.0, 80.0)], &["desktop"])
        );
        let summary = bump_summary(&before, &after).unwrap();
        assert_eq!(summary, "floor core: 50.0 -> 65.6\n");
        assert_eq!(bump_summary(&after, &after).unwrap(), "no floor raised\n");
    }

    #[test]
    fn rendered_config_parses_back() {
        let original = config(
            &[("core", 66.6, 90.0), ("tauri", 0.0, 80.0)],
            &["desktop", "xtask"],
        );
        let text = original.render().unwrap();
        assert!(text.starts_with(HEADER));
        assert_eq!(Config::parse(&text).unwrap(), original);
    }

    #[test]
    fn options_parse() {
        let args = ["--bump", "--report", "x.json"].map(String::from);
        let options = Options::parse(&args).unwrap();
        assert!(options.bump);
        assert_eq!(options.report, PathBuf::from("x.json"));
        assert!(Options::parse(&["--nope".into()]).is_err());
        assert!(Options::parse(&["--report".into()]).is_err());
        assert!(!Options::parse(&[]).unwrap().bump);
    }

    #[test]
    fn empty_crate_counts_as_fully_covered() {
        assert!(lines(0, 0).percent() >= 100.0);
        assert!(lines(4, 1).percent() >= 25.0 && lines(4, 1).percent() < 25.1);
    }
}
