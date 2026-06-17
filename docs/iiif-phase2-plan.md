# IIIF Phase 2: Image API 3.0 — Implementation Plan

## Overview

Phase 2 adds full IIIF Image API 3.0 support to `ullav-dam-server`, and embeds a
Universal Viewer (UV 4.4, MIT licence) in `ullav-dam-browser` so users can view
assets with deep-zoom directly in the browser without leaving the app.

### New server endpoints

| Endpoint | Purpose |
|---|---|
| `GET /iiif/image/{id}/info.json` | IIIF Image API 3.0 service description |
| `GET /iiif/image/{id}/{region}/{size}/{rotation}/{quality}.{format}` | Parameterised image delivery |

### Phase 1 manifest update

Canvas bodies for raster-image assets will be updated to reference Image API
URLs and include the `service` declaration — enabling deep zoom automatically
in any IIIF viewer that opens the manifest.

---

## Decisions

### Sizes-only (no tiling)

`info.json` advertises a `sizes` array but **no `tiles`**.

Tiled viewers fire dozens of requests per image, each requiring a full S3 fetch
and decode. The `image` crate works on fully decoded `DynamicImage`, so there is
no partial-decode path. Without an on-disk derivative cache a tile storm would
be unusable. Sizes-only gives viewers a handful of whole-image scales —
predictable, affordable, and sufficient for typical archival image sizes.

Tiling with a derivative cache can be added in Phase 3.

### Image sizes

Average-sized images assumed (up to ~50 MP). A configurable
`IIIF_MAX_PIXELS` guard (default 50,000,000) rejects requests whose output
would exceed that limit, preventing memory exhaustion. Large TIFF workflows
are a future concern.

### WebP encoding

`image` 0.25 encodes WebP losslessly. Advertised in `extraFormats`; callers
requesting `webp` get lossless WebP output.

### In-app viewer

Universal Viewer 4.4.0 (MIT) embedded in `ullav-dam-browser` via a
dynamically-imported Next.js component (SSR disabled). Opens in a full-screen
modal from an "Open Viewer" button in `AssetDetails`. External viewers
(UV, Mirador, OpenSeadragon) also work automatically via the updated manifest.

---

## Implementation sequence

### Server (`ullav-dam-server`) — branch `feature/iiif-phase2`

#### S1 — Parameter parsing module (`src/iiif_image.rs`)

Typed enums for all Image API 3.0 Level 2 parameters:

```rust
pub enum Region {
    Full,
    Square,
    Pixels { x: u32, y: u32, w: u32, h: u32 },
    Pct { x: f64, y: f64, w: f64, h: f64 },
}

pub enum Size {
    Max,
    Full,
    Width(u32),
    Height(u32),
    Wh(u32, u32),
    BestFit(u32, u32),  // !w,h — fit within box, preserve aspect ratio
    Pct(f64),
}

pub enum Rotation { Degrees(u16), Mirror(u16) } // Mirror = "!n" prefix

pub enum Quality { Default, Color, Gray, Bitonal }

pub enum Format { Jpg, Png, Webp }
```

Parse from URL path segments. Return `400 Bad Request` with IIIF spec error
body on malformed input. Full unit-test coverage for each variant.

#### S2 — `GET /iiif/image/{id}/info.json`

1. Load asset from DB; private asset → 404.
2. Check `asset_type` — non-raster types (PDF, video) → 501.
3. Ensure `width`/`height` populated (lazy S3 backfill, same as manifest).
4. Compute `sizes` array: halve dimensions until shortest side < 128 px.
5. Return with `Content-Type: application/ld+json` and `Access-Control-Allow-Origin: *`.

Response shape:
```json
{
  "@context": "http://iiif.io/api/image/3/context.json",
  "id": "{PUBLIC_BASE_URL}/iiif/image/{uuid}",
  "type": "ImageService3",
  "protocol": "http://iiif.io/api/image",
  "profile": "level2",
  "width": 4032,
  "height": 3024,
  "sizes": [
    { "width": 4032, "height": 3024 },
    { "width": 2016, "height": 1512 },
    { "width": 1008, "height": 756 },
    { "width": 504,  "height": 378 }
  ],
  "extraFormats": ["png", "webp"]
}
```

#### S3 — `GET /iiif/image/{id}/{region}/{size}/{rotation}/{quality}.{format}`

Route: `"/iiif/image/:id/:region/:size/:rotation/:quality_fmt"` — split the
last segment on `.` to separate quality and format.

Processing pipeline (all in-memory):

1. Parse all five parameters → 400 on error.
2. Load asset from DB (visibility + type check).
3. Check pixel budget: `region.width × region.height` vs `IIIF_MAX_PIXELS` → 400.
4. Fetch original bytes from S3.
5. Decode with `image::load_from_memory()`.
6. **Apply region** — crop to rectangle (pixels or percentage of full dimensions).
7. **Apply size** — `image::imageops::resize()` with Lanczos3.
8. **Apply rotation** — `rotate90/180/270` or `fliph` for mirror.
9. **Apply quality** — `to_luma8()` for gray/bitonal; bitonal adds a 128-threshold pass.
10. **Encode** — write to `Vec<u8>` using appropriate encoder.
11. Return with `Content-Type`, `Cache-Control: public, max-age=86400`, `Access-Control-Allow-Origin: *`.

#### S4 — Update Phase 1 manifest builder

For raster-image assets (`asset_type` starts with `image/`) update the canvas body:

```json
{
  "id": "{PUBLIC_BASE_URL}/iiif/image/{id}/full/max/0/default.jpg",
  "type": "Image",
  "format": "image/jpeg",
  "service": [{
    "id": "{PUBLIC_BASE_URL}/iiif/image/{id}",
    "type": "ImageService3",
    "profile": "level2"
  }]
}
```

Non-image assets keep the download URL body unchanged (no `service`).

#### S5 — Register routes

```rust
.route("/iiif/image/:id/info.json",                        get(get_image_info))
.route("/iiif/image/:id/:region/:size/:rotation/:quality_fmt", get(get_image))
```

Both inherit existing CORS middleware.

#### S6 — OpenAPI / Swagger

`#[utoipa::path]` annotations on both handlers. Add to `ApiDoc` paths + `iiif` tag.

#### S7 — Tests

- `parse_region_*` — all variants + error cases
- `parse_size_*` — all variants
- `parse_rotation_*` — degrees, mirror, invalid
- `parse_format_*`
- `info_json_private_is_404`
- `info_json_non_image_is_501`
- `info_json_structure` — field names, context, profile
- `image_endpoint_full_max` — integration with a test PNG fixture
- `image_endpoint_invalid_region_is_400`
- `image_endpoint_exceeds_max_pixels_is_400`

#### S8 — README update

Add new endpoints to the API routes table and IIIF section.

---

### Browser (`ullav-dam-browser`) — branch `feature/iiif-phase2`

#### B1 — Install dependency

```bash
npm install universalviewer
```

UV 4.4.0 has no peer dependencies that conflict with Next.js 16.

#### B2 — `src/components/IiifViewerInner.tsx`

Client-only component (never SSR) that mounts UV 4 in a container div:

```tsx
"use client";
import { useEffect, useRef } from "react";

export default function IiifViewerInner({ manifestUrl }: { manifestUrl: string }) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!containerRef.current) return;
    // UV 4 API: UV.init(element, options)
    import("universalviewer").then(({ UV }) => {
      const uv = new UV(containerRef.current!, { manifestUri: manifestUrl });
      return () => uv.destroy?.();
    });
  }, [manifestUrl]);

  return <div ref={containerRef} className="w-full h-full" />;
}
```

#### B3 — `src/components/IiifViewerModal.tsx`

Full-screen modal wrapping the viewer, dynamically imported (SSR disabled):

```tsx
import dynamic from "next/dynamic";
const IiifViewerInner = dynamic(() => import("./IiifViewerInner"), { ssr: false });

export function IiifViewerModal({ manifestUrl, onClose }: { manifestUrl: string; onClose: () => void }) {
  return (
    <div className="fixed inset-0 z-50 bg-black/90 flex flex-col">
      <div className="flex justify-end p-2">
        <button onClick={onClose} className="text-white ...">✕ Close</button>
      </div>
      <div className="flex-1 min-h-0">
        <IiifViewerInner manifestUrl={manifestUrl} />
      </div>
    </div>
  );
}
```

#### B4 — Update `AssetDetails.tsx`

Add state:
```tsx
const [viewerOpen, setViewerOpen] = useState(false);
const [viewerManifestUrl, setViewerManifestUrl] = useState<string | null>(null);
```

Add handler (reuses the same manifest fetch as the copy button, or caches the URL):
```tsx
async function handleOpenViewer() {
  const url = await fetchIiifManifestId(asset.id);
  setViewerManifestUrl(url);
  setViewerOpen(true);
}
```

Add "Open Viewer" button in the action bar alongside the existing IIIF copy button
(visible only when `!isPrivate` and asset is an image type):
```tsx
<button onClick={handleOpenViewer} title={t("actionIiifView")} ...>
  <IconOpenViewer />
  <span>{t("actionIiifView")}</span>
</button>
```

Render modal conditionally:
```tsx
{viewerOpen && viewerManifestUrl && (
  <IiifViewerModal manifestUrl={viewerManifestUrl} onClose={() => setViewerOpen(false)} />
)}
```

#### B5 — Translation keys

All three locale files (`en.json`, `de.json`, `ga.json`):

```json
"assetDetails": {
  "actionIiifView": "Open Viewer",
  "actionIiifViewTitle": "Open in Universal Viewer (IIIF deep zoom)"
}
```

#### B6 — Help page update

Add a new card to the existing `iiif` help section:

```json
"help.iiif.viewerTitle": "In-app IIIF Viewer",
"help.iiif.viewerBody": "Public image assets can be opened in the Universal Viewer directly inside this app. Click the 'Open Viewer' button in the asset inspector to launch a full-screen deep-zoom experience."
```

---

## Environment and deployment

No new environment variables, no migrations, no Helm changes required for Phase 2.

The existing `PUBLIC_BASE_URL` configuration drives all Image API URL construction,
exactly as it did for Phase 1 manifests.

---

## Out of scope for Phase 2

- Tiled image delivery / derivative caching (Phase 3)
- Large TIFF support (future project requirement)
- Collection-level viewer (UV can already open collection manifests via the copy URL)
- Mirador or OpenSeadragon as embedded viewers (UV selected; all three work via manifest URL)
