use ahand_protocol::JobRequest;
use crate::config::PolicyConfig;

pub struct PolicyChecker {
    allowed_tools: Vec<String>,
    denied_paths: Vec<String>,
}

impl PolicyChecker {
    pub fn new(config: &PolicyConfig) -> Self {
        Self {
            allowed_tools: config.allowed_tools.clone(),
            denied_paths: config.denied_paths.clone(),
        }
    }

    /// Returns `Ok(())` if the job is allowed, or `Err(reason)` if rejected.
    pub fn check(&self, req: &JobRequest) -> Result<(), String> {
        // Check tool allowlist.
        if !self.allowed_tools.is_empty() && !self.allowed_tools.contains(&req.tool) {
            return Err(format!(
                "tool {:?} is not in the allow list",
                req.tool
            ));
        }

        // Check denied working directories.
        if !req.cwd.is_empty() {
            for denied in &self.denied_paths {
                if req.cwd.starts_with(denied) {
                    return Err(format!(
                        "working directory {:?} is denied by policy",
                        req.cwd
                    ));
                }
            }
        }

        Ok(())
    }
}
