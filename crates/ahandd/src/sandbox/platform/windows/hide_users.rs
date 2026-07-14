//! Hides Windows sandbox accounts from the interactive logon UI.

use std::fs::File;

#[cfg(windows)]
use std::io::Write;

use super::setup::{OFFLINE_USERNAME, ONLINE_USERNAME};

const USERLIST_KEY_PATH: &str =
    r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\SpecialAccounts\UserList";

/// Best-effort: sandbox accounts must not appear as interactive login choices.
pub(super) fn hide_sandbox_users_from_logon(log: &mut File) {
    #[cfg(windows)]
    if let Err(err) = hide_users(&[OFFLINE_USERNAME, ONLINE_USERNAME]) {
        let _ = writeln!(
            log,
            "failed to hide Windows sandbox users from the logon UI: {err}"
        );
    }

    #[cfg(not(windows))]
    let _ = log;
}

#[cfg(windows)]
fn hide_users(usernames: &[&str]) -> std::io::Result<()> {
    use std::ffi::OsStr;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, RegCloseKey,
        RegCreateKeyExW, RegSetValueExW,
    };

    let key_path = super::winutil::to_wide(OsStr::new(USERLIST_KEY_PATH));
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            key_path.as_ptr(),
            0,
            std::ptr::null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }

    for username in usernames {
        let name = super::winutil::to_wide(OsStr::new(username));
        let hidden: u32 = 0;
        let status = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_DWORD,
                &hidden as *const u32 as *const u8,
                std::mem::size_of_val(&hidden) as u32,
            )
        };
        if status != 0 {
            unsafe {
                RegCloseKey(key);
            }
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
    }

    unsafe {
        RegCloseKey(key);
    }
    Ok(())
}
