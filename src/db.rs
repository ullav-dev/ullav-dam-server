use anyhow::{Context, Result};
use deadpool_postgres::{Config as PgConfig, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio_postgres::NoTls;

pub type DbPool = Pool;

pub fn create_pool(database_url: &str) -> Result<Pool> {
    let mut cfg = PgConfig::new();
    cfg.url = Some(database_url.to_string());
    cfg.manager = Some(ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    });

    cfg.create_pool(Some(Runtime::Tokio1), NoTls)
        .context("Failed to create database pool")
}

pub async fn run_migrations(pool: &Pool) -> Result<()> {
    let client = pool.get().await.context("Failed to get DB connection for migrations")?;

    let sql = include_str!("../migrations/001_initial.sql");
    client.batch_execute(sql).await.context("Failed to run migration 001")?;

    let sql = include_str!("../migrations/002_asset_metadata_fields.sql");
    client.batch_execute(sql).await.context("Failed to run migration 002")?;

    let sql = include_str!("../migrations/003_asset_is_locked.sql");
    client.batch_execute(sql).await.context("Failed to run migration 003")?;

    let sql = include_str!("../migrations/004_asset_visibility.sql");
    client.batch_execute(sql).await.context("Failed to run migration 004")?;

    let sql = include_str!("../migrations/005_category_access_level_creator.sql");
    client.batch_execute(sql).await.context("Failed to run migration 005")?;

    tracing::info!("Migrations applied successfully");
    Ok(())
}
