//! Windows firewall helpers for hard offline network blocking.

use std::fs::File;

use super::setup_error::SetupErrorCode;
use super::setup_error::SetupFailure;

#[cfg(windows)]
const OFFLINE_BLOCK_RULE_NAME: &str = "ahand_sandbox_offline_block_outbound";
#[cfg(windows)]
const OFFLINE_BLOCK_RULE_FRIENDLY: &str = "aHand Sandbox Offline - Block Non-Loopback Outbound";
const NON_LOOPBACK_REMOTE_ADDRESSES: &str = "0.0.0.0-126.255.255.255,128.0.0.0-255.255.255.255,::,::2-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff";

struct BlockRuleSpec<'a> {
    internal_name: &'a str,
    protocol: i32,
    application_name: &'a str,
    service_name: &'a str,
    local_addresses: &'a str,
    local_ports: Option<&'a str>,
    interface_types: &'a str,
    remote_addresses: &'a str,
    remote_ports: &'a str,
    #[cfg_attr(not(windows), allow(dead_code))]
    local_user_spec: &'a str,
    offline_sid: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockRuleReadback {
    direction_out: bool,
    action_block: bool,
    enabled: bool,
    profiles_all: bool,
    protocol: i32,
    application_name: String,
    service_name: String,
    local_addresses: String,
    local_ports: String,
    interface_types: String,
    remote_addresses: String,
    remote_ports: String,
    local_user_authorized_list: String,
}

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

fn verify_block_rule_readback(
    spec: &BlockRuleSpec<'_>,
    readback: &BlockRuleReadback,
) -> Result<(), SetupFailure> {
    let mut mismatches = Vec::new();
    if !readback.direction_out {
        mismatches.push("Direction must be outbound".to_string());
    }
    if !readback.action_block {
        mismatches.push("Action must be block".to_string());
    }
    if !readback.enabled {
        mismatches.push("Enabled must be true".to_string());
    }
    if !readback.profiles_all {
        mismatches.push("Profiles must cover all profiles".to_string());
    }
    if readback.protocol != spec.protocol {
        mismatches.push(format!(
            "Protocol expected {}, got {}",
            spec.protocol, readback.protocol
        ));
    }
    if readback.application_name != spec.application_name {
        mismatches.push(format!(
            "ApplicationName expected {:?}, got {:?}",
            spec.application_name, readback.application_name
        ));
    }
    if readback.service_name != spec.service_name {
        mismatches.push(format!(
            "ServiceName expected {:?}, got {:?}",
            spec.service_name, readback.service_name
        ));
    }
    if readback.local_addresses != spec.local_addresses {
        mismatches.push(format!(
            "LocalAddresses expected {}, got {}",
            spec.local_addresses, readback.local_addresses
        ));
    }
    if let Some(local_ports) = spec.local_ports
        && readback.local_ports != local_ports
    {
        mismatches.push(format!(
            "LocalPorts expected {}, got {}",
            local_ports, readback.local_ports
        ));
    }
    if readback.interface_types != spec.interface_types {
        mismatches.push(format!(
            "InterfaceTypes expected {}, got {}",
            spec.interface_types, readback.interface_types
        ));
    }
    if readback.remote_addresses != spec.remote_addresses {
        mismatches.push(format!(
            "RemoteAddresses expected {}, got {}",
            spec.remote_addresses, readback.remote_addresses
        ));
    }
    if readback.remote_ports != spec.remote_ports {
        mismatches.push(format!(
            "RemotePorts expected {}, got {}",
            spec.remote_ports, readback.remote_ports
        ));
    }
    if !readback
        .local_user_authorized_list
        .contains(spec.offline_sid)
    {
        mismatches.push(format!(
            "LocalUserAuthorizedList expected SID {}, got {}",
            spec.offline_sid, readback.local_user_authorized_list
        ));
    }

    if mismatches.is_empty() {
        return Ok(());
    }

    Err(SetupFailure::new(
        SetupErrorCode::FirewallRuleVerifyFailed,
        format!(
            "offline firewall rule {} shape mismatch: {}",
            spec.internal_name,
            mismatches.join("; ")
        ),
    ))
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
    use windows::Win32::NetworkManagement::WindowsFirewall::NET_FW_IP_PROTOCOL_ANY;

    let local_user_spec = format!("O:LSD:(A;;CC;;;{offline_sid})");
    let spec = BlockRuleSpec {
        internal_name: OFFLINE_BLOCK_RULE_NAME,
        protocol: NET_FW_IP_PROTOCOL_ANY.0,
        application_name: "",
        service_name: "",
        local_addresses: "*",
        local_ports: None,
        interface_types: "All",
        remote_addresses: non_loopback_remote_addresses(),
        remote_ports: "*",
        local_user_spec: &local_user_spec,
        offline_sid,
    };

    with_firewall_rules(|rules| ensure_block_rule(rules, &spec))?;
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

#[cfg(not(windows))]
#[allow(dead_code)]
pub(super) fn verify_offline_outbound_block(_: &str) -> Result<(), SetupFailure> {
    Err(SetupFailure::unavailable(
        "Windows firewall offline outbound block is only available on Windows",
    ))
}

#[cfg(windows)]
pub(super) fn verify_offline_outbound_block(offline_sid: &str) -> Result<(), SetupFailure> {
    use windows::Win32::NetworkManagement::WindowsFirewall::NET_FW_IP_PROTOCOL_ANY;

    let local_user_spec = format!("O:LSD:(A;;CC;;;{offline_sid})");
    let spec = BlockRuleSpec {
        internal_name: OFFLINE_BLOCK_RULE_NAME,
        protocol: NET_FW_IP_PROTOCOL_ANY.0,
        application_name: "",
        service_name: "",
        local_addresses: "*",
        local_ports: None,
        interface_types: "All",
        remote_addresses: non_loopback_remote_addresses(),
        remote_ports: "*",
        local_user_spec: &local_user_spec,
        offline_sid,
    };

    with_firewall_rules(|rules| verify_block_rule_by_name(rules, &spec))
}

#[cfg(windows)]
fn with_firewall_rules<T>(
    f: impl FnOnce(
        &windows::Win32::NetworkManagement::WindowsFirewall::INetFwRules,
    ) -> Result<T, SetupFailure>,
) -> Result<T, SetupFailure> {
    use windows::Win32::NetworkManagement::WindowsFirewall::{INetFwPolicy2, NetFwPolicy2};
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

    f(&rules)
}

#[cfg(windows)]
fn ensure_block_rule(
    rules: &windows::Win32::NetworkManagement::WindowsFirewall::INetFwRules,
    spec: &BlockRuleSpec<'_>,
) -> Result<(), SetupFailure> {
    use windows::Win32::NetworkManagement::WindowsFirewall::{INetFwRule3, NetFwRule};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::core::BSTR;

    remove_rule_if_present(rules, spec.internal_name)?;

    let name = BSTR::from(spec.internal_name);
    let rule: INetFwRule3 = unsafe { CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER) }
        .map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::FirewallRuleCreateOrAddFailed,
                format!("CoCreateInstance NetFwRule failed: {err:?}"),
            )
        })?;
    unsafe { rule.SetName(&name) }.map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::FirewallRuleCreateOrAddFailed,
            format!("SetName failed: {err:?}"),
        )
    })?;
    configure_rule(&rule, OFFLINE_BLOCK_RULE_FRIENDLY, spec)?;
    unsafe { rules.Add(&rule) }.map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::FirewallRuleCreateOrAddFailed,
            format!("Rules::Add failed: {err:?}"),
        )
    })?;
    verify_block_rule(&rule, spec)
}

#[cfg(windows)]
fn remove_rule_if_present(
    rules: &windows::Win32::NetworkManagement::WindowsFirewall::INetFwRules,
    internal_name: &str,
) -> Result<(), SetupFailure> {
    use windows::core::BSTR;

    let name = BSTR::from(internal_name);
    if unsafe { rules.Item(&name) }.is_ok() {
        unsafe { rules.Remove(&name) }.map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::FirewallRuleCreateOrAddFailed,
                format!("Rules::Remove failed for {internal_name}: {err:?}"),
            )
        })?;
    }
    Ok(())
}

#[cfg(windows)]
fn verify_block_rule_by_name(
    rules: &windows::Win32::NetworkManagement::WindowsFirewall::INetFwRules,
    spec: &BlockRuleSpec<'_>,
) -> Result<(), SetupFailure> {
    use windows::Win32::NetworkManagement::WindowsFirewall::INetFwRule3;
    use windows::core::{BSTR, Interface};

    let name = BSTR::from(spec.internal_name);
    let rule: INetFwRule3 = unsafe { rules.Item(&name) }
        .map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::FirewallRuleVerifyFailed,
                format!("firewall rule {} is missing: {err:?}", spec.internal_name),
            )
        })?
        .cast()
        .map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::FirewallRuleVerifyFailed,
                format!("cast existing firewall rule to INetFwRule3 failed: {err:?}"),
            )
        })?;
    verify_block_rule(&rule, spec)
}

#[cfg(windows)]
fn verify_block_rule(
    rule: &windows::Win32::NetworkManagement::WindowsFirewall::INetFwRule3,
    spec: &BlockRuleSpec<'_>,
) -> Result<(), SetupFailure> {
    use windows::Win32::Foundation::VARIANT_TRUE;
    use windows::Win32::NetworkManagement::WindowsFirewall::{
        NET_FW_ACTION_BLOCK, NET_FW_PROFILE2_ALL, NET_FW_RULE_DIR_OUT,
    };

    let readback = unsafe {
        BlockRuleReadback {
            direction_out: rule.Direction().map_err(|err| {
                SetupFailure::new(
                    SetupErrorCode::FirewallRuleVerifyFailed,
                    format!("Direction read-back failed: {err:?}"),
                )
            })? == NET_FW_RULE_DIR_OUT,
            action_block: rule.Action().map_err(|err| {
                SetupFailure::new(
                    SetupErrorCode::FirewallRuleVerifyFailed,
                    format!("Action read-back failed: {err:?}"),
                )
            })? == NET_FW_ACTION_BLOCK,
            enabled: rule.Enabled().map_err(|err| {
                SetupFailure::new(
                    SetupErrorCode::FirewallRuleVerifyFailed,
                    format!("Enabled read-back failed: {err:?}"),
                )
            })? == VARIANT_TRUE,
            profiles_all: rule.Profiles().map_err(|err| {
                SetupFailure::new(
                    SetupErrorCode::FirewallRuleVerifyFailed,
                    format!("Profiles read-back failed: {err:?}"),
                )
            })? == NET_FW_PROFILE2_ALL.0,
            protocol: rule.Protocol().map_err(|err| {
                SetupFailure::new(
                    SetupErrorCode::FirewallRuleVerifyFailed,
                    format!("Protocol read-back failed: {err:?}"),
                )
            })?,
            application_name: rule
                .ApplicationName()
                .map_err(|err| {
                    SetupFailure::new(
                        SetupErrorCode::FirewallRuleVerifyFailed,
                        format!("ApplicationName read-back failed: {err:?}"),
                    )
                })?
                .to_string(),
            service_name: rule
                .ServiceName()
                .map_err(|err| {
                    SetupFailure::new(
                        SetupErrorCode::FirewallRuleVerifyFailed,
                        format!("ServiceName read-back failed: {err:?}"),
                    )
                })?
                .to_string(),
            local_addresses: rule
                .LocalAddresses()
                .map_err(|err| {
                    SetupFailure::new(
                        SetupErrorCode::FirewallRuleVerifyFailed,
                        format!("LocalAddresses read-back failed: {err:?}"),
                    )
                })?
                .to_string(),
            local_ports: rule
                .LocalPorts()
                .map_err(|err| {
                    SetupFailure::new(
                        SetupErrorCode::FirewallRuleVerifyFailed,
                        format!("LocalPorts read-back failed: {err:?}"),
                    )
                })?
                .to_string(),
            interface_types: rule
                .InterfaceTypes()
                .map_err(|err| {
                    SetupFailure::new(
                        SetupErrorCode::FirewallRuleVerifyFailed,
                        format!("InterfaceTypes read-back failed: {err:?}"),
                    )
                })?
                .to_string(),
            remote_addresses: rule
                .RemoteAddresses()
                .map_err(|err| {
                    SetupFailure::new(
                        SetupErrorCode::FirewallRuleVerifyFailed,
                        format!("RemoteAddresses read-back failed: {err:?}"),
                    )
                })?
                .to_string(),
            remote_ports: rule
                .RemotePorts()
                .map_err(|err| {
                    SetupFailure::new(
                        SetupErrorCode::FirewallRuleVerifyFailed,
                        format!("RemotePorts read-back failed: {err:?}"),
                    )
                })?
                .to_string(),
            local_user_authorized_list: rule
                .LocalUserAuthorizedList()
                .map_err(|err| {
                    SetupFailure::new(
                        SetupErrorCode::FirewallRuleVerifyFailed,
                        format!("LocalUserAuthorizedList read-back failed: {err:?}"),
                    )
                })?
                .to_string(),
        }
    };

    verify_block_rule_readback(spec, &readback)
}

#[cfg(windows)]
fn configure_rule(
    rule: &windows::Win32::NetworkManagement::WindowsFirewall::INetFwRule3,
    friendly_desc: &str,
    spec: &BlockRuleSpec<'_>,
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
        rule.SetProtocol(spec.protocol)
            .map_err(|err| firewall_rule_error("SetProtocol", err))?;
        rule.SetApplicationName(&BSTR::from(spec.application_name))
            .map_err(|err| firewall_rule_error("SetApplicationName", err))?;
        rule.SetServiceName(&BSTR::from(spec.service_name))
            .map_err(|err| firewall_rule_error("SetServiceName", err))?;
        rule.SetLocalAddresses(&BSTR::from(spec.local_addresses))
            .map_err(|err| firewall_rule_error("SetLocalAddresses", err))?;
        if let Some(local_ports) = spec.local_ports {
            rule.SetLocalPorts(&BSTR::from(local_ports))
                .map_err(|err| firewall_rule_error("SetLocalPorts", err))?;
        }
        rule.SetInterfaceTypes(&BSTR::from(spec.interface_types))
            .map_err(|err| firewall_rule_error("SetInterfaceTypes", err))?;
        rule.SetRemoteAddresses(&BSTR::from(spec.remote_addresses))
            .map_err(|err| firewall_rule_error("SetRemoteAddresses", err))?;
        rule.SetRemotePorts(&BSTR::from(spec.remote_ports))
            .map_err(|err| firewall_rule_error("SetRemotePorts", err))?;
        rule.SetLocalUserAuthorizedList(&BSTR::from(spec.local_user_spec))
            .map_err(|err| firewall_rule_error("SetLocalUserAuthorizedList", err))?;
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

    #[test]
    fn block_rule_readback_accepts_expected_shape() {
        let spec = test_block_rule_spec();
        let readback = test_block_rule_readback();

        verify_block_rule_readback(&spec, &readback).unwrap();
    }

    #[test]
    fn block_rule_readback_rejects_narrow_remote_ports() {
        let spec = test_block_rule_spec();
        let mut readback = test_block_rule_readback();
        readback.remote_ports = "443".to_string();

        let err = verify_block_rule_readback(&spec, &readback).unwrap_err();

        assert_eq!(err.code, SetupErrorCode::FirewallRuleVerifyFailed);
        assert!(err.message.contains("RemotePorts"));
    }

    #[test]
    fn block_rule_readback_rejects_wrong_action_or_disabled_rule() {
        let spec = test_block_rule_spec();
        let mut readback = test_block_rule_readback();
        readback.action_block = false;
        readback.enabled = false;

        let err = verify_block_rule_readback(&spec, &readback).unwrap_err();

        assert_eq!(err.code, SetupErrorCode::FirewallRuleVerifyFailed);
        assert!(err.message.contains("Action"));
        assert!(err.message.contains("Enabled"));
    }

    #[test]
    fn block_rule_readback_rejects_application_or_local_address_scope() {
        let spec = test_block_rule_spec();
        let mut readback = test_block_rule_readback();
        readback.application_name = r"C:\Tools\narrow.exe".to_string();
        readback.local_addresses = "127.0.0.1".to_string();

        let err = verify_block_rule_readback(&spec, &readback).unwrap_err();

        assert_eq!(err.code, SetupErrorCode::FirewallRuleVerifyFailed);
        assert!(err.message.contains("ApplicationName"));
        assert!(err.message.contains("LocalAddresses"));
    }

    fn test_block_rule_spec() -> BlockRuleSpec<'static> {
        BlockRuleSpec {
            internal_name: "ahand_sandbox_offline_block_outbound",
            protocol: 256,
            application_name: "",
            service_name: "",
            local_addresses: "*",
            local_ports: None,
            interface_types: "All",
            remote_addresses: non_loopback_remote_addresses(),
            remote_ports: "*",
            local_user_spec: "O:LSD:(A;;CC;;;S-1-5-21-1-2-3-1001)",
            offline_sid: "S-1-5-21-1-2-3-1001",
        }
    }

    fn test_block_rule_readback() -> BlockRuleReadback {
        BlockRuleReadback {
            direction_out: true,
            action_block: true,
            enabled: true,
            profiles_all: true,
            protocol: 256,
            application_name: String::new(),
            service_name: String::new(),
            local_addresses: "*".to_string(),
            local_ports: "*".to_string(),
            interface_types: "All".to_string(),
            remote_addresses: non_loopback_remote_addresses().to_string(),
            remote_ports: "*".to_string(),
            local_user_authorized_list: "O:LSD:(A;;CC;;;S-1-5-21-1-2-3-1001)".to_string(),
        }
    }
}
