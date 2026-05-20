use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use deadpool_postgres::Client;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{AppError, AppResult},
    models::custom_field_schema::{
        CreateCustomFieldSchemaRequest, CustomFieldSchema, FieldType, UpdateCustomFieldSchemaRequest,
    },
    AppState,
};

const SCHEMA_COLUMNS: &str = "id, team_id, key, name, field_type, required, created_at, updated_at";

// ── List schemas for a team ───────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/teams/{team_id}/custom-field-schemas",
    tag = "custom-fields",
    params(
        ("team_id" = String, Path, description = "Team ID"),
    ),
    responses(
        (status = 200, description = "List of custom field schemas", body = Vec<CustomFieldSchema>),
        (status = 403, description = "Not a member of this team", body = crate::error::ErrorResponse),
    )
)]
pub async fn list_schemas(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path(team_id): Path<String>,
) -> AppResult<Json<Vec<CustomFieldSchema>>> {
    auth_user.require_team_member(&team_id)?;
    let client = state.db.get().await?;
    let rows = client
        .query(
            &format!("SELECT {SCHEMA_COLUMNS} FROM custom_field_schemas WHERE team_id = $1 ORDER BY name"),
            &[&team_id],
        )
        .await?;
    Ok(Json(rows.iter().map(CustomFieldSchema::from).collect()))
}

// ── Create schema ─────────────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/teams/{team_id}/custom-field-schemas",
    tag = "custom-fields",
    params(
        ("team_id" = String, Path, description = "Team ID"),
    ),
    request_body = CreateCustomFieldSchemaRequest,
    responses(
        (status = 201, description = "Schema created", body = CustomFieldSchema),
        (status = 400, description = "Invalid request or duplicate key", body = crate::error::ErrorResponse),
        (status = 403, description = "Not a team owner or leader", body = crate::error::ErrorResponse),
    )
)]
pub async fn create_schema(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path(team_id): Path<String>,
    Json(body): Json<CreateCustomFieldSchemaRequest>,
) -> AppResult<(StatusCode, Json<CustomFieldSchema>)> {
    auth_user.require_team_admin(&team_id)?;

    if body.key.is_empty() || body.name.is_empty() {
        return Err(AppError::BadRequest("key and name must not be empty".into()));
    }

    let client = state.db.get().await?;
    let row = client
        .query_one(
            &format!(
                "INSERT INTO custom_field_schemas (team_id, key, name, field_type, required) \
                 VALUES ($1, $2, $3, $4, $5) \
                 RETURNING {SCHEMA_COLUMNS}"
            ),
            &[
                &team_id,
                &body.key,
                &body.name,
                &body.field_type.to_string(),
                &body.required.unwrap_or(false),
            ],
        )
        .await
        .map_err(|e| {
            if e.to_string().contains("unique") {
                AppError::BadRequest(format!(
                    "A custom field with key '{}' already exists for this team",
                    body.key
                ))
            } else {
                AppError::Database(e)
            }
        })?;

    Ok((StatusCode::CREATED, Json(CustomFieldSchema::from(&row))))
}

// ── Update schema (name / required only — key is immutable) ──────────────────

#[utoipa::path(
    put,
    path = "/teams/{team_id}/custom-field-schemas/{schema_id}",
    tag = "custom-fields",
    params(
        ("team_id" = String, Path, description = "Team ID"),
        ("schema_id" = Uuid, Path, description = "Schema ID"),
    ),
    request_body = UpdateCustomFieldSchemaRequest,
    responses(
        (status = 200, description = "Updated schema", body = CustomFieldSchema),
        (status = 403, description = "Not a team owner or leader", body = crate::error::ErrorResponse),
        (status = 404, description = "Schema not found", body = crate::error::ErrorResponse),
    )
)]
pub async fn update_schema(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path((team_id, schema_id)): Path<(String, Uuid)>,
    Json(body): Json<UpdateCustomFieldSchemaRequest>,
) -> AppResult<Json<CustomFieldSchema>> {
    auth_user.require_team_admin(&team_id)?;
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            &format!(
                "SELECT {SCHEMA_COLUMNS} FROM custom_field_schemas WHERE id = $1 AND team_id = $2"
            ),
            &[&schema_id, &team_id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Schema {schema_id} not found")))?;

    let current = CustomFieldSchema::from(&row);
    let name = body.name.unwrap_or(current.name);
    let required = body.required.unwrap_or(current.required);

    let updated = client
        .query_one(
            &format!(
                "UPDATE custom_field_schemas \
                 SET name = $1, required = $2, updated_at = NOW() \
                 WHERE id = $3 AND team_id = $4 \
                 RETURNING {SCHEMA_COLUMNS}"
            ),
            &[&name, &required, &schema_id, &team_id],
        )
        .await?;

    Ok(Json(CustomFieldSchema::from(&updated)))
}

// ── Delete schema ─────────────────────────────────────────────────────────────

#[utoipa::path(
    delete,
    path = "/teams/{team_id}/custom-field-schemas/{schema_id}",
    tag = "custom-fields",
    params(
        ("team_id" = String, Path, description = "Team ID"),
        ("schema_id" = Uuid, Path, description = "Schema ID"),
    ),
    responses(
        (status = 204, description = "Schema deleted"),
        (status = 403, description = "Not a team owner or leader", body = crate::error::ErrorResponse),
        (status = 404, description = "Schema not found", body = crate::error::ErrorResponse),
    )
)]
pub async fn delete_schema(
    auth_user: AuthUser,
    State(state): State<AppState>,
    Path((team_id, schema_id)): Path<(String, Uuid)>,
) -> AppResult<StatusCode> {
    auth_user.require_team_admin(&team_id)?;
    let client = state.db.get().await?;

    let n = client
        .execute(
            "DELETE FROM custom_field_schemas WHERE id = $1 AND team_id = $2",
            &[&schema_id, &team_id],
        )
        .await?;

    if n == 0 {
        return Err(AppError::NotFound(format!("Schema {schema_id} not found")));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── Validation helper (used by asset handlers) ────────────────────────────────

/// Validate `custom_fields` JSON against the team's schema definitions.
///
/// Rules:
/// - Every key in `fields` must correspond to a known schema `key` for the team.
/// - Values are type-checked against `field_type`.
/// - Unknown keys are rejected.
pub(crate) async fn validate_custom_fields(
    client: &Client,
    team_id: &str,
    fields: &JsonValue,
) -> AppResult<()> {
    let obj = fields.as_object().ok_or_else(|| {
        AppError::BadRequest("custom_fields must be a JSON object".into())
    })?;

    if obj.is_empty() {
        return Ok(());
    }

    let rows = client
        .query(
            "SELECT key, field_type FROM custom_field_schemas WHERE team_id = $1",
            &[&team_id],
        )
        .await?;

    let schema_map: std::collections::HashMap<String, String> = rows
        .iter()
        .map(|r| (r.get::<_, String>("key"), r.get::<_, String>("field_type")))
        .collect();

    for (key, value) in obj {
        let field_type_str = schema_map.get(key).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Unknown custom field key '{key}' for this team. \
                 Create a schema definition first."
            ))
        })?;

        let field_type: FieldType = field_type_str.parse().expect("valid field_type in DB");
        check_value_type(key, value, &field_type)?;
    }

    Ok(())
}

fn check_value_type(key: &str, value: &JsonValue, expected: &FieldType) -> AppResult<()> {
    let ok = match expected {
        FieldType::String => value.is_string() || value.is_null(),
        FieldType::Boolean => value.is_boolean() || value.is_null(),
        FieldType::Integer => value.is_i64() || value.is_null(),
        FieldType::Float => value.is_f64() || value.is_i64() || value.is_null(),
        FieldType::DateTime => value.is_string() || value.is_null(),
    };
    if !ok {
        return Err(AppError::BadRequest(format!(
            "Custom field '{key}' expects type {expected}, got {}",
            json_type_name(value)
        )));
    }
    Ok(())
}

fn json_type_name(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(n) if n.is_f64() => "float",
        JsonValue::Number(_) => "integer",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}
