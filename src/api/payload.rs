//! The two ceilings a document has to pass, and why they are not one number.
//!
//! `MAX_DOCUMENT_SIZE_MB` names a **document**. The HTTP payload that carries
//! one is always bigger: JSON wraps the body in quotes, escapes some of its
//! characters, and adds the `title` and the `metadata` beside it. Setting both
//! ceilings to the same number therefore makes the documented maximum
//! unreachable — a body of exactly `MAX_DOCUMENT_SIZE_MB` produces a payload
//! larger than `MAX_DOCUMENT_SIZE_MB` and is refused by the framework before
//! the service ever sees it. Measured on the running binary at
//! `MAX_DOCUMENT_SIZE_MB=1`, before this module existed:
//!
//! ```text
//! content = ceiling - 64          wire = 1048551 -> 201
//! content = ceiling               wire = 1048615 -> 413 text/plain
//! content = ceiling + 1           wire = 1048616 -> 413 text/plain
//! content = ceiling/2, all '"'    wire = 1048615 -> 413 text/plain
//! ```
//!
//! The last line is the one that names the mistake: the ceiling was being
//! applied to the **encoding**, so the largest document a client could store
//! depended on which characters were in it — a limit no caller can compute and
//! none was ever told.
//!
//! Two ceilings, then, one derived from the other:
//!
//! * the **body** ceiling is `MAX_DOCUMENT_SIZE_MB` — what a product may store,
//!   and what [`DocumentService`](crate::service::document_service) checks with
//!   a `422` that names the field to shrink;
//! * the **payload** ceiling is what it takes to carry such a body, and it is
//!   the last line of defence rather than the rule: reaching it means the
//!   request was too long to read at all, which is a `413`.
//!
//! Both are installed by [`limits`], which `main.rs` and the tests call. That
//! is deliberate. The ceiling was previously tested by an `App` carrying no
//! `JsonConfig` whatsoever, against a service built with
//! `with_max_content_bytes(64)` — a bench from which the boundary that refuses
//! first is invisible, which is exactly how a limit can be green in the suite
//! and wrong in production.

use actix_web::error::JsonPayloadError;
use actix_web::web;

use crate::error::AppError;

/// Room for everything in the envelope that is not the body.
///
/// Not a round number chosen for comfort: it has to cover the largest envelope
/// this service's own validation admits — a `title`, the `metadata`, a
/// `template_id`, and the field names and punctuation around them.
///
/// "Admits" is the load-bearing word, and it was false here until 2026-09-14.
/// The sentence used to reason from the *prose* rules ("twenty metadata keys of
/// 64 bytes holding 256-byte values … under 7 KiB") while the validation
/// applied the 256-character rule at depth 1 only and bounded the object's size
/// not at all — so what this allowance actually admitted was 20 MiB of
/// metadata, three hundred times its own value, and the test below asserted the
/// arithmetic of the prose against itself. It now reads the constants the
/// domain enforces. See [`crate::domain::metadata`].
pub const ENVELOPE_ALLOWANCE_BYTES: usize = 64 * 1024;

/// The largest HTTP payload that can carry a document of `body_ceiling` bytes.
///
/// The factor of two is the worst case of JSON string escaping for text:
/// `"` becomes `\"` and `\` becomes `\\`, and nothing else in a document body
/// grows — non-ASCII is emitted verbatim by `serde_json`, so an accented or CJK
/// body does not expand at all. Control characters (`U+0000`–`U+001F`) do
/// expand sixfold as `\u00XX`, and a body made mostly of those will still meet
/// the payload ceiling; that is the intended answer, because such a request
/// really is too large to read, and it now says so in this service's own error
/// shape rather than in `text/plain`.
///
/// Saturating: `body_ceiling` comes from `MAX_DOCUMENT_SIZE_MB`, an operator
/// value, and a service must not depend on an operator not typing a large one.
pub fn payload_ceiling(body_ceiling: usize) -> usize {
    body_ceiling
        .saturating_mul(2)
        .saturating_add(ENVELOPE_ALLOWANCE_BYTES)
}

/// Translate the boundary's own failures into this service's error shape.
///
/// Without this, the only refusals a client cannot parse are the ones that come
/// from the framework: `413` and `400` arrived as `text/plain` while every
/// other error of this API is `{"error": ..., "message": ...}`, which is what
/// `docs/openapi.yaml` publishes and what `tests/error_surface.rs` guards.
fn as_app_error(err: JsonPayloadError, body_ceiling: usize) -> AppError {
    let ceiling = payload_ceiling(body_ceiling);
    match err {
        JsonPayloadError::OverflowKnownLength { length, limit: _ } => {
            AppError::PayloadTooLarge(format!(
                "Request body is {length} bytes; at most {ceiling} can be read, which is what \
                 it takes to carry a document of {body_ceiling} bytes"
            ))
        }
        JsonPayloadError::Overflow { limit: _ } => AppError::PayloadTooLarge(format!(
            "Request body exceeds {ceiling} bytes, which is what it takes to carry a document \
             of {body_ceiling} bytes"
        )),
        JsonPayloadError::ContentType => {
            AppError::BadRequest("Content-Type must be application/json".to_string())
        }
        JsonPayloadError::Deserialize(e) => AppError::BadRequest(format!("Malformed JSON: {e}")),
        // `JsonPayloadError` is `#[non_exhaustive]`; anything else is still a
        // request this service could not read, which is the caller's to fix.
        other => AppError::BadRequest(format!("Unreadable request body: {other}")),
    }
}

/// The JSON extractor, bounded by the payload ceiling and answering in the
/// service's error shape.
pub fn json_config(body_ceiling: usize) -> web::JsonConfig {
    web::JsonConfig::default()
        .limit(payload_ceiling(body_ceiling))
        .error_handler(move |err, _req| as_app_error(err, body_ceiling).into())
}

/// The raw-payload extractor, bounded by the same ceiling.
pub fn payload_config(body_ceiling: usize) -> web::PayloadConfig {
    web::PayloadConfig::default().limit(payload_ceiling(body_ceiling))
}

/// Install both ceilings on an application.
///
/// One call, used by `main.rs` and by the tests, so the bench cannot be wired
/// differently from production — which is the defect this module exists to
/// close, not merely a tidiness.
pub fn limits(body_ceiling: usize) -> impl Fn(&mut web::ServiceConfig) + Clone {
    move |cfg: &mut web::ServiceConfig| {
        cfg.app_data(json_config(body_ceiling));
        cfg.app_data(payload_config(body_ceiling));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::metadata::MAX_METADATA_BYTES;
    use crate::domain::text::MAX_TITLE_CHARS;

    /// The allowance is not a magic number: it must cover the rest of the
    /// envelope this service already bounds.
    ///
    /// Every term below is read from the constant that *enforces* it, so the
    /// day someone raises the metadata bound past what the payload ceiling
    /// reserves, this goes red instead of the instance going down. The previous
    /// version of this test restated the prose of the rules and was green while
    /// the service admitted three hundred times the number it asserted.
    #[test]
    fn the_envelope_allowance_covers_the_fields_the_service_bounds() {
        // A title is bounded in characters, and a character is at most 4 bytes
        // of UTF-8 — or 6 on the wire, since a control character is escaped as
        // `\u00XX`. Metadata is bounded in serialised bytes, which is already
        // what it costs here. Plus a uuid `template_id`, the field names and
        // the braces around them.
        let largest_envelope = MAX_TITLE_CHARS * 6 + MAX_METADATA_BYTES + 36 + 200;
        assert!(
            ENVELOPE_ALLOWANCE_BYTES > largest_envelope,
            "the allowance ({ENVELOPE_ALLOWANCE_BYTES}) must exceed the largest envelope the \
             validation rules admit ({largest_envelope})"
        );
    }

    /// A body at the ceiling must fit, whatever escaping does to it — that is
    /// the promise `MAX_DOCUMENT_SIZE_MB` makes by being named after documents.
    #[test]
    fn the_payload_ceiling_admits_the_worst_case_encoding_of_a_full_body() {
        let body = 10 * 1024 * 1024;
        let worst_case_wire = body * 2 + 1024; // every byte escaped, plus a title
        assert!(payload_ceiling(body) >= worst_case_wire);
    }

    /// And it never wraps, whatever an operator puts in the environment.
    #[test]
    fn the_payload_ceiling_saturates_instead_of_wrapping() {
        assert_eq!(payload_ceiling(usize::MAX), usize::MAX);
        assert!(payload_ceiling(usize::MAX / 2) >= usize::MAX / 2);
    }
}
