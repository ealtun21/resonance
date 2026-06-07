//! Synchronous IPC client for the Resonance daemon.
//!
//! Mirrors the TUI's `IpcClient`: one length-prefixed `postcard` request, one
//! response. A short read timeout keeps the GUI responsive if the daemon stalls.
//! The underlying transport is a Unix socket on Unix and a loopback TCP socket
//! on Windows (see `resonance_ipc::transport`).

use anyhow::{Result, anyhow};
use resonance_ipc::{
    Command, DaemonState, Response,
    transport::{ClientStream, connect, read_response, write_command},
};
use std::{
    io::{BufReader, BufWriter, Write},
    time::Duration,
};

pub struct IpcClient {
    reader: BufReader<ClientStream>,
    writer: BufWriter<ClientStream>,
}

impl IpcClient {
    pub fn connect() -> Result<Self> {
        let stream = connect().map_err(|e| anyhow!("connect to daemon: {e}"))?;
        // Short timeouts: the GUI talks to the daemon on its UI thread, so a
        // stalled/restarting daemon must not freeze the window for long. A
        // healthy GetState answers in well under a millisecond.
        stream.set_read_timeout(Some(Duration::from_millis(150)))?;
        stream.set_write_timeout(Some(Duration::from_millis(150)))?;
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
