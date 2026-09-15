# ADR-014 — An update writes the columns it was given, and no others

- **Status**: accepted
- **Date**: 2026-09-15
- **Context**: `PATCH /api/v1/documents/{id}`, a title of twenty-six bytes, and
  eight megabytes of write-ahead log

## The measurement

`document_repo::update_document` locked the row with `SELECT {DOC_COLUMNS} …
FOR UPDATE` — the body included — and then wrote **every** column back:

```sql
SET title = $1, status = $2, metadata = $3, content = $4, word_count = $5,
    current_version = $6, updated_at = now()
```

`$4` was bound to the body that had just been read one statement earlier. A
column bound to a parameter is a column PostgreSQL must store afresh, so the
body was re-TOASTed on every PATCH: new chunks, new write-ahead log, and the
previous chunks dead until autovacuum — for a request that changed a title.

Measured on the running binary (release build, PostgreSQL 17.11, a document
whose body is 8 000 000 bytes of incompressible text — the ceiling this service
published until HR-20260914-001):

```text
                                             WAL written    latency
  PATCH {"title": "Renommage A"}              9 087 272 B     506 ms
  PATCH {"status": "published"}               9 086 976 B     286 ms
  an idle window of the same length                   0 B          —   <- no noise
  PATCH {"content": …}  (the legitimate one) 17 670 984 B     427 ms
```

and, after this ADR, on the same binary, same document size:

```text
  PATCH {"title": …}                            299 144 B     120 ms
  PATCH {"status": …}                           299 160 B     123 ms
  PATCH {"metadata": …}                         299 144 B     120 ms
  PATCH {"content": …}                       17 650 104 B  ~450-500 ms
```

**Thirty times less write-ahead log for a change that touches no body.** The
299 kB that remain are the index work every update here owes and cannot avoid:
`updated_at` is itself indexed (`idx_documents_tenant_updated`), so no update of
this table is ever HOT, and every one of them inserts new entries into all four
indexes — including the GIN expression index of ADR-006, which re-evaluates
`to_tsvector(editor.searchable_text(title, content))`. That floor is identical
before and after. What disappears is the body.

The same statement also stopped *reading* the body it had no use for. Under
contention that read was the visible cost: 20 concurrent renames of that
document produced **19 sqlx "slow statement" warnings**, the locking
`SELECT … content … FOR UPDATE` taking up to **10.0 s** — each waiter, once it
got the lock, detoasting and shipping 8 MB before deciding whether a status
transition was legal.

## What it cost, and what it did not

Three consequences, none of them visible in any response:

1. **Write-ahead log volume.** Billed, replicated, and — per ADR-004 of the
   platform — read by Datastream: documentary analytics would have carried 8 MB
   into BigQuery every time someone renamed a file.
2. **Table bloat.** Each rename left a full dead copy of the body in the TOAST
   relation until autovacuum reclaimed it.
3. **Two useless copies of the body in the process**, on every PATCH —
   including on a PATCH that *replaces* the body, which read the old one first
   for nothing.

**And the hypothesis the measurement refused.** At the deployment's own limits
(512 MiB, and the concurrency of ADR-009), renaming a large document was *not*
an instance killer. Twenty and then forty concurrent renames of the 8 MB
document, in a 512 MiB cgroup:

```text
                    wall       peak RSS   HTTP        slow statements
  before  N=20    6 486 ms      369 MiB   200 x20          19
  after   N=20    2 376 ms      260 MiB   200 x20          11
  before  N=40   15 718 ms      384 MiB   200 x40          40
  after   N=40   13 623 ms      238 MiB   200 x40          31
```

Nothing was OOM-killed on either side. The repair buys ~146 MiB of headroom and
a factor of thirty on the log; it does not rescue the instance from a death it
was not dying. Said here because a fix whose benefit is overstated is how the
next reader inherits a false premise — and because the wall-clock gain at N=40
is modest for a reason worth knowing: forty renames of one document serialise on
that document's row lock (ADR-004) and each still returns the whole body to its
caller, which the published contract requires.

## Decision

**An update writes the columns it was given.** One statement, and every column
the caller did not name keeps the value it already has:

```sql
SET title    = COALESCE($1, title),
    status   = COALESCE($2, status),
    metadata = COALESCE($3, metadata),
    content  = COALESCE($4, content),
    word_count      = CASE WHEN $4 IS NULL THEN word_count ELSE $5 END,
    current_version = CASE WHEN $4 IS NULL THEN current_version
                                           ELSE current_version + 1 END,
    updated_at = now()
```

Passing the column through leaves the TOAST pointer untouched, which is the
whole mechanism — measured, not assumed: the same rename written as a partial
`UPDATE` by hand cost 299 120 bytes against 9 087 272.

`word_count` and `current_version` move with the body and only with it, so they
are `CASE`s on the same parameter rather than values computed in Rust from a row
read under the lock. That also removes the last read-modify-write on
`current_version`: the number is now assigned by PostgreSQL inside the locked
statement, and `insert_version` takes it from the `RETURNING` rather than
recomputing it. Two spellings of "the next version" is how a document row and
its history come to disagree.

**The lock stays exactly where it was.** `SELECT … FOR UPDATE` on the same row,
in the same order as `version_repo::create_version` — ADR-004 is untouched, and
`tests/concurrency_test.rs` still passes unchanged. What changed is its
projection: `UPDATE_LOCK_COLUMNS` is `status`, which is all the decision needs
(the transition to judge, and the existence of a live row to answer 404).

## Consequences

- A rename, a publication and a re-tag cost the size of what they change.
- `DOC_COLUMNS` and `UPDATE_LOCK_COLUMNS` are named constants, as `version_repo`
  names its two: a projection written inline is a projection that grows a body
  column back the next time someone needs one more field.
- `tests/update_cost_test.rs` measures the property rather than restating it,
  using PostgreSQL 17's `pg_column_toast_chunk_id`: a body that was rewritten
  points at a **new** TOAST value; one that was left alone points at the same
  one. That is a per-row fact, immune to what the rest of the suite is writing
  in parallel — unlike a WAL-position delta, which is cluster-wide and would
  make the file fail for someone else's megabytes.
- The witness of non-vacuity is in the file: a content change *must* move the
  chunk id. Without it, every assertion would also hold for an instrument that
  sees nothing.
- Nothing about the API changes: same statuses, same bodies, same contract. This
  ADR is invisible from the wire, which is exactly why nothing was red.

## The family it belongs to

Fifth batch running in which a cost was attached to a quantity other than the
one its name promised: the payload ceiling applied to the JSON encoding rather
than to the body (ADR-009), a bound named in characters and applied in bytes,
the search index bounded by the document's vocabulary rather than its size
(ADR-006), the history list reading every body it had promised not to return
(ADR-007). Here: "rename" priced in megabytes of body.
