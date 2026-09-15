//! The bounded text a caller types: parsed once, counted in characters.
//!
//! Three fields of this service are bounded by `docs/openapi.yaml`, and all
//! three are bounded in **characters** — `maxLength` counts characters, and so
//! does the `VARCHAR(500)` that migrations 002 and 003 declare for `title` and
//! `comment`. Rust's `str::len()` counts **bytes**, and using it here made the
//! documented maximum depend on the alphabet: measured on the running binary,
//! the largest storable title was 500 ASCII characters, 250 accented ones or
//! 166 Chinese ones — a number no caller can compute, for a product whose own
//! examples read `Contrat de prestation`.
//!
//! The other half of the defect was worse and is the reason these are *types*
//! rather than a pair of `fn validate_*`. `update_document` validated
//! `title.trim()` and then handed the **untrimmed** string to the repository,
//! so renaming a document to a 500-character title with a leading space stored
//! 501 characters into `VARCHAR(500)`: `22001 value too long`, surfaced to the
//! caller as `500 {"error":"internal_error"}` on a request that was perfectly
//! legal. Creating trimmed and renaming untrimmed also stored the same title
//! two different ways.
//!
//! A value that is validated and a value that is stored can only diverge while
//! they are two different values. [`Title`] and [`Comment`] can be built only
//! by parsing, they carry the normalised form, and `document_repo` and
//! `version_repo` take them instead of `&str` — so there is nowhere left to
//! check one string and write another. This is the same shape as
//! [`crate::domain::pagination::Pagination`], which answers for the page that
//! was served rather than the one that was asked for.

use crate::error::{AppError, AppResult};

/// Where the first `U+0000` sits in `value`, as a **character** index, or
/// `None` when there is none.
///
/// # Why this rule exists, and why it is named once
///
/// `U+0000` is an ordinary character in JSON (`"\u0000"`) and in a query string
/// (`%00`), and it is the one character PostgreSQL will not store: `text`
/// answers `22021 invalid byte sequence for encoding "UTF8": 0x00` and `jsonb`
/// answers `22P05 unsupported Unicode escape sequence`. Nothing here looked for
/// it until 2026-09-15, so five fields carried it to the database and the
/// **database** wrote the reply — measured on the running binary, `title`,
/// `content`, a metadata value and `?search=` each answered
/// `500 {"error":"internal_error"}`, with an ERROR log line behind it.
///
/// That is the wrong answer three times over: it says *our fault, try again*
/// about a request that can never succeed, it names no field, and it pages
/// somebody for an input a caller chose. The contract already has the right
/// answers — `422` for a body field, `400` for a query parameter — and this is
/// the same shape as the byte/character bound above it: a constraint that lives
/// in the **column** rather than in the code, refusing what the boundary
/// accepted.
///
/// It is one function rather than five `if s.contains('\0')` for the reason
/// `ensure_live_document` is one function: a rule copied to the places somebody
/// thought about is a rule missing from the place nobody did.
///
/// The fast path is `find`, which is a `memchr` byte scan with no allocation;
/// the character index costs a second walk and is only paid on the way to a
/// refusal. Measured on the release build the Dockerfile produces, which is the
/// only build where the number means anything:
///
/// ```text
///  2 MiB body (the ceiling), no NUL   0.63 ms      NUL at the last byte  0.79 ms
/// 10 MiB body (a stored legacy one)   2.28 ms      NUL at the last byte  3.68 ms
/// ```
///
/// Once per write, against a save that already costs tens of milliseconds of
/// WAL and index work (ADR-014).
pub fn nul_at(value: &str) -> Option<usize> {
    let byte = value.find(NUL)?;
    Some(value[..byte].chars().count())
}

/// The character this service cannot store, written once.
pub const NUL: char = '\u{0}';

/// The refusal, worded once so the five fields answer alike.
///
/// It names the field and the position and stops there: *why* it cannot be
/// stored is PostgreSQL's business and belongs in this file, not on the wire.
pub fn nul_refusal(what: &str, at: usize) -> String {
    format!("{what} must not contain the NUL character U+0000 (at character {at})")
}

/// `CreateDocumentRequest.title` / `UpdateDocumentRequest.title`:
/// `minLength: 1, maxLength: 500`, "Non-blank once trimmed".
pub const MAX_TITLE_CHARS: usize = 500;

/// `CreateVersionRequest.comment`: `maxLength: 500`.
pub const MAX_COMMENT_CHARS: usize = 500;

/// `Metadata`: "string values are at most 256 characters".
pub const MAX_METADATA_VALUE_CHARS: usize = 256;

/// A document title: trimmed, non-blank, at most [`MAX_TITLE_CHARS`]
/// characters — whatever those characters cost in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title(String);

impl Title {
    /// Trim, then bound. The trimmed form is what the caller gets back and
    /// what the column receives; the raw form does not survive this call.
    pub fn parse(raw: &str) -> AppResult<Self> {
        let trimmed = raw.trim();
        let length = trimmed.chars().count();

        if length == 0 {
            return Err(AppError::Validation(
                "Title must not be blank once trimmed".to_string(),
            ));
        }
        if length > MAX_TITLE_CHARS {
            return Err(AppError::Validation(format!(
                "Title must be at most {MAX_TITLE_CHARS} characters once trimmed (got {length})"
            )));
        }
        if let Some(at) = nul_at(trimmed) {
            return Err(AppError::Validation(nul_refusal("Title", at)));
        }

        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Title {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a snapshot was taken: at most [`MAX_COMMENT_CHARS`] characters.
///
/// Not trimmed, unlike a title: the contract states a maximum and nothing
/// about normalisation, and a comment is prose rather than an identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment(String);

impl Comment {
    pub fn parse(raw: &str) -> AppResult<Self> {
        let length = raw.chars().count();
        if length > MAX_COMMENT_CHARS {
            return Err(AppError::Validation(format!(
                "Comment must be at most {MAX_COMMENT_CHARS} characters (got {length})"
            )));
        }
        if let Some(at) = nul_at(raw) {
            return Err(AppError::Validation(nul_refusal("Comment", at)));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the repository relies on: whatever comes out of `parse`
    /// fits the column, in characters, for every alphabet.
    #[test]
    fn a_parsed_title_always_fits_the_column() {
        for c in ['a', 'é', '合', '𝄞'] {
            let at_the_limit = c.to_string().repeat(MAX_TITLE_CHARS);
            let title = Title::parse(&at_the_limit).expect("the documented maximum is storable");
            assert_eq!(title.as_str().chars().count(), MAX_TITLE_CHARS);

            let over = c.to_string().repeat(MAX_TITLE_CHARS + 1);
            assert!(Title::parse(&over).is_err(), "{c:?} above the maximum");
        }
    }

    /// The byte/character confusion, stated as a test: 500 accented characters
    /// are 1 000 bytes and still 500 characters.
    #[test]
    fn a_title_is_bounded_in_characters_not_bytes() {
        let accented = "é".repeat(MAX_TITLE_CHARS);
        assert_eq!(
            accented.len(),
            2 * MAX_TITLE_CHARS,
            "the premise: 2 bytes each"
        );
        assert!(Title::parse(&accented).is_ok());
    }

    /// What used to reach `VARCHAR(500)` as 501 characters.
    #[test]
    fn a_title_is_trimmed_before_it_is_measured_and_before_it_is_stored() {
        let padded = format!("  {}  ", "a".repeat(MAX_TITLE_CHARS));
        let title = Title::parse(&padded).expect("the padding is not part of the title");
        assert_eq!(title.as_str().chars().count(), MAX_TITLE_CHARS);
        assert_eq!(title.as_str(), "a".repeat(MAX_TITLE_CHARS));

        assert_eq!(Title::parse("   Contrat   ").unwrap().as_str(), "Contrat");
    }

    #[test]
    fn a_blank_title_is_refused_whatever_the_whitespace_is_made_of() {
        for blank in ["", " ", "\t\n ", "\u{00a0}", "\u{3000}"] {
            assert!(Title::parse(blank).is_err(), "{blank:?} was accepted");
        }
    }

    /// The character index, not the byte offset — the same unit every other
    /// bound in this module counts in, so the message means the same thing in
    /// every alphabet.
    #[test]
    fn the_position_reported_is_a_character_index() {
        assert_eq!(nul_at("clean"), None);
        assert_eq!(nul_at("a\u{0}b"), Some(1));
        // Four accented characters are eight bytes; the answer is 4.
        assert_eq!(nul_at("ééée\u{0}"), Some(4));
        assert_eq!(nul_at("\u{0}"), Some(0));
    }

    /// The five fields answer alike because they ask the same function.
    #[test]
    fn every_bounded_field_refuses_the_one_character_the_column_cannot_hold() {
        let title = Title::parse("a\u{0}b").expect_err("a title carrying U+0000");
        let comment = Comment::parse("relu\u{0}").expect_err("a comment carrying U+0000");
        for err in [title, comment] {
            let message = err.to_string();
            assert!(
                message.contains("U+0000"),
                "the refusal must name the character: {message}"
            );
        }

        // One character wide: its neighbour is storable and stays accepted.
        assert_eq!(Title::parse("a\u{1}b").unwrap().as_str(), "a\u{1}b");
        assert_eq!(Comment::parse("relu\u{1}").unwrap().as_str(), "relu\u{1}");
    }

    #[test]
    fn a_comment_is_bounded_in_characters_and_kept_verbatim() {
        let accented = "é".repeat(MAX_COMMENT_CHARS);
        let comment = Comment::parse(&accented).expect("the documented maximum is storable");
        assert_eq!(comment.as_str(), accented);

        assert!(Comment::parse(&"a".repeat(MAX_COMMENT_CHARS + 1)).is_err());
        // Prose, not an identifier: nothing is trimmed away.
        assert_eq!(Comment::parse("  relu  ").unwrap().as_str(), "  relu  ");
    }
}
