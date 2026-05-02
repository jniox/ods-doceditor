CREATE TABLE IF NOT EXISTS editor.templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NULL,
    name VARCHAR(200) NOT NULL,
    description VARCHAR(1000) NULL,
    category VARCHAR(100) NULL,
    content_html TEXT NOT NULL,
    content_yjs BYTEA NULL,
    is_system BOOLEAN NOT NULL DEFAULT false,
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_templates_tenant ON editor.templates (tenant_id) WHERE tenant_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_templates_system ON editor.templates (is_system) WHERE is_system = true;
CREATE INDEX IF NOT EXISTS idx_templates_category ON editor.templates (category) WHERE category IS NOT NULL;
