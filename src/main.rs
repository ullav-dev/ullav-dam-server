use anyhow::Result;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use uuid::Uuid;

mod auth;
mod config;
mod db;
mod error;
mod handlers;
mod models;
mod storage;

use config::Config;
use db::DbPool;
use storage::StorageClient;

pub type ThumbnailCache = Arc<RwLock<HashMap<Uuid, Bytes>>>;

#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub storage: StorageClient,
    pub thumbnail_cache: ThumbnailCache,
    pub thumbnail_size: u32,
    pub jwt_secret: String,
    pub auth_service_url: String,
    pub auth_client: reqwest::Client,
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "DAM Server API",
        version = "0.1.0",
        description = "Digital Asset Management HTTP API"
    ),
    paths(
        handlers::assets::list_assets,
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
        handlers::assets::get_usage,
        handlers::categories::list_categories,
        handlers::categories::get_category,
        handlers::categories::create_category,
        handlers::categories::update_category,
        handlers::categories::delete_category,
    ),
    components(schemas(
        models::asset::Asset,
        models::asset::AssetWithCategories,
        models::asset::CreateAssetRequest,
        models::asset::UpdateAssetRequest,
        handlers::assets::UsageSummary,
        models::category::AccessLevel,
        models::category::Category,
        models::category::CategoryWithChildren,
        models::category::CreateCategoryRequest,
        models::category::UpdateCategoryRequest,
        error::ErrorResponse,
    )),
    tags(
        (name = "assets", description = "Asset management"),
        (name = "categories", description = "Category management"),
    )
)]
struct ApiDoc;

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

    let state = AppState {
        db: pool,
        storage,
        thumbnail_cache: Arc::new(RwLock::new(HashMap::new())),
        thumbnail_size: cfg.thumbnail_size,
        jwt_secret: cfg.jwt_secret,
        auth_service_url: cfg.auth_service_url,
        auth_client: reqwest::Client::new(),
    };

    let app = Router::new()
        // Docs
        .merge(SwaggerUi::new("/docs").url("/api-doc/openapi.json", ApiDoc::openapi()))
        // Auth proxy (forwards to ullav-user-management)
        .route("/auth/login", post(handlers::auth::login))
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
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    tracing::info!("Listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
