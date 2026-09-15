---
name: doceditor-batch-20260915-lot19
description: Lot 19 — a thirty-byte request whose cost was the stored document, an OOM kill at the platform's own concurrency, the counting TCP proxy that made it testable, and a premise I had to withdraw because my own fixtures were the evidence
metadata:
  type: project
---

**The turn.** Fourteenth BA report in a row with nothing code-actionable (31 MET,
1 PARTIAL, 0 MISSING; the PARTIAL is AC-012, a traffic promotion). Method from
[[go-read-the-path-yourself]]: re-measure the non-code deviation, then read a
path.

**The defect.** `POST /documents/{id}/versions` carries a document id and at most
a 500-character comment — 24 bytes of body in the measurement. Its cost was set
by the **stored** document: `create_version` locked the row with
`SELECT current_version, yjs_state, content … FOR UPDATE`, read the body into a
`String`, and bound that same string back as a parameter of the `INSERT` one
statement later. The body crossed the connection twice and sat in the process in
between.

Nothing in the request bounds that, and **`MAX_DOCUMENT_SIZE_MB` does not
either** — it is checked on what a caller *sends*, and ADR-009 deliberately
leaves already-stored larger bodies snapshottable. Measured on the running
binary in a 512 MiB cgroup (the deployment's allocation), N concurrent snapshots
of one document, 80 being Cloud Run's default concurrency:

```text
  10 MB N=20 -> oom-kill, 3-4 answers lost, /health gone   |  after: 200 x20, 75 MiB
  10 MB N=80 -> oom-kill, 52 answers lost of 80            |  after: 200 x80, 75 MiB
   8 MB N=20 -> 200 x20, 473 MiB, 9 684 ms                 |  after: 200 x20, 65 MiB
   2 MB N=80 -> 200 x80, 126 MiB                           |  after: 200 x80, 20 MiB
```

Fourth batch running in which a cost is attached to a quantity other than the one
its name promises (ADR-007, ADR-008, ADR-014, now ADR-015), and the second in
which the quantity is *the stored document*.

**The fix.** `NewVersion` carries a `VersionBody`: `Supplied` (creation, content
mutation — the caller sent the body, binding it costs nothing new) and
`OfDocumentAsItStands` (the snapshot — `INSERT … SELECT … FROM editor.documents`,
the body never leaves the server; `snapshot_size_bytes` moves into SQL as
`octet_length`, which counts the bytes `str::len()` counted). The locking read
drops to `SNAPSHOT_LOCK_COLUMNS = "current_version"`. **The lock did not move** —
same row, same `FOR UPDATE`, same order as `update_document`; ADR-004 untouched,
`concurrency_test.rs` still green.

## Three things worth keeping

**1. The instrument, again, decided whether the guard could exist.** Peak RSS is
useless here for the same reason `pg_current_wal_lsn()` was useless in lot 18:
31 test binaries share one instance. The narrow-scoped fact is *how many bytes
crossed this pool's own socket*, and that is exactly measurable — point the pool
at a counting TCP proxy inside the test process, forwarding to the real
PostgreSQL. Nothing another test does can move it. Generalise: **when a property
looks untestable, ask "is there a narrower-scoped instrument?" before asking
"can I bound the noise?"** And write the non-vacuity assertion first — here, the
same meter must *see* a body when a read genuinely carries one, or a meter stuck
at zero would make the real assertion pass while measuring nothing.

**2. I had to withdraw my own premise, and that is the part to remember.** The
draft argued reachability from the dev instance: *"six documents there exceed the
2 MB ceiling"*. After deleting this turn's fixtures, **two** were left — titled
`ceiling probe`, dated 2026-09-14: **lot 14's own measurement fixtures**. Four of
the six had been mine. A database full of agents' leftovers looks exactly like a
database full of traffic. The argument that holds is a *deployment* fact:
`gcloud run revisions describe` shows **neither live revision sets
`MAX_DOCUMENT_SIZE_MB`**, so `doceditor-00003-vkq` (100 % of traffic, May 2026
binary) runs at the old 10 MB default and the revision carrying HR-20260914-001's
2 MB carries 0 %. Same trap as lot 17's *"check your own fixture before you
report the service"*, one level up: **check your own fixture before you report
the world.**

**3. The check for a taken-but-unrouted decision came back negative, and that is
information too.** Lots 14 and 16 each found an `HR-*.json` the report called
pending that was in fact decided with a dispatcher `enactError`. Here
HR-20260915-003 is genuinely `PENDING` — no `resolution`, no `enactError`. Run
the check every time; do not assume either answer.

## What the repair does not buy — published, per [[go-read-the-path-yourself]]

- **The reads that return a body are untouched and have no headroom.**
  `GET /documents/{id}` on a 10 MB document, 20 at once: **449–512 MiB** before
  and after (run-to-run variance on identical code). It survives at the cap, and
  `GET …/versions/{n}` has the same shape. That cost is the response; bounding it
  is a product decision.
- **Nothing bounds concurrency.** ADR-009's options C and D stay open.
- **The save path was deliberately left alone.** A `PATCH` with a body still
  sends it twice; that is bounded by the payload ceiling — the quantity its name
  promises — and ADR-009's sizing measurement, the evidence under a human
  decision, was taken against exactly that path. Changing it in passing would
  have invalidated the measurement for a gain nobody asked for.

See [[doceditor-batch-20260915-lot18]] for the sibling finding on the write path,
and [[doceditor-test-database]] for why the suite runs where it runs.
