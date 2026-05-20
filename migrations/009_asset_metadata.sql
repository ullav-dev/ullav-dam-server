CREATE TABLE IF NOT EXISTS asset_metadata (
    asset_id     UUID PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
    exif         JSONB,
    iptc         JSONB,
    xmp          JSONB,
    extracted_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- GIN indexes for containment and path queries on each metadata block
CREATE INDEX IF NOT EXISTS idx_asset_metadata_exif ON asset_metadata USING GIN (exif jsonb_path_ops);
CREATE INDEX IF NOT EXISTS idx_asset_metadata_iptc ON asset_metadata USING GIN (iptc jsonb_path_ops);
CREATE INDEX IF NOT EXISTS idx_asset_metadata_xmp  ON asset_metadata USING GIN (xmp  jsonb_path_ops);

-- Functional btree indexes on GPS coordinates for efficient bounding-box range queries.
-- GIN does not accelerate numeric range predicates, so these are necessary for nearby search.
CREATE INDEX IF NOT EXISTS idx_asset_metadata_gps_lat
    ON asset_metadata (((exif->>'gps_lat')::float8))
    WHERE exif IS NOT NULL AND exif ? 'gps_lat';

CREATE INDEX IF NOT EXISTS idx_asset_metadata_gps_lon
    ON asset_metadata (((exif->>'gps_lon')::float8))
    WHERE exif IS NOT NULL AND exif ? 'gps_lon';
