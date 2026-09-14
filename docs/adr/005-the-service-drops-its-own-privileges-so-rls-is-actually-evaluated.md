# ADR-005: The service drops its own privileges, so row-level security is actually evaluated

**Date**: 2026-09-14  **Status**: accepted
**Amends**: ADR-003 (tenant isolation is defended twice, and the service measures which defence is live)

## Context

ADR-003 built the database half of the isolation and then said, honestly, that
it was not live:

> Under all of that sits a fourth fact that no SQL can fix: the dev role `ods`
> is `SUPERUSER` and `BYPASSRLS`. PostgreSQL does not apply policies to such a
> role at all. Every RLS guarantee this service could write was, on that
> instance, inert.

That sentence is correct, and it was repeated — measured live, with the same
`psql` one-liner, returning the same `t | t` — by **seven consecutive BA review
cycles** between 2026-09-10 and 2026-09-14. Every one of them graded AC-011 a
MEDIUM deviation and closed it the same way: *"remaining fix is an operational
role change (non-superuser, non-BYPASSRLS), not code."*

Seven cycles, no movement. Not because anyone was negligent: the remediation on
file was *provision a role, rotate the secret, redeploy*, and a repository
cannot rotate a secret. The finding had no owner who could act on it, so it was
re-measured and re-filed instead, which costs a review cycle each time and
protects nobody.

**The premise everybody inherited was wrong in one word.** PostgreSQL does not
evaluate policies against the role that *authenticated*; it evaluates them
against the **effective** role, `current_user`. A superuser session that runs
`SET ROLE editor_app` is subject to every policy for the rest of that session.
The privileges in `DATABASE_URL` are a ceiling, not a floor — and a process is
allowed to stand below its own ceiling.

Measured on this instance (PostgreSQL 17, tables owned by the superuser and
marked `FORCE`) before any of this was written:

| session | `current_user` | `SELECT count(*) FROM editor.documents` |
|---|---|---|
| as connected, no tenant context | `ods` | 1278 — policies never evaluated |
| after `SET ROLE`, no tenant context | `editor_app` | **0** — fails closed |
| after `SET ROLE` + `app.tenant_id` | `editor_app` | 5 — that tenant only |
| as connected, **the same** tenant context | `ods` | 1302, across **964** tenants |

The last row is the one that settles it. The tenant context was set in both
cases; nothing about the data, the schema or the policies differs. What changes
the answer is which role asks.

## Decision

**1. The service provisions the unprivileged role itself.** Migration 008
creates `editor_app` — `NOLOGIN`, no attribute spelled out beyond that so the
`CREATE ROLE` defaults (`NOSUPERUSER`, `NOBYPASSRLS`) apply — grants it `USAGE`
on `editor` and DML on its tables, adds `ALTER DEFAULT PRIVILEGES` so a future
table cannot silently become unreadable, and grants the role to the connecting
user so `SET ROLE` is permitted.

**2. The serving pool adopts it on every connection.** `after_connect` runs
`SET search_path = editor, public; SET ROLE editor_app;` — in that order, so the
runtime role is never the one resolving the schema.

**3. Administrative work keeps its privileges, on a separate pool.** A short
boot pool creates the schema, runs the migrations and probes the role, then
closes. Creating a schema is an administrative act; serving a request is not,
and after this ADR they no longer run as the same role.

**4. `NOLOGIN`, deliberately.** The service never authenticates as `editor_app`,
it drops into it. A role that cannot log in is one fewer credential to leak, to
store in Secret Manager, or to rotate.

**5. Adoption is *tried*, never inferred, and its failure is not fatal.**
`runtime_role_is_adoptable` reads `rolsuper`/`rolbypassrls` back from `pg_roles`
— so a role that acquired either out of band is refused rather than trusted —
and then actually executes `SET ROLE …; RESET ROLE;` on a real connection.
Membership is not enough: on PostgreSQL 16+ a role created by a `CREATEROLE`
user is granted back to its creator with `ADMIN` but without `SET`, so
`pg_has_role(…, 'MEMBER')` answers true while the statement still fails — and
the pool would then fail to check out a single connection. When adoption is
impossible the service starts anyway and logs the WARN of ADR-003, unchanged.
A locked-down Neon or Cloud SQL project must still boot.

**6. The test suite runs as the runtime role too.** `setup_test_pool` is wired
exactly like the serving pool. This is the point, not a detail: a suite that ran
as the privileged role would exercise every repository path with the policies
switched off, and a query that forgot its tenant context would stay green here
and fail the day the role is hardened.

## Alternatives considered

**Rotate the secret to a non-privileged LOGIN role (the remediation on file).**
Not rejected — it is *still the stronger configuration*, because it removes the
privileges from the connection string itself, so nothing can `RESET ROLE` back
to them. What is rejected is treating it as a **prerequisite**. It had been the
plan of record for seven review cycles and had produced nothing, because it
requires an actor outside this repository. It is now an improvement on a control
that already works, which is a much better thing for it to be.

**Refuse to start when the policies are not enforced.** ADR-003 considered this
and rejected it because the honest measurement did not exist yet. It is now
*nearly* affordable — enforcement is the normal case on every runner. Still
rejected for this batch, for one reason: the failure mode it guards against
(a database whose admin role lacks `CREATEROLE`) is precisely the one where
refusing to start converts a reportable weakness into an outage. Revisit when
staging and production are both confirmed enforcing; the measurement to gate it
on is already logged at every boot.

**`SET ROLE` per transaction, inside `begin_tenant_tx`.** Rejected. It would
leave every code path that does *not* go through `begin_tenant_tx` running
privileged — which is exactly the class of forgotten path the control exists to
catch. Per connection is the only placement that has no exceptions to remember.

**Drop the redundant `WHERE tenant_id = $n` predicates now that the database
enforces isolation.** Rejected, for the same reason ADR-003 kept both: the
predicates cost nothing and they are what holds if a future deployment lands on
a role that cannot adopt `editor_app`. Two defences, and this ADR only makes the
second one real.

## Consequences

- AC-011 stops being a standing deviation. The startup log changed from
  `WARN  Row-level security is BYPASSED by the database role` to
  `INFO  Row-level security is enforced by PostgreSQL for this role
  role=editor_app session_role=ods` — both roles named, so the privileged half
  of the truth is not hidden by the good news.
- A query that forgets its `tenant_id` predicate now returns **nothing** instead
  of another tenant's rows. That is the whole point, and `tests/rls_enforcement_test.rs`
  asserts it with queries that deliberately carry no predicate of their own.
- The suite grew a **non-vacuity witness** that must keep passing:
  `a_privileged_connection_still_sees_every_tenant` runs the identical
  context-free query on the administrative pool and requires it to see several
  tenants. Without it, an empty table or a wrong schema would satisfy every
  other assertion in the file while proving nothing.
- Fixtures that model an operator rather than a tenant must say so. Seeding a
  **platform** template (`tenant_id IS NULL`) is refused by migration 007's
  `WITH CHECK` — correctly, since that clause exists so no tenant can forge a
  platform-wide template — so `insert_template` escalates with `SET LOCAL ROLE
  NONE`, `LOCAL` so the escalation dies with the transaction. A tenant's own
  template is written under its own context, which exercises the `WITH CHECK`
  instead of stepping around it.
- Any future migration that creates a table in `editor` is covered by the
  `ALTER DEFAULT PRIVILEGES` of migration 008. A table created **outside** a
  migration is not, and would be invisible to the serving pool.
- The generalisable lesson, which is not about PostgreSQL: before writing
  *"operational, outside the repository"* on a finding, ask what the **process
  itself** can do about the condition — drop its own privileges, provision its
  own role, refuse to serve. The same reframing closed the same finding on `oid`
  on 2026-09-13 (migration 020, `oid_app`); this is the second service where the
  word "operational" was hiding an in-repo fix.
