//! Guard: an advisory against a crate this service *delivers* fails the build.
//!
//! On 2026-09-14 RUSTSEC-2026-0285 was published against `rustls` 0.23.40 —
//! shipped here since the service was written, through `sqlx`'s `tls-rustls`,
//! and since ADR-011 through `reqwest` as well. Nothing in this repository went
//! red. The only dependency guard, `tests/framework.rs`, names `h2` 0.3; the
//! finding reached us because a reviewer ran `cargo audit` by hand, and a
//! reviewer running a command by hand is precisely what does not repeat.
//! Same shape as the vacuous guards this service has already paid for: *a check
//! that says nothing about the inputs it was not shaped for is not a check.*
//!
//! The judgement itself is not `cargo audit`'s tally, which reads `Cargo.lock`
//! — a file wider than the binary, since it holds the optional dependencies of
//! our dependencies whether or not their feature is on. It is ADR-012's:
//! intersect the advisory database with `cargo tree -e normal`. Measured the
//! same day, that distinction is the difference between "1 vulnerability found"
//! and zero exposure: `rsa` is reachable only through `sqlx-mysql`, and `sqlx`
//! is taken with `postgres`.
//!
//! That comparison needs the advisory database, so it needs a network, so it
//! lives in a CI job (`scripts/audit-delivered-graph.sh`). What is asserted
//! here is everything about it that can be checked without one:
//!
//! 1. the workflow really runs it, on every push and every pull request — an
//!    audit that only runs on a schedule tells you a week late;
//! 2. the script's own classification, exercised on fixtures, including the
//!    cases where it must refuse to answer rather than answer "clean".
//!
//! Needs no database, no network and no cargo subprocess.

use std::path::{Path, PathBuf};
use std::process::Command;

const CI_YML: &str = include_str!("../.github/workflows/ci.yml");
const SCRIPT: &str = "scripts/audit-delivered-graph.sh";

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The workflow runs the audit, and runs it when a change arrives.
///
/// Both halves matter. A job nobody triggers and a job triggered a week later
/// are the same thing as the hand-run command this file replaces.
#[test]
fn the_workflow_audits_the_delivered_graph_on_every_change() {
    assert!(
        repo_root().join(SCRIPT).is_file(),
        "{SCRIPT} is missing, so the CI job below runs nothing"
    );

    assert!(
        CI_YML.contains(SCRIPT),
        "no job in .github/workflows/ci.yml runs {SCRIPT}.\n\
         Removing it puts this service back where it was on 2026-09-14: an \
         advisory against a crate it compiles, and nothing red."
    );

    let triggers = CI_YML
        .split_once("\njobs:")
        .expect("the workflow must declare a `jobs:` block")
        .0;
    for trigger in ["push:", "pull_request:"] {
        assert!(
            triggers.contains(trigger),
            "the workflow does not run on `{trigger}`, so the audit would not \
             see the change that introduces the advisory"
        );
    }
}

/// A vulnerability against a crate we compile fails the run.
#[test]
fn a_delivered_vulnerability_fails() {
    let run = run_script(
        "ods-doceditor v0.1.0 (/repo)\nrustls v0.23.40\nserde v1.0.0\n",
        &report(&[("RUSTSEC-2026-0285", "rustls", "0.23.40")], &[]),
    );

    assert_eq!(
        run.code,
        1,
        "a delivered vulnerability must fail the run.\n{}",
        run.all()
    );
    assert!(
        run.stdout.contains("rustls 0.23.40") && run.stdout.contains("yes"),
        "the failure must name the crate and say it is delivered:\n{}",
        run.all()
    );
}

/// The same advisory against a crate only the lockfile knows does not.
///
/// This is the whole reason the script exists rather than a bare `cargo audit`:
/// `rsa` has carried RUSTSEC-2023-0071 since 2023, has no fixed release, and is
/// reached by nothing this service compiles. A check that cried wolf on it
/// would be turned off, and then the next `rustls` would pass unseen.
#[test]
fn a_vulnerability_outside_the_delivered_graph_does_not() {
    let run = run_script(
        "ods-doceditor v0.1.0 (/repo)\nrustls v0.23.45\nserde v1.0.0\n",
        &report(&[("RUSTSEC-2023-0071", "rsa", "0.9.10")], &[]),
    );

    assert_eq!(
        run.code,
        0,
        "an advisory against a crate nothing compiles must not fail the run.\n{}",
        run.all()
    );
    assert!(
        run.stdout.contains("rsa 0.9.10"),
        "it must still be reported, with its verdict:\n{}",
        run.all()
    );
}

/// A version that differs is a different crate, on both sides.
///
/// The fix for this batch is a lockfile bump, so the comparison has to be on
/// `name version` and not on the name: a guard matching `rustls` alone would
/// have stayed red after the upgrade, and been silenced.
#[test]
fn the_comparison_is_on_the_version_too() {
    let run = run_script(
        "ods-doceditor v0.1.0 (/repo)\nrustls v0.23.45\n",
        &report(&[("RUSTSEC-2026-0285", "rustls", "0.23.40")], &[]),
    );

    assert_eq!(
        run.code,
        0,
        "0.23.45 is delivered, 0.23.40 is what the advisory names — the \
         upgrade is the fix and must read as one.\n{}",
        run.all()
    );
}

/// `yanked` and `unsound` are reported and do not fail, delivered or not.
///
/// The line `cargo audit` itself draws. It is stated here so that moving it is
/// a decision someone takes, rather than a side effect of a refactor.
#[test]
fn warnings_are_reported_without_failing() {
    let run = run_script(
        "ods-doceditor v0.1.0 (/repo)\nspin v0.9.8\n",
        &report(&[], &[("yanked", "spin", "0.9.8")]),
    );

    assert_eq!(
        run.code,
        0,
        "a warning must not fail the run.\n{}",
        run.all()
    );
    assert!(
        run.stdout.contains("spin 0.9.8") && run.stdout.contains("yanked"),
        "a warning must still be printed:\n{}",
        run.all()
    );
}

/// Nothing measured is not a pass.
///
/// The empty set is a subset of everything, so a `cargo tree` that printed
/// nothing would clear every advisory at once — the exact failure mode
/// `tests/framework.rs` guards against on the other side, and the one that lets
/// a guard go green while guarding nothing.
#[test]
fn an_unmeasurable_graph_is_neither_pass_nor_fail() {
    let empty_graph = run_script(
        "",
        &report(&[("RUSTSEC-2026-0285", "rustls", "0.23.40")], &[]),
    );
    assert_eq!(
        empty_graph.code,
        2,
        "an empty delivered graph must be refused, not cleared.\n{}",
        empty_graph.all()
    );

    let broken_report = run_script("ods-doceditor v0.1.0 (/repo)\n", "not json at all");
    assert_eq!(
        broken_report.code,
        2,
        "an unreadable advisory report must be refused, not cleared.\n{}",
        broken_report.all()
    );
}

// -- harness ---------------------------------------------------------------

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn all(&self) -> String {
        format!(
            "--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout, self.stderr
        )
    }
}

/// `cargo audit --json`, reduced to the two shapes the script reads.
fn report(vulnerabilities: &[(&str, &str, &str)], warnings: &[(&str, &str, &str)]) -> String {
    let package = |name: &str, version: &str| {
        format!(r#""package":{{"name":"{name}","version":"{version}"}}"#)
    };
    let vulns: Vec<String> = vulnerabilities
        .iter()
        .map(|(id, name, version)| {
            format!(
                r#"{{"advisory":{{"id":"{id}"}},"versions":{{"patched":[]}},{}}}"#,
                package(name, version)
            )
        })
        .collect();
    let warns: Vec<String> = warnings
        .iter()
        .map(|(kind, name, version)| {
            format!(
                r#""{kind}":[{{"kind":"{kind}","advisory":null,"versions":null,{}}}]"#,
                package(name, version)
            )
        })
        .collect();

    format!(
        r#"{{"vulnerabilities":{{"count":{},"list":[{}]}},"warnings":{{{}}}}}"#,
        vulns.len(),
        vulns.join(","),
        warns.join(",")
    )
}

/// Run the script against a fixed graph and a fixed advisory report.
fn run_script(delivered_graph: &str, audit_report: &str) -> Run {
    let dir = scratch_dir();
    let graph_file = dir.join("graph.txt");
    let report_file = dir.join("audit.json");
    std::fs::write(&graph_file, delivered_graph).expect("fixture must be writable");
    std::fs::write(&report_file, audit_report).expect("fixture must be writable");

    let out = Command::new("bash")
        .arg(repo_root().join(SCRIPT))
        .current_dir(repo_root())
        .env("DELIVERED_GRAPH_FILE", &graph_file)
        .env("AUDIT_REPORT_FILE", &report_file)
        // Proof that no measurement is taken from the machine: if the script
        // ever fell back to running cargo, this is what it would try to run.
        .env("CARGO", "/nonexistent/cargo")
        .output()
        .expect("bash must be runnable");

    let _ = std::fs::remove_dir_all(&dir);
    Run {
        code: out
            .status
            .code()
            .expect("the script must exit, not be signalled"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "doceditor-advisories-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a scratch directory must be creatable");
    dir
}
