---
name: go-read-the-path-yourself
description: When a BA report has little or nothing code-actionable — twelve turns running on doceditor, then one that finally did and got the cause wrong — what to do with the turn, the thirteen places the real defects have actually been, and the duty to measure a hypothesis (and a report's own attribution) before coding it
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

**Why:** measured twelve turns running on doceditor (lots 5 to 16 —
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

- **lot 12 — the response was correct and the query was not.** `GET
  /documents/{id}/versions` renders no bodies, the published contract says so,
  and the SQL under it read every body anyway for a history that is
  **unpaginated by contract** — so the projection was the only bound and it
  bounded nothing. At the published ceilings (55 versions of a 10 MB document,
  512 MiB of instance) the answer was not a 500 but an **OOM kill of the
  process**: on Cloud Run that is the instance, so other tenants' in-flight
  requests die too, and one caller triggers it with two ordinary calls. The
  proof that this class is invisible from the usual vantage point: one of the
  new tests, the one asserting the HTTP response carries no body, **passed
  before the fix**. So lot 9's "test from where the caller stands" has a
  complement — **ask not only what an endpoint returns but what it had to read
  to return it**, because a correct answer can be produced at unbounded cost.

- **lot 13 — a rule stated about one shape of input, and a constant quoting the
  rule as if it held.** `metadata` string values are bounded "at most 256
  characters"; the check was `if let Some(s) = value.as_str()`, so wrapping the
  same string in `{}` or `[]` stored **5 MB** in that field. Nothing bounded the
  object as a whole either, and `DocumentSummary` keeps `metadata` while
  deliberately dropping `content` — so a page multiplied it by a hundred:
  **300 MB and 945 MiB of RSS against a 512 MiB instance**, from 30 ordinary
  `201`s. Two compounding lessons. First, **ask a validation rule which shapes of
  input it is silent about** — this was the third instance in one repo of "a
  check that says nothing about the inputs it was not shaped for is not a check",
  and the first two were at depth 0. Second, **a test that restates a rule
  instead of exercising it confirms the author, not the code**: the constant
  `ENVELOPE_ALLOWANCE_BYTES` was documented as covering "the largest envelope
  this service's own validation admits — under 7 KiB" and its unit test computed
  that envelope from the prose and compared it to the constant. Green, while the
  service admitted three hundred times the number. The durable half of the fix is
  that the test now reads the constants the code *enforces*.
  And when the repair needed a number no spec provided: **look for one the
  codebase already asserts** (here, the allowance the payload ceiling was already
  computed from) before inventing one — that is what separates a repair from a
  product decision you are not allowed to make.

- **lot 14 — the actionable work was in the decisions the report CITED.** Tenth
  report in a row with nothing code-actionable, and both of its non-code
  deviations pointed at human reviews. Read as JSON rather than through the
  report's summary, those files contained a decision **already taken** by the
  human, with `"enactor": "dev"` and "Exécutant : dev, dans le dépôt doceditor"
  in the option text, and a `enactError: {"reason": "verbe inconnu"}` recording
  that the dispatcher had failed to route it. A second one had been executed by
  the resolver *during the turn*, putting a canonical `spec.md` on disk after
  fourteen cycles of its absence — and that spec settled, in writing, a question
  this repository's `CLAUDE.md` still called "open, not to be guessed". **So
  before hunting a new defect: re-read every `HR-*.json` the report cites, and
  re-check the specs directory even when a dozen cycles say it is empty.** A
  decision that is taken but unexecuted looks exactly like a decision that is
  pending, from one level up. And when the decision offers an example value
  ("ex. 2 Mo"), measure whether the example is the right one — here it was, and
  saying so with numbers made the decision stronger than repeating it would
  have.

- **lot 15 — the input nobody parsed, and the test that could not see it.**
  Every other input is parsed into a value at the boundary; the **query string**
  and the **path parameters** were not, so `serde` typed them and the
  *framework* answered whatever it refused — seven wire responses in
  `text/plain` (or with no body at all) outside the closed error enumeration the
  contract publishes. `tests/error_surface.rs` was green throughout because it
  compares `src/error.rs` with `docs/openapi.yaml`: **a test that compares two
  sources cannot see a response neither source produces.** Ask of every
  framework extractor: *who answers when it refuses, and in what shape?* On the
  same query string, one gesture — the field left empty — had four answers, and
  the worst was the silent one: `?search=` answered `200` **with an empty page**
  to a tenant owning documents. Lot 13's "plausible lie" again, one parameter
  over.
  Two more durable halves. First, **the suite made the fix smaller**: trimming
  every parameter turned `?status=published%20` — a typo lot 13 refuses *on
  purpose* — into a valid status, and `list_contract_test.rs` went red. Run the
  whole suite before believing a repair; a neighbouring test often encodes a
  deliberate decision that a generalisation would overturn in silence. Second,
  **re-measuring a "non-code" deviation is where the other find was**: the live
  Cloud Run revision already carried `EVENT_BUS=pubsub` and
  `PUBSUB_TOPIC=editor-events`, variables no line of `config.rs` reads and which
  the deployment descriptor does not list — the deployment had chosen, the code
  never learned. That is decision-grade information, so it went to a human
  review (HR-20260914-007) instead of a fourth cycle of the same paragraph.

- **lot 16 — the decision the report cited, taken and unrouted, for the second
  time in two batches.** Twelfth report with nothing code-actionable; it
  described HR-20260914-007 as `PENDING` and "not fixable by a doceditor dev
  cycle alone". Read as JSON forty minutes later: `status: DISPATCHED`,
  `resolution.by: it@orbusdigital.com`, `options[0].enactor: "dev"`, and
  `enactError: "verbe inconnu : doceditor transport A"` — **the same dispatcher
  failure as lot 14's `doceditor sizing A`**. Two instances make it a rule: the
  BA report's paraphrase is precisely the level at which taken-but-unrouted and
  pending look identical. The defect underneath was the same class as lot 8's
  two layers: the *deployment* had carried `EVENT_BUS=pubsub` and three other
  variables since May 2026 and **no line of `src/config.rs` read any of them**,
  so the no-op producer was chosen and four months of events went nowhere in
  silence. Two further halves worth keeping. First, **measure the premise of
  the option you are about to enact**, not just the defect: option A was
  recommended as "no infrastructure change", and the thing that made that true —
  `roles/pubsub.publisher` already granted to the runtime service account — is
  deducible from **no file in the repository**; three `gcloud` calls, five
  minutes. Second, **when the repair adds a dependency, re-measure the estate
  rule it could break**: an HTTP client is exactly how `h2` comes back, so
  `cargo tree -e normal` was read again after the change (675 crates, h2 in
  none) instead of assumed.

- **lot 17 — the streak ended, and the finding was mis-attributed.** The
  thirteenth report carried something real: RUSTSEC-2026-0285 against `rustls`,
  a crate this service compiles. It also carried a cause — "introduced by this
  lot's new reqwest+rustls-tls chain" — and that was **false**, provable in one
  command: `git show bce825b^:Cargo.lock` holds `rustls 0.23.40`, from before
  that batch, reached through `sqlx`'s `tls-rustls` since the service was
  written. The dependency is old; only the **advisory** is new. The inversion
  decides the repair: had a dependency decision caused it, a dependency
  decision would prevent the next one — as it stands, only a standing
  measurement does, and the repository had none (the one dependency guard names
  `h2` 0.3 by name). **So: an actionable report does not end your measuring, it
  starts it.** Take the fix *and* ask what would have caught it. The
  path-reading half of the same turn found the better defect anyway: a page walk
  over ties returned 2 000 rows holding 1 999 documents, because
  `ORDER BY updated_at DESC` is not total and the planner changes plan between
  `OFFSET 0` and `OFFSET 1900`.

**How to apply.** These thirteen are a checklist, not anecdotes: concurrency on the
write path, a predicate across every call site of its rule, a premise nobody has
re-measured, a value normalised in one layer and reported in another, a
configured limit applied to a different quantity from the one it names, a bound
whose unit (bytes/characters) differs from the unit its contract and its column
count in, and a **constraint that lives outside the application code entirely**
— an index expression, a column type, a trigger — refusing what the code
happily accepts, a rule whose check is silent about every shape of input it
was not written for, **an input the boundary never parsed at all, whose
refusals are therefore written by the framework**, and **a decision the report
calls pending that the JSON says was taken and never routed**. Also
compare the code against the repo's **published contract** (`docs/openapi.yaml`
here) — when the two disagree, work out which is the defect before editing
either; on lot 8 the contract was right four times out of four.

**Publish the hypothesis measurement refuses to credit, too — after the code as
well as before it.** On lot 12 the fix had two halves; the read half turned an
OOM kill into `200` in 55 ms, and the write half (`RETURNING` the body back out
of PostgreSQL for three callers that never read it) moved latency 2 635 → 2 563
ms (noise) and peak memory 433 → 418 MiB, **without moving the concurrency at
which the instance dies**. It was kept — a value no caller reads should not
travel — but the ADR, the commit and the evidence all say what it does not buy.
A commit that lets a reader believe both halves paid is how the next reviewer
inherits a false premise, which is the thing [[operational-not-code]] cost seven
cycles.

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

**And check that your own guard can express the defect, before you trust it.**
Lot 17's first draft of the ordering test compared the two query plans over the
*whole* list and they agreed: the instability is produced by the bounded sort,
i.e. by the **page**, so the guard was green against the very defect it was
written for. What caught it was writing the non-vacuity assertion first and
watching it fail. Related, same turn: **do not build a test that waits for the
planner to change its mind** — the switch point is a cost estimate, so it goes
green vacuously on a smaller database. Pin the two plans instead.
