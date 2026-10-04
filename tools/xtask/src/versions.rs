//! One version for the whole product: the workspace's. The npm packages
//! carry it, the desktop app takes it from Cargo (its Tauri config sets
//! none), the changelog has a section for it, and a release tag names it.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;

use crate::coverage::Outcome;

const NPM_PACKAGES: &[&str] = &[
    "apps/desktop/package.json",
    "apps/pwa/package.json",
    "packages/ui/package.json",
];
const TAURI_CONFIG: &str = "apps/desktop/src-tauri/tauri.conf.json";
const CHANGELOG: &str = "CHANGELOG.md";

/// Checks the repository at `root`; with `tag` (e.g. `v0.3.0`), also that
/// the tag names the workspace version.
pub(crate) fn check(root: &Path, tag: Option<&str>) -> Result<Outcome> {
    let read =
        |path: &str| fs::read_to_string(root.join(path)).with_context(|| format!("reading {path}"));
    let mut packages = Vec::with_capacity(NPM_PACKAGES.len());
    for &path in NPM_PACKAGES {
        packages.push((path, read(path)?));
    }
    let problems = problems(&Sources {
        cargo_toml: &read("Cargo.toml")?,
        packages: &packages,
        tauri_config: &read(TAURI_CONFIG)?,
        changelog: &read(CHANGELOG)?,
        tag,
    })?;
    let mut text = String::new();
    for problem in &problems {
        writeln!(text, "version mismatch: {problem}")?;
    }
    if problems.is_empty() {
        text.push_str("versions agree\n");
    }
    Ok(Outcome {
        passed: problems.is_empty(),
        text,
    })
}

struct Sources<'a> {
    cargo_toml: &'a str,
    packages: &'a [(&'a str, String)],
    tauri_config: &'a str,
    changelog: &'a str,
    tag: Option<&'a str>,
}

#[derive(Deserialize)]
struct CargoToml {
    workspace: Workspace,
}

#[derive(Deserialize)]
struct Workspace {
    package: WorkspacePackage,
}

#[derive(Deserialize)]
struct WorkspacePackage {
    version: String,
}

#[derive(Deserialize)]
struct Versioned {
    version: Option<String>,
}

fn problems(sources: &Sources<'_>) -> Result<Vec<String>> {
    let cargo: CargoToml = toml::from_str(sources.cargo_toml).context("parsing Cargo.toml")?;
    let version = cargo.workspace.package.version;
    let mut problems = Vec::new();
    for (path, text) in sources.packages {
        let found: Versioned =
            serde_json::from_str(text).with_context(|| format!("parsing {path}"))?;
        if found.version.as_deref() != Some(version.as_str()) {
            problems.push(format!(
                "{path} has {found:?}, the workspace {version}",
                found = found.version
            ));
        }
    }
    let tauri: Versioned = serde_json::from_str(sources.tauri_config)
        .with_context(|| format!("parsing {TAURI_CONFIG}"))?;
    if tauri.version.is_some() {
        problems.push(format!(
            "{TAURI_CONFIG} sets a version; remove it so Tauri takes Cargo's"
        ));
    }
    let heading = format!("## [{version}]");
    if !sources
        .changelog
        .lines()
        .any(|line| line.starts_with(&heading))
    {
        problems.push(format!("{CHANGELOG} has no `{heading}` section"));
    }
    if let Some(tag) = sources.tag
        && tag.strip_prefix('v') != Some(version.as_str())
    {
        problems.push(format!(
            "tag {tag} does not name the workspace version {version}"
        ));
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO: &str = "[workspace.package]\nversion = \"0.3.0\"\n";
    const CHANGES: &str = "# Changes\n\n## [0.3.0] — 2026-10-04\n";

    fn check(
        packages: &[(&str, String)],
        tauri: &str,
        changelog: &str,
        tag: Option<&str>,
    ) -> Vec<String> {
        problems(&Sources {
            cargo_toml: CARGO,
            packages,
            tauri_config: tauri,
            changelog,
            tag,
        })
        .unwrap()
    }

    fn package(version: &str) -> (&'static str, String) {
        (
            "apps/pwa/package.json",
            format!(r#"{{"name": "pwa", "version": "{version}"}}"#),
        )
    }

    #[test]
    fn agreeing_versions_pass_and_a_matching_tag_too() {
        let packages = [package("0.3.0")];
        assert!(check(&packages, "{}", CHANGES, None).is_empty());
        assert!(check(&packages, "{}", CHANGES, Some("v0.3.0")).is_empty());
    }

    #[test]
    fn every_disagreement_is_reported() {
        let found = check(
            &[package("0.2.0")],
            r#"{"version": "0.1.1"}"#,
            "## [0.2.0]\n",
            Some("v0.3.1"),
        );
        assert_eq!(found.len(), 4, "{found:#?}");
    }
}
