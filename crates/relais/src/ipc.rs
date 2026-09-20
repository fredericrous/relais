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

    /// Connections as they arrive. On Windows a connection that does not
    /// present the nonce within the request timeout is an `Err` item,
    /// which the accept loop skips like any failed accept.
    pub fn incoming(&self) -> impl Iterator<Item = io::Result<Stream>> + '_ {
        std::iter::from_fn(move || Some(self.inner.accept().map(|inner| Stream { inner })))
    }
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
            let listener = std::os::unix::net::UnixListener::bind(path)?;
            // Permission-restricted local socket (SPEC §23): the owner only.
            let mut permissions = std::fs::metadata(path)?.permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(path, permissions)?;
            Ok(Self(listener))
        }

        pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
            self.0.set_nonblocking(nonblocking)
        }

        pub fn accept(&self) -> io::Result<Stream> {
            self.0.accept().map(|(stream, _)| stream)
        }
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
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

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
        let mut stream = TcpStream::connect_timeout(&addr.into(), CONNECT_TIMEOUT)?;
        stream.write_all(nonce.as_bytes())?;
        stream.write_all(b"\n")?;
        Ok(stream)
    }

    /// Unpredictable bytes from the OS-seeded hasher state — the same
    /// entropy `HashMap` is randomised with. The nonce guards a loopback
    /// port whose endpoint file the user's ACL already protects; it is
    /// defence in depth, not the only wall.
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

    fn endpoint(tag: &str) -> std::path::PathBuf {
        let dir = if cfg!(unix) {
            // Unix socket paths are short (104 bytes on macOS): /tmp, not
            // the deep per-user temp dir.
            std::path::PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let dir = dir.join(format!("rl-ipc-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir.join("endpoint")
    }

    #[test]
    fn a_line_goes_there_and_a_line_comes_back() {
        let path = endpoint("rt");
        let listener = Listener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let stream = listener.incoming().next().expect("one").expect("accepted");
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
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_missing_endpoint_is_not_found_and_a_stale_one_is_refused() {
        let path = endpoint("stale");
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
        std::fs::remove_file(&path).ok();
    }

    #[cfg(windows)]
    #[test]
    fn a_connection_without_the_nonce_is_dropped_unread() {
        use std::net::TcpStream;
        let path = endpoint("nonce");
        let listener = Listener::bind(&path).expect("bind");
        let port: u16 = std::fs::read_to_string(&path)
            .expect("file")
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let server = std::thread::spawn(move || listener.incoming().next().expect("one"));
        let mut raw = TcpStream::connect(("127.0.0.1", port)).expect("tcp");
        let junk = [b'0'; 65];
        raw.write_all(&junk).expect("write");
        let accepted = server.join().expect("server");
        assert_eq!(
            accepted.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        std::fs::remove_file(&path).ok();
    }
}
