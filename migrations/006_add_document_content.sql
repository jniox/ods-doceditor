-- The document body DocEditor exists to store.
--
-- Until this migration the service stored only title/status/metadata: `yjs_state`
-- was read when snapshotting a version but written by no code path, so every
-- version row carried an empty byte array and no product could ever author
-- content. `content` is the authored body (HTML/Markdown/JSON — the product
-- brings its own editor, DocEditor owns the canonical storage and the history).
--
-- `yjs_state` / `yjs_snapshot` are deliberately left in place and untouched:
-- they are the CRDT channel for the collaborative-editing layer that has not
-- been built, and overloading them to carry plain text would make that layer
-- unbuildable later.
--
-- Additive and idempotent (shared dev database): existing rows get ''.
ALTER TABLE editor.documents
    ADD COLUMN IF NOT EXISTS content TEXT NOT NULL DEFAULT '';

ALTER TABLE editor.document_versions
    ADD COLUMN IF NOT EXISTS content TEXT NOT NULL DEFAULT '';

-- Full-text search currently indexes the title only; the body is searchable too.
CREATE INDEX IF NOT EXISTS idx_documents_content_fts
    ON editor.documents USING GIN (to_tsvector('english', title || ' ' || content));
