//! Synchronous IPC client for the Resonance daemon.
//!
//! Thin re-export of the shared `resonance_ipc::transport::SyncClient`: one
//! length-prefixed `postcard` request, one response, over a Unix socket (Unix)
//! or loopback TCP socket (Windows). The GUI talks to the daemon on a worker
//! thread with a short read+write timeout so a stalled daemon can't wedge it.

use resonance_ipc::transport::SyncClient;
use std::time::Duration;

/// Read/write timeout for the worker's socket. A healthy GetState answers in
/// well under a millisecond; this only bounds how long the worker waits on a
/// stalled daemon before tearing the connection down. It runs off the UI
/// thread, so a generous value never freezes the window — and being generous
/// stops a brief daemon stall (e.g. a CoreAudio device switch holding the state
/// lock) from tripping a needless reconnect. The 150 ms it used to be was tight
/// enough to flap on macOS under load.
const TIMEOUT: Duration = Duration::from_millis(1000);

pub type IpcClient = SyncClient;

/// Connect with the GUI's standard short timeout.
pub fn connect() -> Result<IpcClient, resonance_ipc::transport::TransportError> {
    SyncClient::connect_with_timeout(TIMEOUT)
}
