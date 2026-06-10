//! Cross-platform local IPC transport.
//!
//! Unix: `UnixListener`/`UnixStream` at a filesystem socket path, with a
//! configurable file mode and `peer_cred`-derived peer identity.
//! Windows: named pipes (`\\.\pipe\ahandd-<user>`). The default pipe
//! security descriptor only grants write access to the creating user (plus
//! Administrators/SYSTEM), so cross-user clients cannot connect; the `mode`
//! argument is ignored and peer identity is reported as `"pipe:local"`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// An IPC endpoint address.
///
/// On Unix this is a filesystem path to a Unix domain socket.
/// On Windows this is a named-pipe path of the form `\\.\pipe\<name>`.
#[derive(Clone, Debug)]
pub struct IpcEndpoint(PathBuf);

impl IpcEndpoint {
    /// Construct an endpoint from an explicit path.
    pub fn from_path(p: PathBuf) -> Self {
        Self(p)
    }

    /// Return the underlying path.
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Default endpoint for the current user.
    ///
    /// * Unix — `~/.ahand/ahandd.sock`
    /// * Windows — `\\.\pipe\ahandd-<USERNAME>`
    pub fn default_for_user() -> Self {
        #[cfg(unix)]
        {
            Self(home_dir().join(".ahand").join("ahandd.sock"))
        }
        #[cfg(windows)]
        {
            let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".into());
            Self(PathBuf::from(format!(r"\\.\pipe\ahandd-{user}")))
        }
    }
}

#[cfg(unix)]
fn home_dir() -> PathBuf {
    // ahand-platform deliberately avoids the `dirs` crate; HOME is always set
    // in practice (ahandd config loading hard-fails without it — see the I2
    // note on expand_tilde_with in crates/ahandd/src/config.rs).
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Platform stream type returned from `IpcListener::accept`.
#[cfg(unix)]
pub type IpcServerStream = tokio::net::UnixStream;

/// Platform stream type returned from `ipc_connect`.
#[cfg(unix)]
pub type IpcClientStream = tokio::net::UnixStream;

/// Platform stream type returned from `IpcListener::accept`.
#[cfg(windows)]
pub type IpcServerStream = tokio::net::windows::named_pipe::NamedPipeServer;

/// Platform stream type returned from `ipc_connect`.
#[cfg(windows)]
pub type IpcClientStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// Listens for incoming IPC connections.
pub struct IpcListener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,

    /// The pending server instance waiting for `.connect()`.
    ///
    /// On Windows, named pipe accept is a two-step process:
    ///
    /// 1. Create a server instance (the "slot").
    /// 2. Call `.connect()` to wait for a client.
    ///
    /// We pre-create the *next* slot immediately after each accept so there is
    /// no window where the pipe name has zero live instances.
    ///
    /// The field is `Option` so we can `take()` it before `.connect()`.  It
    /// should always be `Some` between calls to `accept()`; if a previous
    /// accept left it `None` (e.g. transient pipe exhaustion), the next call
    /// recreates the instance lazily so a log-and-continue accept loop
    /// self-heals instead of panicking the daemon.
    #[cfg(windows)]
    next: Option<tokio::net::windows::named_pipe::NamedPipeServer>,

    #[cfg(windows)]
    name: String,
}

impl IpcListener {
    /// Bind the endpoint and start listening.
    ///
    /// `mode` is the Unix socket file mode (e.g. `0o660`); it is ignored on
    /// Windows (see module docs).
    #[allow(unused_variables)]
    pub fn bind(endpoint: &IpcEndpoint, mode: u32) -> Result<Self> {
        #[cfg(unix)]
        {
            let path = endpoint.as_path();
            // Remove a stale socket file from a previous run.
            let _ = std::fs::remove_file(path);
            // Create parent directories (e.g. ~/.ahand/).
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            let inner = tokio::net::UnixListener::bind(path)
                .with_context(|| format!("bind {}", path.display()))?;
            // Apply requested file mode.
            use std::os::unix::fs::PermissionsExt;
            // The bind→chmod window (socket briefly at umask-default perms) is
            // intentional parity with the previous ahandd behavior; the socket
            // lives under the user's home directory, so the exposure window is
            // acceptable.
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .with_context(|| format!("chmod {:04o} {}", mode, path.display()))?;
            Ok(Self { inner })
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let name = endpoint.as_path().to_string_lossy().into_owned();
            let first = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)
                .with_context(|| format!("create named pipe {name} (already exists? another daemon or a squatter may hold the name)"))?;
            Ok(Self {
                next: Some(first),
                name,
            })
        }
    }

    /// Accept one incoming connection.
    ///
    /// Returns the per-platform stream and a peer-identity string:
    /// - Unix: `"uid:<n>"` (from `peer_cred`)
    /// - Windows: `"pipe:local"` (Windows named pipes do not expose per-client
    ///   UIDs through Tokio's API; the SD restricts callers anyway)
    ///
    /// # Windows error semantics
    ///
    /// The listener holds a pre-created pipe instance in `self.next`.  On
    /// entry we take it (leaving `None`), await the client, then immediately
    /// pre-create the *next* instance.  If `self.next` is `None` on entry
    /// (because a previous accept failed to pre-create it), the instance is
    /// recreated lazily so a log-and-continue accept loop self-heals.
    /// If the pre-creation fails after a successful connect we return that
    /// error, leaving `next` as `None` for the next lazy-recreate attempt.
    pub async fn accept(&mut self) -> Result<(IpcServerStream, String)> {
        #[cfg(unix)]
        {
            let (stream, _addr) = self.inner.accept().await.context("IPC accept")?;
            let peer = match stream.peer_cred() {
                Ok(cred) => format!("uid:{}", cred.uid()),
                Err(_) => "uid:unknown".to_string(),
            };
            Ok((stream, peer))
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            // Take the pending instance.  It is normally Some between accept
            // calls; if a previous accept() failed to pre-create the next
            // instance (e.g. transient pipe exhaustion), recreate it lazily so
            // a log-and-continue accept loop self-heals instead of panicking.
            let server = match self.next.take() {
                Some(s) => s,
                // A previous accept() failed to pre-create the next instance
                // (e.g. transient pipe exhaustion). Recreate lazily so a
                // log-and-continue accept loop self-heals instead of
                // panicking the daemon.
                None => ServerOptions::new()
                    .create(&self.name)
                    .with_context(|| format!("lazy recreate named pipe {}", self.name))?,
            };

            // Wait for a client to connect.
            let connect_result = server.connect().await.context("IPC pipe connect");

            // Pre-create the next slot regardless of whether connect succeeded.
            // If pre-creation fails, store the error but still propagate the
            // connect error if there was one (connect error takes priority).
            let recreate_result = ServerOptions::new()
                .create(&self.name)
                .with_context(|| format!("recreate named pipe {}", self.name));

            match recreate_result {
                Ok(next_instance) => {
                    self.next = Some(next_instance);
                }
                Err(recreate_err) => {
                    // Leave next as None — listener is now unusable.
                    // Only surface this error if connect also succeeded
                    // (otherwise the connect error is more informative).
                    connect_result?;
                    return Err(recreate_err);
                }
            }

            // Propagate any connect error (next is already repopulated above).
            connect_result?;

            Ok((server, "pipe:local".to_string()))
        }
    }
}

/// Connect to the daemon IPC endpoint as a client.
pub async fn ipc_connect(endpoint: &IpcEndpoint) -> Result<IpcClientStream> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(endpoint.as_path())
            .await
            .with_context(|| format!("connect {}", endpoint.as_path().display()))
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        // ERROR_PIPE_BUSY: all pipe instances are busy; retry after a short sleep.
        // A missing daemon still fails fast via the catch-all arm below.
        const ERROR_PIPE_BUSY: i32 = 231;
        const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        let name = endpoint.as_path().to_string_lossy().into_owned();
        let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(client),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(e).with_context(|| {
                            format!(
                                "IPC pipe busy: all instances of {name} still in use after {CONNECT_TIMEOUT:?}"
                            )
                        });
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e).with_context(|| format!("connect {name}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_endpoint(tag: &str) -> IpcEndpoint {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            // Keep the tempdir alive for the test process lifetime: the
            // socket path must outlive the returned endpoint.
            let path = Box::leak(Box::new(dir)).path().join(format!("{tag}.sock"));
            IpcEndpoint::from_path(path)
        }
        #[cfg(windows)]
        {
            IpcEndpoint::from_path(std::path::PathBuf::from(format!(
                r"\\.\pipe\ahand-test-{tag}-{}",
                std::process::id()
            )))
        }
    }

    #[tokio::test]
    async fn loopback_roundtrip() {
        let ep = test_endpoint("roundtrip");
        let mut listener = IpcListener::bind(&ep, 0o660).expect("bind");
        let server = tokio::spawn(async move {
            let (mut stream, peer) = listener.accept().await.expect("accept");
            assert!(!peer.is_empty());
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await.expect("read");
            assert_eq!(&buf, b"ping");
            stream.write_all(b"pong").await.expect("write");
            stream.flush().await.expect("flush");
        });
        let mut client = ipc_connect(&ep).await.expect("connect");
        client.write_all(b"ping").await.expect("write");
        client.flush().await.expect("flush");
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).await.expect("read");
        assert_eq!(&buf, b"pong");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn sequential_connections_are_accepted() {
        // Pins the windows next-instance pre-creation logic: a second client
        // must be able to connect after the first disconnects.
        let ep = test_endpoint("sequential");
        let mut listener = IpcListener::bind(&ep, 0o660).expect("bind");
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _peer) = listener.accept().await.expect("accept");
                let mut buf = [0u8; 2];
                stream.read_exact(&mut buf).await.expect("read");
                stream.write_all(&buf).await.expect("write");
                stream.flush().await.expect("flush");
            }
        });
        for i in 0..2u8 {
            let mut client = ipc_connect(&ep).await.expect("connect");
            client.write_all(&[i, i]).await.expect("write");
            let mut buf = [0u8; 2];
            client.read_exact(&mut buf).await.expect("read");
            assert_eq!(buf, [i, i]);
            drop(client);
        }
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_mode_is_applied() {
        use std::os::unix::fs::PermissionsExt;
        let ep = test_endpoint("mode");
        let _listener = IpcListener::bind(&ep, 0o600).expect("bind");
        let mode = std::fs::metadata(ep.as_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn default_endpoint_shape() {
        let ep = IpcEndpoint::default_for_user();
        let s = ep.as_path().to_string_lossy().into_owned();
        #[cfg(unix)]
        assert!(s.ends_with(".ahand/ahandd.sock"), "{s}");
        #[cfg(windows)]
        assert!(s.starts_with(r"\\.\pipe\ahandd-"), "{s}");
    }
}
