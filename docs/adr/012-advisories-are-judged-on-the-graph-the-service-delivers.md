# ADR-012 — Advisories are judged on the graph the service delivers

- **Status**: accepted
- **Date**: 2026-09-14
- **Context**: RUSTSEC-2026-0285 (`rustls`), the guard that did not fire, and
  `cargo audit`'s tally
- **Supersedes nothing.** Extends the practice of ADR-002's sibling decision
  HR-20260909-001 (`h2` 0.3 must leave the shipped graph) from one crate to the
  rule behind it.

## What happened

On 2026-09-14 the RustSec database published **RUSTSEC-2026-0285** against
`rustls` 0.23.40: under TLS 1.3, handshake messages accepted across encryption
level boundaries. A fixed release, 0.23.45, already existed.

`rustls` is in this service's delivered graph and has been since it was
written — `sqlx` is taken with `tls-rustls` — and since ADR-011 it arrives a
second way, through `reqwest`'s `rustls-tls`. Measured:

```text
$ cargo tree -e normal -i rustls
rustls v0.23.40
├── hyper-rustls v0.27.9 -> reqwest v0.12.28 -> ods-doceditor
├── reqwest v0.12.28 (*)
├── sqlx-core v0.8.6 -> sqlx v0.8.6 -> ods-doceditor
└── tokio-rustls v0.26.4 -> …
```

**Nothing in this repository went red.** The suite has a dependency guard,
`tests/framework.rs`, and it names `h2` 0.3; `tests/dependencies.rs` finds
crates nothing imports. Neither of them asks whether a crate we compile carries
an advisory. The finding reached us because a reviewer ran `cargo audit` by
hand during a BA cycle — which is the part that does not repeat.

It is the same shape this service has paid for four times over: *a check that
says nothing about the inputs it was not shaped for is not a check* (the
metadata guard that only looked at objects, the status filter that only knew
the words it expected, the `maxLength` applied to bytes).

The attribution in that review — "introduced by this lot's new
`reqwest`+`rustls-tls` chain" — is worth correcting, because it changes what
has to be fixed: `rustls` 0.23.40 is in `Cargo.lock` at `bce825b^`, i.e. before
the batch that added `reqwest`. The **dependency** is old; the **advisory** is
one day old. No dependency decision caused this, so no dependency decision
prevents the next one. Only a standing measurement does.

## Decision

1. **Take the fix.** `cargo update -p rustls` → 0.23.45 (and `rustls-webpki`
   0.103.13 → 0.103.15 with it). Lockfile only; no manifest, no source.
2. **Judge exposure on `cargo tree -e normal`, not on `cargo audit`'s tally**,
   and write that judgement down as a command rather than as a habit:
   `scripts/audit-delivered-graph.sh`, run by the `Advisories` job of CI on
   every push and every pull request.
3. **Keep the judgement honest in both directions.** The script prints every
   advisory `cargo audit` finds, with a DELIVERED column, and fails only on the
   delivered ones. `yanked` and `unsound` are reported without failing — the
   line `cargo audit` itself draws.

## Why the delivered graph and not the lockfile

A lockfile is wider than a binary: it pins the optional dependencies of our
dependencies whether or not their feature is on. Measured the same day, every
advisory `cargo audit` reports against this repository is against a crate
nothing here compiles:

| crate | advisory | in `cargo tree -e normal`? | reached only by |
|---|---|---|---|
| `rsa` 0.9.10 | RUSTSEC-2023-0071 | **no** | `sqlx-mysql`; `sqlx` is taken with `postgres` |
| `anyhow` 1.0.102 | RUSTSEC-2026-0190 (unsound) | **no** | removed as a dependency in `735647d` |
| `spin` 0.9.8 | yanked | **no** | `sqlx-sqlite`, `num-bigint-dig` |
| `rustls` 0.23.40 | RUSTSEC-2026-0285 | **yes** | `sqlx-core`, `reqwest` |

`rsa` has no fixed release and has had none since 2023. A check that failed the
build on it would be switched off within a week — and then the next `rustls`
would pass unseen. That is the whole reason the intersection is the rule rather
than `cargo audit`'s exit code, and it is the reading `735647d` already applied
by hand ("judged on the DELIVERED graph, not on `cargo audit`'s tally") when it
fixed `event-listener` and `chacha20` and left `proc-macro-error2` alone.

The order of remedies, printed by the script on failure, is also that
commit's: take the semver-compatible release; else turn off the feature that
pulls the crate; else remove the dependency nothing imports; else escalate.
**Not** a blanket ignore: there is no `.cargo/audit.toml` in this repository and
this decision is the reason to keep it that way — an advisory against something
we ship is exposure, and it belongs in the open.

## What is guarded, and by what

`scripts/audit-delivered-graph.sh` needs the advisory database, so it needs a
network, so it cannot live in the test suite. `tests/advisories.rs` asserts
everything about it that can be checked offline, and the split matters: the CI
job is the measurement, the test is what stops the measurement being deleted or
quietly rendered vacuous.

- the workflow really runs the script, on `push` **and** `pull_request` — an
  audit that only ran on a schedule would report a week late;
- the script's classification, on fixtures: a delivered vulnerability fails; the
  same advisory against a crate outside the graph does not; the comparison is on
  `name version`, so an upgrade reads as a fix; warnings report without failing;
- and, twice, that **nothing measured is not a pass**: an empty delivered graph
  or an unreadable report exits 2, because the empty set is a subset of
  everything and would otherwise clear every advisory at once.

Mutation-tested three ways before being trusted — membership forced to `false`,
non-vacuity check removed, comparison reduced to the crate name — each produced
exactly one failure, in the test that names the property.

## Consequences

- One more CI job, about two minutes warm (`cargo install cargo-audit --locked`
  is cached by `Swatinem/rust-cache`, which also caches `~/.cargo/bin`). It
  installs the same apt packages as every other cargo job, which
  `tests/ci_workflow.rs` requires and which this ADR does not relitigate.
- The job can go red on a day this repository has no diff, because advisories
  are published by other people. That is the intended behaviour: today's
  finding was exactly that, and it was carried for a day by a human reviewer.
- `cargo-audit` is installed from crates.io with `--locked` rather than through
  a prebuilt-binary action. A check on the supply chain is the wrong place to
  add a new third party to trust.

## Open, and named rather than silently inherited

This repository has cited **BR-0010** — "judged on the delivered graph" — since
`735647d`, and it is also cited in `CLAUDE.md`. It is **not** in
`~/dev/specs/ods-platform/context/business-rules.md`, which on 2026-09-14
contains BR-0001, 0003, 0004, 0006, 0007, 0008, 0009 and 0013 and nothing
between 0009 and 0013. The rule this ADR writes down is therefore local to this
service until someone injects it platform-wide; it is not invented here (the
practice and the wording predate it), but it rests on a citation that resolves
to nothing. Reported rather than fixed: injecting a business rule is a cockpit
act, not a dev one (BR-0002).
