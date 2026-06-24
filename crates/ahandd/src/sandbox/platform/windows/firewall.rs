//! Windows firewall helpers for hard offline network blocking.

use std::fs::File;

#[cfg(windows)]
use super::setup_error::SetupErrorCode;
use super::setup_error::SetupFailure;

#[cfg(windows)]
const OFFLINE_BLOCK_RULE_NAME: &str = "ahand_sandbox_offline_block_outbound";
#[cfg(windows)]
const OFFLINE_BLOCK_RULE_FRIENDLY: &str = "aHand Sandbox Offline - Block Non-Loopback Outbound";
const NON_LOOPBACK_REMOTE_ADDRESSES: &str = "0.0.0.0-126.255.255.255,128.0.0.0-255.255.255.255,::,::2-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff";

pub(super) fn non_loopback_remote_addresses() -> &'static str {
    NON_LOOPBACK_REMOTE_ADDRESSES
}

#[allow(dead_code)]
pub(super) fn blocked_loopback_tcp_remote_ports(proxy_ports: &[u16]) -> Option<String> {
    let mut allowed_ports = proxy_ports
        .iter()
        .copied()
        .filter(|port| *port != 0)
        .collect::<Vec<_>>();
    allowed_ports.sort_unstable();
    allowed_ports.dedup();

    let mut blocked_ranges = Vec::new();
    let mut start = 1_u32;
    for port in allowed_ports {
        let port = u32::from(port);
        if port < start {
            continue;
        }
        if port > start {
            blocked_ranges.push(port_range_string(start, port - 1));
        }
        start = port + 1;
    }

    if start <= u32::from(u16::MAX) {
        blocked_ranges.push(port_range_string(start, u32::from(u16::MAX)));
    }

    if blocked_ranges.is_empty() {
        None
    } else {
        Some(blocked_ranges.join(","))
    }
}

#[allow(dead_code)]
fn port_range_string(start: u32, end: u32) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub(super) fn ensure_offline_outbound_block(_: &str, _: &mut File) -> Result<(), SetupFailure> {
    Err(SetupFailure::unavailable(
        "Windows firewall offline outbound block is only available on Windows",
    ))
}

#[cfg(windows)]
pub(super) fn ensure_offline_outbound_block(
    offline_sid: &str,
    log: &mut File,
) -> Result<(), SetupFailure> {
    use std::io::Write;
    use windows::Win32::NetworkManagement::WindowsFirewall::{
        INetFwPolicy2, NET_FW_IP_PROTOCOL_ANY, NetFwPolicy2,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };

    struct ComGuard;
    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }

    let local_user_spec = format!("O:LSD:(A;;CC;;;{offline_sid})");
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }.map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::FirewallComInitFailed,
            format!("CoInitializeEx failed: {err:?}"),
        )
    })?;
    let _guard = ComGuard;

    let policy: INetFwPolicy2 =
        unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }.map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::FirewallPolicyAccessFailed,
                format!("CoCreateInstance NetFwPolicy2 failed: {err:?}"),
            )
        })?;
    let rules = unsafe { policy.Rules() }.map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::FirewallPolicyAccessFailed,
            format!("INetFwPolicy2::Rules failed: {err:?}"),
        )
    })?;

    ensure_block_rule(
        &rules,
        OFFLINE_BLOCK_RULE_NAME,
        OFFLINE_BLOCK_RULE_FRIENDLY,
        NET_FW_IP_PROTOCOL_ANY.0,
        non_loopback_remote_addresses(),
        &local_user_spec,
        offline_sid,
    )?;
    writeln!(
        log,
        "firewall rule configured name={} RemoteAddresses={} LocalUserAuthorizedList={}",
        OFFLINE_BLOCK_RULE_NAME,
        non_loopback_remote_addresses(),
        local_user_spec
    )
    .map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SetupLogFailed,
            format!("failed to write setup log: {err}"),
        )
    })?;
    Ok(())
}

#[cfg(windows)]
fn ensure_block_rule(
    rules: &windows::Win32::NetworkManagement::WindowsFirewall::INetFwRules,
    internal_name: &str,
    friendly_desc: &str,
    protocol: i32,
    remote_addresses: &str,
    local_user_spec: &str,
    offline_sid: &str,
) -> Result<(), SetupFailure> {
    use windows::Win32::NetworkManagement::WindowsFirewall::{INetFwRule3, NetFwRule};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::core::{BSTR, Interface};

    let name = BSTR::from(internal_name);
    let rule: INetFwRule3 = match unsafe { rules.Item(&name) } {
        Ok(existing) => existing.cast().map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::FirewallRuleCreateOrAddFailed,
                format!("cast existing firewall rule to INetFwRule3 failed: {err:?}"),
            )
        })?,
        Err(_) => {
            let new_rule: INetFwRule3 =
                unsafe { CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER) }.map_err(
                    |err| {
                        SetupFailure::new(
                            SetupErrorCode::FirewallRuleCreateOrAddFailed,
                            format!("CoCreateInstance NetFwRule failed: {err:?}"),
                        )
                    },
                )?;
            unsafe { new_rule.SetName(&name) }.map_err(|err| {
                SetupFailure::new(
                    SetupErrorCode::FirewallRuleCreateOrAddFailed,
                    format!("SetName failed: {err:?}"),
                )
            })?;
            configure_rule(
                &new_rule,
                friendly_desc,
                protocol,
                remote_addresses,
                local_user_spec,
                offline_sid,
            )?;
            unsafe { rules.Add(&new_rule) }.map_err(|err| {
                SetupFailure::new(
                    SetupErrorCode::FirewallRuleCreateOrAddFailed,
                    format!("Rules::Add failed: {err:?}"),
                )
            })?;
            new_rule
        }
    };

    configure_rule(
        &rule,
        friendly_desc,
        protocol,
        remote_addresses,
        local_user_spec,
        offline_sid,
    )
}

#[cfg(windows)]
fn configure_rule(
    rule: &windows::Win32::NetworkManagement::WindowsFirewall::INetFwRule3,
    friendly_desc: &str,
    protocol: i32,
    remote_addresses: &str,
    local_user_spec: &str,
    offline_sid: &str,
) -> Result<(), SetupFailure> {
    use windows::Win32::Foundation::VARIANT_TRUE;
    use windows::Win32::NetworkManagement::WindowsFirewall::{
        NET_FW_ACTION_BLOCK, NET_FW_PROFILE2_ALL, NET_FW_RULE_DIR_OUT,
    };
    use windows::core::BSTR;

    unsafe {
        rule.SetDescription(&BSTR::from(friendly_desc))
            .map_err(|err| firewall_rule_error("SetDescription", err))?;
        rule.SetDirection(NET_FW_RULE_DIR_OUT)
            .map_err(|err| firewall_rule_error("SetDirection", err))?;
        rule.SetAction(NET_FW_ACTION_BLOCK)
            .map_err(|err| firewall_rule_error("SetAction", err))?;
        rule.SetEnabled(VARIANT_TRUE)
            .map_err(|err| firewall_rule_error("SetEnabled", err))?;
        rule.SetProfiles(NET_FW_PROFILE2_ALL.0)
            .map_err(|err| firewall_rule_error("SetProfiles", err))?;
        rule.SetProtocol(protocol)
            .map_err(|err| firewall_rule_error("SetProtocol", err))?;
        rule.SetRemoteAddresses(&BSTR::from(remote_addresses))
            .map_err(|err| firewall_rule_error("SetRemoteAddresses", err))?;
        rule.SetRemotePorts(&BSTR::from("*"))
            .map_err(|err| firewall_rule_error("SetRemotePorts", err))?;
        rule.SetLocalUserAuthorizedList(&BSTR::from(local_user_spec))
            .map_err(|err| firewall_rule_error("SetLocalUserAuthorizedList", err))?;
    }

    let actual = unsafe { rule.LocalUserAuthorizedList() }.map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::FirewallRuleVerifyFailed,
            format!("LocalUserAuthorizedList read-back failed: {err:?}"),
        )
    })?;
    let actual = actual.to_string();
    if !actual.contains(offline_sid) {
        return Err(SetupFailure::new(
            SetupErrorCode::FirewallRuleVerifyFailed,
            format!(
                "offline firewall rule user scope mismatch: expected SID {offline_sid}, got {actual}"
            ),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn firewall_rule_error(method: &str, err: windows::core::Error) -> SetupFailure {
    SetupFailure::new(
        SetupErrorCode::FirewallRuleCreateOrAddFailed,
        format!("{method} failed: {err:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_loopback_remote_address_literal_excludes_ipv4_and_ipv6_loopback() {
        assert_eq!(
            non_loopback_remote_addresses(),
            "0.0.0.0-126.255.255.255,128.0.0.0-255.255.255.255,::,::2-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"
        );
    }

    #[test]
    fn blocked_loopback_tcp_remote_ports_returns_complement_of_allowed_proxy_ports() {
        assert_eq!(
            blocked_loopback_tcp_remote_ports(&[8080, 1081, 8080]),
            Some("1-1080,1082-8079,8081-65535".to_string())
        );
    }

    #[test]
    fn blocked_loopback_tcp_remote_ports_returns_none_when_all_ports_are_allowed() {
        let all_ports = (1..=u16::MAX).collect::<Vec<_>>();

        assert_eq!(blocked_loopback_tcp_remote_ports(&all_ports), None);
    }
}
