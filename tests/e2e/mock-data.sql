-- E2E Test Mock Data for doceditor service
-- Run BEFORE executing test scenarios
-- Tenant isolation: Tenant A owns documents, Tenant B is used for cross-tenant rejection tests

-- Tenants
INSERT INTO editor.tenants (id, name) VALUES
  ('aaaaaaaa-0000-0000-0000-000000000001', 'Tenant A E2E'),
  ('bbbbbbbb-0000-0000-0000-000000000002', 'Tenant B E2E')
ON CONFLICT (id) DO NOTHING;

-- Users
INSERT INTO editor.users (id, email, tenant_id) VALUES
  ('aaaaaaaa-0001-0000-0000-000000000001', 'e2e-user-a@tenant-a.test', 'aaaaaaaa-0000-0000-0000-000000000001'),
  ('bbbbbbbb-0001-0000-0000-000000000002', 'e2e-user-b@tenant-b.test', 'bbbbbbbb-0000-0000-0000-000000000002')
ON CONFLICT (id) DO NOTHING;

-- Template for Tenant A (used in HP-002)
INSERT INTO editor.templates (id, tenant_id, name, content) VALUES
  ('aaaaaaaa-aaaa-0000-0000-000000000001', 'aaaaaaaa-0000-0000-0000-000000000001', 'E2E Template A', '')
ON CONFLICT (id) DO NOTHING;

-- Base document for Tenant A (used in MT, VAL, ERR scenarios)
INSERT INTO editor.documents (id, tenant_id, title, status, created_by, current_version, word_count, metadata) VALUES
  (
    'dddddddd-aaaa-0000-0000-000000000001',
    'aaaaaaaa-0000-0000-0000-000000000001',
    'E2E Base Document A',
    'draft',
    'aaaaaaaa-0001-0000-0000-000000000001',
    1,
    0,
    '{"category": "e2e"}'
  )
ON CONFLICT (id) DO NOTHING;

-- Draft document for status transition test (HP-008)
INSERT INTO editor.documents (id, tenant_id, title, status, created_by, current_version, word_count, metadata) VALUES
  (
    'dddddddd-aaaa-0000-0000-000000000002',
    'aaaaaaaa-0000-0000-0000-000000000001',
    'E2E Draft Document (to publish)',
    'draft',
    'aaaaaaaa-0001-0000-0000-000000000001',
    1,
    0,
    '{}'
  )
ON CONFLICT (id) DO NOTHING;

-- Published document for status transition test (HP-009, VAL-005)
INSERT INTO editor.documents (id, tenant_id, title, status, created_by, current_version, word_count, metadata) VALUES
  (
    'dddddddd-aaaa-0000-0000-000000000003',
    'aaaaaaaa-0000-0000-0000-000000000001',
    'E2E Published Document',
    'published',
    'aaaaaaaa-0001-0000-0000-000000000001',
    1,
    0,
    '{}'
  )
ON CONFLICT (id) DO NOTHING;

-- Archived document for invalid transition test (VAL-006, VAL-007)
INSERT INTO editor.documents (id, tenant_id, title, status, created_by, current_version, word_count, metadata) VALUES
  (
    'dddddddd-aaaa-0000-0000-000000000004',
    'aaaaaaaa-0000-0000-0000-000000000001',
    'E2E Archived Document',
    'archived',
    'aaaaaaaa-0001-0000-0000-000000000001',
    1,
    0,
    '{}'
  )
ON CONFLICT (id) DO NOTHING;

-- Document to delete in HP-011
INSERT INTO editor.documents (id, tenant_id, title, status, created_by, current_version, word_count, metadata) VALUES
  (
    'dddddddd-aaaa-0000-0000-000000000005',
    'aaaaaaaa-0000-0000-0000-000000000001',
    'E2E Document to Delete',
    'draft',
    'aaaaaaaa-0001-0000-0000-000000000001',
    1,
    0,
    '{}'
  )
ON CONFLICT (id) DO NOTHING;

-- Document to soft-delete and verify 404 in HP-012
INSERT INTO editor.documents (id, tenant_id, title, status, created_by, current_version, word_count, metadata) VALUES
  (
    'dddddddd-aaaa-0000-0000-000000000006',
    'aaaaaaaa-0000-0000-0000-000000000001',
    'E2E Document for Soft Delete Verification',
    'draft',
    'aaaaaaaa-0001-0000-0000-000000000001',
    1,
    0,
    '{}'
  )
ON CONFLICT (id) DO NOTHING;

-- Document with an existing version for HP-015, HP-016, ERR-002
INSERT INTO editor.documents (id, tenant_id, title, status, created_by, current_version, word_count, metadata) VALUES
  (
    'dddddddd-aaaa-0000-0000-000000000007',
    'aaaaaaaa-0000-0000-0000-000000000001',
    'E2E Versioned Document',
    'draft',
    'aaaaaaaa-0001-0000-0000-000000000001',
    1,
    0,
    '{}'
  )
ON CONFLICT (id) DO NOTHING;

-- Version 1 for the versioned document
INSERT INTO editor.document_versions (id, document_id, tenant_id, version, yjs_snapshot, created_by, comment, snapshot_size_bytes, is_auto) VALUES
  (
    'vvvvvvvv-aaaa-0000-0000-000000000001',
    'dddddddd-aaaa-0000-0000-000000000007',
    'aaaaaaaa-0000-0000-0000-000000000001',
    1,
    '',
    'aaaaaaaa-0001-0000-0000-000000000001',
    'Initial version',
    0,
    false
  )
ON CONFLICT (id) DO NOTHING;

-- Variable substitution map (reference only — not SQL)
-- TENANT_A_ID  = aaaaaaaa-0000-0000-0000-000000000001
-- TENANT_B_ID  = bbbbbbbb-0000-0000-0000-000000000002
-- USER_A_ID    = aaaaaaaa-0001-0000-0000-000000000001
-- USER_B_ID    = bbbbbbbb-0001-0000-0000-000000000002
-- TEMPLATE_A_ID         = aaaaaaaa-aaaa-0000-0000-000000000001
-- DOCUMENT_A_ID         = dddddddd-aaaa-0000-0000-000000000001
-- DOCUMENT_DRAFT_ID     = dddddddd-aaaa-0000-0000-000000000002
-- DOCUMENT_PUBLISHED_ID = dddddddd-aaaa-0000-0000-000000000003
-- DOCUMENT_ARCHIVED_ID  = dddddddd-aaaa-0000-0000-000000000004
-- DOCUMENT_DELETE_ID    = dddddddd-aaaa-0000-0000-000000000005
-- DOCUMENT_SOFTDELETE_ID= dddddddd-aaaa-0000-0000-000000000006
-- DOCUMENT_VERSIONED_ID = dddddddd-aaaa-0000-0000-000000000007
