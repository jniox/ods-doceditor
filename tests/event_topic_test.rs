//! The name this service publishes under, which had four spellings and one
//! resource.
//!
//! Publishing to the wrong topic is **silent**: the producer returns `Ok`, the
//! callers check it, the tests observe that `publish` was called, and nothing
//! anywhere fails. This service already spent four months publishing into a
//! `NoopProducer` for exactly that reason (ADR-002), so a name that four sources
//! spelled four ways was not a tidiness problem.
//!
//! Measured by the spec-writer on 2026-09-14 at 08:28 UTC against
//! `orbus-ods-staging`, among 25 provisioned topics:
//!
//! ```text
//! editor-events (+ editor-events-dlq)  EXISTS — provisioned for this service
//! editor.events                        does not exist — src/config.rs's default
//! ods.editor.events                    does not exist — GTM brief, 4 times
//! doceditor-events                     does not exist — the {service}-events rule
//! ```
//!
//! `~/dev/specs/ods-platform/specs/doceditor/spec.md` §4.2 settles it —
//! **`editor-events`** — in execution of HR-20260913-001 (option A,
//! it@orbusdigital.com, 2026-09-13), and names this repository's default as one
//! of the sources to rewrite. `editor` is the *domain* (the schema is `editor`,
//! the event source is `/editor`, the types are `com.ods.editor.*`);
//! `doceditor` is the name of the deployment. Consumers read the domain.
//!
//! This file exists because that name has no other guard: nothing fails when it
//! is wrong, which is precisely why it had to be written down.

use ods_doceditor::config::DEFAULT_EVENT_TOPIC;

const ENV_TEMPLATE: &str = include_str!("../.env.example");

/// The canonical name, spelled once in the code.
#[test]
fn the_default_topic_is_the_one_the_specification_settled() {
    assert_eq!(
        DEFAULT_EVENT_TOPIC, "editor-events",
        "spec.md §4.2 (executing HR-20260913-001) settles the canonical topic as `editor-events`, \
         the only one of the four candidate spellings that exists as a provisioned resource"
    );
}

/// And the template a deployer copies spells it the same way.
///
/// AC-029 made the deployment set `REDPANDA_TOPIC` explicitly *for as long as*
/// the default disagreed with the canonical name. Once the default is right,
/// that obligation lapses — but only if the file a human copies from is right
/// too, since that is where the value is actually typed.
#[test]
fn the_environment_template_names_the_canonical_topic() {
    let expected = format!("REDPANDA_TOPIC={DEFAULT_EVENT_TOPIC}");
    assert!(
        ENV_TEMPLATE.contains(&expected),
        ".env.example must set {expected}"
    );
}

/// The dead-letter queue is named after it, and the template says so.
///
/// `editor-events-dlq` is provisioned beside the topic. This service does not
/// publish to it — nothing in the code names it — so the only place a reader can
/// learn it exists is the documentation, which makes the documentation the
/// thing to guard.
#[test]
fn the_dead_letter_queue_is_documented_beside_it() {
    assert!(
        ENV_TEMPLATE.contains(&format!("{DEFAULT_EVENT_TOPIC}-dlq")),
        ".env.example must name the dead-letter queue provisioned beside the topic"
    );
}

/// The stale spellings are gone from the template.
///
/// Not a style rule: `editor.events` was the *default*, so a file still showing
/// it is a file someone will paste into a deployment.
#[test]
fn no_stale_spelling_survives_in_the_template() {
    for stale in ["editor.events", "ods.editor.events", "doceditor-events"] {
        assert!(
            !ENV_TEMPLATE.contains(&format!("REDPANDA_TOPIC={stale}")),
            ".env.example still sets the superseded topic name {stale}; spec.md §4.2"
        );
    }
}
