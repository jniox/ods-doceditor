---
name: doceditor-test-database
description: How doceditor's integration tests reach Postgres and the broker — the 5435 fallback, why the `editor` schema must pre-exist, the shared-instance collisions, and what CI now provides
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

**How to apply:** the harness and `src/main.rs` both ensure the `editor` schema exists **before**
`migrate!` now, so the chicken-and-egg is closed in code rather than by a manual psql gesture. The
`?options=` pin is still what keeps `_sqlx_migrations` in `editor` afterwards; both are needed,
neither is sufficient alone.

**A shared instance that has existed for months hides every fresh-database defect, and CI is where
they surface.** `CREATE SCHEMA IF NOT EXISTS` is **not safe against itself** — the look and the
insert are not atomic, so concurrent racers get `23505` on `pg_namespace_nspname_index` instead of
a no-op. It is unreproducible here (the `editor` schema exists, so the check short-circuits before
any race) and it killed three `api_test` cases the first time CI ever reached the tests, on
2026-09-13. Both call sites go through `repository::schema::ensure_schema_exists` now, which
accepts having lost and then *confirms* the postcondition against `pg_namespace`. **An advisory
lock would not have been enough** — migration 001 runs the same statement under sqlx's own
migration lock, so two different locks are held and can still collide. Generalise it: any
concurrent `CREATE … IF NOT EXISTS` (SCHEMA, ROLE, EXTENSION) must tolerate the duplicate error,
and **a defect that only a virgin database shows will never appear on this instance**. The old `DELETE FROM _sqlx_migrations WHERE description
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

**CI is GREEN, for the first time in the repository's history** — run `34731214292` on
2026-09-13, 4/4 jobs, 70 tests. `gh run list -L 100` shows exactly **one** `success` in a hundred;
everything since 2026-05-02 was `failure` or `startup_failure`. It took three separate fixes, in
this order, each hidden by the one before it: the trigger listened only to `staging` (so a PR to
`dev` ran nothing); then the `lint` and `test` jobs lacked `libcurl4-openssl-dev`, which
`rdkafka`'s `cmake-build` needs and which the Dockerfile's builder stage had always installed — so
nothing compiled at all; then the schema race above. **`tests/ci_workflow.rs` now reads the
workflow's apt list against the Dockerfile's and fails when a `cargo` job installs less.**

Corollary worth keeping: **a green local run proves nothing about CI.** This machine has libcurl's
headers system-wide and a months-old `editor` schema; the runner has neither. When a CI definition
is itself in the diff, read the live run (`gh run view <id> --log-failed`), never a local re-run.

`cargo fmt --check` used to be red on ~40 pre-existing sites; the whole crate was formatted on
2026-09-13 in a dedicated style commit, so it is clean and must stay clean.

**Since 2026-09-13 the suite also needs a BROKER, and it does not skip without one.**
`tests/events_roundtrip.rs` publishes through the real `RedpandaProducer` and consumes back;
`common::broker_addr()` falls back to `127.0.0.1:19092` (announced on stderr) and CI sets
`REDPANDA_BROKERS` explicitly. Start one locally with the `docker run` line in `CLAUDE.md`'s
`## Tests` block — and **remove it afterwards**. CI starts it with `docker run`, not a `services:`
container: GitHub Actions offers no way to pass a command to a service container and
`redpanda start` needs its listener flags. Without a broker the tests fail in **15s**, naming the
address; that is deliberate, see [[doceditor-batch-20260913]].

**The `## Tests` block of `CLAUDE.md` said port 5433 until 2026-09-13** — twenty lines under the
paragraph explaining that 5433 is another project's container. The Database section had been fixed
earlier the same day and this one was missed, which is the general shape of the thing: *a document
that warns about a trap can still contain it.* Trust `docker ps`.
