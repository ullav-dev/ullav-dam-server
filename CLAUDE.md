# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`ullav-dam-server` is a Digital Asset Management (DAM) HTTP API server written in Rust.

**Tech stack:**
- **Web:** axum 0.7 (with multipart support)
- **Database:** tokio-postgres + deadpool-postgres (native SQL, no ORM)
- **Storage:** aws-sdk-s3 against a MinIO instance (path-style, S3-compatible)
- **Runtime:** Tokio

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

## Architecture

```
src/
  main.rs          – AppState assembly, axum Router, server startup
  config.rs        – Config loaded from env vars
  db.rs            – deadpool-postgres pool creation + migration runner
  storage.rs       – StorageClient wrapping aws-sdk-s3 (upload/download/delete/presign)
  error.rs         – AppError enum, impl IntoResponse (maps to HTTP status codes)
  models/
    asset.rs       – Asset, AssetWithCategories, CreateAssetRequest, UpdateAssetRequest
    category.rs    – Category, CategoryWithChildren, CreateCategoryRequest, UpdateCategoryRequest
  handlers/
    assets.rs      – CRUD + file upload/download + category membership endpoints
    categories.rs  – CRUD endpoints for categories
migrations/
  001_initial.sql  – Schema: assets, categories (self-ref parent_id), asset_categories (M2M)
```

## Data Model

- **Asset** – `id`, `name`, `description`, `asset_type`, `size` (bytes, BIGINT), `storage_key`, `bucket`, timestamps
- **Category** – `id`, `name`, `description`, `parent_id` (nullable self-FK for sub-categories), timestamps
- **asset_categories** – M2M junction table (asset_id, category_id)

## API Routes

| Method | Path | Description |
|--------|------|-------------|
| GET | `/assets` | List all assets |
| POST | `/assets` | Create asset record (metadata only) |
| GET | `/assets/:id` | Get asset + its categories |
| PUT | `/assets/:id` | Update asset metadata |
| DELETE | `/assets/:id` | Delete asset + remove from storage |
| POST | `/assets/:id/upload` | Upload file via multipart form |
| GET | `/assets/:id/download` | Download file bytes |
| POST | `/assets/:asset_id/categories/:category_id` | Add category to asset |
| DELETE | `/assets/:asset_id/categories/:category_id` | Remove category from asset |
| GET | `/categories` | List all categories |
| POST | `/categories` | Create category |
| GET | `/categories/:id` | Get category + direct sub-categories |
| PUT | `/categories/:id` | Update category |
| DELETE | `/categories/:id` | Delete category |

## Key Patterns

- All handlers take `State(state): State<AppState>` — clone is cheap (Arc-backed pool + S3 client).
- Database rows are converted to model structs via `impl From<&Row> for T`.
- `AppError` implements `IntoResponse`; handlers return `AppResult<T>` (alias for `Result<T, AppError>`).
- Asset upload flow: `POST /assets` creates the record with a `pending/` storage key, then `POST /assets/:id/upload` streams the file to MinIO and updates `storage_key` + `size`.
