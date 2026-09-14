//! Guard: every crate in `[dependencies]` is actually used by `src/`.
//!
//! A dependency that nothing imports is not free. It is compiled, it is
//! resolved, it is locked, and above all it is *shipped*: it joins the graph
//! `cargo audit` walks and the graph an advisory lands in. This service spent
//! a work unit on RUSTSEC-2026-0258 (HR-20260909-001) precisely because of
//! what the graph contains, so carrying crates no line of the service calls is
//! a supply-chain surface bought for nothing.
//!
//! Found by review on 2026-09-13: `actix-cors` was declared as a full
//! dependency and imported nowhere — and the same pass had already removed
//! four other unused crates by hand, missing this one. Reading a manifest
//! against a codebase is exactly the check a human does badly and a test does
//! reliably, so it is written down here instead of being done again next time.
//!
//! Scope is `[dependencies]` only, deliberately. `[dev-dependencies]` holds
//! entries that are unused *on purpose* and say so in the manifest: `reqwest`
//! is kept because it is the sole remaining path by which `h2` enters
//! `Cargo.lock`, which is what the guards in `tests/framework.rs` are written
//! around. A guard that forced their removal would defeat another guard.
//!
//! Only `src/` is scanned, which is also what makes this file safe: a crate
//! named in `tests/` — including in this file's own failure messages — must
//! not count as a use.
//!
//! Needs no database, no network and no cargo subprocess.

use std::path::{Path, PathBuf};

/// The crate names declared in `[dependencies]`, in manifest order.
///
/// Section-aware: `[dev-dependencies]`, `[features]` and `[package]` entries
/// look identical line by line, and only the enclosing header tells them
/// apart.
fn declared_dependencies(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_dependencies = false;

    for line in manifest.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            in_dependencies = trimmed == "[dependencies]";
            continue;
        }
        if !in_dependencies || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        // A dependency entry starts at column 0 with `name = ...`; the
        // continuation lines of a multiline entry are indented or are a lone
        // closing brace.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        if let Some((name, _)) = trimmed.split_once('=') {
            let name = name.trim();
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
    }

    names
}

/// Does `haystack` contain `ident` as a whole Rust identifier?
///
/// Plain substring matching would read `serde` inside `serde_json` and call a
/// dependency used when it is not — the guard has to fail for the right crate
/// or it teaches nothing.
fn mentions_identifier(haystack: &str, ident: &str) -> bool {
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';
    let bytes = haystack.as_bytes();

    haystack.match_indices(ident).any(|(at, _)| {
        let before_ok = at == 0 || !is_ident_char(bytes[at - 1] as char);
        let after = at + ident.len();
        let after_ok = after >= bytes.len() || !is_ident_char(bytes[after] as char);
        before_ok && after_ok
    })
}

/// Every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files
}

fn manifest_and_sources() -> (String, String) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("Cargo.toml must be readable");

    let sources = rust_sources(&root.join("src"))
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .collect::<Vec<_>>()
        .join("\n");

    (manifest, sources)
}

/// Non-vacuity: both the manifest and the sources were actually read.
///
/// Without this, a parser that returned no dependency, or a walk that found no
/// file, would make the guard below pass while checking nothing at all.
#[test]
fn the_manifest_and_the_sources_were_actually_read() {
    let (manifest, sources) = manifest_and_sources();
    let declared = declared_dependencies(&manifest);

    assert!(
        declared.len() >= 5,
        "read only {} dependency name(s) from [dependencies]: {declared:?} — \
         the parser, not the manifest, is probably what changed",
        declared.len()
    );
    assert!(
        declared.iter().any(|name| name == "actix-web"),
        "actix-web must be among the parsed dependencies: {declared:?}"
    );
    assert!(
        sources.len() > 10_000,
        "read only {} bytes of src/, which cannot be this service",
        sources.len()
    );
    // A control on the matcher itself: it must separate these two.
    assert!(mentions_identifier("use serde::Serialize;", "serde"));
    assert!(!mentions_identifier("serde_json::json!({})", "serde"));
}

/// The guard itself.
#[test]
fn every_declared_dependency_is_used_by_the_service() {
    let (manifest, sources) = manifest_and_sources();

    let unused: Vec<String> = declared_dependencies(&manifest)
        .into_iter()
        // The Rust path of a crate replaces dashes with underscores.
        .filter(|name| !mentions_identifier(&sources, &name.replace('-', "_")))
        .collect();

    assert!(
        unused.is_empty(),
        "declared in [dependencies] but named nowhere in src/: {unused:?}.\n\
         An unused dependency is still compiled, locked and shipped, so it is \
         still a surface for advisories. Remove it from Cargo.toml, or use it."
    );
}
