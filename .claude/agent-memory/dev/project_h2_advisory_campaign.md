---
name: h2-advisory-campaign
description: Platform-wide RUSTSEC-2026-0258 campaign (HR-20260909-001) — delivered in doceditor since 2026-09-09; why the unit keeps re-opening, and the runbook premises that did NOT hold here
metadata:
  type: project
---

RUSTSEC-2026-0258 (`h2`, unbounded empty DATA frames) is being cleared across ~10 ODS Rust
repositories, one work unit each, under human decision **HR-20260909-001** (2026-09-09,
it@orbusdigital.com). The reference procedure lives in another repo:
`~/dev/projects/docstore/docs/security/h2-0.3-removal-runbook.md`, with the framework evaluation in
`docstore/docs/adr/006-web-framework.md`.

**doceditor's share has been DONE since 2026-09-09** (`b4b8103`, plus `2b604b4` for HR-20260909-035),
on branch `feat/doceditor-c20260909-1345-lot1`, and re-verified green on 2026-09-10 **and
2026-09-12**. If this unit is dispatched again: **verify and report, do not redo — there is nothing
to commit.**

**Why it keeps coming back:** **PR #3 is still OPEN** (`MERGEABLE`/`CLEAN`, base `dev`, 4 files:
`Cargo.toml`, `Cargo.lock`, `tests/framework.rs`, `tests/api_test.rs`). Its non-merge — and nothing
else — re-opens the work unit on every dispatcher pass. **Merging is the `pr` agent's role, not
dev's.** As of 2026-09-12 this had already burnt three dev turns. Say so explicitly in the status so
the loop is visible rather than re-diagnosed.

**How to apply — the evidence to re-run (all cheap except the last two):**
`cargo tree -e normal -i h2` → *"nothing to print"* (h2 is absent from the shipped graph **entirely**,
not merely the 0.3 line); `cargo audit` → **RUSTSEC-2026-0258: 0 occurrences**; `cargo clippy
--all-targets -- -D warnings` → exit 0; `cargo test` → 17/17. `cargo` is **not on PATH** — prefix
with `export PATH="$HOME/.cargo/bin:$PATH"`. There is no `.cargo/audit.toml` to clean.

**Two premises of the runbook that were false here** (verify a transverse runbook's premises against
*this* repo before trusting its conclusion; the judge of an advisory is `Cargo.lock`):

1. It assumes `actix-web` is declared **once**. doceditor declares it in `[dependencies]` *and*
   `[dev-dependencies]`. Only the pair together removes `h2` 0.3 from `Cargo.lock` — the file
   `cargo audit` actually reads.
2. It demands `cargo audit: 0 vulnerabilities`, which is **unreachable across the whole Rust
   estate**. See below.

**Corrected 2026-09-12 — an earlier note in this file was wrong:** it claimed doceditor shipped
`h2` 0.4.13 (which the same advisory flags, fixed in ≥ 0.4.16). **False.** `git show
b4b8103:Cargo.lock` shows **0.4.19** from that commit onward — above the fix threshold. 0.4.13 was
never delivered here. Do not re-raise it.

**`cargo audit` will never print `0 vulnerabilities` here, and that is not a failure.** It reports
exactly **1**: RUSTSEC-2023-0071 (`rsa` 0.9.10, Marvin attack, *"No fixed upgrade is available"*),
reached via `sqlx-mysql` — which this PostgreSQL service does not use. Per **BR-0010** the
acceptance criterion is the **delivered graph**, not the tool's tally; the advisory is *declared and
traced*, and does not block. Several lots were burnt platform-wide on this exact confusion.

**Do not add TLS features to `actix-web`** (`rustls-0_*`, `openssl`): each re-enables `http2` without
naming it. HTTP/2 in-process (h2c) is deliberately given up — Cloud Run terminates HTTP/2 at the edge.

**The guards are effective but NOT enforced in CI here.** `tests/framework.rs` holds two non-vacuous
guards (the tree guard refuses to read a failed `cargo tree` as "no offender"; the manifest guard
fails if `actix-web` disappears and brace-balances entries so `sqlx`'s `tls-rustls` is not mistaken
for an actix TLS feature). But `.github/workflows/ci.yml` triggers on `pull_request: branches:
[staging]`, so a PR to `dev` runs nothing at all — and the `test` job has no Postgres either. See
[[doceditor-test-database]]. Deserves its own work unit; reported in PR #3.
