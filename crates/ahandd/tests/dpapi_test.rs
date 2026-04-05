//! DPAPI tests — only compile and run on Windows.

#[cfg(windows)]
mod tests {
    #[test]
    fn dpapi_roundtrip() {
        let plaintext = b"Ed25519-secret-key-bytes-here-32";
        let encrypted = ahandd::dpapi::protect(plaintext).unwrap();

        assert_ne!(
            &encrypted[..],
            plaintext.as_slice(),
            "ciphertext should differ from plaintext"
        );
        assert!(
            encrypted.len() > plaintext.len(),
            "ciphertext should be larger than plaintext"
        );

        let decrypted = ahandd::dpapi::unprotect(&encrypted).unwrap();
        assert_eq!(
            &decrypted[..],
            plaintext.as_slice(),
            "roundtrip decryption failed"
        );
    }

    #[test]
    fn dpapi_empty_input() {
        let encrypted = ahandd::dpapi::protect(b"").unwrap();
        let decrypted = ahandd::dpapi::unprotect(&encrypted).unwrap();
        assert!(decrypted.is_empty(), "empty input should roundtrip to empty");
    }

    #[test]
    fn dpapi_large_payload() {
        let plaintext = vec![0xABu8; 4096];
        let encrypted = ahandd::dpapi::protect(&plaintext).unwrap();
        let decrypted = ahandd::dpapi::unprotect(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext, "large payload roundtrip failed");
    }

    #[test]
    fn dpapi_unprotect_invalid_data() {
        let garbage = b"this is not valid DPAPI ciphertext";
        let result = ahandd::dpapi::unprotect(garbage);
        assert!(
            result.is_err(),
            "unprotect should fail on invalid ciphertext"
        );
    }
}
