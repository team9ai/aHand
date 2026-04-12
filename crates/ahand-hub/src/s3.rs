//! S3 client wrapper used by the file operations transfer path.
//!
//! The hub owns S3 credentials — the daemon never holds them. For large file
//! reads, the hub uploads the daemon's in-memory response bytes to S3 and
//! returns a pre-signed download URL to the client. For large writes, the hub
//! exposes `POST /api/devices/{device_id}/files/upload-url` which returns a
//! pre-signed PUT URL and an `object_key`; the client uploads directly, then
//! re-sends the FileRequest with `s3_object_key` populated. The hub fetches
//! the bytes from S3 and forwards them inside a regular FileRequest envelope.

use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;

use crate::config::S3Config;

pub struct S3Client {
    client: aws_sdk_s3::Client,
    bucket: String,
    expiration: Duration,
    threshold: u64,
}

#[derive(Debug, Clone)]
pub struct PresignedUrl {
    pub url: String,
    pub expires_at_ms: u64,
    pub object_key: String,
}

impl S3Client {
    pub async fn new(config: &S3Config) -> Self {
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()));
        if let Some(endpoint) = &config.endpoint {
            loader = loader.endpoint_url(endpoint);
        }
        let shared = loader.load().await;
        let mut s3_builder = aws_sdk_s3::config::Builder::from(&shared);
        if config.endpoint.is_some() {
            s3_builder = s3_builder.force_path_style(true);
        }
        Self {
            client: aws_sdk_s3::Client::from_conf(s3_builder.build()),
            bucket: config.bucket.clone(),
            expiration: Duration::from_secs(config.url_expiration_secs),
            threshold: config.file_transfer_threshold_bytes,
        }
    }

    pub fn threshold(&self) -> u64 {
        self.threshold
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Generate a pre-signed PUT URL for the given object key.
    pub async fn generate_upload_url(&self, key: &str) -> anyhow::Result<PresignedUrl> {
        let presigning =
            PresigningConfig::expires_in(self.expiration).map_err(anyhow::Error::from)?;
        let req = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presigning)
            .await
            .map_err(anyhow::Error::from)?;
        Ok(PresignedUrl {
            url: req.uri().to_string(),
            expires_at_ms: expires_at_ms(self.expiration),
            object_key: key.to_string(),
        })
    }

    /// Generate a pre-signed GET URL for the given object key.
    pub async fn generate_download_url(&self, key: &str) -> anyhow::Result<PresignedUrl> {
        let presigning =
            PresigningConfig::expires_in(self.expiration).map_err(anyhow::Error::from)?;
        let req = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presigning)
            .await
            .map_err(anyhow::Error::from)?;
        Ok(PresignedUrl {
            url: req.uri().to_string(),
            expires_at_ms: expires_at_ms(self.expiration),
            object_key: key.to_string(),
        })
    }

    /// Upload raw bytes to the given key.
    pub async fn upload_bytes(&self, key: &str, data: Vec<u8>) -> anyhow::Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(anyhow::Error::from)?;
        Ok(())
    }

    /// Download raw bytes for the given key.
    pub async fn download_bytes(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(anyhow::Error::from)?;
        let bytes = resp
            .body
            .collect()
            .await
            .map_err(anyhow::Error::from)?
            .into_bytes()
            .to_vec();
        Ok(bytes)
    }
}

fn expires_at_ms(expiration: Duration) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    now + expiration.as_millis() as u64
}
