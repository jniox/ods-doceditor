//! Guard: the CI jobs that run `cargo` install the same system packages the
//! Dockerfile's builder stage installs.
//!
//! Why this file exists. `rdkafka` is taken with `cmake-build`, so building
//! this crate compiles librdkafka from source, and `rdkafka_conf.c` includes
//! `<curl/curl.h>` — even with `-DWITH_CURL=0`. The Dockerfile's builder stage
//! has installed `libcurl4-openssl-dev` since the service was written; the
//! workflow's apt step did not, so on 2026-09-13 both the `Lint` and `Test`
//! jobs of PR #3 died before compiling a single line of this repository:
//!
//!   error: failed to run custom build command for `rdkafka-sys v4.10.0+2.12.1`
//!   fatal error: curl/curl.h: No such file or directory
//!
//! It is invisible locally, which is the whole problem: a development machine
//! has libcurl's headers installed system-wide, so `cargo test` is green here
//! and red on the runner. The same signature has now cost a batch on oid,
//! notification-hub, ods-common, form-engine and billing-engine. It is one
//! missing package, and it comes back because nothing reads the two lists
//! together.
//!
//! The invariant asserted here is the one those five post-mortems converged
//! on: *it is the same compilation*, so the workflow must install at least
//! what the build image installs. The Dockerfile's list is the reference
//! because it is the list that demonstrably compiles the crate.
//!
//! Needs no database, no network and no cargo subprocess.

use std::collections::BTreeSet;

const DOCKERFILE: &str = include_str!("../Dockerfile");
const CI_YML: &str = include_str!("../.github/workflows/ci.yml");

/// Collect every package name passed to an `apt-get install` in `text`.
///
/// Handles both spellings in play here: one package per continuation line
/// (Dockerfile) and several packages on a single continuation line (the
/// workflow). A command is followed across backslash continuations and stops
/// at the first line that does not end with one, so `&& rm -rf …` tails and
/// the steps that follow are not swallowed.
fn apt_packages(text: &str) -> BTreeSet<String> {
    let mut packages = BTreeSet::new();
    let lines: Vec<&str> = text.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        if !line.contains("apt-get install") {
            continue;
        }

        // The install command, including every continuation line.
        let mut command = String::new();
        let mut j = i;
        loop {
            let current = lines[j].trim();
            command.push(' ');
            command.push_str(current);
            if !current.ends_with('\\') || j + 1 >= lines.len() {
                break;
            }
            j += 1;
        }

        // Only the `apt-get install` segment: a `&&`-chained tail such as
        // `rm -rf /var/lib/apt/lists/*` is a different command.
        let segment = command
            .split("&&")
            .find(|part| part.contains("apt-get install"))
            .unwrap_or("");

        for token in segment.split_whitespace() {
            let token = token.trim_end_matches('\\');
            let skip = token.is_empty()
                || token.starts_with('-') // -y, --no-install-recommends
                || token == "sudo"
                || token == "apt-get"
                || token == "install"
                || token == "RUN";
            if !skip {
                packages.insert(token.to_string());
            }
        }
    }

    packages
}

/// The packages the builder stage installs, i.e. the ones proven sufficient to
/// compile this crate. The runtime stage is deliberately excluded: it installs
/// what the *binary* needs to run, which is a different and smaller set.
fn builder_stage_packages() -> BTreeSet<String> {
    let after_builder = DOCKERFILE
        .split_once("AS builder")
        .expect("the Dockerfile must declare a stage named `builder`")
        .1;
    // Stop at the next stage, so a later `FROM` cannot contribute packages.
    let builder_stage = after_builder
        .split_once("\nFROM ")
        .map(|(stage, _)| stage)
        .unwrap_or(after_builder);

    apt_packages(builder_stage)
}

/// The workflow's jobs, as `(name, body)`, keeping only those that invoke
/// cargo — they are the ones that have to compile the crate.
///
/// Comment lines are dropped first: this file explains itself at length, and a
/// comment mentioning `cargo` must not make a job look like a build job.
fn cargo_jobs() -> Vec<(String, String)> {
    let uncommented: String = CI_YML
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let jobs_block = uncommented
        .split_once("\njobs:\n")
        .expect("the workflow must declare a `jobs:` block")
        .1;

    let mut jobs: Vec<(String, String)> = Vec::new();
    for line in jobs_block.lines() {
        let name = line.trim_end();
        let is_job_header = name.starts_with("  ")
            && !name.starts_with("   ")
            && name.ends_with(':')
            && !name.trim().contains(' ');
        if is_job_header {
            jobs.push((line.trim().trim_end_matches(':').to_string(), String::new()));
        } else if let Some((_, body)) = jobs.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }

    jobs.into_iter()
        .filter(|(_, body)| body.contains("cargo "))
        .collect()
}

/// Non-vacuity: both sides of the comparison were actually read.
///
/// A parser that silently returned nothing would make the real assertion below
/// pass for the wrong reason — the empty set is a subset of everything. This
/// is the failure mode that lets a guard go green while guarding nothing.
#[test]
fn both_package_lists_were_actually_parsed() {
    let required = builder_stage_packages();
    assert!(
        required.len() >= 3,
        "read only {} package(s) from the Dockerfile builder stage: {:?} — \
         the parser, not the Dockerfile, is probably what changed",
        required.len(),
        required
    );

    let jobs = cargo_jobs();
    assert!(
        jobs.len() >= 2,
        "found {} job(s) running cargo in .github/workflows/ci.yml, expected \
         at least the lint and test jobs: {:?}",
        jobs.len(),
        jobs.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );

    for (name, body) in &jobs {
        assert!(
            !apt_packages(body).is_empty(),
            "job `{name}` runs cargo but installs no system package at all"
        );
    }
}

/// The guard itself: every package the build image needs is installed by every
/// CI job that compiles.
#[test]
fn every_cargo_job_installs_the_build_image_packages() {
    let required = builder_stage_packages();

    for (name, body) in cargo_jobs() {
        let installed = apt_packages(&body);
        let missing: Vec<&String> = required.difference(&installed).collect();

        assert!(
            missing.is_empty(),
            "CI job `{name}` does not install {missing:?}, which the \
             Dockerfile's builder stage installs to compile this same crate.\n\
             This is the `curl/curl.h: No such file or directory` failure: it \
             is invisible on a development machine and fatal on the runner.\n\
             Add the package(s) to that job's apt-get step in \
             .github/workflows/ci.yml."
        );
    }
}
