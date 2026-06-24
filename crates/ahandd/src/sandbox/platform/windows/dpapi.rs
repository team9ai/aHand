//! DPAPI password protection helpers for Windows sandbox secrets.
#![allow(dead_code)]

use super::setup_error::{SetupErrorCode, SetupFailure};

#[cfg(windows)]
fn make_blob(data: &[u8]) -> windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
    windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

#[cfg(windows)]
#[allow(clippy::unnecessary_mut_passed)]
pub(super) fn protect(data: &[u8]) -> Result<Vec<u8>, SetupFailure> {
    use windows_sys::Win32::Foundation::{GetLastError, HLOCAL, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    };

    let mut in_blob = make_blob(data);
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut in_blob,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
            &mut out_blob,
        )
    };
    if ok == 0 {
        return Err(SetupFailure::new(
            SetupErrorCode::DpapiProtectFailed,
            format!("CryptProtectData failed: {}", unsafe { GetLastError() }),
        ));
    }
    let out =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) }.to_vec();
    unsafe {
        if !out_blob.pbData.is_null() {
            LocalFree(out_blob.pbData as HLOCAL);
        }
    }
    Ok(out)
}

#[cfg(windows)]
#[allow(clippy::unnecessary_mut_passed)]
pub(super) fn unprotect(blob: &[u8]) -> Result<Vec<u8>, SetupFailure> {
    use windows_sys::Win32::Foundation::{GetLastError, HLOCAL, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN,
        CryptUnprotectData,
    };

    let mut in_blob = make_blob(blob);
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut in_blob,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
            &mut out_blob,
        )
    };
    if ok == 0 {
        return Err(SetupFailure::new(
            SetupErrorCode::DpapiUnprotectFailed,
            format!("CryptUnprotectData failed: {}", unsafe { GetLastError() }),
        ));
    }
    let out =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) }.to_vec();
    unsafe {
        if !out_blob.pbData.is_null() {
            LocalFree(out_blob.pbData as HLOCAL);
        }
    }
    Ok(out)
}

#[cfg(not(windows))]
pub(super) fn protect(_: &[u8]) -> Result<Vec<u8>, SetupFailure> {
    Err(SetupFailure::new(
        SetupErrorCode::DpapiProtectFailed,
        "DPAPI is only available on Windows",
    ))
}

#[cfg(not(windows))]
pub(super) fn unprotect(_: &[u8]) -> Result<Vec<u8>, SetupFailure> {
    Err(SetupFailure::new(
        SetupErrorCode::DpapiUnprotectFailed,
        "DPAPI is only available on Windows",
    ))
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn dpapi_round_trips_protected_blob() {
        let plaintext = b"offline-password";
        let protected = super::protect(plaintext).unwrap();
        assert_ne!(protected, plaintext);

        let unprotected = super::unprotect(&protected).unwrap();
        assert_eq!(unprotected, plaintext);
    }
}
