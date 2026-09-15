# ADR-013 — A list is walked page by page, so its order is total

- **Status**: accepted
- **Date**: 2026-09-14
- **Context**: `GET /api/v1/documents`, `ORDER BY updated_at DESC`, and a
  document that no page ever returns

## The measurement

`ORDER BY updated_at DESC` is not a total order. Two documents written by a
single transaction carry the same `now()` to the microsecond — `now()` is the
*transaction* timestamp — so a seed, an import, a data migration or any future
bulk write ties them exactly. PostgreSQL is then free to return tied rows in
whatever order the plan it chose produces, and **it does not choose the same
plan for every page of the same list**. Measured on PostgreSQL 17, this schema,
2 000 tied documents, `per_page = 100`:

```text
EXPLAIN … ORDER BY updated_at DESC LIMIT 100 OFFSET 0
  ->  Index Scan using idx_documents_tenant_updated on documents
EXPLAIN … ORDER BY updated_at DESC LIMIT 100 OFFSET 1900
  ->  Sort  (Sort Key: updated_at DESC)  ->  Seq Scan on documents

walking all 20 pages:
  rows returned 2000 | distinct documents 1999
  tie-00410 returned twice | tie-00801 never returned at all
```

A client that walks the pages to build its own list **silently loses a
document**, and every response along the way looks perfectly ordinary. The same
family as the page this endpoint used to report having served while serving
another: the answer is not wrong in a way the caller can see.

Two things this is *not*:

- it is not the ordinary caveat of offset pagination (that a concurrent write
  shifts the window). No write happens during the walk above;
- it is not visible in today's data. Measured the same day on this instance:
  **7 074 documents, zero ties** — one document per transaction is what the API
  produces, and microsecond timestamps then differ. The list is stable because
  of a property of the traffic, not because of a property of the query. That is
  exactly the kind of guarantee this service has learned to stop leaning on.

## Decision

`build_list_query` orders by `updated_at DESC, id DESC`. `id` is the primary
key, so the order is total and the pages are a partition of the collection
whatever plan each one gets.

Nothing else changes: the same rows in the same overall order, the tie-break
only decides between rows the old clause left unordered. The existing index
`(tenant_id, updated_at DESC)` still serves the ordering; the tie-break costs
nothing outside of the ties it exists for.

`version_repo::list_versions` needs no such change and is left alone:
`ORDER BY version DESC` is already total, because migration 003 declares
`UNIQUE (document_id, version)`.

## How it is guarded

`tests/list_stability_test.rs`, which asks the database rather than the diff.
Forty documents are created through the repository and then flattened to one
`updated_at` by a single `UPDATE` — which is what a bulk write is — and the
pages are read through two pools pinned to the two plans the planner picked on
its own above (`enable_seqscan = off`; `enable_indexscan`/`bitmapscan`/
`indexonlyscan = off`), the technique `tests/search_index_test.rs` already uses
to reach a plan an index hides.

- **The invariant, which fails first**: the same page, asked of the two plans,
  must come back identical. Before the tie-break, page 1 was
  `…-0039, …-0037, …` from the index and `…-0037, …, …-0039` from the sort —
  the same ten documents in two orders, which is already a broken promise.
- **The consequence**: a walk that takes its pages from the two plans
  alternately must return each document exactly once.

Pinning the plans rather than growing the table to two thousand rows is
deliberate: the planner's switch point depends on cost estimates, so a test
that waited for it would go green vacuously on a smaller or fresher database —
which is the failure mode this repository keeps finding in its own guards.
