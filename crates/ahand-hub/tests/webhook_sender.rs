//! End-to-end tests for the outbound webhook sender (Task 1.5).
//!
//! Each test spins up a lightweight axum mock gateway that behaves the
//! way the real team9 gateway should: verifies the `X-AHand-Signature`
//! header, records received payloads, and returns whatever status the
//! test asks for. The hub side uses a [`MemoryWebhookDeliveryStore`]
//! and a real [`Webhook`] worker wired with a reqwest client that
//! points at the mock's listener.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use ahand_hub::webhook::sender::{backoff_secs, sign, verify};
use ahand_hub::webhook::worker::WorkerHandle;
use ahand_hub::webhook::{Webhook, WebhookConfig, WebhookPayload};
use ahand_hub_store::webhook_delivery_store::{MemoryWebhookDeliveryStore, WebhookDeliveryStore};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

struct MockGateway {
    url: String,
    received: Arc<Mutex<Vec<ReceivedRequest>>>,
    verify_calls: Arc<AtomicU32>,
    task: JoinHandle<()>,
}

#[derive(Debug, Clone)]
struct ReceivedRequest {
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Clone)]
struct MockState {
    received: Arc<Mutex<Vec<ReceivedRequest>>>,
    secret: Arc<Vec<u8>>,
    verify_calls: Arc<AtomicU32>,
    mode: Arc<MockMode>,
}

enum MockMode {
    /// Record and return 2xx. Returns 401 on signature mismatch
    /// so the sender's "permanent 401" path is also exercised by
    /// tests that deliberately sign with the wrong secret.
    Ok,
    /// Always return the same status. Used for retry and
    /// exhaustion tests.
    AlwaysFail { status: StatusCode },
}

impl MockGateway {
    async fn start(mode: MockMode, secret: Vec<u8>) -> Self {
        let received = Arc::new(Mutex::new(Vec::new()));
        let verify_calls = Arc::new(AtomicU32::new(0));
        let state = MockState {
            received: received.clone(),
            secret: Arc::new(secret),
            verify_calls: verify_calls.clone(),
            mode: Arc::new(mode),
        };
        let app = Router::new()
            .route("/webhook", post(handle))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}/webhook", addr);
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Tiny beat to let axum register the route; the listener is
        // already bound so this is belt-and-braces.
        tokio::task::yield_now().await;

        Self {
            url,
            received,
            verify_calls,
            task,
        }
    }

    async fn received(&self) -> Vec<ReceivedRequest> {
        self.received.lock().await.clone()
    }

    async fn wait_for_requests(&self, want: usize, deadline: Duration) -> bool {
        let end = tokio::time::Instant::now() + deadline;
        while tokio::time::Instant::now() < end {
            if self.received.lock().await.len() >= want {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.received.lock().await.len() >= want
    }

    fn verify_calls(&self) -> u32 {
        self.verify_calls.load(Ordering::SeqCst)
    }

    fn shutdown(self) {
        self.task.abort();
    }
}

async fn handle(State(state): State<MockState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    // Verify HMAC first. The real gateway must reject bad sigs with
    // 401; our mock does the same so a mis-signed payload round-trip
    // is observable.
    //
    // Timestamp is now part of the signed material (anti-replay), so
    // we extract it from the X-AHand-Timestamp header before verifying.
    let signature = headers
        .get("x-ahand-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let timestamp_secs: u64 = headers
        .get("x-ahand-timestamp")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let signature_ok = verify(state.secret.as_ref(), timestamp_secs, &body, signature);
    state.verify_calls.fetch_add(1, Ordering::SeqCst);

    state.received.lock().await.push(ReceivedRequest {
        headers: headers.clone(),
        body: body.to_vec(),
    });

    match state.mode.as_ref() {
        MockMode::Ok => {
            if !signature_ok {
                return StatusCode::UNAUTHORIZED;
            }
            StatusCode::NO_CONTENT
        }
        MockMode::AlwaysFail { status } => *status,
    }
}

fn make_webhook_and_worker(
    gateway_url: &str,
    max_retries: u32,
    max_concurrency: usize,
    dlq_path: PathBuf,
) -> (Arc<Webhook>, WorkerHandle, Arc<dyn WebhookDeliveryStore>) {
    let store: Arc<dyn WebhookDeliveryStore> = Arc::new(MemoryWebhookDeliveryStore::new());
    let config = WebhookConfig {
        url: gateway_url.into(),
        secret: "s3cret-bytes".into(),
        max_retries,
        max_concurrency,
        dlq_path,
        request_timeout: Duration::from_secs(2),
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .user_agent(concat!("ahand-hub-webhook/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap();
    let (webhook, handle) = Webhook::new_with_client(store.clone(), config, client);
    (webhook, handle, store)
}

fn tmp_dlq(stem: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ahand-hub-webhook-dlq-{}-{}.jsonl",
        stem,
        std::process::id()
    ))
}

async fn read_dlq_lines(path: &PathBuf) -> Vec<serde_json::Value> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Vec::new();
    }
    let text = tokio::fs::read_to_string(path).await.unwrap();
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn remove_file_quiet(path: &PathBuf) {
    let _ = tokio::fs::remove_file(path).await;
}

#[tokio::test]
async fn happy_path_posts_signed_payload_and_deletes_row() {
    let dlq = tmp_dlq("happy");
    remove_file_quiet(&dlq).await;
    let secret = b"s3cret-bytes".to_vec();
    let gateway = MockGateway::start(MockMode::Ok, secret).await;

    let (webhook, worker, store) = make_webhook_and_worker(&gateway.url, 8, 4, dlq.clone());
    let worker_task = tokio::spawn(worker.run());

    webhook
        .enqueue_online("device-1", Some("user-1"), &[])
        .await
        .unwrap();

    assert!(gateway.wait_for_requests(1, Duration::from_secs(3)).await);
    let received = gateway.received().await;
    assert_eq!(received.len(), 1);
    let request = &received[0];

    let signature = request.headers.get("x-ahand-signature").unwrap();
    assert!(signature.to_str().unwrap().starts_with("sha256="));
    assert!(request.headers.get("x-ahand-event-id").is_some());
    assert!(request.headers.get("x-ahand-timestamp").is_some());
    // `X-AHand-Event-Timestamp` is the stable event-creation time; it
    // must be present on every delivery and should be close to the
    // payload's `occurredAt` (second precision).
    let event_ts_header = request
        .headers
        .get("x-ahand-event-timestamp")
        .expect("X-AHand-Event-Timestamp must be set");
    let event_ts_secs: i64 = event_ts_header
        .to_str()
        .unwrap()
        .parse()
        .expect("event timestamp must be an integer number of seconds");
    let content_type = request.headers.get("content-type").unwrap();
    assert_eq!(content_type, "application/json");
    let ua = request.headers.get("user-agent").unwrap().to_str().unwrap();
    assert!(ua.starts_with("ahand-hub-webhook/"), "unexpected UA: {ua}",);

    let payload: WebhookPayload = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(payload.event_type, "device.online");
    assert_eq!(payload.device_id, "device-1");
    assert_eq!(payload.external_user_id.as_deref(), Some("user-1"));
    // The event timestamp header must match the delivery's creation
    // time, which tracks the payload's `occurred_at`. Allow ~2s slack
    // to cover the gap between `Utc::now()` calls inside
    // `enqueue_typed`.
    let payload_occurred = payload.occurred_at.timestamp();
    assert!(
        (payload_occurred - event_ts_secs).abs() <= 2,
        "X-AHand-Event-Timestamp {event_ts_secs} should match payload.occurredAt {payload_occurred}"
    );

    // Eventually the row is deleted.
    wait_for_store_len(&store, 0, Duration::from_secs(3)).await;

    worker_task.abort();
    gateway.shutdown();
    remove_file_quiet(&dlq).await;
}

#[tokio::test]
async fn event_timestamp_stable_across_retries() {
    // When a retry fires after a transient 5xx, `X-AHand-Event-Timestamp`
    // must stay pinned to the delivery's original `created_at` while
    // `X-AHand-Timestamp` (the signing time, part of the HMAC input) is
    // freshly computed for each POST. The gateway relies on this split
    // to measure event-age latency and dedupe late retries independently
    // from anti-replay.
    let dlq = tmp_dlq("event-ts-stable");
    remove_file_quiet(&dlq).await;

    let gateway = MockGateway::start(
        MockMode::AlwaysFail {
            status: StatusCode::BAD_GATEWAY,
        },
        b"s3cret-bytes".to_vec(),
    )
    .await;

    // max_retries high enough that the second attempt still retries (does
    // not DLQ). First backoff is 2^1 = 2s.
    let (webhook, worker, _store) = make_webhook_and_worker(&gateway.url, 8, 2, dlq.clone());
    let worker_task = tokio::spawn(worker.run());

    webhook
        .enqueue_offline("device-retry-ts", None)
        .await
        .unwrap();

    // Expect at least 2 delivery attempts: first immediate, second
    // after the ~2s backoff. Allow 8s of wall-clock for CI slack.
    assert!(
        gateway.wait_for_requests(2, Duration::from_secs(8)).await,
        "expected two delivery attempts"
    );

    let received = gateway.received().await;
    assert!(
        received.len() >= 2,
        "expected >=2 attempts, got {}",
        received.len()
    );

    let read_header = |req: &ReceivedRequest, name: &str| -> String {
        req.headers
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing on request"))
            .to_str()
            .unwrap()
            .to_string()
    };

    let first_event_ts = read_header(&received[0], "x-ahand-event-timestamp");
    let first_sign_ts = read_header(&received[0], "x-ahand-timestamp");
    let second_event_ts = read_header(&received[1], "x-ahand-event-timestamp");
    let second_sign_ts = read_header(&received[1], "x-ahand-timestamp");

    // Event timestamp is pinned to `delivery.created_at` — must match
    // across both attempts.
    assert_eq!(
        first_event_ts, second_event_ts,
        "X-AHand-Event-Timestamp must not change across retries",
    );
    // Signing timestamp is recomputed per attempt — with the 2s backoff
    // the second one should be strictly greater.
    let first_sign_secs: u64 = first_sign_ts.parse().unwrap();
    let second_sign_secs: u64 = second_sign_ts.parse().unwrap();
    assert!(
        second_sign_secs > first_sign_secs,
        "X-AHand-Timestamp must be freshly computed on retry: first={first_sign_secs}, second={second_sign_secs}"
    );

    worker_task.abort();
    gateway.shutdown();
    remove_file_quiet(&dlq).await;
}

#[tokio::test]
async fn server_error_schedules_retry_and_increments_attempts() {
    let dlq = tmp_dlq("retry");
    remove_file_quiet(&dlq).await;
    let gateway = MockGateway::start(
        MockMode::AlwaysFail {
            status: StatusCode::BAD_GATEWAY,
        },
        b"s3cret-bytes".to_vec(),
    )
    .await;
    let (webhook, worker, store) = make_webhook_and_worker(&gateway.url, 8, 2, dlq.clone());
    let worker_task = tokio::spawn(worker.run());

    webhook.enqueue_offline("device-1", None).await.unwrap();

    // First attempt lands and bumps attempts to 1; backoff is
    // 2^1=2s so the row persists with attempts=1 for that window.
    assert!(gateway.wait_for_requests(1, Duration::from_secs(3)).await);

    // Poll the store until attempts increments — the `send_one`
    // task spawned by the worker writes asynchronously, so it may
    // not be visible the exact millisecond the request lands.
    let row = wait_for_attempts(&store, "device-1", 1, Duration::from_secs(3)).await;
    assert_eq!(row.attempts, 1);
    let delta = (row.next_retry_at - chrono::Utc::now()).num_seconds();
    assert!(
        (1..=3).contains(&delta),
        "expected next_retry_at ~2s out, got {}s",
        delta
    );

    worker_task.abort();
    gateway.shutdown();
    remove_file_quiet(&dlq).await;
}

#[tokio::test]
async fn retries_exhausted_moves_row_to_dlq() {
    let dlq = tmp_dlq("exhaust");
    remove_file_quiet(&dlq).await;

    // Real backoff is 2^attempts, so with default max_retries=8 a
    // natural run would wait ~511s. Override to 2 retries so the
    // test can finish in a few seconds.
    let gateway = MockGateway::start(
        MockMode::AlwaysFail {
            status: StatusCode::INTERNAL_SERVER_ERROR,
        },
        b"s3cret-bytes".to_vec(),
    )
    .await;
    let (webhook, worker, store) = make_webhook_and_worker(&gateway.url, 2, 2, dlq.clone());
    let worker_task = tokio::spawn(worker.run());

    webhook.enqueue_offline("device-1", None).await.unwrap();

    // With max_retries=2, the second failure trips the exhaustion
    // path and moves the row to DLQ. First attempt is immediate,
    // second attempt happens 2s later.
    wait_for_store_len(&store, 0, Duration::from_secs(8)).await;

    let dlq_lines = read_dlq_lines(&dlq).await;
    assert_eq!(dlq_lines.len(), 1);
    assert_eq!(dlq_lines[0]["payload"]["eventType"], "device.offline");
    assert!(dlq_lines[0]["lastError"].as_str().unwrap().contains("500"),);

    // A subsequent enqueue after DLQ draining must still work.
    webhook.enqueue_offline("device-2", None).await.unwrap();
    assert_eq!(
        store.pending_count().await.unwrap(),
        1,
        "new enqueues after DLQ must still persist",
    );

    worker_task.abort();
    gateway.shutdown();
    remove_file_quiet(&dlq).await;
}

#[tokio::test]
async fn unauthorized_moves_row_to_dlq_without_retry() {
    // 401 is a permanent failure; the row must DLQ on the first attempt.
    let dlq = tmp_dlq("unauth");
    remove_file_quiet(&dlq).await;

    let gateway = MockGateway::start(
        MockMode::AlwaysFail {
            status: StatusCode::UNAUTHORIZED,
        },
        b"s3cret-bytes".to_vec(),
    )
    .await;
    let (webhook, worker, store) = make_webhook_and_worker(&gateway.url, 8, 2, dlq.clone());
    let worker_task = tokio::spawn(worker.run());

    webhook.enqueue_offline("device-1", None).await.unwrap();
    wait_for_store_len(&store, 0, Duration::from_secs(3)).await;

    // Exactly one POST: 401 is permanent.
    assert_eq!(gateway.received().await.len(), 1);
    let dlq_lines = read_dlq_lines(&dlq).await;
    assert_eq!(dlq_lines.len(), 1);
    assert!(dlq_lines[0]["lastError"].as_str().unwrap().contains("401"),);

    worker_task.abort();
    gateway.shutdown();
    remove_file_quiet(&dlq).await;
}

#[tokio::test]
async fn too_many_requests_retries_with_backoff() {
    // 429 Too Many Requests is a transient rate-limit response; the worker
    // must NOT DLQ the row but increment attempts and schedule a retry.
    let dlq = tmp_dlq("429-retry");
    remove_file_quiet(&dlq).await;

    let gateway = MockGateway::start(
        MockMode::AlwaysFail {
            status: StatusCode::TOO_MANY_REQUESTS,
        },
        b"s3cret-bytes".to_vec(),
    )
    .await;
    let (webhook, worker, store) = make_webhook_and_worker(&gateway.url, 8, 2, dlq.clone());
    let worker_task = tokio::spawn(worker.run());

    webhook.enqueue_offline("device-1", None).await.unwrap();

    // Wait for the first POST to land and attempts to increment.
    assert!(gateway.wait_for_requests(1, Duration::from_secs(3)).await);
    let row = wait_for_attempts(&store, "device-1", 1, Duration::from_secs(3)).await;

    // Row must still be in the store (not DLQed).
    assert_eq!(row.attempts, 1, "attempts should be 1 after first 429");
    let delta = (row.next_retry_at - chrono::Utc::now()).num_seconds();
    assert!(
        (1..=3).contains(&delta),
        "expected next_retry_at ~2s out after 429, got {}s",
        delta
    );

    // DLQ must be empty — 429 is retriable.
    let dlq_lines = read_dlq_lines(&dlq).await;
    assert_eq!(dlq_lines.len(), 0, "429 must not DLQ the row");

    worker_task.abort();
    gateway.shutdown();
    remove_file_quiet(&dlq).await;
}

#[tokio::test]
async fn signature_mismatch_is_detected_by_receiver() {
    // The mock gateway returns 401 when the signature doesn't
    // verify. We send with the wrong secret by wiring the webhook
    // with a different secret than the mock expects.
    let dlq = tmp_dlq("badsig");
    remove_file_quiet(&dlq).await;
    let gateway = MockGateway::start(MockMode::Ok, b"correct-secret".to_vec()).await;

    let store: Arc<dyn WebhookDeliveryStore> = Arc::new(MemoryWebhookDeliveryStore::new());
    let config = WebhookConfig {
        url: gateway.url.clone(),
        secret: "wrong-secret".into(),
        max_retries: 8,
        max_concurrency: 2,
        dlq_path: dlq.clone(),
        request_timeout: Duration::from_secs(2),
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let (webhook, worker) = Webhook::new_with_client(store.clone(), config, client);
    let worker_task = tokio::spawn(worker.run());

    webhook.enqueue_offline("device-1", None).await.unwrap();
    wait_for_store_len(&store, 0, Duration::from_secs(3)).await;

    // One POST, then DLQ (401).
    assert_eq!(gateway.received().await.len(), 1);
    assert_eq!(gateway.verify_calls(), 1);
    let dlq_lines = read_dlq_lines(&dlq).await;
    assert_eq!(dlq_lines.len(), 1);

    worker_task.abort();
    gateway.shutdown();
    remove_file_quiet(&dlq).await;
}

#[tokio::test]
async fn concurrent_enqueue_is_bounded_by_semaphore() {
    let dlq = tmp_dlq("concurrency");
    remove_file_quiet(&dlq).await;
    let gateway = MockGateway::start(MockMode::Ok, b"s3cret-bytes".to_vec()).await;

    // 100 events, max_concurrency=5 — the worker must serialize
    // them through a 5-permit semaphore. The success path still
    // drains every row, so we just assert the mock received 100
    // and the store emptied.
    let (webhook, worker, store) = make_webhook_and_worker(&gateway.url, 8, 5, dlq.clone());
    let worker_task = tokio::spawn(worker.run());

    for n in 0..100 {
        webhook
            .enqueue_online(&format!("device-{n}"), Some("user-1"), &[])
            .await
            .unwrap();
    }

    assert!(
        gateway
            .wait_for_requests(100, Duration::from_secs(10))
            .await
    );
    wait_for_store_len(&store, 0, Duration::from_secs(10)).await;

    worker_task.abort();
    gateway.shutdown();
    remove_file_quiet(&dlq).await;
}

#[tokio::test]
async fn duplicate_event_id_upserts_single_row() {
    // Direct store test — the enqueue helpers generate fresh
    // ULIDs so we construct the delivery manually to exercise the
    // upsert path.
    use ahand_hub_store::webhook_delivery_store::WebhookDelivery;

    let store = MemoryWebhookDeliveryStore::new();
    let delivery_v1 = WebhookDelivery {
        event_id: "fixed-id".into(),
        payload: serde_json::json!({ "v": 1 }),
        attempts: 0,
        next_retry_at: chrono::Utc::now(),
        last_error: None,
        created_at: chrono::Utc::now(),
    };
    let delivery_v2 = WebhookDelivery {
        event_id: "fixed-id".into(),
        payload: serde_json::json!({ "v": 2 }),
        attempts: 0,
        next_retry_at: chrono::Utc::now(),
        last_error: None,
        created_at: chrono::Utc::now(),
    };
    store.enqueue(delivery_v1).await.unwrap();
    store.enqueue(delivery_v2).await.unwrap();

    assert_eq!(store.pending_count().await.unwrap(), 1);
}

#[test]
fn sign_produces_stable_hex_header() {
    // Signed input is: "1700000000" + "." + body — timestamp is now
    // part of the HMAC material so replay attacks (changing only
    // X-AHand-Timestamp) invalidate the signature.
    let ts = 1_700_000_000u64;
    let body = b"{\"a\":1}";
    let sig = sign(b"secret", ts, body);
    assert!(sig.starts_with("sha256="), "unexpected prefix: {sig}");
    // Round-trip: verify must accept the same timestamp.
    assert!(verify(b"secret", ts, body, &sig));
    // Different timestamp must NOT verify (anti-replay).
    assert!(!verify(b"secret", ts + 1, body, &sig));
}

#[test]
fn backoff_schedule_exposed_for_callers() {
    assert_eq!(backoff_secs(0), 1);
    assert_eq!(backoff_secs(1), 2);
    assert_eq!(backoff_secs(8), 256);
}

/// When `append_dlq` fails (e.g. the directory doesn't exist and can't be
/// created), the worker must apply a 5-minute back-off so it doesn't spin
/// tightly attempting the same failing DLQ write on every tick.
#[tokio::test]
async fn dlq_write_failure_applies_backoff_not_spin() {
    // /dev/null is not a directory, so create_dir_all inside append_dlq
    // will fail with ENOTDIR — a reliable way to simulate a broken DLQ path.
    let bad_dlq = PathBuf::from("/dev/null/subdir/webhook_dlq.jsonl");

    // Point the webhook at an address that will immediately refuse connections.
    // Port 1 is reserved and always fails on all platforms.
    let unreachable_url = "http://127.0.0.1:1/webhook";

    // max_retries=1 means the very first (and only) failed send triggers
    // the DLQ path: next_attempts(1) >= max_retries(1).
    let (webhook, worker, store) = make_webhook_and_worker(unreachable_url, 1, 2, bad_dlq.clone());
    let worker_task = tokio::spawn(worker.run());

    webhook
        .enqueue_offline("device-dlq-fail", None)
        .await
        .unwrap();

    // Wait for the row to stay in the store AND have last_error="dlq_write_failed",
    // meaning the back-off mark_failed was applied.
    let deadline = Duration::from_secs(10);
    let end = tokio::time::Instant::now() + deadline;
    let row = loop {
        let leased = store
            .lease_due(chrono::Utc::now() + chrono::Duration::seconds(7200), 10)
            .await
            .unwrap();
        // Release each row so we don't interfere with the worker.
        for r in &leased {
            let _ = store
                .mark_failed(
                    &r.event_id,
                    r.next_retry_at,
                    r.attempts,
                    r.last_error.as_deref().unwrap_or(""),
                )
                .await;
        }
        if let Some(r) = leased
            .iter()
            .find(|r| r.last_error.as_deref() == Some("dlq_write_failed"))
            .cloned()
        {
            break r;
        }
        if tokio::time::Instant::now() >= end {
            panic!(
                "row never got dlq_write_failed back-off within {deadline:?}; store len={}",
                store.pending_count().await.unwrap()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // The back-off must push next_retry_at into the future (not in the past).
    let delta = (row.next_retry_at - chrono::Utc::now()).num_seconds();
    assert!(
        delta > 0,
        "expected next_retry_at to be in the future after DLQ write failure, got {}s",
        delta
    );
    // Back-off is 5 minutes; allow a small tolerance for slow CI.
    assert!(
        delta <= 305,
        "back-off should be ~5 minutes, got {}s (too large?)",
        delta
    );

    worker_task.abort();
}

async fn wait_for_store_len(
    store: &Arc<dyn WebhookDeliveryStore>,
    want: usize,
    deadline: Duration,
) {
    let end = tokio::time::Instant::now() + deadline;
    while tokio::time::Instant::now() < end {
        if store.pending_count().await.unwrap() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "store len never reached {want} within {deadline:?} (last = {})",
        store.pending_count().await.unwrap()
    );
}

async fn wait_for_attempts(
    store: &Arc<dyn WebhookDeliveryStore>,
    _event_type: &str,
    want: i32,
    deadline: Duration,
) -> ahand_hub_store::webhook_delivery_store::WebhookDelivery {
    // Lease view returns rows that are due — if not due we have
    // to introspect via a fresh lease after sufficient time, so
    // we probe with `lease_due(now + large offset)` to capture
    // rows whose next_retry_at is in the near future.
    let end = tokio::time::Instant::now() + deadline;
    loop {
        let leased = store
            .lease_due(chrono::Utc::now() + chrono::Duration::seconds(3600), 10)
            .await
            .unwrap();
        // Immediately release by marking failed with the same
        // attempts (so the test doesn't break the worker's view).
        for row in &leased {
            let _ = store
                .mark_failed(
                    &row.event_id,
                    row.next_retry_at,
                    row.attempts,
                    row.last_error.as_deref().unwrap_or(""),
                )
                .await;
        }
        if let Some(row) = leased.iter().find(|r| r.attempts == want).cloned() {
            return row;
        }
        if tokio::time::Instant::now() >= end {
            panic!("attempts never reached {want} within {deadline:?} (leased={leased:?})",);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
