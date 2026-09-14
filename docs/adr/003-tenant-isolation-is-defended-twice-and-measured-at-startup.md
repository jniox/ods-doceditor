# ADR-003: Tenant isolation is defended twice, and the service measures which defence is live

**Date**: 2026-09-13  **Status**: accepted

## Context

The platform mandate is that tenant isolation is enforced by PostgreSQL
row-level security, *independently of application code*. DocEditor declared that
it was. The 2026-09-12 security review graded it HIGH, and the reasons were
three distinct failures stacked on one claim:

1. RLS was `ENABLE`d but not `FORCE`d, so the **table owner** — which is the role
   the service connects as — was never subject to its own policies.
2. The policies had no `WITH CHECK`, so a write could place a row in another
   tenant even where a read could not see it.
3. `editor.templates` had RLS enabled and **no policy at all**. Migration 005's
   idempotence guard queried `pg_policies WHERE tablename = 'templates' AND
   policyname = 'tenant_isolation'` with no `schemaname` filter; on the shared
   `ods-postgres` instance, `securemail.templates` already carried a policy of
   that exact name, so the `DO` block saw `EXISTS = true` and skipped creating
   the policy for a different schema's table. The table sat unprotected for
   months and three review cycles read the migration without seeing it.

Under all of that sits a fourth fact that no SQL can fix: the dev role `ods` is
`SUPERUSER` and `BYPASSRLS`. PostgreSQL does not apply policies to such a role at
all. Every RLS guarantee this service could write was, on that instance, inert.

## Decision

**1. Two independent defences, both always on.**
   - *Database*: `FORCE ROW LEVEL SECURITY` on all three tables, policies
     qualified by `schemaname = 'editor'`, each with a `WITH CHECK` as well as a
     `USING` clause, and `current_setting('app.tenant_id', true)` in its
     missing_ok form so an unset setting denies rather than errors.
   - *Application*: every repository query carries an explicit `tenant_id`
     predicate, and every transaction is opened through `begin_tenant_tx`, which
     sets `app.tenant_id` from the JWT claim.

**2. The tenant comes from the token. Never from a body field, never from a
header.** `X-Tenant-Id` is read for logging only.

**3. The service measures its own posture at startup and says so.** It reads
`rolname, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user`
and logs at `info` when the database will enforce the policies, at **`warn`**
when the role bypasses them — naming the role and stating in the message that
isolation currently rests on the application predicates alone.

**4. On a shared PostgreSQL instance, any idempotence guard reading
`pg_policies`, `pg_class`, `pg_indexes` or `information_schema` must filter on
the schema.** Table and policy names are not unique across schemas, and another
ODS service will eventually pick yours.

## Alternatives considered

**Rely on RLS alone, and drop the redundant predicates.** Rejected. It is the
cleaner design and it is what the mandate literally asks for, but it makes the
whole isolation of the service contingent on a deployment-time property — the
privileges of a role — that the code cannot check at compile time and that was
*in fact* false on the instance being reviewed.

**Rely on the predicates alone, and drop RLS as theatre.** Rejected for the
mirror reason: the predicates are correct because someone wrote them correctly
in every query, which is a property that holds until the next query.

**Refuse to start when the role bypasses RLS.** Considered seriously, and
rejected *for now*. It would have made the dev instance unusable for every
developer and every CI run, on a repository whose tests share that instance —
and the honest measurement was not available before this batch, so the first
effect would have been a service that refuses to start with no explanation. A
WARN naming the role is the weaker but truthful version. If the service is ever
confirmed to run under a proper role in staging and prod, this should be
revisited and upgraded to a refusal.

**Fix migration 005 in place.** Rejected: editing an applied migration changes
its checksum and sqlx refuses to run. Migration 007 instead *asserts* the end
state rather than assuming 005 succeeded.

## Consequences

- Deploy this service with a role that is neither `SUPERUSER` nor `BYPASSRLS`.
  Until that is confirmed, the database-level defence is declared and not live,
  and the startup log says which one is which. This is written down rather than
  claimed closed.
- `tests/rls_test.rs` asserts the posture, the policies and the `FORCE` flag by
  querying the catalog. That is what found the missing `templates` policy —
  re-reading the SQL had not, twice. **Write the assertion, do not re-read the
  file.**
- The residual risk is real and bounded: a `tenant_id` predicate missing from a
  future query would not be caught by the database on an instance where the role
  bypasses RLS. `tests/content_test.rs` covers cross-tenant reads for the
  current surface.
- A cross-tenant lookup is answered exactly like an unknown id (404). A
  distinguishable response would be an existence oracle across tenants; this is
  stated in `docs/openapi.yaml` so it is not "fixed" later as a poor error
  message.
