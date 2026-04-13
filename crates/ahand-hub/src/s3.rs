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

/// Test-only constructor that builds an `S3Client` from an already-configured
/// `aws_sdk_s3::Client`. This lets tests inject explicit credentials via the
/// SDK config builder instead of mutating process-wide environment variables,
/// which is unsound under cargo's parallel test runner.
#[cfg(test)]
impl S3Client {
    pub(crate) fn for_test(
        client: aws_sdk_s3::Client,
        bucket: &str,
        threshold: u64,
        expiration_secs: u64,
    ) -> Self {
        Self {
            client,
            bucket: bucket.into(),
            expiration: Duration::from_secs(expiration_secs),
            threshold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::S3Config;
    use aws_credential_types::Credentials;

    fn fake_endpoint_config() -> S3Config {
        S3Config {
            bucket: "test-bucket".into(),
            region: "us-east-1".into(),
            endpoint: Some("http://127.0.0.1:1".into()),
            file_transfer_threshold_bytes: 1_048_576,
            url_expiration_secs: 3_600,
        }
    }

    /// Build an `aws_sdk_s3::Client` wired with explicit synthetic credentials
    /// and the given endpoint. The presigner is a pure local HMAC-SHA256
    /// computation, so the credentials only need to exist — they do not need
    /// to be real. Crucially, this path never touches process env, so tests
    /// are safe to run in parallel.
    fn sdk_client_with_explicit_creds(endpoint: &str, region: &str) -> aws_sdk_s3::Client {
        let creds = Credentials::new("test", "test", None, None, "ahand-hub-s3-tests");
        let s3_config = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .credentials_provider(creds)
            .region(Region::new(region.to_string()))
            .endpoint_url(endpoint)
            .force_path_style(true)
            .build();
        aws_sdk_s3::Client::from_conf(s3_config)
    }

    fn build_test_s3_client(config: &S3Config) -> S3Client {
        let endpoint = config.endpoint.as_ref().expect("test config has endpoint");
        let aws_client = sdk_client_with_explicit_creds(endpoint, &config.region);
        S3Client::for_test(
            aws_client,
            &config.bucket,
            config.file_transfer_threshold_bytes,
            config.url_expiration_secs,
        )
    }

    #[test]
    fn s3_config_default_values() {
        let config = S3Config::default();
        assert!(config.bucket.is_empty());
        assert_eq!(config.region, "us-east-1");
        assert!(config.endpoint.is_none());
        assert_eq!(config.file_transfer_threshold_bytes, 1_048_576);
        assert_eq!(config.url_expiration_secs, 3_600);
    }

    #[tokio::test]
    async fn s3_client_constructs_with_fake_endpoint() {
        let config = fake_endpoint_config();
        let client = build_test_s3_client(&config);
        assert_eq!(client.bucket(), "test-bucket");
        assert_eq!(client.threshold(), 1_048_576);
    }

    #[tokio::test]
    async fn s3_client_generate_upload_url_returns_populated_url() {
        let config = fake_endpoint_config();
        let client = build_test_s3_client(&config);

        let key = "file-ops/device-1/obj-key";
        let result = client.generate_upload_url(key).await;
        assert!(
            result.is_ok(),
            "generate_upload_url should succeed locally: {:?}",
            result.err()
        );
        let presigned = result.unwrap();
        assert!(
            presigned.url.starts_with("http://127.0.0.1:1/"),
            "url should start with the forced endpoint, got: {}",
            presigned.url
        );
        assert!(
            presigned.url.contains("test-bucket"),
            "url should contain the bucket in path-style form, got: {}",
            presigned.url
        );
        assert!(
            presigned.url.contains("file-ops/device-1/obj-key"),
            "url should contain the object key, got: {}",
            presigned.url
        );
        assert!(presigned.expires_at_ms > 0);
        assert_eq!(presigned.object_key, key);
    }

    #[tokio::test]
    async fn s3_client_generate_download_url_returns_populated_url() {
        let config = fake_endpoint_config();
        let client = build_test_s3_client(&config);

        let key = "file-ops/device-1/obj-key";
        let result = client.generate_download_url(key).await;
        assert!(
            result.is_ok(),
            "generate_download_url should succeed locally: {:?}",
            result.err()
        );
        let presigned = result.unwrap();
        assert!(
            presigned.url.starts_with("http://127.0.0.1:1/"),
            "url should start with the forced endpoint, got: {}",
            presigned.url
        );
        assert!(
            presigned.url.contains("test-bucket"),
            "url should contain the bucket in path-style form, got: {}",
            presigned.url
        );
        assert!(
            presigned.url.contains("file-ops/device-1/obj-key"),
            "url should contain the object key, got: {}",
            presigned.url
        );
        assert!(presigned.expires_at_ms > 0);
        assert_eq!(presigned.object_key, key);
    }
}
