-- What a document may contain, and what PostgreSQL is asked to index of it.
--
-- Migration 006 indexed the whole of `title || ' ' || content`:
--
--     CREATE INDEX idx_documents_content_fts ON editor.documents
--         USING GIN (to_tsvector('english', title || ' ' || content));
--
-- A `tsvector` cannot exceed 1 048 575 bytes of lexemes, and an index
-- expression that raises makes the WRITE raise — the row cannot be stored at
-- all. What blows that budget is the VOCABULARY of the text rather than its
-- length, so the largest document this service could store was a number no
-- caller could compute. Measured on PostgreSQL 17 before this migration:
--
--     body      348 893 B, every word distinct  -> stored
--     body      708 893 B, every word distinct  -> stored
--     body      798 893 B, every word distinct  -> ERROR: string is too long
--                                                  for tsvector
--     body    1 888 894 B, every word distinct  -> ERROR (2 598 012 B of lexemes)
--     body   10 050 000 B, repetitive prose     -> stored
--
-- `MAX_DOCUMENT_SIZE_MB` says 10 MB and `src/api/payload.rs` exists so that a
-- body of exactly that size can be carried over HTTP. A 0.8 MB annex of
-- reference codes — a pasted export, a list of identifiers, a generated
-- appendix — answered `500 {"error":"internal_error"}` instead, on the service
-- whose entire purpose is to own the document body.
--
-- The bound therefore belongs to what is INDEXED, never to what is STORED:
-- full-text search reaches the first 250 000 characters of a document, and the
-- document itself is kept, versioned and served whole. 250 000 is measured, not
-- chosen for looks: the worst case for it (distinct accented tokens, the
-- costliest alphabet per character) produces 576 628 bytes of `tsvector`,
-- little over half the hard limit. `tests/search_index_test.rs` re-measures that
-- in three alphabets and fails if the constant is ever raised past what fits.
--
-- The projection is NAMED rather than inlined so the index and the service's
-- own predicate cannot drift apart: `src/repository/document_repo.rs` sends
-- `editor.searchable_text(title, content)` too. An inlined copy would still
-- answer, but it would stop matching the index — and a sequential scan would
-- then evaluate `to_tsvector` over the untruncated body, raising on a READ the
-- very error this migration removes from the WRITE.
--
-- The `RETURN` body form (PostgreSQL 14+) rather than the `AS $$ … $$` string:
-- it is parsed at creation time, so the function cannot be re-interpreted later
-- under a different `search_path` — which matters here because an index depends
-- on it.
CREATE OR REPLACE FUNCTION editor.searchable_text(title text, content text)
    RETURNS text
    LANGUAGE sql
    IMMUTABLE
    PARALLEL SAFE
    RETURN left(coalesce(title, '') || ' ' || coalesce(content, ''), 250000);

-- The old index IS the defect, so it goes. This is the one destructive
-- statement in this repository's migrations and it is deliberate: an expression
-- index is re-evaluated on every insert and update, so leaving it in place would
-- keep refusing the same documents no matter what the new one covers. No data is
-- involved — an index is derived, and the statement below rebuilds the whole of
-- what this one provided, over a bounded projection.
DROP INDEX IF EXISTS editor.idx_documents_content_fts;

CREATE INDEX IF NOT EXISTS idx_documents_searchable_fts
    ON editor.documents
    USING GIN (to_tsvector('english', editor.searchable_text(title, content)));
