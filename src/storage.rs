use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    config::{Builder, Region},
    primitives::ByteStream,
    Client,
};
use bytes::Bytes;

use crate::config::Config;
use crate::error::AppError;

#[derive(Clone, Debug)]
pub struct StorageClient {
    pub client: Client,
    pub bucket: String,
}

impl StorageClient {
    pub async fn new(cfg: &Config) -> Result<Self> {
        let credentials = Credentials::new(
            &cfg.s3_access_key_id,
            &cfg.s3_secret_access_key,
            None,
            None,
            "dam-static",
        );

        let s3_config = Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .endpoint_url(&cfg.s3_endpoint)
            .region(Region::new(cfg.s3_region.clone()))
            .credentials_provider(credentials)
            .force_path_style(true) // required for MinIO
            .build();

        let client = Client::from_conf(s3_config);

        // Ensure the bucket exists
        let storage = Self {
            client,
            bucket: cfg.s3_bucket.clone(),
        };
        storage.ensure_bucket().await?;

        Ok(storage)
    }

    async fn ensure_bucket(&self) -> Result<()> {
        let exists = self
            .client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .is_ok();

        if !exists {
            self.client
                .create_bucket()
                .bucket(&self.bucket)
                .send()
                .await?;
            tracing::info!("Created S3 bucket: {}", self.bucket);
        }

        Ok(())
    }

    pub async fn upload(
        &self,
        key: &str,
        data: Bytes,
        content_type: &str,
    ) -> Result<(), AppError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(|e| AppError::Storage(e.to_string()))?;

        Ok(())
    }

    pub async fn download(&self, key: &str) -> Result<Bytes, AppError> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| AppError::Storage(e.to_string()))?;

        let data = resp
            .body
            .collect()
            .await
            .map_err(|e| AppError::Storage(e.to_string()))?
            .into_bytes();

        Ok(data)
    }

    /// Download an object, returning `None` if the key does not exist (404).
    /// All other S3 errors are returned as `Err`.
    pub async fn try_download(&self, key: &str) -> Result<Option<Bytes>, AppError> {
        use aws_sdk_s3::operation::get_object::GetObjectError;

        match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(resp) => {
                let data = resp
                    .body
                    .collect()
                    .await
                    .map_err(|e| AppError::Storage(e.to_string()))?
                    .into_bytes();
                Ok(Some(data))
            }
            Err(e) => {
                if matches!(e.as_service_error(), Some(GetObjectError::NoSuchKey(_))) {
                    Ok(None)
                } else {
                    Err(AppError::Storage(e.to_string()))
                }
            }
        }
    }

    pub async fn delete(&self, key: &str) -> Result<(), AppError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| AppError::Storage(e.to_string()))?;

        Ok(())
    }

    /// Generate a presigned URL valid for `expires_in` seconds.
    pub async fn presigned_url(&self, key: &str, expires_in: u64) -> Result<String, AppError> {
        use aws_sdk_s3::presigning::PresigningConfig;
        use std::time::Duration;

        let presigning_config = PresigningConfig::expires_in(Duration::from_secs(expires_in))
            .map_err(|e| AppError::Storage(e.to_string()))?;

        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presigning_config)
            .await
            .map_err(|e| AppError::Storage(e.to_string()))?;

        Ok(presigned.uri().to_string())
    }
}
