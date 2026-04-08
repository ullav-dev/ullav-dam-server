# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`ullav-dam-server` is a Digital Asset Management (DAM) HTTP API server written in Rust.

**Tech stack:**
- **Web:** axum 0.7 (with multipart support)
- **Database:** tokio-postgres + deadpool-postgres (native SQL, no ORM)
- **Storage:** aws-sdk-s3 against a MinIO instance (path-style, S3-compatible)
- **Runtime:** Tokio
- **API Docs:** utoipa + Swagger UI (served at `/docs`, spec at `/api-doc/openapi.json`)
- **Image processing:** `image` crate (thumbnail generation), `pdfium-render` (PDF first-page thumbnail), LibreOffice headless (Office document → PDF → thumbnail), `zip` crate (Apple iWork QuickLook thumbnail extraction)
- **MIME inference:** `mime_guess` (file extension → MIME type)

## Commands

```bash
cargo build                    # compile
cargo run                      # run the server (requires .env or env vars)
cargo test                     # run all tests
cargo test <test_name>         # run a single test by name
cargo clippy                   # lint
cargo fmt                      # format code
```

Copy `.env.example` to `.env` and fill in values before running.

Migrations run automatically on startup via `db::run_migrations` (idempotent `CREATE TABLE IF NOT EXISTS`).

MinIO can be started with Docker:
```bash
docker run -d --name minio -p 9000:9000 -p 9001:9001 \
  -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
  minio/minio server /data --console-address ":9001"
```

## Architecture

```
src/
  main.rs          – AppState (db, storage, thumbnail_cache, thumbnail_size), ThumbnailCache type,
                     axum Router (incl. SwaggerUi), ApiDoc, server startup
  config.rs        – Config loaded from env vars (incl. THUMBNAIL_SIZE)
  db.rs            – deadpool-postgres pool creation + migration runner (runs all migrations in order)
  storage.rs       – StorageClient wrapping aws-sdk-s3 (upload/download/delete/presign)
  error.rs         – AppError enum, ErrorResponse schema, impl IntoResponse
  models/
    asset.rs       – Asset, AssetWithCategories, CreateAssetRequest, UpdateAssetRequest
    category.rs    – AccessLevel enum, Category, CategoryWithChildren, CreateCategoryRequest, UpdateCategoryRequest
  handlers/
    assets.rs      – CRUD + file upload/download + category membership + thumbnail endpoints
    categories.rs  – CRUD endpoints for categories
    zip.rs         – POST /zip/upload: batch-imports a ZIP archive, creating categories from directories and assets from files
migrations/
  001_initial.sql  – Schema: assets, categories (self-ref parent_id), asset_categories (M2M)
  002_asset_metadata_fields.sql – Adds caption, keywords, creator, copyright_notice, available, available_until
  003_asset_is_locked.sql       – Adds is_locked (BOOLEAN NOT NULL DEFAULT FALSE)
  004_asset_visibility.sql      – Adds is_private (default true), public_read/download/write (default false)
  005_category_access_level_creator.sql – Adds access_level enum (Private/Group/Global, default Private) and creator (nullable TEXT) to categories
```

## Data Model

- **Asset** – `id`, `name`, `description`, `asset_type`, `size` (bytes, BIGINT), `storage_key`, `bucket`, `caption`, `keywords`, `creator`, `copyright_notice`, `available` (bool, default true), `available_until` (nullable timestamptz), `is_locked` (bool, default false), `is_private` (bool, default true), `public_read`/`public_download`/`public_write` (bool, default false), timestamps
- **Category** – `id`, `name`, `description`, `parent_id` (nullable self-FK for sub-categories), `access_level` (enum: `Private`/`Group`/`Global`, default `Private`), `creator` (nullable text), timestamps
- **asset_categories** – M2M junction table (asset_id, category_id)

## API Routes

| Method | Path | Description |
|--------|------|-------------|
| GET | `/assets` | List all assets |
| POST | `/assets` | Create asset record (metadata only) |
| POST | `/assets/upload` | Create asset + upload file in one multipart request |
| GET | `/assets/:id` | Get asset + its categories |
| PUT | `/assets/:id` | Update asset metadata |
| DELETE | `/assets/:id` | Delete asset + remove from storage (403 if locked) |
| POST | `/assets/:id/upload` | Upload file for an existing asset |
| GET | `/assets/:id/download` | Download file bytes |
| GET | `/assets/:id/thumbnail` | Get resized thumbnail (PNG) or SVG fallback icon |
| POST | `/assets/:asset_id/categories/:category_id` | Add category to asset |
| DELETE | `/assets/:asset_id/categories/:category_id` | Remove category from asset |
| POST | `/zip/upload` | Batch-import a ZIP archive — categories from directories, assets from files |
| GET | `/categories` | List all categories |
| POST | `/categories` | Create category |
| GET | `/categories/:id` | Get category + direct sub-categories |
| PUT | `/categories/:id` | Update category |
| DELETE | `/categories/:id` | Delete category |
| GET | `/docs` | Swagger UI |
| GET | `/api-doc/openapi.json` | OpenAPI 3.0 spec |

## Key Patterns

- All handlers take `State(state): State<AppState>` — clone is cheap (Arc-backed pool + S3 client).
- Database rows are converted to model structs via `impl From<&Row> for T`.
- `AppError` implements `IntoResponse`; handlers return `AppResult<T>` (alias for `Result<T, AppError>`). Variants: `NotFound` → 404, `BadRequest` → 400, `Forbidden` → 403, `Database`/`Pool`/`Storage`/`Internal` → 500.
- `ErrorResponse` in `error.rs` is the shared OpenAPI schema for all error response bodies.
- All model structs and request/response types derive `ToSchema` for OpenAPI generation.
- Handler functions carry `#[utoipa::path(...)]` annotations; `ApiDoc` in `main.rs` aggregates them.
- Asset upload flows:
  - Two-step: `POST /assets` creates record with a `pending/` storage key → `POST /assets/:id/upload` uploads file and updates `storage_key` + `size`.
  - One-step: `POST /assets/upload` accepts multipart with `file`, optional `name` (defaults to filename), optional `asset_type` (inferred from extension via `mime_guess`), optional `description`. Uploads to S3 first, then inserts DB record.
- Thumbnail generation: `GET /assets/:id/thumbnail` checks the in-memory `ThumbnailCache` first (read lock), downloads from S3 on miss, and renders inside `spawn_blocking` (CPU-bound work must not block the async runtime):
  - **Raster images** — decoded and resized with the `image` crate (Lanczos3), returned as PNG.
  - **PDFs** — first page rendered via `pdfium-render` (wraps Google PDFium). Set `PDFIUM_LIB_PATH` to the `.so`/`.dylib` path; falls back to the system library if unset.
  - **Office documents** (`.docx`/`.doc`, `.xlsx`/`.xls`, `.pptx`/`.ppt`) — written to a temp file, converted to PDF by LibreOffice headless (`soffice --headless --convert-to pdf`), then rendered via pdfium. Set `SOFFICE_PATH` to override the binary location (defaults to `soffice` on `$PATH`).
  - **Apple iWork** (Pages, Numbers, Keynote) — extracted from the ZIP archive's embedded QuickLook thumbnail (`QuickLook/Thumbnail.jpg` or `.png`) using the `zip` crate, then resized with the `image` crate.
  - **Everything else** (SVG, video, audio, pending uploads) — returns a type-appropriate SVG fallback icon immediately, no download attempted.
  - On any render failure the fallback icon is returned instead of an error. Successful PNGs are written to cache with `Cache-Control: public, max-age=86400`.
- Branch policy: all development happens on `claude-work`; do not commit to `main`.
