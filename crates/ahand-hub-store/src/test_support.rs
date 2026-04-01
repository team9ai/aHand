use anyhow::Context;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

pub struct TestStack {
    pub devices: crate::device_store::PgDeviceStore,
    pub jobs: crate::job_store::PgJobStore,
    pub audit: crate::audit_store::PgAuditStore,
    pub presence: crate::presence_store::RedisPresenceStore,
    _postgres: ContainerAsync<GenericImage>,
    _redis: ContainerAsync<GenericImage>,
}

impl TestStack {
    pub async fn start() -> anyhow::Result<Self> {
        let postgres = GenericImage::new("postgres", "17-alpine")
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
        let postgres_host = postgres.get_host().await.context("resolve postgres host")?;
        let postgres_port = postgres
            .get_host_port_ipv4(5432.tcp())
            .await
            .context("resolve postgres port")?;
        let database_url =
            format!("postgres://postgres:postgres@{postgres_host}:{postgres_port}/ahand_hub_test");
        let postgres_pool = crate::postgres::connect_database(&database_url).await?;

        let redis = GenericImage::new("redis", "7-alpine")
            .with_exposed_port(6379.tcp())
            .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
            .start()
            .await
            .context("start redis test container (docker daemon required)")?;
        let redis_host = redis.get_host().await.context("resolve redis host")?;
        let redis_port = redis
            .get_host_port_ipv4(6379.tcp())
            .await
            .context("resolve redis port")?;
        let redis_url = format!("redis://{redis_host}:{redis_port}");
        let redis_connection = crate::redis::connect_redis(&redis_url).await?;

        Ok(Self {
            devices: crate::device_store::PgDeviceStore::new(postgres_pool.clone()),
            jobs: crate::job_store::PgJobStore::new(postgres_pool.clone()),
            audit: crate::audit_store::PgAuditStore::new(postgres_pool),
            presence: crate::presence_store::RedisPresenceStore::new(redis_connection),
            _postgres: postgres,
            _redis: redis,
        })
    }
}
