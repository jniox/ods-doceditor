//! The page a list endpoint actually serves.
//!
//! This type exists because the normalisation and the reporting of a page used
//! to live in different modules: `DocumentService::list_documents` clamped the
//! caller's numbers, `api::documents::list_documents` echoed the caller's
//! numbers, and neither knew about the other. The result was a response whose
//! metadata contradicted its own payload — `"per_page": 1000` above at most a
//! hundred documents, `"page": 0` above the first page — which is worse than a
//! rejection, because a client that trusts `total` and `per_page` to paginate
//! computes the wrong number of pages and never sees an error.
//!
//! The fix is not a second clamp at the boundary; a second clamp is a second
//! thing to forget. It is to make the normalised page a **value that travels**:
//! the handler builds one, the service and the repository consume it, and the
//! response is rendered from the same value. There is then no other number in
//! scope to report by mistake.

use serde::Serialize;

/// A normalised page request: what the server will actually serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Pagination {
    page: i64,
    per_page: i64,
}

impl Pagination {
    /// Largest page size a caller may ask for. Declared in `docs/openapi.yaml`
    /// as `maximum: 100`, which is only true of the answer if the answer says
    /// so too.
    pub const MAX_PER_PAGE: i64 = 100;

    /// Page size used when the caller does not ask for one.
    pub const DEFAULT_PER_PAGE: i64 = 20;

    /// Normalise what arrived in the query string.
    ///
    /// Absent means the documented defaults; out of range means the nearest
    /// value in range, never an error: a page past the end of a collection is a
    /// legitimate question with an empty answer, and pagination is exactly the
    /// place where clients drift out of range by counting.
    pub fn new(page: Option<i64>, per_page: Option<i64>) -> Self {
        Self {
            page: page.unwrap_or(1).max(1),
            per_page: per_page
                .unwrap_or(Self::DEFAULT_PER_PAGE)
                .clamp(1, Self::MAX_PER_PAGE),
        }
    }

    pub fn page(&self) -> i64 {
        self.page
    }

    pub fn per_page(&self) -> i64 {
        self.per_page
    }

    /// Rows to skip — saturating, never wrapping.
    ///
    /// `(page - 1) * per_page` overflows `i64` for pages a caller can type in a
    /// query string: `page=9223372036854775807` panics a debug build and, on a
    /// release build, wraps to `OFFSET -200`, which PostgreSQL rejects (`2201X`)
    /// and the service reports as a 500. Saturating turns that into what it
    /// always meant — a page far past the end, which is empty.
    pub fn offset(&self) -> i64 {
        (self.page - 1).saturating_mul(self.per_page)
    }
}

#[cfg(test)]
mod tests {
    use super::Pagination;

    #[test]
    fn ordinary_pages_are_plain_arithmetic() {
        let p = Pagination::new(Some(3), Some(20));
        assert_eq!((p.page(), p.per_page(), p.offset()), (3, 20, 40));
    }

    #[test]
    fn out_of_range_input_is_normalised_rather_than_refused() {
        assert_eq!(Pagination::new(Some(0), None).page(), 1);
        assert_eq!(Pagination::new(Some(-9), None).page(), 1);
        assert_eq!(
            Pagination::new(None, Some(10_000)).per_page(),
            Pagination::MAX_PER_PAGE
        );
        assert_eq!(Pagination::new(None, Some(0)).per_page(), 1);
        assert_eq!(Pagination::new(None, Some(-3)).per_page(), 1);
    }

    #[test]
    fn the_offset_never_wraps_negative() {
        for page in [i64::MAX, i64::MAX - 1, i64::MAX / 2, 1_000_000_000_000] {
            let offset = Pagination::new(Some(page), Some(Pagination::MAX_PER_PAGE)).offset();
            assert!(
                offset >= 0,
                "page={page} produced OFFSET {offset}: the multiplication wrapped"
            );
        }
    }

    #[test]
    fn absent_input_means_the_documented_defaults() {
        let p = Pagination::new(None, None);
        assert_eq!((p.page(), p.per_page(), p.offset()), (1, 20, 0));
    }
}
