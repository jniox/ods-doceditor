-- =============================================================================
-- Migration 008 — provision the unprivileged role the service runs its queries as
--
-- WHAT WAS STILL WRONG AFTER 007
--
-- Migration 007 marks the three `editor` tables FORCE ROW LEVEL SECURITY, which
-- closes the OWNER exemption. It cannot close the other one: PostgreSQL exempts
-- any role carrying `rolsuper` or `rolbypassrls` from every policy, FORCE or not,
-- and a migration does not choose the role in DATABASE_URL.
--
-- Seven BA cycles (2026-09-10 -> 2026-09-14) re-measured the same live state and
-- filed it again each time, always with the same verdict — "remaining fix is an
-- operational role change, not code":
--
--     dev / CI  — `ods`   rolsuper = t, rolbypassrls = t
--
-- The measurement was right. The verdict was not. Nothing moved in seven cycles
-- because the remediation on file (provision a role, rotate the secret, redeploy)
-- is something a repository cannot do.
--
-- THE PART EVERY CYCLE MISSED
--
-- PostgreSQL evaluates policies against the EFFECTIVE role (`current_user`), not
-- against the role that authenticated. A superuser session that runs
-- `SET ROLE editor_app` is subject to every policy for the rest of that session.
-- Measured on this instance (PostgreSQL 17) before this file was written, on
-- tables owned by the superuser and marked FORCE:
--
--     as connected (ods), no tenant context     -> 1278 rows   policies not evaluated
--     after SET ROLE editor_app, no context     ->    0 rows   fails closed
--     after SET ROLE + app.tenant_id            ->    5 rows   that tenant, and no other
--     as connected (ods), the SAME context      -> 1302 rows across 964 tenants
--
-- The last line is the one that settles it: the tenant context was set in both
-- cases. What changes the answer is which role asks.
--
-- So this migration creates the role, and `src/main.rs` adopts it on every
-- connection of the serving pool (`tenant_context::session_setup`). No secret to
-- rotate, no operator step, and the same posture on a laptop, on CI, on staging
-- and in production.
--
-- Pointing DATABASE_URL at a plain LOGIN role remains the stronger configuration:
-- it removes the privileges from the connection string itself, so nothing can
-- RESET ROLE back to them. This makes that an improvement rather than a
-- PREREQUISITE for the control to exist at all.
--
-- WHY EVERY STEP CATCHES ITS OWN insufficient_privilege
--
-- A database whose administrative role lacks CREATEROLE (a locked-down Neon or
-- Cloud SQL project) must still migrate and still start. It simply will not be
-- able to adopt the role — which `tenant_context::runtime_role_is_adoptable`
-- then reports as false and `log_rls_posture` says out loud at boot, exactly as
-- the service did before this migration existed. A migration that aborted here
-- would turn a reportable weakness into an outage.
--
-- ADDITIVE: no row is read, written or removed; no column, table or policy
-- changes. Idempotent, which on this shared instance is not optional.
-- =============================================================================

-- -- 1. The role ---------------------------------------------------------------
--
-- `NOLOGIN`: the service never authenticates as it, it drops into it. A role that
-- cannot log in is one fewer credential to leak or rotate.
--
-- No attribute is spelled out beyond that, on purpose. CREATE ROLE already
-- defaults to NOSUPERUSER / NOBYPASSRLS / NOCREATEDB / NOCREATEROLE, and naming
-- SUPERUSER or BYPASSRLS in either direction requires being a superuser — which
-- `postgres` on Cloud SQL is NOT. Asserting the safe defaults here would fail
-- exactly where it matters most. `runtime_role_is_adoptable` re-reads `rolsuper`
-- and `rolbypassrls` from `pg_roles` at every boot instead, and refuses to adopt
-- a role that acquired either out of band.
--
-- `duplicate_object` is caught alongside `insufficient_privilege` because roles
-- are CLUSTER-wide while sqlx's migration lock is per-database: the IF NOT EXISTS
-- is a look followed by an insert, and two databases of the same cluster
-- migrating at once are not serialised against each other. Same class of race as
-- `CREATE SCHEMA IF NOT EXISTS` — see src/repository/schema.rs.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'editor_app') THEN
        EXECUTE 'CREATE ROLE editor_app NOLOGIN';
        RAISE NOTICE 'migration 008: created runtime role editor_app';
    END IF;
EXCEPTION
    WHEN duplicate_object THEN
        RAISE NOTICE 'migration 008: editor_app was created concurrently, which is the postcondition';
    WHEN insufficient_privilege THEN
        RAISE WARNING 'migration 008: cannot create the runtime role editor_app (%). Row-level security will NOT be enforced; the service reports this at boot (tenant_context::log_rls_posture).', SQLERRM;
END
$$;

-- -- 2. What the role is allowed to do -----------------------------------------
--
-- Data, and nothing else: no DDL, no ownership, no CREATE on the schema. The
-- migrations keep running as the administrative role, so the tables stay owned by
-- it — which means `editor_app` is not their owner and the policies apply to it
-- even if FORCE were somehow lost.
--
-- ALTER DEFAULT PRIVILEGES covers the tables a FUTURE migration creates, so a new
-- table cannot silently become unreadable by the serving pool. The sequence
-- grants are empty today (the schema has none) and exist for the same reason.
DO $$
BEGIN
    GRANT USAGE ON SCHEMA editor TO editor_app;
    GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA editor TO editor_app;
    GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA editor TO editor_app;
    ALTER DEFAULT PRIVILEGES IN SCHEMA editor GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO editor_app;
    ALTER DEFAULT PRIVILEGES IN SCHEMA editor GRANT USAGE, SELECT ON SEQUENCES TO editor_app;
EXCEPTION
    WHEN insufficient_privilege OR undefined_object THEN
        RAISE WARNING 'migration 008: cannot grant the editor schema to editor_app (%). See the boot-time measurement in tenant_context::log_rls_posture.', SQLERRM;
END
$$;

-- -- 3. Permission to adopt it -------------------------------------------------
--
-- `SET ROLE` is only allowed into a role the session is a member of. The GRANT is
-- issued unconditionally (short of already being that role), and NOT skipped when
-- `pg_has_role(..., 'MEMBER')` already answers true: on PostgreSQL 16+ a role
-- created by a CREATEROLE user is granted back to its creator with ADMIN but
-- WITHOUT SET, so membership can be true while `SET ROLE` still fails. An explicit
-- GRANT defaults to SET TRUE on every supported version and is a no-op when the
-- membership is already complete. `runtime_role_is_adoptable` still *tries* the
-- statement rather than trusting this, because the whole serving pool would
-- otherwise fail to check out a single connection.
DO $$
BEGIN
    IF current_user <> 'editor_app' THEN
        EXECUTE format('GRANT editor_app TO %I', current_user);
    END IF;
EXCEPTION
    WHEN insufficient_privilege OR undefined_object THEN
        RAISE WARNING 'migration 008: cannot grant editor_app to %; the service will not be able to adopt it (%).', current_user, SQLERRM;
END
$$;
