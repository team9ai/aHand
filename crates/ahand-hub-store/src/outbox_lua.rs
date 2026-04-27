//! Lua scripts for the per-device outbox protocol on the hub side.
//!
//! All scripts are loaded once at construction (`SCRIPT LOAD`) and invoked
//! via `EVALSHA`. On `NOSCRIPT` (Redis restart, FLUSHALL), `redis::Script`
//! transparently falls back to `EVAL` and re-caches the SHA. Callers do
//! not need to handle script loading or SHA management.
//!
//! The scripts are designed to be the unit of atomicity. The Rust caller
//! is responsible for ordering the two-step send (`fenced_incr_seq` →
//! encode envelope with assigned seq → `fenced_xadd`); see the design doc
//! for the rationale.

use redis::Script;

pub const ACQUIRE_LOCK: &str = r#"
-- KEYS[1] = lock:device:{id}
-- ARGV[1] = session_id
-- ARGV[2] = ttl_secs
local ok = redis.call('SET', KEYS[1], ARGV[1], 'NX', 'EX', ARGV[2])
if ok then return 1 else return 0 end
"#;

pub const RENEW_LOCK: &str = r#"
-- KEYS[1] = lock:device:{id}
-- ARGV[1] = session_id
-- ARGV[2] = ttl_secs
if redis.call('GET', KEYS[1]) == ARGV[1] then
  redis.call('EXPIRE', KEYS[1], ARGV[2])
  return 1
end
return 0
"#;

pub const RELEASE_LOCK: &str = r#"
-- KEYS[1] = lock:device:{id}
-- ARGV[1] = session_id
if redis.call('GET', KEYS[1]) == ARGV[1] then
  return redis.call('DEL', KEYS[1])
end
return 0
"#;

pub const RECONCILE_ON_HELLO: &str = r#"
-- KEYS[1] = lock:device:{id}
-- KEYS[2] = seq:{id}
-- KEYS[3] = outbox:{id}
-- ARGV[1] = session_id
-- ARGV[2] = last_ack (decimal)
-- ARGV[3] = retention_secs
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
  return redis.error_reply('NOT_OWNER')
end
local current = tonumber(redis.call('GET', KEYS[2])) or 0
local last_ack = tonumber(ARGV[2])
if last_ack > current then
  redis.call('SET', KEYS[2], last_ack)
  redis.call('DEL', KEYS[3])
  redis.call('EXPIRE', KEYS[2], ARGV[3])
  return last_ack
end
if last_ack > 0 then
  redis.call('XTRIM', KEYS[3], 'MINID', '0-' .. (last_ack + 1))
end
return current
"#;

pub const FENCED_INCR_SEQ: &str = r#"
-- KEYS[1] = lock:device:{id}
-- KEYS[2] = seq:{id}
-- ARGV[1] = session_id
-- ARGV[2] = retention_secs
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
  return redis.error_reply('NOT_OWNER')
end
local seq = redis.call('INCR', KEYS[2])
redis.call('EXPIRE', KEYS[2], ARGV[2])
return seq
"#;

pub const FENCED_XADD: &str = r#"
-- KEYS[1] = lock:device:{id}
-- KEYS[2] = outbox:{id}
-- ARGV[1] = session_id
-- ARGV[2] = seq (decimal)
-- ARGV[3] = frame (binary)
-- ARGV[4] = max_buffer (decimal)
-- ARGV[5] = retention_secs
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
  return redis.error_reply('NOT_OWNER')
end
local id = '0-' .. ARGV[2]
redis.call('XADD', KEYS[2], 'MAXLEN', '~', ARGV[4], id, 'frame', ARGV[3])
redis.call('EXPIRE', KEYS[2], ARGV[5])
return 1
"#;

/// Pre-built [`redis::Script`] handles. `redis::Script` itself caches the
/// SHA1 internally and uses `EVALSHA`-then-`EVAL` automatically, so this
/// type is mostly a named bundle so callers do not have to repeat the raw
/// strings everywhere.
pub struct OutboxScripts {
    pub acquire_lock: Script,
    pub renew_lock: Script,
    pub release_lock: Script,
    pub reconcile_on_hello: Script,
    pub fenced_incr_seq: Script,
    pub fenced_xadd: Script,
}

impl OutboxScripts {
    pub fn load() -> Self {
        Self {
            acquire_lock: Script::new(ACQUIRE_LOCK),
            renew_lock: Script::new(RENEW_LOCK),
            release_lock: Script::new(RELEASE_LOCK),
            reconcile_on_hello: Script::new(RECONCILE_ON_HELLO),
            fenced_incr_seq: Script::new(FENCED_INCR_SEQ),
            fenced_xadd: Script::new(FENCED_XADD),
        }
    }
}
