//! Control channel to the service: one command line in, one reply out (Unix socket, or local TCP with a token).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

pub mod autostart;

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

    pub fn set_write_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(t)
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

/// What the service is waiting for between camera sessions, as `status` reports it (`waiting <code>`): a title and
/// a line of help, for the control panel and the tray.
pub fn waiting_text(code: &str) -> (&'static str, &'static str) {
    match code {
        "camera" => ("Camera not found", "Plug in the Opal C1."),
        "usb2" => (
            "The camera is connected over USB 2",
            "Unplug it and plug it back in. If this keeps happening, use a USB 3 port.",
        ),
        "replug" => ("The camera stopped responding", "Unplug it and plug it back in."),
        "windows10" => (
            "Windows 11 is needed",
            "Opal C1 still works as a plain webcam: in applications, choose \"Opal C1\".",
        ),
        "output" => (
            "Opal C1 Virtual is not set up yet",
            "Restart the computer to finish setting it up.",
        ),
        _ => ("Starting the camera", ""),
    }
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
        if stream.set_nonblocking(false).is_err() {
            return Ok(None);
        }
        // A connection that fails here is the client's problem, not the listener's.
        #[cfg(not(unix))]
        {
            let token = || -> io::Result<Vec<u8>> {
                stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                // Byte by byte, so the command line after the token stays unread.
                let mut token = Vec::new();
                let mut byte = [0u8];
                while token.len() < 64 && (&stream).read(&mut byte)? == 1 && byte[0] != b'\n' {
                    token.push(byte[0]);
                }
                Ok(token)
            };
            if !token().is_ok_and(|t| t == self.token.as_bytes()) {
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

/// Largest binary reply taken, well above a 4K frame.
const MAX_DATA: usize = 64 << 20;

/// Sends one command whose reply is binary: a line `bytes <n> <words...>` and then n bytes. Returns the words and
/// the bytes; any other reply line comes back as an error.
pub fn request_data(command: &str, timeout: Duration) -> io::Result<(String, Vec<u8>)> {
    let mut s = connect()?;
    s.set_read_timeout(Some(timeout))?;
    writeln!(s, "{command}")?;
    let mut reader = BufReader::new(&s);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let rest = line
        .trim_end()
        .strip_prefix("bytes ")
        .ok_or_else(|| io::Error::other(line.trim_end().to_owned()))?;
    let (n, words) = rest.split_once(' ').unwrap_or((rest, ""));
    let n: usize = n
        .parse()
        .ok()
        .filter(|&n| n <= MAX_DATA)
        .ok_or(io::ErrorKind::InvalidData)?;
    let mut data = vec![0u8; n];
    reader.read_exact(&mut data)?;
    Ok((words.to_owned(), data))
}

/// A command's words after the first `skip`, as typed, so a file path with spaces stays whole.
pub fn tail(line: &str, skip: usize) -> &str {
    let mut s = line.trim();
    for _ in 0..skip {
        s = s.find(char::is_whitespace).map_or("", |i| &s[i..]).trim_start();
    }
    s
}

/// A look or file followed by an optional amount, as `lut` takes them.
pub fn split_amount(args: &str) -> (&str, Option<f32>) {
    match args.rsplit_once(char::is_whitespace) {
        Some((rest, a)) if a.parse::<f32>().is_ok() => (rest.trim_end(), a.parse().ok()),
        _ => (args, None),
    }
}

/// Process creation flag for a child without a console window.
#[cfg(windows)]
pub const NO_WINDOW: u32 = 0x0800_0000;

/// One of the opal-rs programs: next to the running one if it is there, else found on the PATH.
pub fn program(name: &str) -> PathBuf {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(&file)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| file.into())
}

/// Starts the service if it is not running: through systemd on Linux; on Windows without a window, logging to
/// `%LOCALAPPDATA%\opal-rs\service.log`.
pub fn start_service() -> io::Result<()> {
    start_service_with(&program("opal-rs"))
}

/// [`start_service`], with the service program given (on Windows; Linux always starts it through systemd).
pub fn start_service_with(exe: &std::path::Path) -> io::Result<()> {
    if request("status", Duration::from_secs(2)).is_ok() {
        return Ok(());
    }
    #[cfg(unix)]
    let _ = exe;
    #[cfg(unix)]
    {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "start", "opal-rs.service"])
            .status()?;
        if !status.success() {
            return Err(io::Error::other("systemctl --user start opal-rs.service failed"));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let dir = std::env::var_os("LOCALAPPDATA")
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("opal-rs");
        std::fs::create_dir_all(&dir)?;
        // The last run's log is kept, for when something went wrong in it.
        let _ = std::fs::rename(dir.join("service.log"), dir.join("service.old.log"));
        let log = std::fs::File::create(dir.join("service.log"))?;
        let mut c = std::process::Command::new(exe);
        c.arg("run")
            .creation_flags(NO_WINDOW)
            .stdout(log.try_clone()?)
            .stderr(log);
        c.spawn().map(drop)
    }
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
    fn words() {
        assert_eq!(
            tail("background  C:\\My Pictures\\a b.jpg ", 1),
            "C:\\My Pictures\\a b.jpg"
        );
        assert_eq!(tail("lut", 1), "");
        assert_eq!(
            split_amount("my looks/film.cube 0.80"),
            ("my looks/film.cube", Some(0.8))
        );
        assert_eq!(split_amount("noir"), ("noir", None));
    }

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
