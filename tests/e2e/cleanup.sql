-- =============================================================================
-- E2E Test Cleanup for doceditor — REWRITTEN 2026-09-14 (scenario agent)
-- =============================================================================
-- Run AFTER test scenarios complete (pass or fail), as the SAME admin
-- connection string used for mock-data.sql (never as editor_app — it is
-- NOLOGIN and cannot open a session at all; see mock-data.sql header).
--
-- Deletes are scoped by tenant_id, not by the fixed seed ids: this also
-- removes every document/version a scenario CREATED at runtime (capture-driven
-- ids the test harness only learns at run time — e.g. HP-001's "create
-- document" response), not just the rows this fixture pre-seeded. That is
-- deliberate: a cleanup keyed only to fixed ids would leak one row per test
-- run into a database this instance shares with a dozen other ODS schemas.
--
-- Order matters: versions before documents (document_versions.document_id
-- references documents.id, migration 003). Templates have no FK to either and
-- can be removed at any point; the system template is removed separately
-- because tenant_id IS NULL does not match the tenant_id IN (...) filter.
--
-- Pass the SAME -v values used for mock-data.sql:
--   psql "$DATABASE_URL" -v tenant_a_id=... -v tenant_b_id=... -f cleanup.sql
-- =============================================================================

-- See mock-data.sql for why this must be conditional (\if :{?var}) rather than
-- a plain \set -- a plain \set would clobber a real -v override.
\if :{?tenant_a_id}
\else
\set tenant_a_id aaaaaaaa-0000-0000-0000-000000000001
\endif
\if :{?tenant_b_id}
\else
\set tenant_b_id bbbbbbbb-0000-0000-0000-000000000002
\endif

-- Version rows first (FK to documents).
DELETE FROM editor.document_versions
WHERE tenant_id IN (:'tenant_a_id', :'tenant_b_id');

-- Documents (seeded + anything a scenario created and captured at runtime).
DELETE FROM editor.documents
WHERE tenant_id IN (:'tenant_a_id', :'tenant_b_id');

-- Tenant-owned templates.
DELETE FROM editor.templates
WHERE tenant_id IN (:'tenant_a_id', :'tenant_b_id');

-- The system/platform template (tenant_id IS NULL) — matched by fixed id,
-- since it cannot be matched by the tenant_id filter above.
DELETE FROM editor.templates
WHERE id = '11111111-eeee-0000-0000-000000000001'
  AND tenant_id IS NULL
  AND is_system = true;
