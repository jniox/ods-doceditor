# ADR-001: Documents carry their content, and every content change writes a version

**Date**: 2026-09-13  **Status**: accepted

## Context

DocEditor is positioned in the GTM brief as the service that *authors and stores
the document*, with history. Until 2026-09-13 it stored a title, a status, some
metadata and a `current_version` counter, and nothing else.

The review of 2026-09-12 named the consequence precisely: `editor.documents`
had exactly one content-shaped column, `yjs_state`, it was **read** once (to
snapshot into a version row) and **written** nowhere, and neither
`CreateDocumentRequest` nor `UpdateDocumentRequest` carried a content field at
all. There was no API path by which a document's body could ever enter the
service. A second defect hid underneath: `current_version` was set to 1 at
creation with no version row to match, so the original state of every document
was permanently unreachable — the history began at the *second* state.

Two questions had to be answered together, because the answer to one constrains
the other: what is a document's content, and when does a version exist?

## Decision

**1. A document carries a `content` body, and DocEditor does not interpret it.**
`content` is a text column holding HTML, Markdown or JSON as the calling product
chooses. The product brings its own editor; DocEditor owns the storage and the
history. There is no server-side schema for the body, no sanitisation and no
rendering.

**2. Creation writes version 1.** A document exists at version 1 *and* there is
a row in `editor.document_versions` describing it. `current_version` is never a
number without a row behind it.

**3. Any change to `content` advances `current_version` and writes the matching
immutable version row in the same transaction.** Not a trigger, not a background
job, not a second call from the service layer: one SQL transaction, so a
document whose version counter moved without its snapshot being written is not a
state the database can be left in.

**4. `is_auto` distinguishes who asked.** `true` for the snapshots the service
takes itself on a content change, `false` for an explicit
`POST /api/v1/documents/{id}/versions`. Both are immutable; the flag is for the
product's history UI, not for the storage rule.

**5. `yjs_state` and `yjs_snapshot` stay reserved for the collaborative CRDT
layer, which is not built.** They are not repurposed as the content column, and
no text is stored in them.

## Alternatives considered

**Store the body in `yjs_state`, since the column exists.** Rejected, and it was
the tempting move — it would have closed the finding with one line. A Yjs update
vector is a CRDT binary encoding; putting HTML there gives the column two
incompatible meanings and guarantees that the day collaborative editing is built,
every existing row must be distinguished by guesswork. An empty column is
cheaper than an ambiguous one.

**Version on every `PATCH`, whatever it changed.** Rejected: renaming a document
or publishing it is not an edit of the document. The history would fill with
snapshots identical to their predecessor and the product could no longer show a
useful diff.

**Version asynchronously, from an event consumer.** Rejected: it makes the
history eventually consistent with the document, so a read of `current_version`
can legitimately find no matching row — which is the defect this ADR exists to
remove, reintroduced deliberately.

**Let the product own the history and store only the head.** Rejected: it is the
one feature the GTM brief names as the reason the service exists ("create + 2
edits = 3 records").

## Consequences

- `migrations/006_add_document_content.sql` adds the column; the migration is
  additive and idempotent like every other one in this repository.
- Version rows grow with the size of the body, once per content change. The
  payload is bounded by `MAX_DOCUMENT_SIZE_MB` (default 10), which now bounds
  both the request and the stored body. No retention policy is defined yet; if
  history growth becomes a cost, that is a separate decision and it must not be
  taken by making versions mutable.
- `GET /api/v1/documents/{id}/versions/{version}` returns the body, so a prior
  state is genuinely restorable by the product.
- DocEditor storing an opaque body means it cannot render, index or sanitise it.
  Any product embedding that HTML is responsible for escaping it. This is
  written in `docs/openapi.yaml` rather than left to be discovered.
- The collaborative layer, when built, has to reconcile a CRDT with a linear
  version history. Keeping `yjs_*` empty and untouched is what leaves that
  decision open.
