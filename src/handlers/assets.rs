use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
use image::{ImageFormat, imageops::FilterType};
use std::io::Cursor;
use uuid::Uuid;

// ── PDF thumbnail rendering ───────────────────────────────────────────────────

/// Render the first page of a PDF to a PNG thumbnail of at most `size × size` pixels.
/// Returns `Err(String)` on any failure; the caller falls back to the SVG icon.
fn render_pdf_thumbnail(data: &[u8], size: u32) -> Result<bytes::Bytes, String> {
    use pdfium_render::prelude::*;

    // Bind to the PDFium shared library.  PDFIUM_LIB_PATH takes precedence so
    // that deployments can point at the right binary without recompiling.
    let bindings = match std::env::var("PDFIUM_LIB_PATH") {
        Ok(path) => Pdfium::bind_to_library(&path)
            .map_err(|e| format!("pdfium load from {path}: {e}"))?,
        Err(_) => Pdfium::bind_to_system_library()
            .map_err(|e| format!("pdfium system library: {e}"))?,
    };

    let pdfium = Pdfium::new(bindings);

    let doc = pdfium
        .load_pdf_from_byte_slice(data, None)
        .map_err(|e| format!("PDF parse: {e}"))?;

    let page = doc
        .pages()
        .get(0)
        .map_err(|e| format!("PDF no page 0: {e}"))?;

    let config = PdfRenderConfig::new()
        .set_target_width(size as i32)
        .set_maximum_height(size as i32);

    let bitmap = page
        .render_with_config(&config)
        .map_err(|e| format!("PDF render: {e}"))?;

    let img = bitmap.as_image();

    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| e.to_string())?;

    Ok(bytes::Bytes::from(buf.into_inner()))
}

// ── Office document thumbnail rendering ──────────────────────────────────────

/// Map an Office MIME type to the file extension LibreOffice needs on the temp
/// input file.  Returns `None` for non-Office types.
fn office_mime_to_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        "application/msword" => Some("doc"),
        "application/vnd.ms-excel" => Some("xls"),
        "application/vnd.ms-powerpoint" => Some("ppt"),
        _ => None,
    }
}

/// Convert an Office document to PDF via LibreOffice headless, then render the
/// first page as a PNG thumbnail.
///
/// `SOFFICE_PATH` overrides the LibreOffice binary location (defaults to
/// `soffice` on `$PATH`).  Returns `Err(String)` on any failure; the caller
/// falls back to the SVG icon.
fn render_office_thumbnail(data: &[u8], ext: &str, size: u32) -> Result<bytes::Bytes, String> {
    use std::process::Command;

    // Write Office bytes to a temp file with the correct extension so LibreOffice
    // can detect the format.
    let input = tempfile::Builder::new()
        .suffix(&format!(".{ext}"))
        .tempfile()
        .map_err(|e| format!("create temp input file: {e}"))?;
    std::fs::write(input.path(), data)
        .map_err(|e| format!("write temp input file: {e}"))?;

    // LibreOffice writes `<stem>.pdf` into the output directory.
    let outdir = tempfile::tempdir()
        .map_err(|e| format!("create temp output dir: {e}"))?;

    let soffice = std::env::var("SOFFICE_PATH").unwrap_or_else(|_| "soffice".into());

    let status = Command::new(&soffice)
        .args([
            "--headless",
            "--convert-to",
            "pdf",
            "--outdir",
            outdir.path().to_str().unwrap_or("/tmp"),
            input.path().to_str().unwrap_or(""),
        ])
        .status()
        .map_err(|e| format!("soffice exec: {e}"))?;

    if !status.success() {
        return Err(format!("soffice exited with {status}"));
    }

    let stem = input
        .path()
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let pdf_path = outdir.path().join(format!("{stem}.pdf"));
    let pdf_bytes = std::fs::read(&pdf_path)
        .map_err(|e| format!("read converted PDF {}: {e}", pdf_path.display()))?;

    render_pdf_thumbnail(&pdf_bytes, size)
}

// ── Apple iWork thumbnail extraction ─────────────────────────────────────────

/// Returns `true` for Pages, Numbers, and Keynote MIME types.
fn is_iwork_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/x-iwork-pages-sffpages"
            | "application/x-iwork-numbers-sffnumbers"
            | "application/x-iwork-keynote-sffkey"
            | "application/vnd.apple.pages"
            | "application/vnd.apple.numbers"
            | "application/vnd.apple.keynote"
    )
}

/// Extract the embedded QuickLook thumbnail from an iWork ZIP archive and
/// resize it to `size × size` (preserving aspect ratio).  Returns `Err` if
/// no thumbnail entry is found or decoding fails; the caller falls back to the
/// SVG icon.
fn extract_iwork_thumbnail(data: &[u8], size: u32) -> Result<bytes::Bytes, String> {
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    let mut archive = ZipArchive::new(Cursor::new(data))
        .map_err(|e| format!("open iWork ZIP: {e}"))?;

    // Apple embeds a pre-rendered JPEG thumbnail at one of these paths.
    let candidates = ["QuickLook/Thumbnail.jpg", "QuickLook/Thumbnail.png", "preview.jpg"];

    for candidate in &candidates {
        if let Ok(mut entry) = archive.by_name(candidate) {
            let mut img_bytes = Vec::new();
            entry.read_to_end(&mut img_bytes)
                .map_err(|e| format!("read {candidate}: {e}"))?;

            let img = image::load_from_memory(&img_bytes)
                .map_err(|e| format!("decode {candidate}: {e}"))?;
            let thumb = img.thumbnail(size, size);
            let mut buf = Cursor::new(Vec::new());
            thumb.write_to(&mut buf, ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            return Ok(bytes::Bytes::from(buf.into_inner()));
        }
    }

    Err("no QuickLook thumbnail found in iWork archive".into())
}

use crate::{
    AppState,
    auth::AuthUser,
    error::{AppError, AppResult},
    models::asset::{Asset, AssetWithCategories, CreateAssetRequest, UpdateAssetRequest},
    models::category::Category,
};

const ASSET_COLUMNS: &str =
    "id, owner_id, name, description, asset_type, size, storage_key, bucket, \
     caption, keywords, creator, copyright_notice, available, available_until, \
     is_locked, is_private, public_read, public_download, public_write, \
     created_at, updated_at";

// ── List all assets ───────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/assets",
    tag = "assets",
    responses(
        (status = 200, description = "List of all assets", body = Vec<Asset>),
    )
)]
pub async fn list_assets(auth_user: AuthUser, State(state): State<AppState>) -> AppResult<Json<Vec<Asset>>> {
    auth_user.require_access()?;
    let client = state.db.get().await?;
    let rows = client
        .query(
            &format!("SELECT {ASSET_COLUMNS} FROM assets ORDER BY created_at DESC"),
            &[],
        )
        .await?;

    let assets = rows.iter().map(Asset::from).collect();
    Ok(Json(assets))
}

// ── Get one asset with its categories ────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/assets/{id}",
    tag = "assets",
    params(
        ("id" = uuid::Uuid, Path, description = "Asset ID"),
    ),
    responses(
        (status = 200, description = "Asset with its categories", body = AssetWithCategories),
        (status = 404, description = "Asset not found", body = ErrorResponse),
    )
)]
pub async fn get_asset(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AssetWithCategories>> {
    auth_user.require_access()?;
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            &format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id = $1"),
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let asset = Asset::from(&row);

    let cat_rows = client
        .query(
            "SELECT c.id, c.name, c.description, c.parent_id, c.creator, c.access_level, c.created_at, c.updated_at
             FROM categories c
             JOIN asset_categories ac ON ac.category_id = c.id
             WHERE ac.asset_id = $1",
            &[&id],
        )
        .await?;

    let categories = cat_rows.iter().map(Category::from).collect();

    Ok(Json(AssetWithCategories { asset, categories }))
}

// ── Create asset record (metadata only, no file yet) ─────────────────────────

#[utoipa::path(
    post,
    path = "/assets",
    tag = "assets",
    request_body = CreateAssetRequest,
    responses(
        (status = 201, description = "Asset record created", body = Asset),
        (status = 400, description = "Invalid request body", body = ErrorResponse),
    )
)]
pub async fn create_asset(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreateAssetRequest>,
) -> AppResult<(StatusCode, Json<Asset>)> {
    auth_user.require_mime_allowed(&body.asset_type)?;
    let client = state.db.get().await?;

    // Enforce asset count quota
    let count_row = client
        .query_one(
            "SELECT COUNT(*) AS n FROM assets WHERE owner_id = $1",
            &[&auth_user.user_id],
        )
        .await?;
    let count: i64 = count_row.get("n");
    auth_user.require_asset_quota(count)?;

    let storage_key = format!("pending/{}", Uuid::new_v4());
    let available = body.available.unwrap_or(true);

    let row = client
        .query_one(
            &format!(
                "INSERT INTO assets \
                 (owner_id, name, description, asset_type, size, storage_key, bucket, \
                  caption, keywords, creator, copyright_notice, available, available_until, \
                  is_locked, is_private, public_read, public_download, public_write) \
                 VALUES ($1, $2, $3, $4, 0, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
                 RETURNING {ASSET_COLUMNS}"
            ),
            &[
                &auth_user.user_id,
                &body.name,
                &body.description,
                &body.asset_type,
                &storage_key,
                &state.storage.bucket,
                &body.caption,
                &body.keywords,
                &body.creator,
                &body.copyright_notice,
                &available,
                &body.available_until,
                &body.is_locked.unwrap_or(false),
                &body.is_private.unwrap_or(true),
                &body.public_read.unwrap_or(false),
                &body.public_download.unwrap_or(false),
                &body.public_write.unwrap_or(false),
            ],
        )
        .await?;

    Ok((StatusCode::CREATED, Json(Asset::from(&row))))
}

// ── Upload file for an asset ──────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/assets/{id}/upload",
    tag = "assets",
    params(
        ("id" = uuid::Uuid, Path, description = "Asset ID"),
    ),
    request_body(content = Vec<u8>, content_type = "multipart/form-data", description = "File to upload"),
    responses(
        (status = 200, description = "Asset with updated storage key and size", body = Asset),
        (status = 400, description = "Missing or invalid file field", body = ErrorResponse),
        (status = 404, description = "Asset not found", body = ErrorResponse),
    )
)]
pub async fn upload_asset(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> AppResult<Json<Asset>> {
    auth_user.require_access()?;
    let client = state.db.get().await?;

    // Verify asset exists and fetch current size for storage delta calculation
    let existing_row = client
        .query_opt("SELECT size FROM assets WHERE id = $1", &[&id])
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;
    let current_size: i64 = existing_row.get("size");

    // Read the first field from multipart
    let field = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .ok_or_else(|| AppError::BadRequest("No file field in multipart body".into()))?;

    let content_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    let file_name = field
        .file_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|| id.to_string());

    let data = field
        .bytes()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let size = data.len() as i64;
    let storage_key = format!("assets/{}/{}", id, file_name);

    // Enforce storage quota: total used - old size + new size must not exceed limit
    let usage_row = client
        .query_one(
            "SELECT COALESCE(SUM(size), 0) AS used FROM assets WHERE owner_id = $1",
            &[&auth_user.user_id],
        )
        .await?;
    let used_bytes: i64 = usage_row.get("used");
    auth_user.require_storage_quota(used_bytes - current_size, size)?;

    state
        .storage
        .upload(&storage_key, data, &content_type)
        .await?;

    // Evict stale thumbnail so the next request regenerates from the new file
    state.thumbnail_cache.write().await.remove(&id);

    let row = client
        .query_one(
            &format!("UPDATE assets SET storage_key = $1, size = $2 WHERE id = $3 RETURNING {ASSET_COLUMNS}"),
            &[&storage_key, &size, &id],
        )
        .await?;

    Ok(Json(Asset::from(&row)))
}

// ── Download / presigned URL ──────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/assets/{id}/download",
    tag = "assets",
    params(
        ("id" = uuid::Uuid, Path, description = "Asset ID"),
    ),
    responses(
        (status = 200, description = "Raw file bytes as attachment", content_type = "application/octet-stream"),
        (status = 404, description = "Asset not found", body = ErrorResponse),
    )
)]
pub async fn download_asset(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            "SELECT storage_key FROM assets WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let storage_key: String = row.get("storage_key");
    let data = state.storage.download(&storage_key).await?;

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", storage_key.split('/').last().unwrap_or("file")),
        )
        .body(Body::from(data))
        .map_err(|e| AppError::Internal(e.into()))?;

    Ok(response)
}

// ── Update asset metadata ─────────────────────────────────────────────────────

#[utoipa::path(
    put,
    path = "/assets/{id}",
    tag = "assets",
    params(
        ("id" = uuid::Uuid, Path, description = "Asset ID"),
    ),
    request_body = UpdateAssetRequest,
    responses(
        (status = 200, description = "Updated asset", body = Asset),
        (status = 404, description = "Asset not found", body = ErrorResponse),
    )
)]
pub async fn update_asset(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateAssetRequest>,
) -> AppResult<Json<Asset>> {
    auth_user.require_access()?;
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            &format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id = $1"),
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let current = Asset::from(&row);

    let name = body.name.unwrap_or(current.name);
    let description = body.description.or(current.description);
    let asset_type = body.asset_type.unwrap_or(current.asset_type);
    let caption = body.caption.or(current.caption);
    let keywords = body.keywords.or(current.keywords);
    let creator = body.creator.or(current.creator);
    let copyright_notice = body.copyright_notice.or(current.copyright_notice);
    let available = body.available.unwrap_or(current.available);
    let available_until = body.available_until.or(current.available_until);
    let is_locked = body.is_locked.unwrap_or(current.is_locked);
    let is_private = body.is_private.unwrap_or(current.is_private);
    let public_read = body.public_read.unwrap_or(current.public_read);
    let public_download = body.public_download.unwrap_or(current.public_download);
    let public_write = body.public_write.unwrap_or(current.public_write);

    let updated = client
        .query_one(
            &format!(
                "UPDATE assets \
                 SET name = $1, description = $2, asset_type = $3, \
                     caption = $4, keywords = $5, creator = $6, copyright_notice = $7, \
                     available = $8, available_until = $9, is_locked = $10, \
                     is_private = $11, public_read = $12, public_download = $13, public_write = $14 \
                 WHERE id = $15 \
                 RETURNING {ASSET_COLUMNS}"
            ),
            &[
                &name, &description, &asset_type,
                &caption, &keywords, &creator, &copyright_notice,
                &available, &available_until, &is_locked,
                &is_private, &public_read, &public_download, &public_write,
                &id,
            ],
        )
        .await?;

    Ok(Json(Asset::from(&updated)))
}

// ── Delete asset ──────────────────────────────────────────────────────────────

#[utoipa::path(
    delete,
    path = "/assets/{id}",
    tag = "assets",
    params(
        ("id" = uuid::Uuid, Path, description = "Asset ID"),
    ),
    responses(
        (status = 204, description = "Asset deleted"),
        (status = 403, description = "Asset is locked and cannot be deleted", body = ErrorResponse),
        (status = 404, description = "Asset not found", body = ErrorResponse),
    )
)]
pub async fn delete_asset(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    auth_user.require_access()?;
    let client = state.db.get().await?;

    let row = client
        .query_opt("SELECT storage_key, is_locked FROM assets WHERE id = $1", &[&id])
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let is_locked: bool = row.get("is_locked");
    if is_locked {
        return Err(AppError::Forbidden("Asset is locked and cannot be deleted".into()));
    }

    let storage_key: String = row.get("storage_key");

    // Delete from storage (best-effort; file may not have been uploaded yet)
    if !storage_key.starts_with("pending/") {
        let _ = state.storage.delete(&storage_key).await;
    }

    client
        .execute("DELETE FROM assets WHERE id = $1", &[&id])
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

// ── Create asset + upload file in one request ─────────────────────────────────

/// Multipart form schema for POST /assets/upload.
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
struct UploadAssetForm {
    /// File to upload (required)
    #[schema(format = Binary)]
    file: Vec<u8>,
    /// Asset name — defaults to the uploaded filename if omitted
    name: Option<String>,
    /// MIME type — inferred from the file extension if omitted (e.g. `image/png`, `application/pdf`)
    asset_type: Option<String>,
    description: Option<String>,
    caption: Option<String>,
    keywords: Option<String>,
    creator: Option<String>,
    copyright_notice: Option<String>,
    /// Whether the asset is publicly available (`"true"` or `"false"`). Defaults to `"true"` if omitted.
    #[schema(example = "true")]
    available: Option<String>,
    /// Expiry datetime in ISO 8601 format.
    #[schema(format = DateTime, example = "2026-12-31T00:00:00Z")]
    available_until: Option<String>,
    /// Whether the asset is locked (`"true"` or `"false"`). Defaults to `"false"` if omitted.
    #[schema(example = "false")]
    is_locked: Option<String>,
    /// Asset is private to the uploader (`"true"` or `"false"`). Defaults to `"true"`.
    #[schema(example = "true")]
    is_private: Option<String>,
    /// Allow unauthenticated read (`"true"` or `"false"`). Defaults to `"false"`.
    #[schema(example = "false")]
    public_read: Option<String>,
    /// Allow unauthenticated download (`"true"` or `"false"`). Defaults to `"false"`.
    #[schema(example = "false")]
    public_download: Option<String>,
    /// Allow unauthenticated write (`"true"` or `"false"`). Defaults to `"false"`.
    #[schema(example = "false")]
    public_write: Option<String>,
}

#[utoipa::path(
    post,
    path = "/assets/upload",
    tag = "assets",
    request_body(
        content = inline(UploadAssetForm),
        content_type = "multipart/form-data",
    ),
    responses(
        (status = 201, description = "Asset created and file uploaded", body = Asset),
        (status = 400, description = "Missing file field or invalid data", body = ErrorResponse),
    )
)]
pub async fn create_and_upload_asset(
    auth_user: AuthUser,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> AppResult<(StatusCode, Json<Asset>)> {
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut asset_type: Option<String> = None;
    let mut caption: Option<String> = None;
    let mut keywords: Option<String> = None;
    let mut creator: Option<String> = None;
    let mut copyright_notice: Option<String> = None;
    let mut available: Option<bool> = None;
    let mut available_until: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut is_locked: Option<bool> = None;
    let mut is_private: Option<bool> = None;
    let mut public_read: Option<bool> = None;
    let mut public_download: Option<bool> = None;
    let mut public_write: Option<bool> = None;
    let mut file_data: Option<bytes::Bytes> = None;
    let mut file_name: Option<String> = None;
    let mut content_type_hdr = "application/octet-stream".to_string();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        let field_name = field.name().map(|s| s.to_string());
        match field_name.as_deref() {
            Some("file") | None => {
                // Named "file" (Swagger UI) or unnamed — treat as the file payload
                content_type_hdr = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                file_name = field.file_name().map(|s| s.to_string());
                file_data = Some(
                    field.bytes().await.map_err(|e| AppError::BadRequest(e.to_string()))?,
                );
            }
            Some(key @ ("name" | "description" | "asset_type" | "caption" | "keywords"
                        | "creator" | "copyright_notice" | "available" | "available_until"
                        | "is_locked" | "is_private" | "public_read" | "public_download"
                        | "public_write")) => {
                let v = field.text().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
                if v.is_empty() {
                    continue;
                }
                match key {
                    "name" => name = Some(v),
                    "description" => description = Some(v),
                    "asset_type" => asset_type = Some(v),
                    "caption" => caption = Some(v),
                    "keywords" => keywords = Some(v),
                    "creator" => creator = Some(v),
                    "copyright_notice" => copyright_notice = Some(v),
                    "available" => {
                        available = Some(matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"));
                    }
                    "is_locked" => {
                        is_locked = Some(matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"));
                    }
                    "is_private" => {
                        is_private = Some(matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"));
                    }
                    "public_read" => {
                        public_read = Some(matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"));
                    }
                    "public_download" => {
                        public_download = Some(matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"));
                    }
                    "public_write" => {
                        public_write = Some(matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"));
                    }
                    "available_until" => {
                        available_until = Some(
                            v.parse::<chrono::DateTime<chrono::Utc>>()
                                .map_err(|_| AppError::BadRequest(
                                    "available_until must be an ISO 8601 datetime (e.g. 2026-12-31T00:00:00Z)".into(),
                                ))?,
                        );
                    }
                    _ => {}
                }
            }
            _ => {
                // Consume and ignore unknown fields
                let _ = field.bytes().await;
            }
        }
    }

    let file_data =
        file_data.ok_or_else(|| AppError::BadRequest("Missing file field".into()))?;

    let asset_id = Uuid::new_v4();
    let resolved_name = file_name.unwrap_or_else(|| asset_id.to_string());

    // Fall back to filename when name is omitted
    let name = name.unwrap_or_else(|| resolved_name.clone());

    // Infer asset_type from the file extension when omitted
    let asset_type = asset_type.unwrap_or_else(|| {
        mime_guess::from_path(&resolved_name)
            .first_or_octet_stream()
            .to_string()
    });

    // Enforce plan-based MIME restriction before writing anything
    auth_user.require_mime_allowed(&asset_type)?;

    let available = available.unwrap_or(true);
    let is_locked = is_locked.unwrap_or(false);
    let is_private = is_private.unwrap_or(true);
    let public_read = public_read.unwrap_or(false);
    let public_download = public_download.unwrap_or(false);
    let public_write = public_write.unwrap_or(false);
    let storage_key = format!("assets/{}/{}", asset_id, resolved_name);
    let size = file_data.len() as i64;

    let client = state.db.get().await?;

    // Enforce asset count quota
    let count_row = client
        .query_one(
            "SELECT COUNT(*) AS n FROM assets WHERE owner_id = $1",
            &[&auth_user.user_id],
        )
        .await?;
    let count: i64 = count_row.get("n");
    auth_user.require_asset_quota(count)?;

    // Enforce storage quota
    let usage_row = client
        .query_one(
            "SELECT COALESCE(SUM(size), 0) AS used FROM assets WHERE owner_id = $1",
            &[&auth_user.user_id],
        )
        .await?;
    let used_bytes: i64 = usage_row.get("used");
    auth_user.require_storage_quota(used_bytes, size)?;

    // Upload first; on failure nothing is written to the DB
    state.storage.upload(&storage_key, file_data, &content_type_hdr).await?;

    let row = client
        .query_one(
            &format!(
                "INSERT INTO assets \
                 (id, owner_id, name, description, asset_type, size, storage_key, bucket, \
                  caption, keywords, creator, copyright_notice, available, available_until, \
                  is_locked, is_private, public_read, public_download, public_write) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) \
                 RETURNING {ASSET_COLUMNS}"
            ),
            &[
                &asset_id, &auth_user.user_id, &name, &description, &asset_type, &size, &storage_key, &state.storage.bucket,
                &caption, &keywords, &creator, &copyright_notice, &available, &available_until,
                &is_locked, &is_private, &public_read, &public_download, &public_write,
            ],
        )
        .await?;

    Ok((StatusCode::CREATED, Json(Asset::from(&row))))
}

// ── Category membership ───────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/assets/{asset_id}/categories/{category_id}",
    tag = "assets",
    params(
        ("asset_id" = uuid::Uuid, Path, description = "Asset ID"),
        ("category_id" = uuid::Uuid, Path, description = "Category ID"),
    ),
    responses(
        (status = 204, description = "Category added to asset"),
    )
)]
pub async fn add_category_to_asset(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path((asset_id, category_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    auth_user.require_access()?;
    let client = state.db.get().await?;

    client
        .execute(
            "INSERT INTO asset_categories (asset_id, category_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
            &[&asset_id, &category_id],
        )
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/assets/{asset_id}/categories/{category_id}",
    tag = "assets",
    params(
        ("asset_id" = uuid::Uuid, Path, description = "Asset ID"),
        ("category_id" = uuid::Uuid, Path, description = "Category ID"),
    ),
    responses(
        (status = 204, description = "Category removed from asset"),
    )
)]
pub async fn remove_category_from_asset(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path((asset_id, category_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    auth_user.require_access()?;
    let client = state.db.get().await?;

    client
        .execute(
            "DELETE FROM asset_categories WHERE asset_id = $1 AND category_id = $2",
            &[&asset_id, &category_id],
        )
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

// ── Thumbnail ─────────────────────────────────────────────────────────────────

const ICON_IMAGE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="100" height="100" rx="12" fill="#E3F2FD"/><rect x="15" y="20" width="70" height="52" rx="5" fill="#fff" stroke="#1976D2" stroke-width="3"/><circle cx="33" cy="37" r="7" fill="#FFC107"/><path d="M15 55 L32 41 L47 53 L62 41 L85 55 L85 72 L15 72Z" fill="#81C784"/></svg>"##;
const ICON_VIDEO: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="100" height="100" rx="12" fill="#F3E5F5"/><rect x="10" y="28" width="60" height="44" rx="5" fill="#fff" stroke="#7B1FA2" stroke-width="3"/><path d="M70 38 L90 30 L90 70 L70 62Z" fill="#7B1FA2"/><polygon points="28,36 28,64 54,50" fill="#7B1FA2"/></svg>"##;
const ICON_AUDIO: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="100" height="100" rx="12" fill="#E8F5E9"/><rect x="38" y="15" width="24" height="40" rx="12" fill="#fff" stroke="#388E3C" stroke-width="3"/><path d="M25 48 Q25 72 50 72 Q75 72 75 48" fill="none" stroke="#388E3C" stroke-width="3"/><line x1="50" y1="72" x2="50" y2="85" stroke="#388E3C" stroke-width="3"/><line x1="35" y1="85" x2="65" y2="85" stroke="#388E3C" stroke-width="3"/></svg>"##;
const ICON_PDF: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="100" height="100" rx="12" fill="#FFEBEE"/><rect x="18" y="10" width="52" height="68" rx="4" fill="#fff" stroke="#C62828" stroke-width="2.5"/><path d="M55 10 L70 26 L55 26Z" fill="#FFCDD2"/><line x1="55" y1="10" x2="70" y2="26" stroke="#C62828" stroke-width="2.5"/><text x="28" y="62" fill="#C62828" font-size="16" font-weight="bold" font-family="sans-serif">PDF</text></svg>"##;
const ICON_TEXT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="100" height="100" rx="12" fill="#F5F5F5"/><rect x="18" y="10" width="52" height="68" rx="4" fill="#fff" stroke="#757575" stroke-width="2.5"/><path d="M55 10 L70 26 L55 26Z" fill="#E0E0E0"/><line x1="55" y1="10" x2="70" y2="26" stroke="#757575" stroke-width="2.5"/><line x1="28" y1="38" x2="60" y2="38" stroke="#BDBDBD" stroke-width="2.5"/><line x1="28" y1="48" x2="60" y2="48" stroke="#BDBDBD" stroke-width="2.5"/><line x1="28" y1="58" x2="60" y2="58" stroke="#BDBDBD" stroke-width="2.5"/><line x1="28" y1="68" x2="45" y2="68" stroke="#BDBDBD" stroke-width="2.5"/></svg>"##;
const ICON_FILE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect width="100" height="100" rx="12" fill="#FFF8E1"/><rect x="18" y="10" width="52" height="68" rx="4" fill="#fff" stroke="#F57F17" stroke-width="2.5"/><path d="M55 10 L70 26 L55 26Z" fill="#FFF9C4"/><line x1="55" y1="10" x2="70" y2="26" stroke="#F57F17" stroke-width="2.5"/><line x1="28" y1="42" x2="60" y2="42" stroke="#FFB300" stroke-width="2.5"/><line x1="28" y1="52" x2="60" y2="52" stroke="#FFB300" stroke-width="2.5"/><line x1="28" y1="62" x2="45" y2="62" stroke="#FFB300" stroke-width="2.5"/></svg>"##;

fn fallback_icon(asset_type: &str) -> &'static str {
    if asset_type.starts_with("video/") {
        ICON_VIDEO
    } else if asset_type.starts_with("audio/") {
        ICON_AUDIO
    } else if asset_type == "application/pdf" {
        ICON_PDF
    } else if asset_type.starts_with("text/") {
        ICON_TEXT
    } else if asset_type.starts_with("image/") {
        ICON_IMAGE
    } else {
        ICON_FILE
    }
}

#[utoipa::path(
    get,
    path = "/assets/{id}/thumbnail",
    tag = "assets",
    params(
        ("id" = uuid::Uuid, Path, description = "Asset ID"),
    ),
    responses(
        (status = 200, description = "PNG thumbnail (image assets) or SVG fallback icon (other types)",
         content_type = "image/png"),
        (status = 404, description = "Asset not found", body = ErrorResponse),
    )
)]
pub async fn get_thumbnail(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    // 1. Check in-memory cache before hitting the database
    {
        let cache = state.thumbnail_cache.read().await;
        if let Some(cached) = cache.get(&id) {
            return Ok(build_thumbnail_response(cached.clone(), "image/png"));
        }
    }

    // 2. Fetch asset metadata
    let client = state.db.get().await?;
    let row = client
        .query_opt(
            "SELECT storage_key, asset_type FROM assets WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let storage_key: String = row.get("storage_key");
    let asset_type: String = row.get("asset_type");

    // 3. Only raster images, PDFs, Office documents, and iWork files get
    //    rendered; everything else (SVG, video, audio…) and pending (not yet
    //    uploaded) assets return the SVG fallback icon immediately.
    let is_raster = asset_type.starts_with("image/") && asset_type != "image/svg+xml";
    let is_pdf = asset_type == "application/pdf";
    let office_ext = office_mime_to_ext(&asset_type);
    let is_iwork = is_iwork_mime(&asset_type);

    if (!is_raster && !is_pdf && office_ext.is_none() && !is_iwork)
        || storage_key.starts_with("pending/")
    {
        let svg = fallback_icon(&asset_type);
        return Ok(build_thumbnail_response(bytes::Bytes::from(svg), "image/svg+xml"));
    }

    // 4. Download the raw file from storage
    let raw = state.storage.download(&storage_key).await?;
    let size = state.thumbnail_size;

    // 5. Render on a blocking thread (CPU-bound — must not block the async runtime)
    let result = tokio::task::spawn_blocking(move || -> Result<bytes::Bytes, String> {
        if is_pdf {
            render_pdf_thumbnail(&raw, size)
        } else if let Some(ext) = office_ext {
            render_office_thumbnail(&raw, ext, size)
        } else if is_iwork {
            extract_iwork_thumbnail(&raw, size)
        } else {
            let reader = image::ImageReader::new(Cursor::new(raw))
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let img = reader.decode().map_err(|e| e.to_string())?;
            let thumb = img.resize(size, size, FilterType::Lanczos3);
            let mut buf = Cursor::new(Vec::new());
            thumb.write_to(&mut buf, ImageFormat::Png).map_err(|e| e.to_string())?;
            Ok(bytes::Bytes::from(buf.into_inner()))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("thumbnail thread panicked: {e}")))?;

    // 6. On failure fall back to the icon rather than returning an error to the client
    let png = match result {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("Thumbnail generation failed for asset {id}: {e}");
            let svg = fallback_icon(&asset_type);
            return Ok(build_thumbnail_response(bytes::Bytes::from(svg), "image/svg+xml"));
        }
    };

    // 7. Write into cache
    state.thumbnail_cache.write().await.insert(id, png.clone());

    Ok(build_thumbnail_response(png, "image/png"))
}

fn build_thumbnail_response(data: bytes::Bytes, content_type: &'static str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .body(Body::from(data))
        .expect("static header values are always valid")
}
