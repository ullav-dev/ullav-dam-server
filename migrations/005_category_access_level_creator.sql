-- Create access_level enum type (idempotent)
DO $$ BEGIN
    CREATE TYPE access_level AS ENUM ('Private', 'Group', 'Global');
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

ALTER TABLE categories
    ADD COLUMN IF NOT EXISTS access_level access_level NOT NULL DEFAULT 'Private',
    ADD COLUMN IF NOT EXISTS creator      TEXT;

-- Backfill existing rows
UPDATE categories
SET access_level = 'Global',
    creator      = 'colin'
WHERE access_level = 'Private' OR creator IS NULL;
