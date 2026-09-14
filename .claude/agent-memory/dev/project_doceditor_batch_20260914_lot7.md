---
name: doceditor-batch-20260914-lot7
description: Lot 7 — the 7-cycle "operational, not code" finding closed in the repo by SET ROLE, the two-pool split, and the sqlx::migrate! compile-time trap that eats 20 minutes
metadata:
  type: project
---

Lot 7 (`6d69a79` → `8f1f925`), **tenth** dev turn on unit `doceditor-c20260909-1345`, still
PR #3. 87 tests (80 before). CI `34794344875`, 4/4 green.

**The BA report said "no dev cycle needed on doceditor's code" and was right about three of its
four deviations.** Broker config (devops, outside the repo), topic name (human, BR-0002), missing
`spec.md` (`HR-20260913-006` still `PENDING`) — all re-measured, none touched. The fourth,
AC-011, had been filed "operational, not code" for **seven cycles** and was in-repo all along.
The reframing is in [[operational-not-code]]; what belongs here is the mechanics.

**The fix: the service drops its own privileges.** Migration 008 provisions `editor_app`
(`NOLOGIN`, **no attribute named beyond that** — spelling out `NOSUPERUSER`/`NOBYPASSRLS`
requires being a superuser, which `postgres` on Cloud SQL is not, so naming the safe defaults
fails exactly where it matters). `src/main.rs` splits the pool: a **boot** pool keeps the
connection string's privileges for schema + migrations + role probe then closes; the **serving**
pool runs `SET search_path = editor, public; SET ROLE editor_app;` on every connection —
**search_path first**, so the runtime role is never the one resolving the schema.

Three details that are not obvious and each cost thought:

1. **Adoption is *tried*, never inferred.** On PostgreSQL 16+ a role created by a `CREATEROLE`
   user is granted back with `ADMIN` but **without `SET`**, so `pg_has_role(…,'MEMBER')` answers
   true while `SET ROLE` still fails — and the pool would then fail to check out a *single*
   connection. Probe with `SET ROLE …; RESET ROLE;` in one simple-query batch (implicit
   transaction, so a missed `RESET` rolls back with it).
2. **Its failure is never fatal.** A database whose admin role lacks `CREATEROLE` must still
   migrate and still boot; the service falls back to the WARN it logged before.
3. **A role is cluster-wide while sqlx's migration lock is per-database**, so `CREATE ROLE IF NOT
   EXISTS` catches `duplicate_object` as well — same race class as `CREATE SCHEMA IF NOT EXISTS`.

**The decision that makes it real: the test harness adopts the role too.** `setup_test_pool` is
wired exactly like the serving pool, so the whole suite now runs under enforced RLS. A suite
keeping the privileged role would exercise every repository path with the policies switched off —
the same trap as a test that skips when the broker is missing. `setup_admin_pool` carries DDL and
operator fixtures. Fallout was exactly one helper: seeding a **platform** template
(`tenant_id IS NULL`) is refused by the `WITH CHECK` **on purpose**, so it escalates with
`SET LOCAL ROLE NONE` — `LOCAL`, verified in psql, reverts at COMMIT so the connection returns to
the pool still running as `editor_app`. A tenant's own template is written under its own context,
which exercises the `WITH CHECK` instead of stepping around it. **All 80 pre-existing tests passed
unchanged** — so every service write path really does open its tenant transaction; now proven
rather than assumed.

**`sqlx::migrate!` embeds the directory at COMPILE time, and adding a file does not invalidate
the build.** The brand-new migration silently did not run: role absent, `_sqlx_migrations` stopped
at 7, five tests red accusing a migration whose SQL was perfect. Touch a file using the macro
(`src/main.rs`, `tests/common/mod.rs`) or `cargo clean -p`. **CI never sees this** — it always
builds from scratch — so it only ever bites locally and looks exactly like a broken migration.
Cost the first 20 minutes. Written into `CLAUDE.md`.

**Two things worth copying as method:**

- **Measure by hand before writing code, varying exactly one thing.** The whole case rests on four
  `psql` lines where only `current_user` changes. The killer is not "0 rows as `editor_app`" — it
  is `1302 rows across 964 tenants` as `ods` **with the same tenant context set**.
- **Mutation in both directions, on the two halves separately.** Without migration 008 → 5 of 7
  red. With the migration but the `SET ROLE` removed → the **same** 5 red, role test green. That
  partition is what proves provisioning and adoption are each necessary. Plus a permanent
  **non-vacuity witness** in the file (`a_privileged_connection_still_sees_every_tenant`), so an
  empty table can never make the others pass.

**Verified on the running service, not only at the bench** — and this is now habit, not extra:
boot logs `INFO … enforced … role=editor_app session_role=ods` where it logged a WARN since the
service was written, 26 log lines, zero WARN/ERROR, plus a live create → cross-tenant 404 →
version → soft-delete-404-on-all-four-read-paths round trip. Note the local `.env` carries
`JWT_RSA_PUBLIC_KEY_B64=__NEEDS_HUMAN__`, which panics the binary: run it from **outside the repo**
so `dotenvy` finds no `.env`, or pass a real key.

**Still reported, still not fixed:** `archived` freezes the status, not the body (product
question, no spec — BR-0002). Unchanged since lot 6.

**The branch question did not arise this time**: the order said "stay on the current `feat/*`
branch", which is what lots 2–6 already had to argue for. See [[doceditor-batch-20260914]].

See [[doceditor-test-database]] for the standing containers (both were up), and
[[doceditor-batch-20260913]] for lots 1–5.
