# Windows Cross-Platform Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make aHand (ahandd + ahandctl) compile, run, and pass full tests on Windows, with security parity to Unix.

**Architecture:** Conditional compilation (`#[cfg(unix)]` / `#[cfg(windows)]`) with inline platform branches. No trait abstractions. IPC uses tokio Named Pipes on Windows, Unix sockets on Unix. Key files protected via DPAPI on Windows, file mode on Unix.

**Tech Stack:** Rust, tokio, windows-sys (Win32 FFI), zip crate, PowerShell, GitHub Actions

**Spec:** `docs/superpowers/specs/2026-04-05-windows-support-design.md`
**Reference branch:** `feat/windows-support` (diverged, use as reference only)
**Base branch:** `dev`

---

## File Structure

### New Files
| File | Responsibility |
|------|---------------|
| `crates/ahandd/src/fs_perms.rs` | Cross-platform file permission functions (`restrict_owner_only`, `restrict_owner_and_group`) |
| `crates/ahandd/src/dpapi.rs` | Windows DPAPI encrypt/decrypt wrapper (`protect`/`unprotect`), `#[cfg(windows)]` only |
| `crates/ahandd/tests/ipc_roundtrip.rs` | IPC integration tests (same code, both platforms) |
| `crates/ahandd/tests/fs_perms_test.rs` | File permission verification tests |
| `crates/ahandd/tests/dpapi_test.rs` | DPAPI roundtrip tests (Windows only) |
| `scripts/dist/install.ps1` | PowerShell first-install script for Windows users |
| `.github/workflows/test-rust.yml` | Cross-platform Rust test workflow |

### Modified Files
| File | What Changes |
|------|-------------|
| `crates/ahandd/Cargo.toml` | Add `windows-sys`, `zip` as `cfg(windows)` deps |
| `crates/ahandd/src/ipc.rs` | Generic `handle_ipc_conn<R,W>`, `serve_ipc_unix`/`serve_ipc_windows`, `spawn_ipc_handler`, Windows peer identity |
| `crates/ahandd/src/main.rs` | Signal handling `#[cfg]` branches, remove `use tokio::signal::unix` top-level import |
| `crates/ahandd/src/config.rs` | `ipc_socket_path()` returns `\\.\pipe\ahandd` on Windows |
| `crates/ahandd/src/lib.rs` | Add `pub mod fs_perms;` and `#[cfg(windows)] pub mod dpapi;` |
| `crates/ahandd/src/browser_init.rs` | Node.js `.zip` download on Windows, `detect_system_chrome` Windows branch, `clean` with taskkill |
| `crates/ahandd/src/openclaw/device_identity.rs` | Use `fs_perms::restrict_owner_only` + DPAPI for key storage |
| `crates/ahandd/src/openclaw/exec_approvals.rs` | Use `fs_perms::restrict_owner_only` |
| `crates/ahandd/src/openclaw/pairing.rs` | Use `fs_perms::restrict_owner_only` |
| `crates/ahandctl/src/main.rs` | Extract `ipc_connect()`, replace 5× `UnixStream::connect` |
| `crates/ahandctl/src/daemon.rs` | `is_process_running` / `send_signal` / `start` Windows branches |
| `crates/ahandctl/src/upgrade.rs` | Full Rust rewrite, remove `Command::new("bash")` |
| `crates/ahandctl/src/admin.rs` | Replace bash `setup-browser.sh` call with `browser_init::run()` |
| `.github/workflows/release-rust.yml` | Add Windows build target, protoc, .exe artifacts |

---

### Task 0: Dependencies & Config Scaffolding

**Goal:** Add Windows-only crate dependencies and make `ipc_socket_path()` return the correct default per platform.

**Files:**
- Modify: `crates/ahandd/Cargo.toml`
- Modify: `crates/ahandd/src/config.rs:248-257`

**Acceptance Criteria:**
- [ ] `windows-sys` and `zip` are conditional dependencies under `[target.'cfg(windows)'.dependencies]`
- [ ] `ipc_socket_path()` returns `\\.\pipe\ahandd` on Windows, `~/.ahand/ahandd.sock` on Unix
- [ ] `cargo check` passes on current platform (Unix) with no regressions

**Verify:** `cargo check -p ahandd` → compiles clean

**Steps:**

- [ ] **Step 1: Add Windows conditional dependencies to Cargo.toml**

Append to end of `crates/ahandd/Cargo.toml`:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_Security",
    "Win32_Security_Authorization",
    "Win32_Security_Cryptography",
    "Win32_System_Pipes",
    "Win32_System_Threading",
] }
zip = "2"
```

- [ ] **Step 2: Make ipc_socket_path() platform-aware**

In `crates/ahandd/src/config.rs`, replace the `ipc_socket_path` method (lines 248-257):

```rust
/// Resolve the IPC socket path.
/// Unix default: ~/.ahand/ahandd.sock
/// Windows default: \\.\pipe\ahandd
pub fn ipc_socket_path(&self) -> PathBuf {
    match &self.ipc_socket_path {
        Some(p) => PathBuf::from(p),
        None => {
            #[cfg(unix)]
            {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".ahand")
                    .join("ahandd.sock")
            }
            #[cfg(windows)]
            {
                PathBuf::from(r"\\.\pipe\ahandd")
            }
        }
    }
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p ahandd`
Expected: clean compilation, no warnings related to our changes

- [ ] **Step 4: Commit**

```bash
git add crates/ahandd/Cargo.toml crates/ahandd/src/config.rs
git commit -m "feat(windows): add conditional dependencies and platform-aware socket path"
```

---

### Task 1: Cross-Platform File Permissions Module

**Goal:** Create `fs_perms.rs` with `restrict_owner_only` and `restrict_owner_and_group` that work on both Unix (chmod) and Windows (ACL via Win32 API).

**Files:**
- Create: `crates/ahandd/src/fs_perms.rs`
- Modify: `crates/ahandd/src/lib.rs` (add `pub mod fs_perms;`)
- Create: `crates/ahandd/tests/fs_perms_test.rs`

**Acceptance Criteria:**
- [ ] `restrict_owner_only` sets `0o600` on Unix
- [ ] `restrict_owner_and_group` sets `0o660` on Unix
- [ ] Windows implementation uses `SetNamedSecurityInfoW` to build correct DACL
- [ ] Unit tests verify permission changes on Unix

**Verify:** `cargo test -p ahandd --test fs_perms_test` → all pass

**Steps:**

- [ ] **Step 1: Write tests first**

Create `crates/ahandd/tests/fs_perms_test.rs`:

```rust
use std::path::Path;
use tempfile::NamedTempFile;

// We test against the ahandd crate's public fs_perms module.
// On Unix we can verify with std::os::unix::fs::PermissionsExt.

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

    // On Windows this test would verify DACL — tested in CI.
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
```

Note: Add `tempfile = "3"` to `[dev-dependencies]` in `crates/ahandd/Cargo.toml`.

- [ ] **Step 2: Run tests — verify they fail**

Run: `cargo test -p ahandd --test fs_perms_test`
Expected: compilation error — `fs_perms` module doesn't exist yet

- [ ] **Step 3: Create fs_perms.rs**

Create `crates/ahandd/src/fs_perms.rs`:

```rust
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
    use windows_sys::Win32::Security::Authorization::*;
    use windows_sys::Win32::Security::*;

    /// Build a DACL granting only the current user GENERIC_READ | GENERIC_WRITE.
    pub fn set_owner_only_acl(path: &Path) -> io::Result<()> {
        unsafe {
            let mut token_handle = 0;
            if OpenProcessToken(
                windows_sys::Win32::System::Threading::GetCurrentProcess(),
                TOKEN_QUERY,
                &mut token_handle,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }

            // Get token user SID
            let mut info_len = 0u32;
            GetTokenInformation(token_handle, TokenUser, std::ptr::null_mut(), 0, &mut info_len);
            let mut buffer = vec![0u8; info_len as usize];
            if GetTokenInformation(
                token_handle,
                TokenUser,
                buffer.as_mut_ptr() as *mut _,
                info_len,
                &mut info_len,
            ) == 0
            {
                windows_sys::Win32::Foundation::CloseHandle(token_handle);
                return Err(io::Error::last_os_error());
            }
            windows_sys::Win32::Foundation::CloseHandle(token_handle);

            let token_user = &*(buffer.as_ptr() as *const TOKEN_USER);
            let user_sid = token_user.User.Sid;

            // Build explicit access entry
            let mut ea = EXPLICIT_ACCESS_W {
                grfAccessPermissions: GENERIC_READ | GENERIC_WRITE,
                grfAccessMode: SET_ACCESS,
                grfInheritance: NO_INHERITANCE,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: std::ptr::null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_USER,
                    ptstrName: user_sid as *mut u16,
                },
            };

            let mut acl = std::ptr::null_mut();
            let result = SetEntriesInAclW(1, &mut ea, std::ptr::null_mut(), &mut acl);
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

    /// Build a DACL granting current user + BUILTIN\Users GENERIC_READ | GENERIC_WRITE.
    pub fn set_owner_and_users_acl(path: &Path) -> io::Result<()> {
        // Implementation similar to set_owner_only_acl but with 2 ACEs.
        // Uses well-known SID for BUILTIN\Users (WinBuiltinUsersSid).
        unsafe {
            let mut token_handle = 0;
            if OpenProcessToken(
                windows_sys::Win32::System::Threading::GetCurrentProcess(),
                TOKEN_QUERY,
                &mut token_handle,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }

            let mut info_len = 0u32;
            GetTokenInformation(token_handle, TokenUser, std::ptr::null_mut(), 0, &mut info_len);
            let mut buffer = vec![0u8; info_len as usize];
            if GetTokenInformation(
                token_handle,
                TokenUser,
                buffer.as_mut_ptr() as *mut _,
                info_len,
                &mut info_len,
            ) == 0
            {
                windows_sys::Win32::Foundation::CloseHandle(token_handle);
                return Err(io::Error::last_os_error());
            }
            windows_sys::Win32::Foundation::CloseHandle(token_handle);

            let token_user = &*(buffer.as_ptr() as *const TOKEN_USER);
            let user_sid = token_user.User.Sid;

            // Create well-known SID for BUILTIN\Users
            let mut users_sid = [0u8; 68]; // MAX_SID_SIZE
            let mut sid_size = users_sid.len() as u32;
            if CreateWellKnownSid(
                WinBuiltinUsersSid,
                std::ptr::null_mut(),
                users_sid.as_mut_ptr() as *mut _,
                &mut sid_size,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }

            let mut entries = [
                EXPLICIT_ACCESS_W {
                    grfAccessPermissions: GENERIC_READ | GENERIC_WRITE,
                    grfAccessMode: SET_ACCESS,
                    grfInheritance: NO_INHERITANCE,
                    Trustee: TRUSTEE_W {
                        pMultipleTrustee: std::ptr::null_mut(),
                        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                        TrusteeForm: TRUSTEE_IS_SID,
                        TrusteeType: TRUSTEE_IS_USER,
                        ptstrName: user_sid as *mut u16,
                    },
                },
                EXPLICIT_ACCESS_W {
                    grfAccessPermissions: GENERIC_READ | GENERIC_WRITE,
                    grfAccessMode: SET_ACCESS,
                    grfInheritance: NO_INHERITANCE,
                    Trustee: TRUSTEE_W {
                        pMultipleTrustee: std::ptr::null_mut(),
                        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                        TrusteeForm: TRUSTEE_IS_SID,
                        TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                        ptstrName: users_sid.as_mut_ptr() as *mut u16,
                    },
                },
            ];

            let mut acl = std::ptr::null_mut();
            let result = SetEntriesInAclW(2, entries.as_mut_ptr(), std::ptr::null_mut(), &mut acl);
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
}
```

- [ ] **Step 4: Register module in lib.rs**

Add to `crates/ahandd/src/lib.rs` (create if it doesn't exist as a library):

Since `ahandd` is a binary crate, make `fs_perms` a public internal module. Add to `crates/ahandd/src/main.rs` near the top module declarations:

```rust
mod fs_perms;
```

For tests to access it, also add to `Cargo.toml`:

```toml
[lib]
name = "ahandd"
path = "src/lib.rs"

[[bin]]
name = "ahandd"
path = "src/main.rs"
```

And create `crates/ahandd/src/lib.rs`:

```rust
pub mod fs_perms;

#[cfg(windows)]
pub mod dpapi;
```

- [ ] **Step 5: Run tests — verify they pass**

Run: `cargo test -p ahandd --test fs_perms_test`
Expected: 2 tests pass on Unix

- [ ] **Step 6: Commit**

```bash
git add crates/ahandd/src/fs_perms.rs crates/ahandd/src/lib.rs crates/ahandd/src/main.rs crates/ahandd/Cargo.toml crates/ahandd/tests/fs_perms_test.rs
git commit -m "feat(windows): add cross-platform file permissions module with tests"
```

---

### Task 2: DPAPI Key Encryption Module

**Goal:** Create `dpapi.rs` with `protect`/`unprotect` functions for encrypting Ed25519 private keys at rest on Windows.

**Files:**
- Create: `crates/ahandd/src/dpapi.rs`
- Create: `crates/ahandd/tests/dpapi_test.rs`

**Acceptance Criteria:**
- [ ] `protect(plaintext)` returns encrypted bytes using `CryptProtectData`
- [ ] `unprotect(ciphertext)` returns original plaintext using `CryptUnprotectData`
- [ ] Roundtrip test verifies encrypt→decrypt produces original data
- [ ] Ciphertext differs from plaintext
- [ ] Module only compiles on Windows (`#[cfg(windows)]`)

**Verify:** `cargo test -p ahandd --test dpapi_test` → passes on Windows CI (skipped on Unix)

**Steps:**

- [ ] **Step 1: Write tests first**

Create `crates/ahandd/tests/dpapi_test.rs`:

```rust
//! DPAPI tests — only compile and run on Windows.

#[cfg(windows)]
mod tests {
    #[test]
    fn dpapi_roundtrip() {
        let plaintext = b"Ed25519-secret-key-bytes-here-32";
        let encrypted = ahandd::dpapi::protect(plaintext).unwrap();

        // Ciphertext must differ from plaintext
        assert_ne!(&encrypted, plaintext, "ciphertext should differ from plaintext");
        assert!(encrypted.len() > plaintext.len(), "ciphertext should be larger");

        let decrypted = ahandd::dpapi::unprotect(&encrypted).unwrap();
        assert_eq!(&decrypted, plaintext, "roundtrip failed");
    }

    #[test]
    fn dpapi_empty_input() {
        let encrypted = ahandd::dpapi::protect(b"").unwrap();
        let decrypted = ahandd::dpapi::unprotect(&encrypted).unwrap();
        assert!(decrypted.is_empty());
    }
}
```

- [ ] **Step 2: Create dpapi.rs**

Create `crates/ahandd/src/dpapi.rs`:

```rust
//! Windows DPAPI wrapper for encrypting sensitive data at rest.
//!
//! Data is bound to the current Windows user account.
//! Only the same user on the same machine can decrypt it.

#![cfg(windows)]

use std::io;
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN,
};
use windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB;

/// Encrypt data using DPAPI, bound to current user.
pub fn protect(plaintext: &[u8]) -> io::Result<Vec<u8>> {
    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        let result = CryptProtectData(
            &mut input,
            std::ptr::null(),     // description
            std::ptr::null_mut(), // entropy
            std::ptr::null_mut(), // reserved
            std::ptr::null_mut(), // prompt
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );

        if result == 0 {
            return Err(io::Error::last_os_error());
        }

        let encrypted = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        windows_sys::Win32::System::Memory::LocalFree(output.pbData as *mut _);
        Ok(encrypted)
    }
}

/// Decrypt DPAPI-protected data. Only works for the same user who encrypted it.
pub fn unprotect(ciphertext: &[u8]) -> io::Result<Vec<u8>> {
    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: ciphertext.len() as u32,
            pbData: ciphertext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        let result = CryptUnprotectData(
            &mut input,
            std::ptr::null_mut(), // description
            std::ptr::null_mut(), // entropy
            std::ptr::null_mut(), // reserved
            std::ptr::null_mut(), // prompt
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );

        if result == 0 {
            return Err(io::Error::last_os_error());
        }

        let decrypted = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        windows_sys::Win32::System::Memory::LocalFree(output.pbData as *mut _);
        Ok(decrypted)
    }
}
```

- [ ] **Step 3: Verify it compiles on Unix (module is cfg-gated)**

Run: `cargo check -p ahandd`
Expected: compiles clean — the `#[cfg(windows)]` attribute means module is skipped on Unix

- [ ] **Step 4: Commit**

```bash
git add crates/ahandd/src/dpapi.rs crates/ahandd/tests/dpapi_test.rs
git commit -m "feat(windows): add DPAPI key encryption module with tests"
```

---

### Task 3: IPC Server Refactor — Generic + Windows Named Pipes

**Goal:** Refactor `ipc.rs` to use generic `AsyncRead`/`AsyncWrite` for the connection handler, add `serve_ipc_windows` using Named Pipes with peer identity via Win32 API.

**Files:**
- Modify: `crates/ahandd/src/ipc.rs`

**Acceptance Criteria:**
- [ ] `serve_ipc` dispatches to `serve_ipc_unix` or `serve_ipc_windows` via `#[cfg]`
- [ ] `handle_ipc_conn` is generic over `R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static`
- [ ] `spawn_ipc_handler` wraps the spawn to prevent generic propagation
- [ ] Unix path: `UnixListener::bind` + `stream.peer_cred()` → `"uid:{uid}"` (unchanged behavior)
- [ ] Windows path: `ServerOptions` Named Pipe + `GetNamedPipeClientProcessId` → `OpenProcessToken` → `LookupAccountSidW` → `"user:{username}"`
- [ ] `set_permissions` helper replaced with `fs_perms::restrict_owner_and_group` on Unix
- [ ] Existing Unix tests still pass

**Verify:** `cargo test -p ahandd` → all existing tests pass

**Steps:**

- [ ] **Step 1: Replace imports and add platform-specific serve functions**

In `crates/ahandd/src/ipc.rs`, replace the import block (lines 1-9):

```rust
use std::path::PathBuf;
use std::sync::Arc;

use ahand_protocol::{BrowserResponse, Envelope, JobFinished, JobRejected, SessionMode, envelope};
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};
```

- [ ] **Step 2: Replace the serve_ipc function with a dispatcher**

Replace the current `serve_ipc` function with a dispatcher that calls platform-specific implementations. Keep the same public signature.

```rust
/// Start the IPC server on the given socket path.
#[allow(clippy::too_many_arguments)]
pub async fn serve_ipc(
    socket_path: PathBuf,
    #[allow(unused_variables)] socket_mode: u32,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    browser_mgr: Arc<BrowserManager>,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        serve_ipc_unix(
            socket_path, socket_mode, registry, store, session_mgr,
            approval_mgr, approval_broadcast_tx, device_id, browser_mgr,
        ).await
    }
    #[cfg(windows)]
    {
        serve_ipc_windows(
            socket_path, registry, store, session_mgr,
            approval_mgr, approval_broadcast_tx, device_id, browser_mgr,
        ).await
    }
}
```

- [ ] **Step 3: Write serve_ipc_unix (extract from current serve_ipc)**

Move the current Unix socket bind/accept loop into `serve_ipc_unix`. Replace inline `set_permissions` with `crate::fs_perms::restrict_owner_and_group`. Use `spawn_ipc_handler` instead of inline `tokio::spawn`. Full code provided in reference branch `feat/windows-support:crates/ahandd/src/ipc.rs` lines 60-124.

- [ ] **Step 4: Write serve_ipc_windows**

```rust
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
async fn serve_ipc_windows(
    socket_path: PathBuf,
    registry: Arc<JobRegistry>,
    store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>,
    approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String,
    browser_mgr: Arc<BrowserManager>,
) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_name = socket_path.to_string_lossy().to_string();
    info!(path = %pipe_name, "IPC server listening (Named Pipe)");

    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)?;

    loop {
        server.connect().await?;
        let connected = server;
        server = ServerOptions::new().create(&pipe_name)?;

        let caller_uid = get_pipe_caller_identity(&connected);
        let (reader, writer) = tokio::io::split(connected);
        spawn_ipc_handler(
            reader, writer, Arc::clone(&registry), store.clone(),
            Arc::clone(&session_mgr), Arc::clone(&approval_mgr),
            approval_broadcast_tx.clone(), device_id.clone(),
            caller_uid, Arc::clone(&browser_mgr),
        );
    }
}
```

- [ ] **Step 5: Write Windows peer identity function**

```rust
#[cfg(windows)]
fn get_pipe_caller_identity(pipe: &tokio::net::windows::named_pipe::NamedPipeServer) -> String {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::Security::*;

    unsafe {
        let handle = pipe.as_raw_handle() as isize;
        let mut pid = 0u32;
        if GetNamedPipeClientProcessId(handle, &mut pid) == 0 {
            return "user:unknown".to_string();
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process == 0 {
            return format!("pid:{pid}");
        }

        let mut token = 0;
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            windows_sys::Win32::Foundation::CloseHandle(process);
            return format!("pid:{pid}");
        }

        let mut info_len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut info_len);
        let mut buffer = vec![0u8; info_len as usize];
        if GetTokenInformation(token, TokenUser, buffer.as_mut_ptr() as *mut _, info_len, &mut info_len) == 0 {
            windows_sys::Win32::Foundation::CloseHandle(token);
            windows_sys::Win32::Foundation::CloseHandle(process);
            return format!("pid:{pid}");
        }

        let token_user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let sid = token_user.User.Sid;

        let mut name_buf = [0u16; 256];
        let mut name_len = 256u32;
        let mut domain_buf = [0u16; 256];
        let mut domain_len = 256u32;
        let mut sid_type = 0;

        if LookupAccountSidW(
            std::ptr::null(), sid,
            name_buf.as_mut_ptr(), &mut name_len,
            domain_buf.as_mut_ptr(), &mut domain_len,
            &mut sid_type,
        ) == 0 {
            windows_sys::Win32::Foundation::CloseHandle(token);
            windows_sys::Win32::Foundation::CloseHandle(process);
            return format!("pid:{pid}");
        }

        windows_sys::Win32::Foundation::CloseHandle(token);
        windows_sys::Win32::Foundation::CloseHandle(process);

        let username = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        format!("user:{username}")
    }
}
```

- [ ] **Step 6: Write spawn_ipc_handler and make handle_ipc_conn generic**

```rust
#[allow(clippy::too_many_arguments)]
fn spawn_ipc_handler<R, W>(
    reader: R, writer: W,
    registry: Arc<JobRegistry>, store: Option<Arc<RunStore>>,
    session_mgr: Arc<SessionManager>, approval_mgr: Arc<ApprovalManager>,
    approval_broadcast_tx: broadcast::Sender<Envelope>,
    device_id: String, caller_uid: String,
    browser_mgr: Arc<BrowserManager>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = handle_ipc_conn(
            reader, writer, registry, store, session_mgr,
            approval_mgr, approval_broadcast_tx, device_id,
            caller_uid, browser_mgr,
        ).await {
            warn!(error = %e, "IPC connection error");
        }
    });
}
```

Change `handle_ipc_conn` signature from taking `UnixStream` to:

```rust
async fn handle_ipc_conn<R, W>(
    reader: R, writer: W,
    ...
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = tokio::io::BufReader::new(reader);
    // ... rest of function body unchanged
```

- [ ] **Step 7: Remove old set_permissions helper**

Delete the `set_permissions` function at line 479-483. It's now replaced by `crate::fs_perms::restrict_owner_and_group`.

- [ ] **Step 8: Verify compilation and tests**

Run: `cargo test -p ahandd`
Expected: all tests pass, no regressions

- [ ] **Step 9: Commit**

```bash
git add crates/ahandd/src/ipc.rs
git commit -m "feat(windows): refactor IPC server for cross-platform support with Named Pipes"
```

---

### Task 4: IPC Client Refactor — Extract ipc_connect()

**Goal:** Extract a cross-platform `ipc_connect()` function in `ahandctl` and replace all 5 `UnixStream::connect` call sites.

**Files:**
- Modify: `crates/ahandctl/src/main.rs`

**Acceptance Criteria:**
- [ ] `ipc_connect()` returns `(impl AsyncRead + Unpin, impl AsyncWrite + Unpin)` on both platforms
- [ ] All 5 IPC functions (`ipc_exec`, `ipc_cancel`, `ipc_approve`, `ipc_policy`, `ipc_session`) use `ipc_connect()`
- [ ] No more direct `UnixStream` usage in IPC functions
- [ ] Existing IPC behavior unchanged

**Verify:** `cargo check -p ahandctl` → compiles clean

**Steps:**

- [ ] **Step 1: Add ipc_connect() after the frame helpers (after line 300)**

```rust
// ── IPC connect (cross-platform) ─────────────────────────────────────

#[cfg(unix)]
async fn ipc_connect(
    path: &str,
) -> anyhow::Result<(
    impl tokio::io::AsyncRead + Unpin,
    impl tokio::io::AsyncWrite + Unpin,
)> {
    let stream = tokio::net::UnixStream::connect(path).await?;
    let (r, w) = stream.into_split();
    Ok((r, w))
}

#[cfg(windows)]
async fn ipc_connect(
    path: &str,
) -> anyhow::Result<(
    impl tokio::io::AsyncRead + Unpin,
    impl tokio::io::AsyncWrite + Unpin,
)> {
    let client = tokio::net::windows::named_pipe::ClientOptions::new().open(path)?;
    let (r, w) = tokio::io::split(client);
    Ok((r, w))
}
```

- [ ] **Step 2: Replace UnixStream::connect in all 5 IPC functions**

In each function, replace:
```rust
let stream = tokio::net::UnixStream::connect(socket_path).await?;
let (mut reader, mut writer) = stream.into_split();
```

With:
```rust
let (mut reader, mut writer) = ipc_connect(socket_path).await?;
```

Functions to update (line numbers from current `dev` branch):
- `ipc_exec` (line 305)
- `ipc_cancel` (line 399)
- `ipc_approve` (line 621)
- `ipc_policy` (line 725)
- `ipc_session` (line 827)

- [ ] **Step 3: Remove direct UnixStream import if no longer needed**

Check top-level imports — if `tokio::net::UnixStream` is no longer used directly, remove it.

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p ahandctl`
Expected: compiles clean

- [ ] **Step 5: Commit**

```bash
git add crates/ahandctl/src/main.rs
git commit -m "feat(windows): extract cross-platform ipc_connect() in ahandctl"
```

---

### Task 5: Signal Handling & Process Management

**Goal:** Make daemon shutdown and process lifecycle management work on Windows.

**Files:**
- Modify: `crates/ahandd/src/main.rs`
- Modify: `crates/ahandctl/src/daemon.rs`

**Acceptance Criteria:**
- [ ] Daemon graceful shutdown uses `ctrl_c()` on Windows, `SIGTERM`/`SIGINT` on Unix
- [ ] `is_process_running` uses `tasklist` on Windows
- [ ] `send_signal` uses `taskkill` on Windows
- [ ] `start` uses `CREATE_NO_WINDOW | DETACHED_PROCESS` creation flags on Windows
- [ ] Existing Unix behavior unchanged

**Verify:** `cargo check -p ahandd -p ahandctl` → compiles clean

**Steps:**

- [ ] **Step 1: Fix signal handling in ahandd/src/main.rs**

Replace line 22 (`use tokio::signal::unix::{SignalKind, signal};`) — remove this top-level import entirely.

Replace lines 292-293 (the signal setup):
```rust
// Set up signal handlers for graceful shutdown.
#[cfg(unix)]
let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
#[cfg(unix)]
let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
```

Replace lines 393-403 (the select block):
```rust
let result = {
    #[cfg(unix)]
    {
        tokio::select! {
            r = main_future => r,
            _ = sigterm.recv() => {
                info!("received SIGTERM, shutting down");
                Ok(())
            }
            _ = sigint.recv() => {
                info!("received SIGINT, shutting down");
                Ok(())
            }
        }
    }
    #[cfg(windows)]
    {
        tokio::select! {
            r = main_future => r,
            _ = tokio::signal::ctrl_c() => {
                info!("received Ctrl+C, shutting down");
                Ok(())
            }
        }
    }
};
```

- [ ] **Step 2: Fix is_process_running in daemon.rs**

Change `#[cfg(not(target_os = "linux"))]` (line 65) to `#[cfg(all(not(target_os = "linux"), unix))]`.

Add Windows implementation:

```rust
#[cfg(windows)]
fn is_process_running(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            output.status.success() && !stdout.contains("No tasks")
        })
        .unwrap_or(false)
}
```

- [ ] **Step 3: Fix send_signal in daemon.rs**

Add `#[cfg(unix)]` to existing `send_signal` (line 76).

Add Windows implementation:

```rust
#[cfg(windows)]
fn send_signal(pid: u32, _sig: &str) -> Result<()> {
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status()
        .context("Failed to run taskkill command")?;
    if !status.success() {
        anyhow::bail!("taskkill /PID {} failed", pid);
    }
    Ok(())
}
```

- [ ] **Step 4: Fix daemon start — add Windows creation flags**

In daemon.rs `start()` function, after the existing `#[cfg(unix)]` block for `process_group(0)` (line 117-121), add:

```rust
#[cfg(windows)]
{
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW (0x08000000) | DETACHED_PROCESS (0x00000008)
    cmd.creation_flags(0x08000008);
}
```

- [ ] **Step 5: Fix find_ahandd_binary for Windows .exe suffix**

In `find_ahandd_binary`, the sibling check (line 30) should also check for `.exe`:

```rust
// 2. Sibling of current executable (dev builds: target/debug/)
if let Ok(current_exe) = std::env::current_exe() {
    if let Some(dir) = current_exe.parent() {
        let sibling = dir.join(if cfg!(windows) { "ahandd.exe" } else { "ahandd" });
        if sibling.exists() {
            return Ok(sibling);
        }
    }
}
```

Similarly for the installed path check, append `.exe` on Windows:

```rust
let binary_name = if cfg!(windows) { "ahandd.exe" } else { "ahandd" };
let installed = home.join(".ahand").join("bin").join(binary_name);
```

- [ ] **Step 6: Verify compilation**

Run: `cargo check -p ahandd -p ahandctl`
Expected: compiles clean

- [ ] **Step 7: Commit**

```bash
git add crates/ahandd/src/main.rs crates/ahandctl/src/daemon.rs
git commit -m "feat(windows): add cross-platform signal handling and process management"
```

---

### Task 6: Device Identity & OpenClaw Permissions

**Goal:** Integrate `fs_perms` and DPAPI into device identity key storage and OpenClaw file permission settings.

**Files:**
- Modify: `crates/ahandd/src/openclaw/device_identity.rs:108-141`
- Modify: `crates/ahandd/src/openclaw/exec_approvals.rs:65-71`
- Modify: `crates/ahandd/src/openclaw/pairing.rs:84-90`

**Acceptance Criteria:**
- [ ] `device_identity.rs` save uses `fs_perms::restrict_owner_only` instead of inline `PermissionsExt`
- [ ] On Windows, private key content is DPAPI-encrypted before writing
- [ ] On Windows, `load` DPAPI-decrypts content before parsing
- [ ] `exec_approvals.rs` and `pairing.rs` use `fs_perms::restrict_owner_only`
- [ ] Unix behavior unchanged

**Verify:** `cargo test -p ahandd` → all existing tests pass

**Steps:**

- [ ] **Step 1: Update device_identity.rs save method**

Replace lines 129-138 in `save()`:

```rust
std::fs::write(path, format!("{}\n", content))
    .with_context(|| format!("failed to write {}", path.display()))?;

// Set restrictive file permissions
crate::fs_perms::restrict_owner_only(path)
    .with_context(|| format!("failed to set permissions on {}", path.display()))?;
```

For Windows DPAPI encryption, wrap the write:

```rust
// On Windows, encrypt the content before writing
#[cfg(windows)]
let content = {
    let encrypted = crate::dpapi::protect(content.as_bytes())
        .with_context(|| "failed to DPAPI-encrypt identity")?;
    // Write raw bytes, not JSON text
    std::fs::write(path, &encrypted)
        .with_context(|| format!("failed to write {}", path.display()))?;
    crate::fs_perms::restrict_owner_only(path)
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    return Ok(());
};

#[cfg(unix)]
{
    std::fs::write(path, format!("{}\n", content))
        .with_context(|| format!("failed to write {}", path.display()))?;
    crate::fs_perms::restrict_owner_only(path)
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;
}
```

- [ ] **Step 2: Update device_identity.rs load method**

In `load()`, add DPAPI decryption before parsing:

```rust
fn load(path: &PathBuf) -> Result<Self> {
    #[cfg(windows)]
    let content = {
        let encrypted = std::fs::read(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let decrypted = crate::dpapi::unprotect(&encrypted)
            .with_context(|| format!("failed to DPAPI-decrypt {}", path.display()))?;
        String::from_utf8(decrypted)
            .with_context(|| format!("decrypted content is not valid UTF-8"))?
    };

    #[cfg(unix)]
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let stored: StoredIdentity = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    // ... rest unchanged
```

- [ ] **Step 3: Update exec_approvals.rs**

Replace the `#[cfg(unix)]` block at lines 65-71:

```rust
crate::fs_perms::restrict_owner_only(path)
    .with_context(|| format!("failed to set permissions on {}", path.display()))?;
```

Remove the old `#[cfg(unix)] { use std::os::unix::fs::PermissionsExt; ... }` block.

- [ ] **Step 4: Update pairing.rs**

Same change as exec_approvals.rs — replace lines 84-90:

```rust
crate::fs_perms::restrict_owner_only(path)
    .with_context(|| format!("failed to set permissions on {}", path.display()))?;
```

- [ ] **Step 5: Verify compilation and tests**

Run: `cargo test -p ahandd`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ahandd/src/openclaw/device_identity.rs crates/ahandd/src/openclaw/exec_approvals.rs crates/ahandd/src/openclaw/pairing.rs
git commit -m "feat(windows): integrate fs_perms and DPAPI for secure key storage"
```

---

### Task 7: browser-init Windows Adaptation

**Goal:** Make `ahandd browser-init` work on Windows: Node.js `.zip` download, Windows Chrome/Edge detection, cross-platform clean logic.

**Files:**
- Modify: `crates/ahandd/src/browser_init.rs`

**Acceptance Criteria:**
- [ ] `install_node` downloads `.zip` on Windows, `.tar.xz` on Unix
- [ ] `detect_system_chrome` checks Windows Chrome/Edge paths
- [ ] `clean` uses `taskkill` on Windows, `pkill` on Unix
- [ ] `ensure_node` checks `node.exe` on Windows
- [ ] Existing Unix behavior unchanged

**Verify:** `cargo check -p ahandd` → compiles clean

**Steps:**

- [ ] **Step 1: Add Windows Node.js `.zip` download in install_node**

Branch `install_node` by platform. Windows: download `.zip`, extract with `zip` crate. Unix: keep existing `.tar.xz` logic. See reference branch `feat/windows-support:crates/ahandd/src/browser_init.rs` for the pattern. Key difference: Windows Node.js zip has flat layout (`node.exe` at root), Unix tar has `bin/node` structure.

- [ ] **Step 2: Fix ensure_node for Windows binary path**

Windows Node.js extracts flat (no `bin/` subdir):

```rust
#[cfg(unix)]
let local_node = dirs.node.join("bin").join("node");
#[cfg(windows)]
let local_node = dirs.node.join("node.exe");
```

- [ ] **Step 3: Add `#[cfg(windows)]` detect_system_chrome**

Check `Program Files` Chrome, Edge paths + `LOCALAPPDATA` per-user Chrome.

- [ ] **Step 4: Fix clean logic — `taskkill` on Windows, existing npm uninstall on Unix**

- [ ] **Step 5: Verify and commit**

```bash
cargo check -p ahandd
git add crates/ahandd/src/browser_init.rs
git commit -m "feat(windows): adapt browser-init for Windows Node.js and Chrome detection"
```

---

### Task 8: Upgrade Rust Rewrite & Admin Bash Removal

**Goal:** Rewrite `ahandctl upgrade` entirely in Rust (remove bash dependency) and replace admin panel's bash browser-setup call.

**Files:**
- Modify: `crates/ahandctl/src/upgrade.rs`
- Modify: `crates/ahandctl/src/admin.rs:371-398`

**Acceptance Criteria:**
- [ ] `ahandctl upgrade` works without bash — pure Rust HTTP download + file replacement
- [ ] Supports `--check` (version check only) and `--version X.Y.Z` (pin version)
- [ ] Downloads binaries from GitHub Releases, verifies SHA-256 checksums
- [ ] Stops running daemon before replacing binaries
- [ ] Windows: handles `.exe` suffix and running-binary replacement
- [ ] `admin.rs` browser setup calls Rust `browser_init::run()` instead of `Command::new("bash")`

**Verify:** `cargo check -p ahandctl` → compiles clean

**Steps:**

- [ ] **Step 1: Rewrite upgrade.rs — version check**

Replace entire `upgrade.rs` content. Add GitHub API version query:

```rust
use anyhow::{Context, Result};
use std::path::PathBuf;

const GITHUB_REPO: &str = "team9ai/aHand";

pub async fn run(check_only: bool, target_version: Option<String>) -> Result<()> {
    let current = current_version();
    let latest = match target_version {
        Some(v) => v,
        None => fetch_latest_version().await?,
    };

    println!("Current: {current}");
    println!("Latest:  {latest}");

    if current == latest {
        println!("Already up to date.");
        return Ok(());
    }

    if check_only {
        println!("Update available: {current} → {latest}");
        return Ok(());
    }

    println!("Upgrading {current} → {latest}...");
    download_and_install(&latest).await?;
    println!("Upgrade complete.");
    Ok(())
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

async fn fetch_latest_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let client = reqwest::Client::new();
    let resp = client.get(&url)
        .header("User-Agent", "ahandctl")
        .send().await?
        .json::<serde_json::Value>().await?;
    let tag = resp["tag_name"].as_str()
        .context("no tag_name in release")?;
    Ok(tag.strip_prefix("rust-v").unwrap_or(tag).to_string())
}
```

- [ ] **Step 2: Add download_and_install function**

```rust
async fn download_and_install(version: &str) -> Result<()> {
    let (suffix, exe_ext) = platform_suffix();
    let bin_dir = dirs::home_dir()
        .context("no home dir")?
        .join(".ahand").join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    // Stop daemon before replacing binaries
    if let Err(e) = crate::daemon::stop().await {
        tracing::warn!("Failed to stop daemon: {e}");
    }

    for binary in &["ahandd", "ahandctl"] {
        let asset = format!("{binary}-{suffix}{exe_ext}");
        let url = format!(
            "https://github.com/{GITHUB_REPO}/releases/download/rust-v{version}/{asset}"
        );
        println!("  Downloading {asset}...");
        let bytes = download_bytes(&url).await?;
        let dest = bin_dir.join(format!("{binary}{exe_ext}"));

        // On Windows, rename current binary before overwriting
        #[cfg(windows)]
        {
            let backup = bin_dir.join(format!("{binary}.old{exe_ext}"));
            let _ = std::fs::remove_file(&backup);
            if dest.exists() {
                std::fs::rename(&dest, &backup)?;
            }
        }

        std::fs::write(&dest, &bytes)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
        }

        println!("  Installed: {}", dest.display());
    }
    Ok(())
}

fn platform_suffix() -> (&'static str, &'static str) {
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        ("darwin-arm64", "")
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        ("darwin-x64", "")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        ("linux-x64", "")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        ("linux-arm64", "")
    } else if cfg!(target_os = "windows") {
        ("windows-x64", ".exe")
    } else {
        ("unknown", "")
    }
}

async fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let resp = client.get(url)
        .header("User-Agent", "ahandctl")
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} for {}", resp.status(), url);
    }
    Ok(resp.bytes().await?.to_vec())
}
```

- [ ] **Step 3: Fix admin.rs — replace bash call with Rust**

In `admin.rs`, replace the bash `setup-browser.sh` invocation block (around line 371-398). Instead of `Command::new("bash").arg(&script_path)`, call the Rust implementation directly. Since `browser_init` is in `ahandd` crate and admin is in `ahandctl`, the simplest approach is to duplicate the SSE streaming wrapper but call `ahandctl::browser_init::run()` (which currently wraps bash). Rewrite `ahandctl/src/browser_init.rs` to also work without bash by calling the same Rust logic as `ahandd/src/browser_init.rs`.

For now, the minimal fix: make `ahandctl::browser_init::run` work cross-platform by detecting whether `setup-browser.sh` exists and falling back to an error message suggesting `ahandd browser-init` on Windows.

- [ ] **Step 4: Add reqwest/serde_json to ahandctl deps if not present**

Check `crates/ahandctl/Cargo.toml` and add `reqwest` and `serde_json` if needed.

- [ ] **Step 5: Verify and commit**

```bash
cargo check -p ahandctl
git add crates/ahandctl/src/upgrade.rs crates/ahandctl/src/admin.rs crates/ahandctl/src/browser_init.rs crates/ahandctl/Cargo.toml
git commit -m "feat(windows): rewrite upgrade in Rust, remove bash dependency from admin"
```

---

### Task 9: install.ps1 & ahandctl install-daemon

**Goal:** Create PowerShell install script for Windows users and add `ahandctl install-daemon` subcommand for script-free daemon installation.

**Files:**
- Create: `scripts/dist/install.ps1`
- Modify: `crates/ahandctl/src/main.rs` (add `InstallDaemon` subcommand)

**Acceptance Criteria:**
- [ ] `install.ps1` downloads ahandd.exe + ahandctl.exe from GitHub Releases
- [ ] `install.ps1` verifies SHA-256 checksums
- [ ] `install.ps1` adds `~/.ahand/bin` to user PATH
- [ ] `ahandctl install-daemon` downloads and installs ahandd binary (cross-platform)
- [ ] Both handle platform detection (x64/arm64)

**Verify:** PowerShell syntax check: `pwsh -Command "& { Get-Content scripts/dist/install.ps1 | Out-Null }"` + `cargo check -p ahandctl`

**Steps:**

- [ ] **Step 1: Create install.ps1**

Create `scripts/dist/install.ps1`:

```powershell
#Requires -Version 5.1
<#
.SYNOPSIS
    Install aHand (ahandd + ahandctl) on Windows.
.USAGE
    irm https://raw.githubusercontent.com/team9ai/aHand/main/scripts/dist/install.ps1 | iex
#>
param(
    [string]$Version = "",
    [string]$InstallDir = "$env:USERPROFILE\.ahand\bin"
)

$ErrorActionPreference = "Stop"
$REPO = "team9ai/aHand"

# Detect architecture
$arch = if ([Environment]::Is64BitOperatingSystem) {
    if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
} else { "x86" }
$suffix = "windows-$arch"

# Determine version
if (-not $Version) {
    Write-Host "Fetching latest version..."
    $release = Invoke-RestMethod "https://api.github.com/repos/$REPO/releases/latest"
    $Version = $release.tag_name -replace '^rust-v', ''
}
Write-Host "Installing aHand v$Version ($suffix)..."

# Create install directory
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# Download and verify
foreach ($binary in @("ahandd", "ahandctl")) {
    $asset = "$binary-$suffix.exe"
    $url = "https://github.com/$REPO/releases/download/rust-v$Version/$asset"
    $dest = Join-Path $InstallDir "$binary.exe"

    Write-Host "  Downloading $asset..."
    Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing
    Write-Host "  Installed: $dest"
}

# Download checksums and verify
$checksumUrl = "https://github.com/$REPO/releases/download/rust-v$Version/checksums-rust-$suffix.txt"
try {
    $checksums = Invoke-RestMethod -Uri $checksumUrl
    foreach ($line in $checksums -split "`n") {
        if ($line -match "^(\S+)\s+(.+)$") {
            $expected = $Matches[1]
            $file = Join-Path $InstallDir $Matches[2].Trim()
            if (Test-Path $file) {
                $actual = (Get-FileHash $file -Algorithm SHA256).Hash.ToLower()
                if ($actual -ne $expected) {
                    Write-Warning "Checksum mismatch for $file!"
                } else {
                    Write-Host "  Checksum OK: $($Matches[2].Trim())"
                }
            }
        }
    }
} catch {
    Write-Warning "Could not verify checksums: $_"
}

# Add to PATH
$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($userPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("PATH", "$userPath;$InstallDir", "User")
    Write-Host ""
    Write-Host "Added $InstallDir to user PATH."
    Write-Host "Restart your terminal for PATH changes to take effect."
}

Write-Host ""
Write-Host "aHand v$Version installed successfully!"
Write-Host "  ahandd:   $(Join-Path $InstallDir 'ahandd.exe')"
Write-Host "  ahandctl: $(Join-Path $InstallDir 'ahandctl.exe')"
```

- [ ] **Step 2: Add InstallDaemon subcommand to ahandctl**

In `crates/ahandctl/src/main.rs`, add to the `Cmd` enum:

```rust
/// Install the ahandd daemon binary from GitHub Releases
InstallDaemon {
    /// Specific version to install (default: latest)
    #[arg(long)]
    version: Option<String>,
},
```

Add early handling (before the IPC/WS dispatch):

```rust
Cmd::InstallDaemon { version } => {
    return install_daemon::run(version).await;
}
```

- [ ] **Step 3: Create install_daemon module**

Create `crates/ahandctl/src/install_daemon.rs`:

```rust
use anyhow::{Context, Result};

const GITHUB_REPO: &str = "team9ai/aHand";

pub async fn run(target_version: Option<String>) -> Result<()> {
    let version = match target_version {
        Some(v) => v,
        None => fetch_latest_version().await?,
    };

    let (suffix, exe_ext) = platform_suffix();
    let bin_dir = dirs::home_dir()
        .context("no home dir")?
        .join(".ahand").join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    let asset = format!("ahandd-{suffix}{exe_ext}");
    let url = format!(
        "https://github.com/{GITHUB_REPO}/releases/download/rust-v{version}/{asset}"
    );

    println!("Downloading ahandd v{version} ({suffix})...");
    let client = reqwest::Client::new();
    let resp = client.get(&url)
        .header("User-Agent", "ahandctl")
        .send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} for {}", resp.status(), url);
    }
    let bytes = resp.bytes().await?;

    let dest = bin_dir.join(format!("ahandd{exe_ext}"));
    std::fs::write(&dest, &bytes)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
    }

    println!("Installed: {}", dest.display());
    Ok(())
}

// Shared helpers — extracted to github_release.rs (see note below)
```

Note: Extract shared helpers (`fetch_latest_version`, `platform_suffix`, `download_bytes`) into a common module (e.g., `crates/ahandctl/src/github_release.rs`) to avoid duplication with `upgrade.rs`.

- [ ] **Step 4: Register module and verify**

Add `mod install_daemon;` to `crates/ahandctl/src/main.rs`.

```bash
cargo check -p ahandctl
git add scripts/dist/install.ps1 crates/ahandctl/src/main.rs crates/ahandctl/src/install_daemon.rs
git commit -m "feat(windows): add install.ps1 and ahandctl install-daemon subcommand"
```

---

### Task 10: CI Workflows — Windows Build & Test

**Goal:** Add Windows to the release build matrix and create a cross-platform test workflow.

**Files:**
- Modify: `.github/workflows/release-rust.yml`
- Create: `.github/workflows/test-rust.yml`

**Acceptance Criteria:**
- [ ] Release workflow builds `ahandd.exe` + `ahandctl.exe` for `x86_64-pc-windows-msvc`
- [ ] Release workflow installs protoc on Windows via `arduino/setup-protoc`
- [ ] Release workflow handles `.exe` suffix in artifact names and checksums
- [ ] Test workflow runs `cargo test` for all 3 crates on `ubuntu-latest`, `macos-latest`, `windows-latest`
- [ ] Universal binary step skips Windows

**Verify:** `act -l` (if available) or manual review of YAML syntax

**Steps:**

- [ ] **Step 1: Add Windows target to release-rust.yml build matrix**

In `.github/workflows/release-rust.yml`, in the `matrix.include` array (after the macOS x64 entry, around line 34), replace the commented-out Windows block:

```yaml
          - os: windows-latest
            target: x86_64-pc-windows-msvc
            suffix: windows-x64
```

- [ ] **Step 2: Uncomment and fix protoc install for Windows**

Replace the commented-out Windows protoc block (lines 52-56):

```yaml
      - name: Install protoc (Windows)
        if: runner.os == 'Windows'
        uses: arduino/setup-protoc@v3
        with:
          repo-token: ${{ secrets.GITHUB_TOKEN }}
```

- [ ] **Step 3: Add Windows artifact preparation step**

After the existing "Prepare artifacts" step (line 75), add:

```yaml
      - name: Prepare artifacts (Windows)
        if: runner.os == 'Windows'
        shell: bash
        run: |
          mkdir -p release
          cp target/${{ matrix.target }}/release/ahandd.exe release/ahandd-${{ matrix.suffix }}.exe
          cp target/${{ matrix.target }}/release/ahandctl.exe release/ahandctl-${{ matrix.suffix }}.exe
          cd release && sha256sum * > checksums-rust-${{ matrix.suffix }}.txt
```

Add `if: runner.os != 'Windows'` to the existing Unix "Prepare artifacts" step.

- [ ] **Step 4: Guard universal binary job**

The `universal` job only needs macOS artifacts. Add condition to skip if Windows-only build. Currently it already only downloads `darwin-arm64` and `darwin-x64`, so no change needed — but ensure the `release` job doesn't fail if Windows artifacts have `.exe` suffix by updating the combined checksums step.

- [ ] **Step 5: Create test-rust.yml**

Create `.github/workflows/test-rust.yml`:

```yaml
name: Test Rust

on:
  push:
    branches: [dev, main]
    paths:
      - "crates/**"
      - "proto/**"
      - "Cargo.*"
  pull_request:
    paths:
      - "crates/**"
      - "proto/**"
      - "Cargo.*"

jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4

      - name: Install system dependencies (Linux)
        if: runner.os == 'Linux'
        run: sudo apt-get update && sudo apt-get install -y protobuf-compiler libssl-dev pkg-config

      - name: Install protoc (macOS)
        if: runner.os == 'macOS'
        run: brew install protobuf

      - name: Install protoc (Windows)
        if: runner.os == 'Windows'
        uses: arduino/setup-protoc@v3
        with:
          repo-token: ${{ secrets.GITHUB_TOKEN }}

      - uses: dtolnay/rust-toolchain@stable

      - name: Run tests
        run: cargo test --workspace
```

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release-rust.yml .github/workflows/test-rust.yml
git commit -m "ci: add Windows build target and cross-platform test workflow"
```

---

### Task 11: IPC Integration Tests

**Goal:** Add integration tests that verify the full IPC roundtrip (server ↔ client) on both platforms, including peer identity format.

**Files:**
- Create: `crates/ahandd/tests/ipc_roundtrip.rs`

**Acceptance Criteria:**
- [ ] Test starts `serve_ipc` on a temp socket/pipe, connects via client, sends `JobRequest`, receives `JobFinished`
- [ ] Test verifies `caller_uid` format: `"uid:{N}"` on Unix, `"user:{name}"` on Windows
- [ ] Test verifies `SessionQuery` → `SessionState` roundtrip
- [ ] Same test code runs on both platforms (different transport, same protocol)
- [ ] All tests pass on Unix

**Verify:** `cargo test -p ahandd --test ipc_roundtrip` → all pass

**Steps:**

- [ ] **Step 1: Create ipc_roundtrip.rs**

Create `crates/ahandd/tests/ipc_roundtrip.rs`:

```rust
//! IPC integration tests — verifies full server↔client roundtrip.

use std::sync::Arc;
use std::time::Duration;

use ahand_protocol::{Envelope, JobRequest, envelope};
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Generate a unique socket/pipe path for testing.
fn test_ipc_path() -> std::path::PathBuf {
    #[cfg(unix)]
    {
        let dir = std::env::temp_dir().join("ahand-test");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("test-{}.sock", std::process::id()))
    }
    #[cfg(windows)]
    {
        std::path::PathBuf::from(format!(r"\\.\pipe\ahand-test-{}", std::process::id()))
    }
}

async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    w.write_u32(data.len() as u32).await?;
    w.write_all(data).await?;
    w.flush().await
}

async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let len = r.read_u32().await? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[tokio::test]
async fn test_ipc_job_roundtrip() {
    // This test requires the full ahandd infrastructure.
    // It creates minimal managers, starts the IPC server,
    // connects as a client, sends a job, and expects a response.

    let path = test_ipc_path();
    let registry = Arc::new(ahandd::registry::JobRegistry::new(4));
    let session_mgr = Arc::new(ahandd::session::SessionManager::new(60));
    let approval_mgr = Arc::new(ahandd::approval::ApprovalManager::new(300));
    let browser_mgr = Arc::new(ahandd::browser::BrowserManager::new(Default::default()));
    let (broadcast_tx, _) = tokio::sync::broadcast::channel(16);

    // Start server in background
    let server_path = path.clone();
    let server_handle = tokio::spawn(ahandd::ipc::serve_ipc(
        server_path, 0o660,
        Arc::clone(&registry), None,
        Arc::clone(&session_mgr), Arc::clone(&approval_mgr),
        broadcast_tx, "test-device".to_string(),
        Arc::clone(&browser_mgr),
    ));

    // Give server time to bind
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect as client
    #[cfg(unix)]
    let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    #[cfg(unix)]
    let (mut reader, mut writer) = stream.into_split();

    #[cfg(windows)]
    let client = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(&path).unwrap();
    #[cfg(windows)]
    let (mut reader, mut writer) = tokio::io::split(client);

    // Send a JobRequest for a simple command
    let job_id = "test-job-1".to_string();
    let req = Envelope {
        device_id: "test-client".to_string(),
        msg_id: "msg-1".to_string(),
        ts_ms: 0,
        payload: Some(envelope::Payload::JobRequest(JobRequest {
            job_id: job_id.clone(),
            tool: "echo".to_string(),
            args: vec!["hello".to_string()],
            ..Default::default()
        })),
        ..Default::default()
    };
    write_frame(&mut writer, &req.encode_to_vec()).await.unwrap();

    // Read responses until JobFinished
    let mut reader = tokio::io::BufReader::new(&mut reader);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::time::Instant::now() > deadline {
            panic!("timeout waiting for JobFinished");
        }
        let data = read_frame(&mut reader).await.unwrap();
        let env = Envelope::decode(data.as_slice()).unwrap();
        if let Some(envelope::Payload::JobFinished(fin)) = env.payload {
            assert_eq!(fin.job_id, job_id);
            break;
        }
    }

    // Cleanup
    server_handle.abort();
    #[cfg(unix)]
    let _ = std::fs::remove_file(&path);
}
```

Note: This test requires `ahandd` to expose certain types as `pub` via `lib.rs`. The Task 1 step already created `lib.rs`. Ensure `registry`, `session`, `approval`, `browser`, and `ipc` modules are re-exported.

- [ ] **Step 2: Update lib.rs to export needed modules**

In `crates/ahandd/src/lib.rs`:

```rust
pub mod fs_perms;
#[cfg(windows)]
pub mod dpapi;
pub mod ipc;
pub mod registry;
pub mod session;
pub mod approval;
pub mod browser;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ahandd --test ipc_roundtrip`
Expected: passes on Unix

- [ ] **Step 4: Commit**

```bash
git add crates/ahandd/tests/ipc_roundtrip.rs crates/ahandd/src/lib.rs
git commit -m "test: add IPC integration roundtrip tests"
```

---

## Task Dependency Graph

```
Task 0 (deps & config)
├── Task 1 (fs_perms) ──┐
├── Task 2 (dpapi) ─────┤
├── Task 3 (IPC server) ─┤── Task 6 (device identity) ─── Task 11 (IPC tests)
├── Task 4 (IPC client) ─┘
├── Task 5 (signals & process)
├── Task 7 (browser-init)
├── Task 8 (upgrade & admin)
├── Task 9 (install.ps1 & install-daemon)
└── Task 10 (CI) ← depends on all code tasks
```

Recommended execution order: 0 → 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11