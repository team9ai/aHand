//! Local account helpers for future Windows sandbox setup.
#![allow(dead_code)]

use std::fs::File;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rand::Rng;

use super::setup::{
    SETUP_VERSION, SandboxUserRecord, SandboxUsersFile, SetupMarker, sandbox_dir,
    sandbox_secrets_dir,
};
use super::setup_error::{SetupErrorCode, SetupFailure};

#[cfg(windows)]
use super::setup::{OFFLINE_USERNAME, ONLINE_USERNAME};
#[cfg(windows)]
use std::io::Write;

pub(super) const SANDBOX_USERS_GROUP: &str = "AhandSandboxUsers";
const SANDBOX_USERS_GROUP_COMMENT: &str = "aHand sandbox internal group (managed)";

#[cfg(windows)]
pub(super) fn ensure_sandbox_users_group(log: &mut File) -> Result<(), SetupFailure> {
    ensure_local_group(SANDBOX_USERS_GROUP, SANDBOX_USERS_GROUP_COMMENT, log)
}

#[cfg(not(windows))]
pub(super) fn ensure_sandbox_users_group(_: &mut File) -> Result<(), SetupFailure> {
    Err(SetupFailure::unavailable(
        "sandbox local users are only available on Windows",
    ))
}

#[cfg(windows)]
pub(super) fn resolve_sandbox_users_group_sid() -> Result<Vec<u8>, SetupFailure> {
    resolve_sid(SANDBOX_USERS_GROUP)
}

#[cfg(not(windows))]
pub(super) fn resolve_sandbox_users_group_sid() -> Result<Vec<u8>, SetupFailure> {
    Err(SetupFailure::unavailable(
        "sandbox local users are only available on Windows",
    ))
}

#[cfg(windows)]
pub(super) fn provision_sandbox_users(
    state_root: &Path,
    proxy_ports: &[u16],
    allow_local_binding: bool,
    log: &mut File,
) -> Result<(), SetupFailure> {
    let users = provision_sandbox_user_accounts(log)?;
    write_sandbox_users_state(
        state_root,
        OFFLINE_USERNAME,
        &users.offline_password,
        ONLINE_USERNAME,
        &users.online_password,
        proxy_ports,
        allow_local_binding,
        false,
    )
}

#[cfg(not(windows))]
pub(super) fn provision_sandbox_users(
    _: &Path,
    _: &[u16],
    _: bool,
    _: &mut File,
) -> Result<(), SetupFailure> {
    Err(SetupFailure::unavailable(
        "sandbox local users are only available on Windows",
    ))
}

#[cfg(windows)]
pub(super) fn ensure_sandbox_user(
    username: &str,
    password: &str,
    log: &mut File,
) -> Result<(), SetupFailure> {
    ensure_local_user(username, password, log)?;
    ensure_local_group_member(SANDBOX_USERS_GROUP, username)
}

#[cfg(windows)]
pub(super) struct ProvisionedSandboxUsers {
    pub(super) offline_password: String,
    pub(super) online_password: String,
}

#[cfg(windows)]
pub(super) fn provision_sandbox_user_accounts(
    log: &mut File,
) -> Result<ProvisionedSandboxUsers, SetupFailure> {
    ensure_sandbox_users_group(log)?;
    let offline_password = random_password();
    let online_password = random_password();
    ensure_sandbox_user(OFFLINE_USERNAME, &offline_password, log)?;
    ensure_sandbox_user(ONLINE_USERNAME, &online_password, log)?;
    Ok(ProvisionedSandboxUsers {
        offline_password,
        online_password,
    })
}

#[cfg(windows)]
fn ensure_local_user(name: &str, password: &str, log: &mut File) -> Result<(), SetupFailure> {
    use std::ffi::OsStr;
    use windows_sys::Win32::NetworkManagement::NetManagement::{
        LOCALGROUP_MEMBERS_INFO_3, NERR_Success, NetLocalGroupAddMembers, NetUserAdd,
        NetUserSetInfo, UF_DONT_EXPIRE_PASSWD, UF_SCRIPT, USER_INFO_1, USER_INFO_1003,
        USER_PRIV_USER,
    };

    let name_w = super::winutil::to_wide(OsStr::new(name));
    let pwd_w = super::winutil::to_wide(OsStr::new(password));
    unsafe {
        let info = USER_INFO_1 {
            usri1_name: name_w.as_ptr() as *mut u16,
            usri1_password: pwd_w.as_ptr() as *mut u16,
            usri1_password_age: 0,
            usri1_priv: USER_PRIV_USER,
            usri1_home_dir: std::ptr::null_mut(),
            usri1_comment: std::ptr::null_mut(),
            usri1_flags: UF_SCRIPT | UF_DONT_EXPIRE_PASSWD,
            usri1_script_path: std::ptr::null_mut(),
        };
        let status = NetUserAdd(
            std::ptr::null(),
            1,
            &info as *const _ as *mut u8,
            std::ptr::null_mut(),
        );
        if status != NERR_Success {
            let pw_info = USER_INFO_1003 {
                usri1003_password: pwd_w.as_ptr() as *mut u16,
            };
            let updated = NetUserSetInfo(
                std::ptr::null(),
                name_w.as_ptr(),
                1003,
                &pw_info as *const _ as *mut u8,
                std::ptr::null_mut(),
            );
            if updated != NERR_Success {
                let _ = writeln!(
                    log,
                    "NetUserSetInfo failed for {name} code {updated}; add code {status}"
                );
                return Err(SetupFailure::new(
                    SetupErrorCode::UserCreateOrUpdateFailed,
                    format!("failed to create/update user {name}, code {status}/{updated}"),
                ));
            }
        }

        if let Ok(group_name) = lookup_account_name_for_sid("S-1-5-32-545") {
            let group_w = super::winutil::to_wide(OsStr::new(&group_name));
            let member = LOCALGROUP_MEMBERS_INFO_3 {
                lgrmi3_domainandname: name_w.as_ptr() as *mut u16,
            };
            let _ = NetLocalGroupAddMembers(
                std::ptr::null(),
                group_w.as_ptr(),
                3,
                &member as *const _ as *mut u8,
                1,
            );
        } else {
            let _ = writeln!(
                log,
                "LookupAccountSidW failed for Users SID; skipping Users group"
            );
        }
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_local_group(name: &str, comment: &str, log: &mut File) -> Result<(), SetupFailure> {
    use std::ffi::OsStr;
    use windows_sys::Win32::NetworkManagement::NetManagement::{
        LOCALGROUP_INFO_1, NERR_Success, NetLocalGroupAdd,
    };

    const ERROR_ALIAS_EXISTS: u32 = 1379;
    const NERR_GROUP_EXISTS: u32 = 2223;

    let name_w = super::winutil::to_wide(OsStr::new(name));
    let comment_w = super::winutil::to_wide(OsStr::new(comment));
    unsafe {
        let info = LOCALGROUP_INFO_1 {
            lgrpi1_name: name_w.as_ptr() as *mut u16,
            lgrpi1_comment: comment_w.as_ptr() as *mut u16,
        };
        let mut parm_err: u32 = 0;
        let status = NetLocalGroupAdd(
            std::ptr::null(),
            1,
            &info as *const _ as *mut u8,
            &mut parm_err as *mut _,
        );
        if status != NERR_Success && status != ERROR_ALIAS_EXISTS && status != NERR_GROUP_EXISTS {
            let _ = writeln!(
                log,
                "NetLocalGroupAdd failed for {name} code {status} parm_err={parm_err}"
            );
            return Err(SetupFailure::new(
                SetupErrorCode::UsersGroupCreateFailed,
                format!("failed to create local group {name}, code {status}"),
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_local_group_member(group_name: &str, member_name: &str) -> Result<(), SetupFailure> {
    use std::ffi::OsStr;
    use windows_sys::Win32::NetworkManagement::NetManagement::{
        LOCALGROUP_MEMBERS_INFO_3, NetLocalGroupAddMembers,
    };

    let group_w = super::winutil::to_wide(OsStr::new(group_name));
    let member_w = super::winutil::to_wide(OsStr::new(member_name));
    unsafe {
        let member = LOCALGROUP_MEMBERS_INFO_3 {
            lgrmi3_domainandname: member_w.as_ptr() as *mut u16,
        };
        let status = NetLocalGroupAddMembers(
            std::ptr::null(),
            group_w.as_ptr(),
            3,
            &member as *const _ as *mut u8,
            1,
        );
        if !local_group_member_add_status_is_success(status) {
            return Err(SetupFailure::new(
                SetupErrorCode::UsersGroupMemberAddFailed,
                format!(
                    "failed to add user {member_name} to local group {group_name}, code {status}"
                ),
            ));
        }
    }
    Ok(())
}

fn local_group_member_add_status_is_success(status: u32) -> bool {
    const NERR_SUCCESS: u32 = 0;
    const ERROR_MEMBER_IN_GROUP: u32 = 1320;
    const ERROR_MEMBER_IN_ALIAS: u32 = 1378;
    const NERR_USER_IN_GROUP: u32 = 2236;

    matches!(
        status,
        NERR_SUCCESS | ERROR_MEMBER_IN_GROUP | ERROR_MEMBER_IN_ALIAS | NERR_USER_IN_GROUP
    )
}

#[cfg(windows)]
fn resolve_sid(name: &str) -> Result<Vec<u8>, SetupFailure> {
    use std::ffi::OsStr;
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
    use windows_sys::Win32::Security::{LookupAccountNameW, SID_NAME_USE};

    if let Some(sid_str) = well_known_sid_str(name) {
        return super::winutil::sid_bytes_from_string(sid_str);
    }
    let name_w = super::winutil::to_wide(OsStr::new(name));
    let mut sid_buffer = vec![0u8; 68];
    let mut sid_len: u32 = sid_buffer.len() as u32;
    let mut domain: Vec<u16> = Vec::new();
    let mut domain_len: u32 = 0;
    let mut use_type: SID_NAME_USE = 0;
    loop {
        let ok = unsafe {
            LookupAccountNameW(
                std::ptr::null(),
                name_w.as_ptr(),
                sid_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                &mut sid_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                &mut use_type,
            )
        };
        if ok != 0 {
            sid_buffer.truncate(sid_len as usize);
            return Ok(sid_buffer);
        }
        let err = unsafe { GetLastError() };
        if err == ERROR_INSUFFICIENT_BUFFER {
            sid_buffer.resize(sid_len as usize, 0);
            domain.resize(domain_len as usize, 0);
            continue;
        }
        return Err(SetupFailure::new(
            SetupErrorCode::SidResolveFailed,
            format!("LookupAccountNameW failed for {name}: {err}"),
        ));
    }
}

#[cfg(windows)]
pub(super) fn resolve_sandbox_user_sid(username: &str) -> Result<String, SetupFailure> {
    let sid = resolve_sid(username)?;
    super::winutil::string_from_sid_bytes(&sid)
}

#[cfg(windows)]
fn well_known_sid_str(name: &str) -> Option<&'static str> {
    match name {
        "Administrators" => Some("S-1-5-32-544"),
        "Users" => Some("S-1-5-32-545"),
        "Authenticated Users" => Some("S-1-5-11"),
        "Everyone" => Some("S-1-1-0"),
        "SYSTEM" => Some("S-1-5-18"),
        _ => None,
    }
}

#[cfg(windows)]
fn lookup_account_name_for_sid(sid_str: &str) -> Result<String, SetupFailure> {
    use std::ffi::OsStr;
    use windows_sys::Win32::Foundation::{
        ERROR_INSUFFICIENT_BUFFER, GetLastError, HLOCAL, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::{LookupAccountSidW, SID_NAME_USE};

    let sid_w = super::winutil::to_wide(OsStr::new(sid_str));
    let mut psid: *mut std::ffi::c_void = std::ptr::null_mut();
    if unsafe { ConvertStringSidToSidW(sid_w.as_ptr(), &mut psid) } == 0 {
        return Err(SetupFailure::new(
            SetupErrorCode::SidResolveFailed,
            format!("ConvertStringSidToSidW failed for {sid_str}: {}", unsafe {
                GetLastError()
            }),
        ));
    }
    let mut name_len: u32 = 0;
    let mut domain_len: u32 = 0;
    let mut use_type: SID_NAME_USE = 0;
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            psid,
            std::ptr::null_mut(),
            &mut name_len,
            std::ptr::null_mut(),
            &mut domain_len,
            &mut use_type,
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err != ERROR_INSUFFICIENT_BUFFER {
            unsafe {
                LocalFree(psid as HLOCAL);
            }
            return Err(SetupFailure::new(
                SetupErrorCode::SidResolveFailed,
                format!("LookupAccountSidW preflight failed for {sid_str}: {err}"),
            ));
        }
    }
    let mut name_buf = vec![0u16; name_len as usize];
    let mut domain_buf = vec![0u16; domain_len as usize];
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            psid,
            name_buf.as_mut_ptr(),
            &mut name_len,
            domain_buf.as_mut_ptr(),
            &mut domain_len,
            &mut use_type,
        )
    };
    unsafe {
        LocalFree(psid as HLOCAL);
    }
    if ok == 0 {
        return Err(SetupFailure::new(
            SetupErrorCode::SidResolveFailed,
            format!("LookupAccountSidW failed for {sid_str}: {}", unsafe {
                GetLastError()
            }),
        ));
    }
    Ok(String::from_utf16_lossy(&name_buf)
        .trim_end_matches('\0')
        .to_string())
}

pub(super) fn write_sandbox_users_state(
    state_root: &Path,
    offline_user: &str,
    offline_pwd: &str,
    online_user: &str,
    online_pwd: &str,
    proxy_ports: &[u16],
    allow_local_binding: bool,
    hard_network_block: bool,
) -> Result<(), SetupFailure> {
    let sandbox_dir = sandbox_dir(state_root);
    std::fs::create_dir_all(&sandbox_dir).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SecretsWriteFailed,
            format!("failed to create {}: {err}", sandbox_dir.display()),
        )
    })?;
    let secrets_dir = sandbox_secrets_dir(state_root);
    std::fs::create_dir_all(&secrets_dir).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::SecretsWriteFailed,
            format!("failed to create {}: {err}", secrets_dir.display()),
        )
    })?;
    let offline_blob = super::dpapi::protect(offline_pwd.as_bytes()).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::DpapiProtectFailed,
            format!("dpapi protect failed for offline user: {err}"),
        )
    })?;
    let online_blob = super::dpapi::protect(online_pwd.as_bytes()).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::DpapiProtectFailed,
            format!("dpapi protect failed for online user: {err}"),
        )
    })?;
    let users = SandboxUsersFile {
        version: SETUP_VERSION,
        offline: SandboxUserRecord {
            username: offline_user.to_string(),
            password: BASE64_STANDARD.encode(offline_blob),
        },
        online: SandboxUserRecord {
            username: online_user.to_string(),
            password: BASE64_STANDARD.encode(online_blob),
        },
    };
    let marker = SetupMarker {
        version: SETUP_VERSION,
        offline_username: offline_user.to_string(),
        online_username: online_user.to_string(),
        created_at: None,
        hard_network_block,
        proxy_ports: proxy_ports.to_vec(),
        allow_local_binding,
    };
    let users_path = secrets_dir.join("sandbox_users.json");
    let marker_path = sandbox_dir.join("setup_marker.json");
    let users_json = serde_json::to_vec_pretty(&users).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::UsersWriteFailed,
            format!("failed to serialize sandbox users: {err}"),
        )
    })?;
    std::fs::write(&users_path, users_json).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::UsersWriteFailed,
            format!("failed to write {}: {err}", users_path.display()),
        )
    })?;
    let marker_json = serde_json::to_vec_pretty(&marker).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::MarkerWriteFailed,
            format!("failed to serialize setup marker: {err}"),
        )
    })?;
    std::fs::write(&marker_path, marker_json).map_err(|err| {
        SetupFailure::new(
            SetupErrorCode::MarkerWriteFailed,
            format!("failed to write {}: {err}", marker_path.display()),
        )
    })?;
    Ok(())
}

fn random_password() -> String {
    const CHARS: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()-_=+";
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_group_member_status_accepts_success_and_existing_membership_only() {
        assert!(local_group_member_add_status_is_success(0));
        assert!(local_group_member_add_status_is_success(1378));
        assert!(local_group_member_add_status_is_success(1320));
        assert!(local_group_member_add_status_is_success(2236));

        assert!(!local_group_member_add_status_is_success(2220));
        assert!(!local_group_member_add_status_is_success(5));
    }
}
