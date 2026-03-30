use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::Row;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::models::category::Category;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Asset {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub asset_type: String,
    pub size: i64,
    pub storage_key: String,
    pub bucket: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&Row> for Asset {
    fn from(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            name: row.get("name"),
            description: row.get("description"),
            asset_type: row.get("asset_type"),
            size: row.get("size"),
            storage_key: row.get("storage_key"),
            bucket: row.get("bucket"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AssetWithCategories {
    #[serde(flatten)]
    pub asset: Asset,
    pub categories: Vec<Category>,
}

/// Request body for creating an asset record (before file upload).
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateAssetRequest {
    pub name: String,
    pub description: Option<String>,
    pub asset_type: String,
}

/// Request body for updating asset metadata.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateAssetRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub asset_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── CreateAssetRequest ────────────────────────────────────────────────────

    #[test]
    fn create_asset_request_with_all_fields() {
        let req: CreateAssetRequest = serde_json::from_value(json!({
            "name": "logo.png",
            "description": "Company logo",
            "asset_type": "image"
        }))
        .unwrap();
        assert_eq!(req.name, "logo.png");
        assert_eq!(req.description, Some("Company logo".to_string()));
        assert_eq!(req.asset_type, "image");
    }

    #[test]
    fn create_asset_request_without_description() {
        let req: CreateAssetRequest = serde_json::from_value(json!({
            "name": "video.mp4",
            "asset_type": "video"
        }))
        .unwrap();
        assert_eq!(req.name, "video.mp4");
        assert!(req.description.is_none());
        assert_eq!(req.asset_type, "video");
    }

    #[test]
    fn create_asset_request_missing_required_field_fails() {
        // `name` is required
        let result: Result<CreateAssetRequest, _> =
            serde_json::from_value(json!({ "asset_type": "image" }));
        assert!(result.is_err());
    }

    // ── UpdateAssetRequest ────────────────────────────────────────────────────

    #[test]
    fn update_asset_request_all_fields_none_on_empty_object() {
        let req: UpdateAssetRequest = serde_json::from_value(json!({})).unwrap();
        assert!(req.name.is_none());
        assert!(req.description.is_none());
        assert!(req.asset_type.is_none());
    }

    #[test]
    fn update_asset_request_partial_patch() {
        let req: UpdateAssetRequest =
            serde_json::from_value(json!({ "name": "renamed.png" })).unwrap();
        assert_eq!(req.name, Some("renamed.png".to_string()));
        assert!(req.description.is_none());
        assert!(req.asset_type.is_none());
    }

    #[test]
    fn update_asset_request_all_fields() {
        let req: UpdateAssetRequest = serde_json::from_value(json!({
            "name": "new.jpg",
            "description": "new desc",
            "asset_type": "image"
        }))
        .unwrap();
        assert_eq!(req.name, Some("new.jpg".to_string()));
        assert_eq!(req.description, Some("new desc".to_string()));
        assert_eq!(req.asset_type, Some("image".to_string()));
    }

    // ── Asset serialization ───────────────────────────────────────────────────

    fn sample_asset() -> Asset {
        let now = chrono::Utc::now();
        Asset {
            id: Uuid::new_v4(),
            name: "test.jpg".to_string(),
            description: Some("a test image".to_string()),
            asset_type: "image".to_string(),
            size: 4096,
            storage_key: "assets/abc/test.jpg".to_string(),
            bucket: "dam-bucket".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn asset_serializes_expected_fields() {
        let asset = sample_asset();
        let json = serde_json::to_value(&asset).unwrap();
        assert_eq!(json["name"], "test.jpg");
        assert_eq!(json["asset_type"], "image");
        assert_eq!(json["size"], 4096_i64);
        assert_eq!(json["description"], "a test image");
        assert_eq!(json["storage_key"], "assets/abc/test.jpg");
        assert_eq!(json["bucket"], "dam-bucket");
        assert!(json["id"].is_string());
        assert!(json["created_at"].is_string());
        assert!(json["updated_at"].is_string());
    }

    #[test]
    fn asset_null_description_serializes_as_null() {
        let now = chrono::Utc::now();
        let asset = Asset {
            id: Uuid::new_v4(),
            name: "no-desc.pdf".to_string(),
            description: None,
            asset_type: "document".to_string(),
            size: 0,
            storage_key: "pending/xyz".to_string(),
            bucket: "dam-bucket".to_string(),
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_value(&asset).unwrap();
        assert!(json["description"].is_null());
    }

    // ── AssetWithCategories serialization ─────────────────────────────────────

    #[test]
    fn asset_with_categories_flattens_asset_fields() {
        let awc = AssetWithCategories {
            asset: sample_asset(),
            categories: vec![],
        };
        let json = serde_json::to_value(&awc).unwrap();
        // Flattened: asset fields appear at top level, not nested under "asset"
        assert_eq!(json["name"], "test.jpg");
        assert!(json.get("asset").is_none(), "asset key should not exist when flattened");
        assert!(json["categories"].is_array());
        assert_eq!(json["categories"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn asset_with_categories_includes_categories() {
        use crate::models::category::Category;
        let cat_now = chrono::Utc::now();
        let cat = Category {
            id: Uuid::new_v4(),
            name: "Photos".to_string(),
            description: None,
            parent_id: None,
            created_at: cat_now,
            updated_at: cat_now,
        };
        let awc = AssetWithCategories {
            asset: sample_asset(),
            categories: vec![cat],
        };
        let json = serde_json::to_value(&awc).unwrap();
        let cats = json["categories"].as_array().unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0]["name"], "Photos");
    }
}
