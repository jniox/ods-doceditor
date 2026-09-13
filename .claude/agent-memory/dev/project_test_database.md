---
name: doceditor-test-database
description: How doceditor's integration tests reach Postgres — the 5435 fallback, why the `editor` schema must pre-exist, the shared-instance collisions, and what CI now provides
metadata:
  type: project
---

Since 2026-09-13 the harness lives in **`tests/common/mod.rs`**; every integration test binary
declares `mod common;`. It reads `DATABASE_URL` and falls back to
`postgres://ods:ods-dev-2026@127.0.0.1:5435/ods?options=-c search_path=editor,public`, announcing
that fallback on stderr (settled 2026-09-10 by **HR-20260909-035**, sha `2b604b4`).

**Why the shape is what it is** — three things bit, in this order:

1. The old fallback said **5433**, which on this host is another project's container
   (`honcho-poc-database-1`, pgvector). It *answers* and rejects the `ods` role with `28P01`, so the
   failure looked like broken code. `ods-postgres` is on **5435**. The repo `CLAUDE.md` said 5433
   for months; **it was corrected on 2026-09-13** — but trust `docker ps` over any doc.
2. `dotenvy` is called only in `src/main.rs`, **never in the test binary**. The repo's `.env` is
   decorative for tests; fixing `.env` cannot fix a test.
3. The dev instance is **shared** (`clm`, `docstore`, `pdf`, `securemail`, `public`…). sqlx creates
   `_sqlx_migrations` with an *unqualified* `CREATE TABLE IF NOT EXISTS` **before** running
   migration 001 — which is the migration that creates the `editor` schema. With `editor` absent
   the table resolves into `public`, whose version 7 belongs to another service, and every test
   dies on `VersionMissing(7)`. `after_connect`'s `SET search_path` does not save you — PostgreSQL
   silently ignores a non-existent schema in the path.

**How to apply:** the harness and `src/main.rs` both run `CREATE SCHEMA IF NOT EXISTS editor`
**before** `migrate!` now, so the chicken-and-egg is closed in code rather than by a manual psql
gesture. The `?options=` pin is still what keeps `_sqlx_migrations` in `editor` afterwards; both
are needed, neither is sufficient alone. The old `DELETE FROM _sqlx_migrations WHERE description
LIKE …` is **gone** — migrations are idempotent, the tracking table is now correct, and deleting
rows before a concurrent `migrate!` was a race waiting to fire as test binaries multiplied.
Read `VersionMissing(N)` as *shared migration table*, never as a corrupt migration.

**`VersionMismatch(N)` is the other one, and it will bite you.** Editing a migration file that has
already been applied changes its checksum and sqlx refuses to run. Delete **that single row**
(`DELETE FROM editor._sqlx_migrations WHERE version = N`) and re-run. Never touch
`public._sqlx_migrations` or another service's row.

**The shared instance collides on names, not just on tables.** Migration 005 checked
`pg_policies WHERE tablename='templates' AND policyname='tenant_isolation'` with no `schemaname`,
matched **`securemail.templates`**, and therefore never created the policy on `editor.templates` —
which sat RLS-enabled and unprotected until 2026-09-13. Anything querying `pg_policies`,
`pg_class` or `information_schema` must filter on the schema. See [[doceditor-batch-20260913]].

**CI is no longer environmentally red.** As of 2026-09-13 `.github/workflows/ci.yml` triggers on
pull requests to **`dev`** (it only listened to `staging`, so PRs from feature branches ran
nothing) and the `test` job gets a `postgres:17` service with the DSN pinned the way the harness
needs. Before that, `cargo test` stopped at the first failing target, so `tests/framework.rs` —
the [[h2-advisory-campaign]] guards — was never reached in CI either.

`cargo fmt --check` used to be red on ~40 pre-existing sites; the whole crate was formatted on
2026-09-13 in a dedicated style commit, so it is clean and must stay clean.
