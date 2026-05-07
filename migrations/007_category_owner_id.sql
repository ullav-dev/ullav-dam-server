ALTER TABLE categories ADD COLUMN IF NOT EXISTS owner_id TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_categories_owner ON categories(owner_id);
