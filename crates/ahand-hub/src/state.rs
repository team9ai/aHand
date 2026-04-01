use std::sync::{Arc, Mutex};

use ahand_hub_core::audit::{AuditEntry, AuditFilter};
use ahand_hub_core::auth::AuthService;
use ahand_hub_core::device::{Device, NewDevice};
use ahand_hub_core::job::{Job, JobFilter, JobStatus, NewJob};
use ahand_hub_core::services::device_manager::DeviceManager;
use ahand_hub_core::services::job_dispatcher::JobDispatcher;
use ahand_hub_core::traits::{AuditStore, DeviceStore, JobStore};
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use dashmap::{mapref::entry::Entry, DashMap};
use ed25519_dalek::SigningKey;

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthService>,
    pub device_manager: Arc<DeviceManager>,
    pub job_dispatcher: Arc<JobDispatcher>,
    pub devices: Arc<MemoryDeviceStore>,
    pub jobs_store: Arc<MemoryJobStore>,
    pub audit_store: Arc<MemoryAuditStore>,
    pub jobs: Arc<crate::http::jobs::JobRuntime>,
    pub connections: Arc<crate::ws::device_gateway::ConnectionRegistry>,
    pub events: Arc<crate::events::EventBus>,
    pub output_stream: Arc<crate::output_stream::OutputStream>,
    pub device_bootstrap_token: Arc<String>,
    pub device_bootstrap_device_id: Arc<String>,
    pub device_hello_max_age_ms: u64,
    pub service_token: Arc<String>,
}

impl AppState {
    pub async fn from_config(config: crate::config::Config) -> Self {
        let devices = Arc::new(MemoryDeviceStore::default());
        let jobs_store = Arc::new(MemoryJobStore::default());
        let audit_store = Arc::new(MemoryAuditStore::default());
        let output_stream = Arc::new(crate::output_stream::OutputStream::default());
        let connections = Arc::new(crate::ws::device_gateway::ConnectionRegistry::default());
        let events = Arc::new(crate::events::EventBus::new(audit_store.clone()));
        let device_manager = Arc::new(DeviceManager::new(devices.clone()));
        let job_dispatcher = Arc::new(JobDispatcher::new(
            devices.clone(),
            jobs_store.clone(),
            audit_store.clone(),
        ));
        let jobs = Arc::new(crate::http::jobs::JobRuntime::new(
            job_dispatcher.clone(),
            jobs_store.clone(),
            connections.clone(),
            output_stream.clone(),
        ));

        Self {
            auth: Arc::new(AuthService::new_for_tests(&config.jwt_secret)),
            device_manager,
            job_dispatcher,
            devices,
            jobs_store,
            audit_store,
            jobs,
            connections,
            events,
            output_stream,
            device_bootstrap_token: Arc::new(config.device_bootstrap_token),
            device_bootstrap_device_id: Arc::new(config.device_bootstrap_device_id),
            device_hello_max_age_ms: config.device_hello_max_age_ms,
            service_token: Arc::new(config.service_token),
        }
    }

    pub async fn for_tests() -> Self {
        let state = Self::from_config(crate::config::Config::for_tests()).await;
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        state
            .devices
            .seed_registered_device("device-1", signing_key.verifying_key().to_bytes().to_vec());
        state
    }
}

#[derive(Default)]
pub struct MemoryDeviceStore {
    devices: DashMap<String, StoredDevice>,
}

#[derive(Clone)]
struct StoredDevice {
    device: Device,
    last_signed_at_ms: u64,
}

impl MemoryDeviceStore {
    pub fn seed_registered_device(&self, device_id: &str, public_key: Vec<u8>) {
        self.devices.insert(
            device_id.into(),
            StoredDevice {
                device: Device {
                    id: device_id.into(),
                    public_key: Some(public_key),
                    hostname: "seeded-device".into(),
                    os: "linux".into(),
                    capabilities: vec!["exec".into()],
                    version: Some("0.1.2".into()),
                    auth_method: "ed25519".into(),
                    online: false,
                },
                last_signed_at_ms: 0,
            },
        );
    }

    pub fn accept_verified_hello(
        &self,
        device_id: &str,
        hello: &ahand_protocol::Hello,
        verified: &crate::auth::VerifiedDeviceHello,
    ) -> Result<Device> {
        match self.devices.entry(device_id.into()) {
            Entry::Occupied(mut entry) => {
                let stored = entry.get_mut();
                match stored.device.public_key.as_ref() {
                    Some(existing_key) if existing_key == &verified.public_key => {}
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
                stored.device.online = true;
                stored.last_signed_at_ms = verified.signed_at_ms;

                Ok(stored.device.clone())
            }
            Entry::Vacant(entry) => {
                if !verified.allow_registration {
                    return Err(HubError::Unauthorized);
                }

                let device = Device {
                    id: device_id.into(),
                    public_key: Some(verified.public_key.clone()),
                    hostname: hello.hostname.clone(),
                    os: hello.os.clone(),
                    capabilities: hello.capabilities.clone(),
                    version: Some(hello.version.clone()),
                    auth_method: verified.auth_method.into(),
                    online: true,
                };
                entry.insert(StoredDevice {
                    device: device.clone(),
                    last_signed_at_ms: verified.signed_at_ms,
                });
                Ok(device)
            }
        }
    }

    pub fn mark_offline(&self, device_id: &str) -> Result<()> {
        let mut device = self
            .devices
            .get_mut(device_id)
            .ok_or_else(|| HubError::DeviceNotFound(device_id.into()))?;
        device.device.online = false;
        Ok(())
    }
}

#[async_trait]
impl DeviceStore for MemoryDeviceStore {
    async fn insert(&self, device: NewDevice) -> Result<Device> {
        let device = Device {
            id: device.id,
            public_key: device.public_key,
            hostname: device.hostname,
            os: device.os,
            capabilities: device.capabilities,
            version: device.version,
            auth_method: device.auth_method,
            online: true,
        };
        self.devices.insert(
            device.id.clone(),
            StoredDevice {
                device: device.clone(),
                last_signed_at_ms: 0,
            },
        );
        Ok(device)
    }

    async fn get(&self, device_id: &str) -> Result<Option<Device>> {
        Ok(self
            .devices
            .get(device_id)
            .map(|device| device.device.clone()))
    }

    async fn list(&self) -> Result<Vec<Device>> {
        let mut devices = self
            .devices
            .iter()
            .map(|entry| entry.value().device.clone())
            .collect::<Vec<_>>();
        devices.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(devices)
    }

    async fn delete(&self, device_id: &str) -> Result<()> {
        self.devices.remove(device_id);
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryJobStore {
    jobs: DashMap<String, Job>,
}

#[async_trait]
impl JobStore for MemoryJobStore {
    async fn insert(&self, job: NewJob) -> Result<Job> {
        let job = Job {
            id: uuid::Uuid::new_v4(),
            device_id: job.device_id,
            tool: job.tool,
            args: job.args,
            cwd: job.cwd,
            env: job.env,
            timeout_ms: job.timeout_ms,
            status: JobStatus::Pending,
            requested_by: job.requested_by,
        };
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
        jobs.sort_by_key(|job| job.id);
        Ok(jobs)
    }

    async fn update_status(&self, job_id: &str, status: JobStatus) -> Result<()> {
        let mut job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| HubError::JobNotFound(job_id.into()))?;
        job.status = status;
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
        Ok(entries
            .iter()
            .filter(|entry| {
                filter
                    .resource_type
                    .as_ref()
                    .is_none_or(|resource_type| &entry.resource_type == resource_type)
                    && filter
                        .resource_id
                        .as_ref()
                        .is_none_or(|resource_id| &entry.resource_id == resource_id)
                    && filter
                        .action
                        .as_ref()
                        .is_none_or(|action| &entry.action == action)
            })
            .cloned()
            .collect())
    }
}
