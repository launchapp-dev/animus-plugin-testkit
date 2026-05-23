//! Resolve a scenario directory: caller-supplied or the bundled baseline.

use std::path::PathBuf;

use anyhow::{Context, Result};
use testkit_core::{load_scenario_dir, ScenarioFile};

pub fn resolve(dir: Option<PathBuf>) -> Result<Vec<ScenarioFile>> {
    let dir = match dir {
        Some(p) => p,
        None => default_scenarios_dir(),
    };
    let scenarios = load_scenario_dir(&dir)
        .with_context(|| format!("loading scenarios from {}", dir.display()))?;
    if scenarios.is_empty() {
        anyhow::bail!("no scenarios found in {}", dir.display());
    }
    Ok(scenarios)
}

fn default_scenarios_dir() -> PathBuf {
    // Walk up from CARGO_MANIFEST_DIR (the harness crate) to the workspace root.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("scenarios"))
        .unwrap_or_else(|| PathBuf::from("scenarios"))
}
