use std::sync::{Arc, Mutex};
use std::time::Duration;

use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::auth::AuthService;
use ahand_hub_core::device::{Device, NewDevice};
use ahand_hub_core::job::{Job, JobFilter, JobStatus, NewJob};
use ahand_hub_core::services::device_manager::DeviceManager;
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::traits::{AuditStore, DeviceAdminStore, DeviceStore, JobStore};
use ahand_hub_core::{HubError, Result};
use ahand_hub_store::audit_store::PgAuditStore;
use ahand_hub_store::bootstrap_store::RedisBootstrapStore;
use ahand_hub_store::device_store::PgDeviceStore;
use ahand_hub_store::event_fanout::RedisEventFanout;
use ahand_hub_store::job_output_store::RedisJobOutputStore;
use ahand_hub_store::job_store::PgJobStore;
use ahand_hub_store::presence_store::RedisPresenceStore;
use ahand_hub_store::webhook_delivery_store::{
    MemoryWebhookDeliveryStore, PgWebhookDeliveryStore, WebhookDeliveryStore,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::{DashMap, mapref::entry::Entry};
use governor::{Quota, RateLimiter, clock::DefaultClock, state::keyed::DefaultKeyedStateStore};

/// Control-plane rate limiter keyed by `external_user_id`.
///
/// Default policy: **10 requests per second sustained, 100-request burst**
/// — matches the plan's § 8 guidance. Tuning hooks (env vars, per-scope
/// caps) can be added later without changing the type alias.
pub type ControlPlaneRateLimiter =
    RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock>;

pub fn default_control_plane_rate_limiter() -> ControlPlaneRateLimiter {
    // Burst = 100 means we allow up to 100 rapid requests before
    // needing to wait for the 10-per-second replenishment to kick in.
    // `Quota::per_second(10)` sets the sustained cap; `allow_burst(100)`
    // widens the bucket.
    let quota = Quota::per_second(std::num::NonZeroU32::new(10).expect("10 is non-zero"))
        .allow_burst(std::num::NonZeroU32::new(100).expect("100 is non-zero"));
    RateLimiter::keyed(quota)
}

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthService>,
    pub device_manager: Arc<DeviceManager>,
    pub job_dispatcher: Arc<JobDispatcher>,
    pub devices: Arc<MemoryDeviceStore>,
    pub jobs_store: Arc<dyn JobStore>,
    pub audit_store: Arc<dyn AuditStore>,
    audit_writer: Arc<crate::audit_writer::BufferedAuditStore>,
    pub jobs: Arc<crate::http::jobs::JobRuntime>,
    pub control_jobs: Arc<crate::control_jobs::ControlJobTracker>,
    pub control_rate_limiter: Arc<ControlPlaneRateLimiter>,
    pub connections: Arc<crate::ws::device_gateway::ConnectionRegistry>,
    pub browser_pending:
        Arc<DashMap<String, tokio::sync::oneshot::Sender<ahand_protocol::BrowserResponse>>>,
    pub events: Arc<crate::events::EventBus>,
    pub output_stream: Arc<crate::output_stream::OutputStream>,
    pub bootstrap_tokens: Arc<crate::bootstrap::BootstrapCredentials>,
    pub device_bootstrap_token: Arc<String>,
    pub device_bootstrap_device_id: Arc<String>,
    pub device_hello_max_age_ms: u64,
    /// Cadence for the staleness-monitor probe loop. See
    /// [`crate::config::Config::device_staleness_probe_interval_ms`].
    pub device_staleness_probe_interval_ms: u64,
    /// Timeout after which an idle device WS is closed. See
    /// [`crate::config::Config::device_staleness_timeout_ms`].
    pub device_staleness_timeout_ms: u64,
    /// Expected daemon heartbeat cadence in seconds. Advertised to
    /// downstream webhook consumers via `presenceTtlSeconds` hints
    /// (`secs × 3`).
    pub device_expected_heartbeat_secs: u64,
    pub device_presence_refresh_ms: u64,
    pub service_token: Arc<String>,
    pub dashboard_shared_password: Arc<String>,
    pub dashboard_allowed_origins: Arc<Vec<String>>,
    pub terminal_tokens: Arc<DashMap<String, crate::http::terminal::TerminalToken>>,
    pub pending_file_requests: Arc<crate::pending_file_requests::PendingFileRequests>,
    pub s3_client: Option<Arc<crate::s3::S3Client>>,
    /// Outbound webhook dispatcher. Always present; when
    /// `config.webhook_url` is `None`, this is a no-op (`Webhook::disabled()`)
    /// so call sites can always invoke `state.webhook.enqueue_*` without
    /// branching.
    pub webhook: Arc<crate::webhook::Webhook>,
}

impl AppState {
    pub async fn from_config(config: crate::config::Config) -> anyhow::Result<Self> {
        let finished_retention = Duration::from_millis(config.output_retention_ms);
        let audit_retention_days = config.audit_retention_days;
        let audit_fallback_path = config.audit_fallback_path.clone();
        let bootstrap_reservation_ttl =
            crate::bootstrap::BootstrapCredentials::reservation_ttl(config.device_hello_max_age_ms);
        let (
            devices,
            jobs_store,
            raw_audit_store,
            persistent_output,
            persistent_fanout,
            bootstrap_tokens,
            webhook_delivery_store,
        ) = match &config.store {
            crate::config::StoreConfig::Memory => (
                Arc::new(MemoryDeviceStore::default()),
                Arc::new(MemoryJobStore::default()) as Arc<dyn JobStore>,
                Arc::new(MemoryAuditStore::default()) as Arc<dyn AuditStore>,
                None,
                None,
                crate::bootstrap::BootstrapCredentials::memory(),
                Arc::new(MemoryWebhookDeliveryStore::new()) as Arc<dyn WebhookDeliveryStore>,
            ),
            crate::config::StoreConfig::Persistent {
                database_url,
                redis_url,
            } => {
                let pool = ahand_hub_store::postgres::connect_database(database_url).await?;
                let presence_redis = ahand_hub_store::redis::connect_redis(redis_url).await?;
                let bootstrap_redis = ahand_hub_store::redis::connect_redis(redis_url).await?;
                let presence = RedisPresenceStore::new_with_ttl(
                    presence_redis,
                    config.device_presence_ttl_secs,
                );
                (
                    Arc::new(MemoryDeviceStore::with_persistent(
                        PgDeviceStore::with_presence(pool.clone(), presence),
                    )),
                    Arc::new(PgJobStore::new(pool.clone())) as Arc<dyn JobStore>,
                    Arc::new(PgAuditStore::new(pool.clone())) as Arc<dyn AuditStore>,
                    Some(RedisJobOutputStore::new(redis_url, finished_retention).await?),
                    Some(RedisEventFanout::new(redis_url).await?),
                    crate::bootstrap::BootstrapCredentials::redis(RedisBootstrapStore::new(
                        bootstrap_redis,
                        bootstrap_reservation_ttl,
                    )),
                    Arc::new(PgWebhookDeliveryStore::new(pool)) as Arc<dyn WebhookDeliveryStore>,
                )
            }
        };
        let audit_writer = Arc::new(
            crate::audit_writer::BufferedAuditStore::new_with_fallback_path(
                raw_audit_store,
                audit_fallback_path,
            ),
        );
        let audit_store = audit_writer.clone() as Arc<dyn AuditStore>;
        spawn_audit_retention_task(audit_store.clone(), audit_retention_days);
        let output_stream = Arc::new(match persistent_output {
            Some(store) => crate::output_stream::OutputStream::persistent(store),
            None => crate::output_stream::OutputStream::new(finished_retention, 256),
        });
        let connections = Arc::new(crate::ws::device_gateway::ConnectionRegistry::default());
        let events = Arc::new(match persistent_fanout {
            Some(fanout) => crate::events::EventBus::new_with_fanout(audit_store.clone(), fanout),
            None => crate::events::EventBus::new(audit_store.clone()),
        });
        let device_manager = Arc::new(DeviceManager::new(devices.clone()));
        let job_dispatcher = Arc::new(JobDispatcher::new(
            devices.clone(),
            jobs_store.clone(),
            audit_store.clone(),
        ));
        let pending_file_requests = crate::pending_file_requests::new_pending_requests();
        let s3_client = if let Some(ref s3_cfg) = config.s3 {
            Some(Arc::new(crate::s3::S3Client::new(s3_cfg).await))
        } else {
            None
        };
        let jobs = Arc::new(crate::http::jobs::JobRuntime::new(
            job_dispatcher.clone(),
            jobs_store.clone(),
            connections.clone(),
            events.clone(),
            output_stream.clone(),
            config.job_timeout_grace_ms,
            config.device_disconnect_grace_ms,
            pending_file_requests.clone(),
        ));

        let browser_pending = Arc::new(DashMap::new());
        let control_jobs = Arc::new(crate::control_jobs::ControlJobTracker::new());
        let control_rate_limiter = Arc::new(default_control_plane_rate_limiter());

        // Webhook dispatcher — always present. When the URL/secret are
        // unconfigured we build a no-op so downstream call sites don't
        // have to branch on Option. When configured we spin up the
        // background worker that drains the delivery store.
        let webhook = match (&config.webhook_url, &config.webhook_secret) {
            (Some(url), Some(secret)) if !url.is_empty() && !secret.is_empty() => {
                let webhook_config = crate::webhook::WebhookConfig {
                    url: url.clone(),
                    secret: secret.clone(),
                    max_retries: config.webhook_max_retries,
                    max_concurrency: config.webhook_max_concurrency,
                    dlq_path: crate::webhook::worker::dlq_path_from_audit_fallback(
                        &config.audit_fallback_path,
                    ),
                    request_timeout: Duration::from_millis(config.webhook_timeout_ms),
                };
                let (webhook, handle) =
                    crate::webhook::Webhook::new(webhook_delivery_store, webhook_config);
                tokio::spawn(handle.run());
                webhook
            }
            _ => crate::webhook::Webhook::disabled(),
        };

        let state = Self {
            auth: Arc::new(AuthService::new(&config.jwt_secret)),
            device_manager,
            job_dispatcher,
            devices,
            jobs_store,
            audit_store,
            audit_writer,
            jobs,
            control_jobs,
            control_rate_limiter,
            connections,
            browser_pending,
            events,
            output_stream,
            bootstrap_tokens: Arc::new(bootstrap_tokens),
            device_bootstrap_token: Arc::new(config.device_bootstrap_token),
            device_bootstrap_device_id: Arc::new(config.device_bootstrap_device_id),
            device_hello_max_age_ms: config.device_hello_max_age_ms,
            device_staleness_probe_interval_ms: config.device_staleness_probe_interval_ms,
            device_staleness_timeout_ms: config.device_staleness_timeout_ms,
            device_expected_heartbeat_secs: config.device_expected_heartbeat_secs,
            device_presence_refresh_ms: config.device_presence_refresh_ms,
            service_token: Arc::new(config.service_token),
            dashboard_shared_password: Arc::new(config.dashboard_shared_password),
            dashboard_allowed_origins: Arc::new(config.dashboard_allowed_origins),
            terminal_tokens: Arc::new(DashMap::new()),
            pending_file_requests,
            s3_client,
            webhook,
        };
        state
            .preregister_bootstrap_device(state.device_bootstrap_device_id.as_str())
            .await?;
        Ok(state)
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.audit_writer.shutdown().await?;
        Ok(())
    }

    pub async fn append_audit_entry(
        &self,
        action: &str,
        resource_type: &str,
        resource_id: &str,
        actor: &str,
        detail: serde_json::Value,
    ) {
        if let Err(err) = self
            .audit_store
            .append(&[AuditEntry {
                timestamp: chrono::Utc::now(),
                action: action.into(),
                resource_type: resource_type.into(),
                resource_id: resource_id.into(),
                actor: actor.into(),
                detail,
                source_ip: None,
            }])
            .await
        {
            tracing::warn!(
                action,
                resource_type,
                resource_id,
                actor,
                error = %err,
                "failed to append audit entry"
            );
        }
    }

    async fn preregister_bootstrap_device(&self, device_id: &str) -> Result<()> {
        if self.devices.get(device_id).await?.is_some() {
            return Ok(());
        }

        self.devices
            .insert(NewDevice {
                id: device_id.into(),
                public_key: None,
                hostname: "pending-device".into(),
                os: "unknown".into(),
                capabilities: Vec::new(),
                version: None,
                auth_method: "bootstrap".into(),
                external_user_id: None,
            })
            .await?;
        self.devices.mark_offline(device_id).await?;
        Ok(())
    }
}

pub struct MemoryDeviceStore {
    devices: DashMap<String, StoredDevice>,
    persistent: Option<PgDeviceStore>,
}

#[derive(Clone)]
struct StoredDevice {
    device: Device,
    last_signed_at_ms: u64,
}

impl MemoryDeviceStore {
    pub fn with_persistent(persistent: PgDeviceStore) -> Self {
        Self {
            devices: DashMap::new(),
            persistent: Some(persistent),
        }
    }

    pub async fn accept_verified_hello(
        &self,
        device_id: &str,
        hello: &ahand_protocol::Hello,
        verified: &crate::auth::VerifiedDeviceHello,
    ) -> Result<Device> {
        if let Some(persistent) = &self.persistent {
            return self
                .accept_verified_hello_persistent(persistent, device_id, hello, verified)
                .await;
        }

        match self.devices.entry(device_id.into()) {
            Entry::Occupied(mut entry) => {
                let stored = entry.get_mut();
                match (
                    verified.allow_registration,
                    stored.device.public_key.as_ref(),
                ) {
                    (true, None) => {
                        stored.device.public_key = Some(verified.public_key.clone());
                    }
                    (false, Some(existing_key)) if existing_key == &verified.public_key => {}
                    _ => return Err(HubError::Unauthorized),
                }
                if verified.signed_at_ms <= stored.last_signed_at_ms {
                    return Err(HubError::Unauthorized);
                }

                stored.device.hostname = hello.hostname.clone();
                stored.device.os = hello.os.clone();
                stored.device.capabilities = hello.capabilities.clone();
                stored.device.version = Some(hello.version.clone());
                stored.device.auth_method = verified.auth_method.into();
                stored.last_signed_at_ms = verified.signed_at_ms;

                Ok(stored.device.clone())
            }
            Entry::Vacant(_) => Err(HubError::Unauthorized),
        }
    }

    pub async fn mark_online(&self, device_id: &str, endpoint: &str) -> Result<()> {
        let exists = if self.devices.contains_key(device_id) {
            true
        } else if let Some(persistent) = &self.persistent {
            persistent.get(device_id).await?.is_some()
        } else {
            false
        };

        if !exists {
            return Err(HubError::DeviceNotFound(device_id.into()));
        }

        if let Some(mut device) = self.devices.get_mut(device_id) {
            device.device.online = true;
        }
        if let Some(persistent) = &self.persistent {
            persistent.mark_online(device_id, endpoint).await?;
        }

        Ok(())
    }

    pub async fn mark_offline(&self, device_id: &str) -> Result<()> {
        if let Some(mut device) = self.devices.get_mut(device_id) {
            device.device.online = false;
        }
        if let Some(persistent) = &self.persistent {
            persistent.mark_offline(device_id).await?;
            return Ok(());
        }

        if self.devices.contains_key(device_id) {
            Ok(())
        } else {
            Err(HubError::DeviceNotFound(device_id.into()))
        }
    }

    async fn accept_verified_hello_persistent(
        &self,
        persistent: &PgDeviceStore,
        device_id: &str,
        hello: &ahand_protocol::Hello,
        verified: &crate::auth::VerifiedDeviceHello,
    ) -> Result<Device> {
        let last_signed_at_ms = self
            .devices
            .get(device_id)
            .map(|entry| entry.last_signed_at_ms)
            .unwrap_or(0);
        if verified.signed_at_ms <= last_signed_at_ms {
            return Err(HubError::Unauthorized);
        }

        match persistent.get(device_id).await? {
            Some(existing) => match (verified.allow_registration, existing.public_key.as_ref()) {
                (true, None) => {}
                (false, Some(existing_key)) if existing_key == &verified.public_key => {}
                _ => return Err(HubError::Unauthorized),
            },
            None => return Err(HubError::Unauthorized),
        }

        let device = persistent
            .upsert_device(NewDevice {
                id: device_id.into(),
                public_key: Some(verified.public_key.clone()),
                hostname: hello.hostname.clone(),
                os: hello.os.clone(),
                capabilities: hello.capabilities.clone(),
                version: Some(hello.version.clone()),
                auth_method: verified.auth_method.into(),
                // Hello-flow upserts never overwrite an admin-set
                // external_user_id; the upsert uses COALESCE so
                // passing None preserves whatever was pre-registered.
                external_user_id: None,
            })
            .await?;
        let device = persistent.get(device_id).await?.unwrap_or(Device {
            online: false,
            ..device
        });
        self.devices.insert(
            device_id.into(),
            StoredDevice {
                device: device.clone(),
                last_signed_at_ms: verified.signed_at_ms,
            },
        );
        Ok(device)
    }
}

impl Default for MemoryDeviceStore {
    fn default() -> Self {
        Self {
            devices: DashMap::new(),
            persistent: None,
        }
    }
}

#[async_trait]
impl DeviceStore for MemoryDeviceStore {
    async fn insert(&self, device: NewDevice) -> Result<Device> {
        if let Some(persistent) = &self.persistent {
            let device = persistent.insert(device).await?;
            self.devices.insert(
                device.id.clone(),
                StoredDevice {
                    device: device.clone(),
                    last_signed_at_ms: 0,
                },
            );
            return Ok(device);
        }

        match self.devices.entry(device.id.clone()) {
            Entry::Occupied(_) => Err(HubError::DeviceAlreadyExists(device.id)),
            Entry::Vacant(entry) => {
                let device = Device {
                    id: device.id,
                    public_key: device.public_key,
                    hostname: device.hostname,
                    os: device.os,
                    capabilities: device.capabilities,
                    version: device.version,
                    auth_method: device.auth_method,
                    online: false,
                    external_user_id: device.external_user_id,
                };
                entry.insert(StoredDevice {
                    device: device.clone(),
                    last_signed_at_ms: 0,
                });
                Ok(device)
            }
        }
    }

    async fn get(&self, device_id: &str) -> Result<Option<Device>> {
        if let Some(persistent) = &self.persistent {
            return persistent.get(device_id).await;
        }
        Ok(self
            .devices
            .get(device_id)
            .map(|device| device.device.clone()))
    }

    async fn list(&self) -> Result<Vec<Device>> {
        if let Some(persistent) = &self.persistent {
            return persistent.list().await;
        }
        let mut devices = self
            .devices
            .iter()
            .map(|entry| entry.value().device.clone())
            .collect::<Vec<_>>();
        devices.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(devices)
    }

    async fn delete(&self, device_id: &str) -> Result<()> {
        if let Some(persistent) = &self.persistent {
            persistent.delete(device_id).await?;
        }
        self.devices.remove(device_id);
        Ok(())
    }
}

#[async_trait]
impl DeviceAdminStore for MemoryDeviceStore {
    async fn pre_register(
        &self,
        device_id: &str,
        public_key: &[u8],
        external_user_id: &str,
    ) -> Result<(Device, DateTime<Utc>)> {
        if let Some(persistent) = &self.persistent {
            let (device, registered_at) = persistent
                .pre_register(device_id, public_key, external_user_id)
                .await?;
            self.devices.insert(
                device.id.clone(),
                StoredDevice {
                    device: device.clone(),
                    last_signed_at_ms: 0,
                },
            );
            return Ok((device, registered_at));
        }

        // Memory-backed: mirror the PgDeviceStore semantics exactly so
        // tests that exercise the admin API against an in-memory hub
        // behave identically to production.
        //
        // DashMap::entry() serializes concurrent first-inserts for the same key,
        // preventing the unique-constraint race that the Pg path must retry around.
        match self.devices.entry(device_id.into()) {
            Entry::Occupied(mut entry) => {
                let existing_user_id = entry.get().device.external_user_id.clone();
                // Ownership check: if a row exists and is already claimed by a
                // different external user, reject. A row with external_user_id = None
                // (device inserted via the legacy bootstrap flow) can be claimed by
                // any caller — this is intentional behavior that allows admin
                // pre-registration to adopt unclaimed devices.
                if let Some(existing_user) = existing_user_id.as_ref()
                    && existing_user != external_user_id
                {
                    return Err(HubError::DeviceOwnedByDifferentUser {
                        device_id: device_id.into(),
                        existing_external_user_id: existing_user.clone(),
                    });
                }
                // Update the row: overwrite public_key + external_user_id.
                let stored = entry.get_mut();
                stored.device.public_key = Some(public_key.to_vec());
                stored.device.external_user_id = Some(external_user_id.into());
                // Memory store has no persistent registered_at; use now() as a
                // best-effort stable value (callers using the in-memory backend are
                // in tests only — the real stable timestamp only matters for Postgres).
                Ok((stored.device.clone(), Utc::now()))
            }
            Entry::Vacant(entry) => {
                let registered_at = Utc::now();
                let device = Device {
                    id: device_id.into(),
                    public_key: Some(public_key.to_vec()),
                    hostname: "pending-device".into(),
                    os: "unknown".into(),
                    capabilities: Vec::new(),
                    version: None,
                    auth_method: "preregistered".into(),
                    online: false,
                    external_user_id: Some(external_user_id.into()),
                };
                entry.insert(StoredDevice {
                    device: device.clone(),
                    last_signed_at_ms: 0,
                });
                Ok((device, registered_at))
            }
        }
    }

    async fn find_by_id(&self, device_id: &str) -> Result<Option<Device>> {
        self.get(device_id).await
    }

    async fn delete_device(&self, device_id: &str) -> Result<bool> {
        if let Some(persistent) = &self.persistent {
            let removed = persistent.delete_device(device_id).await?;
            self.devices.remove(device_id);
            return Ok(removed);
        }
        Ok(self.devices.remove(device_id).is_some())
    }

    async fn list_by_external_user(&self, external_user_id: &str) -> Result<Vec<Device>> {
        if let Some(persistent) = &self.persistent {
            return persistent.list_by_external_user(external_user_id).await;
        }
        let mut devices = self
            .devices
            .iter()
            .filter(|entry| {
                entry
                    .value()
                    .device
                    .external_user_id
                    .as_deref()
                    .map(|user| user == external_user_id)
                    .unwrap_or(false)
            })
            .map(|entry| entry.value().device.clone())
            .collect::<Vec<_>>();
        devices.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(devices)
    }
}

#[derive(Default)]
pub struct MemoryJobStore {
    jobs: DashMap<String, Job>,
}

#[async_trait]
impl JobStore for MemoryJobStore {
    async fn insert(&self, job: NewJob) -> Result<Job> {
        let job = Job::new_pending(uuid::Uuid::new_v4(), job, chrono::Utc::now());
        self.jobs.insert(job.id.to_string(), job.clone());
        Ok(job)
    }

    async fn get(&self, job_id: &str) -> Result<Option<Job>> {
        Ok(self.jobs.get(job_id).map(|job| job.clone()))
    }

    async fn list(&self, filter: JobFilter) -> Result<Vec<Job>> {
        let mut jobs = self
            .jobs
            .iter()
            .filter(|entry| {
                let job = entry.value();
                filter
                    .device_id
                    .as_ref()
                    .is_none_or(|device_id| &job.device_id == device_id)
                    && filter.status.is_none_or(|status| job.status == status)
            })
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        jobs.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(jobs)
    }

    async fn transition_status(
        &self,
        job_id: &str,
        status: JobStatus,
    ) -> Result<Option<JobStatus>> {
        let mut job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| HubError::JobNotFound(job_id.into()))?;
        if job.status == status {
            return Ok(None);
        }
        let next_status = job.apply_status_transition(status, chrono::Utc::now())?;
        Ok(Some(next_status))
    }

    async fn update_status(&self, job_id: &str, status: JobStatus) -> Result<()> {
        let _ = self.transition_status(job_id, status).await?;
        Ok(())
    }

    async fn update_terminal(
        &self,
        job_id: &str,
        exit_code: i32,
        error: &str,
        output_summary: &str,
    ) -> Result<()> {
        let mut job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| HubError::JobNotFound(job_id.into()))?;
        job.exit_code = Some(exit_code);
        job.error = Some(error.into());
        job.output_summary = Some(output_summary.into());
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryAuditStore {
    entries: Mutex<Vec<AuditEntry>>,
}

#[async_trait]
impl AuditStore for MemoryAuditStore {
    async fn append(&self, entries: &[AuditEntry]) -> Result<()> {
        self.entries
            .lock()
            .map_err(|err| HubError::Internal(err.to_string()))?
            .extend(entries.iter().cloned());
        Ok(())
    }

    async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>> {
        let entries = self
            .entries
            .lock()
            .map_err(|err| HubError::Internal(err.to_string()))?;
        Ok(filter.apply(entries.iter().cloned()))
    }

    async fn prune_before(&self, cutoff: chrono::DateTime<chrono::Utc>) -> Result<u64> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|err| HubError::Internal(err.to_string()))?;
        let original_len = entries.len();
        entries.retain(|entry| entry.timestamp >= cutoff);
        Ok((original_len - entries.len()) as u64)
    }
}

fn spawn_audit_retention_task(audit_store: Arc<dyn AuditStore>, retention_days: u64) {
    tokio::spawn(async move {
        if let Err(err) = prune_audit_entries(audit_store.as_ref(), retention_days).await {
            tracing::warn!(error = %err, retention_days, "failed to prune audit entries");
        }

        let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        interval.tick().await;

        loop {
            interval.tick().await;
            if let Err(err) = prune_audit_entries(audit_store.as_ref(), retention_days).await {
                tracing::warn!(error = %err, retention_days, "failed to prune audit entries");
            }
        }
    });
}

async fn prune_audit_entries(audit_store: &dyn AuditStore, retention_days: u64) -> Result<u64> {
    let retention_days = std::cmp::min(retention_days, i64::MAX as u64) as i64;
    let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days);
    audit_store.prune_before(cutoff).await
}
