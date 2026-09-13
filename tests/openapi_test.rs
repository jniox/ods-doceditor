//! AC-015 — a published API contract, and a guard that keeps it honest.
//!
//! The contract is the artefact DocSign will write its client against (GTM:
//! "required before Phase 2"). A contract nothing checks drifts from the code
//! within one batch, so this file does not merely assert that the file exists:
//! it compares the documented surface with the routes `main.rs` actually
//! registers, in both directions.

const OPENAPI: &str = include_str!("../docs/openapi.yaml");
const MAIN_RS: &str = include_str!("../src/main.rs");

/// Every route the service serves, with its method and its documented path.
/// Adding a route without adding it here fails `no_undocumented_route_exists`.
const SURFACE: &[(&str, &str)] = &[
    ("get", "/health"),
    ("get", "/ready"),
    ("post", "/api/v1/documents"),
    ("get", "/api/v1/documents"),
    ("get", "/api/v1/documents/{id}"),
    ("patch", "/api/v1/documents/{id}"),
    ("delete", "/api/v1/documents/{id}"),
    ("post", "/api/v1/documents/{id}/versions"),
    ("get", "/api/v1/documents/{id}/versions"),
    ("get", "/api/v1/documents/{doc_id}/versions/{version}"),
];

/// The document declares OpenAPI 3.1, as BR-0007 requires of the format.
#[test]
fn the_contract_is_openapi_3_1() {
    assert!(
        OPENAPI.starts_with("openapi: 3.1"),
        "the contract must declare OpenAPI 3.1"
    );
}

/// Every path of the surface appears in the contract, with every one of its
/// methods.
#[test]
fn every_route_is_documented() {
    let paths_section = OPENAPI
        .split_once("\npaths:\n")
        .expect("the contract must have a paths section")
        .1
        .split_once("\ncomponents:\n")
        .expect("the contract must have a components section")
        .0;

    for (method, path) in SURFACE {
        let block = path_block(paths_section, path)
            .unwrap_or_else(|| panic!("{path} is served but absent from docs/openapi.yaml"));
        assert!(
            block.contains(&format!("\n    {method}:")),
            "{method} {path} is served but not documented"
        );
    }
}

/// And the converse: the contract documents nothing the service does not serve.
#[test]
fn the_contract_documents_no_phantom_route() {
    let paths_section = OPENAPI
        .split_once("\npaths:\n")
        .unwrap()
        .1
        .split_once("\ncomponents:\n")
        .unwrap()
        .0;

    let documented: Vec<&str> = paths_section
        .lines()
        .filter(|l| l.starts_with("  /"))
        .map(|l| l.trim().trim_end_matches(':'))
        .collect();

    for path in &documented {
        assert!(
            SURFACE.iter().any(|(_, p)| p == path),
            "{path} is documented but no route serves it"
        );
    }

    let served: std::collections::BTreeSet<&str> = SURFACE.iter().map(|(_, p)| *p).collect();
    assert_eq!(
        documented.len(),
        served.len(),
        "documented paths {documented:?} do not match served paths {served:?}"
    );
}

/// The guard that actually bites: a route added to `main.rs` without being
/// added to `SURFACE` — and therefore without being documented — fails here.
#[test]
fn no_undocumented_route_exists() {
    let registered = MAIN_RS.matches(".route(").count();
    assert_eq!(
        registered,
        SURFACE.len(),
        "main.rs registers {registered} routes but the documented surface has \
         {}. Add the new route to docs/openapi.yaml and to SURFACE.",
        SURFACE.len()
    );
}

/// The contract must describe the authentication it actually enforces, and say
/// which probes are exempt — the two mistakes a client integrator makes first.
#[test]
fn the_contract_states_the_authentication_rules() {
    assert!(OPENAPI.contains("bearerAuth"));
    assert!(OPENAPI.contains("bearerFormat: JWT"));

    for probe in ["/health", "/ready"] {
        let block = path_block(OPENAPI, probe).unwrap();
        assert!(
            block.contains("security: []"),
            "{probe} must be documented as unauthenticated"
        );
    }
}

/// Read the YAML block belonging to one top-level path entry.
///
/// The first entry of a section has no newline before it, hence the two cases.
fn path_block<'a>(haystack: &'a str, path: &str) -> Option<&'a str> {
    let head = format!("  {path}:\n");
    let start = if haystack.starts_with(&head) {
        0
    } else {
        haystack.find(&format!("\n{head}"))? + 1
    };
    let rest = &haystack[start..];
    let end = rest[1..].find("\n  /").map(|i| i + 1).unwrap_or(rest.len());
    Some(&rest[..end])
}
