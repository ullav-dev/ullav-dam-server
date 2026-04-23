ALTER TABLE assets ADD COLUMN IF NOT EXISTS owner_id TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_assets_owner ON assets(owner_id);
