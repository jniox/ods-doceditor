//! Guards for the single manifest decision that keeps `h2` 0.3 out of this
//! service: `actix-web` is taken without its `http2` feature.
//!
//! Advisory: RUSTSEC-2026-0258 (`h2` 0.3.27, unbounded empty DATA frames), for
//! which no fix exists on the 0.3 line. Decision: HR-20260909-001. Rationale
//! and platform-wide procedure: docstore's
//! `docs/security/h2-0.3-removal-runbook.md`.
//!
//! The fix is one feature list. A feature list is undone by accident, which is
//! what these two tests are for. They need no dependency and no database.

/// The whole normal-edge dependency graph, as cargo resolves it.
///
/// Deliberately not `cargo tree -i h2`, which the runbook suggests: this
/// repository can hold two `h2` majors at once (0.4 comes in through
/// `reqwest`, as a dev-dependency), and `-i h2` then refuses to answer with
/// "there are multiple `h2` packages [...] the specification is ambiguous",
/// writing nothing to stdout. A guard that tolerated an empty stdout would go
/// green in exactly the case it exists to catch — h2 0.3 coming back next to
/// h2 0.4. The forward tree has one answer whatever the versions in play, and
/// it is never empty: its first line is this crate.
fn shipped_dependency_graph() -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args(["tree", "--offline", "-e", "normal", "--prefix", "none"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo must be runnable to check the dependency graph");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Proving we read something is half the guard: a command that failed to
    // run must not read as "no offender found".
    assert!(
        stdout
            .lines()
            .next()
            .is_some_and(|first| first.starts_with(env!("CARGO_PKG_NAME"))),
        "could not read the dependency graph, so nothing was checked:\n{stderr}"
    );
    stdout
}

/// The proof that counts: `h2` 0.3 is not in what we ship.
///
/// Asked of cargo rather than of the manifest, so that it holds whatever the
/// way back in: a feature of ours, a satellite that stops saying
/// `default-features = false`, a dependency we do not have yet.
#[test]
fn no_h2_zero_three_in_the_shipped_dependency_graph() {
    let graph = shipped_dependency_graph();

    // `h2` 0.4 is the fixed line of the crate and is allowed to be here; the
    // absence of the package altogether is a valid outcome too. What is read
    // is the absence of v0.3, not the absence of h2.
    let offenders: Vec<&str> = graph
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("h2 v0.3"))
        .collect();
    assert!(
        offenders.is_empty(),
        "h2 0.3 is back in the shipped graph (RUSTSEC-2026-0258). Found:\n{}",
        offenders.join("\n")
    );
}

/// The manifest side of the same fact, so the failure reads plainly.
///
/// Every `actix-web` entry is checked, not just the first: this repository
/// declares the crate twice, and a `[dev-dependencies]` entry left on the
/// default feature set is enough to pin `h2` 0.3 back into `Cargo.lock`, which
/// is the file `cargo audit` reads.
#[test]
fn no_actix_web_entry_asks_for_http2_by_any_of_its_names() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("Cargo.toml must be readable");

    let entries = actix_web_entries(&manifest);
    assert!(
        !entries.is_empty(),
        "the manifest must declare actix-web; if it no longer does, this guard \
         has to be revisited rather than deleted"
    );
    for entry in entries {
        assert!(
            entry.contains("default-features = false"),
            "actix-web's default feature set turns on `http2`, the only thing \
             pulling h2 0.3 into this service:\n{entry}"
        );
        // Every rustls-* and openssl feature of actix-web requires http2 in
        // its own list, so TLS added the obvious way puts the vulnerable frame
        // parser back without a single line naming HTTP/2. TLS is terminated
        // upstream (GCP load balancer + IAP).
        for dangerous in ["\"http2\"", "rustls", "\"openssl\""] {
            assert!(
                !entry.contains(dangerous),
                "the actix-web feature {dangerous} pulls h2 0.3 back in:\n{entry}"
            );
        }
    }
}

/// Every `actix-web = ...` entry of the manifest, braces balanced.
///
/// Stopping at the next line would work for a one-line entry and swallow the
/// rest of the manifest for a multiline one — and `rustls` would end up
/// matching a feature of `sqlx`.
fn actix_web_entries(manifest: &str) -> Vec<String> {
    let depth = |s: &str| s.matches('{').count() as i32 - s.matches('}').count() as i32;
    let mut entries = Vec::new();
    let mut lines = manifest.lines();
    while let Some(first) = lines
        .by_ref()
        .find(|l| l.trim_start().starts_with("actix-web"))
    {
        let mut entry = String::from(first);
        let mut open = depth(first);
        // Pull a line only while braces are still open: testing after the pull
        // would swallow the line that follows a single-line entry, and with it
        // a second declaration sitting right underneath.
        while open > 0 {
            let Some(line) = lines.next() else { break };
            open += depth(line);
            entry.push('\n');
            entry.push_str(line);
        }
        entries.push(entry);
    }
    entries
}
