-- =============================================================================
-- E2E Test Mock Data for doceditor — REWRITTEN 2026-09-14 (scenario agent)
-- =============================================================================
--
-- The previous version of this file (dated 2026-05-02) inserted into
-- `editor.tenants` and `editor.users`. NEITHER TABLE EXISTS. Verified against
-- migrations/001..009 on 2026-09-14: this service has exactly three tables —
-- editor.documents, editor.document_versions, editor.templates — and
-- tenant_id / created_by are bare UUID columns with no foreign key and no
-- tenant/user table of their own inside doceditor's schema. Tenant and user
-- identity is owned entirely by OID; doceditor never validates them against
-- anything but the JWT claim (see spec.md §5.6). The old file also inserted
-- into editor.templates with a `content` column — the real column is
-- `content_html`, and `created_by` (NOT NULL) was omitted entirely, which
-- would have failed on any schema that actually enforces its constraints.
--
-- WHO MUST RUN THIS, AND AS WHICH ROLE
-- -------------------------------------------------------------------------
-- Run this as the ADMIN/migration connection string (the DATABASE_URL secret),
-- never as `editor_app`: editor_app is NOLOGIN (migration 008) and cannot open
-- a session at all. The admin role is SUPERUSER/BYPASSRLS on every instance
-- measured so far (see ~/dev/projects/doceditor/CLAUDE.md, "Multi-Tenancy"),
-- so this script bypasses the FORCE ROW LEVEL SECURITY policies of migration
-- 007 by construction — no `SET app.tenant_id` is needed to make these INSERTs
-- visible to their own admin session. (RLS still applies normally to the
-- application's own `editor_app` session when the API serves a request.)
--
--   STAGING (the tagged Cloud Run revision under test):
--     DATABASE_URL=$(gcloud secrets versions access latest \
--       --secret=doceditor-database-url --project=orbus-ods-staging)
--     psql "$DATABASE_URL" -v tenant_a_id=... -v tenant_b_id=... \
--                           -v user_a_id=...   -v user_b_id=...  -f mock-data.sql
--
--   LOCAL DEV (127.0.0.1:5435, HS256 test mode — see repo CLAUDE.md):
--     psql "postgres://ods:ods-dev-2026@127.0.0.1:5435/ods?options=-c%20search_path%3Deditor%2Cpublic" \
--       -f mock-data.sql   # falls back to the fixed UUIDs below (no -v needed)
--
-- WHERE tenant_a_id / tenant_b_id / user_a_id / user_b_id COME FROM
-- -------------------------------------------------------------------------
-- This deployment has JWT_ALLOW_HS256 unset — only RS256 tokens signed by
-- OID's real key validate (spec.md §7.2). There is therefore no "invented"
-- tenant UUID that will ever match a real Authorization header: the rows
-- below must carry the SAME tenant_id / sub the E2E runner's real OID tokens
-- carry, or every scenario gated on {{TOKEN_TENANT_A}} sees an empty RLS view
-- and reads 404/empty-list, not because the code is wrong but because the
-- fixture and the token disagree. Extract them from the tokens themselves
-- (see scenarios.json → "setup" for how {{TOKEN_TENANT_A}} / {{TOKEN_TENANT_B}}
-- are obtained from OID in the first place):
--
--   tenant_a_id=$(echo "$TOKEN_TENANT_A" | cut -d. -f2 | tr '_-' '/+' | base64 -d 2>/dev/null | python3 -c 'import json,sys;print(json.load(sys.stdin)["tenant_id"])')
--   user_a_id=$(echo   "$TOKEN_TENANT_A" | cut -d. -f2 | tr '_-' '/+' | base64 -d 2>/dev/null | python3 -c 'import json,sys;print(json.load(sys.stdin)["sub"])')
--   (repeat for TOKEN_TENANT_B -> tenant_b_id / user_b_id)
--
-- If -v is not passed, the \set defaults below apply. They will NOT match any
-- real OID tenant on the RS256-only staging deployment — every scenario that
-- depends on seeded rows will then read empty/404, which is the expected,
-- fail-closed outcome (RLS hides rows with no matching context; see
-- ADR-003/ADR-005), not a bug in this file. They exist so the file stays
-- syntactically loadable standalone against a local HS256 dev DB, where the
-- harness mints its own test tokens carrying these same fixed UUIDs.
-- =============================================================================

-- IMPORTANT: `\set x default` runs UNCONDITIONALLY, so writing plain \set here
-- would silently CLOBBER a real value passed via `-v tenant_a_id=...` -- caught
-- live in this session's dry run (real overrides were being discarded, rows
-- kept landing on the fixed aaaaaaaa-... default no matter what -v carried).
-- `\if :{?varname}` (psql 10+, "is this variable already defined") makes the
-- default apply ONLY when nothing was passed on the command line.
\if :{?tenant_a_id}
\else
\set tenant_a_id aaaaaaaa-0000-0000-0000-000000000001
\endif
\if :{?tenant_b_id}
\else
\set tenant_b_id bbbbbbbb-0000-0000-0000-000000000002
\endif
\if :{?user_a_id}
\else
\set user_a_id aaaaaaaa-0001-0000-0000-000000000001
\endif
\if :{?user_b_id}
\else
\set user_b_id bbbbbbbb-0001-0000-0000-000000000002
\endif

-- -----------------------------------------------------------------------------
-- Templates (editor.templates — no route writes here; seeded directly, §3.3)
-- -----------------------------------------------------------------------------

-- Platform/system template: tenant_id IS NULL, is_system = true. Visible to
-- EVERY tenant per the asymmetric RLS policy of migration 007 (AC-018).
INSERT INTO editor.templates
  (id, tenant_id, name, description, category, content_html, is_system, created_by)
VALUES
  ('11111111-eeee-0000-0000-000000000001', NULL, 'E2E System Template',
   'Seeded for AC-018 platform-template visibility', 'e2e',
   '<h1>System Template</h1><p>Seeded body.</p>', true, :'user_a_id')
ON CONFLICT (id) DO NOTHING;

-- Tenant A's own template — used by the happy-path "create from template" scenario.
INSERT INTO editor.templates
  (id, tenant_id, name, description, category, content_html, is_system, created_by)
VALUES
  ('aaaaaaaa-eeee-0000-0000-000000000001', :'tenant_a_id', 'E2E Tenant A Template',
   'Seeded for AC-018', 'e2e', '<h1>Tenant A Template</h1><p>Seeded body.</p>',
   false, :'user_a_id')
ON CONFLICT (id) DO NOTHING;

-- Tenant B's own template — Tenant A referencing this id must get the SAME 404
-- as an unknown id (AC-018, AC-028). Never readable/usable by Tenant A.
INSERT INTO editor.templates
  (id, tenant_id, name, description, category, content_html, is_system, created_by)
VALUES
  ('bbbbbbbb-eeee-0000-0000-000000000001', :'tenant_b_id', 'E2E Tenant B Template',
   'Seeded for cross-tenant 404 (AC-018/AC-028)', 'e2e',
   '<h1>Tenant B Template</h1><p>Seeded body.</p>', false, :'user_b_id')
ON CONFLICT (id) DO NOTHING;

-- -----------------------------------------------------------------------------
-- Tenant A documents (editor.documents + editor.document_versions)
-- -----------------------------------------------------------------------------

-- 1. Cross-tenant target: Tenant B must get 404 on every read/write against
--    this id (MT scenarios), and it doubles as Tenant A's own happy-path GET.
INSERT INTO editor.documents
  (id, tenant_id, title, status, created_by, current_version, word_count, metadata, content)
VALUES
  ('dddddddd-a000-0000-0000-000000000001', :'tenant_a_id', 'E2E Cross-Tenant Target',
   'draft', :'user_a_id', 1, 3, '{"category": "e2e", "purpose": "cross-tenant"}',
   'Seeded body content')
ON CONFLICT (id) DO NOTHING;
INSERT INTO editor.document_versions
  (id, document_id, tenant_id, version, content, yjs_snapshot, created_by, comment, snapshot_size_bytes, is_auto)
VALUES
  ('11111111-a000-0000-0000-000000000001', 'dddddddd-a000-0000-0000-000000000001',
   :'tenant_a_id', 1, 'Seeded body content', '', :'user_a_id', NULL, 20, true)
ON CONFLICT (id) DO NOTHING;

-- 2. Published document — for the "published -> archived" valid transition
--    and the "published -> draft" INVALID transition (§5.3).
INSERT INTO editor.documents
  (id, tenant_id, title, status, created_by, current_version, word_count, metadata, content)
VALUES
  ('dddddddd-a000-0000-0000-000000000002', :'tenant_a_id', 'E2E Published Document',
   'published', :'user_a_id', 1, 2, '{}', 'Published body')
ON CONFLICT (id) DO NOTHING;
INSERT INTO editor.document_versions
  (id, document_id, tenant_id, version, content, yjs_snapshot, created_by, comment, snapshot_size_bytes, is_auto)
VALUES
  ('11111111-a000-0000-0000-000000000002', 'dddddddd-a000-0000-0000-000000000002',
   :'tenant_a_id', 1, 'Published body', '', :'user_a_id', NULL, 15, true)
ON CONFLICT (id) DO NOTHING;

-- 3. Archived document — terminal status; every transition attempt from here
--    must be 400 (§5.3, "archived est terminal").
INSERT INTO editor.documents
  (id, tenant_id, title, status, created_by, current_version, word_count, metadata, content)
VALUES
  ('dddddddd-a000-0000-0000-000000000003', :'tenant_a_id', 'E2E Archived Document',
   'archived', :'user_a_id', 1, 2, '{}', 'Archived body')
ON CONFLICT (id) DO NOTHING;
INSERT INTO editor.document_versions
  (id, document_id, tenant_id, version, content, yjs_snapshot, created_by, comment, snapshot_size_bytes, is_auto)
VALUES
  ('11111111-a000-0000-0000-000000000003', 'dddddddd-a000-0000-0000-000000000003',
   :'tenant_a_id', 1, 'Archived body', '', :'user_a_id', NULL, 14, true)
ON CONFLICT (id) DO NOTHING;

-- 4. Already soft-deleted document, WITH two versions — proves AC-026: every
--    one of the three read paths (document, version list, specific version)
--    answers 404, even though the row and its whole history are retained.
INSERT INTO editor.documents
  (id, tenant_id, title, status, created_by, current_version, word_count, metadata, content, deleted_at)
VALUES
  ('dddddddd-a000-0000-0000-000000000004', :'tenant_a_id', 'E2E Soft-Deleted Document',
   'archived', :'user_a_id', 2, 3, '{}', 'Deleted doc body v2', now())
ON CONFLICT (id) DO NOTHING;
INSERT INTO editor.document_versions
  (id, document_id, tenant_id, version, content, yjs_snapshot, created_by, comment, snapshot_size_bytes, is_auto)
VALUES
  ('11111111-a000-0000-0000-000000000004', 'dddddddd-a000-0000-0000-000000000004',
   :'tenant_a_id', 1, 'Deleted doc body v1', '', :'user_a_id', NULL, 18, true),
  ('22222222-a000-0000-0000-000000000004', 'dddddddd-a000-0000-0000-000000000004',
   :'tenant_a_id', 2, 'Deleted doc body v2', '', :'user_a_id', 'pre-deletion snapshot', 18, false)
ON CONFLICT (id) DO NOTHING;

-- 5. Multi-version document (3 immutable versions) — for the history-list and
--    get-specific-version happy paths (AC-006, AC-008), independent of
--    whatever a test run creates and versions itself.
INSERT INTO editor.documents
  (id, tenant_id, title, status, created_by, current_version, word_count, metadata, content)
VALUES
  ('dddddddd-a000-0000-0000-000000000005', :'tenant_a_id', 'E2E Multi-Version Document',
   'draft', :'user_a_id', 3, 4, '{}', 'Third revision body')
ON CONFLICT (id) DO NOTHING;
INSERT INTO editor.document_versions
  (id, document_id, tenant_id, version, content, yjs_snapshot, created_by, comment, snapshot_size_bytes, is_auto)
VALUES
  ('11111111-a000-0000-0000-000000000005', 'dddddddd-a000-0000-0000-000000000005',
   :'tenant_a_id', 1, 'First revision body',  '', :'user_a_id', NULL, 20, true),
  ('22222222-a000-0000-0000-000000000005', 'dddddddd-a000-0000-0000-000000000005',
   :'tenant_a_id', 2, 'Second revision body', '', :'user_a_id', 'explicit snapshot', 21, false),
  ('33333333-a000-0000-0000-000000000005', 'dddddddd-a000-0000-0000-000000000005',
   :'tenant_a_id', 3, 'Third revision body',  '', :'user_a_id', NULL, 20, true)
ON CONFLICT (id) DO NOTHING;

-- -----------------------------------------------------------------------------
-- Tenant B document (baseline for isolation checks — MT scenarios)
-- -----------------------------------------------------------------------------

INSERT INTO editor.documents
  (id, tenant_id, title, status, created_by, current_version, word_count, metadata, content)
VALUES
  ('dddddddd-b000-0000-0000-000000000001', :'tenant_b_id', 'E2E Tenant B Own Document',
   'draft', :'user_b_id', 1, 3, '{"category": "e2e"}', 'Tenant B body content')
ON CONFLICT (id) DO NOTHING;
INSERT INTO editor.document_versions
  (id, document_id, tenant_id, version, content, yjs_snapshot, created_by, comment, snapshot_size_bytes, is_auto)
VALUES
  ('11111111-b000-0000-0000-000000000001', 'dddddddd-b000-0000-0000-000000000001',
   :'tenant_b_id', 1, 'Tenant B body content', '', :'user_b_id', NULL, 21, true)
ON CONFLICT (id) DO NOTHING;

-- =============================================================================
-- Reference map — not SQL, for scenarios.json readers
-- =============================================================================
-- TEMPLATE_SYSTEM_ID      = 11111111-eeee-0000-0000-000000000001
-- TEMPLATE_A_ID            = aaaaaaaa-eeee-0000-0000-000000000001
-- TEMPLATE_B_ID            = bbbbbbbb-eeee-0000-0000-000000000001
-- DOCUMENT_A_CROSSTENANT   = dddddddd-a000-0000-0000-000000000001  (draft, v1)
-- DOCUMENT_A_PUBLISHED     = dddddddd-a000-0000-0000-000000000002  (published, v1)
-- DOCUMENT_A_ARCHIVED      = dddddddd-a000-0000-0000-000000000003  (archived, v1)
-- DOCUMENT_A_DELETED       = dddddddd-a000-0000-0000-000000000004  (soft-deleted, v2, 2 versions)
-- DOCUMENT_A_MULTIVERSION  = dddddddd-a000-0000-0000-000000000005  (draft, v3, 3 versions)
-- DOCUMENT_B_OWN           = dddddddd-b000-0000-0000-000000000001  (draft, v1)
-- =============================================================================
