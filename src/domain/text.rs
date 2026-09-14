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
