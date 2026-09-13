## Task: Close the BA non-compliance findings on doceditor (ba-report.json @ bf5c29f)

### Objective
Turn the 4 MISSING + 4 PARTIAL + 6 actionable DEVIATION findings of the BA review into
delivered code, on branch `feat/doceditor-c20260909-1345-lot1`, TDD, tests green.

### Source of truth
No `spec.md` exists for doceditor (AC-000, 3rd cycle). Criteria are graded against
`~/dev/specs/ods-platform/pdlc/gtm/doceditor-gtm.md` + platform-mandatory `~/.claude/CLAUDE.md`.
AC-000 is a SPEC defect, not a code defect -> escalated by human review, not "fixed" here.
BR-0007 (OpenAPI) does not name doceditor; AC-015 is delivered against the GTM's own Phase-2 gate.

### Plan
- [x] 0. Un-commit the idle-watchdog `wip:` commit (.env.example 5433->5435), re-land it properly
- [x] 1. `cargo fmt` on its own commit (BA MEDIUM: 511-line diff, repo CI lint job is red)
- [x] 2. AC-019 CRITICAL — document content: `content` on create/update, stored, versioned,
        word_count maintained, MAX_DOCUMENT_SIZE_MB enforced
- [x] 3. AC-018 MEDIUM — `template_id` wired (seeds content) instead of silently discarded
- [x] 4. AC-011 HIGH — `FORCE ROW LEVEL SECURITY` + policies hardened (missing_ok + WITH CHECK)
        + startup warning when the DB role bypasses RLS
- [x] 5. AC-012 CRITICAL — real Redpanda producer wired in main.rs (NoopProducer only as fallback)
- [x] 6. AC-013 MISSING — X-Correlation-Id / X-Source-Service inbound middleware + echo + events
- [x] 7. AC-002/003/008/009 PARTIAL — the missing integration tests
- [x] 8. AC-015 MISSING — `docs/openapi.yaml` (OpenAPI 3.1) + a test that guards it against drift
- [x] 9. CI MEDIUM — PRs to `dev` trigger CI; `test` job gets a Postgres service + DATABASE_URL
- [x] 10. LOG_LEVEL LOW — actually applied to the tracing subscriber
- [x] 11. `.env.example` aligned with what `config.rs` reads (BA LOW)

### Risks
- `rdkafka` build is cmake-based and slow; it is already a declared dependency so the cost is paid.
- `FORCE ROW LEVEL SECURITY` cannot be proven on the dev DB: role `ods` is `rolsuper`/`rolbypassrls`.
  Mitigation: declare it in the migration, and detect+warn at startup instead of pretending.
- A real producer must not make the service fail to boot when no broker is reachable.

### Rollback
Every migration is additive (`ADD COLUMN IF NOT EXISTS`, `ALTER POLICY`). No DROP, no TRUNCATE.
Revert = `git revert` the commit; the added columns are nullable and ignored by the prior image.

### Verification
DATABASE_URL=... cargo test        (expect >= 17 baseline, grown)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo tree -e normal -i h2         (must stay "nothing to print" — HR-20260909-001)

### Review Notes
(filled at the end)

### Review Notes — 2026-09-13

**Delivered.** 10 commits on `feat/doceditor-c20260909-1345-lot1`, 58 tests green
(baseline was 17), `cargo clippy --all-targets -- -D warnings` and
`cargo fmt --check` clean, `cargo tree -e normal -i h2` still "nothing to print".

| BA finding | Severity | Outcome |
|---|---|---|
| AC-019 no way to write content | CRITICAL | Delivered: `content` on create/update, versioned, word-counted, size-bounded |
| AC-012 events never published | CRITICAL | Delivered: `RedpandaProducer`, CloudEvents binary mode, keyed by document |
| AC-000 no spec.md | CRITICAL | **Not a code defect** — escalated, HR-20260913-001 |
| AC-011 RLS declared, not forced | HIGH | Delivered: FORCE + policies repaired + startup posture warning |
| AC-013 no correlation headers | MISSING | Delivered: middleware, echo, span, events |
| AC-015 no OpenAPI | MISSING | Delivered: `docs/openapi.yaml` 3.1 + drift guard test |
| AC-018 template_id discarded | MEDIUM | Delivered: seeds the body, 404/400 instead of silence |
| AC-002/003/008/009 untested | PARTIAL | Delivered: 9 integration tests |
| cargo fmt 511-line diff | MEDIUM | Delivered: isolated style commit |
| CI never ran on PRs to dev, no DB | MEDIUM | Delivered: triggers + PostgreSQL service + contract job |
| LOG_LEVEL never applied | LOW | Delivered, with `RUST_LOG` precedence |
| .env.example richer than config.rs | LOW | Delivered: aligned, `__NEEDS_HUMAN__` marked |

**Found while working, not in the BA report:**
- `editor.templates` had RLS enabled and **no policy at all**. Migration 005's
  existence check was not schema-qualified and matched `securemail.templates` on
  the shared instance. Fixed in 007; surfaced by writing the test, not by
  reading the file.
- `span.enter()` held across `.await` in the new middleware — the wrong form in
  async code. Found by running the service and grepping for a correlation id
  that was not there. Fixed with `.instrument()` plus an access log.
- Documents started at `current_version = 1` with no version-1 row, so the first
  snapshot was version 2 and version 1 was unreachable forever.

**Escalated, deliberately not decided here (HR-20260913-001):**
- No `spec.md` for doceditor — writing one means inventing the requirement.
- The event topic has three contradictory names across sources of truth
  (`editor.events` / `ods.editor.events` / `doceditor-events`). Publishing to
  the wrong one is silent. The code was left on the repo's own default.

**Verified live**, not only by tests: service started locally on 8187 against
ods-postgres, full authoring cycle exercised (create with body → 2 edits →
3-version history → version 1 still original → explicit snapshot v4 →
cross-tenant 404 → soft delete → 404), probes without auth, 401 without token,
422/400/404 on the validation paths, correlation id echoed and present in the
structured logs, RLS bypass warning naming the role.
