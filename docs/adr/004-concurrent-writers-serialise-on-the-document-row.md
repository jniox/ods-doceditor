# ADR-004: Concurrent writers serialise on the document row

**Date**: 2026-09-13  **Status**: accepted

## Context

ADR-001 made every content change write its own immutable version row in the
same transaction. It settled *what* a version is; it said nothing about what
happens when two writers ask for one at the same time.

Both write paths allocated the next version number the same way, in Rust,
between two statements:

```text
SELECT current_version ...            -- plain read, no lock
    version = current_version + 1     -- computed in the service
UPDATE documents SET current_version = <that absolute value>
INSERT INTO document_versions (version = <that absolute value>)
```

Nothing between the read and the write stopped a second transaction from
reading the same `current_version`. Two concurrent saves therefore both claimed
the same number. Measured on the dev instance on 2026-09-13, with a third
transaction holding the document row so the interleaving is guaranteed rather
than hoped for (`tests/concurrency_test.rs`):

- two content updates → `duplicate key value violates unique constraint
  "document_versions_document_id_version_key"`;
- a content update racing an explicit `POST /versions` → **`deadlock
  detected`**, because the two paths take the same two locks in opposite orders
  (the update path writes the document row then the version row, the snapshot
  path writes the version row then the document row).

Both reach the client as **HTTP 500 on a perfectly legitimate request**. For a
service whose purpose is several people editing one document, that is normal
traffic, not an edge case. The `UNIQUE (document_id, version)` of migration 003
is what turned the race into a loud error rather than a quiet one: without it,
the same race would have written two rows numbered alike and dropped one edit
from the history — the exact guarantee ADR-001 exists to give.

## Decision

**1. The first read of both write paths takes `FOR UPDATE` on the document
row.** `document_repo::update_document` and `version_repo::create_version` hold
the row for the rest of their transaction, so the read-modify-write on
`current_version` is serialised per document. The second writer waits, re-reads
the committed `current_version`, and takes the next number.

**2. Both paths lock the SAME row FIRST.** This, and not the lock alone, is what
removes the deadlock: the lock order is now identical on both paths regardless
of the order in which they write afterwards.

**3. Contention is per document, never per tenant.** Two documents of the same
tenant are written in parallel exactly as before. The lock is held for the
duration of one small transaction, with no external call inside it.

**4. The content itself remains last-write-wins.** Two writers still overwrite
each other's body; what is guaranteed is that *neither edit disappears from the
history* and *neither request fails*. Restoring the other edit is a product
gesture over `GET /documents/{id}/versions`, which is why that endpoint exists.

## Alternatives considered

**Compute the number in SQL (`SET current_version = current_version + 1 …
RETURNING`).** Rejected as insufficient on its own: it fixes the update path's
own counter, but the version row is a second statement against a second table,
and the snapshot path reads the counter before writing anything at all. The two
paths would still race each other, and the deadlock would remain.

**A `UNIQUE` violation mapped to 409 Conflict, with the client retrying.**
Rejected: it turns an internal ordering problem into an API contract every
product must implement, and the retry can itself lose to a third writer. It also
leaves the deadlock, which is not a conflict the client can resolve.

**Optimistic concurrency: `If-Match` on the version, 412 on mismatch.** Rejected
*for now*, not on merit — it is the right answer to the question "who wins when
two people edit the same paragraph", which this ADR deliberately does not
answer. It is an API contract change that belongs in the spec DocEditor does not
yet have (HR-20260913-001), and adding it here would be inventing the
requirement. The lock is compatible with it: if `If-Match` arrives later, the
lock is what makes the check-and-write atomic.

**A serializable isolation level.** Rejected: it converts the collision into a
`40001` the caller must retry, i.e. the 409 option with a less explicit
contract, and it applies to every transaction in the service rather than to the
two that need it.

## Consequences

- A writer can now wait on another writer of the same document. Bounded by one
  short transaction; no HTTP call, no broker call and no user think-time is ever
  inside the lock.
- `tests/concurrency_test.rs` makes the interleaving happen on purpose: a third
  transaction holds the row and is released only once PostgreSQL itself reports
  both writers queued behind it. The probe walks the blocking chain
  recursively — only the *first* waiter is blocked by the lock holder, the ones
  behind wait on that waiter's tuple lock, so counting direct waiters finds one
  writer however many are queued.
- Each half was mutation-checked: removing either `FOR UPDATE` turns exactly the
  matching test red (`duplicate key` for the update path, `deadlock detected`
  for the snapshot path), and leaves the other green.
- Any future write path that advances `current_version` must take the same lock
  on the same row first. The rule is "the document row is the lock for its own
  history".
