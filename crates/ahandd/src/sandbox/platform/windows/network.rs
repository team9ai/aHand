//! Windows network policy setup for sandboxed commands.

use crate::sandbox::types::{NetworkPolicy, SandboxError, SandboxResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowsNetworkMode {
    Online,
    Offline,
}

pub(super) fn mode_for_policy(policy: NetworkPolicy) -> SandboxResult<WindowsNetworkMode> {
    match policy {
        NetworkPolicy::Enabled => Ok(WindowsNetworkMode::Online),
        NetworkPolicy::Disabled => Ok(WindowsNetworkMode::Offline),
        NetworkPolicy::ProxyOnly => Err(SandboxError::unavailable(
            "NetworkPolicy::ProxyOnly is not supported by the aHand sandbox yet",
        )),
    }
}

#[cfg(test)]
mod tests {
    use crate::sandbox::types::NetworkPolicy;

    use super::*;

    #[test]
    fn enabled_policy_selects_online_mode() {
        assert_eq!(
            mode_for_policy(NetworkPolicy::Enabled).unwrap(),
            WindowsNetworkMode::Online
        );
    }

    #[test]
    fn disabled_policy_selects_offline_mode() {
        assert_eq!(
            mode_for_policy(NetworkPolicy::Disabled).unwrap(),
            WindowsNetworkMode::Offline
        );
    }

    #[test]
    fn proxy_only_policy_is_unsupported() {
        let err = mode_for_policy(NetworkPolicy::ProxyOnly).unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("ProxyOnly"));
    }
}
