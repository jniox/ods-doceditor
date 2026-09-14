# ADR-007: The history list is a projection without bodies

**Date**: 2026-09-14  **Status**: accepted

## Context

`GET /api/v1/documents/{id}/versions` answers with version numbers, dates,
authors, comments and sizes. `docs/openapi.yaml` states it plainly — *"Most
recent version first. **Bodies are not included.**"* — and `src/api/versions.rs`
renders exactly those fields and nothing else.

The SQL underneath asked for `content` anyway, for every version of the
document, and the handler dropped it one layer higher.

The same rule was already stated and implemented one module over. `document_repo`
splits `DOC_COLUMNS` from `SUMMARY_COLUMNS` and explains why in a comment:
*"shipping every body in a page of 100 would make the endpoint unusable for the
very case pagination exists for."* The version history is **not paginated at
all** — by contract, it returns the whole history — so it is the endpoint where
that reasoning mattered most, and it is the one where the projection was never
split.

Three write paths had the mirror image of the defect: `insert_version` ended in
`RETURNING … content`, so creation, content mutation and explicit snapshot each
read the body straight back out of PostgreSQL immediately after writing it, for
three callers of which **none** looks at it (the first two discard the value
entirely).

## What was measured

On the running binary, under the deployment's own limits —
`ops/cloudrun/doceditor.json` allocates **512 MiB**, `MAX_DOCUMENT_SIZE_MB`
defaults to **10**.

A document at the published body ceiling, with 55 versions — 54 content
`PATCH`es, which is an afternoon of an editor's autosave traffic:

```text
GET /api/v1/documents/{id}/versions
  -> Remote end closed connection without response     (0.7 s)
  -> systemd: Result=oom-kill, MainPID=0
```

Not a 500, and not a failure confined to that request: on Cloud Run an OOM kill
takes the **instance** down, so every other tenant's in-flight request on it
fails too. The trigger is two ordinary calls by one caller, and the answer the
request was building is nine kilobytes of JSON containing no body at all.

At a size small enough to survive, the ratio that explains it:

| | |
|---|---|
| versions × body | 25 × 1 048 560 B |
| bytes the query read from PostgreSQL | 25 MB |
| bytes the response carried | 4 268 |
| process RSS across the single call | 35 348 kB → 51 420 kB |

This is the fourth batch running in which a cost was attached to a quantity
other than the one its name promises — payload versus body (ADR: `api::payload`),
bytes versus characters (`domain::text`), the search index versus the document
(ADR-006), and now the projection versus the response.

## Decision

Two projections, named once each in `version_repo`, and a domain type for the
one that carries no body.

* `VERSION_COLUMNS` — the full row. Used by exactly one read,
  `GET /documents/{id}/versions/{n}`, because handing a prior body back to a
  product is that endpoint's whole purpose. It returns `DocumentVersion`.
* `VERSION_SUMMARY_COLUMNS` — everything except `content` and `yjs_snapshot`.
  Used by the history list **and** by `insert_version`'s `RETURNING`. It returns
  `DocumentVersionSummary`.

`DocumentVersionSummary` is a type rather than a convention for the same reason
`domain::text::Title` and `domain::pagination::Pagination` are types: a value
that must not travel is easiest to keep still when it is not in the struct at
all. A future read path cannot leak a body into the history list by forgetting a
column — the field does not exist on the value it returns.

## Consequences

Measured after the change, same binary, same 512 MiB cap, same document (55
versions of 10 484 720 B):

```text
GET /api/v1/documents/{id}/versions
  -> 200, 9 423 bytes, 55 ms
  -> cgroup memory 6.3 MiB -> 7.1 MiB, process alive
  -> ten consecutive reads: 200 ×10 in 0.1 s, 7.3 MiB

GET /api/v1/documents/{id}/versions/1
  -> 200, 10 484 915 bytes          (the body-bearing read is unchanged)
```

And in the suite, `tests/history_read_test.rs` asks PostgreSQL rather than
trusting this document: two documents with identical histories and bodies four
orders of magnitude apart must cost the same to list. Before the split it
measured 1 451 bytes against 5 244 076; re-introducing `content` into the
summary projection turns it red again with the real byte counts.

### What this does *not* fix, measured

The `RETURNING` half is correct but small. A/B on the write path, same
conditions:

| | 6 concurrent 10 MB saves | 10 concurrent |
|---|---|---|
| body-carrying `RETURNING` | 200 ×6, peak 433 MiB | — |
| summary `RETURNING` | 200 ×6, peak 418 MiB | oom-kill |

Ten content saves of full-size documents arriving together kill a 512 MiB
instance **either way**: the dominant cost on that path is the buffered request
payload plus its parsed `String`, not the row that comes back. Removing one copy
out of several does not move the threshold, and this ADR does not claim it does.

That leaves a real, separate question this repository cannot answer alone:
`MAX_DOCUMENT_SIZE_MB=10`, `memory: 512Mi` and an unbounded number of concurrent
requests are three settings that only make sense together, and nothing today
relates them. Raising the document ceiling to 50 MB would make **two**
simultaneous saves fatal. The remedies — a smaller body ceiling, more instance
memory, a Cloud Run concurrency bound, or an in-process admission limit on large
payloads — are a sizing decision with no spec to settle it, and
`ops/cloudrun/doceditor.json` is outside this repository. It is reported with
its measurement rather than guessed at here.

### Still open, still not decided

The history remains **unpaginated**: the contract returns every version, and a
document with ten thousand of them now answers with ~1.5 MB of summaries instead
of gigabytes of bodies. That is survivable where the previous behaviour was not,
but it is still unbounded. Adding a default page size would silently truncate
history for existing clients, which is a product decision, not a repair.
Reported, not taken — like `archived` freezing the status but not the body.

## Alternatives considered

**Paginate the history instead.** It would bound the cost, but it changes the
published contract and every existing caller's meaning of "the history", and it
would leave the write-path `RETURNING` untouched. It also fixes the symptom at
one endpoint rather than the rule at its call sites.

**Select the bodies and drop them earlier in Rust.** This is what the code
already did; the cost is paid in PostgreSQL, on the wire and in the allocator
long before the handler chooses what to render.

**Keep one projection and a `SELECT` per call site.** Rejected for the reason
ADR-006 gives for `editor.searchable_text`: a rule that is re-typed at each call
site is a rule that will differ at one of them. The `deleted_at IS NULL`
predicate was written inline in `list_versions` and forgotten in `get_version`
forty lines below; naming the thing once is what stops the next omission.
