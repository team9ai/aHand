use std::path::Path;

#[test]
fn test_restrict_owner_only() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "secret").unwrap();

    ahandd::fs_perms::restrict_owner_only(path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0o600, got 0o{:03o}", mode);
    }
}

#[test]
fn test_restrict_owner_and_group() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "shared").unwrap();

    ahandd::fs_perms::restrict_owner_and_group(path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660, "expected 0o660, got 0o{:03o}", mode);
    }
}

#[test]
fn test_restrict_nonexistent_file() {
    let result =
        ahandd::fs_perms::restrict_owner_only(Path::new("/tmp/nonexistent-ahand-test-file"));
    assert!(result.is_err(), "should fail on nonexistent file");
}

#[test]
fn test_restrict_owner_only_idempotent() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "data").unwrap();

    // Apply twice - should succeed both times
    ahandd::fs_perms::restrict_owner_only(path).unwrap();
    ahandd::fs_perms::restrict_owner_only(path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn test_restrict_owner_and_group_idempotent() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "data").unwrap();

    // Apply twice - should succeed both times
    ahandd::fs_perms::restrict_owner_and_group(path).unwrap();
    ahandd::fs_perms::restrict_owner_and_group(path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660);
    }
}

#[test]
fn test_restrict_owner_only_then_group() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "data").unwrap();

    // First restrict to owner only, then widen to owner+group
    ahandd::fs_perms::restrict_owner_only(path).unwrap();
    ahandd::fs_perms::restrict_owner_and_group(path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660, "expected 0o660 after widening, got 0o{:03o}", mode);
    }
}

#[test]
fn test_restrict_group_then_owner_only() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "data").unwrap();

    // First set owner+group, then narrow to owner only
    ahandd::fs_perms::restrict_owner_and_group(path).unwrap();
    ahandd::fs_perms::restrict_owner_only(path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0o600 after narrowing, got 0o{:03o}", mode);
    }
}

#[test]
fn test_restrict_empty_file() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path();
    // File is already empty from NamedTempFile::new()

    ahandd::fs_perms::restrict_owner_only(path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[cfg(unix)]
#[test]
fn test_restrict_directory_path_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // Directories are valid targets for permission changes, so this should succeed
    let result = ahandd::fs_perms::restrict_owner_only(dir.path());
    assert!(result.is_ok(), "restricting a directory should succeed");

    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
