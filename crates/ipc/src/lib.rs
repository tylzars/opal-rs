//! The control channel between the service and `ctl`, the control panel and the tray: one command line in, one
//! reply line out. A Unix socket on Linux; on Windows a local TCP port, whose number and a secret token are kept in a
//! file only this user can read, and every connection starts with the token.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

#[cfg(unix)]
type Inner = std::os::unix::net::UnixStream;
#[cfg(not(unix))]
type Inner = std::net::TcpStream;

/// One connection.
pub struct Stream(Inner);

impl Stream {
    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(t)
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Read for &Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.0).read(buf)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Where the channel is: the socket, or on Windows the file with its port and token.
pub fn location() -> PathBuf {
    #[cfg(unix)]
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    #[cfg(not(unix))]
    let dir = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("opal-rs"));
    dir.unwrap_or_else(std::env::temp_dir).join("opal-rs.sock")
}

/// The service's end.
pub struct Listener {
    #[cfg(unix)]
    inner: std::os::unix::net::UnixListener,
    #[cfg(not(unix))]
    inner: std::net::TcpListener,
    #[cfg(not(unix))]
    token: String,
}

impl Listener {
    /// Starts listening, replacing a channel left behind by an earlier run.
    pub fn bind() -> io::Result<Self> {
        let path = location();
        let _ = std::fs::remove_file(&path);
        #[cfg(unix)]
        {
            let inner = std::os::unix::net::UnixListener::bind(&path)?;
            inner.set_nonblocking(true)?;
            Ok(Self { inner })
        }
        #[cfg(not(unix))]
        {
            let inner = std::net::TcpListener::bind(("127.0.0.1", 0))?;
            inner.set_nonblocking(true)?;
            let token = new_token();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, format!("{} {token}\n", inner.local_addr()?.port()))?;
            Ok(Self { inner, token })
        }
    }

    /// The next waiting connection, if any. A Windows connection that does not start with the token is dropped.
    pub fn accept(&self) -> io::Result<Option<Stream>> {
        let stream = match self.inner.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(e),
        };
        stream.set_nonblocking(false)?;
        #[cfg(not(unix))]
        {
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            // Byte by byte, so the command line after the token stays unread.
            let mut token = Vec::new();
            let mut byte = [0u8];
            while token.len() < 64 && (&stream).read(&mut byte)? == 1 && byte[0] != b'\n' {
                token.push(byte[0]);
            }
            if token != self.token.as_bytes() {
                return Ok(None);
            }
        }
        Ok(Some(Stream(stream)))
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(location());
    }
}

/// Connects to the running service.
pub fn connect() -> io::Result<Stream> {
    #[cfg(unix)]
    {
        Ok(Stream(std::os::unix::net::UnixStream::connect(location())?))
    }
    #[cfg(not(unix))]
    {
        let text = std::fs::read_to_string(location())?;
        let (port, token) = text.trim().split_once(' ').ok_or(io::ErrorKind::InvalidData)?;
        let port: u16 = port.parse().map_err(|_| io::ErrorKind::InvalidData)?;
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port))?;
        writeln!(s, "{token}")?;
        Ok(Stream(s))
    }
}

/// Sends one command and returns the reply line, waiting at most `timeout` for it.
pub fn request(command: &str, timeout: Duration) -> io::Result<String> {
    let mut s = connect()?;
    s.set_read_timeout(Some(timeout))?;
    writeln!(s, "{command}")?;
    let mut reply = String::new();
    BufReader::new(&s).read_line(&mut reply)?;
    Ok(reply.trim_end().to_owned())
}

/// 128 random bits as hex, from the standard library's randomly keyed hasher.
#[cfg(not(unix))]
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let half = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(std::process::id() as u64);
        h.finish()
    };
    format!("{:016x}{:016x}", half(), half())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_reply() {
        let dir = std::env::temp_dir().join(format!("opal-ipc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Only this test changes the environment.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
            std::env::set_var("LOCALAPPDATA", &dir);
        }
        assert!(location().ends_with("opal-rs.sock"));
        let listener = Listener::bind().unwrap();
        let server = std::thread::spawn(move || {
            let stream = loop {
                if let Some(s) = listener.accept().unwrap() {
                    break s;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let mut stream = stream;
            writeln!(stream, "got {}", line.trim()).unwrap();
        });
        assert_eq!(request("status", Duration::from_secs(5)).unwrap(), "got status");
        server.join().unwrap();
        assert!(!location().exists(), "channel removed when the listener goes");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
