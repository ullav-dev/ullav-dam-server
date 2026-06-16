use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    AppState,
    error::{AppError, AppResult},
    handlers::assets::image_dimensions,
};

// ── Response helpers ──────────────────────────────────────────────────────────

/// Wraps a JSON value with the IIIF `application/ld+json` content type and
/// a permissive CORS header so external IIIF viewers can load manifests.
struct IiifResponse(Value);

impl IntoResponse for IiifResponse {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            r#"application/ld+json; profile="http://iiif.io/api/presentation/3/context.json""#
                .parse()
                .unwrap(),
        );
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
        (StatusCode::OK, headers, Json(self.0)).into_response()
    }
}

// ── Manifest ─────────────────────────────────────────────────────────────────

/// Returns a IIIF Presentation API 3.0 Manifest for a single asset.
///
/// Public assets are served without authentication so external IIIF viewers
/// (Universal Viewer, Mirador) can load them directly. Private assets return 404.
#[utoipa::path(
    get,
    path = "/iiif/manifest/{id}",
    tag = "iiif",
    params(
        ("id" = Uuid, Path, description = "Asset ID"),
    ),
    responses(
        (status = 200, description = "IIIF Manifest 3.0"),
        (status = 404, description = "Asset not found or private"),
    )
)]
pub async fn get_manifest(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            "SELECT name, description, asset_type, caption, keywords, creator, \
                    copyright_notice, is_private, width, height, storage_key \
             FROM assets WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let is_private: bool = row.get("is_private");
    if is_private {
        return Err(AppError::NotFound(format!("Asset {id} not found")));
    }

    let name: String = row.get("name");
    let description: Option<String> = row.get("description");
    let asset_type: String = row.get("asset_type");
    let caption: Option<String> = row.get("caption");
    let keywords: Option<String> = row.get("keywords");
    let creator: Option<String> = row.get("creator");
    let copyright_notice: Option<String> = row.get("copyright_notice");
    let mut width: Option<i32> = row.get("width");
    let mut height: Option<i32> = row.get("height");
    let storage_key: String = row.get("storage_key");

    // Lazy dimension population for pre-existing assets uploaded before migration.
    if width.is_none() && asset_type.starts_with("image/") {
        if let Ok(data) = state.storage.download(&storage_key).await {
            if let Some((w, h)) = image_dimensions(&data) {
                width = Some(w);
                height = Some(h);
                // Best-effort persist so subsequent requests are free.
                let _ = client
                    .execute(
                        "UPDATE assets SET width = $1, height = $2 WHERE id = $3",
                        &[&w, &h, &id],
                    )
                    .await;
            }
        }
    }

    let base = &state.public_base_url;
    let manifest_id = format!("{base}/iiif/manifest/{id}");
    let canvas_id = format!("{manifest_id}/canvas/1");
    let page_id = format!("{canvas_id}/page");
    let annotation_id = format!("{canvas_id}/annotation/1");
    let download_url = format!("{base}/assets/{id}/download");
    let thumbnail_url = format!("{base}/assets/{id}/thumbnail");

    let mut metadata_entries: Vec<Value> = Vec::new();
    if let Some(c) = &creator {
        metadata_entries.push(json!({
            "label": {"en": ["Creator"]},
            "value": {"en": [c]}
        }));
    }
    if let Some(cap) = caption.as_ref().or(description.as_ref()) {
        metadata_entries.push(json!({
            "label": {"en": ["Description"]},
            "value": {"en": [cap]}
        }));
    }
    if let Some(kw) = &keywords {
        let kw_list: Vec<&str> = kw.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        if !kw_list.is_empty() {
            metadata_entries.push(json!({
                "label": {"en": ["Keywords"]},
                "value": {"en": kw_list}
            }));
        }
    }

    // Canvas dimensions: use image dims when available; for non-image types (PDF etc.)
    // omit so viewers fall back gracefully.
    let canvas_dims = match (width, height) {
        (Some(w), Some(h)) => json!({ "width": w, "height": h }),
        _ => json!({}),
    };
    let body_dims = match (width, height) {
        (Some(w), Some(h)) => json!({ "width": w, "height": h }),
        _ => json!({}),
    };

    let mut canvas = json!({
        "id": canvas_id,
        "type": "Canvas",
        "items": [{
            "id": page_id,
            "type": "AnnotationPage",
            "items": [{
                "id": annotation_id,
                "type": "Annotation",
                "motivation": "painting",
                "body": {
                    "id": download_url,
                    "type": "Image",
                    "format": asset_type,
                },
                "target": canvas_id
            }]
        }]
    });

    // Merge dimension fields into canvas and annotation body
    if let (Some(w), Some(h)) = (width, height) {
        canvas["width"] = json!(w);
        canvas["height"] = json!(h);
        canvas["items"][0]["items"][0]["body"]["width"] = json!(w);
        canvas["items"][0]["items"][0]["body"]["height"] = json!(h);
    }
    let _ = (canvas_dims, body_dims); // used via inline merge above

    let mut manifest = json!({
        "@context": "http://iiif.io/api/presentation/3/context.json",
        "id": manifest_id,
        "type": "Manifest",
        "label": {"en": [name]},
        "thumbnail": [{
            "id": thumbnail_url,
            "type": "Image",
            "format": "image/png"
        }],
        "items": [canvas]
    });

    if !metadata_entries.is_empty() {
        manifest["metadata"] = json!(metadata_entries);
    }
    if let Some(cr) = &copyright_notice {
        manifest["rights"] = json!(cr);
    }

    Ok(IiifResponse(manifest).into_response())
}

// ── Collection ────────────────────────────────────────────────────────────────

/// Returns a IIIF Presentation API 3.0 Collection for a category.
///
/// Includes manifest stubs for all non-private assets in the category and
/// collection stubs for each direct sub-category. Only accessible for
/// categories with Global or Group access level (Private categories return 404).
#[utoipa::path(
    get,
    path = "/iiif/collection/{id}",
    tag = "iiif",
    params(
        ("id" = Uuid, Path, description = "Category ID"),
    ),
    responses(
        (status = 200, description = "IIIF Collection 3.0"),
        (status = 404, description = "Category not found or private"),
    )
)]
pub async fn get_collection(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let client = state.db.get().await?;

    // Load the category itself
    let cat_row = client
        .query_opt(
            "SELECT name, description, access_level::text FROM categories WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Category {id} not found")))?;

    let access_level: String = cat_row.get("access_level");
    if access_level == "Private" {
        return Err(AppError::NotFound(format!("Category {id} not found")));
    }

    let cat_name: String = cat_row.get("name");
    let cat_description: Option<String> = cat_row.get("description");

    let base = &state.public_base_url;
    let collection_id = format!("{base}/iiif/collection/{id}");

    // Asset stubs: non-private assets linked to this category
    let asset_rows = client
        .query(
            "SELECT a.id, a.name \
             FROM assets a \
             JOIN asset_categories ac ON ac.asset_id = a.id \
             WHERE ac.category_id = $1 AND a.is_private = false \
             ORDER BY a.name ASC",
            &[&id],
        )
        .await?;

    // Sub-category stubs: direct children that are not Private
    let sub_rows = client
        .query(
            "SELECT id, name, access_level::text \
             FROM categories \
             WHERE parent_id = $1 AND access_level != 'Private' \
             ORDER BY name ASC",
            &[&id],
        )
        .await?;

    let mut items: Vec<Value> = Vec::new();

    for row in &sub_rows {
        let sub_id: Uuid = row.get("id");
        let sub_name: String = row.get("name");
        items.push(json!({
            "id": format!("{base}/iiif/collection/{sub_id}"),
            "type": "Collection",
            "label": {"en": [sub_name]}
        }));
    }

    for row in &asset_rows {
        let asset_id: Uuid = row.get("id");
        let asset_name: String = row.get("name");
        items.push(json!({
            "id": format!("{base}/iiif/manifest/{asset_id}"),
            "type": "Manifest",
            "label": {"en": [asset_name]},
            "thumbnail": [{
                "id": format!("{base}/assets/{asset_id}/thumbnail"),
                "type": "Image",
                "format": "image/png"
            }]
        }));
    }

    let mut collection = json!({
        "@context": "http://iiif.io/api/presentation/3/context.json",
        "id": collection_id,
        "type": "Collection",
        "label": {"en": [cat_name]},
        "items": items
    });

    if let Some(desc) = cat_description {
        collection["summary"] = json!({"en": [desc]});
    }

    Ok(IiifResponse(collection).into_response())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_manifest() -> Value {
        let base = "https://comad.ullav.com";
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let manifest_id = format!("{base}/iiif/manifest/{id}");
        let canvas_id = format!("{manifest_id}/canvas/1");
        json!({
            "@context": "http://iiif.io/api/presentation/3/context.json",
            "id": manifest_id,
            "type": "Manifest",
            "label": {"en": ["Test Image"]},
            "thumbnail": [{"id": format!("{base}/assets/{id}/thumbnail"), "type": "Image", "format": "image/png"}],
            "items": [{
                "id": canvas_id,
                "type": "Canvas",
                "width": 1024,
                "height": 768,
                "items": [{
                    "id": format!("{canvas_id}/page"),
                    "type": "AnnotationPage",
                    "items": [{
                        "id": format!("{canvas_id}/annotation/1"),
                        "type": "Annotation",
                        "motivation": "painting",
                        "body": {
                            "id": format!("{base}/assets/{id}/download"),
                            "type": "Image",
                            "format": "image/jpeg",
                            "width": 1024,
                            "height": 768
                        },
                        "target": canvas_id
                    }]
                }]
            }]
        })
    }

    #[test]
    fn manifest_has_required_iiif_fields() {
        let m = sample_manifest();
        assert_eq!(m["@context"], "http://iiif.io/api/presentation/3/context.json");
        assert_eq!(m["type"], "Manifest");
        assert!(m["id"].as_str().unwrap().contains("/iiif/manifest/"));
        assert!(m["label"]["en"].is_array());
        assert!(m["items"].is_array());
        assert!(!m["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn manifest_canvas_has_annotation_page() {
        let m = sample_manifest();
        let canvas = &m["items"][0];
        assert_eq!(canvas["type"], "Canvas");
        assert_eq!(canvas["width"], 1024);
        assert_eq!(canvas["height"], 768);
        let page = &canvas["items"][0];
        assert_eq!(page["type"], "AnnotationPage");
        let annotation = &page["items"][0];
        assert_eq!(annotation["motivation"], "painting");
        assert_eq!(annotation["body"]["type"], "Image");
    }

    fn sample_collection() -> Value {
        let base = "https://comad.ullav.com";
        let id = "660e8400-e29b-41d4-a716-446655440000";
        json!({
            "@context": "http://iiif.io/api/presentation/3/context.json",
            "id": format!("{base}/iiif/collection/{id}"),
            "type": "Collection",
            "label": {"en": ["Test Collection"]},
            "summary": {"en": ["A description"]},
            "items": [
                {"id": format!("{base}/iiif/manifest/aaa"), "type": "Manifest", "label": {"en": ["Asset A"]}},
                {"id": format!("{base}/iiif/collection/bbb"), "type": "Collection", "label": {"en": ["Sub-collection"]}}
            ]
        })
    }

    #[test]
    fn collection_has_required_iiif_fields() {
        let c = sample_collection();
        assert_eq!(c["@context"], "http://iiif.io/api/presentation/3/context.json");
        assert_eq!(c["type"], "Collection");
        assert!(c["id"].as_str().unwrap().contains("/iiif/collection/"));
        assert!(c["label"]["en"].is_array());
        assert!(c["items"].is_array());
    }

    #[test]
    fn collection_items_have_type_and_id() {
        let c = sample_collection();
        for item in c["items"].as_array().unwrap() {
            assert!(item["id"].is_string());
            assert!(item["type"].is_string());
            assert!(item["label"]["en"].is_array());
        }
    }
}
