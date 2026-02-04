use std::collections::HashMap;
use std::time::{Duration, Instant};

use ahand_protocol::{JobRequest, RefusalContext, SessionMode, SessionState};
use tokio::sync::Mutex;
use tracing::info;

/// Session-level decision for a job request.
pub enum SessionDecision {
    /// Trust / AutoAccept — proceed immediately.
    Allow,
    /// Inactive or trust expired — reject immediately.
    Deny(String),
    /// Strict mode — suspend and request user approval.
    NeedsApproval {
        reason: String,
        previous_refusals: Vec<RefusalContext>,
    },
}

struct CallerSession {
    mode: SessionMode,
    /// When trust expires (only meaningful for Trust mode).
    trust_expires: Option<Instant>,
    /// Configured trust timeout in minutes.
    trust_timeout_mins: u64,
}

struct RefusalEntry {
    tool: String,
    reason: String,
    /// When this entry expires (refused_at + 24h).
    expires_at: Instant,
    /// Absolute timestamp for the proto message.
    refused_at_ms: u64,
}

pub struct SessionManager {
    sessions: Mutex<HashMap<String, CallerSession>>,
    refusal_log: Mutex<Vec<RefusalEntry>>,
    default_trust_timeout_mins: u64,
}

impl SessionManager {
    pub fn new(default_trust_timeout_mins: u64) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            refusal_log: Mutex::new(Vec::new()),
            default_trust_timeout_mins,
        }
    }

    /// Register a caller with default Inactive mode (no-op if already registered).
    pub async fn register_caller(&self, caller_uid: &str) {
        let mut sessions = self.sessions.lock().await;
        sessions.entry(caller_uid.to_string()).or_insert_with(|| {
            info!(caller_uid, "registering new caller (inactive)");
            CallerSession {
                mode: SessionMode::Inactive,
                trust_expires: None,
                trust_timeout_mins: self.default_trust_timeout_mins,
            }
        });
    }

    /// Evaluate a job request against the caller's session mode.
    pub async fn check(&self, req: &JobRequest, caller_uid: &str) -> SessionDecision {
        let mut sessions = self.sessions.lock().await;

        let session = match sessions.get_mut(caller_uid) {
            Some(s) => s,
            None => {
                // No session exists → Inactive.
                return SessionDecision::Deny("session not activated".to_string());
            }
        };

        match session.mode {
            SessionMode::Inactive => {
                SessionDecision::Deny("session not activated".to_string())
            }
            SessionMode::Strict => {
                // Drop the sessions lock before acquiring refusal_log lock.
                drop(sessions);
                let refusals = self.get_refusals(&req.tool).await;
                SessionDecision::NeedsApproval {
                    reason: format!("strict mode: approval required for {:?}", req.tool),
                    previous_refusals: refusals,
                }
            }
            SessionMode::Trust => {
                if let Some(expires) = session.trust_expires {
                    if Instant::now() >= expires {
                        // Trust expired → revert to Inactive.
                        info!(caller_uid, "trust expired, reverting to inactive");
                        session.mode = SessionMode::Inactive;
                        session.trust_expires = None;
                        return SessionDecision::Deny("trust expired".to_string());
                    }
                    // Reset the inactivity timer on activity.
                    session.trust_expires =
                        Some(Instant::now() + Duration::from_secs(session.trust_timeout_mins * 60));
                }
                SessionDecision::Allow
            }
            SessionMode::AutoAccept => SessionDecision::Allow,
        }
    }

    /// Set the session mode for a caller. Returns the new SessionState.
    pub async fn set_mode(
        &self,
        caller_uid: &str,
        mode: SessionMode,
        trust_timeout_mins: u64,
    ) -> SessionState {
        let timeout = if trust_timeout_mins == 0 {
            self.default_trust_timeout_mins
        } else {
            trust_timeout_mins
        };

        let trust_expires = if mode == SessionMode::Trust {
            Some(Instant::now() + Duration::from_secs(timeout * 60))
        } else {
            None
        };

        let trust_expires_ms = trust_expires
            .map(|exp| {
                let remaining = exp.duration_since(Instant::now());
                now_ms() + remaining.as_millis() as u64
            })
            .unwrap_or(0);

        let session = CallerSession {
            mode,
            trust_expires,
            trust_timeout_mins: timeout,
        };

        info!(
            caller_uid,
            mode = ?mode,
            trust_timeout_mins = timeout,
            "session mode set"
        );

        self.sessions
            .lock()
            .await
            .insert(caller_uid.to_string(), session);

        SessionState {
            caller_uid: caller_uid.to_string(),
            mode: mode.into(),
            trust_expires_ms,
            trust_timeout_mins: timeout,
        }
    }

    /// Record a refusal with reason (stored for 24h).
    pub async fn record_refusal(&self, _caller_uid: &str, tool: &str, reason: &str) {
        let entry = RefusalEntry {
            tool: tool.to_string(),
            reason: reason.to_string(),
            expires_at: Instant::now() + Duration::from_secs(24 * 3600),
            refused_at_ms: now_ms(),
        };
        self.refusal_log.lock().await.push(entry);
    }

    /// Get recent refusals for a specific tool (within 24h).
    pub async fn get_refusals(&self, tool: &str) -> Vec<RefusalContext> {
        let mut log = self.refusal_log.lock().await;
        let now = Instant::now();

        // Prune expired entries.
        log.retain(|e| e.expires_at > now);

        log.iter()
            .filter(|e| e.tool == tool)
            .map(|e| RefusalContext {
                tool: e.tool.clone(),
                reason: e.reason.clone(),
                refused_at_ms: e.refused_at_ms,
            })
            .collect()
    }

    /// Get the current session state for a caller.
    pub async fn get_session_state(&self, caller_uid: &str) -> SessionState {
        let sessions = self.sessions.lock().await;
        match sessions.get(caller_uid) {
            Some(session) => {
                let trust_expires_ms = session
                    .trust_expires
                    .and_then(|exp| {
                        let now = Instant::now();
                        if exp > now {
                            Some(now_ms() + exp.duration_since(now).as_millis() as u64)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);

                SessionState {
                    caller_uid: caller_uid.to_string(),
                    mode: session.mode.into(),
                    trust_expires_ms,
                    trust_timeout_mins: session.trust_timeout_mins,
                }
            }
            None => SessionState {
                caller_uid: caller_uid.to_string(),
                mode: SessionMode::Inactive.into(),
                trust_expires_ms: 0,
                trust_timeout_mins: self.default_trust_timeout_mins,
            },
        }
    }

    /// Get session states for all callers (or a specific one if caller_uid is non-empty).
    pub async fn query_sessions(&self, caller_uid: &str) -> Vec<SessionState> {
        if !caller_uid.is_empty() {
            return vec![self.get_session_state(caller_uid).await];
        }

        let sessions = self.sessions.lock().await;
        sessions
            .keys()
            .map(|uid| {
                let session = &sessions[uid];
                let trust_expires_ms = session
                    .trust_expires
                    .and_then(|exp| {
                        let now = Instant::now();
                        if exp > now {
                            Some(now_ms() + exp.duration_since(now).as_millis() as u64)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);

                SessionState {
                    caller_uid: uid.clone(),
                    mode: session.mode.into(),
                    trust_expires_ms,
                    trust_timeout_mins: session.trust_timeout_mins,
                }
            })
            .collect()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
