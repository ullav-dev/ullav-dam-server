use axum::{
    extract::{Path, State},
    Json,
};
use uuid::Uuid;

use crate::{
    AppState,
    auth::AuthUser,
    error::{AppError, AppResult},
    metadata,
    models::asset_metadata::AssetMetadata,
};

// ── GET /assets/:id/metadata ──────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/assets/{id}/metadata",
    tag = "metadata",
    params(
        ("id" = uuid::Uuid, Path, description = "Asset ID"),
    ),
    responses(
        (status = 200, description = "Extracted EXIF/IPTC/XMP metadata", body = AssetMetadata),
        (status = 404, description = "Asset or metadata not found", body = crate::error::ErrorResponse),
    )
)]
pub async fn get_asset_metadata(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AssetMetadata>> {
    auth_user.require_access()?;
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            "SELECT asset_id, exif, iptc, xmp, extracted_at \
             FROM asset_metadata WHERE asset_id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("No metadata found for asset {id}")))?;

    Ok(Json(AssetMetadata::from(&row)))
}

// ── POST /assets/:id/metadata/refresh ────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/assets/{id}/metadata/refresh",
    tag = "metadata",
    params(
        ("id" = uuid::Uuid, Path, description = "Asset ID"),
    ),
    responses(
        (status = 200, description = "Re-extracted metadata (file values override assets table fields)", body = AssetMetadata),
        (status = 404, description = "Asset not found or no file uploaded yet", body = crate::error::ErrorResponse),
    )
)]
pub async fn refresh_asset_metadata(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AssetMetadata>> {
    auth_user.require_access()?;
    let client = state.db.get().await?;

    // Owner check + fetch storage key
    let row = client
        .query_opt(
            "SELECT storage_key FROM assets WHERE id = $1 AND owner_id = $2",
            &[&id, &auth_user.user_id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let storage_key: String = row.get("storage_key");
    if storage_key.starts_with("pending/") {
        return Err(AppError::BadRequest(
            "Asset has no uploaded file yet; upload a file before refreshing metadata".into(),
        ));
    }

    let raw = state.storage.download(&storage_key).await?;

    let extracted = tokio::task::spawn_blocking(move || metadata::extract_from_bytes(&raw))
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("metadata extraction panicked: {e}")))?;

    metadata::store_metadata(&client, id, &extracted)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("store metadata: {e}")))?;

    let meta_row = client
        .query_one(
            "SELECT asset_id, exif, iptc, xmp, extracted_at \
             FROM asset_metadata WHERE asset_id = $1",
            &[&id],
        )
        .await?;

    Ok(Json(AssetMetadata::from(&meta_row)))
}
