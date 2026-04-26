//! Smoke test for `ahandd browser-doctor`.
//!
//! This test doesn't assert specific output — it only verifies the command
//! exits cleanly with either 0 (all checks pass) or 1 (some checks fail).
//! Any other outcome (panic, non-zero/non-one exit, hang) is a bug.

use std::process::Command;
use std::time::Duration;

#[test]
fn browser_doctor_exits_with_zero_or_one() {
    // `CARGO_BIN_EXE_ahandd` is an absolute path to the test binary that
    // Cargo automatically builds and provides for integration tests.
    let bin = env!("CARGO_BIN_EXE_ahandd");

    let mut child = Command::new(bin)
        .arg("browser-doctor")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ahandd browser-doctor");

    // Wait up to 10 seconds — doctor shouldn't take anywhere near this long.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                let code = status.code().unwrap_or(-1);
                assert!(
                    code == 0 || code == 1,
                    "browser-doctor returned unexpected exit code: {code}"
                );
                return;
            }
            None => {
                if start.elapsed() > Duration::from_secs(10) {
                    let _ = child.kill();
                    panic!("browser-doctor took more than 10s to finish");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}
