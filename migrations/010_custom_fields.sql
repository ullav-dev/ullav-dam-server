-- Custom field schema definitions (team-scoped).
-- `key` is the immutable JSONB property slug; `name` is the editable display label.
CREATE TABLE IF NOT EXISTS custom_field_schemas (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    team_id     TEXT        NOT NULL,
    key         TEXT        NOT NULL,
    name        TEXT        NOT NULL,
    field_type  TEXT        NOT NULL CHECK (field_type IN ('String', 'Boolean', 'Integer', 'Float', 'DateTime')),
    required    BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (team_id, key)
);

CREATE INDEX IF NOT EXISTS idx_custom_field_schemas_team ON custom_field_schemas (team_id);

-- Team membership and user-defined metadata on assets.
ALTER TABLE assets ADD COLUMN IF NOT EXISTS team_id       TEXT;
ALTER TABLE assets ADD COLUMN IF NOT EXISTS custom_fields JSONB;

CREATE INDEX IF NOT EXISTS idx_assets_team_id
    ON assets (team_id)
    WHERE team_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_assets_custom_fields
    ON assets USING GIN (custom_fields jsonb_path_ops)
    WHERE custom_fields IS NOT NULL;
