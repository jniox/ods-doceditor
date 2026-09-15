//! What a caller actually supplied in the query string.
//!
//! One rule, stated once, for the four parameters of `GET /api/v1/documents`:
//! **a value that is blank once trimmed is a value that was not supplied.**
//!
//! It exists because the same gesture — *the field was left empty* — used to
//! have four different answers on one request line, none of them chosen:
//!
//! ```text
//! ?page=      400 text/plain    (serde: "cannot parse integer from empty string")
//! ?per_page=  400 text/plain    (same)
//! ?status=    400 application/json  {"error":"bad_request","message":"Invalid status: "}
//! ?search=    200 with an EMPTY page
//! ```
//!
//! `?page=&per_page=&status=&search=` is what a form submits when nothing was
//! typed into it, so the first three refuse an ordinary request and the fourth
//! answers a falsehood: a tenant owning documents is told it owns none, which
//! is exactly the reading `?status=bogus` was changed to refuse on 2026-09-14
//! ("a typo must not read as *you own no documents*"). A wrong answer nothing
//! surfaces is worse than a rejection.
//!
//! The rule is not invented here either: `api::middleware::correlate` already
//! applies it to `X-Correlation-Id`, `X-Source-Service` and `X-Tenant-Id` —
//! `.map(str::trim).filter(|v| !v.is_empty())` — so an empty header is an
//! absent header. This module is that same sentence, moved to where the other
//! half of the request is read. See `tests/query_contract_test.rs`.

use crate::domain::text::{nul_at, nul_refusal};
use crate::error::{AppError, AppResult};

/// The value a caller supplied, or `None` when the parameter carries nothing.
///
/// Blankness is judged after trimming — `?search=%20` is a field the caller
/// left alone as surely as `?search=` is — but a value that survives that test
/// travels **exactly as it was sent**. That asymmetry is deliberate and it is
/// the whole width of this change: `?status=published%20` is a *typo*, and
/// `tests/list_contract_test.rs` refuses it with a `400` on purpose ("a value
/// that looks like a status and is not one is the realistic typo"). Trimming
/// here would have quietly overturned that decision while claiming to fix
/// something else, which is a decision this repository does not get to make in
/// passing.
pub fn supplied(raw: Option<&str>) -> Option<&str> {
    raw.filter(|value| !value.trim().is_empty())
}

/// The search term a caller supplied, refused when it carries a character the
/// database cannot be asked about.
///
/// `search` is the only one of the four parameters whose value travels to
/// PostgreSQL as text — `page` and `per_page` are parsed into integers, and
/// `status` must be one of three words — so it is the only one that could
/// carry `U+0000` into a query. It did: `?search=%00` answered
/// `500 {"error":"internal_error"}` until 2026-09-15, because the parameter
/// reached `plainto_tsquery` and PostgreSQL refused the *bind* with `22021`.
///
/// A `400` and not a `422`, because that is what this service already answers
/// for a query parameter it will not accept (`?status=published%20`,
/// `?page=abc`) — the refusal belongs to the query string, not to a body
/// field. The rule itself is [`crate::domain::text::nul_at`], the same one the
/// four body fields cross.
pub fn search_term(raw: Option<&str>) -> AppResult<Option<&str>> {
    let Some(term) = supplied(raw) else {
        return Ok(None);
    };
    if let Some(at) = nul_at(term) {
        return Err(AppError::BadRequest(nul_refusal(
            "Query parameter 'search'",
            at,
        )));
    }
    Ok(Some(term))
}

#[cfg(test)]
mod tests {
    use super::{search_term, supplied};

    #[test]
    fn a_blank_value_is_no_value() {
        assert_eq!(supplied(None), None);
        assert_eq!(supplied(Some("")), None);
        assert_eq!(supplied(Some("   ")), None);
        assert_eq!(supplied(Some("\t\n")), None);
    }

    /// `?search=%00` reached `plainto_tsquery` and answered `500`. It is the
    /// query string's own refusal — `400` — and the term is otherwise untouched.
    #[test]
    fn a_search_term_carrying_the_unstorable_character_is_refused_as_a_bad_request() {
        assert_eq!(search_term(None).unwrap(), None);
        assert_eq!(search_term(Some("  ")).unwrap(), None);
        assert_eq!(
            search_term(Some(" clause resolutoire ")).unwrap(),
            Some(" clause resolutoire ")
        );

        let err = search_term(Some("clause\u{0}resolutoire")).expect_err("U+0000 in a search term");
        assert!(
            matches!(err, crate::error::AppError::BadRequest(_)),
            "{err}"
        );
        assert!(err.to_string().contains("search"), "{err}");
    }

    #[test]
    fn a_supplied_value_travels_exactly_as_it_was_sent() {
        assert_eq!(supplied(Some("draft")), Some("draft"));
        // Not trimmed: `?status=published%20` is a typo the list contract
        // refuses with a 400, and this function must not launder it into a
        // valid status on its way past.
        assert_eq!(supplied(Some("published ")), Some("published "));
        assert_eq!(
            supplied(Some(" clause resolutoire ")),
            Some(" clause resolutoire ")
        );
    }
}
