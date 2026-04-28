use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use ahand_hub_core::traits::{AbortOnDropHandle, KickSubscription, OutboxStore};
use ahand_hub_core::{HubError, Result};
use async_trait::async_trait;
use tokio::sync::{Mutex, watch};

#[derive(Default)]
struct DeviceState {
    lock: Option<String>,
    seq: u64,
    buffer: VecDeque<(u64, Vec<u8>)>,
    kick_tx: Option<watch::Sender<u64>>,
    kick_count: u64,
}

const MAX_BUFFER: usize = 10_000;

/// Process-local OutboxStore for `StoreConfig::Memory` and unit tests.
/// Mirrors the Redis semantics method-for-method.
#[derive(Default, Clone)]
pub struct InMemoryOutboxStore {
    inner: Arc<Mutex<HashMap<String, DeviceState>>>,
}

impl InMemoryOutboxStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OutboxStore for InMemoryOutboxStore {
    async fn try_acquire_lock(&self, device_id: &str, session_id: &str) -> Result<bool> {
        let mut g = self.inner.lock().await;
        let entry = g.entry(device_id.to_string()).or_default();
        if entry.lock.is_some() {
            return Ok(false);
        }
        entry.lock = Some(session_id.to_string());
        Ok(true)
    }

    async fn kick(&self, device_id: &str, _new_session_id: &str) -> Result<()> {
        let mut g = self.inner.lock().await;
        let entry = g.entry(device_id.to_string()).or_default();
        entry.kick_count = entry.kick_count.wrapping_add(1);
        if entry.kick_tx.is_none() {
            // No subscriber yet — create the channel so the next subscribe
            // sees the bumped value. (For correctness, subscribers expect
            // to see ticks after they subscribe; if no subscriber existed,
            // dropping the tick is fine.)
            let (tx, _rx) = watch::channel(entry.kick_count);
            entry.kick_tx = Some(tx);
        } else if let Some(tx) = &entry.kick_tx {
            let _ = tx.send(entry.kick_count);
        }
        Ok(())
    }

    async fn subscribe_kick(&self, device_id: &str) -> Result<KickSubscription> {
        let mut g = self.inner.lock().await;
        let entry = g.entry(device_id.to_string()).or_default();
        let tx = match &entry.kick_tx {
            Some(tx) => tx.clone(),
            None => {
                let (tx, _rx) = watch::channel(0u64);
                entry.kick_tx = Some(tx.clone());
                tx
            }
        };
        let recv = tx.subscribe();
        // In-memory impl does not need a real background task; spawn a
        // no-op so we have something to wrap in AbortOnDropHandle.
        let join = tokio::spawn(async {});
        Ok(KickSubscription {
            recv,
            _drop_guard: AbortOnDropHandle::new(join),
        })
    }

    async fn renew_lock(&self, device_id: &str, session_id: &str) -> Result<bool> {
        let g = self.inner.lock().await;
        Ok(g.get(device_id)
            .and_then(|e| e.lock.as_ref())
            .map(|owner| owner == session_id)
            .unwrap_or(false))
    }

    async fn release_lock(&self, device_id: &str, session_id: &str) -> Result<()> {
        let mut g = self.inner.lock().await;
        if let Some(entry) = g.get_mut(device_id)
            && entry.lock.as_deref() == Some(session_id)
        {
            entry.lock = None;
        }
        Ok(())
    }

    async fn reconcile_on_hello(
        &self,
        device_id: &str,
        session_id: &str,
        last_ack: u64,
    ) -> Result<u64> {
        let mut g = self.inner.lock().await;
        let entry = g.get_mut(device_id).ok_or(HubError::Unauthorized)?;
        if entry.lock.as_deref() != Some(session_id) {
            return Err(HubError::Unauthorized);
        }
        if last_ack > entry.seq {
            // Bootstrap path
            entry.seq = last_ack;
            entry.buffer.clear();
            return Ok(last_ack);
        }
        // Normal path: trim acked frames
        while let Some((seq, _)) = entry.buffer.front() {
            if *seq <= last_ack {
                entry.buffer.pop_front();
            } else {
                break;
            }
        }
        Ok(entry.seq)
    }

    async fn unacked_frames(&self, device_id: &str, last_ack: u64) -> Result<Vec<Vec<u8>>> {
        let g = self.inner.lock().await;
        Ok(g.get(device_id)
            .map(|e| {
                e.buffer
                    .iter()
                    .filter(|(seq, _)| *seq > last_ack)
                    .map(|(_, bytes)| bytes.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn fenced_incr_seq(&self, device_id: &str, session_id: &str) -> Result<u64> {
        let mut g = self.inner.lock().await;
        let entry = g.get_mut(device_id).ok_or(HubError::Unauthorized)?;
        if entry.lock.as_deref() != Some(session_id) {
            return Err(HubError::Unauthorized);
        }
        entry.seq += 1;
        Ok(entry.seq)
    }

    async fn xadd_frame(
        &self,
        device_id: &str,
        session_id: &str,
        seq: u64,
        frame: Vec<u8>,
    ) -> Result<()> {
        let mut g = self.inner.lock().await;
        let entry = g.get_mut(device_id).ok_or(HubError::Unauthorized)?;
        if entry.lock.as_deref() != Some(session_id) {
            return Err(HubError::Unauthorized);
        }
        entry.buffer.push_back((seq, frame));
        while entry.buffer.len() > MAX_BUFFER {
            entry.buffer.pop_front();
        }
        Ok(())
    }

    async fn observe_ack(&self, device_id: &str, ack: u64) -> Result<()> {
        let mut g = self.inner.lock().await;
        if let Some(entry) = g.get_mut(device_id) {
            // Ignore invalid acks (claim > issued) to protect the legitimate
            // replay buffer from a buggy/compromised client.
            if ack > entry.seq {
                return Ok(());
            }
            while let Some((seq, _)) = entry.buffer.front() {
                if *seq <= ack {
                    entry.buffer.pop_front();
                } else {
                    break;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> InMemoryOutboxStore {
        InMemoryOutboxStore::new()
    }

    #[tokio::test]
    async fn try_acquire_lock_first_succeeds_second_fails() {
        let s = store();
        assert!(s.try_acquire_lock("dev", "sess-a").await.unwrap());
        assert!(!s.try_acquire_lock("dev", "sess-b").await.unwrap());
    }

    #[tokio::test]
    async fn release_then_reacquire_succeeds() {
        let s = store();
        assert!(s.try_acquire_lock("dev", "sess-a").await.unwrap());
        s.release_lock("dev", "sess-a").await.unwrap();
        assert!(s.try_acquire_lock("dev", "sess-b").await.unwrap());
    }

    #[tokio::test]
    async fn release_with_wrong_session_is_noop() {
        let s = store();
        assert!(s.try_acquire_lock("dev", "sess-a").await.unwrap());
        s.release_lock("dev", "sess-other").await.unwrap();
        assert!(!s.try_acquire_lock("dev", "sess-b").await.unwrap());
    }

    #[tokio::test]
    async fn renew_lock_succeeds_for_owner_fails_for_other() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        assert!(s.renew_lock("dev", "sess-a").await.unwrap());
        assert!(!s.renew_lock("dev", "sess-b").await.unwrap());
    }

    #[tokio::test]
    async fn fenced_incr_seq_increments_per_owner() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        assert_eq!(s.fenced_incr_seq("dev", "sess-a").await.unwrap(), 1);
        assert_eq!(s.fenced_incr_seq("dev", "sess-a").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn fenced_incr_seq_rejects_non_owner() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        let err = s.fenced_incr_seq("dev", "sess-b").await.unwrap_err();
        assert!(matches!(err, HubError::Unauthorized));
    }

    #[tokio::test]
    async fn xadd_frame_stores_for_replay() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
        s.xadd_frame("dev", "sess-a", seq, b"hello".to_vec())
            .await
            .unwrap();
        let frames = s.unacked_frames("dev", 0).await.unwrap();
        assert_eq!(frames, vec![b"hello".to_vec()]);
    }

    #[tokio::test]
    async fn unacked_frames_returns_only_after_last_ack() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        for i in 1..=3u8 {
            let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
            s.xadd_frame("dev", "sess-a", seq, vec![i]).await.unwrap();
        }
        let frames = s.unacked_frames("dev", 1).await.unwrap();
        assert_eq!(frames, vec![vec![2u8], vec![3u8]]);
    }

    #[tokio::test]
    async fn observe_ack_trims_buffer() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        for i in 1..=3u8 {
            let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
            s.xadd_frame("dev", "sess-a", seq, vec![i]).await.unwrap();
        }
        s.observe_ack("dev", 2).await.unwrap();
        let frames = s.unacked_frames("dev", 0).await.unwrap();
        assert_eq!(frames, vec![vec![3u8]]);
    }

    #[tokio::test]
    async fn observe_ack_ignores_invalid_ack_above_issued_seq() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        // Issue one frame, then send a bogus ack that exceeds the issued seq.
        let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
        s.xadd_frame("dev", "sess-a", seq, vec![1u8]).await.unwrap();
        s.observe_ack("dev", 99).await.unwrap();
        // The legitimate seq=1 frame must remain in the replay buffer.
        let frames = s.unacked_frames("dev", 0).await.unwrap();
        assert_eq!(frames, vec![vec![1u8]]);
    }

    #[tokio::test]
    async fn reconcile_normal_path_returns_current_seq() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        for _ in 0..5 {
            let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
            s.xadd_frame("dev", "sess-a", seq, vec![]).await.unwrap();
        }
        let current = s.reconcile_on_hello("dev", "sess-a", 3).await.unwrap();
        assert_eq!(current, 5);
        let frames = s.unacked_frames("dev", 0).await.unwrap();
        assert_eq!(frames.len(), 2); // 1..=3 trimmed; 4..=5 remain
    }

    #[tokio::test]
    async fn reconcile_bootstrap_path_seeds_seq_and_clears_buffer() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        // Fresh store, last_ack=9 (the wedged-device case)
        let returned = s.reconcile_on_hello("dev", "sess-a", 9).await.unwrap();
        assert_eq!(returned, 9);
        // Next incr should produce 10
        assert_eq!(s.fenced_incr_seq("dev", "sess-a").await.unwrap(), 10);
        // No frames to replay
        assert!(s.unacked_frames("dev", 9).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn kick_subscriber_fires_on_publish() {
        let s = store();
        let mut sub = s.subscribe_kick("dev").await.unwrap();
        assert_eq!(*sub.recv.borrow_and_update(), 0u64);
        s.kick("dev", "new-sess").await.unwrap();
        // The watch::Receiver should observe a change.
        sub.recv.changed().await.unwrap();
        assert!(*sub.recv.borrow() >= 1);
    }

    #[tokio::test]
    async fn maxlen_caps_buffer() {
        let s = store();
        s.try_acquire_lock("dev", "sess-a").await.unwrap();
        for _ in 0..(MAX_BUFFER + 5) {
            let seq = s.fenced_incr_seq("dev", "sess-a").await.unwrap();
            s.xadd_frame("dev", "sess-a", seq, vec![]).await.unwrap();
        }
        let frames = s.unacked_frames("dev", 0).await.unwrap();
        assert_eq!(frames.len(), MAX_BUFFER);
    }
}
