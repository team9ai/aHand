//! Windows ACL helpers for the sandbox backend.

use std::ffi::c_void;
use std::io;
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetSecurityInfo, SE_FILE_OBJECT, SE_KERNEL_OBJECT, SET_ACCESS,
    SetEntriesInAclW, SetNamedSecurityInfoW, SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
    TRUSTEE_W,
};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, EqualSid, GENERIC_MAPPING, GetAce, GetAclInformation,
    INHERIT_ONLY_ACE, MapGenericMask,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL, FILE_DELETE_CHILD,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
};

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AppliedAcl {
    pub(super) path: PathBuf,
    pub(super) access: AppliedAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) enum AppliedAccess {
    Writable,
    Readonly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum AclTrustee {
    SandboxUsersGroup,
    Capability,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
struct PlannedAcl {
    path: PathBuf,
    access: AppliedAccess,
    trustees: Vec<AclTrustee>,
}

#[cfg(windows)]
pub(super) const WRITE_ALLOW_MASK: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE | FILE_DELETE_CHILD;
#[cfg(windows)]
pub(super) const READ_EXECUTE_ALLOW_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
const CONTAINER_AND_OBJECT_INHERIT_ACE: u32 = 0x03;

fn plan_filesystem_acls(roots: &super::roots::DerivedFilesystemRoots) -> Vec<PlannedAcl> {
    let mut plan = Vec::with_capacity(roots.write_roots.len() + roots.read_roots.len());
    for root in &roots.write_roots {
        plan.push(PlannedAcl {
            path: root.clone(),
            access: AppliedAccess::Writable,
            trustees: vec![AclTrustee::SandboxUsersGroup, AclTrustee::Capability],
        });
    }
    for root in &roots.read_roots {
        plan.push(PlannedAcl {
            path: root.clone(),
            access: AppliedAccess::Readonly,
            trustees: vec![AclTrustee::SandboxUsersGroup, AclTrustee::Capability],
        });
    }
    plan
}

#[cfg(windows)]
#[allow(dead_code)]
pub(super) fn apply_filesystem_roots(
    roots: &super::roots::DerivedFilesystemRoots,
    sandbox_users_group_sid: *mut c_void,
    capability_sid: *mut c_void,
) -> io::Result<Vec<AppliedAcl>> {
    if sandbox_users_group_sid.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox users group SID pointer is null",
        ));
    }
    if capability_sid.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capability SID pointer is null",
        ));
    }

    let plan = plan_filesystem_acls(roots);
    let mut applied = Vec::with_capacity(plan.len());
    for entry in plan {
        // Group ACEs let the sandbox-user logon runner access prepared roots;
        // capability ACEs preserve the per-workspace boundary for the child.
        let sids = entry
            .trustees
            .iter()
            .map(|trustee| match trustee {
                AclTrustee::SandboxUsersGroup => sandbox_users_group_sid,
                AclTrustee::Capability => capability_sid,
            })
            .collect::<Vec<_>>();
        let allow_mask = match entry.access {
            AppliedAccess::Writable => WRITE_ALLOW_MASK,
            AppliedAccess::Readonly => READ_EXECUTE_ALLOW_MASK,
        };
        ensure_allow_mask_aces_with_inheritance(
            &entry.path,
            &sids,
            allow_mask,
            CONTAINER_AND_OBJECT_INHERIT_ACE,
        )?;
        applied.push(AppliedAcl {
            path: entry.path,
            access: entry.access,
        });
    }
    Ok(applied)
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub(super) fn apply_filesystem_roots(
    roots: &super::roots::DerivedFilesystemRoots,
    sandbox_users_group_sid: *mut c_void,
    capability_sid: *mut c_void,
) -> io::Result<Vec<AppliedAcl>> {
    let _ = (roots, sandbox_users_group_sid, capability_sid);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows ACL sandbox policy is unavailable on this platform",
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
}

#[cfg(windows)]
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
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

    fn as_acl(&self) -> *mut ACL {
        self.0 as *mut ACL
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
fn fetch_dacl_handle(path: &Path) -> io::Result<(*mut ACL, LocalMemory)> {
    let wide_path = super::path::wide_null(path);
    let handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = HandleGuard::new(handle);

    let mut security_descriptor: *mut c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let code = unsafe {
        GetSecurityInfo(
            handle.handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut security_descriptor,
        )
    };
    let security_descriptor = LocalMemory::new(security_descriptor);
    if code != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(code as i32));
    }

    let security_descriptor = security_descriptor.ok_or_else(|| {
        io::Error::other(format!(
            "GetSecurityInfo returned no security descriptor for {}",
            path.display()
        ))
    })?;

    Ok((dacl, security_descriptor))
}

#[cfg(windows)]
fn dacl_mask_allows(
    dacl: *mut ACL,
    psids: &[*mut c_void],
    desired_mask: u32,
    require_all_bits: bool,
) -> bool {
    dacl_mask_allows_with_inheritance(dacl, psids, desired_mask, require_all_bits, None)
}

#[cfg(windows)]
fn dacl_mask_allows_with_inheritance(
    dacl: *mut ACL,
    psids: &[*mut c_void],
    desired_mask: u32,
    require_all_bits: bool,
    required_inheritance: Option<u32>,
) -> bool {
    if dacl.is_null() {
        return false;
    }

    unsafe {
        let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
        let ok = GetAclInformation(
            dacl as *const ACL,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        );
        if ok == 0 {
            return false;
        }

        let mapping = GENERIC_MAPPING {
            GenericRead: FILE_GENERIC_READ,
            GenericWrite: FILE_GENERIC_WRITE,
            GenericExecute: FILE_GENERIC_EXECUTE,
            GenericAll: FILE_ALL_ACCESS,
        };

        for index in 0..info.AceCount {
            let mut ace_ptr: *mut c_void = std::ptr::null_mut();
            if GetAce(dacl as *const ACL, index, &mut ace_ptr) == 0 || ace_ptr.is_null() {
                continue;
            }

            let header = &*(ace_ptr as *const ACE_HEADER);
            if header.AceType != 0 {
                continue;
            }
            if (header.AceFlags & INHERIT_ONLY_ACE as u8) != 0 {
                continue;
            }
            if required_inheritance.is_some_and(|inheritance| {
                !ace_has_requested_inheritance(header.AceFlags, inheritance)
            }) {
                continue;
            }

            let ace = &*(ace_ptr as *const ACCESS_ALLOWED_ACE);
            let sid_ptr = std::ptr::addr_of!(ace.SidStart) as *mut c_void;
            if !psids.iter().any(|sid| EqualSid(sid_ptr, *sid) != 0) {
                continue;
            }

            let mut mask = ace.Mask;
            MapGenericMask(&mut mask, &mapping);
            if (require_all_bits && (mask & desired_mask) == desired_mask)
                || (!require_all_bits && (mask & desired_mask) != 0)
            {
                return true;
            }
        }
    }

    false
}

fn ace_has_requested_inheritance(ace_flags: u8, requested_inheritance: u32) -> bool {
    (ace_flags as u32 & requested_inheritance) == requested_inheritance
}

#[cfg(windows)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn path_mask_allows(
    path: &Path,
    psids: &[*mut c_void],
    desired_mask: u32,
    require_all_bits: bool,
) -> io::Result<bool> {
    let (dacl, _security_descriptor) = fetch_dacl_handle(path)?;
    Ok(dacl_mask_allows(
        dacl,
        psids,
        desired_mask,
        require_all_bits,
    ))
}

#[cfg(windows)]
fn ensure_allow_mask_aces_with_inheritance(
    path: &Path,
    sids: &[*mut c_void],
    allow_mask: u32,
    inheritance: u32,
) -> io::Result<bool> {
    let (dacl, _security_descriptor) = fetch_dacl_handle(path)?;
    let mut entries = Vec::new();

    for sid in sids {
        if sid.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SID pointer is null",
            ));
        }
        if dacl_mask_allows_with_inheritance(dacl, &[*sid], allow_mask, true, Some(inheritance)) {
            continue;
        }

        entries.push(EXPLICIT_ACCESS_W {
            grfAccessPermissions: allow_mask,
            grfAccessMode: SET_ACCESS,
            grfInheritance: inheritance,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: *sid as *mut u16,
            },
        });
    }

    if entries.is_empty() {
        return Ok(false);
    }

    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let code =
        unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), dacl, &mut new_dacl) };
    if code != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let new_dacl =
        LocalMemory::new(new_dacl as *mut c_void).ok_or_else(io::Error::last_os_error)?;

    let wide_path = super::path::wide_null(path);
    let code = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl.as_acl(),
            std::ptr::null_mut(),
        )
    };
    if code != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(code as i32));
    }

    Ok(true)
}

#[cfg(windows)]
#[allow(dead_code)]
pub(super) fn allow_null_device(capability_sid: *mut c_void) -> io::Result<()> {
    if capability_sid.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capability SID pointer is null",
        ));
    }

    let null_device = super::path::string_wide_null(r"\\.\NUL");
    let handle = unsafe {
        CreateFileW(
            null_device.as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let handle = HandleGuard::new(handle);

    let mut security_descriptor: *mut c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let code = unsafe {
        GetSecurityInfo(
            handle.handle(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut security_descriptor,
        )
    };
    let _security_descriptor = LocalMemory::new(security_descriptor);
    if code != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let _security_descriptor = _security_descriptor
        .ok_or_else(|| io::Error::other("GetSecurityInfo returned no NUL security descriptor"))?;
    if dacl.is_null() {
        return Ok(());
    }

    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE,
        grfAccessMode: SET_ACCESS,
        grfInheritance: 0,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: capability_sid as *mut u16,
        },
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let code = unsafe { SetEntriesInAclW(1, &entry, dacl, &mut new_dacl) };
    if code != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let new_dacl =
        LocalMemory::new(new_dacl as *mut c_void).ok_or_else(io::Error::last_os_error)?;

    let code = unsafe {
        SetSecurityInfo(
            handle.handle(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl.as_acl(),
            std::ptr::null_mut(),
        )
    };
    if code != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(code as i32));
    }

    Ok(())
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub(super) fn allow_null_device(capability_sid: *mut c_void) -> io::Result<()> {
    let _ = capability_sid;
    Ok(())
}

#[cfg(test)]
mod inheritance_tests {
    use super::*;

    #[test]
    fn ace_inheritance_match_requires_all_requested_flags() {
        assert!(ace_has_requested_inheritance(
            CONTAINER_AND_OBJECT_INHERIT_ACE as u8,
            CONTAINER_AND_OBJECT_INHERIT_ACE
        ));
        assert!(!ace_has_requested_inheritance(
            0x01,
            CONTAINER_AND_OBJECT_INHERIT_ACE
        ));
        assert!(!ace_has_requested_inheritance(
            0x02,
            CONTAINER_AND_OBJECT_INHERIT_ACE
        ));
    }

    #[test]
    fn filesystem_acl_plan_grants_group_and_capability_to_all_roots() {
        let roots = super::super::roots::DerivedFilesystemRoots {
            write_roots: vec![PathBuf::from(r"C:\workspace")],
            read_roots: vec![PathBuf::from(r"C:\runtime")],
        };

        let plan = plan_filesystem_acls(&roots);

        assert_eq!(
            plan,
            vec![
                PlannedAcl {
                    path: PathBuf::from(r"C:\workspace"),
                    access: AppliedAccess::Writable,
                    trustees: vec![AclTrustee::SandboxUsersGroup, AclTrustee::Capability],
                },
                PlannedAcl {
                    path: PathBuf::from(r"C:\runtime"),
                    access: AppliedAccess::Readonly,
                    trustees: vec![AclTrustee::SandboxUsersGroup, AclTrustee::Capability],
                },
            ]
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn null_dacl_is_not_treated_as_prepared_access() {
        assert!(!dacl_mask_allows(
            std::ptr::null_mut(),
            &[],
            WRITE_ALLOW_MASK,
            true
        ));
        assert!(!dacl_mask_allows_with_inheritance(
            std::ptr::null_mut(),
            &[],
            WRITE_ALLOW_MASK,
            true,
            Some(CONTAINER_AND_OBJECT_INHERIT_ACE)
        ));
    }

    #[test]
    fn applies_writable_and_readonly_acl_entries() {
        let workspace = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let cap =
            crate::sandbox::platform::windows::cap::capability_for_root(workspace.path()).unwrap();
        let token = crate::sandbox::platform::windows::token::create(&cap).unwrap();
        let mut sandbox_users_group_sid =
            crate::sandbox::platform::windows::winutil::sid_bytes_from_string("S-1-5-32-545")
                .unwrap();
        let roots = crate::sandbox::platform::windows::roots::DerivedFilesystemRoots {
            write_roots: vec![workspace.path().to_path_buf()],
            read_roots: vec![runtime.path().to_path_buf()],
        };

        let applied = apply_filesystem_roots(
            &roots,
            sandbox_users_group_sid.as_mut_ptr() as *mut std::ffi::c_void,
            token.capability_sid(),
        )
        .unwrap();

        assert!(
            applied
                .iter()
                .any(|entry| entry.access == AppliedAccess::Writable)
        );
        assert!(
            applied
                .iter()
                .any(|entry| entry.access == AppliedAccess::Readonly)
        );
        assert!(
            path_mask_allows(
                workspace.path(),
                &[token.capability_sid()],
                WRITE_ALLOW_MASK,
                true
            )
            .unwrap()
        );
        assert!(
            path_mask_allows(
                workspace.path(),
                &[sandbox_users_group_sid.as_mut_ptr() as *mut std::ffi::c_void],
                WRITE_ALLOW_MASK,
                true
            )
            .unwrap()
        );
        assert!(
            path_mask_allows(
                runtime.path(),
                &[token.capability_sid()],
                READ_EXECUTE_ALLOW_MASK,
                true
            )
            .unwrap()
        );
        assert!(
            path_mask_allows(
                runtime.path(),
                &[sandbox_users_group_sid.as_mut_ptr() as *mut std::ffi::c_void],
                READ_EXECUTE_ALLOW_MASK,
                true
            )
            .unwrap()
        );
    }
}
