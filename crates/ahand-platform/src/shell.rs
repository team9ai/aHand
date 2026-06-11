//! Shell resolution. Unix: `$SHELL`, fallback `/bin/sh`, login flag `-l`.
//! Windows: `%COMSPEC%`, fallback `cmd.exe`, no login concept.
//!
//! NOTE: callers send shell *arguments* over the protocol (e.g. `-c <cmd>`),
//! which are inherently platform-flavored; M1 does not translate them. The
//! daemon only guarantees the shell BINARY resolves per-platform.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSpec {
    pub path: String,
    /// Args injected before user args when the "shell"/"$SHELL" sentinel is
    /// used (`-l` on Unix; empty on Windows).
    pub login_args: Vec<String>,
}

/// The platform fallback shell (ignores environment).
pub fn default_shell() -> ShellSpec {
    #[cfg(unix)]
    {
        ShellSpec {
            path: "/bin/sh".to_string(),
            login_args: vec!["-l".to_string()],
        }
    }
    #[cfg(windows)]
    {
        ShellSpec {
            path: "cmd.exe".to_string(),
            login_args: Vec::new(),
        }
    }
}

/// The user's configured shell from the environment (`SHELL` / `COMSPEC`),
/// or `None` if unset.
pub fn env_shell() -> Option<String> {
    #[cfg(unix)]
    {
        std::env::var("SHELL").ok()
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").ok()
    }
}

/// The "run this command string" flag: `-c` (Unix) / `/C` (Windows).
pub fn shell_c_flag() -> &'static str {
    if cfg!(windows) { "/C" } else { "-c" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_matches_platform() {
        let s = default_shell();
        #[cfg(unix)]
        {
            assert_eq!(s.path, "/bin/sh");
            assert_eq!(s.login_args, vec!["-l".to_string()]);
        }
        #[cfg(windows)]
        {
            assert_eq!(s.path, "cmd.exe");
            assert!(s.login_args.is_empty());
        }
    }

    #[test]
    fn shell_c_flag_matches_platform() {
        #[cfg(unix)]
        assert_eq!(shell_c_flag(), "-c");
        #[cfg(windows)]
        assert_eq!(shell_c_flag(), "/C");
    }

    // ── env_shell Some/None branches (#4) ─────────────────────────────────────
    //
    // Rust 2024: std::env::set_var / remove_var are unsafe. We guard all
    // mutations under a static Mutex so concurrent tests cannot race on the
    // process-global environment.

    #[cfg(unix)]
    mod env_shell_tests_unix {
        use super::*;
        use std::sync::Mutex;

        static ENV_MUTEX: Mutex<()> = Mutex::new(());

        #[test]
        fn env_shell_returns_some_when_shell_is_set() {
            let _guard = ENV_MUTEX.lock().unwrap();
            let original = std::env::var("SHELL").ok();
            // SAFETY: guarded by ENV_MUTEX; no other thread touches SHELL concurrently.
            unsafe { std::env::set_var("SHELL", "/bin/test-shell") };
            let result = env_shell();
            // Restore.
            if let Some(v) = &original {
                unsafe { std::env::set_var("SHELL", v) }
            } else {
                unsafe { std::env::remove_var("SHELL") }
            }
            assert_eq!(result, Some("/bin/test-shell".to_string()));
        }

        #[test]
        fn env_shell_returns_none_when_shell_is_unset() {
            let _guard = ENV_MUTEX.lock().unwrap();
            let original = std::env::var("SHELL").ok();
            // SAFETY: guarded by ENV_MUTEX.
            unsafe { std::env::remove_var("SHELL") };
            let result = env_shell();
            // Restore.
            if let Some(v) = &original {
                unsafe { std::env::set_var("SHELL", v) }
            }
            assert!(result.is_none(), "SHELL unset should return None");
        }
    }

    #[cfg(windows)]
    mod env_shell_tests_windows {
        use super::*;
        use std::sync::Mutex;

        static ENV_MUTEX: Mutex<()> = Mutex::new(());

        #[test]
        fn env_shell_returns_some_when_comspec_is_set() {
            let _guard = ENV_MUTEX.lock().unwrap();
            let original = std::env::var("COMSPEC").ok();
            // SAFETY: guarded by ENV_MUTEX; no other thread touches COMSPEC concurrently.
            unsafe { std::env::set_var("COMSPEC", r"C:\Windows\System32\cmd.exe") };
            let result = env_shell();
            match &original {
                Some(v) => unsafe { std::env::set_var("COMSPEC", v) },
                None => unsafe { std::env::remove_var("COMSPEC") },
            }
            assert_eq!(result, Some(r"C:\Windows\System32\cmd.exe".to_string()));
        }

        #[test]
        fn env_shell_returns_none_when_comspec_is_unset() {
            let _guard = ENV_MUTEX.lock().unwrap();
            let original = std::env::var("COMSPEC").ok();
            // SAFETY: guarded by ENV_MUTEX.
            unsafe { std::env::remove_var("COMSPEC") };
            let result = env_shell();
            if let Some(v) = &original {
                unsafe { std::env::set_var("COMSPEC", v) }
            }
            assert!(result.is_none(), "COMSPEC unset should return None");
        }
    }
}
