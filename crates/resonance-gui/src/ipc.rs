//! Synchronous Unix-socket client for the Resonance daemon.
//!
//! Mirrors the TUI's `IpcClient`: one length-prefixed `postcard` request, one
//! response. A short read timeout keeps the GUI responsive if the daemon stalls.

use anyhow::{Result, anyhow};
use resonance_ipc::{
    Command, DaemonState, Response,
    transport::{read_response, write_command},
};
use std::{
    env,
    io::{BufReader, BufWriter, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

pub struct IpcClient {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

impl IpcClient {
    pub fn connect() -> Result<Self> {
        let path = socket_path();
        let stream =
            UnixStream::connect(&path).map_err(|e| anyhow!("connect {}: {e}", path.display()))?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        let writer = BufWriter::new(stream.try_clone()?);
        let reader = BufReader::new(stream);
        Ok(Self { reader, writer })
    }

    pub fn get_state(&mut self) -> Result<DaemonState> {
        match self.send_recv(Command::GetState)? {
            Response::State(s) => Ok(s),
            Response::Error(e) => Err(anyhow!("{e}")),
            _ => Err(anyhow!("unexpected response")),
        }
    }

    /// Fire-and-acknowledge: send a command and discard the OK-ish reply.
    pub fn send(&mut self, cmd: Command) -> Result<()> {
        match self.send_recv(cmd)? {
            Response::Error(e) => Err(anyhow!("{e}")),
            _ => Ok(()),
        }
    }

    pub fn send_recv(&mut self, cmd: Command) -> Result<Response> {
        write_command(&mut self.writer, &cmd)?;
        self.writer.flush()?;
        Ok(read_response(&mut self.reader)?)
    }
}

fn socket_path() -> PathBuf {
    if let Ok(p) = env::var(resonance_ipc::SOCKET_PATH_ENV) {
        return PathBuf::from(p);
    }
    let runtime = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join(resonance_ipc::DEFAULT_SOCKET_FILENAME)
}
