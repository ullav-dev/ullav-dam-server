ALTER TABLE assets
    ADD COLUMN IF NOT EXISTS caption           TEXT,
    ADD COLUMN IF NOT EXISTS keywords          TEXT,
    ADD COLUMN IF NOT EXISTS creator           TEXT,
    ADD COLUMN IF NOT EXISTS copyright_notice  TEXT,
    ADD COLUMN IF NOT EXISTS available         BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN IF NOT EXISTS available_until   TIMESTAMPTZ;
