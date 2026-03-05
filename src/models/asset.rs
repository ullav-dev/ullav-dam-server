use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::Row;
use uuid::Uuid;

use crate::models::category::Category;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetWithCategories {
    #[serde(flatten)]
    pub asset: Asset,
    pub categories: Vec<Category>,
}

/// Request body for creating an asset record (before file upload).
#[derive(Debug, Deserialize)]
pub struct CreateAssetRequest {
    pub name: String,
    pub description: Option<String>,
    pub asset_type: String,
}

/// Request body for updating asset metadata.
#[derive(Debug, Deserialize)]
pub struct UpdateAssetRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub asset_type: Option<String>,
}
