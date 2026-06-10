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
}
