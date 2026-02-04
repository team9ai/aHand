use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use ahand_protocol::{Envelope, JobRequest};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::warn;

/// Direction of an envelope (for trace logging).
#[derive(Clone, Copy)]
pub enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::Inbound => "in",
            Direction::Outbound => "out",
        }
    }
}

/// Persists trace logs and per-job run artifacts to disk.
pub struct RunStore {
    data_dir: PathBuf,
    trace_file: Mutex<BufWriter<File>>,
}

impl RunStore {
    /// Create or open the store at the given directory.
    pub fn new(data_dir: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(data_dir)?;
        fs::create_dir_all(data_dir.join("runs"))?;

        let trace_path = data_dir.join("trace.jsonl");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_path)?;

        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            trace_file: Mutex::new(BufWriter::new(file)),
        })
    }

    /// Append an envelope record to trace.jsonl.
    pub async fn log_envelope(&self, envelope: &Envelope, direction: Direction) {
        let payload_type = describe_payload(envelope);
        let record = json!({
            "ts_ms": envelope.ts_ms,
            "direction": direction.as_str(),
            "device_id": envelope.device_id,
            "msg_id": envelope.msg_id,
            "seq": envelope.seq,
            "ack": envelope.ack,
            "payload": payload_type,
        });

        let mut file = self.trace_file.lock().await;
        if let Err(e) = writeln!(file, "{}", record) {
            warn!(error = %e, "failed to write trace");
        }
        let _ = file.flush();
    }

    /// Create the run directory and write request.json.
    pub fn start_run(&self, job_id: &str, req: &JobRequest) {
        let run_dir = self.data_dir.join("runs").join(job_id);
        if let Err(e) = fs::create_dir_all(&run_dir) {
            warn!(job_id = %job_id, error = %e, "failed to create run dir");
            return;
        }

        let request = json!({
            "job_id": req.job_id,
            "tool": req.tool,
            "args": req.args,
            "cwd": req.cwd,
            "env": req.env,
            "timeout_ms": req.timeout_ms,
            "start_ms": now_ms(),
        });

        if let Err(e) = write_json(&run_dir.join("request.json"), &request) {
            warn!(job_id = %job_id, error = %e, "failed to write request.json");
        }
    }

    /// Append a chunk to the stdout file for a run.
    pub fn append_stdout(&self, job_id: &str, chunk: &[u8]) {
        self.append_to_file(job_id, "stdout", chunk);
    }

    /// Append a chunk to the stderr file for a run.
    pub fn append_stderr(&self, job_id: &str, chunk: &[u8]) {
        self.append_to_file(job_id, "stderr", chunk);
    }

    /// Write the final result.json for a completed run.
    pub fn finish_run(&self, job_id: &str, exit_code: i32, error: &str) {
        let run_dir = self.data_dir.join("runs").join(job_id);
        let result = json!({
            "job_id": job_id,
            "exit_code": exit_code,
            "error": error,
            "end_ms": now_ms(),
        });

        if let Err(e) = write_json(&run_dir.join("result.json"), &result) {
            warn!(job_id = %job_id, error = %e, "failed to write result.json");
        }
    }

    fn append_to_file(&self, job_id: &str, name: &str, chunk: &[u8]) {
        let path = self.data_dir.join("runs").join(job_id).join(name);
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(chunk));

        if let Err(e) = result {
            warn!(job_id = %job_id, file = name, error = %e, "failed to append");
        }
    }
}

fn describe_payload(envelope: &Envelope) -> &'static str {
    use ahand_protocol::envelope::Payload;
    match &envelope.payload {
        Some(Payload::Hello(_)) => "Hello",
        Some(Payload::JobRequest(_)) => "JobRequest",
        Some(Payload::JobEvent(_)) => "JobEvent",
        Some(Payload::JobFinished(_)) => "JobFinished",
        Some(Payload::JobRejected(_)) => "JobRejected",
        Some(Payload::CancelJob(_)) => "CancelJob",
        Some(Payload::ApprovalRequest(_)) => "ApprovalRequest",
        Some(Payload::ApprovalResponse(_)) => "ApprovalResponse",
        Some(Payload::PolicyQuery(_)) => "PolicyQuery",
        Some(Payload::PolicyState(_)) => "PolicyState",
        Some(Payload::PolicyUpdate(_)) => "PolicyUpdate",
        Some(Payload::SetSessionMode(_)) => "SetSessionMode",
        Some(Payload::SessionState(_)) => "SessionState",
        Some(Payload::SessionQuery(_)) => "SessionQuery",
        None => "none",
    }
}

fn write_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, value)?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
