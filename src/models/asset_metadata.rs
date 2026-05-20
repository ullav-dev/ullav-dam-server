use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::Row;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AssetMetadata {
    pub asset_id: Uuid,
    /// Raw EXIF tags plus normalised fields (camera_make, camera_model, datetime, gps_lat, gps_lon, …)
    pub exif: Option<serde_json::Value>,
    /// Raw IPTC tags plus normalised fields (creator, copyright, caption, keywords, city, country, …)
    pub iptc: Option<serde_json::Value>,
    /// Raw XMP tags
    pub xmp: Option<serde_json::Value>,
    pub extracted_at: DateTime<Utc>,
}

impl From<&Row> for AssetMetadata {
    fn from(row: &Row) -> Self {
        Self {
            asset_id: row.get("asset_id"),
            exif: row.get("exif"),
            iptc: row.get("iptc"),
            xmp: row.get("xmp"),
            extracted_at: row.get("extracted_at"),
        }
    }
}
