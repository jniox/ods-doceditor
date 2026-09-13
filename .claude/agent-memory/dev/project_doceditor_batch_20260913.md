---
name: doceditor-batch-20260913
description: The 2026-09-13 batch that closed the BA FAIL — what shipped, and the two questions deliberately left to a human (HR-20260913-001)
metadata:
  type: project
---

On 2026-09-13 the BA FAIL of `bf5c29f` was closed on `feat/doceditor-c20260909-1345-lot1`:
12 of the 13 actionable findings delivered, 58 tests (baseline 17). The 13th — the missing
`spec.md` — is not a code defect.

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

See [[doceditor-test-database]] for the shared-instance traps, [[h2-advisory-campaign]] for why
PR #3 keeps re-opening.
