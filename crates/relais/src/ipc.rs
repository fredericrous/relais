//! The coordinator's local endpoint, one shape on every platform (SPEC
//! §23: "a permission-restricted local Unix socket on macOS/Linux and an
//! equivalent local IPC mechanism on other supported platforms").
//!
//! On Unix the endpoint IS a Unix socket at the path, mode 0600. On
//! Windows there is no Unix socket a Rust std listener can bind, so the
//! same path holds a small file instead: a loopback TCP port and a
//! 32-byte nonce. A client reads the file, connects to 127.0.0.1 on that
//! port and sends the nonce first; the listener drops any connection that
//! does not. The file lives under the user's profile, whose ACL is the
//! permission restriction — the same way gpg-agent's Assuan emulation
//! has done it on Windows for years. Everything above this module —
//! election, the stale-endpoint probe, `remove_file` on exit — sees one
//! path and one `Listener`/`Stream` pair and never asks which it got.

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

/// How long a client's connect to the coordinator endpoint may take. On
/// Windows this bounds the loopback TCP connect the emulation makes; on
/// Unix a connect to a local socket path does not block on the network,
/// so the constant does not gate anything there, but it is still the
/// number a caller should reserve a budget for talking to this endpoint
/// at all — `install::settings::derived_pretooluse_timeout` reads it from
/// here rather than assuming a value.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// A bound endpoint. Dropping it does not remove the path; the owner
/// removes the file on exit exactly as it would a Unix socket.
#[derive(Debug)]
pub struct Listener {
    inner: imp::Listener,
}

/// One connection, client or server side.
#[derive(Debug)]
pub struct Stream {
    inner: imp::Stream,
}

impl Listener {
    /// Bind at `path`, replacing nothing: a leftover endpoint must be
    /// probed and removed by the caller first (election owns that).
    pub fn bind(path: &Path) -> io::Result<Self> {
        imp::Listener::bind(path).map(|inner| Self { inner })
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.inner.set_nonblocking(nonblocking)
    }

    /// Accept one connection, or `Ok(None)` when a non-blocking listener
    /// has none waiting. The accepted stream is always blocking,
    /// whatever the listener is: whether a socket inherits the flag from
    /// the listener that accepted it differs between platforms, and one
    /// short request per connection is a blocking read either way.
    ///
    /// The peer is checked before a byte of the request is read: its uid
    /// on Unix, the endpoint nonce on Windows. A connection that fails
    /// either is `Err(PermissionDenied)` and nothing is served on it.
    pub fn accept(&self) -> io::Result<Option<Stream>> {
        match self.inner.accept() {
            Ok(inner) => Ok(Some(Stream { inner })),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Is this accept failure the process running out of file descriptors?
///
/// `EMFILE`/`ENFILE` do not pass: every later accept fails identically
/// until a descriptor frees, so an accept loop that retries at full
/// speed is a hot loop that starves the very handlers trying to close
/// theirs (A9). Everything else — a peer that went away, an interrupted
/// syscall — is transient and the next accept is unaffected.
///
/// The errno spellings belong to `procs`, the one module that names a
/// platform API (`boundaries.own-the-interface`).
pub fn out_of_descriptors(error: &io::Error) -> bool {
    crate::procs::out_of_descriptors(error)
}

/// Make a directory owner-only, and prove it. A 0600 endpoint inside a
/// world-writable directory is not permission-restricted: the path can
/// be replaced under it. On platforms with no POSIX mode this is the
/// user profile's ACL and there is nothing to set.
pub fn restrict_directory(path: &Path) -> io::Result<()> {
    imp::restrict_directory(path)
}

impl Stream {
    /// Connect to the endpoint at `path`. A missing endpoint is
    /// `NotFound`; a stale one that nothing answers is a connection error
    /// — both are what a Unix socket would report, so election's probe
    /// reads the same on every platform.
    pub fn connect(path: &Path) -> io::Result<Self> {
        imp::connect(path).map(|inner| Self { inner })
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_write_timeout(timeout)
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        self.inner.try_clone().map(|inner| Self { inner })
    }

    /// Half-close: this side sends nothing more, and the peer sees the
    /// end of the answer. Dropping the stream instead is a full close,
    /// and a full close with data still unread in the receive queue is a
    /// RESET on Windows — which throws away the reply already queued for
    /// the peer. A server that answers and hangs up (the over-long
    /// request path) needs this, and the drain that goes with it.
    pub fn shutdown_write(&self) -> io::Result<()> {
        self.inner.shutdown(std::net::Shutdown::Write)
    }

    /// The uid on the other end of this connection, as the kernel has
    /// it. `Listener::accept` has already refused anything but the
    /// coordinator's own uid; this is for reporting who that was.
    #[cfg(unix)]
    pub fn peer_uid(&self) -> io::Result<u32> {
        crate::procs::peer_uid(&self.inner)
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

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    pub type Stream = std::os::unix::net::UnixStream;

    #[derive(Debug)]
    pub struct Listener(std::os::unix::net::UnixListener);

    impl Listener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            // `bind` creates the socket inode with the process umask
            // applied, so a default 0022 umask made it 0755 — world
            // connectable — until the `set_permissions` below. A
            // protocol carrying `shutdown` and `cancel_run` was
            // reachable in that window (A8). Narrowing the umask around
            // the bind closes it: the socket is never created with more
            // than owner access, and the explicit mode afterwards is
            // what proves it on platforms where the umask does not
            // apply to sockets.
            // 0o077, not 0o177: the umask is process-wide, so anything
            // else this process creates during the window is affected
            // too, and clearing the owner's execute bit would make a
            // directory created in another thread unenterable. Clearing
            // group and other entirely is what the socket needs — its
            // base mode is 0666, so the inode is 0600 either way.
            let listener = {
                let _mask = crate::procs::narrow_umask(0o077);
                std::os::unix::net::UnixListener::bind(path)?
            };
            // Permission-restricted local socket (SPEC §23): the owner only.
            let mut permissions = std::fs::metadata(path)?.permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(path, permissions)?;
            let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
            if mode != 0o600 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("the endpoint is mode {mode:o}, not 0600"),
                ));
            }
            Ok(Self(listener))
        }

        pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
            self.0.set_nonblocking(nonblocking)
        }

        pub fn accept(&self) -> io::Result<Stream> {
            let (stream, _) = self.0.accept()?;
            // Whether an accepted socket inherits the listener's
            // non-blocking flag is platform-defined; the request read
            // that follows is blocking under a timeout either way.
            stream.set_nonblocking(false)?;
            // One coordinator per OS user (SPEC §23), so a connection
            // from another uid is never this coordinator's business —
            // and the protocol it would be speaking includes `shutdown`
            // and `cancel_run` (A8). Checked before a byte is read.
            check_peer(crate::procs::peer_uid(&stream)?, crate::procs::uid())?;
            Ok(stream)
        }
    }

    /// The uid that may talk to this coordinator is the one running it,
    /// and only that one — root included, which is somebody else's
    /// session with somebody else's runs.
    pub fn check_peer(peer: u32, me: u32) -> io::Result<()> {
        if peer == me {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("connection from uid {peer}; this coordinator serves uid {me} only"),
        ))
    }

    pub fn restrict_directory(path: &Path) -> io::Result<()> {
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)?;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is mode {mode:o}, not 0700", path.display()),
            ));
        }
        Ok(())
    }

    pub fn connect(path: &Path) -> io::Result<Stream> {
        Stream::connect(path)
    }
}

#[cfg(windows)]
mod imp {
    use std::io::{self, Read, Write};
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
    use std::path::Path;
    use std::time::Duration;

    pub type Stream = TcpStream;

    /// The nonce a client must present before its request is read. 32
    /// bytes, hex on the wire, followed by a newline.
    const NONCE_BYTES: usize = 32;
    const NONCE_LINE: usize = NONCE_BYTES * 2 + 1;
    /// How long a fresh connection has to present the nonce.
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

    #[derive(Debug)]
    pub struct Listener {
        listener: TcpListener,
        nonce: [u8; NONCE_LINE],
    }

    impl Listener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
            let port = listener.local_addr()?.port();
            let nonce = fresh_nonce();
            let mut line = [0u8; NONCE_LINE];
            line[..NONCE_BYTES * 2].copy_from_slice(hex(&nonce).as_bytes());
            line[NONCE_BYTES * 2] = b'\n';
            // `create_new`: an endpoint file that already exists is a
            // coordinator that may be alive; election probes and removes
            // it first, exactly as for a Unix socket path.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            write!(file, "{port}\n{}\n", hex(&nonce))?;
            Ok(Self {
                listener,
                nonce: line,
            })
        }

        pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
            self.listener.set_nonblocking(nonblocking)
        }

        /// Accept, then require the nonce. A wrong or missing nonce is a
        /// refused connection: the stream is dropped and the error says
        /// why, without a byte of the request being read.
        pub fn accept(&self) -> io::Result<Stream> {
            let (mut stream, peer) = self.listener.accept()?;
            // A non-blocking listener can hand back a non-blocking
            // socket; the nonce handshake below is a blocking read under
            // a timeout.
            stream.set_nonblocking(false)?;
            if !peer.ip().is_loopback() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "non-loopback peer on the coordinator port",
                ));
            }
            stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
            let mut presented = [0u8; NONCE_LINE];
            stream.read_exact(&mut presented)?;
            if !constant_time_eq(&presented, &self.nonce) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the connection did not present the endpoint nonce",
                ));
            }
            stream.set_read_timeout(None)?;
            Ok(stream)
        }
    }

    /// Client side: the endpoint file names the port and the nonce.
    pub fn connect(path: &Path) -> io::Result<Stream> {
        let text = std::fs::read_to_string(path)?;
        let mut lines = text.lines();
        let port: u16 = lines
            .next()
            .and_then(|line| line.trim().parse().ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "endpoint file has no port")
            })?;
        let nonce = lines.next().map(str::trim).unwrap_or_default();
        if nonce.len() != NONCE_BYTES * 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "endpoint file has no nonce",
            ));
        }
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let mut stream = TcpStream::connect_timeout(&addr.into(), super::CONNECT_TIMEOUT)?;
        stream.write_all(nonce.as_bytes())?;
        stream.write_all(b"\n")?;
        Ok(stream)
    }

    /// Windows has no POSIX mode: the endpoint file's restriction is the
    /// ACL its directory inherits from the user profile, which relais
    /// does not set and must not replace with a weaker one.
    pub fn restrict_directory(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    /// Unpredictable bytes from the OS-seeded hasher state — the same
    /// entropy `HashMap` is randomised with.
    ///
    /// Why not a CSPRNG: the standard library exposes none on stable,
    /// this crate is std-only by decision (SPEC §13), and `RandomState`
    /// is the one OS-seeded source std does expose. Its seed is taken
    /// from the platform's secure random source once per thread, and
    /// each `RandomState::new()` mixes a fresh key into a SipHash whose
    /// output is what is used here — so the 32 bytes carry that seed's
    /// entropy, not a counter's. The nonce is also not the only wall:
    /// it guards a loopback-only port whose endpoint file sits under the
    /// user's profile ACL, and it is compared in constant time.
    fn fresh_nonce() -> [u8; NONCE_BYTES] {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut out = [0u8; NONCE_BYTES];
        for (i, chunk) in out.chunks_mut(8).enumerate() {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(i as u64);
            hasher.write_u32(std::process::id());
            let word = hasher.finish().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        out
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn endpoint(tag: &str) -> (crate::test_support::TempDir, std::path::PathBuf) {
        let dir = crate::test_support::short_temp_dir(&format!("ipc-{tag}"));
        let path = dir.join("endpoint");
        (dir, path)
    }

    #[test]
    fn a_line_goes_there_and_a_line_comes_back() {
        let (_dir, path) = endpoint("rt");
        let listener = Listener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let stream = listener.accept().expect("accepted").expect("a connection");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("read");
            let mut writer = stream;
            writeln!(writer, "echo {}", line.trim()).expect("write");
        });
        let mut stream = Stream::connect(&path).expect("connect");
        writeln!(stream, "hello").expect("write");
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).expect("read");
        assert_eq!(line.trim(), "echo hello");
        server.join().expect("server");
    }

    #[test]
    fn a_missing_endpoint_is_not_found_and_a_stale_one_is_refused() {
        let (_dir, path) = endpoint("stale");
        assert_eq!(
            Stream::connect(&path).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let listener = Listener::bind(&path).expect("bind");
        drop(listener);
        // The file outlives the listener, exactly like a Unix socket path
        // after its owner died: connecting must fail, not hang.
        assert!(path.exists());
        assert!(Stream::connect(&path).is_err());
    }

    // A8: the socket carries `shutdown` and `cancel_run`, and `bind`
    // created it with the process umask applied — 0755 with a default
    // umask — until a `set_permissions` a syscall later. It is never
    // created wider than the owner now, and the mode is proved.
    #[cfg(unix)]
    #[test]
    fn the_endpoint_is_owner_only_from_the_moment_it_exists() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = endpoint("mode");
        let mode_of = |file: &std::path::Path| {
            std::fs::metadata(file)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };
        let listener = Listener::bind(&path).expect("bind");
        assert_eq!(mode_of(&path), 0o600, "permission-restricted (SPEC §23)");
        // The umask window is restored, so a second bind in the same
        // process is not relying on the first having left it narrowed.
        let again = path.with_extension("again");
        let second = Listener::bind(&again).expect("bind");
        assert_eq!(mode_of(&again), 0o600);
        drop(second);
        drop(listener);
    }

    // A8: one coordinator per OS user (SPEC §23), so the uid on the
    // other end is checked — by the kernel, not by the peer — before a
    // byte of the request is read.
    #[cfg(unix)]
    #[test]
    fn the_peer_of_a_connection_is_checked_against_our_own_uid() {
        let (_dir, path) = endpoint("peer");
        let listener = Listener::bind(&path).expect("bind");
        let connecting = std::thread::spawn({
            let path = path.clone();
            move || Stream::connect(&path).expect("connect")
        });
        let accepted = listener.accept().expect("accept").expect("a connection");
        assert!(accepted.peer_uid().expect("peer uid") == crate::procs::uid());
        drop(connecting.join().expect("client"));
        // And the rule itself: our own uid passes, anything else does
        // not — root included, which is somebody else's session.
        assert!(imp::check_peer(7, 7).is_ok());
        let refused = imp::check_peer(0, 501).expect_err("a foreign uid is refused");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(refused.to_string().contains("uid 0"), "{refused}");
        drop(listener);
    }

    // The errno spellings live with the syscall, in `procs`; the test
    // that pins them went with them.
    #[test]
    fn a_transient_accept_failure_is_not_descriptor_exhaustion() {
        assert!(!out_of_descriptors(&io::Error::from(
            io::ErrorKind::WouldBlock
        )));
    }

    #[test]
    fn a_non_blocking_listener_says_when_nothing_is_waiting() {
        let (_dir, path) = endpoint("nonblocking");
        let listener = Listener::bind(&path).expect("bind");
        listener.set_nonblocking(true).expect("non-blocking");
        assert!(
            listener.accept().expect("no error").is_none(),
            "nothing is connecting, and that is not a failure"
        );
        let mut client = Stream::connect(&path).expect("connect");
        let accepted = loop {
            if let Some(stream) = listener.accept().expect("accept") {
                break stream;
            }
        };
        // Accepted from a non-blocking listener and blocking anyway: the
        // request read that follows is one short line under a timeout.
        writeln!(client, "hello").expect("write");
        let mut line = String::new();
        BufReader::new(accepted).read_line(&mut line).expect("read");
        assert_eq!(line.trim(), "hello");
        drop(listener);
    }

    #[cfg(windows)]
    #[test]
    fn a_connection_without_the_nonce_is_dropped_unread() {
        use std::net::TcpStream;
        let (_dir, path) = endpoint("nonce");
        let listener = Listener::bind(&path).expect("bind");
        let port: u16 = std::fs::read_to_string(&path)
            .expect("file")
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let server = std::thread::spawn(move || listener.accept());
        let mut raw = TcpStream::connect(("127.0.0.1", port)).expect("tcp");
        let junk = [b'0'; 65];
        raw.write_all(&junk).expect("write");
        let accepted = server.join().expect("server");
        assert_eq!(
            accepted.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
