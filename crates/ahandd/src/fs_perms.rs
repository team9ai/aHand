use std::io;
use std::path::Path;

/// Restrict file to owner-only read/write (Unix 0o600 equivalent).
#[cfg(unix)]
pub fn restrict_owner_only(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Restrict file to owner + group read/write (Unix 0o660 equivalent).
#[cfg(unix)]
pub fn restrict_owner_and_group(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
}

#[cfg(windows)]
pub fn restrict_owner_only(path: &Path) -> io::Result<()> {
    win_acl::set_owner_only_acl(path)
}

#[cfg(windows)]
pub fn restrict_owner_and_group(path: &Path) -> io::Result<()> {
    win_acl::set_owner_and_users_acl(path)
}

#[cfg(windows)]
mod win_acl {
    use std::io;
    use std::path::Path;

    pub fn set_owner_only_acl(path: &Path) -> io::Result<()> {
        use windows_sys::Win32::Security::*;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        unsafe {
            let mut token_handle = 0isize;
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) == 0 {
                return Err(io::Error::last_os_error());
            }

            let result = set_acl_with_token(path, token_handle, false);
            windows_sys::Win32::Foundation::CloseHandle(token_handle);
            result
        }
    }

    pub fn set_owner_and_users_acl(path: &Path) -> io::Result<()> {
        use windows_sys::Win32::Security::*;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        unsafe {
            let mut token_handle = 0isize;
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) == 0 {
                return Err(io::Error::last_os_error());
            }

            let result = set_acl_with_token(path, token_handle, true);
            windows_sys::Win32::Foundation::CloseHandle(token_handle);
            result
        }
    }

    unsafe fn set_acl_with_token(
        path: &Path,
        token_handle: isize,
        include_users_group: bool,
    ) -> io::Result<()> {
        use windows_sys::Win32::Security::Authorization::*;
        use windows_sys::Win32::Security::*;

        // Get current user SID from token
        let mut info_len = 0u32;
        GetTokenInformation(
            token_handle,
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut info_len,
        );
        let mut buffer = vec![0u8; info_len as usize];
        if GetTokenInformation(
            token_handle,
            TokenUser,
            buffer.as_mut_ptr() as *mut _,
            info_len,
            &mut info_len,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }

        let token_user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let user_sid = token_user.User.Sid;

        // GENERIC_READ | GENERIC_WRITE
        const GENERIC_RW: u32 = 0x10000000 | 0x20000000;

        // Build ACE for current user
        let mut entries = Vec::with_capacity(2);
        entries.push(EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_RW,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: user_sid as *mut u16,
            },
        });

        // Optionally add BUILTIN\Users group
        let mut users_sid_buf = [0u8; 68];
        if include_users_group {
            let mut sid_size = users_sid_buf.len() as u32;
            if CreateWellKnownSid(
                WinBuiltinUsersSid,
                std::ptr::null_mut(),
                users_sid_buf.as_mut_ptr() as *mut _,
                &mut sid_size,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            entries.push(EXPLICIT_ACCESS_W {
                grfAccessPermissions: GENERIC_RW,
                grfAccessMode: SET_ACCESS,
                grfInheritance: NO_INHERITANCE,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: std::ptr::null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                    ptstrName: users_sid_buf.as_mut_ptr() as *mut u16,
                },
            });
        }

        let mut acl = std::ptr::null_mut();
        let result = SetEntriesInAclW(
            entries.len() as u32,
            entries.as_mut_ptr(),
            std::ptr::null_mut(),
            &mut acl,
        );
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }

        let path_wide: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let result = SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null_mut(),
        );

        windows_sys::Win32::System::Memory::LocalFree(acl as *mut _);

        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        Ok(())
    }
}
