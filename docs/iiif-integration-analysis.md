# IIIF Integration Analysis — Comad DAM

**Date:** 2026-06-16  
**Scope:** `ullav-dam-server`, `ullav-dam-browser`, `comad-macos`

---

## What is IIIF?

The International Image Interoperability Framework (IIIF) is a set of open API standards widely adopted by galleries, libraries, archives, and museums (GLAM). Two APIs are relevant to Comad:

- **Presentation API 3.0** — JSON-LD documents (Manifests, Collections) that describe content structurally. Any IIIF-compliant viewer (Universal Viewer, Mirador, Clover) can load a manifest URL and display an asset with its metadata.
- **Image API 3.0** — Parameterised image serving: `/{id}/{region}/{size}/{rotation}/{quality}.{format}`. Enables deep zoom (OpenSeadragon), region selection, and format negotiation.

---

## Current System Capabilities

### Asset data model

| Field | Status |
|---|---|
| `id`, `name`, `description` | ✅ |
| `asset_type` (MIME) | ✅ |
| `creator`, `copyright_notice`, `caption`, `keywords` | ✅ |
| `custom_fields` (JSONB) | ✅ |
| `ocr_text` | ✅ |
| `is_private`, `public_read`, `public_download` | ✅ |
| `width`, `height` | ❌ **missing** — only available in EXIF metadata when present |

### Image serving

Two endpoints exist, neither is parameterised:

- `GET /assets/{id}/thumbnail` — single fixed-size PNG (configurable, typically ~256px); cached to S3
- `GET /assets/{id}/download` — raw binary, no transformation

No region, size, rotation, quality, or format parameters exist anywhere in the system.

### Metadata extraction

Extraction is comprehensive and runs at upload time:

- **EXIF** — all image types (JPEG, PNG, WebP, TIFF, HEIF); camera, exposure, GPS, dimensions
- **IPTC** — JPEG only (APP13/IIM); title, creator, caption, keywords, copyright, city, country
- **XMP** — JPEG only (APP1); Dublin Core (`dc:creator`, `dc:subject`, `dc:rights`, `dc:description`), Photoshop, IPTC Core namespaces

Dublin Core fields are already normalised from XMP. This maps directly to IIIF Manifest metadata.

### Category / collection structure

- Unlimited-depth hierarchy via `parent_id` (self-referential)
- Many-to-many asset↔category via junction table
- Access levels: Private / Group / Global
- API returns direct children only (not recursive)

This maps naturally to nested IIIF Collections.

---

## Gap Analysis

| IIIF Requirement | Status | Notes |
|---|---|---|
| Asset storage (S3/MinIO) | ✅ | |
| Hierarchical categories → Collections | ✅ | Good structural fit |
| Dublin Core / IPTC / XMP metadata | ✅ | Already extracted and normalised |
| `width`/`height` on asset model | ❌ | Required for IIIF `info.json` and Canvas dimensions |
| Parameterised image serving | ❌ | No region/size/rotation/quality/format support |
| `info.json` endpoint | ❌ | |
| Manifest endpoint | ❌ | |
| Collection endpoint | ❌ | |

---

## Recommended Implementation: Two Phases

### Phase 1 — Presentation API (low effort, high value)

**Goal:** Make every asset and collection shareable with any IIIF viewer.

**Changes to `ullav-dam-server`:**

1. **Migration:** Add `width` and `height` columns (`INTEGER`, nullable) to the `assets` table. Populate at upload time by reading image headers (the `image` crate already decodes these; EXIF extraction already normalises them — just needs to be promoted to a first-class column).

2. **`GET /iiif/manifest/{asset_id}`** — returns a IIIF Manifest 3.0 JSON-LD document:
   - `label` from `asset.name`
   - `metadata` from IPTC/XMP fields (creator, copyright, description, keywords, date)
   - Single Canvas sized to `asset.width` × `asset.height`
   - Canvas body pointing to the existing `/assets/{id}/download` URL
   - `thumbnail` pointing to `/assets/{id}/thumbnail`
   - `rights` from `copyright_notice` if set
   - Respects existing visibility flags (`is_private`, `public_read`)

3. **`GET /iiif/collection/{category_id}`** — returns a IIIF Collection JSON-LD:
   - `label` from `category.name`
   - `description` from `category.description`
   - `items` list of manifest stubs for assets in the category + nested collection stubs for subcategories
   - Respects `access_level`

**What this unlocks:** Any external IIIF viewer can consume a manifest URL. Collections of digitised materials become shareable with cultural heritage aggregators (Europeana, DPLA, etc.) that speak IIIF. No image processing work required.

**What it doesn't do:** No deep zoom, no region selection, no tiling. Viewers get the full original image.

### Phase 2 — Image API (moderate effort)

**Goal:** Enable deep zoom and proper tiling in IIIF viewers.

**Changes to `ullav-dam-server`:**

1. **`GET /iiif/image/{id}/info.json`** — IIIF Image API 3.0 service description. Reports `width`, `height`, supported region/size/quality/format parameters, and tile sizes.

2. **`GET /iiif/image/{id}/{region}/{size}/{rotation}/{quality}.{format}`** — Parameterised image delivery. Implemented using the `image` crate (already a dependency) for:
   - Region extraction (`full`, `square`, `x,y,w,h`, `pct:x,y,w,h`)
   - Size scaling (`full`, `max`, `w,`, `,h`, `w,h`, `!w,h`, `pct:n`)
   - Rotation (0, 90, 180, 270; `!` prefix for mirror)
   - Quality (`color`, `gray`, `bitonal`, `default`)
   - Format (`jpg`, `png`, `webp`)
   - Response caching strategy: warm from MinIO, process on-demand, optionally cache derivative tiles to S3

3. **Update Phase 1 manifests** — Canvas body annotations updated to reference Image API URLs instead of download URLs. Viewers can now use tiled deep zoom.

**What this unlocks:** Deep zoom in Universal Viewer / Mirador / OpenSeadragon. Standard IIIF Level 2 compliance. Interop with cultural heritage aggregation pipelines that expect Image API endpoints.

---

## Scope Boundaries

- IIIF **Search API** (full-text search over annotation bodies) — not in scope for either phase; would require structuring `ocr_text` as IIIF annotations, which is a separate piece of work.
- IIIF **Authentication API** — Phase 1 manifests respect existing visibility flags but don't implement the IIIF Auth spec. For public collections this is fine; for gated collections, viewers will need to handle 401 responses gracefully.
- **Audio/video** — IIIF Presentation API 3.0 supports AV content, but Comad's non-image assets currently have no IIIF Image API equivalent. Phase 1 manifests can reference download URLs for AV assets; deep zoom is image-only.

---

## Recommendation

Implement Phase 1 first. It is a pure addition (one migration, two new endpoint groups), touches no existing logic, and immediately delivers the collection management interoperability use case. Phase 2 can follow independently once Phase 1 is in use.
