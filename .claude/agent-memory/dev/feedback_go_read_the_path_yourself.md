---
name: go-read-the-path-yourself
description: When a BA report has nothing code-actionable — four turns running on doceditor — what to do with the turn, and the four places the real defects have actually been
metadata:
  type: feedback
---

A "BA FAIL — implement the missing criteria or fix the defect" order whose report
contains **no code-actionable finding** is not an empty turn. Do two things, in
this order:

1. **Re-measure every non-code deviation and put the transcripts in the evidence
   folder.** Five minutes. That is what justifies not touching them, and it is
   what stops the next reviewer re-deriving the same diagnosis from zero. Never
   *apply* a decision that belongs to a spec or to a human — that is the
   divergence BR-0002 exists to prevent.
2. **Then go find a defect yourself, by reading a path rather than a report.**

**Why:** measured four turns running on doceditor (lots 5, 6, 7, 8 —
2026-09-13/14). Each report said, in substance, *"no dev cycle needed on the
code"*. Each turn found something real, and none of it was subtle once looked at:

- **lot 5 — the write path, read for its concurrency and not its features.**
  Two editors saving at once got `duplicate key`; a save racing a snapshot got
  `deadlock detected`. Both are HTTP 500 on a legitimate request, on the service
  whose purpose is concurrent editing. The BA had graded those criteria MET for
  six cycles — **a criterion can be met by the happy path and broken by the
  second caller.**
- **lot 6 — a rule audited across all its call sites, not a function at a time.**
  `deleted_at IS NULL` was written inline in `list_versions` and forgotten in
  `get_version` forty lines below. `get_version` looks complete read alone; it is
  only wrong *next to its neighbour*. **Grep the predicate, not the function.**
- **lot 7 — a premise copied instead of measured.** See
  [[operational-not-code]]: seven cycles of "operational, not code" rested on one
  wrong word in an inherited ADR.
- **lot 8 — the layer that normalises versus the layer that reports.** The
  service clamped `page`/`per_page`; the handler echoed what the client sent. The
  existing test called the service, which is the one vantage point from which the
  response is invisible. **Ask of every normalisation: who tells the caller, and
  does that code read the same value?**

**How to apply.** These four are a checklist, not anecdotes: concurrency on the
write path, a predicate across every call site of its rule, a premise nobody has
re-measured, and a value normalised in one layer and reported in another. Also
compare the code against the repo's **published contract** (`docs/openapi.yaml`
here) — when the two disagree, work out which is the defect before editing
either; on lot 8 the contract was right four times out of four.

Hold the line in the other direction too: an internal inconsistency you can *see*
is not automatically a defect you may *decide*. `archived` freezing the status
but not the body is a product question with no spec — reported in `CLAUDE.md`,
still not fixed, across three lots. See [[doceditor-batch-20260914-lot8]] and
[[doceditor-batch-20260913]].
