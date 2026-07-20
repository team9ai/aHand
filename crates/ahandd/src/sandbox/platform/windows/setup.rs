//! Windows sandbox setup orchestration helpers.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use super::network::WindowsNetworkMode;
use super::setup_error::SetupFailure;
use super::setup_error::{
    SetupErrorCode, clear_setup_error_report, read_setup_error_report, write_setup_error_report,
};
use crate::sandbox::types::{SandboxError, SandboxResult};

pub(super) const SETUP_VERSION: u32 = 1;
pub(super) const OFFLINE_USERNAME: &str = "AhandSandboxOffline";
pub(super) const ONLINE_USERNAME: &str = "AhandSandboxOnline";

pub(super) fn sandbox_dir(state_root: &Path) -> PathBuf {
    state_root.join(".sandbox")
}

pub(super) fn sandbox_secrets_dir(state_root: &Path) -> PathBuf {
    state_root.join(".sandbox-secrets")
}

pub(super) fn setup_marker_path(state_root: &Path) -> PathBuf {
    sandbox_dir(state_root).join("setup_marker.json")
}

pub(super) fn sandbox_users_path(state_root: &Path) -> PathBuf {
    sandbox_secrets_dir(state_root).join("sandbox_users.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(super) struct SetupMarker {
    pub(super) version: u32,
    pub(super) offline_username: String,
    pub(super) online_username: String,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    #[serde(default)]
    pub(super) hard_network_block: bool,
    #[serde(default)]
    pub(super) proxy_ports: Vec<u16>,
    #[serde(default)]
    pub(super) allow_local_binding: bool,
}

impl SetupMarker {
    pub(super) fn version_matches(&self) -> bool {
        self.version == SETUP_VERSION
    }

    pub(super) fn usernames_match(&self) -> bool {
        self.offline_username == OFFLINE_USERNAME && self.online_username == ONLINE_USERNAME
    }

    pub(super) fn hard_network_block_ready(&self) -> bool {
        self.hard_network_block
    }

    pub(super) fn request_mismatch_reason(
        &self,
        network_identity: SandboxNetworkIdentity,
        offline_proxy_settings: &OfflineProxySettings,
    ) -> Option<String> {
        if !network_identity.uses_offline_identity() {
            return None;
        }
        if self.proxy_ports == offline_proxy_settings.proxy_ports
            && self.allow_local_binding == offline_proxy_settings.allow_local_binding
        {
            return None;
        }
        Some(format!(
            "offline firewall settings changed (stored_ports={:?}, desired_ports={:?}, stored_allow_local_binding={}, desired_allow_local_binding={})",
            self.proxy_ports,
            offline_proxy_settings.proxy_ports,
            self.allow_local_binding,
            offline_proxy_settings.allow_local_binding
        ))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(super) struct SandboxUserRecord {
    pub(super) username: String,
    pub(super) password: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(super) struct SandboxUsersFile {
    pub(super) version: u32,
    pub(super) offline: SandboxUserRecord,
    pub(super) online: SandboxUserRecord,
}

impl SandboxUsersFile {
    pub(super) fn version_matches(&self) -> bool {
        self.version == SETUP_VERSION
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum SandboxNetworkIdentity {
    Offline,
    Online,
}

impl SandboxNetworkIdentity {
    pub(super) fn uses_offline_identity(self) -> bool {
        matches!(self, Self::Offline)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OfflineProxySettings {
    pub(super) proxy_ports: Vec<u16>,
    pub(super) allow_local_binding: bool,
}

const PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "WS_PROXY",
    "WSS_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "ws_proxy",
    "wss_proxy",
];
const ALLOW_LOCAL_BINDING_ENV_KEY: &str = "AHAND_NETWORK_ALLOW_LOCAL_BINDING";
#[cfg(all(windows, test))]
const ALLOW_REAL_SETUP_IN_TESTS_ENV_KEY: &str = "AHAND_WINDOWS_SANDBOX_ALLOW_REAL_SETUP_IN_TESTS";
const SETUP_HELPER_ARG: &str = "--ahand-windows-sandbox-setup";

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SetupHelperMode {
    Online,
    Offline,
}

impl SetupHelperMode {
    fn network_identity(self) -> SandboxNetworkIdentity {
        match self {
            Self::Online => SandboxNetworkIdentity::Online,
            Self::Offline => SandboxNetworkIdentity::Offline,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct SetupHelperPayload {
    version: u32,
    mode: SetupHelperMode,
    state_root: PathBuf,
    env: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WindowsNetworkContext {
    pub(super) mode: WindowsNetworkMode,
    pub(super) state_root: PathBuf,
    pub(super) sandbox_creds: Option<super::identity::SandboxCreds>,
}

pub(super) fn try_run_helper_from_args() -> Result<bool, String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let Some(payload_arg) = setup_helper_payload_arg(&args)? else {
        return Ok(false);
    };
    run_setup_helper_payload_b64(&payload_arg.to_string_lossy()).map_err(|err| err.to_string())?;
    Ok(true)
}

fn setup_helper_payload_arg(args: &[OsString]) -> Result<Option<OsString>, String> {
    let Some(first) = args.first() else {
        return Ok(None);
    };
    if first != OsStr::new(SETUP_HELPER_ARG) {
        return Ok(None);
    }
    if args.len() != 2 {
        return Err(format!(
            "{SETUP_HELPER_ARG} requires exactly one payload argument"
        ));
    }
    Ok(Some(args[1].clone()))
}

fn run_setup_helper_payload_b64(payload_b64: &str) -> Result<(), SetupFailure> {
    let payload = decode_setup_helper_payload(payload_b64)?;
    let result = run_setup_helper_payload(&payload);
    match result {
        Ok(()) => {
            let _ = clear_setup_error_report(&payload.state_root);
            Ok(())
        }
        Err(err) => {
            let _ = write_setup_error_report(&payload.state_root, &err);
            Err(err)
        }
    }
}

fn decode_setup_helper_payload(payload_b64: &str) -> Result<SetupHelperPayload, SetupFailure> {
    let bytes = BASE64_STANDARD
        .decode(payload_b64.as_bytes())
        .map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::SetupHelperPayloadDecodeFailed,
                format!("failed to base64-decode Windows sandbox setup payload: {err}"),
            )
        })?;
    let payload: SetupHelperPayload = serde_json::from_slice(&bytes).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SetupHelperPayloadDecodeFailed,
            format!("failed to decode Windows sandbox setup payload JSON: {err}"),
        )
    })?;
    if payload.version != SETUP_VERSION {
        return Err(SetupFailure::new(
            SetupErrorCode::SetupHelperPayloadDecodeFailed,
            format!(
                "Windows sandbox setup payload version {} does not match required version {}",
                payload.version, SETUP_VERSION
            ),
        ));
    }
    Ok(payload)
}

fn encode_setup_helper_payload(payload: &SetupHelperPayload) -> Result<String, SetupFailure> {
    let json = serde_json::to_vec(payload).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SetupHelperPayloadEncodeFailed,
            format!("failed to serialize Windows sandbox setup payload: {err}"),
        )
    })?;
    Ok(BASE64_STANDARD.encode(json))
}

#[cfg(windows)]
fn run_setup_helper_payload(payload: &SetupHelperPayload) -> Result<(), SetupFailure> {
    match payload.mode {
        SetupHelperMode::Online => run_online_setup_elevated(&payload.env, &payload.state_root),
        SetupHelperMode::Offline => run_offline_setup_elevated(&payload.env, &payload.state_root),
    }
    .map(|_| ())
}

#[cfg(not(windows))]
fn run_setup_helper_payload(payload: &SetupHelperPayload) -> Result<(), SetupFailure> {
    Err(SetupFailure::unavailable(format!(
        "Windows sandbox setup helper cannot run on this platform for {:?}",
        payload.mode
    )))
}

pub(super) fn prepare_network_context(
    mode: WindowsNetworkMode,
    env: &HashMap<String, String>,
    sandbox_state_root: &Path,
) -> SandboxResult<WindowsNetworkContext> {
    match mode {
        WindowsNetworkMode::Online => match run_online_setup(env, sandbox_state_root) {
            Ok(creds) => Ok(WindowsNetworkContext {
                mode,
                state_root: sandbox_state_root.to_path_buf(),
                sandbox_creds: Some(creds),
            }),
            Err(err) => Err(SandboxError::unavailable(format!(
                "NetworkPolicy::Enabled sandbox user setup is unavailable or incomplete on Windows: {err}"
            ))),
        },
        WindowsNetworkMode::Offline => match run_offline_setup(env, sandbox_state_root) {
            Ok(creds) => Ok(WindowsNetworkContext {
                mode,
                state_root: sandbox_state_root.to_path_buf(),
                sandbox_creds: Some(creds),
            }),
            Err(err) => Err(SandboxError::unavailable(format!(
                "NetworkPolicy::Disabled hard network blocking/setup is unavailable or incomplete on Windows: {err}"
            ))),
        },
    }
}

pub(super) fn run_online_setup(
    env: &HashMap<String, String>,
    state_root: &Path,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    match super::identity::load_sandbox_creds_for_identity(
        SandboxNetworkIdentity::Online,
        state_root,
        env,
    ) {
        Ok(creds) => Ok(creds),
        Err(loader_error) => run_online_setup_inner(env, state_root, loader_error),
    }
}

#[cfg(not(windows))]
fn run_online_setup_inner(
    _: &HashMap<String, String>,
    _: &Path,
    loader_error: SetupFailure,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    Err(SetupFailure::unavailable(format!(
        "online sandbox user setup requires Windows local user support; existing setup is missing or unverified: {loader_error}"
    )))
}

#[cfg(windows)]
fn run_online_setup_inner(
    env: &HashMap<String, String>,
    state_root: &Path,
    loader_error: SetupFailure,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    #[cfg(test)]
    if !real_setup_allowed_in_tests() {
        return Err(test_real_setup_disabled_error(
            "online sandbox user setup",
            &loader_error,
        ));
    }

    if is_elevated()? {
        return run_online_setup_elevated(env, state_root);
    }

    run_elevated_setup_helper(SetupHelperMode::Online, env, state_root, loader_error)
}

#[cfg(windows)]
fn run_online_setup_elevated(
    env: &HashMap<String, String>,
    state_root: &Path,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    if !is_elevated()? {
        return Err(SetupFailure::new(
            SetupErrorCode::ElevationRequired,
            "online sandbox user setup helper must run from an elevated process",
        ));
    }
    let sandbox_dir = sandbox_dir(state_root);
    std::fs::create_dir_all(&sandbox_dir).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SetupLogFailed,
            format!("failed to create {}: {err}", sandbox_dir.display()),
        )
    })?;
    let log_path = sandbox_dir.join("setup.log");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::SetupLogFailed,
                format!("failed to open {}: {err}", log_path.display()),
            )
        })?;

    let marker_network_settings =
        offline_proxy_settings_from_env(env, SandboxNetworkIdentity::Online);
    let users = super::sandbox_users::provision_sandbox_user_accounts(&mut log)?;
    super::sandbox_users::write_sandbox_users_state(
        super::sandbox_users::SandboxUsersStateWrite {
            state_root,
            offline_user: OFFLINE_USERNAME,
            offline_pwd: &users.offline_password,
            online_user: ONLINE_USERNAME,
            online_pwd: &users.online_password,
            proxy_ports: &marker_network_settings.proxy_ports,
            allow_local_binding: marker_network_settings.allow_local_binding,
            hard_network_block: false,
        },
    )?;
    super::identity::load_sandbox_creds_for_identity(
        SandboxNetworkIdentity::Online,
        state_root,
        env,
    )
}

pub(super) fn run_offline_setup(
    env: &HashMap<String, String>,
    state_root: &Path,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    #[cfg(not(windows))]
    {
        let loader_error = super::identity::load_sandbox_creds_for_identity(
            SandboxNetworkIdentity::Offline,
            state_root,
            env,
        )
        .err()
        .unwrap_or_else(|| {
            SetupFailure::unavailable(
                "offline hard network block marker is present but cannot be trusted without Windows firewall support",
            )
        });
        run_offline_setup_inner(env, state_root, loader_error)
    }

    #[cfg(windows)]
    match load_verified_offline_setup(env, state_root) {
        Ok(creds) => Ok(creds),
        Err(loader_error) => run_offline_setup_inner(env, state_root, loader_error),
    }
}

#[cfg(windows)]
fn load_verified_offline_setup(
    env: &HashMap<String, String>,
    state_root: &Path,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    let creds = super::identity::load_sandbox_creds_for_identity(
        SandboxNetworkIdentity::Offline,
        state_root,
        env,
    )?;
    #[cfg(test)]
    if !real_setup_allowed_in_tests() {
        return Ok(creds);
    }

    let offline_sid = super::sandbox_users::resolve_sandbox_user_sid(OFFLINE_USERNAME)?;
    super::firewall::verify_offline_outbound_block(&offline_sid)?;
    Ok(creds)
}

#[cfg(not(windows))]
fn run_offline_setup_inner(
    _: &HashMap<String, String>,
    _: &Path,
    loader_error: SetupFailure,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    Err(SetupFailure::unavailable(format!(
        "offline hard network block setup requires Windows firewall support; existing setup is missing or unverified: {loader_error}"
    )))
}

#[cfg(windows)]
fn run_offline_setup_inner(
    env: &HashMap<String, String>,
    state_root: &Path,
    loader_error: SetupFailure,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    #[cfg(test)]
    if !real_setup_allowed_in_tests() {
        return Err(test_real_setup_disabled_error(
            "offline hard network block setup",
            &loader_error,
        ));
    }

    if is_elevated()? {
        return run_offline_setup_elevated(env, state_root);
    }

    run_elevated_setup_helper(SetupHelperMode::Offline, env, state_root, loader_error)
}

#[cfg(windows)]
fn run_offline_setup_elevated(
    env: &HashMap<String, String>,
    state_root: &Path,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    if !is_elevated()? {
        return Err(SetupFailure::new(
            SetupErrorCode::ElevationRequired,
            "offline hard network block setup helper must run from an elevated process",
        ));
    }
    let sandbox_dir = sandbox_dir(state_root);
    std::fs::create_dir_all(&sandbox_dir).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SetupLogFailed,
            format!("failed to create {}: {err}", sandbox_dir.display()),
        )
    })?;
    let log_path = sandbox_dir.join("setup.log");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|err| {
            SetupFailure::new(
                SetupErrorCode::SetupLogFailed,
                format!("failed to open {}: {err}", log_path.display()),
            )
        })?;

    let offline_proxy_settings =
        offline_proxy_settings_from_env(env, SandboxNetworkIdentity::Offline);
    let users = super::sandbox_users::provision_sandbox_user_accounts(&mut log)?;
    let offline_sid = super::sandbox_users::resolve_sandbox_user_sid(OFFLINE_USERNAME)?;
    super::firewall::ensure_offline_outbound_block(&offline_sid, &mut log)?;
    super::sandbox_users::write_sandbox_users_state(
        super::sandbox_users::SandboxUsersStateWrite {
            state_root,
            offline_user: OFFLINE_USERNAME,
            offline_pwd: &users.offline_password,
            online_user: ONLINE_USERNAME,
            online_pwd: &users.online_password,
            proxy_ports: &offline_proxy_settings.proxy_ports,
            allow_local_binding: offline_proxy_settings.allow_local_binding,
            hard_network_block: true,
        },
    )?;
    load_verified_offline_setup(env, state_root)
}

#[cfg(windows)]
fn run_elevated_setup_helper(
    mode: SetupHelperMode,
    env: &HashMap<String, String>,
    state_root: &Path,
    loader_error: SetupFailure,
) -> Result<super::identity::SandboxCreds, SetupFailure> {
    launch_elevated_setup_helper(mode, env, state_root).map_err(|err| {
        SetupFailure::new(
            err.code,
            format!(
                "Windows sandbox setup helper failed: {err}; existing setup is missing or unverified: {loader_error}"
            ),
        )
    })?;
    super::identity::load_sandbox_creds_for_identity(mode.network_identity(), state_root, env)
}

#[cfg(windows)]
fn launch_elevated_setup_helper(
    mode: SetupHelperMode,
    env: &HashMap<String, String>,
    state_root: &Path,
) -> Result<(), SetupFailure> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };

    let sandbox_dir = sandbox_dir(state_root);
    std::fs::create_dir_all(&sandbox_dir).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SetupLogFailed,
            format!(
                "failed to create {} before setup helper launch: {err}",
                sandbox_dir.display()
            ),
        )
    })?;
    let _ = clear_setup_error_report(state_root);

    let payload = SetupHelperPayload {
        version: SETUP_VERSION,
        mode,
        state_root: state_root.to_path_buf(),
        env: env.clone(),
    };
    let payload_b64 = encode_setup_helper_payload(&payload)?;
    let exe = std::env::current_exe().map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SetupHelperLaunchFailed,
            format!("failed to resolve current executable for setup helper: {err}"),
        )
    })?;
    let params = format!("{SETUP_HELPER_ARG} {payload_b64}");
    let exe_w = super::winutil::to_wide(exe.as_os_str());
    let params_w = super::winutil::to_wide(OsStr::new(&params));
    let verb_w = super::winutil::to_wide(OsStr::new("runas"));
    let mut shell_info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    shell_info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    shell_info.fMask = SEE_MASK_NOCLOSEPROCESS;
    shell_info.lpVerb = verb_w.as_ptr();
    shell_info.lpFile = exe_w.as_ptr();
    shell_info.lpParameters = params_w.as_ptr();
    shell_info.nShow = 0;
    let ok = unsafe { ShellExecuteExW(&mut shell_info) };
    if ok == 0 || shell_info.hProcess.is_null() {
        let last_error = unsafe { GetLastError() };
        let code = if last_error == ERROR_CANCELLED {
            SetupErrorCode::SetupHelperLaunchCanceled
        } else {
            SetupErrorCode::SetupHelperLaunchFailed
        };
        return Err(SetupFailure::new(
            code,
            format!("ShellExecuteExW failed to launch Windows sandbox setup helper: {last_error}"),
        ));
    }

    let mut exit_code: u32 = 1;
    unsafe {
        WaitForSingleObject(shell_info.hProcess, INFINITE);
        if GetExitCodeProcess(shell_info.hProcess, &mut exit_code) == 0 {
            let last_error = GetLastError();
            CloseHandle(shell_info.hProcess);
            return Err(SetupFailure::new(
                SetupErrorCode::SetupHelperExitFailed,
                format!("failed to read Windows sandbox setup helper exit code: {last_error}"),
            ));
        }
        CloseHandle(shell_info.hProcess);
    }
    if exit_code != 0 {
        match read_setup_error_report(state_root) {
            Ok(Some(report)) => return Err(SetupFailure::from_report(report)),
            Ok(None) => {
                return Err(SetupFailure::new(
                    SetupErrorCode::SetupHelperExitFailed,
                    format!("Windows sandbox setup helper exited with code {exit_code}"),
                ));
            }
            Err(err) => {
                return Err(SetupFailure::new(
                    err.code,
                    format!(
                        "Windows sandbox setup helper exited with code {exit_code}; failed to read setup_error.json: {err}"
                    ),
                ));
            }
        }
    }
    let _ = clear_setup_error_report(state_root);
    Ok(())
}

#[cfg(windows)]
fn is_elevated() -> Result<bool, SetupFailure> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Security::{
        AllocateAndInitializeSid, CheckTokenMembership, FreeSid, SECURITY_NT_AUTHORITY,
    };

    const SECURITY_BUILTIN_DOMAIN_RID: u32 = 0x0000_0020;
    const DOMAIN_ALIAS_RID_ADMINS: u32 = 0x0000_0220;

    unsafe {
        let mut administrators_group: *mut std::ffi::c_void = std::ptr::null_mut();
        let ok = AllocateAndInitializeSid(
            &SECURITY_NT_AUTHORITY,
            2,
            SECURITY_BUILTIN_DOMAIN_RID,
            DOMAIN_ALIAS_RID_ADMINS,
            0,
            0,
            0,
            0,
            0,
            0,
            &mut administrators_group,
        );
        if ok == 0 {
            return Err(SetupFailure::unavailable(format!(
                "AllocateAndInitializeSid failed while checking elevation: {}",
                GetLastError()
            )));
        }
        let mut is_member = 0i32;
        let check = CheckTokenMembership(
            std::ptr::null_mut(),
            administrators_group,
            &mut is_member as *mut _,
        );
        FreeSid(administrators_group);
        if check == 0 {
            return Err(SetupFailure::unavailable(format!(
                "CheckTokenMembership failed while checking elevation: {}",
                GetLastError()
            )));
        }
        Ok(is_member != 0)
    }
}

#[cfg(all(windows, test))]
fn real_setup_allowed_in_tests() -> bool {
    std::env::var_os(ALLOW_REAL_SETUP_IN_TESTS_ENV_KEY).is_some()
}

#[cfg(all(windows, test))]
fn test_real_setup_disabled_error(kind: &str, loader_error: &SetupFailure) -> SetupFailure {
    SetupFailure::unavailable(format!(
        "{kind} is disabled during unit tests; existing setup is missing or unverified: {loader_error}"
    ))
}

pub(super) fn offline_proxy_settings_from_env(
    env_map: &HashMap<String, String>,
    network_identity: SandboxNetworkIdentity,
) -> OfflineProxySettings {
    if !network_identity.uses_offline_identity() {
        return OfflineProxySettings {
            proxy_ports: vec![],
            allow_local_binding: false,
        };
    }
    OfflineProxySettings {
        proxy_ports: proxy_ports_from_env(env_map),
        allow_local_binding: env_map
            .get(ALLOW_LOCAL_BINDING_ENV_KEY)
            .is_some_and(|value| value == "1"),
    }
}

pub(super) fn proxy_ports_from_env(env_map: &HashMap<String, String>) -> Vec<u16> {
    let mut ports = BTreeSet::new();
    for key in PROXY_ENV_KEYS {
        if let Some(value) = env_map.get(*key)
            && let Some(port) = loopback_proxy_port_from_url(value)
        {
            ports.insert(port);
        }
    }
    ports.into_iter().collect()
}

fn loopback_proxy_port_from_url(url: &str) -> Option<u16> {
    let authority = url.trim().split_once("://")?.1.split('/').next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);

    if let Some(host) = host_port.strip_prefix('[') {
        let (host, rest) = host.split_once(']')?;
        if host != "::1" {
            return None;
        }
        let port = rest.strip_prefix(':')?.parse::<u16>().ok()?;
        return (port != 0).then_some(port);
    }

    let (host, port) = host_port.rsplit_once(':')?;
    if !(host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1") {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    (port != 0).then_some(port)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::super::network::WindowsNetworkMode;
    use super::*;

    fn write_valid_test_setup_state(state_root: &Path) {
        fs::create_dir_all(sandbox_dir(state_root)).unwrap();
        fs::write(
            setup_marker_path(state_root),
            serde_json::json!({
                "version": SETUP_VERSION,
                "offline_username": OFFLINE_USERNAME,
                "online_username": ONLINE_USERNAME,
                "created_at": "2026-06-24T00:00:00Z",
                "hard_network_block": false,
                "proxy_ports": [],
                "allow_local_binding": false,
            })
            .to_string(),
        )
        .unwrap();

        fs::create_dir_all(sandbox_secrets_dir(state_root)).unwrap();
        fs::write(
            sandbox_users_path(state_root),
            serde_json::json!({
                "version": SETUP_VERSION,
                "offline": {
                    "username": OFFLINE_USERNAME,
                    "password": "test-plain:offline-password",
                },
                "online": {
                    "username": ONLINE_USERNAME,
                    "password": "test-plain:online-password",
                },
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn setup_helper_payload_arg_ignores_normal_invocations() {
        assert_eq!(
            setup_helper_payload_arg(&[OsString::from("--not-setup")]).unwrap(),
            None
        );
    }

    #[test]
    fn setup_helper_payload_arg_requires_one_payload() {
        let err = setup_helper_payload_arg(&[OsString::from(SETUP_HELPER_ARG)])
            .expect_err("missing payload should fail");

        assert!(err.contains("requires exactly one payload"));
    }

    #[test]
    fn setup_helper_payload_round_trips() {
        let payload = SetupHelperPayload {
            version: SETUP_VERSION,
            mode: SetupHelperMode::Online,
            state_root: PathBuf::from(r"C:\coffice\sandbox-state"),
            env: HashMap::from([(
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:8080".to_string(),
            )]),
        };

        let encoded = encode_setup_helper_payload(&payload).unwrap();
        let decoded = decode_setup_helper_payload(&encoded).unwrap();

        assert_eq!(decoded, payload);
    }

    #[test]
    fn online_network_context_fails_closed_until_sandbox_user_setup_exists() {
        let temp = tempfile::tempdir().unwrap();
        let err = prepare_network_context(WindowsNetworkMode::Online, &HashMap::new(), temp.path())
            .unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("Enabled"));
        assert!(err.message.contains("sandbox user setup"));
        assert!(err.message.contains("missing"));
    }

    #[test]
    fn online_network_context_loads_existing_online_creds_without_hard_network_block() {
        let temp = tempfile::tempdir().unwrap();
        write_valid_test_setup_state(temp.path());

        let context =
            prepare_network_context(WindowsNetworkMode::Online, &HashMap::new(), temp.path())
                .unwrap();

        assert_eq!(context.mode, WindowsNetworkMode::Online);
        let creds = context.sandbox_creds.unwrap();
        assert_eq!(creds.username, ONLINE_USERNAME);
        assert_eq!(creds.password, "online-password");
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
        assert!(err.message.contains("setup is unavailable or incomplete"));
    }

    #[test]
    fn offline_network_context_still_fails_closed_when_identity_state_exists() {
        let temp = tempfile::tempdir().unwrap();
        write_valid_test_setup_state(temp.path());

        let err =
            prepare_network_context(WindowsNetworkMode::Offline, &HashMap::new(), temp.path())
                .unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("hard network blocking"));
        assert!(err.message.contains("hard network block"));
    }

    #[test]
    fn setup_marker_defaults_hard_network_block_to_false_when_absent() {
        let marker: SetupMarker = serde_json::from_value(serde_json::json!({
            "version": SETUP_VERSION,
            "offline_username": OFFLINE_USERNAME,
            "online_username": ONLINE_USERNAME,
            "created_at": "2026-06-24T00:00:00Z",
            "proxy_ports": [],
            "allow_local_binding": false,
        }))
        .unwrap();

        assert!(!marker.hard_network_block);
    }

    #[cfg(windows)]
    #[test]
    fn offline_network_context_loads_creds_when_hard_block_is_ready() {
        let temp = tempfile::tempdir().unwrap();
        write_valid_test_setup_state(temp.path());

        let mut marker: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(setup_marker_path(temp.path())).unwrap())
                .unwrap();
        marker["hard_network_block"] = serde_json::Value::Bool(true);
        fs::write(setup_marker_path(temp.path()), marker.to_string()).unwrap();

        let context =
            prepare_network_context(WindowsNetworkMode::Offline, &HashMap::new(), temp.path())
                .unwrap();

        assert_eq!(context.mode, WindowsNetworkMode::Offline);
        assert_eq!(context.sandbox_creds.unwrap().username, OFFLINE_USERNAME);
    }

    #[cfg(not(windows))]
    #[test]
    fn offline_network_context_rejects_ready_marker_without_windows_firewall_support() {
        let temp = tempfile::tempdir().unwrap();
        write_valid_test_setup_state(temp.path());

        let mut marker: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(setup_marker_path(temp.path())).unwrap())
                .unwrap();
        marker["hard_network_block"] = serde_json::Value::Bool(true);
        fs::write(setup_marker_path(temp.path()), marker.to_string()).unwrap();

        let err =
            prepare_network_context(WindowsNetworkMode::Offline, &HashMap::new(), temp.path())
                .unwrap_err();

        assert_eq!(err.code, "SANDBOX_UNAVAILABLE");
        assert!(err.message.contains("Windows firewall support"));
    }

    #[test]
    fn loopback_proxy_url_parsing_supports_common_forms() {
        assert_eq!(
            loopback_proxy_port_from_url("http://localhost:3128"),
            Some(3128)
        );
        assert_eq!(
            loopback_proxy_port_from_url("https://127.0.0.1:8080"),
            Some(8080)
        );
        assert_eq!(
            loopback_proxy_port_from_url("socks5h://user:pass@[::1]:1080"),
            Some(1080)
        );
    }

    #[test]
    fn loopback_proxy_url_parsing_rejects_non_loopback_and_zero_port() {
        assert_eq!(
            loopback_proxy_port_from_url("http://example.com:3128"),
            None
        );
        assert_eq!(loopback_proxy_port_from_url("http://127.0.0.1:0"), None);
        assert_eq!(loopback_proxy_port_from_url("localhost:8080"), None);
    }

    #[test]
    fn proxy_ports_from_env_dedupes_and_sorts() {
        let env = HashMap::from([
            (
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:8080".to_string(),
            ),
            (
                "http_proxy".to_string(),
                "http://localhost:8080".to_string(),
            ),
            ("ALL_PROXY".to_string(), "socks5h://[::1]:1081".to_string()),
            (
                "HTTPS_PROXY".to_string(),
                "https://example.com:9999".to_string(),
            ),
        ]);

        assert_eq!(proxy_ports_from_env(&env), vec![1081, 8080]);
    }

    #[test]
    fn offline_proxy_settings_ignore_proxy_env_when_online_identity_selected() {
        let env = HashMap::from([
            (
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:8080".to_string(),
            ),
            (
                "AHAND_NETWORK_ALLOW_LOCAL_BINDING".to_string(),
                "1".to_string(),
            ),
        ]);

        assert_eq!(
            offline_proxy_settings_from_env(&env, SandboxNetworkIdentity::Online),
            OfflineProxySettings {
                proxy_ports: vec![],
                allow_local_binding: false,
            }
        );
    }

    #[test]
    fn offline_proxy_settings_capture_proxy_ports_and_local_binding_for_offline_identity() {
        let env = HashMap::from([
            (
                "HTTP_PROXY".to_string(),
                "http://127.0.0.1:8080".to_string(),
            ),
            (
                "ALL_PROXY".to_string(),
                "socks5h://127.0.0.1:1081".to_string(),
            ),
            (
                "AHAND_NETWORK_ALLOW_LOCAL_BINDING".to_string(),
                "1".to_string(),
            ),
        ]);

        assert_eq!(
            offline_proxy_settings_from_env(&env, SandboxNetworkIdentity::Offline),
            OfflineProxySettings {
                proxy_ports: vec![1081, 8080],
                allow_local_binding: true,
            }
        );
    }

    #[test]
    fn setup_marker_request_mismatch_reason_ignores_proxy_drift_for_online_identity() {
        let marker = SetupMarker {
            version: SETUP_VERSION,
            offline_username: OFFLINE_USERNAME.to_string(),
            online_username: ONLINE_USERNAME.to_string(),
            created_at: None,
            hard_network_block: true,
            proxy_ports: vec![3128],
            allow_local_binding: false,
        };
        let desired = OfflineProxySettings {
            proxy_ports: vec![1081, 8080],
            allow_local_binding: true,
        };

        assert_eq!(
            marker.request_mismatch_reason(SandboxNetworkIdentity::Online, &desired),
            None
        );
    }

    #[test]
    fn setup_marker_request_mismatch_reason_reports_offline_firewall_drift() {
        let marker = SetupMarker {
            version: SETUP_VERSION,
            offline_username: OFFLINE_USERNAME.to_string(),
            online_username: ONLINE_USERNAME.to_string(),
            created_at: None,
            hard_network_block: true,
            proxy_ports: vec![3128],
            allow_local_binding: false,
        };
        let desired = OfflineProxySettings {
            proxy_ports: vec![1081, 8080],
            allow_local_binding: true,
        };

        assert_eq!(
            marker.request_mismatch_reason(SandboxNetworkIdentity::Offline, &desired),
            Some(
                "offline firewall settings changed (stored_ports=[3128], desired_ports=[1081, 8080], stored_allow_local_binding=false, desired_allow_local_binding=true)"
                    .to_string()
            )
        );
    }
}
