---
name: doceditor-batch-20260914-lot17
description: Lot 17 — the first BA report in thirteen with something code-actionable, and it was mis-attributed; the guard that names one crate, the list that loses a document, and a fixture that faked the measurement taken to justify it
metadata:
  type: project
---

Lot 17 (`6627534` → `5888639`), **twentieth** dev turn on unit
`doceditor-c20260909-1345`, new branch `feat/doceditor-c20260909-1345-lot2`
(PR #3 was merged, so lot 1's branch was closed), **PR #4**. 200 tests (192
before), 30 binaries, CI 5/5 — including the new `Advisories` job.

**The first report in thirteen cycles with a code-actionable finding**, and the
useful part of the turn was in *re-measuring the finding* rather than in
accepting it. The BA wrote: RUSTSEC-2026-0285 "introduced by this lot's new
reqwest+rustls-tls dependency chain". One command:

```text
git show bce825b^:Cargo.lock | grep -A2 '^name = "rustls"'   ->  0.23.40
```

`rustls` was already there, through `sqlx`'s `tls-rustls`, since the service was
written. **The dependency is old; the advisory is one day old** (published
2026-09-14). That inversion is the whole batch: if a dependency decision had
caused it, a dependency decision would prevent the next one. Nothing prevents
the next one but a standing measurement — and there was none. `tests/framework.rs`
names `h2` 0.3; `tests/dependencies.rs` finds unused crates; neither asks
whether a crate we *compile* carries an advisory. The finding reached the
estate because a human ran `cargo audit` by hand.

**The doctrine already existed in a commit message and nowhere executable.**
`735647d` (lot before): *"Judged on the DELIVERED graph per BR-0010, not on
`cargo audit`'s tally"* — and it was right, measured again here: of the four
findings `cargo audit` reports, only `rustls` is in `cargo tree -e normal`.
`rsa` (RUSTSEC-2023-0071, **no fixed release, ever**) arrives only through
`sqlx-mysql`, which `sqlx = { features = ["postgres"] }` does not build. A
check that failed the build on `rsa` would be switched off within a week, and
the next `rustls` would pass unseen. Hence `scripts/audit-delivered-graph.sh`
(CI job `Advisories`, push **and** pull_request) + `tests/advisories.rs`, which
guards offline everything the job cannot: that the workflow calls it, and the
classification itself on fixtures — including two cases that exit **2**, because
nothing measured is not a pass.

**BR-0010 does not exist.** `~/dev/specs/ods-platform/context/business-rules.md`
holds 0001, 0003, 0004, 0006, 0007, 0008, 0009, 0013 — nothing between 0009 and
0013, and no `### BR-0010` anywhere under `~/dev/specs`. This repository has
cited it since `735647d` and `CLAUDE.md` cites it too. Stated locally in ADR-012
and reported; injecting a business rule is a cockpit act (BR-0002). *Check a
citation before propagating it — I nearly wrote it into three new files.*

**Then the path-reading half, and this one is the better find.**
`ORDER BY updated_at DESC` is not a total order, and PostgreSQL **does not pick
the same plan for every page of one list**:

```text
LIMIT 100 OFFSET 0     ->  Index Scan using idx_documents_tenant_updated
LIMIT 100 OFFSET 1900  ->  Sort (Sort Key: updated_at DESC)
walk of all 20 pages   ->  2000 rows, 1999 distinct
                           tie-00410 twice, tie-00801 never returned
```

One document lost to a client that walks pages, every response ordinary. Fixed
with `, id DESC`. **And the measurement that refuses the alarming reading**:
7 178 documents on this instance, **zero ties** — the API writes one document
per transaction, so the list was stable by a property of the traffic rather than
of the query. A single bulk write (seed, import, migration) removes it. Said in
ADR-013 and in the commit, because a fix whose harm is conditional must say so.

**Three traps worth keeping:**

1. **The first draft of the guard was green against the defect it was written
   for.** It compared the two plans over the *whole* list (no `LIMIT`) and they
   agreed. The instability is produced by the **bounded sort**, i.e. by the
   page. What saved it was writing the non-vacuity assertion *first* and seeing
   it fail. A guard whose fixture cannot express the defect is worse than none.
2. **Do not wait for the planner to change its mind in a test.** The switch
   point is a cost estimate, so a test that grew the table to 2 000 rows would
   go green vacuously on a smaller or fresher database. Two pools pinned to the
   two plans (`enable_seqscan=off` / `enable_indexscan+bitmapscan+indexonlyscan=off`)
   — the technique `search_index_test.rs` already used.
3. **My own fixture faked the measurement that justified the fix.** Asking "does
   `updated_at` tie in real data?" returned four tenants × 40 tied rows — they
   were the new test's own fixtures, seeded under *random* tenants by earlier
   runs of the same hour. 480 rows removed by hand; the test now uses two
   **fixed** tenants and sweeps them **before** seeding (the run that leaves
   rows is the failing one, which is the run a reviewer repeats) and clears them
   after. Exactly the `events_roundtrip` topic-sweep shape, one storage layer
   over.

**Mechanics:**

- `clippy::unusual_byte_groupings` rejects a leetspeak UUID constant
  (`0x0d0ce_d17_…`). Groups of four. Caught locally, after `cargo test` was
  already green — **`cargo test` passing is not `clippy -D warnings` passing.**
- `cargo audit --json` exits non-zero when it finds anything, so `set -e` in a
  wrapper script hides the report. The script uses `set -uo pipefail` only.
- The `tests/ci_workflow.rs` guard requires **every** job whose body contains
  `cargo ` to install the Dockerfile's apt list. The new job does, rather than
  the guard being narrowed — narrowing an existing guard to fit a new job is how
  guards die.
- The **idle watchdog struck again**, committing the scenario agent's
  `tests/e2e/*.sql` as `wip: auto-commit … (idle 34386s)` between two of my
  commits. `git reset origin/<branch>` undid it, local-only. It was caught
  because `git rev-parse --short HEAD` returned a sha I did not recognise while
  naming an evidence folder. **Check `git log`, not `git status`.**

**Left open and reported, not fixed:** AC-012's remaining half — re-measured
live this turn, 100% of traffic still on `doceditor-00003-vkq` (May 2026), the
fixed `doceditor-00004-jon` Ready at **0%**. A traffic promotion, i.e. a
deploy-agent act.

See [[go-read-the-path-yourself]] for the running checklist, and
[[doceditor-batch-20260914-lot16]] for the transport batch whose dependency this
one had to re-measure rather than believe.
