use chrono::{DateTime, Utc};
use postgres_types::{FromSql, ToSql};
use serde::{Deserialize, Serialize};
use tokio_postgres::Row;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, ToSql, FromSql)]
#[postgres(name = "access_level")]
pub enum AccessLevel {
    #[postgres(name = "Private")]
    Private,
    #[postgres(name = "Group")]
    Group,
    #[postgres(name = "Global")]
    Global,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Category {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub access_level: AccessLevel,
    pub creator: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&Row> for Category {
    fn from(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            name: row.get("name"),
            description: row.get("description"),
            parent_id: row.get("parent_id"),
            access_level: row.get("access_level"),
            creator: row.get("creator"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CategoryWithChildren {
    #[serde(flatten)]
    pub category: Category,
    pub sub_categories: Vec<Category>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateCategoryRequest {
    pub name: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    /// Defaults to `Private` if omitted.
    pub access_level: Option<AccessLevel>,
    pub creator: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateCategoryRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub access_level: Option<AccessLevel>,
    pub creator: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── CreateCategoryRequest ─────────────────────────────────────────────────

    #[test]
    fn create_category_request_with_all_fields() {
        let req: CreateCategoryRequest = serde_json::from_value(json!({
            "name": "Images",
            "description": "All image assets"
        }))
        .unwrap();
        assert_eq!(req.name, "Images");
        assert_eq!(req.description, Some("All image assets".to_string()));
        assert!(req.parent_id.is_none());
    }

    #[test]
    fn create_category_request_with_parent_id() {
        let parent_id = Uuid::new_v4();
        let req: CreateCategoryRequest = serde_json::from_value(json!({
            "name": "RAW Photos",
            "parent_id": parent_id.to_string()
        }))
        .unwrap();
        assert_eq!(req.name, "RAW Photos");
        assert_eq!(req.parent_id, Some(parent_id));
        assert!(req.description.is_none());
    }

    #[test]
    fn create_category_request_missing_name_fails() {
        let result: Result<CreateCategoryRequest, _> =
            serde_json::from_value(json!({ "description": "no name" }));
        assert!(result.is_err());
    }

    // ── UpdateCategoryRequest ─────────────────────────────────────────────────

    #[test]
    fn update_category_request_all_none_on_empty_object() {
        let req: UpdateCategoryRequest = serde_json::from_value(json!({})).unwrap();
        assert!(req.name.is_none());
        assert!(req.description.is_none());
        assert!(req.parent_id.is_none());
    }

    #[test]
    fn update_category_request_partial_patch() {
        let req: UpdateCategoryRequest =
            serde_json::from_value(json!({ "name": "Renamed" })).unwrap();
        assert_eq!(req.name, Some("Renamed".to_string()));
        assert!(req.description.is_none());
        assert!(req.parent_id.is_none());
    }

    #[test]
    fn update_category_request_with_parent_id() {
        let parent_id = Uuid::new_v4();
        let req: UpdateCategoryRequest =
            serde_json::from_value(json!({ "parent_id": parent_id.to_string() })).unwrap();
        assert_eq!(req.parent_id, Some(parent_id));
    }

    // ── Category serialization ────────────────────────────────────────────────

    fn sample_category() -> Category {
        let now = chrono::Utc::now();
        Category {
            id: Uuid::new_v4(),
            name: "Videos".to_string(),
            description: Some("Video assets".to_string()),
            parent_id: None,
            access_level: AccessLevel::Private,
            creator: Some("colin".to_string()),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn category_serializes_expected_fields() {
        let cat = sample_category();
        let json = serde_json::to_value(&cat).unwrap();
        assert_eq!(json["name"], "Videos");
        assert_eq!(json["description"], "Video assets");
        assert!(json["parent_id"].is_null());
        assert_eq!(json["access_level"], "Private");
        assert_eq!(json["creator"], "colin");
        assert!(json["id"].is_string());
        assert!(json["created_at"].is_string());
        assert!(json["updated_at"].is_string());
    }

    #[test]
    fn category_with_parent_id_serializes() {
        let parent_id = Uuid::new_v4();
        let now = chrono::Utc::now();
        let cat = Category {
            id: Uuid::new_v4(),
            name: "Sub".to_string(),
            description: None,
            parent_id: Some(parent_id),
            access_level: AccessLevel::Group,
            creator: None,
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_value(&cat).unwrap();
        assert_eq!(json["parent_id"], parent_id.to_string());
        assert!(json["description"].is_null());
    }

    // ── CategoryWithChildren serialization ────────────────────────────────────

    #[test]
    fn category_with_children_flattens_category_fields() {
        let cwc = CategoryWithChildren {
            category: sample_category(),
            sub_categories: vec![],
        };
        let json = serde_json::to_value(&cwc).unwrap();
        // Flattened: category fields appear at top level
        assert_eq!(json["name"], "Videos");
        assert!(json.get("category").is_none(), "category key should not exist when flattened");
        assert!(json["sub_categories"].is_array());
        assert_eq!(json["sub_categories"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn category_with_children_includes_sub_categories() {
        let child_now = chrono::Utc::now();
        let child = Category {
            id: Uuid::new_v4(),
            name: "Action".to_string(),
            description: None,
            parent_id: Some(Uuid::new_v4()),
            access_level: AccessLevel::Global,
            creator: Some("colin".to_string()),
            created_at: child_now,
            updated_at: child_now,
        };
        let cwc = CategoryWithChildren {
            category: sample_category(),
            sub_categories: vec![child],
        };
        let json = serde_json::to_value(&cwc).unwrap();
        let subs = json["sub_categories"].as_array().unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0]["name"], "Action");
    }
}
