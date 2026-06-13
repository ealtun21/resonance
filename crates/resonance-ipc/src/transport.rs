use crate::{Command, DaemonState, Response};
use postcard::{from_bytes, to_stdvec};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("encode: {0}")]
    Encode(#[from] postcard::Error),
    #[error("{0}")]
    Daemon(String),
}

/// Upper bound on a single framed message. Guards both the blocking reader here
/// and the daemon's async reader against a hostile/garbled length prefix turning
/// into a multi-gigabyte allocation.
pub const MAX_MSG_LEN: u32 = 4 * 1024 * 1024;

/// Write a length-prefixed postcard message to a stream.
pub fn write_msg<W: Write, T: serde::Serialize>(
    writer: &mut W,
    msg: &T,
) -> Result<(), TransportError> {
    let bytes = to_stdvec(msg)?;
    // Enforce the same ceiling the reader does, before the `as u32` cast — so an
    // oversized frame fails here with a clear error instead of being written and
    // then rejected (or silently truncated past 4 GiB) on the far side.
    if bytes.len() > MAX_MSG_LEN as usize {
        return Err(TransportError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {} bytes", bytes.len()),
        )));
    }
    let len = bytes.len() as u32;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&bytes)?;
    Ok(())
}

/// Read a length-prefixed postcard message from a stream.
pub fn read_msg<R: Read, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> Result<T, TransportError> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_MSG_LEN {
        return Err(TransportError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {len} bytes"),
        )));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf)?;
    Ok(from_bytes(&buf)?)
}

pub fn write_command<W: Write>(writer: &mut W, cmd: &Command) -> Result<(), TransportError> {
    write_msg(writer, cmd)
}

pub fn read_command<R: Read>(reader: &mut R) -> Result<Command, TransportError> {
    read_msg(reader)
}

pub fn write_response<W: Write>(writer: &mut W, resp: &Response) -> Result<(), TransportError> {
    write_msg(writer, resp)
}

pub fn read_response<R: Read>(reader: &mut R) -> Result<Response, TransportError> {
    read_msg(reader)
}

// ── Cross-platform client transport ──────────────────────────────────────────
//
// Linux/macOS use a Unix domain socket (filesystem path, per-user perms).
// Windows has no usable AF_UNIX in std/tokio, so we use a loopback TCP socket
// on `127.0.0.1`; the daemon binds an ephemeral port and writes it to a port
// file (see `paths::port_file_path`) that clients read here. Both ends are
// blocking `Read + Write` streams, so the framing helpers above work unchanged.

/// Blocking client stream type: `UnixStream` on Unix, `TcpStream` on Windows.
#[cfg(unix)]
pub type ClientStream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub type ClientStream = std::net::TcpStream;

/// Connect to the running daemon. On Unix this dials the Unix socket; on Windows
/// it reads the daemon's port file and dials `127.0.0.1:<port>`.
pub fn connect() -> io::Result<ClientStream> {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(crate::paths::default_socket_path())
    }
    #[cfg(windows)]
    {
        let port = crate::paths::read_port_file().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "daemon port file not found — is resonanced running?",
            )
        })?;
        // Bounded connect so a stuck/restarting daemon can't hang the caller
        // (the GUI dials this on its UI thread).
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(250))
    }
}

/// Best-effort check whether the daemon is currently accepting connections.
pub fn is_reachable() -> bool {
    connect().is_ok()
}

/// Blocking request/response client shared by the CLI, TUI, and GUI: one
/// length-prefixed `postcard` command, one response, over the platform stream.
///
/// Centralizes the connect + buffered reader/writer + response classification
/// the three clients each used to open-code (and which had drifted — e.g. the
/// TUI set only a read timeout, never a write timeout).
pub struct SyncClient {
    reader: BufReader<ClientStream>,
    writer: BufWriter<ClientStream>,
}

impl SyncClient {
    fn from_stream(stream: ClientStream) -> Result<Self, TransportError> {
        let writer = BufWriter::new(stream.try_clone()?);
        let reader = BufReader::new(stream);
        Ok(Self { reader, writer })
    }

    /// Connect with no I/O timeout (blocking). Use for the CLI, where a command
    /// may legitimately wait on slower daemon work (preset import, AutoEq).
    pub fn connect() -> Result<Self, TransportError> {
        Self::from_stream(connect()?)
    }

    /// Connect with a read+write timeout, so a stalled daemon can't wedge an
    /// interactive client. Both directions are bounded (the TUI previously
    /// bounded only reads).
    pub fn connect_with_timeout(timeout: Duration) -> Result<Self, TransportError> {
        let stream = connect()?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Self::from_stream(stream)
    }

    /// Send a command and return the raw response.
    pub fn send_recv(&mut self, cmd: Command) -> Result<Response, TransportError> {
        write_command(&mut self.writer, &cmd)?;
        self.writer.flush()?;
        read_response(&mut self.reader)
    }

    /// Fetch the daemon state snapshot.
    pub fn get_state(&mut self) -> Result<DaemonState, TransportError> {
        match self.send_recv(Command::GetState)? {
            Response::State(s) => Ok(s),
            Response::Error(e) => Err(TransportError::Daemon(e)),
            _ => Err(TransportError::Daemon("unexpected response".into())),
        }
    }

    /// Send a command and treat only `Response::Error` as failure; any other
    /// reply (Ok / State / lists) counts as success.
    pub fn send(&mut self, cmd: Command) -> Result<(), TransportError> {
        match self.send_recv(cmd)? {
            Response::Error(e) => Err(TransportError::Daemon(e)),
            _ => Ok(()),
        }
    }
}
