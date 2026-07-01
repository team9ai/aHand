//! Small Win32 utility helpers shared by future sandbox setup code.
#![allow(dead_code)]

use std::ffi::OsStr;

use super::setup_error::{SetupErrorCode, SetupFailure};

#[cfg(windows)]
pub(super) fn to_wide<S: AsRef<OsStr>>(s: S) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    let mut v: Vec<u16> = s.as_ref().encode_wide().collect();
    v.push(0);
    v
}

#[cfg(not(windows))]
pub(super) fn to_wide<S: AsRef<OsStr>>(s: S) -> Vec<u16> {
    let mut v: Vec<u16> = s.as_ref().to_string_lossy().encode_utf16().collect();
    v.push(0);
    v
}

#[cfg(windows)]
pub(super) fn string_from_sid_bytes(sid: &[u8]) -> Result<String, SetupFailure> {
    use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;

    unsafe {
        let mut str_ptr: *mut u16 = std::ptr::null_mut();
        let ok = ConvertSidToStringSidW(sid.as_ptr() as *mut std::ffi::c_void, &mut str_ptr);
        if ok == 0 || str_ptr.is_null() {
            return Err(SetupFailure::new(
                SetupErrorCode::SidResolveFailed,
                format!(
                    "ConvertSidToStringSidW failed: {}",
                    std::io::Error::last_os_error()
                ),
            ));
        }
        let mut len = 0;
        while *str_ptr.add(len) != 0 {
            len += 1;
        }
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(str_ptr, len));
        LocalFree(str_ptr as HLOCAL);
        Ok(out)
    }
}

#[cfg(not(windows))]
pub(super) fn string_from_sid_bytes(_: &[u8]) -> Result<String, SetupFailure> {
    Err(SetupFailure::new(
        SetupErrorCode::SidResolveFailed,
        "SID conversion is only available on Windows",
    ))
}

#[cfg(windows)]
pub(super) fn sid_bytes_from_string(sid_str: &str) -> Result<Vec<u8>, SetupFailure> {
    use windows_sys::Win32::Foundation::{GetLastError, HLOCAL, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::{CopySid, GetLengthSid};

    let sid_w = to_wide(OsStr::new(sid_str));
    let mut psid: *mut std::ffi::c_void = std::ptr::null_mut();
    if unsafe { ConvertStringSidToSidW(sid_w.as_ptr(), &mut psid) } == 0 {
        return Err(SetupFailure::new(
            SetupErrorCode::SidResolveFailed,
            format!("ConvertStringSidToSidW failed for {sid_str}: {}", unsafe {
                GetLastError()
            }),
        ));
    }
    let sid_len = unsafe { GetLengthSid(psid) };
    if sid_len == 0 {
        unsafe {
            LocalFree(psid as HLOCAL);
        }
        return Err(SetupFailure::new(
            SetupErrorCode::SidResolveFailed,
            format!("GetLengthSid failed for {sid_str}"),
        ));
    }
    let mut out = vec![0u8; sid_len as usize];
    let ok = unsafe { CopySid(sid_len, out.as_mut_ptr() as *mut std::ffi::c_void, psid) };
    unsafe {
        LocalFree(psid as HLOCAL);
    }
    if ok == 0 {
        return Err(SetupFailure::new(
            SetupErrorCode::SidResolveFailed,
            format!("CopySid failed for {sid_str}"),
        ));
    }
    Ok(out)
}

#[cfg(not(windows))]
pub(super) fn sid_bytes_from_string(_: &str) -> Result<Vec<u8>, SetupFailure> {
    Err(SetupFailure::new(
        SetupErrorCode::SidResolveFailed,
        "SID conversion is only available on Windows",
    ))
}
