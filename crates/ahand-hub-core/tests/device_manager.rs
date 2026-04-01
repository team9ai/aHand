use ahand_hub_core::services::device_manager::DeviceManager;

#[tokio::test]
async fn list_devices_returns_store_snapshot() {
    let manager = DeviceManager::for_tests();

    let devices = manager.list_devices().await.unwrap();

    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, "device-1");
    assert!(!devices[0].online);
}
