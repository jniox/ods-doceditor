---
name: doceditor-batch-20260913
description: The two 2026-09-13 batches that closed the BA FAILs — what shipped, why the second one mattered more, and the two questions left to a human (HR-20260913-001)
metadata:
  type: project
---

On 2026-09-13, **two** BA FAILs were closed in succession on `feat/doceditor-c20260909-1345-lot1`.
Lot 1 (`bf5c29f` → `735647d`): 12 of 13 actionable findings, 58 tests (baseline 17).
Lot 2 (`735647d` → `d93d51d`): the 4 remaining findings — **and CI went green for the first time
in the repository's history** (run `34731214292`, 4/4 jobs, 70 tests).

**The second lot is the one to remember, and not for what the BA asked.** Fixing the HIGH finding
(CI could not compile: `libcurl4-openssl-dev` missing from the `lint`/`test` apt lists) let CI reach
the tests *for the first time ever* — and CI immediately found a defect no reviewer could have:
`CREATE SCHEMA IF NOT EXISTS` is not safe against itself, and `src/main.rs` ran it at startup, so
two Cloud Run instances booting together on a virgin database would have crash-looped one. See
[[doceditor-test-database]]. **Repairing the thing that measures is worth more than the findings it
was measuring** — budget for the backlog to come out when a long-red gate finally opens.

**Two questions are open and belong to a human, not to a dev turn — `HR-20260913-001` (product).**
If a later turn is tempted to "just fix" either of them, don't; check the review's state first.

1. **doceditor has no `spec.md`, and no PDLC handoff** (11 other services have one). Every BA cycle
   re-derives acceptance criteria from `pdlc/gtm/doceditor-gtm.md`, which declares its own
   inference at line 224. Three cycles have now reported the same gap, and the BA itself noted that
   re-derivation produced *different* findings each time. Writing the spec means inventing the
   requirement — that is scoping's job. `docs/openapi.yaml`, published in this batch, gives whoever
   writes it the exact delivered surface.
2. **The event topic has three contradictory names**: `editor.events` (repo CLAUDE.md and the
   current code default), `ods.editor.events` (GTM brief, named four times as what products
   subscribe to), `doceditor-events` (global platform rule `{service}-events`). Publishing to the
   wrong one is **silent** — the service emits, nobody consumes, nothing fails. The code was
   deliberately left on the repo's own default.

**What changed in the service's shape**, so a later turn does not re-derive it from scratch:
documents now carry a `content` body, creation writes version 1, and any content change advances
the version and writes its immutable snapshot in the same transaction. `yjs_state`/`yjs_snapshot`
stay reserved for the unbuilt collaborative CRDT layer — **do not store text in them**, that was
the temptation the batch refused. A real `RedpandaProducer` replaced `NoopProducer`, which had
been quietly discarding every event since the service was written.

**Two defects found by doing rather than by reading**, both worth repeating as method:

- `editor.templates` had RLS enabled and **no policy at all**, because migration 005's
  `pg_policies` check was not schema-qualified and matched `securemail.templates` on the shared
  instance. Writing the assertion found it; re-reading the migration had not, twice.
- The correlation middleware's `span.enter()` was held across an `.await` and no event was ever
  logged under the span, so the correlation id reached the response header and the events but
  never the logs. Found by **running the service and grepping for an id I had just sent**. The
  tests could not see it — they assert on the header and on the events, both correct.

**Lot 2 also removed two things that had been read as capabilities:** `actix-cors` *and*
`actix-rt` (the latter found by the guard, missed by the review and by lot 1's hand-made dependency
cleanup), and `AppError::Forbidden`/`Conflict` — a 403 and a 409 no branch could produce, already
copied into the published contract's `Error.error` enum. Both were replaced by guards rather than by
care. `docs/adr/` holds three ADRs now, each carrying the rejected alternatives.

**Method that paid twice over, worth repeating:** every guard was written with a companion
non-vacuity test, and *both* earned their keep immediately — the CI-workflow guard's caught a bug in
my own parser that made the real assertion pass while inspecting zero jobs; the error-surface guard's
second direction turned red on `["conflict", "forbidden"]` the instant the variants were removed,
before the yaml was touched.

See [[doceditor-test-database]] for the shared-instance traps and the fresh-database class of defect,
[[h2-advisory-campaign]] for why PR #3 keeps re-opening (**still open after lot 2 — merging is the
`pr` agent's gesture, and this unit has now cost five dev turns**).
