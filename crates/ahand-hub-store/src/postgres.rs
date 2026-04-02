use anyhow::Context;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::time::{Duration, sleep};

#[cfg(any(test, feature = "test-support"))]
pub async fn connect_test_database() -> PgPool {
    let database_url = std::env::var("AHAND_HUB_TEST_DATABASE_URL")
        .expect("AHAND_HUB_TEST_DATABASE_URL must be set by TestStack");

    connect_database(&database_url)
        .await
        .expect("test database should connect and migrate")
}

pub async fn connect_database(database_url: &str) -> anyhow::Result<PgPool> {
    let mut last_error = None;
    let mut pool = None;

    for _ in 0..40 {
        match PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
        {
            Ok(connected) => {
                pool = Some(connected);
                break;
            }
            Err(err) => {
                last_error = Some(err);
                sleep(Duration::from_millis(250)).await;
            }
        }
    }

    let pool = pool.with_context(|| {
        format!(
            "connect postgres at {database_url}: {}",
            last_error
                .as_ref()
                .map(std::string::ToString::to_string)
                .unwrap_or_else(|| "unknown error".into())
        )
    })?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run ahand-hub-store migrations")?;

    Ok(pool)
}
