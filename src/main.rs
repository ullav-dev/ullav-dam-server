use anyhow::Result;
use axum::{
    Router,
    routing::{get, post},
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod config;
mod db;
mod error;
mod handlers;
mod models;
mod storage;

use config::Config;
use db::DbPool;
use storage::StorageClient;

#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub storage: StorageClient,
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

    let state = AppState { db: pool, storage };

    let app = Router::new()
        // Assets
        .route("/assets", get(handlers::assets::list_assets).post(handlers::assets::create_asset))
        .route(
            "/assets/:id",
            get(handlers::assets::get_asset)
                .put(handlers::assets::update_asset)
                .delete(handlers::assets::delete_asset),
        )
        .route("/assets/:id/upload", post(handlers::assets::upload_asset))
        .route("/assets/:id/download", get(handlers::assets::download_asset))
        .route(
            "/assets/:asset_id/categories/:category_id",
            post(handlers::assets::add_category_to_asset)
                .delete(handlers::assets::remove_category_from_asset),
        )
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

    let addr: std::net::SocketAddr = cfg.bind_addr().parse()?;
    tracing::info!("Listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
