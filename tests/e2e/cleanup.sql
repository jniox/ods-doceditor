-- E2E Test Cleanup for doceditor service
-- Run AFTER test scenarios complete (pass or fail)
-- Order matters: versions before documents, documents before users/tenants

-- Remove all document versions belonging to E2E tenants
DELETE FROM editor.document_versions
WHERE tenant_id IN (
  'aaaaaaaa-0000-0000-0000-000000000001',
  'bbbbbbbb-0000-0000-0000-000000000002'
);

-- Remove all documents created during E2E (seeded + any created by test scenarios)
DELETE FROM editor.documents
WHERE tenant_id IN (
  'aaaaaaaa-0000-0000-0000-000000000001',
  'bbbbbbbb-0000-0000-0000-000000000002'
);

-- Remove templates
DELETE FROM editor.templates
WHERE tenant_id IN (
  'aaaaaaaa-0000-0000-0000-000000000001',
  'bbbbbbbb-0000-0000-0000-000000000002'
);

-- Remove users
DELETE FROM editor.users
WHERE tenant_id IN (
  'aaaaaaaa-0000-0000-0000-000000000001',
  'bbbbbbbb-0000-0000-0000-000000000002'
);

-- Remove tenants last
DELETE FROM editor.tenants
WHERE id IN (
  'aaaaaaaa-0000-0000-0000-000000000001',
  'bbbbbbbb-0000-0000-0000-000000000002'
);
