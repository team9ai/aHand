//! Restricted token creation for Windows sandboxed commands.

use std::ffi::c_void;
use std::io;

use super::cap::CapabilitySid;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HLOCAL, LUID, LocalFree};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, SetEntriesInAclW, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    ACL, AdjustTokenPrivileges, CopySid, CreateRestrictedToken, CreateWellKnownSid, GetLengthSid,
    GetTokenInformation, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SID_AND_ATTRIBUTES,
    SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_PRIVILEGES, TOKEN_ADJUST_SESSIONID,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenDefaultDacl,
    TokenGroups, WinWorldSid,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[cfg(windows)]
type RawTokenHandle = HANDLE;
#[cfg(not(windows))]
type RawTokenHandle = usize;

#[cfg(windows)]
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;

#[cfg_attr(not(windows), allow(dead_code))]
pub(super) struct RestrictedToken {
    handle: RawTokenHandle,
    capability_sid: *mut c_void,
}

impl RestrictedToken {
    #[allow(dead_code)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn handle(&self) -> RawTokenHandle {
        self.handle
    }

    pub(super) fn capability_sid(&self) -> *mut c_void {
        self.capability_sid
    }
}

#[cfg(windows)]
impl Drop for RestrictedToken {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                CloseHandle(self.handle);
            }
            if !self.capability_sid.is_null() {
                LocalFree(self.capability_sid as HLOCAL);
            }
        }
    }
}

#[cfg(not(windows))]
impl Drop for RestrictedToken {
    fn drop(&mut self) {}
}

#[cfg(windows)]
pub(super) fn create(capability: &CapabilitySid) -> io::Result<RestrictedToken> {
    let psid =
        LocalMemory::new(convert_string_sid_to_sid(capability.sid_string())?).ok_or_else(|| {
            io::Error::other(format!(
                "failed to convert capability SID '{}'",
                capability.sid_string()
            ))
        })?;
    let base = HandleGuard::new(get_current_token_for_restriction()?);
    let token = create_workspace_write_token_with_caps_from(base.handle(), &[psid.as_ptr()])?;

    Ok(RestrictedToken {
        handle: token,
        capability_sid: psid.into_raw(),
    })
}

#[cfg(not(windows))]
pub(super) fn create(capability: &CapabilitySid) -> io::Result<RestrictedToken> {
    let _ = capability;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows restricted token support is unavailable on this platform",
    ))
}

#[cfg(windows)]
struct HandleGuard(HANDLE);

#[cfg(windows)]
impl HandleGuard {
    fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    fn handle(&self) -> HANDLE {
        self.0
    }

    fn into_raw(mut self) -> HANDLE {
        let handle = self.0;
        self.0 = std::ptr::null_mut();
        handle
    }
}

#[cfg(windows)]
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
struct LocalMemory(*mut c_void);

#[cfg(windows)]
impl LocalMemory {
    fn new(ptr: *mut c_void) -> Option<Self> {
        (!ptr.is_null()).then_some(Self(ptr))
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0
    }

    fn into_raw(mut self) -> *mut c_void {
        let ptr = self.0;
        self.0 = std::ptr::null_mut();
        ptr
    }
}

#[cfg(windows)]
impl Drop for LocalMemory {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                LocalFree(self.0 as HLOCAL);
            }
        }
    }
}

#[cfg(windows)]
#[repr(C)]
struct TokenDefaultDaclInfo {
    default_dacl: *mut ACL,
}

#[cfg(windows)]
fn convert_string_sid_to_sid(sid: &str) -> io::Result<*mut c_void> {
    let sid_wide = super::path::string_wide_null(sid);
    let mut psid: *mut c_void = std::ptr::null_mut();
    let ok = unsafe { ConvertStringSidToSidW(sid_wide.as_ptr(), &mut psid) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(psid)
}

#[cfg(windows)]
fn get_current_token_for_restriction() -> io::Result<HANDLE> {
    let desired = TOKEN_DUPLICATE
        | TOKEN_QUERY
        | TOKEN_ASSIGN_PRIMARY
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_SESSIONID
        | TOKEN_ADJUST_PRIVILEGES;
    let mut handle: HANDLE = std::ptr::null_mut();
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), desired, &mut handle) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(handle)
}

#[cfg(windows)]
fn get_logon_sid_bytes(token: HANDLE) -> io::Result<Vec<u8>> {
    fn scan_token_groups_for_logon(token: HANDLE) -> Option<Vec<u8>> {
        unsafe {
            let mut needed: u32 = 0;
            GetTokenInformation(token, TokenGroups, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                return None;
            }

            let mut buffer = vec![0u8; needed as usize];
            let ok = GetTokenInformation(
                token,
                TokenGroups,
                buffer.as_mut_ptr() as *mut c_void,
                needed,
                &mut needed,
            );
            if ok == 0 || (needed as usize) < std::mem::size_of::<u32>() {
                return None;
            }

            let group_count = std::ptr::read_unaligned(buffer.as_ptr() as *const u32) as usize;
            let after_count = buffer.as_ptr().add(std::mem::size_of::<u32>()) as usize;
            let align = std::mem::align_of::<SID_AND_ATTRIBUTES>();
            let aligned = (after_count + (align - 1)) & !(align - 1);
            let groups = aligned as *const SID_AND_ATTRIBUTES;

            for index in 0..group_count {
                let entry = std::ptr::read_unaligned(groups.add(index));
                if (entry.Attributes & SE_GROUP_LOGON_ID) != SE_GROUP_LOGON_ID {
                    continue;
                }

                let sid_len = GetLengthSid(entry.Sid);
                if sid_len == 0 {
                    return None;
                }

                let mut out = vec![0u8; sid_len as usize];
                if CopySid(sid_len, out.as_mut_ptr() as *mut c_void, entry.Sid) == 0 {
                    return None;
                }
                return Some(out);
            }

            None
        }
    }

    if let Some(sid) = scan_token_groups_for_logon(token) {
        return Ok(sid);
    }

    #[repr(C)]
    struct TokenLinkedToken {
        linked_token: HANDLE,
    }

    const TOKEN_LINKED_TOKEN_CLASS: i32 = 19;

    unsafe {
        let mut needed: u32 = 0;
        GetTokenInformation(
            token,
            TOKEN_LINKED_TOKEN_CLASS,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );

        if needed >= std::mem::size_of::<TokenLinkedToken>() as u32 {
            let mut buffer = vec![0u8; needed as usize];
            let ok = GetTokenInformation(
                token,
                TOKEN_LINKED_TOKEN_CLASS,
                buffer.as_mut_ptr() as *mut c_void,
                needed,
                &mut needed,
            );
            if ok != 0 {
                let linked = std::ptr::read_unaligned(buffer.as_ptr() as *const TokenLinkedToken);
                if !linked.linked_token.is_null() {
                    let linked = HandleGuard::new(linked.linked_token);
                    if let Some(sid) = scan_token_groups_for_logon(linked.handle()) {
                        return Ok(sid);
                    }
                }
            }
        }
    }

    Err(io::Error::other("Logon SID not present on token"))
}

#[cfg(windows)]
fn world_sid() -> io::Result<Vec<u8>> {
    unsafe {
        let mut size: u32 = 0;
        CreateWellKnownSid(
            WinWorldSid,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        );
        if size == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut buffer = vec![0u8; size as usize];
        let ok = CreateWellKnownSid(
            WinWorldSid,
            std::ptr::null_mut(),
            buffer.as_mut_ptr() as *mut c_void,
            &mut size,
        );
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buffer)
    }
}

#[cfg(windows)]
fn set_default_dacl(token: HANDLE, sids: &[*mut c_void]) -> io::Result<()> {
    if sids.is_empty() {
        return Ok(());
    }

    let entries = sids
        .iter()
        .map(|sid| EXPLICIT_ACCESS_W {
            grfAccessPermissions: windows_sys::Win32::Foundation::GENERIC_ALL,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: 0,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: *sid as *mut u16,
            },
        })
        .collect::<Vec<_>>();

    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let code = unsafe {
        SetEntriesInAclW(
            entries.len() as u32,
            entries.as_ptr(),
            std::ptr::null(),
            &mut new_dacl,
        )
    };
    if code != windows_sys::Win32::Foundation::ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let new_dacl =
        LocalMemory::new(new_dacl as *mut c_void).ok_or_else(|| io::Error::last_os_error())?;

    let info = TokenDefaultDaclInfo {
        default_dacl: new_dacl.as_ptr() as *mut ACL,
    };
    let ok = unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            &info as *const _ as *const c_void,
            std::mem::size_of::<TokenDefaultDaclInfo>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(windows)]
fn enable_single_privilege(token: HANDLE, name: &str) -> io::Result<()> {
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    let name_wide = super::path::string_wide_null(name);
    let ok = unsafe { LookupPrivilegeValueW(std::ptr::null(), name_wide.as_ptr(), &mut luid) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut privileges: TOKEN_PRIVILEGES = unsafe { std::mem::zeroed() };
    privileges.PrivilegeCount = 1;
    privileges.Privileges[0].Luid = luid;
    privileges.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;

    let ok = unsafe {
        AdjustTokenPrivileges(
            token,
            0,
            &privileges,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let err = unsafe { GetLastError() };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err as i32));
    }

    Ok(())
}

#[cfg(windows)]
fn create_token_with_caps_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
) -> io::Result<HANDLE> {
    if psid_capabilities.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no capability SIDs provided",
        ));
    }

    let mut logon_sid_bytes = get_logon_sid_bytes(base_token)?;
    let psid_logon = logon_sid_bytes.as_mut_ptr() as *mut c_void;
    let mut everyone = world_sid()?;
    let psid_everyone = everyone.as_mut_ptr() as *mut c_void;

    let mut entries = vec![
        SID_AND_ATTRIBUTES {
            Sid: std::ptr::null_mut(),
            Attributes: 0,
        };
        psid_capabilities.len() + 2
    ];
    for (index, psid) in psid_capabilities.iter().enumerate() {
        entries[index].Sid = *psid;
    }
    let logon_index = psid_capabilities.len();
    entries[logon_index].Sid = psid_logon;
    entries[logon_index + 1].Sid = psid_everyone;

    const DISABLE_MAX_PRIVILEGE: u32 = 0x01;
    const LUA_TOKEN: u32 = 0x04;
    const WRITE_RESTRICTED: u32 = 0x08;

    let mut new_token: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        CreateRestrictedToken(
            base_token,
            DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            entries.len() as u32,
            entries.as_mut_ptr(),
            &mut new_token,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let new_token = HandleGuard::new(new_token);
    let mut dacl_sids = Vec::with_capacity(psid_capabilities.len() + 2);
    dacl_sids.push(psid_logon);
    dacl_sids.push(psid_everyone);
    dacl_sids.extend_from_slice(psid_capabilities);

    set_default_dacl(new_token.handle(), &dacl_sids)?;
    enable_single_privilege(new_token.handle(), "SeChangeNotifyPrivilege")?;

    Ok(new_token.into_raw())
}

#[cfg(windows)]
fn create_workspace_write_token_with_caps_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
) -> io::Result<HANDLE> {
    create_token_with_caps_from(base_token, psid_capabilities)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn creates_restricted_token_for_capability_sid() {
        let temp = tempfile::tempdir().unwrap();
        let cap = crate::sandbox::platform::windows::cap::capability_for_root(temp.path()).unwrap();
        let token = create(&cap).unwrap();

        assert!(!token.handle().is_null());
        assert!(!token.capability_sid().is_null());
    }
}
