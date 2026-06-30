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
use rmcp::{
    RoleServer,
    handler::server::wrapper::Parameters,
    model::{ServerCapabilities, ServerInfo},
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, StreamableHttpServerConfig,
    session::local::LocalSessionManager,
};
use schemars::JsonSchema;
use serde::Deserialize;
use ullav_mcp_auth::McpClaims;

use crate::db::DbPool;

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

// ── Server implementation ─────────────────────────────────────────────────────

pub struct DamServer {
    db: DbPool,
}

impl DamServer {
    fn new(db: DbPool) -> Self {
        Self { db }
    }
}

#[tool_router]
impl DamServer {
    /// Search DAM assets by name, caption, description, keywords, or OCR text.
    #[tool(description = "Search digital assets by name, caption, description, keywords, or OCR text")]
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
    #[tool(description = "Get full details of a DAM asset including metadata, dimensions, and rights information")]
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
        let category_id = p.category_id
            .as_deref()
            .map(parse_uuid)
            .transpose()?;
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
}

#[tool_handler]
impl rmcp::ServerHandler for DamServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_instructions(
            "DAM (Comad) MCP server — Digital Asset Management. \
             Use list_categories to browse the category tree. \
             Use list_assets to browse assets (filter by category_id). \
             Use search_assets to find assets by name, caption, keywords, or OCR text. \
             Use get_asset for full detail on a specific asset.",
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
}

// ── Service factory ───────────────────────────────────────────────────────────

pub fn make_dam_mcp_service(
    db: DbPool,
    external_host: &str,
) -> StreamableHttpService<DamServer, LocalSessionManager> {
    let session_manager = Arc::new(LocalSessionManager::default());
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(["localhost", "127.0.0.1", "::1", external_host]);
    StreamableHttpService::new(
        move || Ok(DamServer::new(db.clone())),
        session_manager,
        config,
    )
}
