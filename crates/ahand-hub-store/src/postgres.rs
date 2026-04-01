use anyhow::Context;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub async fn connect_test_database() -> PgPool {
    let database_url = std::env::var("AHAND_HUB_TEST_DATABASE_URL")
        .expect("AHAND_HUB_TEST_DATABASE_URL must be set by TestStack");

    connect_database(&database_url)
        .await
        .expect("test database should connect and migrate")
}

pub(crate) async fn connect_database(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .with_context(|| format!("connect postgres at {database_url}"))?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run ahand-hub-store migrations")?;

    Ok(pool)
}
