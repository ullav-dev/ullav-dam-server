use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
use uuid::Uuid;

use crate::{
    AppState,
    error::{AppError, AppResult},
    models::asset::{Asset, AssetWithCategories, CreateAssetRequest, UpdateAssetRequest},
    models::category::Category,
};

// ── List all assets ───────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/assets",
    tag = "assets",
    responses(
        (status = 200, description = "List of all assets", body = Vec<Asset>),
    )
)]
pub async fn list_assets(State(state): State<AppState>) -> AppResult<Json<Vec<Asset>>> {
    let client = state.db.get().await?;
    let rows = client
        .query(
            "SELECT id, name, description, asset_type, size, storage_key, bucket, created_at, updated_at
             FROM assets
             ORDER BY created_at DESC",
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
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AssetWithCategories>> {
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            "SELECT id, name, description, asset_type, size, storage_key, bucket, created_at, updated_at
             FROM assets WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let asset = Asset::from(&row);

    let cat_rows = client
        .query(
            "SELECT c.id, c.name, c.description, c.parent_id, c.created_at, c.updated_at
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
    State(state): State<AppState>,
    Json(body): Json<CreateAssetRequest>,
) -> AppResult<(StatusCode, Json<Asset>)> {
    let client = state.db.get().await?;

    // Placeholder storage key; real key assigned on upload
    let storage_key = format!("pending/{}", Uuid::new_v4());

    let row = client
        .query_one(
            "INSERT INTO assets (name, description, asset_type, size, storage_key, bucket)
             VALUES ($1, $2, $3, 0, $4, $5)
             RETURNING id, name, description, asset_type, size, storage_key, bucket, created_at, updated_at",
            &[
                &body.name,
                &body.description,
                &body.asset_type,
                &storage_key,
                &state.storage.bucket,
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
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> AppResult<Json<Asset>> {
    let client = state.db.get().await?;

    // Verify asset exists
    let _row = client
        .query_opt("SELECT id FROM assets WHERE id = $1", &[&id])
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

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

    state
        .storage
        .upload(&storage_key, data, &content_type)
        .await?;

    let row = client
        .query_one(
            "UPDATE assets SET storage_key = $1, size = $2 WHERE id = $3
             RETURNING id, name, description, asset_type, size, storage_key, bucket, created_at, updated_at",
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
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateAssetRequest>,
) -> AppResult<Json<Asset>> {
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            "SELECT id, name, description, asset_type, size, storage_key, bucket, created_at, updated_at
             FROM assets WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let current = Asset::from(&row);

    let name = body.name.unwrap_or(current.name);
    let description = body.description.or(current.description);
    let asset_type = body.asset_type.unwrap_or(current.asset_type);

    let updated = client
        .query_one(
            "UPDATE assets SET name = $1, description = $2, asset_type = $3
             WHERE id = $4
             RETURNING id, name, description, asset_type, size, storage_key, bucket, created_at, updated_at",
            &[&name, &description, &asset_type, &id],
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
        (status = 404, description = "Asset not found", body = ErrorResponse),
    )
)]
pub async fn delete_asset(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let client = state.db.get().await?;

    let row = client
        .query_opt("SELECT storage_key FROM assets WHERE id = $1", &[&id])
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

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
    /// Optional description
    description: Option<String>,
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
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> AppResult<(StatusCode, Json<Asset>)> {
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut asset_type: Option<String> = None;
    let mut file_data: Option<bytes::Bytes> = None;
    let mut file_name: Option<String> = None;
    let mut content_type_hdr = "application/octet-stream".to_string();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        match field.name() {
            Some("name") => {
                name = Some(field.text().await.map_err(|e| AppError::BadRequest(e.to_string()))?);
            }
            Some("description") => {
                let v = field.text().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
                if !v.is_empty() {
                    description = Some(v);
                }
            }
            Some("asset_type") => {
                let v = field.text().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
                if !v.is_empty() {
                    asset_type = Some(v);
                }
            }
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

    let storage_key = format!("assets/{}/{}", asset_id, resolved_name);
    let size = file_data.len() as i64;

    // Upload first; on failure nothing is written to the DB
    state.storage.upload(&storage_key, file_data, &content_type_hdr).await?;

    let client = state.db.get().await?;
    let row = client
        .query_one(
            "INSERT INTO assets (id, name, description, asset_type, size, storage_key, bucket)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id, name, description, asset_type, size, storage_key, bucket, created_at, updated_at",
            &[
                &asset_id,
                &name,
                &description,
                &asset_type,
                &size,
                &storage_key,
                &state.storage.bucket,
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
    State(state): State<AppState>,
    Path((asset_id, category_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
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
    State(state): State<AppState>,
    Path((asset_id, category_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    let client = state.db.get().await?;

    client
        .execute(
            "DELETE FROM asset_categories WHERE asset_id = $1 AND category_id = $2",
            &[&asset_id, &category_id],
        )
        .await?;

    Ok(StatusCode::NO_CONTENT)
}
