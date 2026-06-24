//! Windows sandbox setup orchestration helpers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::network::WindowsNetworkMode;
use crate::sandbox::types::{SandboxError, SandboxResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WindowsNetworkContext {
    pub(super) mode: WindowsNetworkMode,
    pub(super) state_root: PathBuf,
    pub(super) requires_hard_block: bool,
}

pub(super) fn prepare_network_context(
    mode: WindowsNetworkMode,
    env: &HashMap<String, String>,
    sandbox_state_root: &Path,
) -> SandboxResult<WindowsNetworkContext> {
    let _ = env;
    match mode {
        WindowsNetworkMode::Online => Ok(WindowsNetworkContext {
            mode,
            state_root: sandbox_state_root.to_path_buf(),
            requires_hard_block: false,
        }),
        WindowsNetworkMode::Offline => Err(SandboxError::unavailable(
            "NetworkPolicy::Disabled is unavailable on Windows until hard network blocking is implemented",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::network::WindowsNetworkMode;
    use super::*;

    #[test]
    fn online_network_context_succeeds_without_setup() {
        let temp = tempfile::tempdir().unwrap();
        let context =
            prepare_network_context(WindowsNetworkMode::Online, &HashMap::new(), temp.path())
                .unwrap();

        assert_eq!(context.mode, WindowsNetworkMode::Online);
        assert!(!context.requires_hard_block);
    }

    #[test]
    fn offline_network_context_fails_closed_until_hard_block_exists() {
        let temp = tempfile::tempdir().unwrap();
        let err =
            prepare_network_context(WindowsNetworkMode::Offline, &HashMap::new(), temp.path())
                .unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("Disabled"));
        assert!(err.message.contains("hard network blocking"));
    }
}
