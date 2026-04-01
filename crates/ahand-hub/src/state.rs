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
use dashmap::DashMap;

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
            service_token: Arc::new(config.service_token),
        }
    }

    pub async fn for_tests() -> Self {
        Self::from_config(crate::config::Config::for_tests()).await
    }
}

#[derive(Default)]
pub struct MemoryDeviceStore {
    devices: DashMap<String, Device>,
}

impl MemoryDeviceStore {
    pub fn upsert_from_hello(&self, device_id: &str, hello: &ahand_protocol::Hello) -> Result<Device> {
        let public_key = match hello.auth.as_ref() {
            Some(ahand_protocol::hello::Auth::Ed25519(auth)) => Some(auth.public_key.clone()),
            _ => None,
        };
        let auth_method = match hello.auth.as_ref() {
            Some(ahand_protocol::hello::Auth::Ed25519(_)) => "ed25519",
            Some(ahand_protocol::hello::Auth::BearerToken(_)) => "bearer_token",
            None => "none",
        };
        let device = Device {
            id: device_id.into(),
            public_key,
            hostname: hello.hostname.clone(),
            os: hello.os.clone(),
            capabilities: hello.capabilities.clone(),
            version: Some(hello.version.clone()),
            auth_method: auth_method.into(),
            online: true,
        };
        self.devices.insert(device_id.into(), device.clone());
        Ok(device)
    }

    pub fn mark_offline(&self, device_id: &str) -> Result<()> {
        let mut device = self
            .devices
            .get_mut(device_id)
            .ok_or_else(|| HubError::DeviceNotFound(device_id.into()))?;
        device.online = false;
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
        self.devices.insert(device.id.clone(), device.clone());
        Ok(device)
    }

    async fn get(&self, device_id: &str) -> Result<Option<Device>> {
        Ok(self.devices.get(device_id).map(|device| device.clone()))
    }

    async fn list(&self) -> Result<Vec<Device>> {
        let mut devices = self
            .devices
            .iter()
            .map(|entry| entry.value().clone())
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
