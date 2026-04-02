use anyhow::Context;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

type ManagedContainer = ContainerAsync<GenericImage>;

pub struct TestStack {
    pub devices: ahand_hub_store::device_store::PgDeviceStore,
    pub jobs: ahand_hub_store::job_store::PgJobStore,
    pub audit: ahand_hub_store::audit_store::PgAuditStore,
    pub presence: ahand_hub_store::presence_store::RedisPresenceStore,
    database_url: String,
    redis_url: String,
    _postgres: ManagedContainer,
    _redis: ManagedContainer,
}

impl TestStack {
    pub async fn start() -> anyhow::Result<Self> {
        let postgres = GenericImage::new("postgres", "16-alpine")
            .with_exposed_port(5432.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_DB", "ahand_hub_test")
            .start()
            .await
            .context("start postgres test container (docker daemon required)")?;
        let postgres_port = postgres
            .get_host_port_ipv4(5432.tcp())
            .await
            .context("resolve postgres port")?;
        let database_url =
            format!("postgres://postgres:postgres@127.0.0.1:{postgres_port}/ahand_hub_test");
        let postgres_pool = ahand_hub_store::postgres::connect_database(&database_url).await?;

        let redis = GenericImage::new("redis", "7-alpine")
            .with_exposed_port(6379.tcp())
            .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
            .start()
            .await
            .context("start redis test container (docker daemon required)")?;
        let redis_port = redis
            .get_host_port_ipv4(6379.tcp())
            .await
            .context("resolve redis port")?;
        let redis_url = format!("redis://127.0.0.1:{redis_port}");
        let redis_connection = ahand_hub_store::redis::connect_redis(&redis_url).await?;
        let presence = ahand_hub_store::presence_store::RedisPresenceStore::new(redis_connection);

        Ok(Self {
            devices: ahand_hub_store::device_store::PgDeviceStore::with_presence(
                postgres_pool.clone(),
                presence.clone(),
            ),
            jobs: ahand_hub_store::job_store::PgJobStore::new(postgres_pool.clone()),
            audit: ahand_hub_store::audit_store::PgAuditStore::new(postgres_pool),
            presence,
            database_url,
            redis_url,
            _postgres: postgres,
            _redis: redis,
        })
    }

    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    pub fn redis_url(&self) -> &str {
        &self.redis_url
    }
}
