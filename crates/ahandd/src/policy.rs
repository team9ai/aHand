use std::collections::{HashMap, HashSet};

use ahand_protocol::{JobRequest, PolicyState, PolicyUpdate};
use tokio::sync::{Mutex, RwLock};
use url::Url;

use crate::config::PolicyConfig;

/// Three-way policy decision.
pub enum PolicyDecision {
    /// Tool and all detected domains are allowed — proceed immediately.
    Allow,
    /// Explicitly denied — reject immediately, no approval opportunity.
    Deny(String),
    /// Not explicitly allowed — suspend and request user approval.
    NeedsApproval {
        reason: String,
        detected_domains: Vec<String>,
    },
}

pub struct PolicyChecker {
    config: RwLock<PolicyConfig>,
    /// Per-user session approvals: caller_uid -> set of approved tool/domain keys.
    session_approvals: Mutex<HashMap<String, HashSet<String>>>,
}

impl PolicyChecker {
    pub fn new(config: &PolicyConfig) -> Self {
        Self {
            config: RwLock::new(config.clone()),
            session_approvals: Mutex::new(HashMap::new()),
        }
    }

    /// Evaluate a job request against the current policy.
    pub async fn check(&self, req: &JobRequest, caller_uid: &str) -> PolicyDecision {
        let cfg = self.config.read().await;

        // 1. Denied tools — hard reject.
        if cfg.denied_tools.contains(&req.tool) {
            return PolicyDecision::Deny(format!(
                "tool {:?} is in the deny list",
                req.tool
            ));
        }

        // 2. Denied paths — hard reject.
        if !req.cwd.is_empty() {
            for denied in &cfg.denied_paths {
                if req.cwd.starts_with(denied) {
                    return PolicyDecision::Deny(format!(
                        "working directory {:?} is denied by policy",
                        req.cwd
                    ));
                }
            }
        }

        // 3. Extract domains from network tool arguments.
        let detected_domains = extract_domains(&req.tool, &req.args);

        // 4. Check per-user session memory.
        let (tool_remembered, remembered_domains) = {
            let session = self.session_approvals.lock().await;
            if let Some(approvals) = session.get(caller_uid) {
                let tr = approvals.contains(&format!("tool:{}", req.tool));
                let rd: HashSet<String> = detected_domains
                    .iter()
                    .filter(|d| approvals.contains(&format!("domain:{d}")))
                    .cloned()
                    .collect();
                (tr, rd)
            } else {
                (false, HashSet::new())
            }
        };

        // 5. Tool allowlist check.
        let tool_allowed = cfg.allowed_tools.is_empty()
            || cfg.allowed_tools.contains(&req.tool)
            || tool_remembered;

        if !tool_allowed {
            return PolicyDecision::NeedsApproval {
                reason: format!("tool {:?} is not in the allow list", req.tool),
                detected_domains,
            };
        }

        // 6. Domain allowlist check — only if domains were detected.
        if !detected_domains.is_empty() && !cfg.allowed_domains.is_empty() {
            let unapproved: Vec<String> = detected_domains
                .iter()
                .filter(|d| {
                    !cfg.allowed_domains.contains(d) && !remembered_domains.contains(*d)
                })
                .cloned()
                .collect();

            if !unapproved.is_empty() {
                return PolicyDecision::NeedsApproval {
                    reason: format!(
                        "domain(s) {} not in allowed domains",
                        unapproved.join(", ")
                    ),
                    detected_domains,
                };
            }
        }

        PolicyDecision::Allow
    }

    /// Record an approval in session memory for a specific user.
    pub async fn remember_approval(&self, caller_uid: &str, tool: &str, domains: &[String]) {
        let mut session = self.session_approvals.lock().await;
        let set = session
            .entry(caller_uid.to_string())
            .or_insert_with(HashSet::new);
        set.insert(format!("tool:{tool}"));
        for d in domains {
            set.insert(format!("domain:{d}"));
        }
    }

    /// Return a snapshot of the current policy as a proto PolicyState.
    pub async fn get_state(&self) -> PolicyState {
        let cfg = self.config.read().await;
        PolicyState {
            allowed_tools: cfg.allowed_tools.clone(),
            denied_tools: cfg.denied_tools.clone(),
            denied_paths: cfg.denied_paths.clone(),
            allowed_domains: cfg.allowed_domains.clone(),
            approval_timeout_secs: cfg.approval_timeout_secs,
        }
    }

    /// Apply an incremental update to the policy.
    pub async fn apply_update(&self, update: &PolicyUpdate) {
        let mut cfg = self.config.write().await;

        apply_list_update(&mut cfg.allowed_tools, &update.add_allowed_tools, &update.remove_allowed_tools);
        apply_list_update(&mut cfg.denied_tools, &update.add_denied_tools, &update.remove_denied_tools);
        apply_list_update(&mut cfg.denied_paths, &update.add_denied_paths, &update.remove_denied_paths);
        apply_list_update(&mut cfg.allowed_domains, &update.add_allowed_domains, &update.remove_allowed_domains);

        if update.approval_timeout_secs > 0 {
            cfg.approval_timeout_secs = update.approval_timeout_secs;
        }
    }

    /// Get a clone of the current PolicyConfig (for persisting to file).
    pub async fn config_snapshot(&self) -> PolicyConfig {
        self.config.read().await.clone()
    }

    /// Get the current approval timeout in seconds.
    #[allow(dead_code)]
    pub async fn approval_timeout_secs(&self) -> u64 {
        self.config.read().await.approval_timeout_secs
    }

}

/// Apply add/remove operations to a list, deduplicating.
fn apply_list_update(list: &mut Vec<String>, add: &[String], remove: &[String]) {
    // Remove first.
    list.retain(|item| !remove.contains(item));
    // Add, avoiding duplicates.
    for item in add {
        if !list.contains(item) {
            list.push(item.clone());
        }
    }
}

// ── Domain heuristic extraction ─────────────────────────────────────────

/// Tools known to make network connections.
const NETWORK_TOOLS: &[&str] = &[
    "curl", "wget", "git", "ssh", "scp", "rsync", "sftp",
    "nc", "ncat", "nmap", "ping", "dig", "nslookup",
    "http", "https", "fetch",
];

/// Extract domain names from tool arguments using heuristics.
pub fn extract_domains(tool: &str, args: &[String]) -> Vec<String> {
    // Only inspect args if the tool is a known network tool.
    let base = tool.rsplit('/').next().unwrap_or(tool);
    if !NETWORK_TOOLS.contains(&base) {
        return Vec::new();
    }

    let mut domains = Vec::new();

    for arg in args {
        // Skip flags (start with -).
        if arg.starts_with('-') {
            continue;
        }

        // Try parsing as a URL.
        if let Some(host) = try_extract_url_host(arg) {
            if !domains.contains(&host) {
                domains.push(host);
            }
            continue;
        }

        // Try user@host:path (git/scp SSH pattern).
        if let Some(host) = try_extract_ssh_host(arg) {
            if !domains.contains(&host) {
                domains.push(host);
            }
            continue;
        }

        // For ssh/ping/dig/nslookup: bare hostname argument (no slashes, contains a dot).
        if matches!(base, "ssh" | "ping" | "dig" | "nslookup" | "nc" | "ncat")
            && !arg.contains('/')
            && arg.contains('.')
        {
            let host = arg.split(':').next().unwrap_or(arg).to_string();
            if !domains.contains(&host) {
                domains.push(host);
            }
        }
    }

    domains
}

/// Try to parse a string as a URL and extract the host.
fn try_extract_url_host(s: &str) -> Option<String> {
    let url = Url::parse(s).ok()?;
    url.host_str().map(|h| h.to_string())
}

/// Try to extract host from user@host:path or host:path patterns.
fn try_extract_ssh_host(s: &str) -> Option<String> {
    // Pattern: user@host:path or user@host
    if let Some(at_pos) = s.find('@') {
        let after_at = &s[at_pos + 1..];
        let host = after_at.split(':').next().unwrap_or(after_at);
        if !host.is_empty() && host.contains('.') {
            return Some(host.to_string());
        }
    }
    None
}
