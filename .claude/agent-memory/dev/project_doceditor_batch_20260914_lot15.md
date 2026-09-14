---
name: doceditor-batch-20260914-lot15
description: Lot 15 — the query string and the path were the last inputs nobody parsed; seven answers outside the contract's closed error enumeration, a blank search field that answered "you own nothing", and a fix deliberately narrowed after the suite caught it overreaching
metadata:
  type: project
---

Lot 15 (`1e57f7a` → `69d7ee2`), **eighteenth** dev turn on unit
`doceditor-c20260909-1345`, still PR #3. 170 tests (158 before), 26 binaries.
Eleventh BA report in a row with nothing code-actionable (31/32 MET).

**The turn's method, confirmed again.** `business-rules.md` read whole — **no
`BR-xxxx` mentions doceditor at all**, so no criterion is contradicted by an
injected decision and there is no spec defect to report on that ground. Every
`HR-*` cited was reopened as JSON (lot 14's lesson): HR-20260914-001..004 all
`DONE`, nothing taken-but-unexecuted this time. So the work came from **reading
a path**, and it was on the one input nothing parsed: the **query string** and
the **path parameters**.

**Finding 1 — seven answers outside a closed enumeration.** `docs/openapi.yaml`
publishes one error body and AC-031 says the enumeration is exact. Measured on
the running binary: `?page=abc`, `?per_page=5.5`, an `int64` overflow and
`?page=` answered `400 text/plain "Query deserialize error: …"`;
`/documents/not-a-uuid` and `…/versions/abc` answered `404 text/plain`; an
unmatched route answered `404` with **no body and no content-type at all**.
Exactly lot 9's seam, two extractors over — and invisible for the same reason:
`tests/error_surface.rs` reads `src/error.rs` and the contract, **never the
wire**. A test that compares two *sources* cannot see a response produced by the
framework.

**Finding 2 — one gesture, four answers.** `?page=&per_page=&status=&search=` is
what a form submits untouched. `page`/`per_page` → `400 text/plain`; `status` →
`400 application/json`; `search` → **`200` with an empty page**, i.e. a tenant
owning three documents told it owns none. The fourth is the worst because it is
the only one with no error code anywhere — the same reading lot 13 refused for
`?status=bogus`.

**The cure, in the repo's own idiom.** `api::payload::limits()` — the one
function `main.rs` and the tests both call — now installs `PathConfig`,
`QueryConfig` and a `default_service` beside the two ceilings (statuses
unchanged, only the body). `domain::query::supplied` states "a blank value is an
absent value" **once**, and it is not invented: `api::middleware::correlate`
already applies it to headers. `page`/`per_page` therefore arrive as **strings**
and go through `Pagination::parse`, whose refusal names the parameter instead of
quoting serde.

**The part worth remembering: the suite made the fix smaller.** The first
implementation trimmed every parameter, and `list_contract_test.rs` went red —
`?status=published%20` must stay a `400`, because that test picked a trailing
space *deliberately* as "the realistic typo". So blankness is judged after
trimming while the value travels **exactly as sent**. Generalising a
normalisation is how you overturn a neighbouring decision in silence; the guard
against it is running the whole suite before believing a repair.

**A hypothesis measurement refused, published anyway.** Lot 13 measured one
worst-case page (100 docs × 32 KiB metadata → 23 MiB); the platform serves 80 at
once in 512 MiB, so 80 of those should kill the instance as the heaviest *write*
did before lot 14. Bench (same `systemd-run --user -p MemoryMax=512M` shape):
N=1 → 17.5 MiB, N=10 → 69.6, N=40 → 90.5, **N=80 → `200` ×80, peak 94.8 MiB,
`Result=success`**. False: the 20-connection pool serialises the expensive half.
Lot 13 had bounded it correctly.

**Re-measuring a "non-code" deviation produced the turn's other find.** D-1 says
the descriptor sets no `REDPANDA_*`. True — and half the story: the **live**
Cloud Run revision (`doceditor-00003-vkq`, May 2026) carries `EVENT_BUS=pubsub`,
`PUBSUB_TOPIC=editor-events`, `PUBSUB_TOPIC_DLQ` and `GCP_PROJECT_ID`, four
variables **no line of `src/config.rs` reads** and which are **absent from
`~/dev/ops/cloudrun/doceditor.json`** (descriptor drifted from the revision). The
deployment already chose Pub/Sub; the code never learned. Opened
**HR-20260914-007** (architecture, three options, `enactor` on each) rather than
filing it a fourth time. Not implemented: replacing the producer is a platform
decision (spec §4.3).

**Traps and mechanics:**

- `use actix_web::test` **shadows the `#[test]` attribute** in that file: a plain
  `#[test] fn` fails to compile with "the async keyword is missing". Declare it
  `#[actix_web::test] async fn`, or move it into a nested module.
- `pgrep -f "/tmp/lot15/ods-doceditor"` **matches the shell running it** just as
  `pkill -f` does (lot 14's trap, one tool over): the command line contains the
  pattern. Killing by that list killed the invoking shell (exit 144). Anchor the
  pattern (`ps -eo pid,args | grep -E "^ *[0-9]+ /path"`).
- A Python file named `token.py` shadows the stdlib `token` module and breaks
  `import jwt` with a circular-import error. Name probe scripts anything else.
- `web::ServiceConfig::default_service` exists in actix-web 4.13, so the
  unmatched-route answer fits in the same `configure()` call as the extractor
  configs — and it covers routes inside a `web::scope` too (measured: both
  `/api/v1/nope` and `/nope`).
- `PathError`/`QueryPayloadError` are `#[non_exhaustive]`; build the message from
  `to_string()` and strip the framework prefix rather than matching variants.
  Note `PathError::status_code()` is 400 but the extractor answers 404 when no
  handler is set — with a handler, the status is whatever you return.
- Broker hygiene: 14 `probe-*` topics left by lot 14's hand probes were still
  there, outside the `doceditor-roundtrip-*` prefix the suite sweeps. Deleted by
  name. **A sweep only cleans what its prefix matches** — hand probes need their
  own cleanup, in the same turn.

See [[go-read-the-path-yourself]] for the running checklist this extends.
