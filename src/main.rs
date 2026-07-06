use anyhow::Result;
use axum::{
    Router,
    extract::{DefaultBodyLimit, Extension, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post, put},
};
use bytes::Bytes;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use uuid::Uuid;
use ullav_mcp_auth::{mcp_auth_middleware, protected_resource_metadata, McpClaims, ProtectedResourceConfig, TokenValidator};

mod auth;
mod config;
mod db;
mod error;
mod handlers;
mod mcp;
mod metadata;
mod models;
mod storage;

use config::Config;
use db::DbPool;
use storage::StorageClient;

pub type ThumbnailCache = Arc<Mutex<LruCache<Uuid, Bytes>>>;

#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub storage: StorageClient,
    pub thumbnail_cache: ThumbnailCache,
    pub thumbnail_size: u32,
    pub api_validator: ullav_mcp_auth::TokenValidator,
    pub auth_service_url: String,
    pub auth_client: reqwest::Client,
    pub public_base_url: String,
    pub mcp_token_validator: TokenValidator,
    pub mcp_prc: ProtectedResourceConfig,
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "DAM Server API",
        version = "0.1.0",
        description = "Digital Asset Management HTTP API"
    ),
    paths(
        health,
        handlers::assets::list_assets,
        handlers::assets::list_geotagged,
        handlers::assets::get_asset,
        handlers::assets::create_asset,
        handlers::assets::upload_asset,
        handlers::assets::download_asset,
        handlers::assets::update_asset,
        handlers::assets::delete_asset,
        handlers::assets::create_and_upload_asset,
        handlers::assets::get_thumbnail,
        handlers::assets::delete_thumbnail,
        handlers::assets::add_category_to_asset,
        handlers::assets::remove_category_from_asset,
        handlers::metadata::get_asset_metadata,
        handlers::metadata::refresh_asset_metadata,
        handlers::search::search_assets,
        handlers::search::search_nearby,
        handlers::assets::get_usage,
        handlers::categories::list_categories,
        handlers::categories::get_category,
        handlers::categories::create_category,
        handlers::categories::update_category,
        handlers::categories::delete_category,
        handlers::custom_fields::list_schemas,
        handlers::custom_fields::create_schema,
        handlers::custom_fields::update_schema,
        handlers::custom_fields::delete_schema,
        handlers::iiif::get_manifest,
        handlers::iiif::get_collection,
        handlers::iiif::get_image_info,
        handlers::iiif::get_image,
    ),
    components(schemas(
        models::asset::Asset,
        models::asset::AssetWithCategories,
        models::asset::AssetGeoPoint,
        models::asset::AssetPage,
        models::asset::AssetQuery,
        models::asset::GeoQuery,
        models::asset::CreateAssetRequest,
        models::asset::UpdateAssetRequest,
        models::asset_metadata::AssetMetadata,
        handlers::search::AssetSearchResult,
        handlers::search::AssetMetadataSummary,
        handlers::assets::UsageSummary,
        models::category::AccessLevel,
        models::category::Category,
        models::category::CategoryWithChildren,
        models::category::CreateCategoryRequest,
        models::category::UpdateCategoryRequest,
        models::custom_field_schema::CustomFieldSchema,
        models::custom_field_schema::FieldType,
        models::custom_field_schema::CreateCustomFieldSchemaRequest,
        models::custom_field_schema::UpdateCustomFieldSchemaRequest,
        error::ErrorResponse,
    )),
    tags(
        (name = "assets", description = "Asset management"),
        (name = "metadata", description = "Asset metadata (EXIF/IPTC/XMP)"),
        (name = "search", description = "Metadata-driven asset search"),
        (name = "categories", description = "Category management"),
        (name = "custom-fields", description = "Team custom field schema management"),
        (name = "iiif", description = "IIIF Presentation API 3.0 manifests and collections"),
    )
)]
struct ApiDoc;

fn host_from_uri(uri: &str) -> &str {
    let without_scheme = uri.find("://").map(|i| &uri[i + 3..]).unwrap_or(uri);
    without_scheme.split('/').next().unwrap_or(without_scheme)
}

async fn dam_scope_guard(req: Request<axum::body::Body>, next: Next) -> Result<Response, StatusCode> {
    let claims = req.extensions().get::<McpClaims>().ok_or(StatusCode::UNAUTHORIZED)?;
    if !claims.scope.split_whitespace().any(|s| s == "dam:tools") {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(req).await)
}

/// Check service health (liveness + DB connectivity)
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is healthy"),
        (status = 503, description = "Database unavailable"),
    ),
    tag = "health"
)]
async fn health(State(state): State<AppState>) -> StatusCode {
    match state.db.get().await {
        Ok(client) => match client.query_one("SELECT 1", &[]).await {
            Ok(_) => StatusCode::OK,
            Err(_) => StatusCode::SERVICE_UNAVAILABLE,
        },
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Logging
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load .env if present
    let _ = dotenvy::dotenv();

    let cfg = Config::from_env()?;

    // Database
    let pool = db::create_pool(&cfg.database_url)?;
    db::run_migrations(&pool).await?;

    // Storage
    let storage = StorageClient::new(&cfg).await?;

    let addr: std::net::SocketAddr = cfg.bind_addr().parse()?;

    let mcp_token_validator = TokenValidator::new(
        cfg.oauth2_jwks_url.clone(),
        cfg.oauth2_issuer.clone(),
        cfg.dam_mcp_canonical_uri.clone(),
    );
    let mcp_prc = ProtectedResourceConfig {
        resource_uri: cfg.dam_mcp_canonical_uri.clone(),
        authorization_server: cfg.oauth2_issuer.clone(),
        scopes_supported: vec!["dam:tools".to_owned()],
        jwks_uri: cfg.oauth2_jwks_url.clone(),
    };

    let state = AppState {
        db: pool.clone(),
        storage: storage.clone(),
        thumbnail_cache: Arc::new(Mutex::new(LruCache::new(
            NonZeroUsize::new(cfg.thumbnail_cache_capacity).unwrap_or(NonZeroUsize::new(512).unwrap()),
        ))),
        thumbnail_size: cfg.thumbnail_size,
        api_validator: ullav_mcp_auth::TokenValidator::new(
            cfg.oauth2_jwks_url,
            cfg.oauth2_issuer,
            String::new(),
        ),
        auth_service_url: cfg.auth_service_url,
        auth_client: reqwest::Client::new(),
        public_base_url: cfg.public_base_url,
        mcp_token_validator: mcp_token_validator.clone(),
        mcp_prc: mcp_prc.clone(),
    };

    let app = Router::new()
        // Docs
        .merge(SwaggerUi::new("/docs").url("/api-doc/openapi.json", ApiDoc::openapi()))
        // Health
        .route("/health", get(health))
        // Auth proxy (forwards to ullav-user-management)
        .route("/auth/login", post(handlers::auth::login))
        // Search (must be before /assets/:id to avoid capture)
        .route("/assets/search", get(handlers::search::search_assets))
        .route("/assets/search/nearby", get(handlers::search::search_nearby))
        .route("/assets/geotagged", get(handlers::assets::list_geotagged))
        // Assets
        .route("/usage", get(handlers::assets::get_usage))
        .route("/assets", get(handlers::assets::list_assets).post(handlers::assets::create_asset))
        .route(
            "/assets/:id",
            get(handlers::assets::get_asset)
                .put(handlers::assets::update_asset)
                .delete(handlers::assets::delete_asset),
        )
        .route("/assets/upload", post(handlers::assets::create_and_upload_asset)
            .layer(DefaultBodyLimit::max(200 * 1024 * 1024)))
        .route("/assets/:id/upload", post(handlers::assets::upload_asset)
            .layer(DefaultBodyLimit::max(200 * 1024 * 1024)))
        .route("/assets/:id/download", get(handlers::assets::download_asset))
        .route("/assets/:id/thumbnail", get(handlers::assets::get_thumbnail).delete(handlers::assets::delete_thumbnail))
        .route("/assets/:id/metadata", get(handlers::metadata::get_asset_metadata))
        .route("/assets/:id/metadata/refresh", post(handlers::metadata::refresh_asset_metadata))
        .route(
            "/assets/:asset_id/categories/:category_id",
            post(handlers::assets::add_category_to_asset)
                .delete(handlers::assets::remove_category_from_asset),
        )
        // ZIP import
        .route("/zip/upload", post(handlers::zip::upload_zip)
            .layer(DefaultBodyLimit::max(200 * 1024 * 1024)))
        // Categories
        .route("/categories", get(handlers::categories::list_categories).post(handlers::categories::create_category))
        .route(
            "/categories/:id",
            get(handlers::categories::get_category)
                .put(handlers::categories::update_category)
                .delete(handlers::categories::delete_category),
        )
        // Custom field schemas (team-scoped)
        .route(
            "/teams/:team_id/custom-field-schemas",
            get(handlers::custom_fields::list_schemas)
                .post(handlers::custom_fields::create_schema),
        )
        .route(
            "/teams/:team_id/custom-field-schemas/:schema_id",
            put(handlers::custom_fields::update_schema)
                .delete(handlers::custom_fields::delete_schema),
        )
        // IIIF Presentation API 3.0 + Image API 3.0
        .route("/iiif/manifest/:id", get(handlers::iiif::get_manifest))
        .route("/iiif/collection/:id", get(handlers::iiif::get_collection))
        .route("/iiif/image/:id/info.json", get(handlers::iiif::get_image_info))
        .route(
            "/iiif/image/:id/:region/:size/:rotation/:quality_fmt",
            get(handlers::iiif::get_image),
        )
        // DAM MCP — audience-bound RS256 + dam:tools scope guard.
        .merge(
            Router::new()
                .route_service("/mcp", mcp::make_dam_mcp_service(pool, storage, host_from_uri(&cfg.dam_mcp_canonical_uri)))
                .layer(middleware::from_fn(dam_scope_guard))
                .layer(middleware::from_fn(mcp_auth_middleware))
                .layer(Extension(mcp_token_validator))
                .layer(Extension(mcp_prc.clone())),
        )
        // RFC 9728 protected resource metadata.
        .merge(
            Router::new()
                .route("/.well-known/oauth-protected-resource/mcp", get(protected_resource_metadata))
                .layer(Extension(mcp_prc)),
        )
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    tracing::info!("Listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
