use tempfile::NamedTempFile;

// We test against the ahandd crate's public fs_perms module.
// On Unix we verify with std::os::unix::fs::PermissionsExt.
// On Windows the DACL verification would be tested in CI.

#[test]
fn test_restrict_owner_only() {
    let tmp = NamedTempFile::new().unwrap();
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
    let tmp = NamedTempFile::new().unwrap();
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
fn test_restrict_owner_only_nonexistent_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does_not_exist.txt");

    let result = ahandd::fs_perms::restrict_owner_only(&path);
    assert!(result.is_err(), "should fail for nonexistent file");
}

#[test]
fn test_restrict_owner_and_group_nonexistent_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does_not_exist.txt");

    let result = ahandd::fs_perms::restrict_owner_and_group(&path);
    assert!(result.is_err(), "should fail for nonexistent file");
}

#[cfg(unix)]
#[test]
fn test_restrict_owner_only_idempotent() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "data").unwrap();

    // Apply twice -- should succeed both times with same result.
    ahandd::fs_perms::restrict_owner_only(path).unwrap();
    ahandd::fs_perms::restrict_owner_only(path).unwrap();

    let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[cfg(unix)]
#[test]
fn test_restrict_owner_and_group_idempotent() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "data").unwrap();

    // Apply twice -- should succeed both times with same result.
    ahandd::fs_perms::restrict_owner_and_group(path).unwrap();
    ahandd::fs_perms::restrict_owner_and_group(path).unwrap();

    let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o660);
}

#[cfg(unix)]
#[test]
fn test_restrict_owner_only_from_permissive() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "data").unwrap();

    // Start with wide-open permissions.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();

    ahandd::fs_perms::restrict_owner_only(path).unwrap();

    let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "should restrict from 0o777 to 0o600");
}

#[cfg(unix)]
#[test]
fn test_restrict_owner_and_group_from_permissive() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, "data").unwrap();

    // Start with wide-open permissions.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();

    ahandd::fs_perms::restrict_owner_and_group(path).unwrap();

    let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o660, "should restrict from 0o777 to 0o660");
}
