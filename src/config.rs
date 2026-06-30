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
    pub thumbnail_cache_capacity: usize,
    pub oauth2_jwks_url: String,
    pub oauth2_issuer: String,
    pub auth_service_url: String,
    /// Canonical public URL of this server (e.g. `https://comad-tip.stage.ullav.setanta.dev` for staging).
    /// Used to build absolute URIs in IIIF manifests. Required for IIIF to work;
    /// defaults to `http://localhost:8080` so local dev produces valid (local) manifests.
    pub public_base_url: String,
    /// Canonical public URI of the DAM MCP endpoint — used as the OAuth2 audience.
    /// Defaults to `http://localhost:8080/mcp`.
    pub dam_mcp_canonical_uri: String,
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
            thumbnail_cache_capacity: std::env::var("THUMBNAIL_CACHE_CAPACITY")
                .unwrap_or_else(|_| "512".into())
                .parse()
                .context("THUMBNAIL_CACHE_CAPACITY must be a positive integer")?,
            oauth2_jwks_url: std::env::var("OAUTH2_JWKS_URL")
                .unwrap_or_else(|_| "http://localhost:8081/oauth2/jwks".into()),
            oauth2_issuer: std::env::var("OAUTH2_ISSUER")
                .unwrap_or_else(|_| "http://localhost:8081".into()),
            auth_service_url: std::env::var("AUTH_SERVICE_URL")
                .unwrap_or_else(|_| "http://localhost:8081".into()),
            public_base_url: std::env::var("PUBLIC_BASE_URL")
                .or_else(|_| std::env::var("PUBLIC_BASE_URL_FILE")
                    .ok()
                    .and_then(|f| std::fs::read_to_string(f).ok())
                    .map(|s| s.trim().to_string())
                    .ok_or_else(|| std::env::VarError::NotPresent))
                .unwrap_or_else(|_| "http://localhost:8080".into()),
            dam_mcp_canonical_uri: std::env::var("DAM_MCP_CANONICAL_URI")
                .unwrap_or_else(|_| "http://localhost:8080/mcp".into()),
        })
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
