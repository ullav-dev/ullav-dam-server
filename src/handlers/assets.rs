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

// ── Category membership ───────────────────────────────────────────────────────

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
