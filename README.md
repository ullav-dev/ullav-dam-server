# ullav-dam-server

A Digital Asset Management (DAM) HTTP API server written in Rust.

## Tech Stack

- **Web:** axum 0.7
- **Database:** tokio-postgres + deadpool-postgres (native SQL, no ORM)
- **Storage:** aws-sdk-s3 against a MinIO instance (path-style, S3-compatible)
- **Runtime:** Tokio
- **API Docs:** utoipa + Swagger UI

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

## API Routes

| Method | Path | Description |
|--------|------|-------------|
| GET | `/assets` | List all assets |
| POST | `/assets` | Create asset record (metadata only) |
| POST | `/assets/upload` | Create asset + upload file in one request |
| GET | `/assets/:id` | Get asset with its categories |
| PUT | `/assets/:id` | Update asset metadata |
| DELETE | `/assets/:id` | Delete asset and remove from storage |
| POST | `/assets/:id/upload` | Upload file for an existing asset |
| GET | `/assets/:id/download` | Download asset file |
| POST | `/assets/:asset_id/categories/:category_id` | Add category to asset |
| DELETE | `/assets/:asset_id/categories/:category_id` | Remove category from asset |
| GET | `/categories` | List all categories |
| POST | `/categories` | Create category |
| GET | `/categories/:id` | Get category with sub-categories |
| PUT | `/categories/:id` | Update category |
| DELETE | `/categories/:id` | Delete category |

### Upload an asset (single request)

`POST /assets/upload` accepts `multipart/form-data` with the following fields:

| Field | Required | Description |
|-------|----------|-------------|
| `file` | yes | The file to upload |
| `name` | no | Asset name — defaults to the filename |
| `asset_type` | no | MIME type — inferred from file extension if omitted |
| `description` | no | Optional description |

```bash
curl -X POST http://localhost:8080/assets/upload \
  -F "file=@photo.png" \
  -F "description=Profile photo"
```

## Data Model

- **Asset** — `id`, `name`, `description`, `asset_type`, `size` (bytes), `storage_key`, `bucket`, timestamps
- **Category** — `id`, `name`, `description`, `parent_id` (nullable self-FK for sub-categories), timestamps
- **asset_categories** — M2M junction table linking assets to categories

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | yes | — | PostgreSQL connection string |
| `S3_ENDPOINT` | yes | — | S3/MinIO endpoint URL |
| `S3_ACCESS_KEY_ID` | yes | — | S3 access key |
| `S3_SECRET_ACCESS_KEY` | yes | — | S3 secret key |
| `S3_BUCKET` | no | `dam-assets` | S3 bucket name |
| `S3_REGION` | no | `us-east-1` | S3 region |
| `HOST` | no | `0.0.0.0` | Bind host |
| `PORT` | no | `8080` | Bind port |
