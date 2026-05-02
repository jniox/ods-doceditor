CREATE TABLE IF NOT EXISTS editor.documents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    title VARCHAR(500) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'draft',
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ NULL,
    current_version INTEGER NOT NULL DEFAULT 1,
    word_count INTEGER NOT NULL DEFAULT 0,
    metadata JSONB DEFAULT '{}',
    yjs_state BYTEA NULL
);

CREATE INDEX IF NOT EXISTS idx_documents_tenant_id ON editor.documents (tenant_id);
CREATE INDEX IF NOT EXISTS idx_documents_tenant_status ON editor.documents (tenant_id, status) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_documents_tenant_updated ON editor.documents (tenant_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_documents_title_fts ON editor.documents USING GIN (to_tsvector('english', title));
