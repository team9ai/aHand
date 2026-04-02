use std::sync::Arc;
use std::time::Duration;

use ahand_hub_core::{HubError, Result};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use redis::streams::{StreamRangeReply, StreamReadOptions, StreamReadReply};
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutputRecord {
    Stdout(String),
    Stderr(String),
    Progress(u32),
    Finished { exit_code: i32, error: String },
}

impl JobOutputRecord {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Stdout(_) => "stdout",
            Self::Stderr(_) => "stderr",
            Self::Progress(_) => "progress",
            Self::Finished { .. } => "finished",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredJobOutputRecord {
    pub stream_id: String,
    pub seq: u64,
    pub record: JobOutputRecord,
}

#[derive(Clone)]
pub struct RedisJobOutputStore {
    connection: Arc<Mutex<ConnectionManager>>,
    retention: Duration,
}

impl RedisJobOutputStore {
    pub fn new(connection: ConnectionManager, retention: Duration) -> Self {
        Self {
            connection: Arc::new(Mutex::new(connection)),
            retention,
        }
    }

    pub async fn append(
        &self,
        job_id: &str,
        record: JobOutputRecord,
    ) -> Result<StoredJobOutputRecord> {
        let stream_key = output_stream_key(job_id);
        let seq_key = output_seq_key(job_id);
        let mut connection = self.connection.lock().await;
        let seq: u64 = connection
            .incr(&seq_key, 1)
            .await
            .map_err(redis_err)?;

        let stream_id: String = match &record {
            JobOutputRecord::Stdout(chunk) | JobOutputRecord::Stderr(chunk) => {
                connection
                    .xadd(
                        &stream_key,
                        "*",
                        &[("seq", seq.to_string()), ("type", record.kind().into()), ("data", chunk.clone())],
                    )
                    .await
                    .map_err(redis_err)?
            }
            JobOutputRecord::Progress(progress) => {
                connection
                    .xadd(
                        &stream_key,
                        "*",
                        &[("seq", seq.to_string()), ("type", record.kind().into()), ("data", progress.to_string())],
                    )
                    .await
                    .map_err(redis_err)?
            }
            JobOutputRecord::Finished { exit_code, error } => {
                connection
                    .xadd(
                        &stream_key,
                        "*",
                        &[
                            ("seq", seq.to_string()),
                            ("type", record.kind().into()),
                            ("exit_code", exit_code.to_string()),
                            ("error", error.clone()),
                        ],
                    )
                    .await
                    .map_err(redis_err)?
            }
        };

        if record.is_terminal() {
            let ttl_ms = self.retention.as_millis().min(u64::MAX as u128) as u64;
            let _: bool = connection
                .pexpire(&stream_key, ttl_ms as i64)
                .await
                .map_err(redis_err)?;
            let _: bool = connection
                .pexpire(&seq_key, ttl_ms as i64)
                .await
                .map_err(redis_err)?;
        }

        Ok(StoredJobOutputRecord {
            stream_id,
            seq,
            record,
        })
    }

    pub async fn read_history(&self, job_id: &str) -> Result<Vec<StoredJobOutputRecord>> {
        let mut connection = self.connection.lock().await;
        let reply: StreamRangeReply = connection
            .xrange_all(output_stream_key(job_id))
            .await
            .map_err(redis_err)?;
        reply
            .ids
            .into_iter()
            .map(parse_stream_record)
            .collect::<Result<Vec<_>>>()
    }

    pub async fn read_live(
        &self,
        job_id: &str,
        last_stream_id: &str,
        block_ms: usize,
    ) -> Result<Vec<StoredJobOutputRecord>> {
        let mut connection = self.connection.lock().await;
        let reply: Option<StreamReadReply> = connection
            .xread_options(
                &[output_stream_key(job_id)],
                &[last_stream_id],
                &StreamReadOptions::default().block(block_ms).count(128),
            )
            .await
            .map_err(redis_err)?;

        let Some(reply) = reply else {
            return Ok(Vec::new());
        };

        reply
            .keys
            .into_iter()
            .flat_map(|stream| stream.ids.into_iter())
            .map(parse_stream_record)
            .collect::<Result<Vec<_>>>()
    }
}

fn parse_stream_record(record: redis::streams::StreamId) -> Result<StoredJobOutputRecord> {
    let seq = record
        .get::<u64>("seq")
        .ok_or_else(|| HubError::Internal("output stream record missing seq".into()))?;
    let kind = record
        .get::<String>("type")
        .ok_or_else(|| HubError::Internal("output stream record missing type".into()))?;
    let parsed = match kind.as_str() {
        "stdout" => JobOutputRecord::Stdout(record.get("data").unwrap_or_default()),
        "stderr" => JobOutputRecord::Stderr(record.get("data").unwrap_or_default()),
        "progress" => JobOutputRecord::Progress(record.get("data").unwrap_or_default()),
        "finished" => JobOutputRecord::Finished {
            exit_code: record.get("exit_code").unwrap_or_default(),
            error: record.get("error").unwrap_or_default(),
        },
        other => {
            return Err(HubError::Internal(format!(
                "unknown output stream record type: {other}"
            )))
        }
    };

    Ok(StoredJobOutputRecord {
        stream_id: record.id,
        seq,
        record: parsed,
    })
}

fn output_stream_key(job_id: &str) -> String {
    format!("ahand:job:{job_id}:output")
}

fn output_seq_key(job_id: &str) -> String {
    format!("ahand:job:{job_id}:output:seq")
}

fn redis_err(err: redis::RedisError) -> HubError {
    HubError::Internal(err.to_string())
}
