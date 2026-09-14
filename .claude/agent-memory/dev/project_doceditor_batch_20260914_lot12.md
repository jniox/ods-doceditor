---
name: doceditor-batch-20260914-lot12
description: Lot 12 — the endpoint whose response was always correct and whose query was not; an OOM kill rather than a 500, and the half of the fix that measurement refused to credit
metadata:
  type: project
---

Lot 12 (`f4b7586` → `f73f993`), **fifteenth** dev turn on unit
`doceditor-c20260909-1345`, still PR #3. 130 tests (125 before), 23 binaries.

**Eighth consecutive BA report with nothing code-actionable** — 18 MET, AC-000
(no `spec.md`, 13th cycle) and AC-012 (`cloudrun/doceditor.json` sets no broker
variable). Both re-verified in five minutes from the HR JSON files
(`HR-20260913-001` still `DISPATCHED` with a resolution and no execution,
`HR-20260913-006` still `PENDING`/`resolution: null`) and from
`business-rules.md` (8 BRs, zero match for `doceditor`/`editor`). Untouched on
purpose — BR-0002, and that includes the three-valued topic name, which
HR-20260913-001's own resolution assigns to the cockpit. Then read a path, as in
lots 5–11.

**The defect: the RESPONSE was right and the QUERY was not.**
`GET /documents/{id}/versions` renders numbers, dates, authors, comments, sizes;
`docs/openapi.yaml` says *"Bodies are not included"*; the handler renders exactly
that. `version_repo::list_versions` selected `content` for every version anyway
and the handler dropped it one layer up. The history is **unpaginated by
contract**, so the projection was the only thing bounding the cost — and it
bounded nothing.

Measured on the running binary in a cgroup set to the deployment's own
allocation (`ops/cloudrun/doceditor.json`: `memory 512Mi`;
`MAX_DOCUMENT_SIZE_MB`: 10):

| | |
|---|---|
| 25 versions × 1 048 560 B | read 25 MB from PostgreSQL, served **4 268 B**, RSS 35 348 → 51 420 kB |
| 55 versions × 10 484 720 B | `Remote end closed connection`, 0.7 s, systemd `Result=oom-kill`, `MainPID=0` |
| after the fix, same request | **`200`, 9 423 B, 55 ms, 6.3 → 7.1 MiB**; ten in a row in 0.1 s |

**An OOM kill is not a failed request** — on Cloud Run the *instance* dies, so
every other tenant's in-flight request dies with it, and one caller triggers it
with two ordinary calls (a PATCH loop, i.e. autosave, then one history read).

**Why eleven review cycles saw nothing, and the test that says so.** One of the
five new tests — `the_history_response_carries_no_body_over_http` — **passed
before the fix**. The answer was always correct; the cost is invisible from the
vantage point of the answer and only visible from the query. Lot 9 taught "test
from where the caller stands"; this is its complement: **some defects are
invisible from there, and the question to ask of an endpoint is not only what it
returns but what it had to read to return it.**

**The fix is the split that already existed one module over.** `document_repo`
separates `DOC_COLUMNS` from `SUMMARY_COLUMNS` *and explains why in a comment*;
`version_repo` had one column list. Now `VERSION_COLUMNS` (only `get_version`,
whose purpose is to hand a body back) and `VERSION_SUMMARY_COLUMNS` (the list,
and `insert_version`'s `RETURNING`), with `domain::document::DocumentVersionSummary`
carrying no `content` field at all — a type, for the reason `Title` and
`Pagination` are types. Exactly lot 6's shape: a rule honoured at one call site
and absent from its neighbour.

**The half measurement refused to credit, and keeping it anyway.**
`insert_version` used to `RETURNING … content`, so all three write paths read the
body back out of PostgreSQL right after writing it, for callers that never look
at it. Hypothesis: saves get cheaper. A/B, same host, same cap:

| | mean latency, 10 × 10 MB PATCH | 6 concurrent 10 MB saves | 10 concurrent |
|---|---|---|---|
| body-carrying `RETURNING` | 2 635 ms | 200 ×6, peak **433 MiB** | — |
| summary `RETURNING` | 2 563 ms | 200 ×6, peak **418 MiB** | **oom-kill** |

Within noise on latency, 15 MiB on memory, and it does **not move the
concurrency at which a 512 MiB instance dies**. Kept (a value no caller reads
should not travel; it is what lets `create_version` return a summary) and
**said so in the ADR, the commit and the evidence** rather than glossed. Lot 10
recorded a falsified hypothesis *before* coding; this one was falsified *after*,
and the discipline is to publish it either way.

**The second finding, escalated instead of guessed — HR-20260914-001.**
Measuring that A/B produced a distinct hazard: `MAX_DOCUMENT_SIZE_MB=10`,
`memory: 512Mi` and an unbounded request concurrency only make sense together
and nothing relates them. **6 concurrent full-size saves survive (418 MiB), 10
kill the instance**, with or without the fix; raising the ceiling to 50 MB would
make **two** fatal. Four plausible remedies (smaller ceiling / more memory /
Cloud Run `--concurrency` / in-process admission limit), three of them in a file
outside this repo, none settled by any spec → human review with the numbers,
`enactor` declared per option, and status **DONE**, not BLOCKED.

**Traps worth reusing:**

- `systemd-run --user --unit=… -p MemoryMax=512M` reproduces a Cloud Run memory
  limit exactly, and `systemctl --user show … -p Result` says `oom-kill` in as
  many words. A `nohup systemd-run --scope &` does **not** survive; a transient
  `--unit` service does.
- `cp` onto a running binary fails with **`Text file busy`**, and the A/B then
  silently measures the *old* binary twice. Stop the unit before copying — the
  first A/B run produced two identical numbers and that is what gave it away.
- `cargo` is not on `PATH` in this shell; `export PATH="$HOME/.cargo/bin:$PATH"`.
- An existing test read bodies *from the list* (`concurrency_test.rs`), so the
  fix broke it — correctly. Rewriting it to fetch each body through
  `get_version` also changed the **order** (`version_numbers` sorts ascending,
  the list is `version DESC`) and flipped a last-writer assertion. Read what a
  helper's order is used for before swapping its source.
- Cleaned 116 probe/fixture documents (**649 MB** of bodies) out of the shared
  `editor` schema; 3 716 rows / 4 062 kB left.

See [[go-read-the-path-yourself]] — eighth turn in a row.
