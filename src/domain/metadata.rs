//! `metadata`: the bounds the contract states, applied to every shape a caller
//! can send.
//!
//! `docs/openapi.yaml` says: *at most 20 keys; keys match
//! `^[a-z][a-z0-9_]{0,63}$`; string values are at most 256 characters*. Until
//! 2026-09-14 the service applied the last rule to the values of the object and
//! to nothing else, so the container decided whether the rule existed —
//! measured on the running binary, `{"resume": 257 × 'a'}` was a `422` and
//! `{"resume": {"inner": 5 000 000 × 'a'}}` was a `201`.
//!
//! And no rule bounded the object as a whole. The only remaining ceiling was
//! the payload one, `2 × MAX_DOCUMENT_SIZE_MB + 64 KiB`, whose 64 KiB term is
//! documented in [`crate::api::payload`] as covering "the largest envelope this
//! service's own validation admits … under 7 KiB". The validation admitted
//! 20 MiB. That premise is what [`MAX_METADATA_BYTES`] restores; it is not a
//! new product limit, it is the number the payload ceiling was already computed
//! from.
//!
//! Why it mattered more than a refused request: [`DocumentSummary`] drops
//! `content` on purpose (ADR-007) and keeps `metadata`, so a page multiplies
//! this field by up to a hundred. Thirty documents of 10 MB of metadata — thirty
//! ordinary `201`s — answered `GET /api/v1/documents?per_page=30` with
//! 300 010 939 bytes and a peak RSS of 945 MiB, against the 512 MiB the staging
//! service is allocated: an OOM kill of the instance, which takes every other
//! tenant's in-flight request with it.
//!
//! [`DocumentSummary`]: crate::domain::document::DocumentSummary

use serde_json::Value;

use crate::domain::text::MAX_METADATA_VALUE_CHARS;
use crate::error::{AppError, AppResult};

/// BR-029: at most twenty keys.
pub const MAX_METADATA_KEYS: usize = 20;

/// The largest serialised metadata object this service accepts.
///
/// Derived, not chosen: [`crate::api::payload::ENVELOPE_ALLOWANCE_BYTES`] is
/// 64 KiB and must cover the whole envelope around a body — the title, this
/// field, a `template_id` and the punctuation between them. Half of it leaves
/// the title its own worst case (500 characters of anything) with an order of
/// magnitude to spare, and still admits five times the ~6.5 KiB the per-key
/// rules describe (20 keys × (64-byte key + 256-character value)).
///
/// `payload.rs` asserts that relationship against *this* constant, so the two
/// cannot drift apart again.
pub const MAX_METADATA_BYTES: usize = 32 * 1024;

/// A validated metadata object.
///
/// Built only by parsing, like [`crate::domain::text::Title`] and
/// [`crate::domain::pagination::Pagination`]: the repository takes this type
/// rather than a `serde_json::Value`, so there is no longer a path that writes
/// an unchecked object into the `jsonb` column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata(Value);

impl Metadata {
    /// The empty object, for a request that sent no metadata at all.
    pub fn empty() -> Self {
        Self(Value::Object(serde_json::Map::new()))
    }

    /// Validate `raw`, then keep a copy of it.
    ///
    /// It takes a reference and clones on success on purpose: the clone then
    /// happens *after* the size bound is known, so a 20 MiB object is refused
    /// without ever being duplicated. Both handlers used to `clone()` first.
    pub fn parse(raw: &Value) -> AppResult<Self> {
        let Some(object) = raw.as_object() else {
            // Every rule below is stated about keys, so a string, a number, a
            // boolean or an array would satisfy all of them vacuously — and the
            // `jsonb` column accepts them happily. A guard that says nothing
            // about the inputs it was not shaped for is not a guard.
            return Err(AppError::Validation(
                "Metadata must be a JSON object".to_string(),
            ));
        };

        // Size first: it is the bound that holds whatever shape the value
        // takes, and refusing here means every walk below runs over at most
        // `MAX_METADATA_BYTES` of input.
        if serialised_size_within(raw, MAX_METADATA_BYTES).is_none() {
            return Err(AppError::Validation(format!(
                "Metadata must serialise to at most {MAX_METADATA_BYTES} bytes"
            )));
        }

        if object.len() > MAX_METADATA_KEYS {
            return Err(AppError::Validation(format!(
                "Metadata cannot have more than {MAX_METADATA_KEYS} keys"
            )));
        }

        let key_re = regex_lite::Regex::new(r"^[a-z][a-z0-9_]{0,63}$").expect("static regex");
        for (key, value) in object {
            if !key_re.is_match(key) {
                return Err(AppError::Validation(format!(
                    "Metadata key '{key}' must match ^[a-z][a-z0-9_]{{0,63}}$"
                )));
            }
            if let Some(length) = longest_string_over(value, MAX_METADATA_VALUE_CHARS) {
                return Err(AppError::Validation(format!(
                    "Metadata value for key '{key}' must be at most \
                     {MAX_METADATA_VALUE_CHARS} characters (got {length})"
                )));
            }
        }

        Ok(Self(raw.clone()))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn into_value(self) -> Value {
        self.0
    }
}

impl Default for Metadata {
    fn default() -> Self {
        Self::empty()
    }
}

/// The length of the first string in `value` — at any depth — longer than
/// `limit` characters, or `None`.
///
/// Characters and not bytes, because that is what the contract counts and what
/// [`crate::domain::text`] already fixed for the title: `str::len()` would make
/// the documented maximum depend on the alphabet.
///
/// Iterative rather than recursive: `serde_json` bounds parsing depth, but a
/// domain type should not owe its stack safety to whoever built the value.
fn longest_string_over(value: &Value, limit: usize) -> Option<usize> {
    let mut stack = vec![value];
    while let Some(node) = stack.pop() {
        match node {
            Value::String(s) => {
                let length = s.chars().count();
                if length > limit {
                    return Some(length);
                }
            }
            Value::Array(items) => stack.extend(items.iter()),
            Value::Object(entries) => stack.extend(entries.values()),
            _ => {}
        }
    }
    None
}

/// The exact serialised size of `value`, or `None` once it passes `limit`.
///
/// It counts into a sink rather than into a `Vec` because the quantity in
/// question *is* a memory bound: `serde_json::to_vec(value).len()` would
/// allocate a second copy of the very object suspected of being too large, and
/// would have to serialise all 20 MiB of it to discover that it is.
fn serialised_size_within(value: &Value, limit: usize) -> Option<usize> {
    let mut budget = SizeBudget { used: 0, limit };
    match serde_json::to_writer(&mut budget, value) {
        Ok(()) => Some(budget.used),
        Err(_) => None,
    }
}

struct SizeBudget {
    used: usize,
    limit: usize,
}

impl std::io::Write for SizeBudget {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.used = self.used.saturating_add(buf.len());
        if self.used > self.limit {
            // The one way to stop `to_writer` early. The error is never shown:
            // the caller reads it as "over budget" and says so in the field's
            // own words.
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "metadata over budget",
            ));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_string_over_the_bound_is_refused_at_every_depth() {
        let long = "a".repeat(MAX_METADATA_VALUE_CHARS + 1);
        for shape in [
            json!({ "resume": long }),
            json!({ "resume": { "inner": long } }),
            json!({ "tags": [long] }),
            json!({ "tags": [{ "label": [long] }] }),
        ] {
            assert!(
                Metadata::parse(&shape).is_err(),
                "accepted {} characters in {shape}",
                MAX_METADATA_VALUE_CHARS + 1
            );
        }
    }

    #[test]
    fn a_string_at_the_bound_is_accepted_at_every_depth() {
        let exact = "é".repeat(MAX_METADATA_VALUE_CHARS);
        for shape in [
            json!({ "resume": exact }),
            json!({ "resume": { "inner": exact } }),
            json!({ "tags": [exact] }),
        ] {
            assert!(
                Metadata::parse(&shape).is_ok(),
                "refused {MAX_METADATA_VALUE_CHARS} accented characters in {shape} — the bound is \
                 counted in characters, and 'é' is two bytes"
            );
        }
    }

    #[test]
    fn the_size_bound_sees_what_the_character_bound_cannot() {
        // Every string here is within the character bound; the object is not.
        let chunk = "a".repeat(MAX_METADATA_VALUE_CHARS);
        let elements = MAX_METADATA_BYTES / MAX_METADATA_VALUE_CHARS + 10;
        let over = json!({ "tags": vec![chunk.clone(); elements] });
        assert!(Metadata::parse(&over).is_err());

        let under = json!({ "tags": vec![chunk; 10] });
        assert!(Metadata::parse(&under).is_ok());
    }

    #[test]
    fn the_size_is_measured_without_copying_the_value() {
        // 4 MiB of metadata must be refused after counting 32 KiB, not after
        // serialising four megabytes into a second buffer.
        let big = json!({ "blob": { "inner": "a".repeat(4 * 1024 * 1024) } });
        assert_eq!(serialised_size_within(&big, MAX_METADATA_BYTES), None);

        let small = json!({ "kind": "contract" });
        let size = serialised_size_within(&small, MAX_METADATA_BYTES).expect("within budget");
        assert_eq!(size, serde_json::to_vec(&small).unwrap().len());
    }

    #[test]
    fn the_shape_rules_still_hold() {
        assert!(Metadata::parse(&json!("a string")).is_err());
        assert!(Metadata::parse(&json!([1, 2, 3])).is_err());
        assert!(Metadata::parse(&json!(42)).is_err());
        assert!(Metadata::parse(&json!({ "Kind": "contract" })).is_err());
        assert!(Metadata::parse(&json!({ "kind": "contract" })).is_ok());

        let too_many: serde_json::Map<String, Value> = (0..=MAX_METADATA_KEYS)
            .map(|i| (format!("k{i}"), json!(i)))
            .collect();
        assert!(Metadata::parse(&Value::Object(too_many)).is_err());
    }

    #[test]
    fn the_empty_object_is_what_a_request_without_metadata_stores() {
        assert_eq!(Metadata::empty().as_value(), &json!({}));
        assert!(Metadata::parse(&json!({})).is_ok());
    }
}
