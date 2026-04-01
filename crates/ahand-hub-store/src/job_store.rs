use ahand_hub_core::job::{Job, JobFilter, JobStatus, NewJob};
use ahand_hub_core::traits::JobStore;
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgJobStore {
    pool: PgPool,
}

impl PgJobStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JobStore for PgJobStore {
    async fn insert(&self, job: NewJob) -> Result<Job> {
        let job_id = Uuid::new_v4();
        let status = encode_status(JobStatus::Pending);
        let env =
            serde_json::to_value(&job.env).map_err(|err| HubError::Internal(err.to_string()))?;
        let timeout_ms =
            i64::try_from(job.timeout_ms).map_err(|err| HubError::Internal(err.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO jobs (id, device_id, tool, args, cwd, env, timeout_ms, status, requested_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(job_id)
        .bind(&job.device_id)
        .bind(&job.tool)
        .bind(&job.args)
        .bind(&job.cwd)
        .bind(env)
        .bind(timeout_ms)
        .bind(status)
        .bind(&job.requested_by)
        .execute(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        self.get(&job_id.to_string())
            .await?
            .ok_or_else(|| HubError::Internal(format!("inserted job missing: {job_id}")))
    }

    async fn get(&self, job_id: &str) -> Result<Option<Job>> {
        let parsed =
            Uuid::parse_str(job_id).map_err(|err| HubError::InvalidToken(err.to_string()))?;
        let row = sqlx::query(
            r#"
            SELECT id, device_id, tool, args, cwd, env, timeout_ms, status, requested_by
            FROM jobs
            WHERE id = $1
            "#,
        )
        .bind(parsed)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        row.map(map_job).transpose()
    }

    async fn list(&self, filter: JobFilter) -> Result<Vec<Job>> {
        let status = filter.status.map(encode_status);
        let rows = sqlx::query(
            r#"
            SELECT id, device_id, tool, args, cwd, env, timeout_ms, status, requested_by
            FROM jobs
            WHERE ($1::text IS NULL OR device_id = $1)
              AND ($2::text IS NULL OR status = $2)
            ORDER BY id
            "#,
        )
        .bind(filter.device_id)
        .bind(status)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| HubError::Internal(err.to_string()))?;

        rows.into_iter().map(map_job).collect()
    }

    async fn update_status(&self, job_id: &str, status: JobStatus) -> Result<()> {
        let parsed =
            Uuid::parse_str(job_id).map_err(|err| HubError::InvalidToken(err.to_string()))?;
        let result = sqlx::query("UPDATE jobs SET status = $2 WHERE id = $1")
            .bind(parsed)
            .bind(encode_status(status))
            .execute(&self.pool)
            .await
            .map_err(|err| HubError::Internal(err.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(HubError::JobNotFound(job_id.into()));
        }

        Ok(())
    }
}

fn map_job(row: sqlx::postgres::PgRow) -> Result<Job> {
    let env = row
        .try_get::<serde_json::Value, _>("env")
        .map_err(|err| HubError::Internal(err.to_string()))?;
    let status = row
        .try_get::<String, _>("status")
        .map_err(|err| HubError::Internal(err.to_string()))?;

    Ok(Job {
        id: row
            .try_get("id")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        device_id: row
            .try_get("device_id")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        tool: row
            .try_get("tool")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        args: row
            .try_get("args")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        cwd: row
            .try_get("cwd")
            .map_err(|err| HubError::Internal(err.to_string()))?,
        env: serde_json::from_value(env).map_err(|err| HubError::Internal(err.to_string()))?,
        timeout_ms: u64::try_from(
            row.try_get::<i64, _>("timeout_ms")
                .map_err(|err| HubError::Internal(err.to_string()))?,
        )
        .map_err(|err| HubError::Internal(err.to_string()))?,
        status: decode_status(&status)?,
        requested_by: row
            .try_get("requested_by")
            .map_err(|err| HubError::Internal(err.to_string()))?,
    })
}

fn encode_status(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Pending => "pending",
        JobStatus::Sent => "sent",
        JobStatus::Running => "running",
        JobStatus::Finished => "finished",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

fn decode_status(status: &str) -> Result<JobStatus> {
    match status {
        "pending" => Ok(JobStatus::Pending),
        "sent" => Ok(JobStatus::Sent),
        "running" => Ok(JobStatus::Running),
        "finished" => Ok(JobStatus::Finished),
        "failed" => Ok(JobStatus::Failed),
        "cancelled" => Ok(JobStatus::Cancelled),
        other => Err(HubError::Internal(format!("unknown job status: {other}"))),
    }
}
