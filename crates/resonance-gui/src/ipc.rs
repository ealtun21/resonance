//! Synchronous IPC client for the Resonance daemon.
//!
//! Thin re-export of the shared `resonance_ipc::transport::SyncClient`: one
//! length-prefixed `postcard` request, one response, over a Unix socket (Unix)
//! or loopback TCP socket (Windows). The GUI talks to the daemon on a worker
//! thread with a short read+write timeout so a stalled daemon can't wedge it.

use resonance_ipc::transport::SyncClient;
use std::time::Duration;

/// Short timeout: a healthy GetState answers in well under a millisecond, so a
/// stalled/restarting daemon must not block the worker for long.
const TIMEOUT: Duration = Duration::from_millis(150);

pub type IpcClient = SyncClient;

/// Connect with the GUI's standard short timeout.
pub fn connect() -> Result<IpcClient, resonance_ipc::transport::TransportError> {
    SyncClient::connect_with_timeout(TIMEOUT)
}
