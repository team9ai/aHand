use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

pub struct TestStack {
    pub devices: crate::device_store::PgDeviceStore,
    pub jobs: crate::job_store::PgJobStore,
    pub audit: crate::audit_store::PgAuditStore,
    pub presence: crate::presence_store::RedisPresenceStore,
    postgres_container_id: String,
    redis_container_id: String,
}

impl TestStack {
    pub async fn start() -> anyhow::Result<Self> {
        let postgres_container_id = docker([
            "run",
            "-d",
            "-e",
            "POSTGRES_USER=postgres",
            "-e",
            "POSTGRES_PASSWORD=postgres",
            "-e",
            "POSTGRES_DB=ahand_hub_test",
            "-p",
            "0:5432",
            "postgres:17-alpine",
        ])
        .context("start postgres test container (docker daemon required)")?;
        wait_for_container_log(
            &postgres_container_id,
            "database system is ready to accept connections",
        )
        .context("wait for postgres readiness")?;
        let postgres_port = docker_host_port(&postgres_container_id, "5432/tcp")
            .context("resolve postgres port")?;
        let database_url =
            format!("postgres://postgres:postgres@127.0.0.1:{postgres_port}/ahand_hub_test");
        let postgres_pool = crate::postgres::connect_database(&database_url).await?;

        let redis_container_id = docker(["run", "-d", "-p", "0:6379", "redis:7-alpine"])
            .context("start redis test container (docker daemon required)")?;
        wait_for_container_log(&redis_container_id, "Ready to accept connections")
            .context("wait for redis readiness")?;
        let redis_port =
            docker_host_port(&redis_container_id, "6379/tcp").context("resolve redis port")?;
        let redis_url = format!("redis://127.0.0.1:{redis_port}");
        let redis_connection = crate::redis::connect_redis(&redis_url).await?;
        let presence = crate::presence_store::RedisPresenceStore::new(redis_connection);

        Ok(Self {
            devices: crate::device_store::PgDeviceStore::with_presence(
                postgres_pool.clone(),
                presence.clone(),
            ),
            jobs: crate::job_store::PgJobStore::new(postgres_pool.clone()),
            audit: crate::audit_store::PgAuditStore::new(postgres_pool),
            presence,
            postgres_container_id,
            redis_container_id,
        })
    }
}

impl Drop for TestStack {
    fn drop(&mut self) {
        let _ = docker([
            "rm",
            "-f",
            &self.postgres_container_id,
            &self.redis_container_id,
        ]);
    }
}

fn docker<const N: usize>(args: [&str; N]) -> anyhow::Result<String> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .context("run docker command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(stderr.trim().to_owned());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn wait_for_container_log(container_id: &str, needle: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);

    while Instant::now() < deadline {
        let output = Command::new("docker")
            .args(["logs", container_id])
            .output()
            .context("read container logs")?;

        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if combined.contains(needle) {
            return Ok(());
        }

        std::thread::sleep(Duration::from_millis(250));
    }

    bail!("timed out waiting for log line: {needle}");
}

fn docker_host_port(container_id: &str, port: &str) -> anyhow::Result<u16> {
    let mapping = docker(["port", container_id, port])?;
    let port = mapping
        .trim()
        .rsplit(':')
        .next()
        .context("parse docker port output")?;
    port.parse::<u16>().context("parse host port as u16")
}
