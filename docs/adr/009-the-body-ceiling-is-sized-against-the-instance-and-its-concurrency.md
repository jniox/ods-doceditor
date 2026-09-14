# ADR-009 — The body ceiling is sized against the instance and its concurrency

- **Status**: accepted
- **Date**: 2026-09-14
- **Decision**: HR-20260914-001, option A, taken by it@orbusdigital.com
  ("Baisser MAX_DOCUMENT_SIZE_MB (ex. 2 Mo) … Exécutant : dev, dans le dépôt
  doceditor")
- **Supersedes**: nothing. Completes ADR-001 (the body is what this service
  owns) and the "Measured and NOT fixed" note ADR-007 left open.

## Context

`MAX_DOCUMENT_SIZE_MB` had no owner. It was set to 10 because ten is a round
number, and it was documented in `.env.example` as an ordinary knob — "the
ceiling on a document body" — as though it concerned only the size of a
document.

It does not. It is one of **three settings that only make sense together**:

| | where it lives | value |
|---|---|---|
| the body ceiling | `src/config.rs`, this repository | was 10 MB |
| the instance memory | `ops/cloudrun/doceditor.json`, outside this repository | `512Mi` |
| requests served at once | nowhere — Cloud Run's default applies | **80** |

The third line is the one nobody had written down. `ops/cloudrun/doceditor.json`
sets no `--concurrency`, so Cloud Run's own default of 80 concurrent requests per
instance is in force. Each request carrying a full-size document holds its
buffered payload (up to `2 × ceiling + 64 KiB`) plus the `String` it parses into,
for as long as it takes to write the row and its version.

ADR-007 removed the worst consequence of that arithmetic — a history read that
killed the instance with two ordinary calls — and closed with a measurement it
deliberately did not act on: at 10 MB, a handful of simultaneous saves was fatal.
Four remedies were possible, three of them in a file outside this repository, and
none of them settled by any specification. It was escalated instead of guessed.
HR-20260914-001 is the answer: **lower the ceiling**, in this repository.

## What was measured

On the running binary, in a cgroup set to the deployment's own allocation
(`systemd-run --user -p MemoryMax=512M`), N concurrent full-size content saves
of **distinct** documents — nothing serialises, which is what N tenants saving at
once actually looks like:

```text
ceiling 10 MB   N=10  -> 200 x10,  peak 451 MiB, Result=success
ceiling 10 MB   N=12  -> 11 of 12 get no response, Result=oom-kill, MainPID=0
ceiling 10 MB   N=16  -> 16 of 16 get no response, Result=oom-kill, MainPID=0
ceiling  4 MB   N=80  -> 75 of 80 get no response, Result=oom-kill, MainPID=0
ceiling  3 MB   N=80  -> 200 x80,  peak 475 MiB  (93% of the cap — no margin)
ceiling  2 MB   N=80  -> 200 x80,  peak 340 MiB  (twice, 335 and 340 MiB)
```

Three things follow, and only the first was already known.

1. **An OOM kill is not a failed request.** `MainPID=0` means the process is
   gone: on Cloud Run the *instance* dies, so every other tenant's in-flight
   request on it dies too. At 10 MB, eleven ordinary saves did that.
2. **80 is not an N invented for the bench** — it is the platform's own default
   concurrency, and therefore the number the deployment has to survive. At 2 MB
   it survives with a third of the memory to spare; at 3 MB it survives with 7%,
   which is not a margin; at 4 MB it does not survive.
3. **A refused request is cheap.** 80 concurrent requests each carrying ~4 MB on
   the wire — over the body ceiling, under the payload ceiling, all answered
   `422` — peaked at **102 MiB**. The dominant cost is on the accepted path, not
   in the buffer, so bounding the payload harder would not have helped; the body
   ceiling is the right lever.

The 2 MB the human proposed as an example ("ex. 2 Mo") is therefore the value the
measurement supports, and the ADR records why rather than restating the choice.

## Decision

`DEFAULT_MAX_DOCUMENT_SIZE_MB = 2`, defined **once**, in `src/config.rs`.

Three consequences, each of which closes a way the number could drift:

- **One constant, two enforcement sites.** `DocumentService::new` carried its own
  `10 * 1024 * 1024` next to `config.rs`'s own `"10"`. Two independent spellings
  of one default is how a ceiling gets lowered in one place and left standing in
  the other. Both now read `config::DEFAULT_MAX_DOCUMENT_BYTES`.
- **One conversion, and it saturates.** `main.rs` computed
  `max_document_size_mb * 1024 * 1024` one line before handing the result to
  `payload_ceiling`, a function that saturates *and documents why* ("a service
  must not depend on an operator not typing a large one"). The multiplication
  before it wrapped on release builds and panicked on debug ones. It is now
  `AppConfig::max_document_bytes()`, saturating — the same cure as
  `Pagination::offset`.
- **A value this service cannot honour stops the boot.**
  `.parse().unwrap_or(10)` turned `MAX_DOCUMENT_SIZE_MB=2MB` into 10 MB in
  silence. Since this ceiling is now a safety setting, being ignored is worse
  than refusing to start: an operator who lowers it and is not obeyed learns
  nothing until an instance dies. `0` is refused for the same reason — it is a
  typo, not a policy of storing nothing.

And the contract says the number. `docs/openapi.yaml` said "bounded by
`MAX_DOCUMENT_SIZE_MB`" in three places and never once said how large that was,
so the one thing a client integrating against it needed was the one thing it
withheld. It now states **2097152 bytes**, in bytes — and carries no `maxLength`
on `content`, because that keyword counts *characters* and would publish a
ceiling three times too large for a Chinese document. That distinction cost a
batch of its own (ADR on `domain::text`, batch 10).

`tests/document_ceiling_test.rs` holds the four statements of the ceiling — the
code's constant, `.env.example`, the contract's number, the contract's *unit* —
against each other, and fails the build when they disagree.

## Consequences

- **Documents between 2 MB and 10 MB are refused from now on.** That is the point
  of the decision and it is a real behaviour change: a product storing 5 MB
  exports gets a `422` naming the limit where it used to get a `201`. It is a
  change in what the service promises, which is why it took a human.
- **Documents already stored above the new ceiling are untouched.** The bound is
  checked on incoming bodies only: an existing 9 MB document is still readable,
  still renamable, still snapshottable. Only a `content` that a caller *sends*
  is measured. Nothing migrates, nothing truncates.
- **Raising it again is a sizing decision, not a preference.** The other two
  terms have to move with it, and they live in
  `ops/cloudrun/doceditor.json`, outside this repository. The constant's
  documentation carries the measurement so the next person to touch it starts
  from the numbers rather than from the round one.
- **Not solved: nothing in this repository bounds concurrency.** 2 MB makes the
  platform's *default* concurrency safe; it does not make the service safe at any
  concurrency. Option C of HR-20260914-001 (`--concurrency 8` on Cloud Run) and
  option D (an in-process admission limit) both remain available and were not
  chosen. What has changed is that the surviving hazard now needs a deliberately
  raised concurrency rather than the default one.
- **The staging deployment picks this up on its next release with no
  configuration change**, precisely because it sets no `MAX_DOCUMENT_SIZE_MB`.
