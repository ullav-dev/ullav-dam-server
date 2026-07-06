/// DAM (Comad) MCP server.
///
/// Exposes digital asset management tools over Streamable HTTP so that MCP
/// clients (Claude Code, Claude Desktop, etc.) can search, browse, and inspect
/// DAM assets without bespoke REST integration.
///
/// Auth: audience-bound RS256 token validated by `mcp_auth_middleware` from
/// ullav-mcp-auth, plus a `dam:tools` scope guard.
use std::sync::Arc;

use axum::http::request::Parts;
use base64::Engine;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{ServerCapabilities, ServerInfo},
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use ullav_mcp_auth::McpClaims;
use uuid::Uuid;

use crate::db::DbPool;
use crate::handlers::assets::image_dimensions;
use crate::storage::StorageClient;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn caller_from_ctx(ctx: &RequestContext<RoleServer>) -> Result<McpClaims, rmcp::ErrorData> {
    let parts = ctx
        .extensions
        .get::<Parts>()
        .ok_or_else(|| rmcp::ErrorData::internal_error("missing request parts", None))?;
    parts
        .extensions
        .get::<McpClaims>()
        .cloned()
        .ok_or_else(|| rmcp::ErrorData::internal_error("missing caller identity", None))
}

fn db_err(e: impl std::fmt::Display) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(format!("database error: {e}"), None)
}

fn not_found(kind: &str, id: &str) -> rmcp::ErrorData {
    rmcp::ErrorData::invalid_params(format!("{kind} '{id}' not found"), None)
}

fn parse_uuid(s: &str) -> Result<uuid::Uuid, rmcp::ErrorData> {
    s.parse::<uuid::Uuid>()
        .map_err(|_| rmcp::ErrorData::invalid_params(format!("'{s}' is not a valid UUID"), None))
}

/// Build the same `AuthUser` the REST API's extractor builds, from MCP token
/// claims instead of an HTTP `Authorization` header — so both surfaces enforce
/// identical plan-based quota/access rules.
fn auth_user_from_claims(claims: &McpClaims) -> crate::auth::AuthUser {
    let teams = claims
        .teams
        .iter()
        .map(|(id, claim)| (id.clone(), claim.role.clone()))
        .collect();
    crate::auth::AuthUser::from_claims(claims.sub.clone(), &claims.roles, &claims.subscriptions, teams)
}

/// Map guard-method failures (`AuthUser::require_*`) to an MCP tool error.
fn app_err(e: crate::error::AppError) -> rmcp::ErrorData {
    match e {
        crate::error::AppError::Forbidden(msg) => rmcp::ErrorData::invalid_request(msg, None),
        other => rmcp::ErrorData::internal_error(other.to_string(), None),
    }
}

// ── Request parameter types ───────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchAssetsParams {
    /// Search query — matched against name, caption, description, keywords, and OCR text.
    pub query: String,
    /// Maximum number of results to return (default 20, max 100).
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetAssetParams {
    /// UUID of the asset.
    pub asset_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListAssetsParams {
    /// Optional UUID of a category to filter by.
    pub category_id: Option<String>,
    /// Maximum number of results to return (default 20, max 100).
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateCategoryParams {
    /// Name of the new category.
    pub name: String,
    /// Slash-separated path to the parent category (e.g. "a/b/c"), walked from the
    /// root by name. Every segment must already exist. Omit to create a top-level category.
    pub parent_path: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadAssetParams {
    /// Name of the file, including extension (e.g. "photo.jpg"). Used as the asset
    /// name if `name` is omitted, and to infer the MIME type if `asset_type` is omitted.
    pub file_name: String,
    /// Raw file bytes, base64-encoded.
    pub file_base64: String,
    /// Asset name — defaults to `file_name` if omitted.
    pub name: Option<String>,
    /// MIME type — inferred from `file_name`'s extension if omitted.
    pub asset_type: Option<String>,
    /// Slash-separated path to an existing category (e.g. "a/b/c") to file the asset
    /// under, walked from the root by name. Every segment must already exist.
    pub category_path: Option<String>,
    pub description: Option<String>,
    pub caption: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub copyright_notice: Option<String>,
}

// ── Server implementation ─────────────────────────────────────────────────────

pub struct DamServer {
    db: DbPool,
    storage: StorageClient,
}

impl DamServer {
    fn new(db: DbPool, storage: StorageClient) -> Self {
        Self { db, storage }
    }
}

/// Walk a slash-separated category path (e.g. "a/b/c") from the root, matching each
/// segment's `name` under the previous segment's `id` and scoped to `owner_id`.
/// Returns the id of the final segment. Errors naming the first missing segment.
async fn resolve_category_path(
    client: &deadpool_postgres::Client,
    owner_id: &str,
    path: &str,
) -> Result<uuid::Uuid, rmcp::ErrorData> {
    let mut parent_id: Option<uuid::Uuid> = None;
    let mut walked = String::new();

    for segment in path.split('/').map(str::trim).filter(|s| !s.is_empty()) {
        let row = client
            .query_opt(
                "SELECT id FROM categories WHERE name = $1 AND owner_id = $2
                 AND parent_id IS NOT DISTINCT FROM $3",
                &[&segment, &owner_id, &parent_id],
            )
            .await
            .map_err(db_err)?;

        let id: uuid::Uuid = row
            .ok_or_else(|| {
                let location = if walked.is_empty() {
                    "at root".to_string()
                } else {
                    format!("under '{walked}'")
                };
                rmcp::ErrorData::invalid_params(
                    format!("category '{segment}' not found {location}"),
                    None,
                )
            })?
            .get("id");

        parent_id = Some(id);
        if !walked.is_empty() {
            walked.push('/');
        }
        walked.push_str(segment);
    }

    parent_id.ok_or_else(|| rmcp::ErrorData::invalid_params("empty category path", None))
}

#[tool_router]
impl DamServer {
    /// Search DAM assets by name, caption, description, keywords, or OCR text.
    #[tool(
        description = "Search digital assets by name, caption, description, keywords, or OCR text"
    )]
    async fn search_assets(
        &self,
        Parameters(p): Parameters<SearchAssetsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, rmcp::ErrorData> {
        let claims = caller_from_ctx(&context)?;
        let limit = p.limit.unwrap_or(20).min(100);
        let pattern = format!("%{}%", p.query);
        let client = self.db.get().await.map_err(db_err)?;

        let rows = client
            .query(
                "SELECT id, name, description, asset_type, size, caption, keywords, creator,
                        copyright_notice, width, height, created_at
                 FROM assets
                 WHERE (owner_id = $1 OR is_private = false)
                   AND (name        ILIKE $2
                     OR caption     ILIKE $2
                     OR description ILIKE $2
                     OR keywords    ILIKE $2
                     OR ocr_text    ILIKE $2)
                 ORDER BY updated_at DESC
                 LIMIT $3",
                &[&claims.sub, &pattern, &limit],
            )
            .await
            .map_err(db_err)?;

        let assets: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id":               r.get::<_, uuid::Uuid>("id"),
                    "name":             r.get::<_, String>("name"),
                    "description":      r.get::<_, Option<String>>("description"),
                    "asset_type":       r.get::<_, String>("asset_type"),
                    "size":             r.get::<_, i64>("size"),
                    "caption":          r.get::<_, Option<String>>("caption"),
                    "keywords":         r.get::<_, Option<String>>("keywords"),
                    "creator":          r.get::<_, Option<String>>("creator"),
                    "copyright_notice": r.get::<_, Option<String>>("copyright_notice"),
                    "width":            r.get::<_, Option<i32>>("width"),
                    "height":           r.get::<_, Option<i32>>("height"),
                    "created_at":       r.get::<_, chrono::DateTime<chrono::Utc>>("created_at"),
                })
            })
            .collect();

        Ok(serde_json::to_string_pretty(&assets).unwrap())
    }

    /// Get full details of a single DAM asset.
    #[tool(
        description = "Get full details of a DAM asset including metadata, dimensions, and rights information"
    )]
    async fn get_asset(
        &self,
        Parameters(p): Parameters<GetAssetParams>,
    ) -> Result<String, rmcp::ErrorData> {
        let id = parse_uuid(&p.asset_id)?;
        let client = self.db.get().await.map_err(db_err)?;

        let row = client
            .query_opt(
                "SELECT id, owner_id, name, description, asset_type, size,
                        caption, keywords, creator, copyright_notice,
                        available, available_until, is_locked, is_private,
                        public_read, public_download, team_id, custom_fields,
                        ocr_text, width, height, created_at, updated_at
                 FROM assets WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(db_err)?
            .ok_or_else(|| not_found("asset", &p.asset_id))?;

        let result = serde_json::json!({
            "id":               row.get::<_, uuid::Uuid>("id"),
            "owner_id":         row.get::<_, String>("owner_id"),
            "name":             row.get::<_, String>("name"),
            "description":      row.get::<_, Option<String>>("description"),
            "asset_type":       row.get::<_, String>("asset_type"),
            "size":             row.get::<_, i64>("size"),
            "caption":          row.get::<_, Option<String>>("caption"),
            "keywords":         row.get::<_, Option<String>>("keywords"),
            "creator":          row.get::<_, Option<String>>("creator"),
            "copyright_notice": row.get::<_, Option<String>>("copyright_notice"),
            "available":        row.get::<_, bool>("available"),
            "available_until":  row.get::<_, Option<chrono::DateTime<chrono::Utc>>>("available_until"),
            "is_locked":        row.get::<_, bool>("is_locked"),
            "is_private":       row.get::<_, bool>("is_private"),
            "public_read":      row.get::<_, bool>("public_read"),
            "public_download":  row.get::<_, bool>("public_download"),
            "team_id":          row.get::<_, Option<String>>("team_id"),
            "custom_fields":    row.get::<_, Option<serde_json::Value>>("custom_fields"),
            "ocr_text":         row.get::<_, Option<String>>("ocr_text"),
            "width":            row.get::<_, Option<i32>>("width"),
            "height":           row.get::<_, Option<i32>>("height"),
            "created_at":       row.get::<_, chrono::DateTime<chrono::Utc>>("created_at"),
            "updated_at":       row.get::<_, chrono::DateTime<chrono::Utc>>("updated_at"),
        });

        Ok(serde_json::to_string_pretty(&result).unwrap())
    }

    /// List DAM assets, optionally filtered by category, ordered by most recently updated.
    #[tool(description = "List DAM assets, optionally filtered by category")]
    async fn list_assets(
        &self,
        Parameters(p): Parameters<ListAssetsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, rmcp::ErrorData> {
        let claims = caller_from_ctx(&context)?;
        let limit = p.limit.unwrap_or(20).min(100);
        let category_id = p.category_id.as_deref().map(parse_uuid).transpose()?;
        let client = self.db.get().await.map_err(db_err)?;

        let rows = client
            .query(
                "SELECT a.id, a.name, a.description, a.asset_type, a.size,
                        a.caption, a.keywords, a.creator, a.width, a.height, a.updated_at
                 FROM assets a
                 WHERE (a.owner_id = $1 OR a.is_private = false)
                   AND ($2::UUID IS NULL OR EXISTS (
                       SELECT 1 FROM asset_categories ac
                       WHERE ac.asset_id = a.id AND ac.category_id = $2
                   ))
                 ORDER BY a.updated_at DESC
                 LIMIT $3",
                &[&claims.sub, &category_id, &limit],
            )
            .await
            .map_err(db_err)?;

        let assets: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id":         r.get::<_, uuid::Uuid>("id"),
                    "name":       r.get::<_, String>("name"),
                    "description": r.get::<_, Option<String>>("description"),
                    "asset_type": r.get::<_, String>("asset_type"),
                    "size":       r.get::<_, i64>("size"),
                    "caption":    r.get::<_, Option<String>>("caption"),
                    "keywords":   r.get::<_, Option<String>>("keywords"),
                    "creator":    r.get::<_, Option<String>>("creator"),
                    "width":      r.get::<_, Option<i32>>("width"),
                    "height":     r.get::<_, Option<i32>>("height"),
                    "updated_at": r.get::<_, chrono::DateTime<chrono::Utc>>("updated_at"),
                })
            })
            .collect();

        Ok(serde_json::to_string_pretty(&assets).unwrap())
    }

    /// List all asset categories.
    #[tool(description = "List all asset categories with their parent relationships")]
    async fn list_categories(&self) -> Result<String, rmcp::ErrorData> {
        let client = self.db.get().await.map_err(db_err)?;

        let rows = client
            .query(
                "SELECT id, name, parent_id FROM categories ORDER BY name ASC",
                &[],
            )
            .await
            .map_err(db_err)?;

        let categories: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id":        r.get::<_, uuid::Uuid>("id"),
                    "name":      r.get::<_, String>("name"),
                    "parent_id": r.get::<_, Option<uuid::Uuid>>("parent_id"),
                })
            })
            .collect();

        Ok(serde_json::to_string_pretty(&categories).unwrap())
    }

    /// Create a new asset category, optionally nested under a parent path.
    #[tool(
        description = "Create a new asset category. Optionally nest it under a parent \
        category by specifying parent_path as a slash-separated path (e.g. \"a/b/c\") of \
        existing category names walked from the root."
    )]
    async fn create_category(
        &self,
        Parameters(p): Parameters<CreateCategoryParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, rmcp::ErrorData> {
        let claims = caller_from_ctx(&context)?;
        let auth_user = auth_user_from_claims(&claims);
        auth_user.require_access().map_err(app_err)?;
        let client = self.db.get().await.map_err(db_err)?;

        let count_row = client
            .query_one(
                "SELECT COUNT(*) AS cnt FROM categories WHERE owner_id = $1",
                &[&claims.sub],
            )
            .await
            .map_err(db_err)?;
        let current_count: i64 = count_row.get("cnt");
        auth_user.require_category_quota(current_count).map_err(app_err)?;

        let parent_id = match &p.parent_path {
            Some(path) => Some(resolve_category_path(&client, &claims.sub, path).await?),
            None => None,
        };

        let row = client
            .query_one(
                "INSERT INTO categories (name, description, parent_id, access_level, creator, owner_id)
                 VALUES ($1, $2, $3, 'Private', $4, $5)
                 RETURNING id, name, description, parent_id, access_level, creator, owner_id, created_at, updated_at",
                &[&p.name, &p.description, &parent_id, &p.creator, &claims.sub],
            )
            .await
            .map_err(db_err)?;

        let result = serde_json::json!({
            "id":          row.get::<_, uuid::Uuid>("id"),
            "name":        row.get::<_, String>("name"),
            "description": row.get::<_, Option<String>>("description"),
            "parent_id":   row.get::<_, Option<uuid::Uuid>>("parent_id"),
            "creator":     row.get::<_, Option<String>>("creator"),
            "owner_id":    row.get::<_, String>("owner_id"),
            "created_at":  row.get::<_, chrono::DateTime<chrono::Utc>>("created_at"),
        });

        Ok(serde_json::to_string_pretty(&result).unwrap())
    }

    /// Create a new asset and upload its file content in one call.
    #[tool(
        description = "Create a new DAM asset by uploading file content (base64-encoded) \
        directly. Optionally file it under an existing category by specifying category_path \
        as a slash-separated path (e.g. \"a/b/c\") of existing category names walked from the root."
    )]
    async fn upload_asset(
        &self,
        Parameters(p): Parameters<UploadAssetParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, rmcp::ErrorData> {
        let claims = caller_from_ctx(&context)?;
        let auth_user = auth_user_from_claims(&claims);
        let client = self.db.get().await.map_err(db_err)?;

        let category_id = match &p.category_path {
            Some(path) => Some(resolve_category_path(&client, &claims.sub, path).await?),
            None => None,
        };

        let file_data = base64::engine::general_purpose::STANDARD
            .decode(p.file_base64.as_bytes())
            .map_err(|e| {
                rmcp::ErrorData::invalid_params(format!("invalid base64 in file_base64: {e}"), None)
            })?;
        let file_data = bytes::Bytes::from(file_data);

        let name = p.name.unwrap_or_else(|| p.file_name.clone());
        let asset_type = p.asset_type.unwrap_or_else(|| {
            mime_guess::from_path(&p.file_name)
                .first_or_octet_stream()
                .to_string()
        });
        auth_user.require_mime_allowed(&asset_type).map_err(app_err)?;

        let count_row = client
            .query_one(
                "SELECT COUNT(*) AS n FROM assets WHERE owner_id = $1",
                &[&claims.sub],
            )
            .await
            .map_err(db_err)?;
        let count: i64 = count_row.get("n");
        auth_user.require_asset_quota(count).map_err(app_err)?;

        let size = file_data.len() as i64;
        let usage_row = client
            .query_one(
                "SELECT CAST(COALESCE(SUM(size), 0) AS BIGINT) AS used FROM assets WHERE owner_id = $1",
                &[&claims.sub],
            )
            .await
            .map_err(db_err)?;
        let used_bytes: i64 = usage_row.get("used");
        auth_user.require_storage_quota(used_bytes, size).map_err(app_err)?;

        let asset_id = Uuid::new_v4();
        let storage_key = format!("assets/{}/{}", asset_id, p.file_name);
        let dims = image_dimensions(&file_data);
        let (width, height) = (dims.map(|d| d.0), dims.map(|d| d.1));

        self.storage
            .upload(&storage_key, file_data, &asset_type)
            .await
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("storage upload failed: {e}"), None)
            })?;

        let row = client
            .query_one(
                "INSERT INTO assets
                 (id, owner_id, name, description, asset_type, size, storage_key, bucket,
                  caption, keywords, creator, copyright_notice, width, height)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                 RETURNING id, name, description, asset_type, size, caption, keywords, creator,
                           copyright_notice, width, height, created_at",
                &[
                    &asset_id,
                    &claims.sub,
                    &name,
                    &p.description,
                    &asset_type,
                    &size,
                    &storage_key,
                    &self.storage.bucket,
                    &p.caption,
                    &p.keywords,
                    &p.creator,
                    &p.copyright_notice,
                    &width,
                    &height,
                ],
            )
            .await
            .map_err(db_err)?;

        if let Some(category_id) = category_id {
            client
                .execute(
                    "INSERT INTO asset_categories (asset_id, category_id) VALUES ($1, $2)
                     ON CONFLICT DO NOTHING",
                    &[&asset_id, &category_id],
                )
                .await
                .map_err(db_err)?;
        }

        let result = serde_json::json!({
            "id":               row.get::<_, uuid::Uuid>("id"),
            "name":             row.get::<_, String>("name"),
            "description":      row.get::<_, Option<String>>("description"),
            "asset_type":       row.get::<_, String>("asset_type"),
            "size":             row.get::<_, i64>("size"),
            "caption":          row.get::<_, Option<String>>("caption"),
            "keywords":         row.get::<_, Option<String>>("keywords"),
            "creator":          row.get::<_, Option<String>>("creator"),
            "copyright_notice": row.get::<_, Option<String>>("copyright_notice"),
            "width":            row.get::<_, Option<i32>>("width"),
            "height":           row.get::<_, Option<i32>>("height"),
            "created_at":       row.get::<_, chrono::DateTime<chrono::Utc>>("created_at"),
        });

        Ok(serde_json::to_string_pretty(&result).unwrap())
    }
}

#[tool_handler]
impl rmcp::ServerHandler for DamServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "DAM (Comad) MCP server — Digital Asset Management. \
             Use list_categories to browse the category tree. \
             Use list_assets to browse assets (filter by category_id). \
             Use search_assets to find assets by name, caption, keywords, or OCR text. \
             Use get_asset for full detail on a specific asset. \
             Use create_category to create a new category, optionally nested under an \
             existing parent via a slash-separated path (e.g. \"a/b/c\"). \
             Use upload_asset to create a new asset from base64-encoded file content, \
             optionally filed under an existing category via the same path syntax.",
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uuid_rejects_invalid() {
        assert!(parse_uuid("not-a-uuid").is_err());
        assert!(parse_uuid("").is_err());
    }

    #[test]
    fn parse_uuid_accepts_valid() {
        assert!(parse_uuid("550e8400-e29b-41d4-a716-446655440000").is_ok());
    }

    // ── auth_user_from_claims (the REST↔MCP quota-enforcement bridge) ────────

    fn mcp_claims_with_subscription(product: &str, tier: &str, status: &str) -> McpClaims {
        let mut subscriptions = std::collections::HashMap::new();
        subscriptions.insert(
            product.to_string(),
            ullav_mcp_auth::SubscriptionClaim { tier: tier.to_string(), status: status.to_string() },
        );
        McpClaims {
            iss: "http://localhost:8081".into(),
            sub: "user-1".into(),
            aud: "http://localhost:8080/mcp".into(),
            iat: 0,
            exp: 0,
            scope: "dam:tools".into(),
            client_id: "claude".into(),
            username: "testuser".into(),
            roles: vec![],
            subscriptions,
            teams: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn auth_user_from_claims_enforces_images_only_mime_restriction() {
        let claims = mcp_claims_with_subscription("comad", "individual", "active");
        let auth_user = auth_user_from_claims(&claims);

        assert!(auth_user.require_mime_allowed("image/jpeg").is_ok());
        assert!(auth_user.require_mime_allowed("video/mp4").is_err());
        assert!(auth_user.require_asset_quota(0).is_ok());
        assert!(auth_user.require_asset_quota(500).is_err(), "Individual plan asset limit is 500");
    }

    #[test]
    fn auth_user_from_claims_blocks_writes_with_no_subscription() {
        let claims = mcp_claims_with_subscription("comad", "individual", "cancelled");
        let auth_user = auth_user_from_claims(&claims);

        assert!(auth_user.require_access().is_err());
    }
}

// ── Service factory ───────────────────────────────────────────────────────────

pub fn make_dam_mcp_service(
    db: DbPool,
    storage: StorageClient,
    external_host: &str,
) -> StreamableHttpService<DamServer, LocalSessionManager> {
    let session_manager = Arc::new(LocalSessionManager::default());
    let config = StreamableHttpServerConfig::default().with_allowed_hosts([
        "localhost",
        "127.0.0.1",
        "::1",
        external_host,
    ]);
    StreamableHttpService::new(
        move || Ok(DamServer::new(db.clone(), storage.clone())),
        session_manager,
        config,
    )
}
