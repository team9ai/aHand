use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

pub struct TestStack {
    pub devices: crate::device_store::PgDeviceStore,
    pub jobs: crate::job_store::PgJobStore,
    pub audit: crate::audit_store::PgAuditStore,
    pub presence: crate::presence_store::RedisPresenceStore,
    database_url: String,
    redis_url: String,
    postgres_container_id: String,
    redis_container_id: String,
}

impl TestStack {
    pub async fn start() -> anyhow::Result<Self> {
        let mut cleanup = ContainerCleanup::default();
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
            "postgres:16-alpine",
        ])
        .context("start postgres test container (docker daemon required)")?;
        cleanup.track(postgres_container_id.clone());
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
        cleanup.track(redis_container_id.clone());
        wait_for_container_log(&redis_container_id, "Ready to accept connections")
            .context("wait for redis readiness")?;
        let redis_port =
            docker_host_port(&redis_container_id, "6379/tcp").context("resolve redis port")?;
        let redis_url = format!("redis://127.0.0.1:{redis_port}");
        let redis_connection = crate::redis::connect_redis(&redis_url).await?;
        let presence = crate::presence_store::RedisPresenceStore::new(redis_connection);

        cleanup.disarm();
        Ok(Self {
            devices: crate::device_store::PgDeviceStore::with_presence(
                postgres_pool.clone(),
                presence.clone(),
            ),
            jobs: crate::job_store::PgJobStore::new(postgres_pool.clone()),
            audit: crate::audit_store::PgAuditStore::new(postgres_pool),
            presence,
            database_url,
            redis_url,
            postgres_container_id,
            redis_container_id,
        })
    }

    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    pub fn redis_url(&self) -> &str {
        &self.redis_url
    }
}

#[derive(Default)]
struct ContainerCleanup {
    container_ids: Vec<String>,
    armed: bool,
}

impl ContainerCleanup {
    fn track(&mut self, container_id: String) {
        self.armed = true;
        self.container_ids.push(container_id);
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        if !self.armed || self.container_ids.is_empty() {
            return;
        }

        let mut args = vec!["rm", "-f"];
        for container_id in &self.container_ids {
            args.push(container_id.as_str());
        }
        let _ = Command::new("docker").args(&args).output();
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
