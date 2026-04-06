use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub s3_endpoint: String,
    pub s3_access_key_id: String,
    pub s3_secret_access_key: String,
    pub s3_bucket: String,
    pub s3_region: String,
    pub thumbnail_size: u32,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".into())
                .parse()
                .context("PORT must be a valid number")?,
            database_url: std::env::var("DATABASE_URL")
                .context("DATABASE_URL is required")?,
            s3_endpoint: std::env::var("S3_ENDPOINT")
                .context("S3_ENDPOINT is required")?,
            s3_access_key_id: std::env::var("S3_ACCESS_KEY_ID")
                .context("S3_ACCESS_KEY_ID is required")?,
            s3_secret_access_key: std::env::var("S3_SECRET_ACCESS_KEY")
                .context("S3_SECRET_ACCESS_KEY is required")?,
            s3_bucket: std::env::var("S3_BUCKET")
                .unwrap_or_else(|_| "dam-assets".into()),
            s3_region: std::env::var("S3_REGION")
                .unwrap_or_else(|_| "us-east-1".into()),
            thumbnail_size: std::env::var("THUMBNAIL_SIZE")
                .unwrap_or_else(|_| "256".into())
                .parse()
                .context("THUMBNAIL_SIZE must be a positive integer")?,
        })
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
