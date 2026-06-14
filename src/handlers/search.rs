use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    auth::AuthUser,
    error::AppResult,
    models::asset::Asset,
};

use super::assets::ASSET_COLUMNS;

/// A single search hit: the asset record plus its extracted metadata (if any).
#[derive(Serialize, ToSchema)]
pub struct AssetSearchResult {
    #[serde(flatten)]
    pub asset: Asset,
    pub metadata: Option<AssetMetadataSummary>,
}

/// Subset of asset_metadata returned inline with search results.
#[derive(Serialize, ToSchema)]
pub struct AssetMetadataSummary {
    pub exif: Option<serde_json::Value>,
    pub iptc: Option<serde_json::Value>,
    pub xmp: Option<serde_json::Value>,
    pub extracted_at: chrono::DateTime<chrono::Utc>,
}

// ── GET /assets/search ────────────────────────────────────────────────────────

/// Filter parameters for asset search.
#[derive(Debug, Deserialize, IntoParams)]
pub struct SearchParams {
    /// Full-text filter: matches asset name, caption, or description (case-insensitive).
    pub q: Option<String>,
    /// Filter by camera make (e.g. `Canon`).
    pub camera_make: Option<String>,
    /// Filter by camera model (e.g. `EOS R5`).
    pub camera_model: Option<String>,
    /// Filter by creator name (partial match).
    pub creator: Option<String>,
    /// Filter by keyword — matches the IPTC keywords array or the assets.keywords text field.
    pub keyword: Option<String>,
    /// Filter by city from IPTC metadata (partial match).
    pub city: Option<String>,
    /// Filter by country from IPTC metadata (partial match).
    pub country: Option<String>,
    /// Return only assets captured on or after this EXIF datetime string (format: `YYYY:MM:DD HH:MM:SS`).
    pub taken_after: Option<String>,
    /// Return only assets captured on or before this EXIF datetime string.
    pub taken_before: Option<String>,
    /// When `true`, return only assets that have GPS coordinates in their EXIF metadata.
    pub has_gps: Option<bool>,
    /// Filter to assets belonging to this category (UUID).
    pub category_id: Option<Uuid>,
    /// Maximum number of results to return (default: 50, max: 200).
    pub limit: Option<i64>,
    /// Pagination offset (default: 0).
    pub offset: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/assets/search",
    tag = "search",
    params(SearchParams),
    responses(
        (status = 200, description = "Matching assets with optional metadata", body = Vec<AssetSearchResult>),
    )
)]
pub async fn search_assets(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Query(p): Query<SearchParams>,
) -> AppResult<Json<Vec<AssetSearchResult>>> {
    auth_user.require_access()?;

    let limit = p.limit.unwrap_or(50).min(200);
    let offset = p.offset.unwrap_or(0);

    let client = state.db.get().await?;

    // All conditions are optional.  The pattern `$N::TEXT IS NULL OR <condition>`
    // short-circuits when the caller omits a filter.
    let q_like = p.q.as_deref().map(|s| format!("%{s}%"));
    let creator_like = p.creator.as_deref().map(|s| format!("%{s}%"));
    let city_like = p.city.as_deref().map(|s| format!("%{s}%"));
    let country_like = p.country.as_deref().map(|s| format!("%{s}%"));

    let rows = client
        .query(
            &format!(
                "SELECT a.{ASSET_COLUMNS_PREFIXED}, \
                        am.exif, am.iptc, am.xmp, am.extracted_at \
                 FROM assets a \
                 LEFT JOIN asset_metadata am ON am.asset_id = a.id \
                 WHERE (a.owner_id = $1 OR a.is_private = false) \
                   AND ($2::TEXT IS NULL \
                        OR a.name        ILIKE $2 \
                        OR a.caption     ILIKE $2 \
                        OR a.description ILIKE $2) \
                   AND ($3::TEXT IS NULL OR am.exif->>'camera_make'  ILIKE $3) \
                   AND ($4::TEXT IS NULL OR am.exif->>'camera_model' ILIKE $4) \
                   AND ($5::TEXT IS NULL \
                        OR a.creator ILIKE $5 \
                        OR am.iptc->>'creator' ILIKE $5) \
                   AND ($6::TEXT IS NULL \
                        OR a.keywords ILIKE '%' || $6 || '%' \
                        OR (am.iptc IS NOT NULL AND am.iptc->'keywords' @> jsonb_build_array($6::TEXT))) \
                   AND ($7::TEXT IS NULL OR am.iptc->>'city'    ILIKE $7) \
                   AND ($8::TEXT IS NULL OR am.iptc->>'country' ILIKE $8) \
                   AND ($9::TEXT  IS NULL OR am.exif->>'datetime' >= $9) \
                   AND ($10::TEXT IS NULL OR am.exif->>'datetime' <= $10) \
                   AND ($11::BOOLEAN IS NULL OR NOT $11 OR (am.exif IS NOT NULL AND am.exif ? 'gps_lat')) \
                   AND ($12::UUID IS NULL OR EXISTS \
                        (SELECT 1 FROM asset_categories ac \
                         WHERE ac.asset_id = a.id AND ac.category_id = $12)) \
                 ORDER BY a.created_at DESC \
                 LIMIT $13 OFFSET $14",
                ASSET_COLUMNS_PREFIXED = ASSET_COLUMNS
                    .split(", ")
                    .map(|c| format!("a.{c}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            &[
                &auth_user.user_id,
                &q_like,
                &p.camera_make,
                &p.camera_model,
                &creator_like,
                &p.keyword,
                &city_like,
                &country_like,
                &p.taken_after,
                &p.taken_before,
                &p.has_gps,
                &p.category_id,
                &limit,
                &offset,
            ],
        )
        .await?;

    let results = rows.iter().map(asset_search_result_from_row).collect();
    Ok(Json(results))
}

// ── GET /assets/search/nearby ─────────────────────────────────────────────────

/// Parameters for location-based asset search.
#[derive(Debug, Deserialize, IntoParams)]
pub struct NearbyParams {
    /// Centre latitude in decimal degrees.
    pub lat: f64,
    /// Centre longitude in decimal degrees.
    pub lon: f64,
    /// Search radius in kilometres (default: 10).
    pub radius_km: Option<f64>,
    /// Maximum number of results (default: 50, max: 200).
    pub limit: Option<i64>,
    /// Pagination offset (default: 0).
    pub offset: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/assets/search/nearby",
    tag = "search",
    params(NearbyParams),
    responses(
        (status = 200, description = "Assets within the given radius, with metadata", body = Vec<AssetSearchResult>),
    )
)]
pub async fn search_nearby(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Query(p): Query<NearbyParams>,
) -> AppResult<Json<Vec<AssetSearchResult>>> {
    auth_user.require_access()?;

    let limit = p.limit.unwrap_or(50).min(200);
    let offset = p.offset.unwrap_or(0);
    let radius_km = p.radius_km.unwrap_or(10.0).abs();

    // Approximate degrees per km for a bounding-box pre-filter.
    // 1° lat ≈ 111 km; 1° lon ≈ 111 km * cos(lat).
    // The bbox selects candidates; haversine in the WHERE clause does the exact circle cut.
    let lat_delta = radius_km / 111.0;
    let lon_delta = radius_km / (111.0 * (p.lat.to_radians().cos()).max(0.0001));

    let lat_min = p.lat - lat_delta;
    let lat_max = p.lat + lat_delta;
    let lon_min = p.lon - lon_delta;
    let lon_max = p.lon + lon_delta;

    let client = state.db.get().await?;

    // Bounding-box pre-filter uses the functional btree indexes; haversine post-filter
    // removes corners.  At this scale a pure bounding box would also be acceptable, but
    // the haversine is cheap once the index has reduced the candidate set.
    let asset_cols = ASSET_COLUMNS
        .split(", ")
        .map(|c| format!("a.{c}"))
        .collect::<Vec<_>>()
        .join(", ");

    let rows = client
        .query(
            &format!(
                "SELECT {asset_cols}, am.exif, am.iptc, am.xmp, am.extracted_at \
                 FROM assets a \
                 JOIN asset_metadata am ON am.asset_id = a.id \
                 WHERE am.exif IS NOT NULL \
                   AND am.exif ? 'gps_lat' \
                   AND am.exif ? 'gps_lon' \
                   AND (am.exif->>'gps_lat')::float8 BETWEEN $1 AND $2 \
                   AND (am.exif->>'gps_lon')::float8 BETWEEN $3 AND $4 \
                   AND (6371.0 * 2.0 * asin(sqrt( \
                         power(sin(radians(((am.exif->>'gps_lat')::float8 - $5) / 2.0)), 2) + \
                         cos(radians($5)) * \
                         cos(radians((am.exif->>'gps_lat')::float8)) * \
                         power(sin(radians(((am.exif->>'gps_lon')::float8 - $6) / 2.0)), 2) \
                       ))) <= $7 \
                   AND (a.owner_id = $8 OR a.is_private = false) \
                 ORDER BY a.created_at DESC \
                 LIMIT $9 OFFSET $10"
            ),
            &[
                &lat_min, &lat_max,
                &lon_min, &lon_max,
                &p.lat, &p.lon, &radius_km,
                &auth_user.user_id,
                &limit, &offset,
            ],
        )
        .await?;

    let results = rows.iter().map(asset_search_result_from_row).collect();
    Ok(Json(results))
}

// ── Row mapping ───────────────────────────────────────────────────────────────

fn asset_search_result_from_row(row: &tokio_postgres::Row) -> AssetSearchResult {
    let asset = Asset::from(row);
    let extracted_at: Option<chrono::DateTime<chrono::Utc>> = row.get("extracted_at");
    let metadata = extracted_at.map(|ea| AssetMetadataSummary {
        exif: row.get("exif"),
        iptc: row.get("iptc"),
        xmp: row.get("xmp"),
        extracted_at: ea,
    });
    AssetSearchResult { asset, metadata }
}
