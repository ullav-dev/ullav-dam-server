# Metadata Storage Options for EXIF/IPTC/XMP

## Context

Standard file metadata (EXIF, IPTC, XMP) extracted from uploaded assets needs a home that:
- Does not bloat the core `assets` Postgres table
- Is searchable / filterable
- Links cleanly to existing Postgres asset records by UUID

---

## Option 1: Separate Postgres table with JSONB

A dedicated `asset_metadata` table keeps the core `assets` table lean while staying in the same DB:

```sql
CREATE TABLE asset_metadata (
    asset_id     UUID PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
    exif         JSONB,
    iptc         JSONB,
    xmp          JSONB,
    extracted_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_asset_metadata_exif ON asset_metadata USING GIN (exif);
CREATE INDEX idx_asset_metadata_iptc ON asset_metadata USING GIN (iptc);
```

**Search:** GIN indexes support `@>` containment queries and `jsonb_path_query`.
Queries like "all images taken in 2023" or "all Canon shots" work fine.

**Pros:**
- Zero new infrastructure
- Trivial JOINs — everything co-located
- Postgres JSONB is space-efficient
- Cascade delete keeps consistency automatic

**Cons:**
- Bloat concern shifts to a separate table (manageable at this scale)
- Complex nested queries are verbose

---

## Option 2: SurrealDB

Each asset gets a `metadata` record in SurrealDB, linked by asset UUID:

```surql
CREATE metadata:⟨asset-uuid⟩ SET
    exif = { make: "Canon", model: "EOS R5", datetime: "2024-03-01T10:00:00Z", gps: { lat: 53.3, lon: -6.2 } },
    iptc = { headline: "...", keywords: ["landscape", "ireland"] },
    xmp  = { ... };
```

**Search:** SurrealDB `SELECT` supports full `WHERE` on nested fields, range queries,
`CONTAINS` for arrays, and full-text search via `search::score()`.

**Pros:**
- Already in the Ullav ecosystem (Clann app)
- Schema-flexible — no migrations for new metadata fields
- Could become a shared metadata layer across Ullav services
- Graph-style queries possible (e.g. "all assets from this GPS location")

**Cons:**
- Cross-service calls: DAM Rust server needs `surrealdb` crate client
- No FK enforcement across systems — delete consistency must be handled in code
- Two systems to keep in sync

---

## Option 3: S3/MinIO sidecar + Postgres typed index (hybrid)

Store raw metadata as JSON alongside the asset in MinIO:

```
assets/{asset-id}/metadata.json
```

Maintain a small curated Postgres table of fields people actually filter on:

```sql
CREATE TABLE asset_metadata_index (
    asset_id     UUID PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
    captured_at  TIMESTAMPTZ,
    camera_make  TEXT,
    camera_model TEXT,
    gps_lat      DOUBLE PRECISION,
    gps_lon      DOUBLE PRECISION,
    width_px     INT,
    height_px    INT,
    color_space  TEXT
);
```

**Search:** Typed columns with btree/spatial indexes — fastest possible for common queries.
Full raw metadata always available via S3 fetch.

**Pros:**
- Maximum query performance on promoted fields
- Raw metadata preserved losslessly in S3
- Index stays small and fast

**Cons:**
- Must decide upfront which fields to promote
- Adding a new searchable field requires a migration
- Extra S3 read to access full metadata

---

## Option 4: Meilisearch

Index metadata documents keyed by asset UUID; Postgres stays authoritative:

```json
{
  "id": "asset-uuid",
  "exif_make": "Nikon",
  "exif_datetime": "2024-01-15",
  "iptc_keywords": ["architecture", "dublin"]
}
```

**Search:** Best-in-class faceted search, typo tolerance, ranking.

**Pros:**
- Ideal for a rich "search by anything" DAM browser UX
- Faceted filtering panes, relevance ranking built-in

**Cons:**
- Another service to operate
- Sync between Postgres and Meilisearch must be maintained in code
- No transactional consistency

---

## Recommendation

**Short term → Option 1 (JSONB table).**
Zero new infrastructure, survives this scale comfortably, GIN indexes are capable.
Extraction runs inside the existing upload flow in `spawn_blocking`.

**Longer term → Option 3 hybrid.**
As query patterns emerge, promote commonly-filtered fields to a typed index.
The JSONB table (or S3 sidecar) becomes the raw source of truth; the typed index serves queries.

**SurrealDB** makes sense only if a shared metadata layer across the whole Ullav suite
(Clann + DAM) is a goal — otherwise the cross-service consistency overhead isn't justified.

**Meilisearch** is worth adding once the DAM browser needs a dedicated search/filter UX.

---

## Open Question: Overlap with Existing Columns

`creator`, `copyright_notice`, `caption`, and `keywords` already exist in the `assets` table
and overlap with IPTC fields. A policy is needed:

> **Proposed:** treat DB columns as the "user override" layer; extracted metadata is the
> raw source layer. On first extraction, populate DB columns only if they are empty.
> Subsequent re-extractions update only the `asset_metadata` table, not the core `assets` columns.
