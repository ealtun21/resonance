# Audio backend robustness — research findings

- **Date:** 2026-09-19
- **Status:** macOS and Windows audits **complete**. PipeWire/Linux and daemon-core/DSP dives **pending** (sections 6–7 are scoped placeholders with known anchors).
- **Method:** read-only audit of the master worktree (source, git history, `resonance verify` harness references). No live-daemon interaction, no code changes.
- **Companion doc:** `2026-09-19-worktree-task-list.md` — open items below land in its §11 (rate/pitch) and §12 (stability) as actionable worktree tasks, referencing the finding IDs used here.

**Finding ID scheme:** `HIST-*` (fixed historical root causes), `MAC-*` (macOS CoreAudio), `WIN-*` (Windows APO + daemon control plane), `PW-*` (PipeWire, pending), `CORE-*` (daemon core + IPC, pending), `DSP-*` (shared DSP path).
**Status per finding:** `fixed` (shipped on master) / `open` (risk remains) / `gap` (untested; no code defect asserted) / `note` (design fact, not a defect).

---

## 1. How pitch breaks — unified model

A pitch shift means the clock that *wrote* a buffer differs from the clock that *consumes* it. Resonance has up to three clocks:

1. **Capture clock** — input stream's device/mixer rate (PipeWire graph rate, macOS HAL tap rate, WASAPI endpoint rate locked into the APO).
2. **DSP clock** — rate of the daemon's mirror chain and the resampler's `to_hz` (built at session setup / on rate-change notification).
3. **Playback clock** — output device rate.

The DSP chain is rate-specific (filters are rate-correct; `c3a0643` body). Stable pitch requires `capture clock → (SRC) → DSP clock == playback clock` at all times. Every pitch bug fixed so far was one of:

- **(a)** a missing or wrong-rate SRC stage;
- **(b)** the DSP chain frozen at construction rate while the device rate moved;
- **(c)** an intermediate cache holding a stale rate (macOS aggregate tap, Windows `SetDeviceFormat`).

Per-OS architecture (the core asymmetry behind "pitch issues on some OSes"):

| | Linux (PipeWire) | macOS (CoreAudio) | Windows (APO) |
|---|---|---|---|
| Who does SRC | In-graph (PipeWire); daemon resamples only when `live_capture_rate != sample_rate` | `io_proc` resampler (tap rate → output rate) | **None — the APO never resamples**; endpoint rate locked once at activate |
| Rate source | Graph `clock.rate`, rebound per block | Output-device properties via tap lifecycle | Endpoint rate at APO lock + daemon `SetDeviceFormat` |
| Rate-change response | Chain rebind every block (tight) | Tap recreation (≤500 ms silence window, `MAC-8`) | Daemon-driven, best-effort (`WIN-2`) |
| Liveness of audio threads | pending (section 6) | Gaps `MAC-19`/`MAC-23`/`MAC-29` | Top gap `WIN-10/13` (daemon-dead ⇒ no liveness signal, stale chain forever) |
| RT allocations | pending | `MAC-13`/`MAC-14` | none in hot path (pre-allocated scratch) |

The original pitch bug was 44.1 kHz capture replayed at 48 kHz (+8.8%): evidence in `meters.rs:31-42` (live capture-rate vs DSP-rate tracking exists "so the pitch bug is diagnosable"), `verify.rs:802,1122-1129` (`fft_peak_detects_pitch_shift` — "the pitch-bug scenario: audio captured at 44.1 k but replayed as 48 k"), and the APO 1 kHz peak check (`ffi.rs` hires harness).

---

## 2. HIST — historical root causes, all fixed on master

| ID | Bug | Root cause | Fix (commit) | Evidence |
|---|---|---|---|---|
| HIST-1 | 44.1 kHz capture replayed at 48 kHz → +8.8% pitch (the original pitch bug) | Backends froze the chain at 48 kHz: PipeWire never rebound the filter to the graph clock; macOS HAL tap pushed capture-rate samples into an output-rate stream with no conversion. Mistuned EQ / shifted pitch on 44.1 kHz DACs, Bluetooth codecs, hi-res 96/192 kHz | `c3a0643` — follow device sample rate end-to-end + rate test harness: new `StreamResampler`; PipeWire rebinds chain to live `clock.rate` each block; macOS resamples tap-capture rate → output rate before the ring; `status` publishes live capture + DSP rates | `c3a0643` body; `meters.rs:31-42`; `verify.rs:1122-1129`; offline capture→DSP→playback harness (THD+N < −80 dB, 16k..192k, before/after guard) |
| HIST-2 | macOS: stale resample after a device rate change | Tap kept the old aggregate rate after the output rate changed | `4dc1110` — recreate the tap on device sample-rate change (no stale resample) | recreate block `CA 237-254` (rate-stale gate `CA 239-240`) |
| HIST-3 | macOS: aggregate rate lagged the output device | Aggregate-tap node rate never followed the output device | `55137e6` — aggregate follows output rate | commit body: forced 96 kHz starved the output ring (silent, live-verified); cap = native mix rate (usually 48 k), graceful degrade if the HAL refuses; supervisor follows the live nominal rate (`CA 237-258`) |
| HIST-4 | Rate change produced non-realizable bands / non-finite params | Coefficients rebound to a new rate could be unrealizable (poles outside the unit circle); params could be NaN/inf | `4298a57` + `a004094` — reject non-finite params; disable unrealizable bands on rate change | commit body: NaN passed `validate` (fails every comparison), poisoning biquads until a reset — reachable from a hostile preset (`.txt` `parse_db` accepts `'nan'`); rebase disables bands at/above the new Nyquist (`chain.rs`/`filter.rs`) |
| HIST-5 | Effect state lost on rate change | Effects held rate-derived internal state | `86340e1` — rebind effects on rate change | commit body: cpal backends re-derived only filter coefficients — effects (Fidelity HP, Ambience reverb, Bass) stayed at the old rate; fix centralizes `ProcessorChain::rebind_sample_rate` (intensity+enabled carried across the rebuild) |
| HIST-6 | Windows hi-res (96/192 kHz) unverified | APO path had no hi-res rate-correctness coverage | `907ce7d` — hi-res rate-correctness harness against the real exports (see `WIN-5`); `14a5c86` — dropped the dead WASAPI loopback backend (APO-only path) | `ffi.rs` hires harness | commit body: cfg(windows) harness drives the real `resonance_apo_{create,lock,process}` exports at fixed + pseudo-random rates (incl. 96/176.4/192 k) up to ~384 kHz, no audiodg; 17 rates pass on the VM, +12 dB @ 1 kHz band check |
| HIST-7 | rubato 0.16 API | Deprecated resampler API in use | `d28d44d` — rubato 0.16 → 3.0 (`StreamResampler` built on `Async` windowed-sinc) | `resample.rs:104-112` | commit body: pure API migration — same windowed-sinc quality (BlackmanHarris2, 256-tap, 256× oversampling, shared `calculate_cutoff`), pitch band 16k..192k, THD+N < −80 dB, bypass bit-exact |

**Residual by design** — pitch is only as good as the OS's rate-change notification: PipeWire per-block `clock.rate` (tight); macOS device property listener (stale window exists, `MAC-5`); Windows daemon-driven `SetDeviceFormat` (best-effort, `WIN-2`). Bluetooth SCO 16 kHz on Linux is a documented user-visible case (`docs/wiki/Troubleshooting.md`).

---

## 3. Shared SRC: `StreamResampler` (`crates/resonance-dsp/src/resample.rs`)

Design facts (lines 60–252):

- Bypass when rates are equal or either ≤ 0 (`resample.rs:94-99`); bypass returns the input unchanged, **bit-exact, no copy** (`resample.rs:178-180`).
- Rubato `Async` windowed-sinc, ratio = `to_hz / from_hz` computed once at construction, `max_relative_ratio 2.0` "leaves room to nudge the ratio later for clock-drift tracking" (`resample.rs:100-112`).
- Chunked emission: input accumulates in `in_acc`; **no output until a full CHUNK (256) input frames is buffered** (`resample.rs:108, 184-189`). Output bound pre-computed per call (`resample.rs:193-194`); buffers pre-sized so steady state is alloc-free (`resample.rs:114-121`).
- `output_delay_frames()` = sinc group delay **only** (`resample.rs:155-161`), reported by backends so `status` can show "true end-to-end latency".
- `reset()` drops buffered input + history (`resample.rs:245-251`).

Findings:

- **DSP-1 (open, med):** `process()` panics via `.expect()` on rubato adapter construction and chunk errors (`resample.rs:208, 210, 224`). The macOS audit proved these unreachable *under the current invariants* (fixed 256-frame chunk cadence, full-block input extend, exact out interleave — `MAC-28`), and it runs on RT threads (`io_proc`, PipeWire filter block). The invariants are structural, not compiler-enforced: a future caller feeding an irregular cadence (or a rubato change altering error conditions) turns this into a render-thread panic = silence on that side. Mitigation: return a `Result` (or fill-last-value + flag in state) instead of panicking on the RT path.
- **DSP-2 (open, low-med):** `status` latency under-reports. `output_delay_frames` excludes the chunking delay of `in_acc` (up to CHUNK frames **of input audio** held before first emission, `resample.rs:184-189`) — i.e. up to ~`CHUNK / from_hz` seconds of added latency (≈ 5.8 ms at 44.1 kHz input, ≈ 16 ms at 16 kHz SCO input) is missing from the reported end-to-end latency.
- **DSP-3 (note):** the resample ratio is lifetime-fixed; rubato's `set_resample_ratio` API (reserved via `max_relative_ratio 2.0`) is **never called** — rate changes are handled only by full resampler rebuild (`MAC-2`). Slow inter-clock drift (±50–100 ppm crystal tolerance) is therefore not tracked. Related to backlog #45 (rubato in-band streaming kernel); windowed-sinc remains the shipped interim.
- **DSP-4 (gap):** resampler `process` under the 256-frame chunk cadence is only partially tested — existing tests cover basic round-trip, not the max-out margin / `R < 1` downsample corner (`resample.rs:254-292`; see `MAC-36`).

Pending (covered by the daemon-core/DSP dive, section 7): rate-change propagation through `chain.rs`/`channel.rs` band-disable and effect rebind, meters/spectrum hot paths, non-finite handling, unbounded growth.

---

## 4. macOS (CoreAudio) — complete

Thread model (verified): (1) `io_proc` (`hal_input.rs:221-322`) on the input stream's render thread — copies tap into the ring, runs the resampler (in-path SRC tap→output rate), metering; (2) `data_cb` (`coreaudio.rs:553-719`) on the output stream's render thread — drains ring into the HAL; (3) tap-lifecycle supervisor (`coreaudio.rs:226-300`, spawned `main.rs:464-476`) — start/stop/recreate of the system tap on device changes; (4) control threads (state/IPC). Non-RT paths (`coreaudio.rs:72/93`) share the same mutex the RT path locks.

Citation keys: CA=`coreaudio.rs`, HI=`hal_input.rs`, ST=`system_tap.rs`, RS=`resample.rs`, A=rubato `asynchro.rs`, AS=rubato `asynchro_sinc.rs`, RL=rubato `lib.rs`, MAIN=`main.rs`, IPC=`ipc_server.rs`, STATE=`state.rs`, MAPP=`mac_apps.rs`.

### §4.1 Rate handling

- **MAC-1 (open):** tap's actual sample rate is never checked against the resample target — format query runs (`HI 359-394`) but there is no mismatch guard; the ratio is baked in the ctor (`HI 107`). If the tap rate ≠ nominal, pitch drifts silently.
- **MAC-2 (note):** resample ratio is lifetime-fixed (`HI 107`); rubato's `set_resample_ratio` (`A 577-595`) is never called; rate changes handled only by full tap rebuild. (= DSP-3.)
- **MAC-3 (open):** output rate read exactly once (`CA 363-366`) with a 48 kHz clamp (`CA 379`); a later output-device rate change leaves a stale ratio until rebuild.
- **MAC-4 (open):** three heterogeneous rate-threshold rules — default-tap rebuild gate ±1.0 Hz (`CA 239-241`), exact f32 equality for per-poll rate-change detection (`CA 500`), per-creation settle-based tolerances for aggregates (`ST 467`/`502`); the rules are not unified — a rate change is only *seen* if it crosses the per-poll exact-eq comparison, only *acted on* if it breaches ±1.0 Hz, and aggregate creation tolerates whatever settle drift passes.
- **MAC-5 (open):** wrong-rate window: between rate detection and the new tap's creation (`CA 237` → `CA 255-279`) the pipeline consumes at the wrong rate — the stale-tap pitch window `HIST-2` fixed the recreate but not the gap inside it.
- **MAC-6 (note):** one ring per stdin (`CA 428` / `HI 127`); overfill drops silently (`HI 318`).

### §4.2 Tap recreation

- **MAC-7 (note):** recreation ownership is race-free, but `io_proc` holds no alive flag; correctness rests entirely on CoreAudio stop/Dispose semantics (`HI 182-205`).
- **MAC-8 (open):** mid-recreation, in-flight ring frames are dropped and output zero-padded (`CA 574-576`); mixer mode adds ≤ 500 ms settle (`ST 501-510`). Audible gap on every recreation.
- **MAC-9 (open, low):** a fresh resampler's first chunk is ~2 frames short (`AS 518-520`, `init_last_index`) → tiny glitch per recreation.
- **MAC-10 (note, verified not a bug at current rates):** overflow margin ≥ ~488 frames (`RS 193-194`; `A 415-417` in-acc full-block extend; `A 542-549` max-out); only R ≲ 0.05 (heavy per-app downsample) approaches it.

### §4.3 Buffer acquisition / RT-thread safety

- **MAC-11 (open):** producers push; backpressure is silent drop (`HI 318`) / zero-pad (`CA 574-576`), never rejection — undiagnosable loss.
- **MAC-12 (note):** layout handshake scattered across channel count (`HI 293/274-282`), frame cap (`RS 41`), interleave (`HI 261-263/278`), ring sizing (`HI 331`).
- **MAC-13 (open):** per-call RT allocations in the resampler (`RS 182/196`; fresh slices each `process()`).
- **MAC-14 (open):** per-callback allocations in `data_cb` (`CA 592`; meters/spectrum `CA 633-671`).
- **MAC-15 (verified, not a bug):** the only blocking operations on RT are the two mutex locks (`CA 593/621`); nothing else blocks.
- **MAC-16 (open):** zero logging on RT threads; all diagnostics come from the supervisor (`CA 464-479, 481-520`). RT stalls are invisible until audible.
- **MAC-17 (open):** backlog > `DRAIN_SLACK_FRAMES` = 4096 (`CA 58`) is drained (`CA 566-570`) → silent loss under sustained overfill.

### §4.4 Error / recovery

- **MAC-18 (verified, works):** device unplug recovers via rebuild (`ST 92-221` / `CA 226-300`).
- **MAC-19 (open, top gap):** delivered-then-frozen tap mid-session is **undetectable**: `nonzero_blocks` (`HI 122` / `CA 428`) and the `nz==0` check (`CA 470`) catch only never-delivered; `callback_count` (`HI 236`) is never diffed. Symptom: permanent silent one-app input, no warning.
- **MAC-20 (open):** TCC preflight is create-only (`ST 827-863`); a mid-session permission loss surfaces through failure, not probing.
- **MAC-21 (verified):** aggregate-creation failure handled by bounded retry (`CA 260-265`).
- **MAC-22 (open):** AU output errors surface only via `err_cb` (`CA 541`) — the sole writer of `stream_err`; no AUHAL status polling.
- **MAC-23 (open):** a clean stop (cpal) produces **no** error callback; the supervisor loop (`CA 455-523`) has no liveness check. A dead output stream stays silently silent.
- **MAC-24 (note):** recovery is client-driven: IPC Reset (`IPC 222`) ⇒ `needs_resync` (`STATE 293`) + ReplaceChain (`STATE 295-298`); no self-heal.
- **MAC-25 (open, low):** dead code — `err_flag.load` (`CA 718`) reads a flag never written; a misleading recovery signal.

### §4.5 Panics / asserts

- **MAC-26 (verified):** `io_proc` (`HI 221-322`) is panic-free; only global backstop is the main panic hook (`MAIN 62-66`).
- **MAC-27 (verified unreachable):** resampler ctor `.expect` (`RS 112`) — the bypass gate (`RS 96`) guarantees ratio > 0 before construction.
- **MAC-28 (verified unreachable, invariants structural):** `process` expects (`RS 208/210/222-224`) — rubato `check_slice_length!` (`AB 134-151`) errors only on under-length inputs; in-acc full-block extend; exact out interleave (`RS 196`); the fixed 256-frame chunk + guard (`RS 221/229`) satisfies `validate_buffers` (`RL 374-413`). See DSP-1 for the fragility note.
- **MAC-29 (open, top gap):** the only live RT panic is `lock().unwrap()` (`CA 593/621`).
- **MAC-30 (open, top gap):** poison propagation — non-RT lockers (`CA 72/93`) can poison the same mutex the RT thread locks ⇒ **any non-RT panic ⇒ permanent RT panic loop = total audio death**. Fix: park-lot mutex or `unwrap_or_else(PoisonError::into_inner)` on RT.
- **MAC-31 (note):** non-RT errors are retry-only (`CA 762-763, 260-265, 292`); no circuit-breaker beyond the 5 s flapping cap (`CA 263/299`).

### §4.6 Test coverage (all `gap` unless noted)

- **MAC-32:** `data_cb` (`CA 553-719`) untested.
- **MAC-33:** supervisor loop (`CA 455-523`) untested (rate-change, aggregate-fail, drain branches).
- **MAC-34:** outer recreation loop (`CA 226-300`) untested (backoff reset `295-297`, 5 s cap `263/299`).
- **MAC-35:** `io_proc` (`HI 221-322`) untested (drop path `HI 318`, resample path).
- **MAC-36:** resampler `process` under the 256-frame chunk cadence (`RS 176-242`) — existing tests (`RS 254-292`) cover basic round-trip only, not max-out margin / R < 1. (= DSP-4.)
- **MAC-37:** `system_tap` branches untested (create `92-221`, mixer `237-328`, Drop `354-380`, aggregate rate `465-516`); `mac_apps.rs` has zero unit tests.
- **MAC-38:** `pick` (`CA 751-764`) untested.
- Existing baseline only: `CA 800-870`, `HI 396-439`, `ST 865-878`, `RS 254-292`, `audio/mod.rs 263-442`.

### §4.7 Ranked gaps (likelihood × impact)

1. **(a) Frozen tap undetectable** (Med × High) — `MAC-19`: per-stdin `nz` (`HI 122` / `CA 428`); `nz==0` (`CA 470`) misses delivered-then-stopped; `callback_count` (`HI 236`) unused.
2. **(b) No output liveness** (Low-Med × High) — `MAC-22/23`: sole `stream_err` writer is `err_cb` (`CA 541`); clean stop invisible in `455-523`.
3. **(c) RT mutex poison** (Low × High) — `MAC-29/30`: non-RT `CA 72/93` poisoning ⇒ RT panic loop at `CA 593/621`.
4. **(e) Per-app rebuild amplification** (Med × Low-Med) — `MAC-8`: each app change walks the full recreation path (≤ 500 ms silence); opt-in via `RESONANCE_PERAPP` (`CA 163`).
5. **(d) Spurious/stale rebuilds** (Med × Low-Med) — `MAC-4/5`: exact-eq rate compare (`CA 500`) vs ±1.0 Hz rebuild gate (`CA 239-241`); name-follow (`CA 486-493`) vs UID (`CA 237`) ⇒ a device rename triggers a rebuild.
6. **(f) Unbounded RT allocations** (Low × Low-Med) — `MAC-13/14`: `RS 182/196`; `CA 592/633-671`.
7. **(g) Dead/dropped signals** (Low × Low) — dead load `CA 718` (`MAC-25`); `.ok()` drops `CA 79/104`; silent drops `HI 318, CA 566-570` (`MAC-6/11/17`).
8. **(h) Doc drift** (n/a × Low) — `MutedWhenTapped` (`CA 9-11`; `ST 13-16`) vs actual `Muted` (`ST 149/259`); "negotiate common rate" (`CA 14-15`) vs read-once (`CA 363-366`).

**Audited non-bugs (resolved):** pinned preferred output never churns the tap (tap_uid = OS default, `CA 257`; gate `CA 241` fires only on uid change or ±1.0 Hz staleness). All five resampler `.expect`s unreachable (proofs `MAC-27/28`). Backoff reset > 10 s (`CA 295-297`); flapping capped at 5 s (`CA 263/299`).

---

## 5. Windows (APO + daemon control plane) — complete

Scope note: the APO is a DLL loaded into `audiodg.exe` — a **thin C++ COM/aggregation shell** (`crates/resonance-apo/cpp/resonance_apo.cpp`, extern "C" forward block `:28-36`) linked against the Rust staticlib (crate-type `["staticlib","lib"]`; MSVC build `contrib/windows/build-apo.ps1:24-32`). All signal processing is Rust (`lib.rs:3-8`, `ffi.rs:1-3`) — that is what the "pure-Rust APO" wording in `2026-07-09-windows-lifecycle-bugfixes.md:7` loosely means; the plan doc is the drift target, not the code (`WIN-23`). The daemon owns **no** audio path on Windows — control plane only. `system_tap.rs` is macOS-only and does not apply anywhere here (no tap exists; audiodg processes in the effect slot directly). Citation keys: FFI=`resonance-apo/src/ffi.rs`, DEV=`win_devices.rs`, MEAS=`win_measure.rs`, STATE=`state.rs`.

### §5.1 Rate handling + hires harness scope

- **WIN-1 (open, note as workaround):** cpal's `default_*_config` may report a supported rate different from the mix (48000 vs 44100), silently triggering WASAPI's internal resampler + high-frequency rolloff (`DEV 280-287`); the workaround queries the undocumented `IPolicyConfigVista::GetDeviceFormat` (`DEV 289-323`).
- **WIN-2 (open, top):** no post-set MixFormat verification. Rate query/set (`DEV 56-88`) and cable rate pinning (`DEV 221-363`) act without re-confirming the endpoint format after the set; `SetDeviceFormat` results — including "format locked while streaming" — are ignored (`DEV 345-356`) and both callers swallow errors entirely (`DEV 256-259, 264-267`); vtable decls only (`DEV 107, 137`). Symptom: a failed/partial rate change leaves daemon and APO operating on mismatched rate assumptions — the Windows analogue of the pitch bug.
- **WIN-3 (note, architectural):** the APO **never resamples**. `resonance_apo_lock` takes the endpoint rate once and builds the whole chain (`FFI 423-460`, chain built at `FFI 62-70`); worker-side rebuilds reuse the same rate (`FFI 250`). The f64 rate is stored from raw bits **with no validity check at lock time** (`FFI 436-438`). All rate correctness therefore depends on the daemon control plane (see WIN-2).
- **WIN-4 (open):** `Process` handles `n = want.min(scratch.len())`, leaving the tail of an oversized buffer **raw** (`FFI 498-502`). Symptom: audible click/discontinuity whenever the endpoint delivers more frames per buffer than the scratch was locked in at.
- **WIN-5 (`gap`, note scope):** the `907ce7d` hires harness is an inline `mod hires_harness` inside `FFI 591-797+` — `crates/resonance-apo/tests/` **does not exist**. It covers **9 fixed consumer rates (up to 384 kHz) + 8 LCG-generated rates in [8000, 384000] Hz** (`FFI 728-737`) exercising the real exports (create→lock→process→unlock→destroy). Harness tests share `default_state_path` (`FFI 724, 780, 791`) ⇒ **not parallel-safe; requires `--test-threads=1`**.
- **WIN-6 (open):** `--measure-loopback` opens via the un-corrected `default_output_config()` (`MEAS 62-66`, within `MEAS 53-114`) — the wrong-rate pitfall of WIN-1 sits inside the very tool that exists for objective spectral comparison. Symptom: wrong-rate loopback ⇒ misleading diagnostics.
- **WIN-7 (note):** rate/chain propagation is via the state file: APO worker polls + fresh-reads every 25 ms (`FFI 204-242`).

### §5.2 APO lifecycle (activate/deactivate, re-attach, audiodg & daemon restart)

- **WIN-8 (open, low):** audiodg crash/restart: the DLL is reloaded per endpoint; configuration is durable on disk (mmap state file) so no settings loss, but filter/scratch state resets → a brief gap window during re-init.
- **WIN-9 (open):** default-device re-attach: the daemon dynamically re-attaches the APO to the new endpoint (`DEV 221-363`; manual path `attach-endpoint.ps1:5-6`). New instances inherit the **global** chain regardless of the new endpoint's rate/format — no per-endpoint chain state. (Interacts with WIN-2/WIN-3: a different-rate endpoint after re-attach = pitch shift until the daemon re-pins rates.)
- **WIN-10 (open, top gap):** **daemon restart/death ⇒ APO keeps the last chain indefinitely** — no liveness signal shipped. The v10 layout was reserved for an unmerged "daemon liveness heartbeat" (`STATE 45-46`); current `STATE_VERSION = 11` (v11 removed Loudness, `STATE 47-48`). Bypass-on-teardown and `enabled=0` mechanisms exist **only in the unmerged plan** (`2026-07-09-windows-lifecycle-bugfixes.md:5-7, 18-19, 23-30, 592-598`) — i.e. the worktree-win-lifecycle-fixes branch / PR #65.
- **WIN-11 (verified, note):** install/update — `resonance.iss:98-109` stops `audiosrv` before DLL overwrite (the DLL is locked while audiodg holds it); uninstall hook at `:77-81`. `install-apo.ps1:114-116` APO catalog + signature-check handling, `:179` audiosrv restart to load the new DLL; `uninstall-apo.ps1:68-70, 76` restore.

### §5.3 IPC transport, daemon-unresponsive, overrun, reconnect, deadlock

- **WIN-12 (note):** transport = **no socket/FIFO**; the bridge is the mmap seqlock state file + APO log file. Single writer (daemon) publishes via seqlock (`STATE 770-783`); the APO reads consistent snapshots via fresh-read (`read_chain_fresh`, `STATE 935`).
- **WIN-13 (open, top):** daemon unresponsive ⇒ worker keeps re-reading the stale file every 25 ms (`FFI 204-242`) with no liveness check and no bypass ⇒ **last chain applied forever**. (Same root as WIN-10; ranked as the #1 Windows gap.)
- **WIN-14 (verified, not a bug):** no ring buffers in the APO — no classic underrun; "overrun" manifests only as the partial-buffer case (WIN-4). Reconnect is N/A (mmap always present).
- **WIN-15 (open, low):** asymmetric visibility assumptions — the read path explicitly distrusts a long-lived mapped view (`FFI 240`), yet telemetry writes use the long-lived view (`write_telemetry`, `STATE 854`) ⇒ stale/missing telemetry can mask faults.
- **WIN-16 (verified, not a bug):** no IPC locks; deadlock-adjacent risks are only the RT log mutex (WIN-18); the single-writer seqlock is deadlock-free by design (reader retries torn reads).

### §5.4 RT-thread safety in `Process`

- **WIN-17 (open, top):** `Process` takes `try_lock` on the config mutex; on contention the buffer is **silently dropped** (`FFI 492-494`). Symptom: silent per-buffer dropouts (clicks) on every daemon publish while contention occurs.
- **WIN-18 (open):** the RT thread takes the **global** log mutex and does **file I/O** every ~100 callbacks (`FFI 517-526`; global mutex + file append `log.rs:11, 26-36`). Symptom: unbounded RT latency on disk stalls or concurrent daemon logging.
- **WIN-19 (verified, note):** scratch is pre-allocated; the partial-buffer path reuses it (`FFI 498-502`) — no per-call allocation in the hot path.
- **WIN-20 (open, top):** workspace release profile is `panic = "abort"` (`Cargo.toml:155`); only `--profile apo` switches to `panic = "unwind"` (`Cargo.toml:163-165`, rationale `:159-162`). **Nothing enforces the apo profile** ⇒ a plain `cargo build --release` build makes any panic **abort audiodg.exe** (service restart wipes all APO instances; total audio interruption until restart).

### §5.5 Protected audio / DRM

- **WIN-21 (open, note):** the installer sets `DisableProtectedAudioDG = 1` **globally** (`install-apo.ps1:91-92`); restore in `uninstall-apo.ps1:35, 68-70, 76`. **No runtime detection** of which path is protected; docs only warn "DRM/protected-audio apps may be muted" (`docs/wiki/Troubleshooting.md`, `Installation.md`). Symptom: DRM playback mutes post-install with no in-band diagnostic.

### §5.6 Test coverage (all `gap` unless noted)

- Control plane: **only `app_streams.rs` has unit tests** (platform-neutral keying helpers). `win_devices.rs`, `win_measure.rs`, `win_apps.rs`, `win_sinks.rs`: zero unit tests (COM-bound).
- APO crate: `STATE 1672, 1701` tests; hires harness `FFI 728-737` (not parallel-safe, WIN-5).
- **WIN-22 (gap):** named untested branches: (a) frames > scratch partial transform `FFI 498-502`; (b) `try_lock` contention drop `FFI 492-494`; (c) RT 100th-callback log I/O `FFI 517-526`; (d) seqlock torn/stale read → retry + keep-last `STATE 770-783, 935`; (e) daemon-dead stale state → worker keeps last chain `FFI 204-242`; (f) mid-session rate change → scratch re-sizing; (g) telemetry write visibility `STATE 854` vs `FFI 240`; (h) invalid/oversize IR blob in state file; (i) dynamic attach to new default endpoint `DEV 221-363`; (j) format-mismatched loopback open `MEAS 62-66`.

- **WIN-23 (note, doc drift):** the "pure-Rust APO/DLL" wording in `2026-07-09-windows-lifecycle-bugfixes.md:7` is loose shorthand — the DLL does contain a thin C++ COM/aggregation shell (`cpp/resonance_apo.cpp:1-36`; extern "C" forwards `:28-36`) linked against the Rust staticlib (crate-type `["staticlib","lib"]` in `resonance-apo/Cargo.toml`; MSVC link `contrib/windows/build-apo.ps1:24-32`). All DSP *is* Rust (`lib.rs:3-8`, `ffi.rs:1-3`), so the code and build agree; only the plan-doc wording drifts. No signal-path impact. Fix = align plan-doc wording (ranked #10, P3).

### §5.7 Ranked gaps (likelihood × impact)

1. **Dead daemon ⇒ infinite stale chain; no bypass/heartbeat** (L: high × I: high) — WIN-10/13. Escape today = killing `audiosrv` or reconfiguring. Fix lives in the unmerged lifecycle work (PR #65).
2. **RT `try_lock` contention ⇒ silent per-buffer dropouts on every publish** (Med × High) — WIN-17 (`FFI 492-494`).
3. **Frames > locked `max_frames` ⇒ partially processed buffer** (Med × High) — WIN-4 (`FFI 498-502`).
4. **Panic profile not enforced ⇒ release build aborts audiodg** (Low × Very high) — WIN-20 (`Cargo.toml:155, 163-165`).
5. **Loopback measurer opens via uncorrected `default_output_config()`** (Med × Med) — WIN-6 (`MEAS 62-66`).
6. **RT-path logging file I/O under global mutex** (Med × Med) — WIN-18 (`FFI 517-526`, `log.rs:26-36`).
7. **Global `DisableProtectedAudioDG=1`, no runtime detection** (Med × Med) — WIN-21 (`install-apo.ps1:91-92`).
8. **Telemetry visibility asymmetry** (Low × Med) — WIN-15 (`STATE 854` vs `FFI 240`).
9. **Hires harness not parallel-safe** (Med × Low) — WIN-5 (`FFI 724, 780, 791`) (CI runs parallel by default ⇒ flaky tests can mask regressions).
10. **Loose "pure-Rust APO" docs** (n/a × Low) — `WIN-23`: the DLL has a thin C++ COM shell (`cpp/resonance_apo.cpp`); all DSP is Rust (`lib.rs:3-8`, `Cargo.toml` staticlib). Fix = align `2026-07-09-windows-lifecycle-bugfixes.md` Architecture wording.

---

## 6. PipeWire / Linux — pending

Dive not yet run (one subagent at a time; token cap). Planned scope and known anchors:

- `crates/resonance-daemon/src/audio/pipewire.rs` — stream lifecycle, the per-block chain rebind to `clock.rate` (introduced by `c3a0643`), buffer/xrun rules, error recovery.
- `crates/resonance-daemon/src/audio/mod.rs`, `stub.rs` — backend dispatch, fallback behavior.
- `meters.rs:31-42` — `live_sample_rate` vs `live_capture_rate` tracking; the bypass condition "daemon resamples only when `live_capture_rate != sample_rate`" (RS 96 gate) — verify exactly where/how the mismatch is detected, at what latency, and what a persistent mismatch does (pitch? drain/slack loss?).
- BT SCO 16 kHz device-switch case (`docs/wiki/Troubleshooting.md`) — the documented user-visible Linux pitch scenario; trace which code path handles it.
- `rate_tests.rs` + `verify.rs` coverage for the Linux path (what is asserted: peak frequency after capture→playback at mismatched rates?).
- Findings will be numbered `PW-1..n`.

### §6.x Findings from the first e2e run (2026-10-07)

- **DSP-E1 (P3):** `ProcessorChain::reset()` does not reset the dither RNG, and the live chain consumes it on every idle graph cycle, so dithered output can never equal an offline render. `fx-all-dithered` compares with `tolerance_dbfs = -80` instead of exact.
- **DSP-E2 (P2):** linear-phase EQ output depends on the caller's block size (spike 4: max abs diff ~0.57 against block 1024 at every size tried, 64..4096). Live it matched the render only because the quantum matched the render block; `block_size_tests::linear_phase_output_is_independent_of_block_size` is `#[ignore]`d as evidence.
- **PW-E1 (P3):** daemon added latency measured through a null sink moves in whole graph quanta (up to 2 quanta apart between identical runs, ring-buffer fill). The e2e gate uses the minimum of three chirp trains; absolute "added latency" values are quantum-granular and can read negative.

## 7. Daemon core + DSP numeric stability — pending

Dive not yet run. Planned scope:

- `main.rs` — thread map (IPC, control, panic hook `62-66`, supervisor spawn `464-476`), startup/shutdown ordering, `shutdown.rs`.
- `state.rs` — lock discipline and ordering (known: non-RT lockers share RT mutex on macOS — `MAC-30`; seqlock single writer `STATE 770-783`; ReplaceChain `STATE 295-298`; `STATE_VERSION`/layout `STATE 45-48`), chain swap atomicity vs concurrent readers.
- `ipc_server.rs` — full read (Reset path `IPC 222`, client errors, timeouts, unbounded message growth).
- meters/spectrum hot paths — allocation, atomics, zero-division guards, non-finite inputs.
- `chain.rs`/`channel.rs`/`convolution.rs`/`effects.rs`/`filter.rs`/`linphase.rs`/`dither.rs` — rate-change propagation (reconcile HIST-4/HIST-5), band-disable correctness, non-finite handling, unbounded growth.
- Grep pass: `Mutex`, `unwrap()`, `thread::spawn`, `.expect(` across daemon + DSP crates to find unproven panic points on hot paths.
- Findings will be numbered `CORE-1..n` / `DSP-5..n`.

---

## 8. Consolidated fix backlog (draft — finalizes in task-list §11/§12 after sections 6–7 complete)

| # | Priority | Finding(s) | Fix sketch | OS |
|---|---|---|---|---|
| 1 | P0 | WIN-10/13 | Daemon liveness heartbeat + APO bypass fallback (v10 layout already reserved); ships via PR #65 — **merge gate: live Windows verify R2** | Win |
| 2 | P0 | MAC-19 | Per-stdin `callback_count` delta watched by the supervisor → stale-tap detection after first delivery | macOS |
| 3 | P0 | MAC-29/30 | Park-lot mutex (or `into_inner`) on the RT-locked `shared` mutex — kills the poison loop | macOS |
| 4 | P1 | WIN-2 | Re-query MixFormat after each `SetDeviceFormat`; fail-loud on "format locked while streaming"; stop swallowing at callers | Win |
| 5 | P1 | WIN-17 | On `try_lock` contention: process with last-known-good config (or short spin + meter the drop), never silent-drop a whole buffer | Win |
| 6 | P1 | WIN-4 | Grow scratch to observed max `n` (bounded) instead of `min(scratch.len(), n)` partial transform; or align scratch to endpoint frame-size multiples | Win |
| 7 | P1 | WIN-20 | Enforce `--profile apo` in the packaging/install pipeline; add a CI assertion that the shipped DLL profile unwinds | Win |
| 8 | P1 | MAC-5/8 | Shrink the rate-change window: pre-create the new tap and cross-fade/switchover, or zero-latency ring swap; unify the ±1 Hz tolerance (`MAC-4`) | macOS |
| 9 | P1 | MAC-22/23 | Output-stream liveness: watchdog on callback timestamps (data_cb already exists to host it) → `stream_err` on silence | macOS |
| 10 | P2 | WIN-6 | `--measure-loopback` must apply the same MixFormat correction as `win_devices.rs` (WIN-1 workaround) | Win |
| 11 | P2 | MAC-13/14, DSP-1 | Pre-size per-callback buffers; make `StreamResampler::process` non-panicking on the RT path | macOS/DSP |
| 12 | P2 | DSP-2 | Include chunking delay (CHUNK frames of input) in reported end-to-end latency | DSP |
| 13 | P2 | WIN-18 | RT log: lock-free ring + background flusher; never file I/O on the RT thread | Win |
| 14 | P2 | WIN-21, WIN-15 | Detect protected-path muting (telemetry + status); align telemetry view lifetime with the read path's distrust | Win |
| 15 | P3 | MAC-6/11/17/25, WIN-5/15/22/23, MAC-32..38, DSP-4 | Test coverage: data_cb/supervisor/io_proc/resampler-cadence/seqlock/attach/loopback — plus dead-code and doc-drift cleanups (ranked (g)/(h) both OSes; WIN-23 "pure-Rust APO" wording) | all |

## 9. Next steps

1. PipeWire deep-dive (fresh subagent, alone) → fill section 6, assign `PW-*`.
2. Daemon-core deep-dive (fresh subagent, alone) → fill section 7, assign `CORE-*`/`DSP-5+`.
3. Re-rank section 8 cross-OS; add **task-list §11 (rate/pitch) + §12 (stability)** with per-worktree actionable items and worktree names.
4. `make check` → conventional lowercase docs commit for this doc + task-list update.
5. (Deferred, pending) task-list §0 housekeeping: `.worktrees/` in `.gitignore`, delete stale branches (`feat/dynamic-eq`, `feat/gui-hide-panes`, `feat/tray-icon`, `feat/convolution-nonuniform`, `feat/preset-metadata`), `git remote prune origin`, `rm 2026-06-*.log`, replace stale `.superpowers/sdd/progress.md`.

Blockers on the user side (unchanged): #42 macOS live-verify (one macOS GUI/TCC session); PR #65 Windows live-verify (R2) before merge; #45 rubato final audiodg-only verification.