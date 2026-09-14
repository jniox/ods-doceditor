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

#[cfg(test)]
mod tests {
    use super::supplied;

    #[test]
    fn a_blank_value_is_no_value() {
        assert_eq!(supplied(None), None);
        assert_eq!(supplied(Some("")), None);
        assert_eq!(supplied(Some("   ")), None);
        assert_eq!(supplied(Some("\t\n")), None);
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
