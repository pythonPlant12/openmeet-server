use async_trait::async_trait;
use aws_config::{BehaviorVersion, Region};
use aws_credential_types::Credentials;
use aws_sdk_s3::{Client, primitives::ByteStream};

#[async_trait]
pub trait AvatarStorage: Send + Sync {
    async fn upload(&self, key: &str, content_type: &str, bytes: Vec<u8>) -> anyhow::Result<()>;
    async fn download(&self, key: &str) -> anyhow::Result<AvatarObject>;
    async fn delete(&self, key: &str) -> anyhow::Result<()>;
}

pub struct AvatarObject {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

pub struct S3AvatarStorage {
    client: Client,
    bucket: String,
}

impl S3AvatarStorage {
    pub async fn from_env() -> anyhow::Result<Self> {
        let endpoint = required_env("RUSTFS_ENDPOINT")?;
        let access_key = required_env("RUSTFS_ACCESS_KEY")?;
        let secret_key = required_env("RUSTFS_SECRET_KEY")?;
        let bucket = required_env("RUSTFS_AVATAR_BUCKET")?;
        let region = std::env::var("RUSTFS_REGION").unwrap_or_else(|_| "us-east-1".to_string());

        let shared_config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region))
            .credentials_provider(Credentials::new(
                access_key, secret_key, None, None, "rustfs",
            ))
            .load()
            .await;
        let config = aws_sdk_s3::config::Builder::from(&shared_config)
            .endpoint_url(endpoint)
            .force_path_style(true)
            .build();

        Ok(Self {
            client: Client::from_conf(config),
            bucket,
        })
    }

    pub async fn ensure_bucket(&self) -> anyhow::Result<()> {
        match self.client.head_bucket().bucket(&self.bucket).send().await {
            Ok(_) => Ok(()),
            Err(_) => {
                self.client
                    .create_bucket()
                    .bucket(&self.bucket)
                    .send()
                    .await?;
                Ok(())
            }
        }
    }
}

#[async_trait]
impl AvatarStorage for S3AvatarStorage {
    async fn upload(&self, key: &str, content_type: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(ByteStream::from(bytes))
            .send()
            .await?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await?;
        Ok(())
    }

    async fn download(&self, key: &str) -> anyhow::Result<AvatarObject> {
        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await?;
        let content_type = object
            .content_type
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let bytes = object.body.collect().await?.into_bytes().to_vec();
        Ok(AvatarObject {
            content_type,
            bytes,
        })
    }
}

fn required_env(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name).map_err(|_| anyhow::anyhow!("{name} must be set"))?;
    if value.trim().is_empty() {
        anyhow::bail!("{name} must not be empty");
    }
    Ok(value)
}
