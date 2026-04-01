use anyhow::Context;
use redis::aio::ConnectionManager;

pub async fn connect_test_redis() -> ConnectionManager {
    let redis_url = std::env::var("AHAND_HUB_TEST_REDIS_URL")
        .expect("AHAND_HUB_TEST_REDIS_URL must be set by TestStack");

    connect_redis(&redis_url)
        .await
        .expect("test redis should connect")
}

pub(crate) async fn connect_redis(redis_url: &str) -> anyhow::Result<ConnectionManager> {
    let client = redis::Client::open(redis_url).context("create redis client")?;
    client
        .get_connection_manager()
        .await
        .context("create redis connection manager")
}
