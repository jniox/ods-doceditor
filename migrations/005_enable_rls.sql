-- Enable RLS on all editor tables (idempotent — ALTER TABLE .. ENABLE is safe to re-run)
ALTER TABLE editor.documents ENABLE ROW LEVEL SECURITY;
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_policies WHERE tablename = 'documents' AND policyname = 'tenant_isolation'
    ) THEN
        CREATE POLICY tenant_isolation ON editor.documents
            USING (tenant_id = current_setting('app.tenant_id')::uuid);
    END IF;
END $$;

ALTER TABLE editor.document_versions ENABLE ROW LEVEL SECURITY;
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_policies WHERE tablename = 'document_versions' AND policyname = 'tenant_isolation'
    ) THEN
        CREATE POLICY tenant_isolation ON editor.document_versions
            USING (tenant_id = current_setting('app.tenant_id')::uuid);
    END IF;
END $$;

ALTER TABLE editor.templates ENABLE ROW LEVEL SECURITY;
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_policies WHERE tablename = 'templates' AND policyname = 'tenant_isolation'
    ) THEN
        CREATE POLICY tenant_isolation ON editor.templates
            USING (tenant_id = current_setting('app.tenant_id')::uuid OR tenant_id IS NULL);
    END IF;
END $$;
