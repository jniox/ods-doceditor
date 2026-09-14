---
name: go-read-the-path-yourself
description: When a BA report has nothing code-actionable — seven turns running on doceditor — what to do with the turn, the seven places the real defects have actually been, and the duty to measure a hypothesis before coding it
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

**Why:** measured seven turns running on doceditor (lots 5, 6, 7, 8, 9, 10, 11 —
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

- **lot 9 — one number used as two different ceilings.**
  `MAX_DOCUMENT_SIZE_MB` bounded the HTTP payload *and* the stored body. JSON
  always makes the payload bigger than the body, so a document of exactly the
  documented maximum was refused (`413 text/plain`) and the service's own 422
  was unreachable from HTTP entirely. **Ask of every configured limit: what
  quantity is it actually applied to, and is that the quantity its name
  promises?** Nothing was red because the only test built an `App` carrying
  **no `JsonConfig` at all** — a hand-made bench does not merely drift from
  production, it can be *missing the component under test*. The cure is not a
  better test: it is one wiring function that `main.rs` and the tests both call.

- **lot 10 — a bound named in one unit, applied in another, and a value
  validated that was not the value stored.** `maxLength: 500` and `VARCHAR(500)`
  both count *characters*; `str::len()` counts bytes, so the largest storable
  title was 500, 250 or 166 characters depending on the alphabet. Worse, the
  rename path validated `title.trim()` and stored the untrimmed string: a
  500-character title with a leading space became `22001 value too long`, i.e. a
  **500 on a legal request**. Lot 9's question ("what quantity is this limit
  applied to?") found a second site the moment it was asked of another field.

- **lot 11 — the thing that refuses the write is not always the code that says
  "limit".** The GIN index of migration 006 covered `title || ' ' || content`
  whole; a `tsvector` cannot exceed 1 048 575 bytes of lexemes, so the *index
  expression* decided whether a document could be **stored**, on a budget set by
  the text's **vocabulary** rather than its size: 798 893 bytes of distinct
  reference codes were refused, 10 050 000 bytes of repetitive prose were not,
  against a published ceiling of 10 MB. Lot 9's question asked of a
  non-obvious enforcer. And the fix's own second half was only visible by
  mutation: bounding the index alone **moves** the error to the read, where a
  bitmap heap scan rechecks the predicate on the heap row. **Ask of every fix:
  which other layer evaluates this same expression?**

**How to apply.** These seven are a checklist, not anecdotes: concurrency on the
write path, a predicate across every call site of its rule, a premise nobody has
re-measured, a value normalised in one layer and reported in another, a
configured limit applied to a different quantity from the one it names, a bound
whose unit (bytes/characters) differs from the unit its contract and its column
count in, and a **constraint that lives outside the application code entirely**
— an index expression, a column type, a trigger — refusing what the code
happily accepts. Also
compare the code against the repo's **published contract** (`docs/openapi.yaml`
here) — when the two disagree, work out which is the defect before editing
either; on lot 8 the contract was right four times out of four.

**Measure the hypothesis before you code it — including when the library source
seems to prove it.** On lot 10 the strongest-looking lead was event loss at
shutdown: rdkafka's `impl Drop for BaseProducer` really does `purge()` before
flushing, and this service has a documented history of dropping events in
silence. Fifteen minutes of probing against the real broker showed it does not
reproduce (the polling thread is joined first). Writing the `Drop`-flush "fix"
would have shipped code for a premise and a test flaky by construction. The
discipline is the same one that unblocked lot 7, applied *before* the code
rather than after. Record the falsified hypothesis in the evidence folder so the
next reader does not re-derive it.

**Check your own fixture before you report the service.** The same lot produced
`search=Reference -> total=0` and, for a minute, "full-text search is entirely
broken". An earlier step of the probe had renamed that document. A defect you
cannot reproduce from a clean fixture is not yet a defect.

Hold the line in the other direction too: an internal inconsistency you can *see*
is not automatically a defect you may *decide*. `archived` freezing the status
but not the body is a product question with no spec — reported in `CLAUDE.md`,
still not fixed, across three lots. See [[doceditor-batch-20260914-lot10]],
[[doceditor-batch-20260914-lot9]], [[doceditor-batch-20260914-lot8]] and
[[doceditor-batch-20260913]].
