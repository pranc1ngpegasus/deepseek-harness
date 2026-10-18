#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::assert_is_empty,
        clippy::bool_assert_comparison,
        clippy::float_cmp
    )
)]
//! Repository gates for the Rust workspace.
//!
//! `mutation-gate` reads the `outcomes.json` cargo-mutants writes and enforces
//! the two thresholds the port owns: zero surviving mutants in pure crates and
//! a workspace-wide kill rate of at least 98%.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Pure crates whose surviving mutants fail the gate outright.
const PURE_CRATES: [&str; 6] = [
    "dsh-agent",
    "dsh-args",
    "dsh-profile",
    "dsh-config",
    "dsh-shutdown",
    "dsh-diagnostics",
];

/// Killed mutants required across every crate, as a percentage.
const MINIMUM_KILLED_PERCENT: f64 = 98.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Killed,
    Survived,
    Timeout,
    Unviable,
    Failure,
    Skipped,
}

impl Verdict {
    fn from_summary(summary: &str) -> Option<Self> {
        match summary {
            "Success" | "CaughtMutant" | "caught" => Some(Self::Killed),
            "MissedMutant" | "missed" | "Survived" => Some(Self::Survived),
            "Timeout" | "timeout" => Some(Self::Timeout),
            "Unviable" | "unviable" => Some(Self::Unviable),
            "Failure" | "failure" => Some(Self::Failure),
            "Skipped" | "skipped" => Some(Self::Skipped),
            other => {
                eprintln!("mutation-gate: unrecognized mutant summary {other:?}");
                None
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Mutant {
    crate_name: String,
    scenario: String,
    verdict: Verdict,
}

#[derive(Debug, Default)]
struct Totals {
    killed: u64,
    survived: u64,
    timeout: u64,
    unviable: u64,
    failure: u64,
    skipped: u64,
}

impl Totals {
    const fn record(&mut self, verdict: Verdict) {
        match verdict {
            Verdict::Killed => self.killed += 1,
            Verdict::Survived => self.survived += 1,
            Verdict::Timeout => self.timeout += 1,
            Verdict::Unviable => self.unviable += 1,
            Verdict::Failure => self.failure += 1,
            Verdict::Skipped => self.skipped += 1,
        }
    }

    const fn scored(&self) -> u64 {
        self.killed + self.survived + self.timeout
    }

    fn killed_percent(&self) -> f64 {
        let scored = self.scored();
        if scored == 0 {
            return 100.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let percentage = self.killed as f64 * 100.0 / scored as f64;
        percentage
    }
}

fn crate_name(outcome: &serde_json::Value) -> Option<String> {
    outcome
        .pointer("/scenario/Mutant/package")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn scenario_name(outcome: &serde_json::Value) -> String {
    outcome
        .pointer("/scenario/Mutant/name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn load(path: &Path) -> Result<Vec<Mutant>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let document: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    let outcomes = document
        .get("outcomes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{} has no outcomes array", path.display()))?;
    let mut mutants = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        // The baseline run is not a mutant; counting its success as a kill
        // would inflate the score.
        if outcome.get("scenario").and_then(serde_json::Value::as_str) == Some("Baseline") {
            continue;
        }
        // `summary` is the outcome discriminator in cargo-mutants' JSON report;
        // older versions used a bare string. Accept both spellings.
        let summary = outcome
            .get("summary")
            .and_then(|summary| {
                summary
                    .as_str()
                    .or_else(|| summary.get("summary").and_then(serde_json::Value::as_str))
            })
            .ok_or_else(|| format!("an outcome in {} has no summary", path.display()))?;
        let Some(verdict) = Verdict::from_summary(summary) else {
            continue;
        };
        let Some(crate_name) = crate_name(outcome) else {
            return Err(format!(
                "an outcome in {} does not name its package",
                path.display()
            ));
        };
        mutants.push(Mutant {
            crate_name,
            scenario: scenario_name(outcome),
            verdict,
        });
    }
    Ok(mutants)
}

fn default_report_dir() -> PathBuf {
    PathBuf::from("mutants.out")
}

fn mutation_gate(args: &[String]) -> Result<bool, String> {
    let mut report_dir = default_report_dir();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--report-dir" => {
                index += 1;
                report_dir = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| "--report-dir needs a path".to_string())?,
                );
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
        index += 1;
    }
    let mutants = load(&report_dir.join("outcomes.json"))?;
    if mutants.is_empty() {
        return Err(format!(
            "{} records no mutants; a gate over an empty run proves nothing",
            report_dir.join("outcomes.json").display()
        ));
    }

    let mut overall = Totals::default();
    let mut by_crate: std::collections::BTreeMap<&str, Totals> = std::collections::BTreeMap::new();
    for mutant in &mutants {
        overall.record(mutant.verdict);
        let entry = by_crate.entry(mutant.crate_name.as_str()).or_default();
        entry.record(mutant.verdict);
    }

    let mut failed = false;
    for name in PURE_CRATES {
        let Some(totals) = by_crate.get(name) else {
            eprintln!("mutation-gate: pure crate {name} has no mutants; the run is incomplete");
            failed = true;
            continue;
        };
        if totals.survived > 0 {
            failed = true;
            eprintln!(
                "mutation-gate: {name} has {} surviving mutant(s)",
                totals.survived
            );
            for mutant in mutants
                .iter()
                .filter(|mutant| mutant.crate_name == name && mutant.verdict == Verdict::Survived)
            {
                eprintln!("  survived: {}", mutant.scenario);
            }
        }
        if totals.timeout > 0 {
            failed = true;
            eprintln!(
                "mutation-gate: {name} has {} timed-out mutant(s); a pure function must terminate",
                totals.timeout
            );
        }
    }

    let killed = overall.killed_percent();
    if killed < MINIMUM_KILLED_PERCENT {
        failed = true;
    }
    println!(
        "mutation-gate: killed {} of {} scored mutants ({killed:.2}%), survived {}, timeout {}, unviable {}, failure {}, skipped {}",
        overall.killed,
        overall.scored(),
        overall.survived,
        overall.timeout,
        overall.unviable,
        overall.failure,
        overall.skipped
    );
    for (name, totals) in &by_crate {
        println!(
            "  {name}: killed {}/{}, survived {}, timeout {}",
            totals.killed,
            totals.scored(),
            totals.survived,
            totals.timeout
        );
    }
    if failed {
        eprintln!(
            "mutation-gate: FAILED; pure crates need 0 survivors and the workspace needs at least {MINIMUM_KILLED_PERCENT:.0}% killed"
        );
    } else {
        println!("mutation-gate: PASSED");
    }
    Ok(!failed)
}

fn usage() {
    eprintln!("usage: xtask mutation-gate [--report-dir <path>]");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = if let Some("mutation-gate") = args.first().map(String::as_str) {
        mutation_gate(&args[1..])
    } else {
        usage();
        return ExitCode::from(2);
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Totals, Verdict, load};
    use std::io::Write;

    #[test]
    fn parses_a_cargo_mutants_report_and_skips_the_baseline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("outcomes.json");
        let mut file = std::fs::File::create(&path).expect("create");
        write!(
            file,
            r#"{{"outcomes":[
              {{"scenario":"Baseline","summary":"Success"}},
              {{"scenario":{{"Mutant":{{"name":"replace add with sub","package":"dsh-args"}}}},"summary":"CaughtMutant"}},
              {{"scenario":{{"Mutant":{{"name":"replace parse with None","package":"dsh-args"}}}},"summary":"MissedMutant"}},
              {{"scenario":{{"Mutant":{{"name":"x","package":"dsh-cli"}}}},"summary":"Timeout"}}
            ]}}"#
        )
        .expect("write");
        let mutants = load(&path).expect("load");
        assert_eq!(mutants.len(), 3);
        assert_eq!(mutants[0].crate_name, "dsh-args");
        assert_eq!(mutants[0].verdict, Verdict::Killed);
        assert_eq!(mutants[1].verdict, Verdict::Survived);
        assert_eq!(mutants[2].verdict, Verdict::Timeout);
        assert_eq!(mutants[2].scenario, "x");
    }

    #[test]
    fn kill_percentage_counts_killed_survived_and_timeout() {
        let mut totals = Totals::default();
        for verdict in [
            Verdict::Killed,
            Verdict::Killed,
            Verdict::Survived,
            Verdict::Timeout,
            Verdict::Unviable,
            Verdict::Skipped,
        ] {
            totals.record(verdict);
        }
        assert_eq!(totals.scored(), 4);
        assert!((totals.killed_percent() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn an_empty_report_loads_no_mutants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("outcomes.json");
        std::fs::write(&path, r#"{"outcomes":[]}"#).expect("write");
        assert!(load(&path).expect("load").is_empty());
    }
}
