//! End-to-end regression tests for hub outbox persistence.
//!
//! These tests boot a real Redis container via testcontainers, exercise
//! `RedisOutboxStore` and `ConnectionRegistry` together, and verify the
//! key invariants from the design spec.

use std::sync::Arc;
use std::time::Duration;

use ahand_hub::ws::device_gateway::ConnectionRegistry;
use ahand_hub_core::traits::OutboxStore;
use ahand_hub_store::outbox_store::RedisOutboxStore;
use testcontainers::{
    ContainerAsync, GenericImage,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

async fn redis_container() -> anyhow::Result<(ContainerAsync<GenericImage>, String)> {
    let container = GenericImage::new("redis", "7-alpine")
        .with_exposed_port(6379.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
        .start()
        .await?;
    let port = container.get_host_port_ipv4(6379.tcp()).await?;
    Ok((container, format!("redis://127.0.0.1:{port}")))
}

#[tokio::test]
async fn replay_after_simulated_hub_restart() -> anyhow::Result<()> {
    let (_redis, url) = redis_container().await?;

    // Phase 1: hub instance A handles a session for dev-1 and sends 5 frames.
    let store_a = Arc::new(RedisOutboxStore::new(&url).await?) as Arc<dyn OutboxStore>;
    let registry_a = ConnectionRegistry::new(store_a.clone());
    let (conn_a, mut rx_a, _close_a) = registry_a.register("dev-1".into(), 0).await?;
    for _ in 0..5 {
        let envelope = ahand_protocol::Envelope {
            device_id: "dev-1".into(),
            ..Default::default()
        };
        registry_a.send_envelope("dev-1", envelope).await?;
        let _ = rx_a.recv().await.expect("frame delivered");
    }
    // Device acked frames 1 and 2 before A "dies".
    registry_a.observe_ack("dev-1", 2).await?;

    // Simulate a graceful hub shutdown: unregister releases the lock
    // and aborts background tasks. (A sudden crash would leave the
    // Redis lock held until its 30s TTL expired; we rely on graceful
    // SIGTERM behavior in production. The durability invariant we are
    // testing is: messages survive process boundary, regardless of
    // shutdown style.)
    registry_a.unregister("dev-1", conn_a).await?;
    drop(registry_a);
    drop(store_a);

    // Phase 2: hub instance B starts up and the device reconnects with last_ack=2.
    let store_b = Arc::new(RedisOutboxStore::new(&url).await?) as Arc<dyn OutboxStore>;
    let registry_b = ConnectionRegistry::new(store_b);
    let (_conn_b, mut rx_b, _close_b) = registry_b.register("dev-1".into(), 2).await?;

    // Frames 3..=5 should replay.
    let mut replayed = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(200), rx_b.recv()).await {
        replayed += 1;
    }
    assert_eq!(replayed, 3, "expected 3 frames (seq 3..=5) to replay");
    Ok(())
}

#[tokio::test]
async fn lock_takeover_via_kick() -> anyhow::Result<()> {
    let (_redis, url) = redis_container().await?;
    let store_a = Arc::new(RedisOutboxStore::new(&url).await?) as Arc<dyn OutboxStore>;
    let store_b = Arc::new(RedisOutboxStore::new(&url).await?) as Arc<dyn OutboxStore>;
    let registry_a = Arc::new(ConnectionRegistry::new(store_a.clone()));
    let registry_b = ConnectionRegistry::new(store_b.clone());

    let (conn_a, _rx_a, mut close_a) = registry_a.register("dev-2".into(), 0).await?;

    // In production, the WS handler watches close_rx and on close runs
    // the unregister teardown (which calls release_lock). Simulate that
    // by spawning a task that bridges close_a → unregister.
    let cleanup_registry = registry_a.clone();
    let cleanup = tokio::spawn(async move {
        let _ = close_a.changed().await;
        cleanup_registry
            .unregister("dev-2", conn_a)
            .await
            .expect("unregister after kick");
    });

    // B's register should kick A; A's kick subscriber fires close_a;
    // the cleanup task above unregisters A; B's retry succeeds.
    let started = tokio::time::Instant::now();
    let (_conn_b, _rx_b, _close_b) = registry_b.register("dev-2".into(), 0).await?;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "kick-and-acquire took {:?}, expected <5s",
        started.elapsed()
    );
    cleanup.await?;
    Ok(())
}

#[tokio::test]
async fn bootstrap_path_unblocks_wedged_device() -> anyhow::Result<()> {
    let (_redis, url) = redis_container().await?;

    // Fresh Redis (= just-deployed hub). Device sends Hello with last_ack=9
    // (the wedged-after-restart case from the original incident).
    let store = Arc::new(RedisOutboxStore::new(&url).await?) as Arc<dyn OutboxStore>;
    let registry = ConnectionRegistry::new(store.clone());
    let (_conn, mut rx, _close) = registry.register("dev-3".into(), 9).await?;

    // No frames to replay (nothing was in the stream).
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "no replay expected"
    );

    // Next send should produce seq=10.
    let envelope = ahand_protocol::Envelope {
        device_id: "dev-3".into(),
        ..Default::default()
    };
    registry.send_envelope("dev-3", envelope).await?;
    let frame = rx.recv().await.expect("frame delivered");
    let decoded = <ahand_protocol::Envelope as prost::Message>::decode(frame.as_slice())?;
    assert_eq!(decoded.seq, 10);
    Ok(())
}

#[tokio::test]
async fn original_incident_regression() -> anyhow::Result<()> {
    // Keystone test: had it existed pre-incident, the InvalidPeerAck
    // wedge bug would not have shipped. Simulates hub restart with the
    // device still holding a non-zero last_ack.
    let (_redis, url) = redis_container().await?;

    // Phase 1: device connects, receives 5 frames, acks them all.
    let store_a = Arc::new(RedisOutboxStore::new(&url).await?) as Arc<dyn OutboxStore>;
    let registry_a = ConnectionRegistry::new(store_a.clone());
    let (conn_a, mut rx_a, _close_a) = registry_a.register("dev-incident".into(), 0).await?;
    for _ in 0..5 {
        registry_a
            .send_envelope(
                "dev-incident",
                ahand_protocol::Envelope {
                    device_id: "dev-incident".into(),
                    ..Default::default()
                },
            )
            .await?;
        rx_a.recv().await.expect("frame delivered");
    }
    registry_a.observe_ack("dev-incident", 5).await?;
    registry_a.unregister("dev-incident", conn_a).await?;

    // Phase 2: hub deploys — B is a fresh process holding a different
    // OutboxStore arc but pointing at the same Redis.
    let store_b = Arc::new(RedisOutboxStore::new(&url).await?) as Arc<dyn OutboxStore>;
    let registry_b = ConnectionRegistry::new(store_b);

    // Device reconnects with last_ack=5. With the fix, register succeeds;
    // without the fix this would have surfaced as InvalidPeerAck → close
    // → daemon Broken pipe.
    let (_conn_b, _rx_b, _close_b) = registry_b
        .register("dev-incident".into(), 5)
        .await
        .expect("device should reconnect cleanly post-restart");
    Ok(())
}
