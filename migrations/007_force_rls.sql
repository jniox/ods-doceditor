-- Make the row-level security of migration 005 actually bite.
--
-- Two defects were declared, not fixed, by 005:
--
-- 1. `ENABLE ROW LEVEL SECURITY` exempts the table OWNER. The service connects
--    as the owner of these tables, so every policy was inert for the only role
--    that matters. `FORCE` removes that exemption. (It does NOT remove the
--    exemption of a SUPERUSER or BYPASSRLS role — that one cannot be fixed from
--    SQL; the service now measures it at startup and says so in its logs.)
-- 2. `current_setting('app.tenant_id')` in its strict form RAISES when the
--    setting is absent. A code path that forgot to open a tenant transaction
--    would therefore return 500 instead of returning nothing. The missing_ok
--    form yields NULL, the comparison yields NULL, and the row is not visible —
--    which is the safe reading of "no tenant context".
--
-- A third defect, found by the test that accompanies this migration: 005's
-- existence checks read `WHERE tablename = 'templates' AND policyname =
-- 'tenant_isolation'` with no `schemaname`. On this shared instance
-- `securemail.templates` already carried a policy by that name, so the check
-- matched ANOTHER SERVICE'S policy and editor.templates was left RLS-enabled
-- with no policy on it at all. Every check below is schema-qualified, and
-- creates the policy when it is genuinely absent instead of assuming 005
-- succeeded.
--
-- Additive and idempotent: FORCE is a no-op when already set, ALTER POLICY
-- rewrites in place. No policy is dropped, no data is touched.
ALTER TABLE editor.documents FORCE ROW LEVEL SECURITY;
ALTER TABLE editor.document_versions FORCE ROW LEVEL SECURITY;
ALTER TABLE editor.templates FORCE ROW LEVEL SECURITY;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_policies
        WHERE schemaname = 'editor' AND tablename = 'documents'
          AND policyname = 'tenant_isolation'
    ) THEN
        ALTER POLICY tenant_isolation ON editor.documents
            USING (tenant_id = current_setting('app.tenant_id', true)::uuid)
            WITH CHECK (tenant_id = current_setting('app.tenant_id', true)::uuid);
    ELSE
        CREATE POLICY tenant_isolation ON editor.documents
            USING (tenant_id = current_setting('app.tenant_id', true)::uuid)
            WITH CHECK (tenant_id = current_setting('app.tenant_id', true)::uuid);
    END IF;
END $$;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_policies
        WHERE schemaname = 'editor' AND tablename = 'document_versions'
          AND policyname = 'tenant_isolation'
    ) THEN
        ALTER POLICY tenant_isolation ON editor.document_versions
            USING (tenant_id = current_setting('app.tenant_id', true)::uuid)
            WITH CHECK (tenant_id = current_setting('app.tenant_id', true)::uuid);
    ELSE
        CREATE POLICY tenant_isolation ON editor.document_versions
            USING (tenant_id = current_setting('app.tenant_id', true)::uuid)
            WITH CHECK (tenant_id = current_setting('app.tenant_id', true)::uuid);
    END IF;
END $$;

-- Templates are asymmetric on purpose: a tenant READS its own templates and the
-- platform's system ones (tenant_id IS NULL), but may only WRITE its own —
-- otherwise any tenant could forge a platform-wide template.
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_policies
        WHERE schemaname = 'editor' AND tablename = 'templates'
          AND policyname = 'tenant_isolation'
    ) THEN
        ALTER POLICY tenant_isolation ON editor.templates
            USING (
                tenant_id = current_setting('app.tenant_id', true)::uuid
                OR (tenant_id IS NULL AND is_system = true)
            )
            WITH CHECK (tenant_id = current_setting('app.tenant_id', true)::uuid);
    ELSE
        CREATE POLICY tenant_isolation ON editor.templates
            USING (
                tenant_id = current_setting('app.tenant_id', true)::uuid
                OR (tenant_id IS NULL AND is_system = true)
            )
            WITH CHECK (tenant_id = current_setting('app.tenant_id', true)::uuid);
    END IF;
END $$;
