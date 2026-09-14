# ADR-006: The search index is bounded, so the document does not have to be

**Date**: 2026-09-14  **Status**: accepted

## Context

`MAX_DOCUMENT_SIZE_MB` (10 by default when this was written, 2 since ADR-009
and HR-20260914-001) is what this service publishes as the largest body it
stores. Nothing below depends on which of the two it is: the index budget is
spent by the *vocabulary* of the text, so the ceiling and this bound are
independent, which is the reason to name the searchable projection once rather
than to keep the two numbers in step. ADR-001 made the body the thing DocEditor owns, and
`src/api/payload.rs` exists so that a body of exactly that size can be carried
over HTTP — the previous batch fixed a ceiling that made the documented maximum
unreachable.

Underneath, migration 006 also put every document into a GIN index:

```sql
CREATE INDEX idx_documents_content_fts ON editor.documents
    USING GIN (to_tsvector('english', title || ' ' || content));
```

A PostgreSQL `tsvector` cannot exceed **1 048 575 bytes of lexemes**, and an
index expression that raises makes the *write* raise: the row cannot be stored
at all. What consumes that budget is the **vocabulary** of the text rather than
its length — duplicate words merge, distinct ones do not — so the real ceiling
was a property of the prose, not of the request. Measured on PostgreSQL 17
before this decision:

| body | contents | outcome |
|---|---|---|
| 348 893 B | every word distinct | stored |
| 708 893 B | every word distinct | stored |
| **798 893 B** | every word distinct | **`ERROR: string is too long for tsvector`** |
| 1 888 894 B | every word distinct | `ERROR` (2 598 012 B of lexemes) |
| 10 050 000 B | ordinary repetitive prose | stored |

So a 9.6 MiB contract of ordinary prose stored fine while a 0.8 MB annex of
reference codes — a pasted export, a generated appendix, a list of identifiers —
answered `500 {"error":"internal_error"}`. Both requests are legal under every
published rule of this API. The largest document a client could store was a
number no client could compute, and nothing told it so: this is the same family
as the two ceilings of `src/api/payload.rs` and the byte-versus-character title
bound of `src/domain/text.rs`, the third time in three batches that a
limit turned out to be applied to a different quantity from the one its name
promises.

## Decision

**The bound belongs to what is indexed, never to what is stored.** Migration 009
names the searchable projection once and builds the index on it:

```sql
CREATE FUNCTION editor.searchable_text(title text, content text)
    RETURNS text IMMUTABLE PARALLEL SAFE
    RETURN left(coalesce(title, '') || ' ' || coalesce(content, ''), 250000);

DROP INDEX IF EXISTS editor.idx_documents_content_fts;
CREATE INDEX idx_documents_searchable_fts ON editor.documents
    USING GIN (to_tsvector('english', editor.searchable_text(title, content)));
```

Three properties make this a decision rather than a patch.

1. **The document is untouched.** It is stored, versioned, served and restored
   whole, at any size the body ceiling admits. Only its *searchability* stops at
   the first 250 000 characters — the title first, so a document is always
   findable by its name however large its body.
2. **The projection is named, not inlined.** `src/repository/document_repo.rs`
   sends `editor.searchable_text(title, content)` too, through a single
   `search_predicate()`. This is load-bearing: truncating the index alone would
   have *moved* the failure rather than removed it. A predicate over the
   untruncated body raises the identical error on a **read** — measured, not
   supposed: with the fixed index in place and the old predicate restored,
   `GET /api/v1/documents?search=prestation` answers `500` for any tenant owning
   one large document, because a bitmap heap scan rechecks the condition on the
   heap row. Naming the expression once is what keeps the index and the query
   the same expression; PostgreSQL confirms the match by using the index for it.
3. **250 000 is measured.** The limit counts bytes of lexemes while `left()`
   counts characters, which is exactly the conversion that produced the previous
   defect in this repository. The worst case for this value — 250 000 characters
   of distinct accented tokens, the costliest alphabet per character — produces
   576 628 bytes of `tsvector`, a little over half the hard limit. Raising it to
   900 000 overflows (1 314 648 B), and `tests/search_index_test.rs` re-measures
   all of it in three alphabets so that the constant cannot be raised blind.

## Alternatives considered

**Refuse documents whose vocabulary is too rich.** Turns an internal limit into
a product rule and contradicts `MAX_DOCUMENT_SIZE_MB`, which is published, in a
way no caller can predict: the refusal would depend on how varied the prose is.
Rejected — the request is legal, and the service exists to hold the body.

**Leave it and document the real ceiling.** There is no real ceiling to
document; it is a function of the text. The honest version of that sentence is
"documents above roughly 0.8 MB may or may not be storable", which is not a
contract.

**A stored `tsvector` column maintained by a trigger.** Same hard limit, more
machinery, and the write still fails — the limit is on the `tsvector`, not on
the index.

**`CREATE INDEX CONCURRENTLY`** to rebuild without locking writes. Impossible
here: sqlx runs each migration inside a transaction, and `CONCURRENTLY` cannot
run in one. The table is small (1 278 rows on the shared dev
instance, fewer on staging) and the rebuild is milliseconds; a service that grows past that should revisit this with a
`-- no-transaction` migration rather than assume it stays cheap.

## Consequences

- A document larger than 250 000 characters is fully stored and fully served,
  and full-text search matches only its first 250 000 characters. This is
  written into `docs/openapi.yaml` next to the `search` parameter rather than
  left for a client to discover.
- `DROP INDEX` appears in a migration of this repository for the first time,
  which the repo's own rule normally avoids. It is deliberate and it loses no
  data: an index is derived, the statement that replaces it covers the same
  documents, and leaving the old one in place would keep refusing exactly the
  writes this decision exists to accept. Nothing else in migration 009 is
  destructive.
- The old index can no longer be rebuilt on a database that has accepted such a
  document — which is the plainest statement of the defect, and is captured in
  this batch's evidence folder.
