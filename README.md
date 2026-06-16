# ullav-dam-server

A Digital Asset Management (DAM) HTTP API server written in Rust.

## Tech Stack

- **Web:** axum 0.7
- **Database:** tokio-postgres + deadpool-postgres (native SQL, no ORM)
- **Storage:** aws-sdk-s3 against a MinIO instance (path-style, S3-compatible)
- **Runtime:** Tokio
- **API Docs:** utoipa + Swagger UI
- **Image processing:** `image` crate (raster thumbnails), `pdfium-render` (PDF thumbnails), LibreOffice headless (Office document thumbnails), `zip` crate (Apple iWork thumbnails)

## Prerequisites

- Rust (stable)
- PostgreSQL
- MinIO (or any S3-compatible object store)

### Start MinIO with Docker

```bash
docker run -d \
  --name minio \
  -p 9000:9000 \
  -p 9001:9001 \
  -e MINIO_ROOT_USER=minioadmin \
  -e MINIO_ROOT_PASSWORD=minioadmin \
  minio/minio server /data --console-address ":9001"
```

MinIO API: `http://localhost:9000` | Console: `http://localhost:9001`

## Setup

```bash
cp .env.example .env
# Edit .env with your database and storage credentials
cargo run
```

Migrations run automatically on startup. The S3 bucket is created automatically if it does not exist.

## Commands

```bash
cargo build        # compile
cargo run          # run the server
cargo test         # run unit tests
cargo clippy       # lint
cargo fmt          # format
```

## API Docs

Swagger UI is available at **`http://localhost:8080/docs`** when the server is running.

The raw OpenAPI JSON spec is at `http://localhost:8080/api-doc/openapi.json`.

## Authentication

All endpoints require a JWT in the `Authorization: Bearer <token>` header, issued by `ullav-user-management`. The token must include a `subscriptions` claim granting Comad DAM access (or an `admin` role to bypass plan checks). Plan tiers determine which file types may be uploaded and enforce per-user asset count and storage quotas.

## API Routes

| Method | Path | Description |
|--------|------|-------------|
| GET | `/assets` | List all assets |
| POST | `/assets` | Create asset record (metadata only) |
| POST | `/assets/upload` | Create asset + upload file in one request |
| GET | `/assets/:id` | Get asset with its categories |
| PUT | `/assets/:id` | Update asset metadata |
| DELETE | `/assets/:id` | Delete asset and remove from storage (403 if locked) |
| POST | `/assets/:id/upload` | Upload file for an existing asset |
| GET | `/assets/:id/download` | Download asset file |
| GET | `/assets/:id/thumbnail` | Get resized thumbnail (PNG for images/PDFs/Office/iWork, SVG icon otherwise) |
| POST | `/assets/:asset_id/categories/:category_id` | Add category to asset |
| DELETE | `/assets/:asset_id/categories/:category_id` | Remove category from asset |
| GET | `/usage` | Current asset count, bytes used, and plan limits for the authenticated user |
| POST | `/zip/upload` | Batch-import a ZIP archive — creates categories from directories, uploads all files as assets |
| GET | `/categories` | List all categories |
| POST | `/categories` | Create category |
| GET | `/categories/:id` | Get category with sub-categories |
| PUT | `/categories/:id` | Update category |
| DELETE | `/categories/:id` | Delete category |
| GET | `/iiif/manifest/:id` | IIIF Presentation API 3.0 Manifest for a public asset (no auth required; private assets return 404) |
| GET | `/iiif/collection/:id` | IIIF Presentation API 3.0 Collection for a non-private category (no auth required; Private categories return 404) |
| GET | `/iiif/image/:id/info.json` | IIIF Image API 3.0 service description (Level 2, sizes-only) for a public raster image |
| GET | `/iiif/image/:id/:region/:size/:rotation/:quality.format` | Parameterised image delivery per IIIF Image API 3.0 |

### Upload an asset (single request)

`POST /assets/upload` accepts `multipart/form-data` with the following fields:

| Field | Required | Description |
|-------|----------|-------------|
| `file` | yes | The file to upload |
| `name` | no | Asset name — defaults to the filename |
| `asset_type` | no | MIME type — inferred from file extension if omitted |
| `description` | no | Free-text description |
| `caption` | no | Display caption |
| `keywords` | no | Comma-separated keywords |
| `creator` | no | Author / creator name |
| `copyright_notice` | no | Copyright string |
| `available` | no | `true` or `false` (default `true`) |
| `available_until` | no | ISO 8601 datetime after which the asset is unavailable |
| `is_locked` | no | `true` or `false` — locked assets cannot be deleted (default `false`) |
| `is_private` | no | `true` or `false` — asset is private to the uploader (default `true`) |
| `public_read` | no | `true` or `false` — allow unauthenticated read (default `false`) |
| `public_download` | no | `true` or `false` — allow unauthenticated download (default `false`) |
| `public_write` | no | `true` or `false` — allow unauthenticated write (default `false`) |

```bash
curl -X POST http://localhost:8080/assets/upload \
  -F "file=@photo.png" \
  -F "description=Profile photo"
```

### Batch-import a ZIP archive

`POST /zip/upload` accepts `multipart/form-data` with the following fields:

| Field | Required | Description |
|-------|----------|-------------|
| `file` | yes | The ZIP archive to import |
| `creator` | no | Author / creator name — applied to all assets extracted from the archive |

The archive's directory structure is mirrored as a category tree (root category named `<stem>-<YYYYMMDD-HHMMSS>`). Every file becomes an asset linked to its directory's category. macOS artifacts (`__MACOSX/`, `.DS_Store`, `._*`) are skipped. The ZIP file itself is never stored as an asset.

```bash
curl -X POST http://localhost:8080/zip/upload \
  -F "file=@photos.zip" \
  -F "creator=colin"
```

## IIIF Presentation API 3.0 + Image API 3.0

Public assets and non-private categories are accessible as [IIIF](https://iiif.io) resources. All IIIF endpoints require no authentication and return `Access-Control-Allow-Origin: *` so any IIIF-compatible viewer can load them cross-origin.

### Presentation API 3.0

| Endpoint | Returns |
|----------|---------|
| `GET /iiif/manifest/:id` | Manifest for a single public asset — includes label, metadata, thumbnail, and a painting Annotation; raster image assets include an `ImageService3` service reference enabling deep zoom |
| `GET /iiif/collection/:id` | Collection for a non-Private category — includes manifest stubs for all public assets and sub-collection stubs for child categories |

The manifest `id` field contains the fully-qualified canonical URL (derived from `PUBLIC_BASE_URL`). This is the URL to share with external viewers such as [Universal Viewer](https://universalviewer.io) or [Mirador](https://projectmirador.org).

### Image API 3.0 (Level 2)

| Endpoint | Returns |
|----------|---------|
| `GET /iiif/image/:id/info.json` | Service description — `ImageService3`, `level2` profile, computed `sizes` (halved down to ≥ 128 px), `extraFormats: ["png", "webp"]` |
| `GET /iiif/image/:id/{region}/{size}/{rotation}/{quality}.{format}` | Parameterised image delivery |

**Parameters:**
- **region** — `full` · `square` · `x,y,w,h` · `pct:x,y,w,h`
- **size** — `max` · `w,` · `,h` · `w,h` · `!w,h` (best fit) · `pct:n` · prefix `^` to allow upscaling
- **rotation** — `0` · `90` · `180` · `270`; prefix `!` for horizontal mirror (e.g. `!90`)
- **quality** — `default` · `color` · `gray` · `bitonal`
- **format** — `jpg` · `png` · `webp` (lossless)

Image processing runs on a blocking thread (Lanczos3 filter). A pixel budget guard rejects images exceeding 50 MP.

**Visibility rules** — Private assets return `404`; non-image assets (`image/svg+xml` included) return `501 Not Implemented`.

**Dimension backfill** — If an asset was uploaded before the `width`/`height` migration, dimensions are fetched from S3 on the first manifest or `info.json` request, decoded from the image header, and persisted so subsequent requests are fast.

## Data Model

- **Asset** — `id`, `owner_id` (JWT `sub` of the uploader), `name`, `description`, `asset_type`, `size` (bytes), `storage_key`, `bucket`, `caption`, `keywords`, `creator`, `copyright_notice`, `available` (bool, default `true`), `available_until` (nullable timestamptz), `is_locked` (bool, default `false`), `is_private` (bool, default `true`), `public_read`/`public_download`/`public_write` (bool, default `false`), `width`/`height` (nullable int — pixel dimensions of raster images, populated at upload time; backfilled lazily on first IIIF manifest request), `ocr_text` (nullable text — full text extracted by the macOS client via Vision framework), timestamps
- **Category** — `id`, `name`, `description`, `parent_id` (nullable self-FK for sub-categories), `access_level` (enum: `Private`/`Group`/`Global`, default `Private`), `creator` (nullable text — username of creator), timestamps. **Note**: any SQL query that SELECTs category rows and maps them through `Category::from` must include `creator` and `access_level` in the column list, or the handler will panic with an ECONNRESET at the client.
- **asset_categories** — M2M junction table linking assets to categories

## Production Deployment

```bash
docker network create ullav-net   # one-time, shared across Ullav services
cp .env.prod.example .env.prod
# Fill in .env.prod with real credentials
docker compose -f docker-compose-prod.yaml up -d --build
```

The image includes LibreOffice (Office thumbnail conversion) and a prebuilt PDFium binary (PDF thumbnail rendering). MinIO is configured via env vars and is expected to be running separately on `ullav-net`.

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | yes | — | PostgreSQL connection string |
| `JWT_SECRET` | yes | — | HS256 secret shared with `ullav-user-management` |
| `S3_ENDPOINT` | yes | — | S3/MinIO endpoint URL |
| `S3_ACCESS_KEY_ID` | yes | — | S3 access key |
| `S3_SECRET_ACCESS_KEY` | yes | — | S3 secret key |
| `S3_BUCKET` | no | `dam-assets` | S3 bucket name |
| `S3_REGION` | no | `us-east-1` | S3 region |
| `HOST` | no | `0.0.0.0` | Bind host |
| `PORT` | no | `8080` | Bind port |
| `THUMBNAIL_SIZE` | no | `256` | Max width/height of generated thumbnails (px) |
| `PDFIUM_LIB_PATH` | no | system | Path to the PDFium shared library (`.so`/`.dylib`) for PDF thumbnails |
| `SOFFICE_PATH` | no | `soffice` | Path to the LibreOffice binary for Office document thumbnails |
| `PUBLIC_BASE_URL` | no | `http://localhost:8080` | Canonical public URL of this server — used to build absolute URIs in IIIF manifests and collections. Set to the externally reachable URL (e.g. `https://comad-tip.stage.ullav.setanta.dev`). Also readable from a Docker secret via `PUBLIC_BASE_URL_FILE`. |
