//! Race-proof filesystem syscalls — closes the TOCTOU window between
//! [`policy::FilePolicyChecker::check_path`](super::policy::FilePolicyChecker::check_path)
//! and the actual mutation by performing the syscall through a dirfd
//! that the attacker cannot redirect.
//!
//! # Threat model
//!
//! The classical race the policy layer is exposed to:
//!
//! ```text
//!     1. policy.check_path(req.path)     → canonical_path P
//!     2. <race window — attacker swaps an ancestor of P for a symlink>
//!     3. tokio::fs::<op>(P, …)           → kernel re-walks P
//! ```
//!
//! At step 3 the kernel follows the new symlink and the operation lands
//! outside the allowlist. The pre-existing [`super::verify_post_create`]
//! helper detects this *after the fact* and (for some op shapes) cleans
//! up the artifact. But the *operation itself* still ran outside the
//! allowlist — what the attacker wanted.
//!
//! This module reorders the work so the kernel walks the path **once**:
//!
//! ```text
//!     1. policy.check_path(req.path)         → canonical_path P
//!     2. safe_open_parent_dirfd_for(P)       → (parent_fd, basename)
//!     3. *at(parent_fd, basename, …)         → kernel resolves only
//!                                              `basename` against
//!                                              parent_fd's open inode
//! ```
//!
//! Once `parent_fd` exists, the attacker can no longer redirect the op:
//! the kernel does not re-traverse `/.../parent` for the *at syscall.
//! Step 2 itself can still be raced; we close that window via:
//!
//! - **Linux 5.6+**: `openat2(AT_FDCWD, parent, RESOLVE_NO_SYMLINKS)` —
//!   atomic kernel-side rejection of any symlink in the resolution
//!   path. ELOOP is the success-of-detection signal.
//! - **macOS / *BSD / older Linux**: chain `openat(O_NOFOLLOW |
//!   O_DIRECTORY)` one component at a time. ELOOP at any component
//!   means an attacker introduced a symlink during the race.
//!
//! # Platform support
//!
//! - **Linux**: openat2 fast path + chain-open fallback (pre-5.6).
//!   Bullet-proof.
//! - **macOS / *BSD**: chain-open. Bullet-proof modulo the very first
//!   ancestor having been swapped before [`policy.check_path`]'s own
//!   [`std::fs::canonicalize`] returned — but `canonicalize` reads
//!   each component synchronously, so that race is identical to "the
//!   policy check itself was wrong", not new attack surface this
//!   module introduces.
//! - **Windows**: no equivalent API ships with std/libc. The race
//!   window remains. Daemon deployments on Windows assume a
//!   single-tenant host where this attacker class is out of model;
//!   call sites fall back to path-based syscalls there.
//!
//! # Scope
//!
//! Closes the race for **write-class** dispatch arms — Mkdir, Move,
//! Copy, CreateSymlink, Chmod. Read-class arms (Stat, List, Glob,
//! ReadText, ReadBinary, ReadImage) are out of scope: the TOCTOU
//! impact of redirecting a *read* is leaking metadata, which the
//! existing canonicalization already constrains, and refactoring the
//! tokio I/O paths would touch significantly more code for marginal
//! benefit.
//!
//! Recursive variants of the supported ops (mkdir -p, recursive copy,
//! recursive chmod) close the race for the **outermost** dirfd open;
//! the inner walk uses path-based ops. That inner window is bounded
//! to attacker-controlled subtrees of the validated root, so it can't
//! escape the allowlist — the documented residual is "an attacker
//! racing inside their own subtree can shuffle which of their files
//! get touched", which is no escalation over what they already control.

#![cfg(unix)]

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use ahand_protocol::{FileError, FileErrorCode};

use super::file_error;

/// A safely-opened parent directory plus the basename to use with *at
/// syscalls. Returned by [`safe_open_parent_dirfd_for`].
///
/// The fd is `OwnedFd` so it closes on drop — never leak it as a raw fd
/// without keeping the owning value alive, otherwise a long-running op
/// (e.g. recursive copy) can have its fd reaped underneath it.
#[derive(Debug)]
pub struct DirHandle {
    pub fd: OwnedFd,
    pub basename: OsString,
}

/// Open the parent directory of `canonical` in a way that closes the
/// TOCTOU race against ancestor-symlink injection, and return it
/// alongside the leaf basename ready for `*at` syscalls.
///
/// Caller contract: `canonical` MUST be the value returned by
/// [`policy::FilePolicyChecker::check_path`](super::policy::FilePolicyChecker::check_path)
/// (or otherwise a value that has already been canonicalized and
/// allowlist-checked). Passing an arbitrary user-supplied path skips
/// the policy step and defeats the purpose.
pub fn safe_open_parent_dirfd_for(canonical: &Path) -> Result<DirHandle, FileError> {
    let basename = canonical
        .file_name()
        .ok_or_else(|| {
            file_error(
                FileErrorCode::InvalidPath,
                &canonical.to_string_lossy(),
                "path has no basename — cannot use it for an *at syscall",
            )
        })?
        .to_os_string();
    // `Path::parent` returns `None` only for path == "/" (or "" / single
    // component on relative paths, which we already excluded above by
    // requiring `file_name`). Treat that as a hard error: no caller has
    // a legitimate reason to operate on "/" itself via these helpers.
    let parent = canonical.parent().ok_or_else(|| {
        file_error(
            FileErrorCode::InvalidPath,
            &canonical.to_string_lossy(),
            "path has no parent — refusing to operate on the filesystem root",
        )
    })?;
    let fd = open_dir_no_symlinks(parent).map_err(|e| io_to_safe_open_error(e, canonical))?;
    Ok(DirHandle { fd, basename })
}

/// Open `canonical` as a directory, refusing to follow any symlink in
/// the resolution path. Same semantics as [`safe_open_parent_dirfd_for`]
/// but for the directory itself rather than its parent — used by
/// recursive ops (e.g. `mkdir -p`'s last step, recursive walks) that
/// need a dirfd to the leaf after it has been created.
pub fn safe_open_dir(canonical: &Path) -> Result<OwnedFd, FileError> {
    open_dir_no_symlinks(canonical).map_err(|e| io_to_safe_open_error(e, canonical))
}

/// Internal: open a directory either via Linux `openat2(RESOLVE_NO_SYMLINKS)`
/// (5.6+) or fall back to a per-component `openat(O_NOFOLLOW)` chain.
fn open_dir_no_symlinks(path: &Path) -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    {
        match linux::open_dir_via_openat2(path) {
            Ok(fd) => return Ok(fd),
            // Pre-5.6 kernel: openat2 returns ENOSYS. Fall through to chain-open.
            // Note: every container/VM running an older kernel (RHEL 7, etc.)
            // hits this branch. We must keep the fallback or those hosts
            // would refuse every file op we route through here.
            Err(err) if err.raw_os_error() == Some(libc::ENOSYS) => {}
            // Anything else (ELOOP, EACCES, ENOENT…) is real and propagates.
            // ELOOP specifically is the success-of-detection signal — the
            // policy check passed but an ancestor turned into a symlink in
            // the race window.
            Err(err) => return Err(err),
        }
    }
    chain_open_dir(path)
}

/// Per-component O_NOFOLLOW walk from `/` to `path`. Each ancestor is
/// opened with `O_NOFOLLOW | O_DIRECTORY`; if any component is a symlink
/// the kernel returns ELOOP and we abort.
///
/// This is the macOS / older-Linux path. It's slower than `openat2` (one
/// syscall per component vs one total) but strictly equivalent in safety
/// for our threat model — RESOLVE_NO_SYMLINKS does the same thing
/// kernel-side; we just emulate it userspace-side here.
fn chain_open_dir(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "chain_open_dir requires an absolute, canonicalized path",
        ));
    }

    // Anchor at `/`. O_NOFOLLOW does not apply to the root itself —
    // there's nothing above it for a symlink to redirect to. From here
    // every step uses NOFOLLOW.
    let root_cstr = CString::new("/").expect("\"/\" has no NUL byte");
    // SAFETY: root_cstr is a valid NUL-terminated C string; flags are valid;
    // libc::open returns a fresh kernel fd that we wrap exclusively below.
    let root_fd = unsafe {
        libc::open(
            root_cstr.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: root_fd is a freshly-opened kernel fd >= 0, owned exclusively
    // by us. Wrapping in OwnedFd transfers responsibility for close().
    let mut current = unsafe { OwnedFd::from_raw_fd(root_fd) };

    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                current = openat_dir_nofollow(&current, name)?;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "chain_open_dir requires a canonicalized path \
                     (no `..` or windows prefix components)",
                ));
            }
        }
    }
    Ok(current)
}

fn openat_dir_nofollow(parent: &OwnedFd, name: &OsStr) -> io::Result<OwnedFd> {
    let cstr = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "component contains NUL byte"))?;
    // SAFETY: parent is a valid open dirfd (OwnedFd guarantees it is open);
    // cstr is a valid NUL-terminated C string. openat returns either >= 0
    // (a fresh fd we own exclusively) or < 0 (errno set, no fd allocated).
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            cstr.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd >= 0 and exclusively ours.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// Mirrors `struct open_how` from `<linux/openat2.h>`. Three u64
    /// fields, no padding, kernel ABI-stable since 5.6.
    ///
    /// - `flags`   — same bits as the second arg of `openat()`.
    /// - `mode`    — file-creation mode (only with O_CREAT/O_TMPFILE).
    /// - `resolve` — `RESOLVE_*` bitset constraining how the kernel
    ///               walks the path. We set `RESOLVE_NO_SYMLINKS` so any
    ///               symlink in the resolution path → ELOOP, atomically.
    #[repr(C)]
    #[derive(Default)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    /// `RESOLVE_NO_SYMLINKS` per `<linux/openat2.h>`. The whole point.
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    /// Linux syscall number for `openat2`. Same value across x86_64,
    /// aarch64, riscv64 and the modern 32-bit ABIs (added in 5.6 with a
    /// shared number; older syscall-table forks pre-date the syscall).
    const SYS_OPENAT2: libc::c_long = 437;

    pub fn open_dir_via_openat2(path: &Path) -> io::Result<OwnedFd> {
        let cstr = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
        let how = OpenHow {
            flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
            mode: 0,
            resolve: RESOLVE_NO_SYMLINKS,
        };
        // SAFETY: cstr is valid NUL-terminated; `&how` points to a
        // properly-aligned OpenHow with the kernel-expected layout; size_of
        // matches the `usize` arg the kernel uses to validate the struct
        // length. `libc::syscall` returns < 0 on error with errno set.
        let ret = unsafe {
            libc::syscall(
                SYS_OPENAT2,
                libc::AT_FDCWD,
                cstr.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat2 success → ret is a fresh kernel fd we own.
        Ok(unsafe { OwnedFd::from_raw_fd(ret as libc::c_int) })
    }
}

/// Safely materialize the directory chain `canonical` (`mkdir -p`-style)
/// using the same per-component O_NOFOLLOW walk as [`chain_open_dir`], but
/// creating any missing components along the way.
///
/// Used by `handle_mkdir` with `recursive=true`. The dirfd-walk replaces
/// `std::fs::create_dir_all`, which would re-walk the whole path on every
/// component and is racy for the same reason path-based ops are.
///
/// Edge cases:
/// - **Concurrent mkdir** by another thread/process: the per-component
///   `mkdirat` may race with someone else creating the same dir. We
///   tolerate `EEXIST` and proceed to `openat` the (now-existing) dir.
/// - **Symlink swap during the walk**: if a component we intended to
///   create or descend into shows up as a symlink, `openat(O_NOFOLLOW)`
///   returns ELOOP (Linux) or ENOTDIR (macOS) — both surface as
///   `PolicyDenied` via [`io_to_safe_open_error`].
pub fn safe_mkdirp(canonical: &Path, mode: u32) -> io::Result<()> {
    if !canonical.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "safe_mkdirp requires an absolute, canonicalized path",
        ));
    }
    // Anchor at "/". Same flag set as chain_open_dir's root open.
    let root_cstr = CString::new("/").expect("\"/\" has no NUL byte");
    // SAFETY: valid C string, valid flags; we wrap the returned fd as OwnedFd.
    let root_fd = unsafe {
        libc::open(
            root_cstr.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: open returned a fresh fd we own exclusively.
    let mut current = unsafe { OwnedFd::from_raw_fd(root_fd) };

    for component in canonical.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => match openat_dir_nofollow(&current, name) {
                Ok(fd) => current = fd,
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
                    // Component doesn't exist — create it, then descend.
                    let cstr = name_cstr(name)?;
                    // SAFETY: current is a valid dirfd; cstr is NUL-terminated.
                    let ret = unsafe {
                        libc::mkdirat(current.as_raw_fd(), cstr.as_ptr(), mode as libc::mode_t)
                    };
                    if ret < 0 {
                        let err = io::Error::last_os_error();
                        // EEXIST: another thread/process created it under us.
                        // Fine — we'll just openat into the existing one.
                        if err.raw_os_error() != Some(libc::EEXIST) {
                            return Err(err);
                        }
                    }
                    current = openat_dir_nofollow(&current, name)?;
                }
                Err(e) => return Err(e),
            },
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "safe_mkdirp requires a canonicalized path \
                     (no `..` or windows prefix components)",
                ));
            }
        }
    }
    Ok(())
}

/// `fstatat(AT_SYMLINK_NOFOLLOW)` on `name` relative to `parent`. Returns
/// `Ok(Some(st))` if the entry exists (regular file, dir, or symlink),
/// `Ok(None)` if it does not, or `Err` on any other failure (permission
/// denied, etc.).
///
/// Used for the existence/already-exists check in handlers that need to
/// distinguish "path exists as a dir → no-op success" from "path exists
/// as a non-dir → AlreadyExists error" without re-walking the parent
/// chain through path-based syscalls.
pub fn fstatat_nofollow(parent: &OwnedFd, name: &OsStr) -> io::Result<Option<libc::stat>> {
    let cstr = name_cstr(name)?;
    // SAFETY: zero-init is valid for libc::stat (POD); kernel populates on success.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: parent is a valid dirfd; cstr is NUL-terminated; &mut st points
    // to a properly-aligned writable stat buffer.
    let ret = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            cstr.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if ret < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(err);
    }
    Ok(Some(st))
}

/// Convenience: classify what an [`fstatat_nofollow`] result represents,
/// for handlers that only need the file-type bit.
pub fn stat_is_dir(st: &libc::stat) -> bool {
    (st.st_mode & libc::S_IFMT) == libc::S_IFDIR
}

/// Same as [`stat_is_dir`] but for symlinks.
pub fn stat_is_symlink(st: &libc::stat) -> bool {
    (st.st_mode & libc::S_IFMT) == libc::S_IFLNK
}

// ── *at syscall wrappers used by handlers ─────────────────────────────────

pub fn mkdirat(parent: &OwnedFd, name: &OsStr, mode: u32) -> io::Result<()> {
    let cstr = name_cstr(name)?;
    // SAFETY: parent is a valid open dirfd; cstr is NUL-terminated.
    let ret = unsafe { libc::mkdirat(parent.as_raw_fd(), cstr.as_ptr(), mode as libc::mode_t) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn renameat(
    src_parent: &OwnedFd,
    src_name: &OsStr,
    dst_parent: &OwnedFd,
    dst_name: &OsStr,
) -> io::Result<()> {
    let src_cstr = name_cstr(src_name)?;
    let dst_cstr = name_cstr(dst_name)?;
    // SAFETY: both parents are valid open dirfds; both cstrs are NUL-terminated.
    let ret = unsafe {
        libc::renameat(
            src_parent.as_raw_fd(),
            src_cstr.as_ptr(),
            dst_parent.as_raw_fd(),
            dst_cstr.as_ptr(),
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn symlinkat(target: &OsStr, parent: &OwnedFd, link_name: &OsStr) -> io::Result<()> {
    let target_cstr = CString::new(target.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "target contains NUL byte"))?;
    let link_cstr = name_cstr(link_name)?;
    // SAFETY: parent is a valid open dirfd; both cstrs are NUL-terminated.
    let ret =
        unsafe { libc::symlinkat(target_cstr.as_ptr(), parent.as_raw_fd(), link_cstr.as_ptr()) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// chmod on `name` relative to `parent`. Symlink-follow semantics match
/// the legacy `std::fs::set_permissions` path: when the leaf is a
/// regular file/dir we set its mode; when the leaf is a symlink, the
/// behavior is platform-dependent (Linux returns ENOTSUP if we asked
/// for AT_SYMLINK_NOFOLLOW, but here we *don't* set that flag — the
/// upstream `reject_if_final_component_is_symlink` already covers the
/// no-follow opt-in case).
pub fn fchmodat(parent: &OwnedFd, name: &OsStr, mode: u32) -> io::Result<()> {
    let cstr = name_cstr(name)?;
    // SAFETY: parent is a valid open dirfd; cstr is NUL-terminated.
    let ret = unsafe { libc::fchmodat(parent.as_raw_fd(), cstr.as_ptr(), mode as libc::mode_t, 0) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Open `name` for write under `parent`. Used by single-file copy. The
/// `truncate`/`exclusive` flags map to `O_TRUNC`/`O_EXCL`; `O_NOFOLLOW`
/// is always set so the leaf basename cannot itself be redirected via a
/// symlink during the race window between [`safe_open_parent_dirfd_for`]
/// returning and this call landing.
pub fn openat_create_write(
    parent: &OwnedFd,
    name: &OsStr,
    truncate: bool,
    exclusive: bool,
    create_mode: u32,
) -> io::Result<OwnedFd> {
    let cstr = name_cstr(name)?;
    let mut flags = libc::O_WRONLY | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if truncate {
        flags |= libc::O_TRUNC;
    }
    if exclusive {
        flags |= libc::O_EXCL;
    }
    // SAFETY: parent is a valid dirfd; cstr is NUL-terminated; flags include
    // O_CREAT so the mode arg is honored.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            cstr.as_ptr(),
            flags,
            create_mode as libc::c_int,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd >= 0 and exclusively ours.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Open `name` for read under `parent`, refusing to follow a symlink leaf.
pub fn openat_read_nofollow(parent: &OwnedFd, name: &OsStr) -> io::Result<OwnedFd> {
    let cstr = name_cstr(name)?;
    // SAFETY: parent is a valid dirfd; cstr is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            cstr.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd >= 0 and exclusively ours.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn name_cstr(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "basename contains NUL byte"))
}

/// Map an [`io::Error`] from an *at syscall back to a [`FileError`] in the
/// same shape the legacy path-based handlers used. Public so handlers can
/// route errors from [`mkdirat`], [`renameat`] etc. without re-deriving
/// the mapping.
pub fn io_to_file_error(err: io::Error, path: &Path) -> FileError {
    let path_str = path.to_string_lossy().into_owned();
    let code = match err.raw_os_error() {
        Some(libc::ELOOP) | Some(libc::ENOTDIR) if err.kind() != io::ErrorKind::NotFound => {
            // ENOTDIR can fire on legitimate "is a regular file" responses
            // (e.g. mkdirat-ing into a dir whose parent is actually a file).
            // The TOCTOU-detection-vs-ordinary-misuse line is fuzzy; we map
            // both to PolicyDenied because the legacy path-based handlers
            // didn't have to distinguish either, and operators investigating
            // these are better served by "denied — ancestor mismatch" than
            // by a generic IO error.
            FileErrorCode::PolicyDenied
        }
        _ => match err.kind() {
            io::ErrorKind::NotFound => FileErrorCode::NotFound,
            io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
            io::ErrorKind::AlreadyExists => FileErrorCode::AlreadyExists,
            _ => match err.raw_os_error() {
                Some(libc::ENOTDIR) => FileErrorCode::NotADirectory,
                Some(libc::EISDIR) => FileErrorCode::IsADirectory,
                Some(libc::ENOTEMPTY) => FileErrorCode::NotEmpty,
                Some(libc::ELOOP) => FileErrorCode::PolicyDenied,
                _ => FileErrorCode::Io,
            },
        },
    };
    FileError {
        code: code as i32,
        message: err.to_string(),
        path: path_str,
    }
}

fn io_to_safe_open_error(err: io::Error, canonical: &Path) -> FileError {
    let path_str = canonical.to_string_lossy().into_owned();
    // ELOOP / ENOTDIR from the no-symlinks walk are the TOCTOU-detection
    // signals we want to surface. Map them both to PolicyDenied so callers
    // (and the operator staring at the error log) understand "this was
    // rejected because an ancestor turned out to be a symlink or a non-dir",
    // not "your disk is broken".
    //
    // Why both errors map the same way:
    // - ELOOP: openat2(RESOLVE_NO_SYMLINKS) and Linux's openat(O_NOFOLLOW)
    //   return this when an ancestor is a symlink — the canonical signal.
    // - ENOTDIR: macOS returns this when openat(O_DIRECTORY|O_NOFOLLOW)
    //   hits a symlink leaf (the kernel sees O_DIRECTORY first, the
    //   symlink isn't a dir, so ENOTDIR wins over ELOOP). It also fires
    //   when the ancestor was swapped for a regular file. Both are
    //   "ancestor is not what policy validated" — denied.
    //
    // The path string in the error is the canonical the policy already
    // approved — so the operator can see what should have worked and
    // reason about which ancestor must have been swapped.
    let code = match err.raw_os_error() {
        Some(libc::ELOOP) | Some(libc::ENOTDIR) => FileErrorCode::PolicyDenied,
        Some(libc::ENOENT) => FileErrorCode::NotFound,
        Some(libc::EACCES) | Some(libc::EPERM) => FileErrorCode::PermissionDenied,
        _ => FileErrorCode::Io,
    };
    FileError {
        code: code as i32,
        message: format!(
            "safe parent-dir open rejected: {err} \
             (likely TOCTOU race or stale canonicalization)"
        ),
        path: path_str,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// macOS resolves `/var` → `/private/var`; the chain-open walk needs
    /// the real on-disk path, so canonicalize the temp root the same way
    /// the policy layer does.
    fn canon(p: &Path) -> std::path::PathBuf {
        p.canonicalize().expect("tempdir canonicalize")
    }

    #[test]
    fn safe_open_parent_handles_real_dir() {
        let tmp = TempDir::new().unwrap();
        let root = canon(tmp.path());
        let leaf = root.join("leaf.txt");
        // The leaf doesn't exist yet — that's fine, we only open the parent.
        let handle = safe_open_parent_dirfd_for(&leaf).expect("real parent must succeed");
        assert_eq!(handle.basename, OsStr::new("leaf.txt"));
        // fd should be a valid open dirfd: stat'ing self via fstat should work.
        let raw = handle.fd.as_raw_fd();
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstat(raw, &mut st) };
        assert_eq!(rc, 0, "fstat on returned dirfd must succeed");
        // S_IFDIR check
        assert_eq!(st.st_mode & libc::S_IFMT, libc::S_IFDIR);
    }

    #[test]
    fn safe_open_parent_rejects_root() {
        // `/` has no parent; refuse politely rather than panicking.
        let err = safe_open_parent_dirfd_for(Path::new("/")).unwrap_err();
        assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    }

    #[cfg(unix)]
    #[test]
    fn safe_open_parent_rejects_symlinked_ancestor() {
        // Reproduce the core TOCTOU shape: a symlink in the ancestor chain
        // must cause the safe-open to fail (no_symlinks rejection is the
        // whole point of this module).
        let tmp = TempDir::new().unwrap();
        let root = canon(tmp.path());
        // Layout:
        //   root/real_dir/        (real)
        //   root/link_dir -> real_dir   (symlink)
        //   root/link_dir/leaf.txt      (path through the symlink)
        let real_dir = root.join("real_dir");
        fs::create_dir(&real_dir).unwrap();
        let link_dir = root.join("link_dir");
        std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();
        let through_link = link_dir.join("leaf.txt");

        // safe_open_parent_dirfd_for of `link_dir/leaf.txt` must reject:
        // its parent `link_dir` is a symlink, and our walk refuses any
        // symlink in the resolution path. The error should be
        // PolicyDenied (mapped from ELOOP), not Io or NotFound.
        let err = safe_open_parent_dirfd_for(&through_link).unwrap_err();
        assert_eq!(
            err.code,
            FileErrorCode::PolicyDenied as i32,
            "expected PolicyDenied (symlinked ancestor), got code={} message={:?}",
            err.code,
            err.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn safe_open_parent_accepts_canonicalized_real_path() {
        // After `fs::canonicalize`, no symlinks remain in the resolution
        // path — chain-open / openat2 must succeed. This is the happy
        // path that every successful op flows through.
        let tmp = TempDir::new().unwrap();
        let root = canon(tmp.path());
        let real = root.join("dir");
        fs::create_dir(&real).unwrap();
        // Create a symlink alongside the real dir, then canonicalize.
        let link = root.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let through_link = link.join("file.txt");
        let canonical = canonicalize_or_real(&through_link, &real);
        assert!(
            !canonical.starts_with(&link),
            "test setup: canonicalized path must collapse the symlink"
        );
        // safe-open the canonical (which goes through the real dir) — must succeed.
        safe_open_parent_dirfd_for(&canonical).expect("canonical real-path must open");
    }

    /// Test helper: emulate `policy::canonicalize_or_parent` for a
    /// not-yet-existing leaf — canonicalize the existing parent and
    /// re-append the basename. The policy module owns the production
    /// version; we replicate the relevant slice here so this unit test
    /// doesn't depend on cross-module visibility.
    fn canonicalize_or_real(path: &Path, existing_ancestor: &Path) -> std::path::PathBuf {
        let canonical_ancestor = existing_ancestor.canonicalize().unwrap();
        let basename = path.file_name().unwrap();
        canonical_ancestor.join(basename)
    }
}
