//! Where a workspace's daemon listens, and how fronts reach it.
//!
//! One daemon per (workspace root, ts-r-lsp version). Its endpoint is derived from a BLAKE3
//! hash of the canonical root path, the crate version and [`PROTOCOL_VERSION`]:
//!
//! ```text
//! <cache>/lsp/<id>.sock    Unix domain socket (or /tmp/ts-r-lsp-<user>/<id>.sock if the
//!                          cache path is too long for sockaddr_un)
//! \\.\pipe\ts-r-lsp-<id>   named pipe on Windows
//! <cache>/lsp/<id>.lock    held while a front spawns the daemon (serializes racing fronts)
//! <cache>/lsp/<id>.pid     held (exclusive file lock) by the running daemon; contains its pid
//! <cache>/lsp/<id>.log     the daemon's stderr log
//! ```
//!
//! [`ensure_daemon`] connects, or takes the spawn lock, re-checks, spawns
//! `ts-r-lsp daemon --root <root>` detached and waits for the endpoint to accept.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Bumped whenever the front <-> daemon wire format changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// The files and socket of one workspace's daemon.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Canonical workspace root.
    pub root: PathBuf,
    /// 16 hex chars identifying the daemon.
    pub id: String,
    /// `<cache>/lsp`.
    pub dir: PathBuf,
    /// Socket path (Unix) or pipe name (Windows).
    pub address: String,
}

impl Endpoint {
    /// Endpoint for a workspace root (canonicalized when possible).
    pub fn for_root(root: &Path) -> Endpoint {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Self::for_root_in(&root, &ts_r_cache::cache_root().join("lsp"))
    }

    /// Endpoint with an explicit state directory (tests).
    pub fn for_root_in(root: &Path, dir: &Path) -> Endpoint {
        let mut h = blake3::Hasher::new();
        h.update(root.to_string_lossy().as_bytes());
        h.update(b"\0");
        h.update(env!("CARGO_PKG_VERSION").as_bytes());
        h.update(&PROTOCOL_VERSION.to_le_bytes());
        let id = h.finalize().to_hex()[..16].to_string();
        let address = address_for(dir, &id);
        Endpoint {
            root: root.to_path_buf(),
            id,
            dir: dir.to_path_buf(),
            address,
        }
    }

    /// Spawn lock file.
    pub fn spawn_lock_path(&self) -> PathBuf {
        self.dir.join(format!("{}.lock", self.id))
    }

    /// Daemon lock / pid file.
    pub fn pid_path(&self) -> PathBuf {
        self.dir.join(format!("{}.pid", self.id))
    }

    /// Daemon log file.
    pub fn log_path(&self) -> PathBuf {
        self.dir.join(format!("{}.log", self.id))
    }

    /// Directory for tsserver cancellation files.
    pub fn cancel_dir(&self) -> PathBuf {
        self.dir.join(format!("{}.cancel", self.id))
    }
}

#[cfg(unix)]
fn address_for(dir: &Path, id: &str) -> String {
    let p = dir.join(format!("{id}.sock"));
    let s = p.to_string_lossy().into_owned();
    // sockaddr_un.sun_path is 104 bytes on macOS, 108 on Linux.
    if s.len() < 100 {
        return s;
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(32)
        .collect();
    format!("/tmp/ts-r-lsp-{user}/{id}.sock")
}

#[cfg(windows)]
fn address_for(_dir: &Path, id: &str) -> String {
    format!(r"\\.\pipe\ts-r-lsp-{id}")
}

// ---------------------------------------------------------------------------------------
// Streams and listeners
// ---------------------------------------------------------------------------------------

/// A connected byte stream between a front and the daemon.
#[derive(Debug)]
pub struct Stream {
    #[cfg(unix)]
    inner: std::os::unix::net::UnixStream,
    #[cfg(windows)]
    inner: windows_impl::Stream,
}

impl Stream {
    /// A second handle to the same connection (one thread reads, another writes).
    pub fn try_clone(&self) -> io::Result<Stream> {
        Ok(Stream {
            inner: self.inner.try_clone()?,
        })
    }

    /// Closes both directions (wakes a blocked reader on Unix).
    pub fn shutdown(&self) {
        #[cfg(unix)]
        let _ = self.inner.shutdown(std::net::Shutdown::Both);
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The daemon's listening endpoint.
#[derive(Debug)]
pub struct Listener {
    #[cfg(unix)]
    inner: std::os::unix::net::UnixListener,
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    inner: windows_impl::Listener,
}

impl Listener {
    /// Accepts one connection.
    pub fn accept(&self) -> io::Result<Stream> {
        #[cfg(unix)]
        {
            let (s, _) = self.inner.accept()?;
            Ok(Stream { inner: s })
        }
        #[cfg(windows)]
        {
            Ok(Stream {
                inner: self.inner.accept()?,
            })
        }
    }
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Connects to a daemon.
pub fn connect(ep: &Endpoint) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        Ok(Stream {
            inner: std::os::unix::net::UnixStream::connect(&ep.address)?,
        })
    }
    #[cfg(windows)]
    {
        Ok(Stream {
            inner: windows_impl::connect(&ep.address)?,
        })
    }
}

/// Binds the daemon's endpoint. The caller must hold the daemon lock, so a leftover socket
/// file belongs to a dead daemon and is removed.
pub fn bind(ep: &Endpoint) -> io::Result<Listener> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = PathBuf::from(&ep.address);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            if parent.starts_with("/tmp") {
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        let _ = fs::remove_file(&path);
        let inner = std::os::unix::net::UnixListener::bind(&path)?;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        Ok(Listener { inner, path })
    }
    #[cfg(windows)]
    {
        Ok(Listener {
            inner: windows_impl::bind(&ep.address)?,
        })
    }
}

#[cfg(windows)]
mod windows_impl {
    //! Named pipes through the `interprocess` crate (std has no named-pipe server).
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName, prelude::*};
    use std::io::{self, Read, Write};
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    pub struct Stream(Arc<interprocess::local_socket::Stream>);

    impl Stream {
        pub fn try_clone(&self) -> io::Result<Stream> {
            Ok(Stream(self.0.clone()))
        }
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            (&*self.0).read(buf)
        }
    }

    impl Write for Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            (&*self.0).write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            (&*self.0).flush()
        }
    }

    #[derive(Debug)]
    pub struct Listener(interprocess::local_socket::Listener);

    impl Listener {
        pub fn accept(&self) -> io::Result<Stream> {
            Ok(Stream(Arc::new(self.0.accept()?)))
        }
    }

    fn pipe_name(address: &str) -> &str {
        address.strip_prefix(r"\\.\pipe\").unwrap_or(address)
    }

    pub fn connect(address: &str) -> io::Result<Stream> {
        let name = pipe_name(address).to_ns_name::<GenericNamespaced>()?;
        Ok(Stream(Arc::new(
            interprocess::local_socket::Stream::connect(name)?,
        )))
    }

    pub fn bind(address: &str) -> io::Result<Listener> {
        let name = pipe_name(address).to_ns_name::<GenericNamespaced>()?;
        Ok(Listener(ListenerOptions::new().name(name).create_sync()?))
    }
}

// ---------------------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------------------

/// Connects to the workspace daemon, spawning `exe daemon --root <root>` if none is running.
pub fn ensure_daemon(ep: &Endpoint, exe: &Path) -> io::Result<Stream> {
    if let Ok(s) = connect(ep) {
        return Ok(s);
    }
    fs::create_dir_all(&ep.dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(ep.spawn_lock_path())?;
    lock.lock()?;
    // Someone may have finished spawning while we waited for the lock.
    if let Ok(s) = connect(ep) {
        return Ok(s);
    }
    let log = open_log(&ep.log_path())?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .arg("--root")
        .arg(&ep.root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    detach(&mut cmd);
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(s) = connect(ep) {
            // Reap the child later (it keeps running); dropping `Child` does not kill it.
            return Ok(s);
        }
        if let Some(status) = child.try_wait()? {
            // Another daemon may have won the race (it exits 0 when the lock is taken).
            if let Ok(s) = connect(ep) {
                return Ok(s);
            }
            return Err(io::Error::other(format!(
                "ts-r-lsp daemon exited ({status}); see {}",
                ep.log_path().display()
            )));
        }
        if Instant::now() > deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "ts-r-lsp daemon did not start; see {}",
                    ep.log_path().display()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Opens the daemon log for appending, truncating it first when it grew past 4 MiB.
pub fn open_log(path: &Path) -> io::Result<File> {
    if fs::metadata(path)
        .map(|m| m.len() > 4 * 1024 * 1024)
        .unwrap_or(false)
    {
        let _ = fs::remove_file(path);
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// Starts a child in its own process group so the editor's signals don't reach it.
pub fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

/// The running daemon's exclusive lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
}

/// Takes the daemon lock. `Ok(None)` if another daemon holds it.
pub fn take_daemon_lock(ep: &Endpoint) -> io::Result<Option<DaemonLock>> {
    fs::create_dir_all(&ep.dir)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(ep.pid_path())?;
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Ok(None),
        Err(fs::TryLockError::Error(e)) => return Err(e),
    }
    file.set_len(0)?;
    write!(file, "{}", std::process::id())?;
    Ok(Some(DaemonLock { _file: file }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = Endpoint::for_root_in(Path::new("/w/a"), Path::new("/c/lsp"));
        let a2 = Endpoint::for_root_in(Path::new("/w/a"), Path::new("/c/lsp"));
        let b = Endpoint::for_root_in(Path::new("/w/b"), Path::new("/c/lsp"));
        assert_eq!(a.id, a2.id);
        assert_ne!(a.id, b.id);
        assert_eq!(a.id.len(), 16);
        assert!(a.log_path().ends_with(format!("{}.log", a.id)));
    }

    #[cfg(unix)]
    #[test]
    fn long_cache_paths_fall_back_to_tmp() {
        let long = "/x".repeat(60);
        let ep = Endpoint::for_root_in(Path::new("/w"), Path::new(&long));
        assert!(ep.address.starts_with("/tmp/ts-r-lsp-"), "{}", ep.address);
        assert!(ep.address.len() < 100);
    }

    #[cfg(unix)]
    #[test]
    fn bind_connect_and_lock() {
        let dir = std::env::temp_dir().join(format!("tsr-ipc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ep = Endpoint::for_root_in(Path::new("/w/ipc"), &dir);
        let lock = take_daemon_lock(&ep).unwrap().expect("first lock");
        assert!(
            take_daemon_lock(&ep).unwrap().is_none(),
            "second lock must fail"
        );
        let listener = bind(&ep).unwrap();
        let t = std::thread::spawn(move || {
            let mut s = listener.accept().unwrap();
            let mut b = [0u8; 4];
            s.read_exact(&mut b).unwrap();
            s.write_all(&b).unwrap();
        });
        let mut c = connect(&ep).unwrap();
        c.write_all(b"ping").unwrap();
        let mut b = [0u8; 4];
        c.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"ping");
        t.join().unwrap();
        drop(lock);
        // A child forked by a parallel test may briefly share the lock's file description
        // (until its exec closes it), so allow a short retry.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while take_daemon_lock(&ep).unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "lock not released");
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
