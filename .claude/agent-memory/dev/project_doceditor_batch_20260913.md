---
name: doceditor-batch-20260913
description: The three 2026-09-13 batches that answered the BA FAILs — what shipped, why lot 3 had nothing code-actionable to fix, and the two questions still left to a human (HR-20260913-001)
metadata:
  type: project
---

On 2026-09-13, **three** BA FAILs were answered in succession on
`feat/doceditor-c20260909-1345-lot1`.
Lot 1 (`bf5c29f` → `735647d`): 12 of 13 actionable findings, 58 tests (baseline 17).
Lot 2 (`735647d` → `d93d51d`): the 4 remaining findings — **and CI went green for the first time
in the repository's history** (run `34731214292`, 4/4 jobs, 70 tests).
Lot 3 (`13e1e2f` → `329f548`): **nothing in the report was code-actionable.** 73 tests, CI
`34733129191` green 4/4.

**Lot 3 is the one to read before the next dispatch, because its shape will recur.** All four
deviations were non-code and the BA said so itself — its first recommendation was *"no dev cycle
needed — there is no remaining code-level finding"*. Two belong to a human (`HR-20260913-001`,
still `PENDING`), one to devops (`REDPANDA_BROKERS` absent from `~/dev/ops/cloudrun/doceditor.json`,
**outside this repo**, and platform-wide), one to ops (the `ods` role is `SUPERUSER`+`BYPASSRLS`,
re-measured `t | t`). **Re-measure all four and put the transcripts in the evidence folder** — that
is what justifies not touching them, and it takes five minutes.

**What to do with such a turn instead of nothing:** the BA re-derives its criteria from
`pdlc/gtm/doceditor-gtm.md`, and that brief carries a *Known Limitations* list with per-item
**owners**. Lot 3 delivered #2 ("verify at least one integration test publishes and reads a document
lifecycle event from a Redpanda instance", owner **Code Agent**, open since 2026-05-02). That is
in-scope work with a citable source, unlike inventing the missing spec. Check that list first.

**The event round trip, and why `assert!(publish().is_ok())` was never evidence.** `send_result`
enqueues and returns, so the `Ok` the caller gets is an *enqueue receipt*; delivery is confirmed
later on a detached task and its failure is only logged. **A `NoopProducer` returns the identical
`Ok`.** `tests/events_roundtrip.rs` publishes through the real producer and **consumes back**.
`broker_addr()` in `tests/common/mod.rs` has **no skip path** — the obvious guard (skip when no
broker) reproduces the defect one storey up: green everywhere there is no broker, i.e. everywhere.
CI therefore starts the broker itself with `docker run`, **not** a `services:` container — Actions
gives no way to pass a command to one and `redpanda start` needs its listener flags.

**What changed in the service's shape** (lots 1–2), so a later turn does not re-derive it:
documents carry a `content` body, creation writes version 1, and any content change advances the
version and writes its immutable snapshot in the same transaction. `yjs_state`/`yjs_snapshot` stay
reserved for the unbuilt collaborative CRDT layer — **do not store text in them**. A real
`RedpandaProducer` replaced `NoopProducer`, which had been discarding every event since the service
was written. Lot 2 also removed `actix-cors`, `actix-rt`, and `AppError::Forbidden`/`Conflict`,
each replaced by a guard rather than by care. `docs/adr/` holds three ADRs, each carrying its
rejected alternatives — **read ADR-002 before "fixing" the missing broker config: it rejected
failing startup on purpose**, and re-deciding it here is what BR-0002 exists to prevent.

**Two questions are open and belong to a human — `HR-20260913-001` (product), `PENDING` as of
lot 3.** If a later turn is tempted to "just fix" either, don't; check the review's state first.

1. **doceditor has no `spec.md` and no PDLC handoff** (11 other services have one). Every BA cycle
   re-derives criteria from the GTM brief, which declares its own inference at line 224.
   `docs/openapi.yaml` gives whoever writes the spec the exact delivered surface.
2. **The event topic has three contradictory names**: `editor.events` (repo `CLAUDE.md` and the code
   default), `ods.editor.events` (GTM, six occurrences), `doceditor-events` (platform rule).
   Publishing to the wrong one is **silent**. The code was deliberately left on the repo's default.

**Three defects found by doing rather than by reading**, all worth repeating as method:

- `editor.templates` had RLS enabled and **no policy at all** — migration 005's `pg_policies` check
  was not schema-qualified and matched `securemail.templates`. Writing the assertion found it;
  re-reading the migration had not, twice.
- The correlation middleware's `span.enter()` was held across an `.await` and nothing was ever
  logged under the span. Found by **running the service and grepping for an id I had just sent**.
- `CLAUDE.md`'s `## Tests` block still said port **5433** — twenty lines under the paragraph
  explaining that 5433 is another project's container. The trap described by the document, inside
  the document. Fixed in lot 3.

**Method that paid three times over:** every guard written with a companion non-vacuity test, and
each earned its keep immediately — the CI-workflow guard's caught a bug in my own parser that made
the real assertion pass while inspecting zero jobs; the error-surface guard's turned red the instant
the variants were removed; and lot 3's round trip was **mutation-checked in both directions**
(`publish` returning `Ok` without sending → 2 of 3 red; `ce_tenantid` corrupted → 1 red) with the
red output captured into the evidence folder rather than narrated.

See [[doceditor-test-database]] for the shared-instance traps and the fresh-database defect class,
[[h2-advisory-campaign]] for why this unit keeps re-opening (**still open after lot 3 — merging
PR #3 is the `pr` agent's gesture, and this unit has now cost six dev turns**).
