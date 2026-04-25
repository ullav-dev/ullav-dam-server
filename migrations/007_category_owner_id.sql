ALTER TABLE categories ADD COLUMN IF NOT EXISTS owner_id TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_categories_owner ON categories(owner_id);
-- Backfill existing rows from creator where available
UPDATE categories SET owner_id = creator WHERE owner_id = '' AND creator IS NOT NULL AND creator != '';
