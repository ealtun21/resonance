# Windows Lifecycle Bug Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three Windows bugs: (1) EQ keeps running in audiodg after the daemon quits/dies — add graceful APO bypass on shutdown plus a heartbeat so the APO auto-bypasses when the daemon vanishes; (2) device-mapped profile is never auto-applied on Windows daemon start — feed the existing output-mapping task from the Windows device-watch thread; (3) a PowerShell/console window flashes at startup and quit — missing `CREATE_NO_WINDOW` on two spawns.

**Architecture:** The Windows DSP lives in a pure-Rust APO inside audiodg, fed by a seqlock'd shared file (`resonance-apo/src/state.rs`); the daemon is control-plane only. Root causes (investigated, evidence in git history of this plan): the APO only reacts to `generation` *changes* and nothing ever publishes `enabled=0` at teardown; `output_tx` (which drives `spawn_output_mapping_task`) is dropped on Windows; two console-subsystem children are spawned without creation flags. Fixes are layered: bypass-on-graceful-shutdown (daemon) + staleness heartbeat (APO) for crash safety, one channel-send for the mapping, two one-line flag fixes.

**Tech Stack:** Rust workspace; `resonance-apo` (cross-platform lib, windows-only consumer), `resonance-daemon`, `resonance-ipc`, `resonance-tray`.

## Global Constraints

- Run `make check` (fmt --check + clippy `-D warnings` pedantic + test --all) before **every** commit.
- Conventional Commits, all lowercase. **No AI-related content anywhere** (no Co-Authored-By trailers, no AI mentions in code/comments/commits).
- Workspace clippy pedantic; `float_cmp` active; cast lints blanket-allowed.
- Windows-gated code must also pass cross-target clippy: `cargo clippy --target x86_64-pc-windows-msvc -p resonance-apo -p resonance-ipc -p resonance-daemon -p resonance-tray -- -D warnings` (if the target is missing: `rustup target add x86_64-pc-windows-msvc`).
- `resonance-apo` state module is cross-platform and unit-tested on Linux; keep it that way (no `windows-sys` in resonance-apo).
- **`STATE_VERSION` bumps 9 → 10 in Task 1.** A v10 daemon + v9 APO dll (or vice versa) read nothing from each other → APO passes through / keeps last chain. Live verification (Task 8) MUST deploy daemon and APO dll together and restart the Windows audio service.
- Behavior contract (user-decided): tray "Quit Resonance" (when `quit_stops_daemon=true`, the default) closes GUI + daemon + tray and audio goes UNPROCESSED; `quit_stops_daemon=false` exits only the tray. EQ must never outlive the daemon by more than ~3 s even on `taskkill /f` or a crash.

## Reference: verified facts (from source, master)

- `SharedState` header (apo/state.rs:246-260): `magic, version, generation: AtomicU64 (seqlock: odd=writing), snapshot: ChainSnapshot, telemetry_enabled: AtomicU32, _pad2, telemetry`. `STATE_VERSION = 9` (state.rs:~34). `ChainSnapshot.enabled: u32` is field 1; `Default` sets `enabled: 1`.
- Writer = `SharedFile` (alias `ApoStateWriter`), `publish(&mut self, chain: &ProcessorChain)` at state.rs:778-787 does the odd/even generation dance around `st.snapshot = snap`.
- `read_chain_fresh(path) -> Option<(u64, ChainSnapshot, bool)>` (state.rs:940-973) re-reads the FILE each call (mapped views don't observe cross-session writes on Windows).
- APO worker loop polls every 25 ms (ffi.rs:204-211), rebuilds chain only on generation change (ffi.rs:241-289). Bypass mechanism: `ProcessorChain::process` returns early when `!self.enabled` (resonance-dsp/src/chain.rs:143-144); `ChainSnapshot::apply_to`/`build_chain` set `chain.enabled = self.enabled != 0` (state.rs:386, 424).
- Daemon: `SharedState::send` publishes via `inner.apo_writer` (daemon/state.rs:283-304); `set_apo_writer` (windows-gated, state.rs:307-315); `apo_writer: Option<ApoStateWriter>` field is **unconditional** (state.rs:185). `pump_telemetry` (windows-gated, state.rs:238-281) runs on a 30 ms tokio interval (daemon/main.rs:352-358) and already locks `inner` + touches `apo_writer`.
- `Command::Shutdown` exists (resonance-ipc/src/lib.rs:258-259); handler in ipc_server.rs:89-103 flushes the reply then `cleanup(); exit(0)` — and it holds `&state`.
- Signal handlers (daemon/shutdown.rs:163-199) call `cleanup(); exit(0)`; they do NOT capture `SharedState` today.
- `service::stop()` on Windows (ipc/service/windows.rs:127-137) = `hidden("taskkill") /im resonanced.exe /t /f` only. `restart()` = stop → poll `is_active()` (33×15 ms) → start. `hidden()` helper at windows.rs:32-38.
- Tray: `execute` (tray/daemon.rs:52-108); `MenuAction::Quit` arm currently `if quit_stops_daemon { service::stop() } exit(0)`. `client() -> Option<SyncClient>` with 400 ms timeout (daemon.rs:14-16); `SyncClient::send(cmd)` exists. `quit_ui()` = `singleton::stop(GUI_INSTANCE)` (tray/control.rs:94-96).
- Flash sites: `daemon/audio/win_devices.rs:243` (`Command::new("powershell.exe")…output()`, NO flags — the startup flash) and `ipc/singleton.rs:142-161` windows branch (`taskkill /PID … /T /F`, NO flags — flash on quit-UI).
- Mapping: `spawn_output_mapping_task(output_rx, &shared)` is spawned cross-platform (daemon/main.rs:454); on Windows `output_tx` is dropped in the `let _ = (…)` tuple (main.rs:478-490); device-watch thread (main.rs:363-391, inside `init_windows_control_plane`) polls every 2 s, computes `default = win_devices::default_render_id()` and writes `inner.active_output` but never sends to `output_tx`. Mapping keys are the same `default_render_id()` strings, so no key mismatch.
- APO test style: `#[cfg(test)] mod tests` at state.rs:1011 with `temp_path("…")` helper; e.g. `snapshot_round_trips_through_file` builds a chain, `ApoStateWriter::create` + `publish`, `SharedFile::open` + `read`.

---

### Task 1: APO shared-state heartbeat + bypass publish (cross-platform, TDD)

**Files:**
- Modify: `crates/resonance-apo/src/state.rs`

**Interfaces:**
- Consumes: existing `SharedFile`/`ApoStateWriter`, `read_u32`/`read_u64` helpers, `temp_path` test helper.
- Produces (used by Tasks 2, 3):
  - `SharedState.heartbeat: AtomicU64` (new last field of the header)
  - `pub fn SharedFile::beat(&mut self)`
  - `pub fn SharedFile::publish_bypass(&mut self)`
  - `pub fn read_heartbeat_fresh(path: &Path) -> Option<u64>`
  - `STATE_VERSION = 10`

- [ ] **Step 1: Write the failing tests**

Append inside the existing `#[cfg(test)] mod tests` in `crates/resonance-apo/src/state.rs` (reuse the module's existing imports and `temp_path`):

```rust
    #[test]
    fn heartbeat_beats_and_reads_fresh() {
        let path = temp_path("hb");
        let mut w = ApoStateWriter::create(&path).unwrap();
        let h0 = read_heartbeat_fresh(&path).unwrap();
        w.beat();
        w.beat();
        assert_eq!(read_heartbeat_fresh(&path).unwrap(), h0 + 2);
    }

    #[test]
    fn publish_bypass_zeroes_enabled_and_advances_generation() {
        let mut chain = ProcessorChain::builder()
            .channels(2)
            .sample_rate(48000.0)
            .preamp_db(-3.5)
            .build();
        chain.enabled = true;
        let path = temp_path("byp");
        let mut w = ApoStateWriter::create(&path).unwrap();
        w.publish(&chain);
        let (g1, s1, _) = read_chain_fresh(&path).unwrap();
        assert_eq!(s1.enabled, 1);

        w.publish_bypass();
        let (g2, s2, _) = read_chain_fresh(&path).unwrap();
        assert_eq!(s2.enabled, 0, "bypass must publish enabled=0");
        assert!(g2 > g1, "generation must advance so the worker notices");
        assert!((s2.preamp_db - (-3.5)).abs() < 1e-12, "other params preserved");
    }
```

(If the test module's chain-builder usage differs slightly — e.g. `preamp_db` set another way in `snapshot_round_trips_through_file` — mirror that test's construction exactly.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p resonance-apo heartbeat_beats_and_reads_fresh publish_bypass`
Expected: compile error — `heartbeat`/`beat`/`publish_bypass`/`read_heartbeat_fresh` not found.

- [ ] **Step 3: Implement**

(a) Header — add as the LAST field of `SharedState` (state.rs:246-260) so all existing offsets stay put:

```rust
    /// Daemon liveness stamp: a counter the daemon bumps ~every 30 ms while it
    /// runs. The APO worker bypasses the chain when it stops advancing (daemon
    /// quit, killed, or crashed) so EQ never outlives its control plane.
    pub heartbeat: AtomicU64,
```

Also update the zero-init/create path the same way the other atomic header fields are initialized (heartbeat starts at 0), and preserve it across re-opens exactly like `generation` (follow whatever `SharedFile::create`/`open` already do for `generation` — the existing re-open test asserts generation is preserved; heartbeat needs no special casing if the header memory is preserved wholesale).

(b) Version bump — `STATE_VERSION` 9 → 10, and extend its doc comment history list:

```rust
/// v10: + daemon liveness heartbeat (APO auto-bypass when stale).
pub const STATE_VERSION: u32 = 10;
```

(c) Writer methods on `impl SharedFile`, next to `publish` (state.rs:778):

```rust
    /// Bump the daemon-liveness stamp (called ~every 30 ms by the daemon's
    /// telemetry pump). Not a seqlock write: a torn read of a monotonically
    /// increasing u64 still reads as "changed", which is all the APO needs.
    pub fn beat(&mut self) {
        let st = self.state_mut();
        let h = st.heartbeat.load(Ordering::Relaxed);
        st.heartbeat.store(h.wrapping_add(1), Ordering::Release);
    }

    /// Publish a bypass: keep the last chain parameters but force
    /// `enabled = 0`, so the APO passes audio through untouched. Called on
    /// graceful daemon shutdown — EQ must not outlive the control plane.
    pub fn publish_bypass(&mut self) {
        let st = self.state_mut();
        let g = st.generation.load(Ordering::Relaxed);
        st.generation.store(g.wrapping_add(1), Ordering::Release); // odd: writing
        st.snapshot.enabled = 0;
        st.generation.store(g.wrapping_add(2), Ordering::Release); // even: done
    }
```

(d) Fresh reader, next to `read_chain_fresh` (state.rs:940):

```rust
/// Read the daemon-liveness heartbeat with a fresh file read (a long-lived
/// mapped view does not observe the daemon's writes across sessions on
/// Windows — see `read_chain_fresh`). `None` = file missing/invalid, which
/// callers must treat as "daemon gone".
#[must_use]
pub fn read_heartbeat_fresh(path: &Path) -> Option<u64> {
    let b = std::fs::read(path).ok()?;
    if b.len() < STATE_SIZE
        || read_u32(&b, 0)? != STATE_MAGIC
        || read_u32(&b, std::mem::offset_of!(SharedState, version))? != STATE_VERSION
    {
        return None;
    }
    read_u64(&b, std::mem::offset_of!(SharedState, heartbeat))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p resonance-apo`
Expected: new tests PASS and every pre-existing state test still passes (the version bump is absorbed by tests using the constants; if any test hardcodes `9`, update it to use `STATE_VERSION`).

- [ ] **Step 5: `make check`, commit**

```bash
make check
git add crates/resonance-apo/src/state.rs
git commit -m "feat(apo): liveness heartbeat and bypass publish in shared state"
```

---

### Task 2: APO worker auto-bypass on stale heartbeat (TDD on the pure logic)

**Files:**
- Modify: `crates/resonance-apo/src/ffi.rs`

**Interfaces:**
- Consumes: `read_heartbeat_fresh` (Task 1); worker loop at ffi.rs:204-289; `Shared.state: Mutex<Option<Locked>>`; `Locked.chain: ProcessorChain`.
- Produces: `HeartbeatWatch` (module-private) with `fn observe(&mut self, hb: Option<u64>, now: Instant) -> bool`; worker forces `l.chain.enabled = false` when stale.

- [ ] **Step 1: Write the failing tests**

Append to the existing `#[cfg(test)]` module in `crates/resonance-apo/src/ffi.rs`:

```rust
    #[test]
    fn heartbeat_watch_goes_stale_only_after_silence() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let mut w = HeartbeatWatch::new();
        assert!(!w.observe(Some(1), t0), "first sight starts the grace window");
        assert!(
            !w.observe(Some(2), t0 + Duration::from_secs(10)),
            "advancing heartbeat never goes stale"
        );
        assert!(
            !w.observe(Some(2), t0 + Duration::from_secs(11)),
            "1 s of silence is not yet stale"
        );
        assert!(
            w.observe(Some(2), t0 + Duration::from_secs(13)),
            "silent past STALE_AFTER -> stale"
        );
        assert!(
            !w.observe(Some(3), t0 + Duration::from_secs(14)),
            "resumed heartbeat recovers"
        );
    }

    #[test]
    fn heartbeat_watch_treats_unreadable_as_silence() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let mut w = HeartbeatWatch::new();
        assert!(!w.observe(None, t0), "grace window on first sight");
        assert!(w.observe(None, t0 + Duration::from_secs(3)));
        assert!(!w.observe(Some(1), t0 + Duration::from_secs(4)), "file back -> recovers");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p resonance-apo heartbeat_watch`
Expected: compile error — `HeartbeatWatch` not found.

- [ ] **Step 3: Implement the watch + worker integration**

(a) Module-level in ffi.rs (near the other worker constants, e.g. `STARVE_TICKS` at ffi.rs:169):

```rust
/// How long the daemon heartbeat may sit unchanged before the worker forces
/// a bypass. The daemon beats every ~30 ms; 2 s of silence means it is gone
/// (quit, killed, or crashed) — taskkill /f skips every shutdown hook, so
/// this staleness check is the only crash-safe teardown signal.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

/// Tracks the daemon heartbeat and decides staleness. Pure logic (fed a
/// timestamp) so the bypass rule is unit-testable without a worker thread.
struct HeartbeatWatch {
    last_seen: Option<u64>,
    changed_at: Option<std::time::Instant>,
}

impl HeartbeatWatch {
    fn new() -> Self {
        Self { last_seen: None, changed_at: None }
    }

    /// Feed the latest heartbeat reading (`None` = state file unreadable).
    /// Returns true once the value has not advanced for `STALE_AFTER`.
    fn observe(&mut self, hb: Option<u64>, now: std::time::Instant) -> bool {
        if self.changed_at.is_none() || hb != self.last_seen {
            self.last_seen = hb;
            self.changed_at = Some(now);
            return false;
        }
        self.changed_at.is_some_and(|t| now.duration_since(t) >= STALE_AFTER)
    }
}
```

(b) Worker loop integration — in the 25 ms loop (ffi.rs:204-289): declare `let mut watch = HeartbeatWatch::new();` before the loop, and after the existing `read_chain_fresh`/generation-change block add:

```rust
            // Daemon liveness: when the heartbeat stops advancing, force a
            // bypass so EQ never outlives its control plane. The daemon's
            // next publish (a generation change) rebuilds the chain and
            // restores normal processing.
            let hb = crate::state::read_heartbeat_fresh(&path);
            if watch.observe(hb, std::time::Instant::now()) {
                if let Ok(mut g) = shared.state.try_lock() {
                    if let Some(l) = g.as_mut() {
                        if l.chain.enabled {
                            l.chain.enabled = false;
                        }
                    }
                }
            }
```

Adapt the variable names (`path`, `shared`) to the loop's actual locals. If the file has a diagnostics/logging helper already used inside the worker (there is a `l.chain.enabled` diagnostic around ffi.rs:522), emit one line on the enabled→bypassed transition using that same mechanism; if none is reachable from the worker, add no new logging.

Recovery needs no extra code: a restarting daemon calls `set_apo_writer` → `publish` → generation advances → the worker's existing rebuild path sets `chain.enabled` from the fresh snapshot.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p resonance-apo`
Expected: both watch tests PASS; existing ffi tests (which never beat the heartbeat) still pass — verify none of them processes audio across a >2 s gap relying on `enabled=true`; if one does, have its setup call `w.beat()` right before processing (state the change in the commit).

- [ ] **Step 5: `make check`, commit**

```bash
make check
git add crates/resonance-apo/src/ffi.rs
git commit -m "feat(apo): auto-bypass dsp when daemon heartbeat goes stale"
```

---

### Task 3: Daemon — beat the heartbeat, publish bypass on every graceful shutdown

**Files:**
- Modify: `crates/resonance-daemon/src/state.rs` (pump_telemetry beat + new method)
- Modify: `crates/resonance-daemon/src/ipc_server.rs:89-103` (Shutdown branch)
- Modify: `crates/resonance-daemon/src/shutdown.rs:163-199` (signal handlers take state)
- Modify: `crates/resonance-daemon/src/main.rs` (updated `install_signal_handlers` call)

**Interfaces:**
- Consumes: `SharedFile::beat`/`publish_bypass` (Task 1); `pump_telemetry` (state.rs:238-281); `install_signal_handlers` call site in main.
- Produces: `pub fn SharedState::publish_apo_bypass(&self)` (unconditional — compiles on all platforms; no-op when `apo_writer` is `None`); `install_signal_handlers(shared: &SharedState)`.

- [ ] **Step 1: Add `publish_apo_bypass` to `SharedState` (daemon/state.rs, near `send`)**

```rust
    /// Force the APO into passthrough before the daemon exits. No-op when the
    /// APO bridge is absent (non-Windows, or bridge init failed): EQ must not
    /// outlive the control plane.
    pub fn publish_apo_bypass(&self) {
        let mut guard = self.0.lock().unwrap();
        if let Some(w) = guard.apo_writer.as_mut() {
            w.publish_bypass();
        }
    }
```

- [ ] **Step 2: Beat from the telemetry pump**

In `pump_telemetry` (state.rs:238-281, `#[cfg(target_os = "windows")]`), at the point where `inner.apo_writer` is first touched, bump the heartbeat every tick:

```rust
        if let Some(w) = inner.apo_writer.as_mut() {
            w.beat();
        }
```

(The pump runs on a 30 ms interval — daemon/main.rs:352-358 — regardless of connected clients, which is exactly the liveness signal Task 2 consumes. If the existing code uses `as_ref()` for the writer, switch that access to `as_mut()` as needed.)

- [ ] **Step 3: Publish bypass in the IPC Shutdown branch**

`ipc_server.rs:89-103` — before cleanup/exit:

```rust
    if is_shutdown {
        use tokio::io::AsyncWriteExt;
        let _ = writer.flush().await;
        info!("shutdown requested");
        state.publish_apo_bypass();
        crate::shutdown::cleanup();
        std::process::exit(0);
    }
```

(Adapt the state variable's name to the handler's actual binding.)

- [ ] **Step 4: Signal handlers publish bypass too**

Change `shutdown.rs` `install_signal_handlers()` to `install_signal_handlers(shared: &crate::state::SharedState)`; clone it into both spawned tasks and call `shared.publish_apo_bypass();` immediately before each `cleanup(); std::process::exit(0);` (both the unix SIGTERM/SIGINT task and the windows Ctrl-C task). Update the call site in `main.rs` to pass `&shared` (move the call after `shared` exists if necessary).

- [ ] **Step 5: Verify + commit**

Run: `cargo test -p resonanced` (or the daemon package name used by `cargo test --all`) and `cargo clippy --target x86_64-pc-windows-msvc -p resonance-daemon -- -D warnings`.
Expected: all pass. There is no practical unit test for a branch that ends in `process::exit` — the live check is Task 8's VM run.

```bash
make check
git add crates/resonance-daemon
git commit -m "feat(daemon): apo heartbeat and bypass publish on graceful shutdown"
```

---

### Task 4: Windows — feed the output-mapping task from the device-watch thread

**Files:**
- Modify: `crates/resonance-daemon/src/main.rs` (windows block ~478-500; `init_windows_control_plane` ~340-392)

**Interfaces:**
- Consumes: existing `spawn_output_mapping_task(output_rx, &shared)` (main.rs:454, cross-platform); `output_tx: tokio::sync::mpsc::UnboundedSender<String>`; `win_devices::default_render_id()`.
- Produces: mapped-profile auto-load now fires on Windows at daemon start and on default-device change.

- [ ] **Step 1: Stop dropping `output_tx`; pass it into the control plane**

In the `#[cfg(target_os = "windows")]` block in `main` (main.rs:478-500): remove `output_tx` from the `let _ = ( … )` discard tuple and change the call to:

```rust
        init_windows_control_plane(&shared, output_tx);
```

- [ ] **Step 2: Send default-device changes from the device-watch thread**

Change the signature:

```rust
#[cfg(target_os = "windows")]
fn init_windows_control_plane(
    shared: &state::SharedState,
    output_tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
```

Move `output_tx` into the device-watch thread closure and, inside its 2 s loop where `default` is computed (main.rs:~369), add change-detection BEFORE the `inner` lock block:

```rust
            // Feed the (cross-platform) output-mapping task: first sight at
            // startup counts as a change, which is what auto-applies the
            // device's mapped profile after a daemon restart.
            if default.is_some() && default != last_default {
                last_default.clone_from(&default);
                if let Some(id) = default.clone() {
                    let _ = output_tx.send(id);
                }
            }
```

with `let mut last_default: Option<String> = None;` declared before the loop. Keep the existing `inner.active_output = default;` write — the mapping task also sets it on event delivery; both writes agree.

- [ ] **Step 3: Verify + commit**

Run: `cargo clippy --target x86_64-pc-windows-msvc -p resonance-daemon -- -D warnings` and `make check` (linux build unaffected — all edits are inside `cfg(windows)`).
Expected: clean. Live proof is Task 8 (restart daemon → `resonance status` shows the mapped profile without manual selection).

```bash
git add crates/resonance-daemon/src/main.rs
git commit -m "fix(daemon): auto-apply device-mapped profile on windows startup"
```

---

### Task 5: Kill the console-window flashes (two spawns)

**Files:**
- Modify: `crates/resonance-daemon/src/audio/win_devices.rs:243` (powershell — the startup flash)
- Modify: `crates/resonance-ipc/src/singleton.rs:142-161` (taskkill — the quit flash)

**Interfaces:**
- Consumes: nothing new. Note the pattern documented at `resonance-ipc/src/service/windows.rs:22-38` (`hidden()`).
- Produces: no visible console windows from any Resonance spawn.

- [ ] **Step 1: Flag the powershell spawn**

`win_devices.rs` `attach_apo_endpoint` (~line 243) — the file is already windows-only. Add the creation flag:

```rust
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new("powershell.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
```

(Place `use`/`const` per the file's local conventions — module top if other spawns exist there. Cannot reuse `service::windows::hidden()` — different crate.)

- [ ] **Step 2: Flag the singleton taskkill**

`singleton.rs` windows branch of `stop` (~line 152):

```rust
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
    }
```

(If `crate::service::windows::hidden` is reachable from singleton.rs — same crate — prefer `hidden("taskkill")` over the inline const; check its visibility first.)

- [ ] **Step 3: Verify + commit**

Run: `cargo clippy --target x86_64-pc-windows-msvc -p resonance-daemon -p resonance-ipc -- -D warnings`; `make check`.
Expected: clean. Visual confirmation (no flash) is Task 8 / tester.

```bash
git add crates/resonance-daemon/src/audio/win_devices.rs crates/resonance-ipc/src/singleton.rs
git commit -m "fix(windows): hide console windows for powershell and taskkill spawns"
```

---

### Task 6: Graceful stop path + settings-driven quit scope

**Files:**
- Modify: `crates/resonance-ipc/src/service/windows.rs:127-137` (`stop()`)
- Modify: `crates/resonance-tray/src/daemon.rs:96-104` (`MenuAction::Quit` arm)
- Modify: `crates/resonance-ipc/src/tray/config.rs:28-32` (doc comment)

**Interfaces:**
- Consumes: `Command::Shutdown` (exists, resonance-ipc/src/lib.rs:258); `SyncClient::connect_with_timeout` + `send`; `is_active()` (windows.rs:68-70); `tray::control::quit_ui()`; daemon-side bypass-on-Shutdown (Task 3).
- Produces: `service::stop()` on Windows tries graceful IPC shutdown first (so the APO bypass publish actually runs), taskkill only as fallback; tray "Quit Resonance" also closes the GUI.

- [ ] **Step 1: Graceful-first `stop()` on Windows**

Replace `stop()` in `service/windows.rs`:

```rust
#[allow(clippy::unnecessary_wraps)]
pub fn stop() -> io::Result<()> {
    // Graceful first: over IPC the daemon publishes an APO bypass before
    // exiting, so audio stops being processed the moment it dies. A bare
    // taskkill (the fallback) skips that hook — the APO's heartbeat
    // staleness then bypasses within ~2 s instead.
    if let Ok(mut c) = crate::transport::SyncClient::connect_with_timeout(
        std::time::Duration::from_millis(400),
    ) {
        if c.send(crate::Command::Shutdown).is_ok() {
            for _ in 0..33 {
                if !is_active() {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
        }
    }
    let _ = hidden("taskkill")
        .args(["/im", "resonanced.exe", "/t", "/f"])
        .output();
    Ok(())
}
```

(Match `SyncClient`'s real API — the tray calls `c.send(cmd)?` today, tray/daemon.rs:61. If `send` expects an owned `Command`, this is already right. Do not change the unix `stop()` — systemd's SIGTERM already reaches the Task 3 signal handler.)

- [ ] **Step 2: Quit scope follows the setting**

`tray/daemon.rs` `MenuAction::Quit` arm:

```rust
        MenuAction::Quit => {
            // "Quit Resonance" (the default) closes everything — GUI, daemon,
            // tray; plain "Quit tray" exits only the tray. Best-effort: a
            // failed stop must not block the tray from exiting.
            if tray::TrayConfig::load().quit_stops_daemon {
                let _ = tray::control::quit_ui();
                let _ = service::stop();
            }
            std::process::exit(0);
        }
```

- [ ] **Step 3: Fix the flag's doc comment**

`tray/config.rs:28-32` — update to match reality:

```rust
    /// When set, the tray's "Quit Resonance" item closes *everything*: the
    /// GUI window, the daemon (graceful IPC shutdown, taskkill fallback) and
    /// the tray itself. On by default. When cleared, it exits just the tray,
    /// leaving the daemon (and the rest of the stack) running. Closing a UI
    /// window never stops the daemon regardless of this flag.
    pub quit_stops_daemon: bool,
```

- [ ] **Step 4: Verify + commit**

Run: `cargo test -p resonance-ipc` (config tests incl. `quit closes everything by default` must still pass); `cargo clippy --target x86_64-pc-windows-msvc -p resonance-ipc -p resonance-tray -- -D warnings`; `make check`.

```bash
git add crates/resonance-ipc crates/resonance-tray
git commit -m "fix(tray): quit closes gui and stops daemon gracefully over ipc"
```

---

### Task 7: Workspace + cross-target verification sweep

**Files:** none (verification only; fix anything it surfaces in the offending task's file).

- [ ] **Step 1: Full linux check**

Run: `make check`
Expected: EXIT=0, all suites green.

- [ ] **Step 2: Windows cross-target clippy over every touched crate**

Run: `rustup target add x86_64-pc-windows-msvc 2>/dev/null; cargo clippy --target x86_64-pc-windows-msvc -p resonance-apo -p resonance-ipc -p resonance-daemon -p resonance-tray -- -D warnings`
Expected: clean. (This catches `cfg(windows)`-only code that linux clippy never sees — the exact blind spot that previously masked daemon lints for two releases.)

- [ ] **Step 3: Commit only if fixes were needed** (message per the task whose code was fixed, e.g. `fix(daemon): windows clippy fallout`).

---

### Task 8: Live verification on the Windows VM (controller-driven)

**Files:** none (verification). The session controller holds the VM access + APO registration recipes (memory: dockur-winvm, winvm-access, windows-apo-proven) and drives or closely scripts this task.

- [ ] **Step 1: Build + deploy TOGETHER** — daemon, tray, cli, gui and the APO dll are all from this branch (STATE_VERSION 10 requires matched daemon+APO). Register/replace the APO per the proven recipe and restart the Windows audio service so audiodg loads the new dll.

- [ ] **Step 2: Bug 2 check (scriptable)** — via CLI in the guest: load a profile, map it to the default device, `resonance shutdown`, restart daemon, then `resonance status --json` must show the mapped profile as current (no manual selection). Repeat once more to prove it's not first-boot luck.

- [ ] **Step 3: Bug 1 graceful path** — with an audible EQ band set (e.g. +6 dB @ 1 kHz): run the `resonance verify` harness to capture the EQ'd response (Windows verify plays + captures itself over WASAPI loopback, daemon-independent). Then `resonance shutdown`. Immediately re-run verify: response must be FLAT (bypass published before exit). `tasklist` must show no `resonanced.exe`.

- [ ] **Step 4: Bug 1 crash path (heartbeat)** — restart daemon, EQ audible again, then `taskkill /im resonanced.exe /f` (bypassing every shutdown hook). Wait 3 s. Verify run must show FLAT response (staleness bypass). Restart the daemon: EQ must come back by itself (mapped profile via Task 4 + generation-change rebuild).

- [ ] **Step 5: Tray quit scope** — with GUI + tray running and `quit_stops_daemon` default: tray "Quit Resonance" → GUI window closes, `resonanced.exe` gone, audio unprocessed, tray gone. Flip `quit_stops_daemon=false` in tray.toml → "Quit tray" exits only the tray.

- [ ] **Step 6: Bug 3 flash** — start the GUI from the Start-menu shortcut (or `resonanced.exe` directly) and watch for a console flash at startup and on Quit-UI. Expected: none. (If no interactive display is available over ssh, record as "static flags verified; tester to confirm visually".)

- [ ] **Step 7: Record results** in the progress ledger; anything failing goes back to its task via the review loop.

---

## Verification checklist (maps to the bug reports)

- [ ] Tray "Quit Resonance": GUI + daemon + tray all exit; audio is unprocessed immediately (graceful bypass). `quit_stops_daemon=false` → tray-only quit.
- [ ] `taskkill /f` or daemon crash: audio unprocessed within ~3 s (heartbeat staleness).
- [ ] Daemon restart on Windows: device-mapped profile auto-applies; `resonance status` shows it with no manual step.
- [ ] No console/PowerShell window at startup, at Quit-UI, or at daemon stop.
- [ ] `make check` green; windows cross-target clippy green; `resonance-apo` tests green on linux.
- [ ] Linux/macOS behavior unchanged (signal-handler bypass is a no-op without an APO writer; unix `service::stop` untouched).
