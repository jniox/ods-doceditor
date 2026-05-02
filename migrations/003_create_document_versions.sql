CREATE TABLE IF NOT EXISTS editor.document_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id UUID NOT NULL REFERENCES editor.documents(id),
    tenant_id UUID NOT NULL,
    version INTEGER NOT NULL,
    yjs_snapshot BYTEA NOT NULL,
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    comment VARCHAR(500) NULL,
    snapshot_size_bytes INTEGER NOT NULL,
    is_auto BOOLEAN NOT NULL DEFAULT false,
    UNIQUE (document_id, version)
);

CREATE INDEX IF NOT EXISTS idx_versions_document ON editor.document_versions (document_id, version DESC);
CREATE INDEX IF NOT EXISTS idx_versions_tenant ON editor.document_versions (tenant_id);
