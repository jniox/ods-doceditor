# ADR-015 — A snapshot copies the body inside PostgreSQL, so its cost is the request's

- **Status**: accepted
- **Date**: 2026-09-15
- **Context**: `POST /api/v1/documents/{id}/versions`, a request of thirty
  bytes, and an OOM kill of the instance

## The measurement

`version_repo::create_version` locked the document row with

```sql
SELECT current_version, yjs_state, content
  FROM editor.documents
 WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL
 FOR UPDATE
```

read `content` into a Rust `String`, and bound that same `String` straight back
as a parameter of the `INSERT` one statement later. The body therefore crossed
the connection **twice** — out of PostgreSQL and back into it — and was held in
this process in between, for a request whose whole input is a document id and,
at most, a five-hundred-character comment.

Nothing in the request bounds that, and `MAX_DOCUMENT_SIZE_MB` does not either:
it is checked on bodies a caller **sends** (`DocumentService::validate_content`),
and ADR-009 deliberately leaves documents stored above it readable, renamable
and snapshottable.

That a 10 MB body is reachable is measured on the deployment, not inferred from
fixtures. **Neither live revision sets `MAX_DOCUMENT_SIZE_MB` at all**:

```text
$ gcloud run revisions describe doceditor-00003-vkq --format='value(spec.containers[0].env)'
  HOST RUST_LOG DB_SCHEMA JWT_ISSUER JWT_AUDIENCE ODS_ENV DATABASE_URL
  JWT_RSA_PUBLIC_KEY_B64 SERVER_HOST SERVER_PORT PUBSUB_TOPIC PUBSUB_TOPIC_DLQ
  GCP_PROJECT_ID EVENT_BUS          <- no MAX_DOCUMENT_SIZE_MB
```

so the revision carrying 100 % of traffic (`doceditor-00003-vkq`, May 2026 binary)
falls back to the **old default of 10 MB**, and the one carrying the new default
of 2 MB (`doceditor-00004-jon`) carries 0 %. Two further routes to the same
place: a stored body survives a lowering of the ceiling by design, and
`MAX_DOCUMENT_SIZE_MB` is an operator knob whose safe values ADR-009 derived
from the **save** path, which pays a different cost from this one.

Said plainly because the first draft of this ADR argued from the dev instance's
own contents: the two documents there above 2 MB are titled *ceiling probe* and
dated 2026-09-14 — they are lot 14's measurement fixtures, not traffic. A
fixture is not evidence about the service.

Measured on the running binary, release build, in a cgroup at the deployment's
own allocation (`systemd-run --user --scope -p MemoryMax=512M -p
MemorySwapMax=0`, `ops/cloudrun/doceditor.json`), N concurrent
`POST /documents/{id}/versions` against one document:

```text
  body     N     before                                   after
  10 MB   20     OOM KILL — 3-4 answers lost, /health gone  200 x20,  75 MiB,  4 413 ms
  10 MB   80     OOM KILL — 52 answers lost, /health gone   200 x80,  75 MiB, 28 705 ms
   8 MB   20     200 x20,  473 MiB,  9 684 ms               200 x20,  65 MiB,  2 942 ms
   2 MB   80     200 x80,  126 MiB,  8 972 ms               200 x80,  20 MiB
```

80 is not a stress figure: `ops/cloudrun/doceditor.json` sets no
`--concurrency`, so Cloud Run's own default of 80 applies, and that is the
number the deployment has to survive (ADR-009). At the published ceiling of the
revision that serves traffic today, **fifty-two of eighty callers got no answer
at all and the instance was gone** — on Cloud Run that takes every other
tenant's in-flight request with it, and one caller triggers it with two ordinary
calls.

This is the fourth time in this repository that a cost has been attached to a
quantity other than the one the operation's name promises — after the history
list (ADR-007), the metadata page (ADR-008) and the partial update (ADR-014) —
and the second time the quantity is *the stored document* rather than *the
request*.

## The decision

`version_repo::NewVersion` carries a `VersionBody`, and the two cases are named
apart because they cost differently:

- `VersionBody::Supplied { content, yjs_snapshot }` — document creation and
  content mutation. The caller sent the body, so it is already in this process
  and binding it costs nothing the request has not already paid.
- `VersionBody::OfDocumentAsItStands` — the explicit snapshot. The body is
  whatever `editor.documents` holds, and PostgreSQL copies it from one table to
  the other without it ever crossing the connection:

```sql
INSERT INTO editor.document_versions (…)
SELECT $1, $2, $3,
       coalesce(d.yjs_state, ''::bytea),
       $4, $5,
       octet_length(d.content) + coalesce(octet_length(d.yjs_state), 0),
       $6,
       d.content
  FROM editor.documents d
 WHERE d.id = $1 AND d.tenant_id = $2 AND d.deleted_at IS NULL
RETURNING …
```

`snapshot_size_bytes` moves into SQL with it. `octet_length` counts bytes of the
server encoding, which is exactly what `str::len()` counted before — the two
branches must agree on that, and the `Supplied` branch says so where it computes
it.

The locking read drops to `SNAPSHOT_LOCK_COLUMNS = "current_version"`, named as
a constant for the same reason `document_repo::UPDATE_LOCK_COLUMNS` is: a
projection written inline is a projection that grows a body column back.

**The lock itself has not moved.** Same row, same `FOR UPDATE`, taken before
anything is decided and in the same order as `document_repo::update_document` —
ADR-004 is untouched, and `tests/concurrency_test.rs` still covers a snapshot
racing a save. Only the projection changed, exactly as in ADR-014.

## Why RLS is not weakened by this

The `INSERT … SELECT` reads `editor.documents` under the same
`tenant_isolation` policy as every other read, in a transaction where
`app.tenant_id` is already set by `begin_tenant_tx`, and inserts into
`editor.document_versions` with `tenant_id = $2` — which is what the policy's
`WITH CHECK` tests. The explicit `d.tenant_id = $2` predicate is kept for the
same defense-in-depth reason every other statement in this repository keeps one.
`d.deleted_at IS NULL` is kept so the statement is self-sufficient about
liveness rather than relying on the caller having taken the lock first.

## The guard, and the instrument it needed

`tests/snapshot_cost_test.rs` asks **the wire**, not the code: the pool is
pointed at a counting TCP proxy inside the test process, which forwards to the
real PostgreSQL and tallies each direction.

The obvious measurement — peak RSS — is useless here, for the reason ADR-014
gave about `pg_current_wal_lsn()`: this suite runs thirty-one binaries in
parallel against one shared instance, so a process-wide number says as much
about the neighbours as about the code. Bytes on *this pool's own socket* is a
narrow-scoped fact that nothing else can move. When a property looks untestable,
the question is usually "is there a narrower-scoped instrument?".

The non-vacuity half is written first and asserted first: the same meter, on the
same pool, must **see** a body when one genuinely crosses — a read of the
document does exactly that. Without it, a meter that reads zero for everything
would make the real assertion pass while measuring nothing. Measured: the guard
fails on the previous code with *"taking a snapshot read 4 000 527 bytes back
from PostgreSQL for a 4 000 008 byte document"*, and passes after.

## What this does not buy

Said out loud, because a benefit overstated is how the next reader inherits a
false premise (ADR-014 said the same about its own WAL gain).

- **The reads that return a body are untouched and are still close to the cap.**
  `GET /api/v1/documents/{id}` on a 10 MB document, twenty at once in the same
  512 MiB cgroup, peaked at **449–512 MiB** both before and after this change.
  It survives — nothing was killed — but there is no headroom, and
  `GET …/versions/{n}` has the same shape. That cost is the response itself and
  the contract requires it; bounding it is a product decision, not a repair.
- **Nothing here bounds concurrency**, and that remains open exactly as ADR-009
  left it. This change makes one endpoint's cost independent of the stored
  document; it does not make the service safe at any concurrency.
- **The save path is unchanged.** A `PATCH` carrying a body still sends it
  twice — once to `editor.documents`, once to `editor.document_versions` — and
  that is bounded by the payload ceiling, which is the quantity its name
  promises. ADR-009's sizing measurement stands as taken.
