use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::Row;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub enum FieldType {
    String,
    Boolean,
    Integer,
    Float,
    DateTime,
}

impl std::fmt::Display for FieldType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            FieldType::String => "String",
            FieldType::Boolean => "Boolean",
            FieldType::Integer => "Integer",
            FieldType::Float => "Float",
            FieldType::DateTime => "DateTime",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for FieldType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "String" => Ok(FieldType::String),
            "Boolean" => Ok(FieldType::Boolean),
            "Integer" => Ok(FieldType::Integer),
            "Float" => Ok(FieldType::Float),
            "DateTime" => Ok(FieldType::DateTime),
            _ => Err(format!("unknown field type: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CustomFieldSchema {
    pub id: Uuid,
    pub team_id: String,
    /// Immutable JSONB key slug (e.g. `"project_code"`).
    pub key: String,
    /// Editable human-readable display label.
    pub name: String,
    pub field_type: FieldType,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&Row> for CustomFieldSchema {
    fn from(row: &Row) -> Self {
        let type_str: String = row.get("field_type");
        Self {
            id: row.get("id"),
            team_id: row.get("team_id"),
            key: row.get("key"),
            name: row.get("name"),
            field_type: type_str.parse().expect("valid field_type in DB"),
            required: row.get("required"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateCustomFieldSchemaRequest {
    /// Immutable slug used as the JSONB property key on assets (e.g. `"project_code"`).
    /// Must be unique within the team. Cannot be changed after creation.
    pub key: String,
    /// Human-readable display label (e.g. `"Project Code"`). Editable.
    pub name: String,
    pub field_type: FieldType,
    pub required: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateCustomFieldSchemaRequest {
    /// New display label. The `key` is immutable and cannot be changed.
    pub name: Option<String>,
    pub required: Option<bool>,
}
