---
name: doceditor-batch-20260914-lot13
description: Lot 13 — the first BA report in nine cycles with a code-actionable finding (a test that never cleaned up), and the bound that existed only in prose while the page multiplied it by a hundred
metadata:
  type: project
---

Sixteenth turn on the unit, branch `feat/doceditor-c20260909-1345-lot1`,
`f88d1e8..a6363bd`. 144 tests (130 before), clippy/fmt clean.

## 1. The BA's own MEDIUM deviation was, for once, in this repository

Eight reports in a row had nothing code-actionable (see
[[go-read-the-path-yourself]]). This one did, and it was about the **test
infrastructure**: `tests/events_roundtrip.rs` created three uniquely named
topics per run and deleted none, on a broker that is a **standing container by
mandate**. The two facts compound — a resource kept alive on purpose, fed by a
test that never cleans up, saturates a fixed ceiling.

Measured: leak of **3 topics per run**; the ceiling is **file descriptors, not
the configured partition limit** (`topic_partitions_per_shard` = 1000, but
`docker logs` says *Refusing to create 1 partitions as total partition count 205
would exceed FD limit 204*). ~65 runs from empty to a suite that is red in no
diff, with `InvalidPartitions` on 3 of 4 tests. That is what the BA spent a turn
diagnosing, and it had already cost one before.

**The fix has two halves and either alone still leaks**: a `Drop` guard (covers
the *panicking* path too — and a failing run is exactly the one a reviewer
repeats) and an **age-based sweep at start-up** (covers `kill -9`, and is what
makes an already-saturated broker heal on the next run instead of staying red).
The sweep runs `delete_topics` unattended on a broker shared with
`editor.events`, so its blast radius is pinned by a **pure test** naming
everything it must never match. The timestamp leads the topic name because
parsing one leading integer is unambiguous; buried between a label and a UUID it
is not.

Generalisable: **a teardown that only runs on the happy path is not a teardown**,
and the third runner (the ADLC pipeline) again turned out to be the one nobody
sized for — same shape as the missing broker of 2026-09-13.

## 2. Then the path: `metadata` was bounded in prose and multiplied by the page

`docs/openapi.yaml` says *string values are at most 256 characters*.
`validate_metadata` said it with `if let Some(s) = value.as_str()` — a statement
about the values of the object and **about nothing else**, so the *container*
decided whether the rule existed. Measured on the running release binary:

```text
{"resume": 257 × 'a'}                  -> 422 "at most 256 characters"
{"resume": {"inner": 257 × 'a'}}       -> 201
{"tags": [257 × 'a']}                  -> 201
{"resume": {"inner": 5 000 000 × 'a'}} -> 201
```

This is the **third instance in this repo** of "a check that says nothing about
the inputs it was not shaped for is not a check" — after `as_object()` returning
`None` and the unknown `?status=`. The first two were found at depth 0; this one
was one level down, which is why six review cycles walked past it.

**The premise underneath was false, and it was quoted in a constant.**
`ENVELOPE_ALLOWANCE_BYTES` (the `+ 64 KiB` of the payload ceiling) is documented
as covering "the largest envelope **this service's own validation admits** …
under 7 KiB". It admitted **20 MiB**. And its unit test computed the envelope
from the *prose* of the rules (`500 + 20*(64+256+6) + 36 + 200`) and compared it
to the constant — an arithmetic restatement checked against itself, green
throughout. Same family as the payload bench that carried no `JsonConfig` at all
(lot 9) and as the ADR sentence that cost seven cycles on RLS (see
[[operational-not-code]]): **a test that restates a rule instead of exercising it
confirms the author, not the code.** The repair that matters is not the bound —
it is that `payload.rs`'s test now reads the constants the domain *enforces*, so
the two cannot drift again.

**What it cost was not a refused request.** `DocumentSummary` drops `content` on
purpose (ADR-007) and keeps `metadata`, so a page multiplies it by up to 100.
30 documents of 10 MB of metadata — 30 ordinary `201`s, empty bodies — then one
list: **300 010 939 bytes, peak RSS 945 MiB** against a 512 MiB allocation, i.e.
an **OOM kill of the instance**. Second batch running to end that way, and
through the one column the previous fix had kept. After: the worst page a caller
can build is 100 × 31 760 B → **3 197 951 bytes, 64 ms, 23 MiB**, and that is a
*bound* (`per_page × MAX_METADATA_BYTES`) where the old number was a function of
what a caller chose to store.

**How the bound was chosen without guessing a product rule** — this is the part
worth reusing. 32 KiB is *derived*: half of `ENVELOPE_ALLOWANCE_BYTES`, the
number the payload ceiling was **already computed from**. Enforcing it does not
invent a limit, it makes true a sentence the code already relied on. When an
inconsistency needs a number and no spec gives one, look for a number the
codebase already asserts before inventing one — that is the difference between a
repair and a product decision.

Two rules, not one, and neither is redundant: the character bound holds whatever
*container* a string sits in; the size bound holds whatever *shape* the value
takes. A million admissible strings in an array breaks only the second; one
5 000-character string breaks only the first.

**Said plainly in ADR-008, commit and evidence — what the fix does not buy**: the
transient cost of a *single* request is unchanged (the extractor parses the
envelope before any rule runs); rows written earlier are not rewritten. Letting a
reader believe otherwise is how the next reviewer inherits a false premise, which
is the thing this batch just spent a section on.

## 3. Hygiene and the unchanged non-code deviations

Probe rows removed from the shared `editor` schema before the end of the turn
(verified: 0 documents over 32 KiB remain, including one left by the *red* run);
no throwaway database created (BR-0011); the standing broker left up.

AC-000 (no `spec.md`, `HR-20260913-001` still `DISPATCHED` and never executed,
`HR-20260913-006` still `PENDING`, `business-rules.md` still zero doceditor
entries), AC-012 (`~/dev/ops/cloudrun/doceditor.json` still carries no
`REDPANDA_BROKERS`, and the file is outside this repository) and the `archived`
question: **re-measured in five minutes, transcripts in
`evidence/a6363bd/03`, deliberately untouched** — deciding any of them here is
the divergence BR-0002 exists to prevent. Seventh batch reporting the `archived`
one.

See [[doceditor-batch-20260914-lot12]] for the sibling defect on the version
history, and [[doceditor-test-database]] for the broker's role in the pipeline.
