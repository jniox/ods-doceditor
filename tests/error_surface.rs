//! Guards on the error surface: what the service can return, and what it says
//! it can return.
//!
//! Two findings on 2026-09-13 sit on either side of the same seam.
//!
//! `AppError::Forbidden` and `AppError::Conflict` were declared, given a full
//! `ResponseError` arm and a status code each, and constructed nowhere -- a
//! 403 and a 409 this service has never once been able to emit. Dead variants
//! of an error enum are worse than ordinary dead code: they are *read* as an
//! API capability, by the client author and by the reviewer, and they are the
//! first thing a published contract copies.
//!
//! Which is exactly what had happened: `docs/openapi.yaml` listed `conflict`
//! and `forbidden` among the values of `Error.error`, so the contract DocSign
//! writes its client against promised two codes no branch could produce.
//!
//! So the two directions are checked here together: a variant nothing
//! constructs must go, and the contract's list of error codes must be exactly
//! the list `error.rs` can actually put on the wire. Neither drifts quietly
//! after that.
//!
//! Needs no database, no network and no cargo subprocess.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ERROR_RS: &str = include_str!("../src/error.rs");
const OPENAPI: &str = include_str!("../docs/openapi.yaml");

/// The variants of `AppError`, read from its declaration.
fn declared_variants() -> Vec<String> {
    let body = ERROR_RS
        .split_once("pub enum AppError {")
        .expect("src/error.rs must declare `pub enum AppError`")
        .1
        .split_once("\n}")
        .expect("the AppError declaration must be closed")
        .0;

    body.lines()
        .map(str::trim)
        // Skip doc comments, attributes such as #[error("...")], and blanks.
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("//"))
        .filter_map(|line| {
            let name = line
                .split(['(', '{', ','])
                .next()
                .map(str::trim)
                .unwrap_or_default();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Every `.rs` file under `src/`, except `error.rs` itself.
///
/// The exclusion is what gives the guard its meaning: `error.rs` names every
/// variant in its own `match` and in its `From<sqlx::Error>`, so scanning it
/// would make each variant look constructed by virtue of being declared.
fn service_sources_excluding_error_rs() -> String {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);

    files
        .into_iter()
        .filter(|path| !path.ends_with("error.rs"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The error codes `error_response` can put in a response body, read from the
/// `"error": "..."` literals it builds.
fn emitted_error_codes() -> BTreeSet<String> {
    ERROR_RS
        .match_indices("\"error\": \"")
        .filter_map(|(at, marker)| {
            let rest = &ERROR_RS[at + marker.len()..];
            rest.split_once('"').map(|(code, _)| code.to_string())
        })
        .collect()
}

/// The values of `Error.error` in the published contract.
fn documented_error_codes() -> BTreeSet<String> {
    let schema = OPENAPI
        .split_once("\n    Error:\n")
        .expect("docs/openapi.yaml must declare an `Error` schema")
        .1;
    let list = schema
        .split_once("enum:\n")
        .expect("the Error schema must list its error codes as an enum")
        .1;

    list.lines()
        .map(str::trim)
        .take_while(|line| line.starts_with("- "))
        .map(|line| line.trim_start_matches("- ").to_string())
        .collect()
}

/// Non-vacuity: all three sources were actually read.
///
/// Empty sets make every assertion below true for the wrong reason.
#[test]
fn the_error_surface_was_actually_read() {
    let variants = declared_variants();
    assert!(
        variants.len() >= 3,
        "read only {} variant(s) of AppError: {variants:?} — the parser, not \
         src/error.rs, is probably what changed",
        variants.len()
    );
    assert!(
        variants.iter().any(|v| v == "NotFound"),
        "NotFound must be among the parsed variants: {variants:?}"
    );

    let emitted = emitted_error_codes();
    assert_eq!(
        emitted.len(),
        variants.len(),
        "every AppError variant should map to exactly one error code, but \
         {} variant(s) produced {} code(s): {variants:?} vs {emitted:?}",
        variants.len(),
        emitted.len()
    );

    let documented = documented_error_codes();
    assert!(
        documented.len() >= 3,
        "read only {} error code(s) from the contract: {documented:?}",
        documented.len()
    );

    assert!(
        service_sources_excluding_error_rs().contains("AppError::"),
        "no source outside src/error.rs mentions AppError, which cannot be \
         this service"
    );
}

/// An error variant the service can never construct is not an API capability.
#[test]
fn every_error_variant_is_constructed_somewhere() {
    let sources = service_sources_excluding_error_rs();

    let unreachable: Vec<String> = declared_variants()
        .into_iter()
        .filter(|variant| !sources.contains(&format!("AppError::{variant}")))
        .collect();

    assert!(
        unreachable.is_empty(),
        "declared on AppError but constructed nowhere in src/: {unreachable:?}.\n\
         A variant no branch can produce is still read as a status code this \
         API returns -- by a client author, and by the contract. Remove it, or \
         use it where it belongs."
    );
}

/// The contract promises exactly the error codes the code can produce.
#[test]
fn the_contract_lists_exactly_the_emitted_error_codes() {
    let emitted = emitted_error_codes();
    let documented = documented_error_codes();

    let undocumented: Vec<&String> = emitted.difference(&documented).collect();
    assert!(
        undocumented.is_empty(),
        "the service can return {undocumented:?}, which docs/openapi.yaml does \
         not list among the values of Error.error"
    );

    let phantom: Vec<&String> = documented.difference(&emitted).collect();
    assert!(
        phantom.is_empty(),
        "docs/openapi.yaml promises the error code(s) {phantom:?}, which no \
         branch of src/error.rs can emit"
    );
}
