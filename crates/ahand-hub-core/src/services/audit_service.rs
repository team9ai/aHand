use std::sync::Arc;

use crate::Result;
use crate::audit::{AuditEntry, AuditFilter};
use crate::traits::AuditStore;

pub struct AuditService {
    audit: Arc<dyn AuditStore>,
}

impl AuditService {
    pub fn new(audit: Arc<dyn AuditStore>) -> Self {
        Self { audit }
    }

    pub async fn append(&self, entry: AuditEntry) -> Result<()> {
        self.audit.append(&[entry]).await
    }

    pub async fn query(&self, filter: AuditFilter) -> Result<Vec<AuditEntry>> {
        self.audit.query(filter).await
    }
}
