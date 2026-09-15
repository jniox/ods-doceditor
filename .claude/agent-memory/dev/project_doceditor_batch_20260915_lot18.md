---
name: doceditor-batch-20260915-lot18
description: Lot 18 — renaming a document rewrote the document (9 MB of WAL for 26 bytes), the per-row instrument that made it testable, and the deploy verdict taken on a tag URL that hides a 100/0 traffic split
metadata:
  type: project
---

Lot 18 (`cfc7bda` → `e5413e2`), **twenty-first** dev turn on unit
`doceditor-c20260909-1345`, branch `feat/doceditor-c20260909-1345-lot2`, PR #4.
206 tests (200 before), 31 binaries, CI 5/5. **Fourteenth BA report with nothing
code-actionable** (31/32 MET, AC-012 PARTIAL) — its one deviation said of itself
*"a deploy/traffic-promotion task, not a dev code task"*.

**The defect: a `PATCH {"title": …}` rewrote the whole body.**
`update_document` locked with `SELECT {DOC_COLUMNS} … FOR UPDATE` — body
included — then wrote every column back, `content = $4` bound to the body it had
just read. **A column bound to a parameter is a column PostgreSQL stores
afresh**, so the body was re-TOASTed on every PATCH. Measured on the release
binary, 8 000 000 incompressible bytes:

```text
                        WAL before     WAL after    latency
  PATCH {"title"}       9 095 640 B     299 160 B   451 -> 142 ms
  PATCH {"status"}      9 094 856 B     299 136 B   411 -> 118 ms
  PATCH {"metadata"}   11 540 528 B     299 168 B   427 -> 117 ms
  PATCH {"content"}    17 664 784 B  17 670 760 B   unchanged (it writes a body)
  one idle second               0 B           0 B   no background noise
```

Fix: `COALESCE($n, column)` for every column the caller did not name — the datum
passes through and the TOAST pointer is reused. `word_count` and
`current_version` become `CASE`s on the same parameter, which also removes the
last read-modify-write on `current_version` (PostgreSQL assigns it inside the
locked statement, `insert_version` takes it from `RETURNING`). **The lock did not
move** — same row, same order as `create_version`, ADR-004 and
`concurrency_test.rs` untouched; only its projection changed
(`UPDATE_LOCK_COLUMNS = "status"`). ADR-014.

**Three things worth keeping.**

1. **The instrument decides whether the guard can exist.** The obvious
   measurement — `pg_current_wal_lsn()` delta — is *cluster-wide*, and this
   suite runs 31 binaries in parallel against a shared instance where
   `history_read_test` alone writes 10 MB documents. A WAL-based test would be
   flaky by construction, which is the trap lot 17 already warned about.
   PostgreSQL 17's **`pg_column_toast_chunk_id(value)`** is a *per-row* fact: a
   rewritten body points at a new TOAST value, an untouched one at the same.
   Before: 43308 → 43309 → 43310 on three ordinary PATCHes. After: 43313 three
   times. Deterministic, and blind to the neighbours.
2. **Measure the mechanism before coding it.** The whole repair rests on "does
   `COALESCE($n, column)` with a NULL parameter really preserve the TOAST
   pointer?" — that is a claim about PostgreSQL's executor, not about my code.
   One `psql` probe on a real 8 MB row: 299 248 bytes, chunk id unchanged. Five
   minutes, before any Rust.
3. **Publish what the measurement refuses.** Hypothesis: at 512 MiB and
   concurrency 80 this kills the instance like ADR-007 and ADR-008. **False.**
   40 concurrent renames of the 8 MB document: `200` ×40 both before (peak
   384 MiB) and after (238 MiB). The repair buys ~146 MiB and a factor of thirty
   on the log, not a rescue. It is in the ADR, the commit and the report.

**A hypothesis killed before code, recorded so nobody re-derives it:** "platform
templates (`tenant_id IS NULL`) are invisible under the enforced RLS of
migration 008, so AC-018 is broken in production". Migration 007's policy
already carries `OR (tenant_id IS NULL AND is_system = true)`. Nothing to fix.

**AC-012, re-measured — and the new half.** 100 % of traffic is still on
`doceditor-00003-vkq` (image of 12 May 2026) and `doceditor-00004-jon` is Ready
at **0 %**. What had never been said: **both `DEPLOYED_HEALTHY` verdicts were
taken on `https://rev-6627534---doceditor-….a.run.app` — the TAG url, which
bypasses the traffic split by construction.** "pubsub transport confirmed live"
is true of the revision that serves nobody, nothing in the chain reads
`status.traffic`, so the deploy step is DONE and no agent will ever open the
promotion. → **HR-20260915-002** (deployment). Same family as HR-20260914-024.

**BR-0002, third lot running.** HR-20260914-001 (2 MB ceiling) and
HR-20260914-007 (Pub/Sub) are taken *and shipped*, while `spec.md` §4.3 and
écarts D-1/D-2 still call both open — the spec was written at 08:45 on 14/09,
between the two decisions. The BA reads the spec, not the journal, hence AC-012
PARTIAL four reports running. → **HR-20260915-003** (product), option A is
`cadrage BR ods-platform …`, the mechanism BR-0002 provides for exactly this.
**And a correction to [[doceditor-batch-20260914-lot17]]: BR-0010 DOES exist** —
in `~/dev/ops/standards/STANDARDS.md` §7, not in
`~/dev/specs/ods-platform/context/business-rules.md`. Lot 17 measured the wrong
file and reported the citation as phantom. Check both files before calling a
rule missing.

**Mechanics:**

- `pkill -f <pattern>` kills **your own shell** when the pattern appears in its
  command line (it does, via the `bash -c` wrapper). Exit 144, no output, the
  measurement silently never ran. Use `systemctl --user stop <scope>` or short
  binary names with `pkill -x`.
- `systemd-run --user --scope` does **not** accept `-p WorkingDirectory=`; wrap
  with `/bin/bash -c 'cd /tmp/… && exec <bin>'`. Needed because starting the
  binary from the repo loads `.env`, whose `JWT_RSA_PUBLIC_KEY_B64=__NEEDS_HUMAN__`
  panics the boot — the same trap as lot 16.
- A harness that `wait`s after starting the server with `&` waits for the server
  too, forever. Collect the curl PIDs and wait on those.
- **The idle watchdog struck twice in one turn** (`67c6cfc`, then `b9ce82d` in
  the middle of a build), both times committing the scenario agent's
  `tests/e2e/*.sql`. `git reset origin/<branch>` each time, local-only. Third
  consecutive lot.
- sqlx's own **"slow statement" WARN** is free diagnostic evidence: it prints the
  offending SQL verbatim. It is what showed the locking read taking 10.0 s.

See [[go-read-the-path-yourself]] for the running checklist this extends.
