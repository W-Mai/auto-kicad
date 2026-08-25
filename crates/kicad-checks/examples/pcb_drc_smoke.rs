//! Read-only live DRC smoke against a real board using the public runner.
//!
//! Build arguments are injected through fixed environment variables only:
//! `KICAD_CHECKS_ROOT` and `KICAD_CHECKS_BOARD`. No arbitrary command
//! arguments, executable, flags, or report path are accepted. The runner
//! strictly confines the board under `allowed_root`, writes its report to an
//! internal temp dir, and removes every internal artifact on all paths. This
//! never modifies the board file.

use std::collections::BTreeMap;
use std::env;
use std::path::Path;
use std::time::Duration;

use kicad_checks::{CheckResult, DrcPolicy, run_pcb_drc};

const ALLOWED_ROOT_ENV: &str = "KICAD_CHECKS_ROOT";
const BOARD_ENV: &str = "KICAD_CHECKS_BOARD";
const TIMEOUT_SECS: u64 = 60;

fn main() {
    let allowed_root = required_env(ALLOWED_ROOT_ENV);
    let board = required_env(BOARD_ENV);

    let policy = DrcPolicy {
        allowed_root: Path::new(&allowed_root).to_owned(),
        timeout: Duration::from_secs(TIMEOUT_SECS),
    };
    let result = run_pcb_drc(Path::new(&board), &policy);

    match result {
        CheckResult::Completed(report) => {
            let mut type_counts = BTreeMap::new();
            for violation in &report.violations {
                *type_counts
                    .entry(violation.rule_type.clone())
                    .or_insert(0usize) += 1;
            }
            println!(
                "{}",
                serde_json::json!({
                    "status": "completed",
                    "schema": report.schema,
                    "parser_version": report.parser_version,
                    "kicad_version": report.kicad_version,
                    "source": report.source,
                    "violations": report.violations.len(),
                    "unconnected_items": report.unconnected_items,
                    "ignored_checks": report.ignored_checks,
                    "by_type": type_counts,
                })
            );
        }
        CheckResult::Failed(failure) => {
            eprintln!("DRC check failed: {failure:?}");
            std::process::exit(1);
        }
    }
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        eprintln!("missing required environment variable {name:?}");
        std::process::exit(2);
    })
}
