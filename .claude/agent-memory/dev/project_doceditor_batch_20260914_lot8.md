---
name: doceditor-batch-20260914-lot8
description: Lot 8 — four defects on one seam (input normalised in one layer, reported in another), the value-that-travels fix, and the actix `use test` trap that shadows #[test]
metadata:
  type: project
---

Lot 8 (`4ab80e2` → `31f2fc8` → `e06e653`), **eleventh** dev turn on unit
`doceditor-c20260909-1345`, still PR #3. 95 tests (87 before). CI `34797237885`,
4/4 green on the exact pushed sha.

**Fourth consecutive BA report with nothing code-actionable** — 18 MET, AC-000
(no `spec.md`, `HR-20260913-006` still `PENDING`) and AC-012 (broker env vars
absent from `~/dev/ops/cloudrun/doceditor.json`). Re-measured in five minutes,
transcripts in `evidence/e06e653/06`, deliberately untouched. So this turn went
looking, like lots 5–7, and found **four** defects sitting on one seam.

**The seam, and it is the generalisable part: a value normalised in one layer
and reported in another.** `GET /api/v1/documents` clamped in `DocumentService`
(`page.max(1)`, `per_page.clamp(1, 100)`) and echoed `query.page` /
`query.per_page` in the handler. Nothing was red, and could not be: the existing
clamp test calls the **service**, which is the one vantage point from which the
response is invisible. **When a test exercises the layer that normalises, it
cannot see what the layer above reports.** Ask, of every normalisation: who
tells the caller, and does that code read the same value?

1. `?per_page=1000` answered `"per_page": 1000` above at most a hundred rows —
   the published contract says `maximum: 100`. `?page=0` answered `"page": 0`
   above the first page. A client computing `ceil(total / per_page)` sees one
   page; one walking `0, 1, 2` reads the first page twice. **No error either
   way**, which is worse than a refusal: there is nothing to notice.
2. `(page - 1) * per_page` overflowed. `page=i64::MAX` → `attempt to multiply
   with overflow` on debug (`document_repo.rs:78`); on the **release** build the
   Dockerfile produces (no `[profile]` section → `overflow-checks = false`) it
   wraps to `OFFSET -200`, and PostgreSQL answers `ERROR: OFFSET must not be
   negative` → a 500. Both halves measured, the second by hand in psql.

**Both closed by one type rather than two patches.** `domain::pagination::
Pagination` is built at the boundary, travels into the repository, and renders
the response; `offset()` is `saturating_mul`. Deliberately **not** a second
clamp at the edge — a second clamp is a second thing to forget, and the defect
was precisely two places disagreeing. Any new paginated endpoint builds one.

3. `validate_metadata` states every BR-029 rule about **keys**, so
   `as_object()` answering `None` skipped all of them and returned `Ok`: a
   string, a number, a boolean, an array went into the `jsonb` column
   unvalidated, in a field the contract types `object`. Now 422, checked first.
4. An unknown `?status=` filtered nothing and answered `200` with an empty page
   — the one reply that is both wrong and plausible ("you own no documents").
   Now 400, as `PATCH` always was for the same word.

3 and 4 share a shape worth carrying: **a check that says nothing about the
inputs it was not shaped for is not a check.** Grep for `if let Some(x) = …`
guards whose `else` is silence.

**Method that paid again:** the four red assertions were written *before* any
fix (the red run is `evidence/01`), and the **mutation matrix is a clean
partition** — reverting any one of the four turns exactly one test red and
leaves the other three green. That partition is what proves each test guards its
own fix and none is vacuous. Plus a non-vacuity case inside each test (an
in-range request echoed unchanged, a well-formed metadata object accepted, the
three documented statuses served).

**Trap that cost ten minutes: `use actix_web::{test, …}` shadows the built-in
`#[test]` attribute.** The imported name is both a module and an attribute
macro, so a plain `#[test] fn` in an integration test file fails with *"the
async keyword is missing from the function declaration"* — an error that says
nothing about the real cause. Put synchronous unit tests in the `src/` module's
own `#[cfg(test)] mod tests` (where they belong anyway), or spell
`#[::core::prelude::v1::test]`.

**Two smaller things worth not re-deriving.** A trailing space in a test URI
(`?status=published `) panics inside actix's test builder with
`InvalidUri(InvalidUriChar)` — percent-encode it (`published%20`). And
`docs/openapi.yaml` was already **right** about `Metadata: type: object`; the
code was wrong. When contract and code disagree, check which one is the defect
before editing the contract.

See [[doceditor-batch-20260914-lot7]] for the RLS posture this lot's boot log
re-confirms (`enforced … role=editor_app`), [[doceditor-batch-20260913]] for
lots 1–5 and the habit of reading a path rather than a report, and
[[h2-advisory-campaign]] for why PR #3 keeps re-opening this unit — **eleven dev
turns now; merging is the `pr` agent's gesture.**
