//! Windows DPAPI wrapper for encrypting sensitive data at rest.
//!
//! Data is bound to the current Windows user account.
//! Only the same user on the same machine can decrypt it.

use std::io;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

/// Encrypt data using DPAPI, bound to current user.
pub fn protect(plaintext: &[u8]) -> io::Result<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        let result = CryptProtectData(
            &input,
            std::ptr::null(),     // description
            std::ptr::null(),     // entropy
            std::ptr::null(),     // reserved
            std::ptr::null(),     // prompt
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );

        if result == 0 {
            return Err(io::Error::last_os_error());
        }

        let encrypted =
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut _);
        Ok(encrypted)
    }
}

/// Decrypt DPAPI-protected data. Only works for the same user who encrypted it.
pub fn unprotect(ciphertext: &[u8]) -> io::Result<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: ciphertext.len() as u32,
            pbData: ciphertext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        let result = CryptUnprotectData(
            &input,
            std::ptr::null_mut(), // description
            std::ptr::null(),     // entropy
            std::ptr::null(),     // reserved
            std::ptr::null(),     // prompt
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );

        if result == 0 {
            return Err(io::Error::last_os_error());
        }

        let decrypted =
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut _);
        Ok(decrypted)
    }
}
