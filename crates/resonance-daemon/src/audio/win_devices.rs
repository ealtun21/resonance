//! Windows-only: resolve WASAPI render-endpoint **friendly names**.
//!
//! cpal reports the endpoint *description* (`DEVPKEY_Device_DeviceDesc`, e.g.
//! "Speakers") which is NOT unique once a virtual cable is installed — the
//! cable's render endpoint is *also* "Speakers", so cpal can't tell it apart
//! from the real speakers. We enumerate the same `eRender`/`DEVICE_STATE_ACTIVE`
//! collection cpal does, in the same order, and read `PKEY_Device_FriendlyName`
//! ("Speakers (VB-Audio Virtual Cable)" vs "Speakers (High Definition Audio
//! Device)") so callers can disambiguate cpal's output devices by index.
//!
//! This mirrors how FxSound's `audiopassthru` identifies endpoints (by ID /
//! friendly name, never the ambiguous description).

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, eAll, eConsole, eRender,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::core::{GUID, HRESULT, PCWSTR};

/// Read a friendly-name PROPVARIANT as an owned String, guarding the union: only
/// `VT_LPWSTR` with a non-null pointer is a valid wide string. A driver/endpoint
/// returning a different variant or null would otherwise be a wild pointer deref
/// — a structured access violation that `catch_unwind` can't catch. Returns ""
/// in that case.
///
/// SAFETY: caller passes a PROPVARIANT obtained from `IPropertyStore::GetValue`.
unsafe fn propvariant_str(prop: &PROPVARIANT) -> String {
    let v = &prop.Anonymous.Anonymous;
    if v.vt == VT_LPWSTR {
        let p = v.Anonymous.pwszVal;
        if !p.is_null() {
            return p.to_string().unwrap_or_default();
        }
    }
    String::new()
}

/// Friendly names of all active render endpoints, in `IMMDeviceEnumerator`
/// order — the same order cpal's `output_devices()` yields, so index `i` here
/// names cpal output device `i`. Empty (or short) entries / an empty vec mean
/// the lookup failed; callers fall back to cpal's ambiguous names.
pub fn render_friendly_names() -> Vec<String> {
    enumerate().unwrap_or_default()
}

fn enumerate() -> windows::core::Result<Vec<String>> {
    unsafe {
        // cpal also initialises COM on its threads; re-initialising here is
        // harmless (returns S_FALSE / RPC_E_CHANGED_MODE, both ignorable).
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let coll = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let count = coll.GetCount()?;

        let mut names = Vec::with_capacity(count as usize);
        for i in 0..count {
            let name = (|| -> windows::core::Result<String> {
                let dev = coll.Item(i)?;
                let store = dev.OpenPropertyStore(STGM_READ)?;
                let prop = store.GetValue(&PKEY_Device_FriendlyName)?;
                Ok(propvariant_str(&prop))
            })()
            .unwrap_or_default();
            names.push(name);
        }
        Ok(names)
    }
}

// ── Auto-match the virtual cable's sample rate to the real output ────────────
//
// VB-CABLE's render (input) and capture (output) endpoints can be configured to
// different shared-mode rates; when they differ, the cable resamples internally
// with a low-quality converter that rolls off the highs. We set the cable's
// endpoints to the real render device's rate so the cable passes audio through
// untouched. This is exactly what FxSound does for its own virtual device via
// the undocumented `IPolicyConfig`/`IPolicyConfigVista` COM interface
// (audiopassthru's `sndDevicesSetDfxDeviceSampleRateAndChannels`).

/// CLSID `CPolicyConfigVistaClient` (works Vista..Win11 for format ops).
const CPOLICYCONFIG_VISTA_CLIENT: GUID = GUID::from_u128(0x294935ce_f637_4e7c_a41b_ab255460b862);

#[windows::core::interface("568b9108-44bf-40b4-9006-86afe5b5a620")]
unsafe trait IPolicyConfigVista: windows::core::IUnknown {
    // Vtable order matters; we only call GetDeviceFormat/SetDeviceFormat but must
    // declare the slots that precede them.
    unsafe fn GetMixFormat(&self, id: PCWSTR, fmt: *mut *mut WAVEFORMATEX) -> HRESULT;
    unsafe fn GetDeviceFormat(
        &self,
        id: PCWSTR,
        default_device: i32,
        fmt: *mut *mut WAVEFORMATEXTENSIBLE,
    ) -> HRESULT;
    unsafe fn SetDeviceFormat(
        &self,
        id: PCWSTR,
        fmt: *const WAVEFORMATEXTENSIBLE,
        mix: *const WAVEFORMATEXTENSIBLE,
    ) -> HRESULT;
}

// ── Device selection (Windows control plane) ─────────────────────────────────
//
// The daemon does no audio on Windows, but it still enumerates render endpoints
// so clients can list/pick an output, and sets the system default playback
// device on selection. The in-graph APO follows whatever endpoint the audio
// engine uses (the installer attaches the APO to the render endpoints).

/// CLSID `CPolicyConfigClient` — the documented IPolicyConfig used by EarTrumpet
/// / nircmd to set the default endpoint.
const CPOLICYCONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

#[windows::core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: windows::core::IUnknown {
    // Slots 3..12 are unused here — declared as opaque placeholders only to fix
    // the vtable layout so `SetDefaultEndpoint` (slot 13) is at the right offset.
    unsafe fn GetMixFormat(&self) -> HRESULT;
    unsafe fn GetDeviceFormat(&self) -> HRESULT;
    unsafe fn ResetDeviceFormat(&self) -> HRESULT;
    unsafe fn SetDeviceFormat(&self) -> HRESULT;
    unsafe fn GetProcessingPeriod(&self) -> HRESULT;
    unsafe fn SetProcessingPeriod(&self) -> HRESULT;
    unsafe fn GetShareMode(&self) -> HRESULT;
    unsafe fn SetShareMode(&self) -> HRESULT;
    unsafe fn GetPropertyValue(&self) -> HRESULT;
    unsafe fn SetPropertyValue(&self) -> HRESULT;
    unsafe fn SetDefaultEndpoint(&self, device_id: PCWSTR, role: i32) -> HRESULT;
    unsafe fn SetEndpointVisibility(&self) -> HRESULT;
}

/// Active render endpoints as `(device_id, friendly_name)`. The id is the WASAPI
/// endpoint id used by [`set_default_render_endpoint`].
pub fn enumerate_render_endpoints() -> Vec<(String, String)> {
    enumerate_with_ids().unwrap_or_default()
}

fn enumerate_with_ids() -> windows::core::Result<Vec<(String, String)>> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let coll = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let count = coll.GetCount()?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let entry = (|| -> windows::core::Result<(String, String)> {
                let dev = coll.Item(i)?;
                let id = dev.GetId()?;
                let id_s = id.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id.0 as *const _));
                let store = dev.OpenPropertyStore(STGM_READ)?;
                let prop = store.GetValue(&PKEY_Device_FriendlyName)?;
                let name = propvariant_str(&prop);
                Ok((id_s, name))
            })();
            if let Ok(e) = entry
                && !e.0.is_empty()
            {
                out.push(e);
            }
        }
        Ok(out)
    }
}

/// The current default render endpoint id (eConsole role), if any.
pub fn default_render_id() -> Option<String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let dev = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id = dev.GetId().ok()?;
        let s = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0 as *const _));
        if s.is_empty() { None } else { Some(s) }
    }
}

/// Make `id` the system default playback device for all roles. Best-effort.
pub fn set_default_render_endpoint(id: &str) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let policy: IPolicyConfig = match CoCreateInstance(&CPOLICYCONFIG_CLIENT, None, CLSCTX_ALL)
        {
            Ok(p) => p,
            Err(_) => return false,
        };
        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        let pid = PCWSTR(wide.as_ptr());
        let mut ok = true;
        for role in 0..3 {
            if policy.SetDefaultEndpoint(pid, role).is_err() {
                ok = false;
            }
        }
        ok
    }
}

/// The per-endpoint APO attach script, compiled into the daemon so dynamic
/// attach has no runtime file dependency (the installer ships it too).
const ATTACH_ENDPOINT_PS: &str = include_str!("../../../../contrib/windows/attach-endpoint.ps1");

/// The registry GUID for a render endpoint, parsed from its WASAPI id
/// (`{0.0.0.00000000}.{<guid>}` → `{<guid>}`).
pub fn endpoint_guid(wasapi_id: &str) -> Option<&str> {
    wasapi_id.rsplit('.').next().filter(|s| s.starts_with('{'))
}

/// Dynamically attach the Resonance APO to one render endpoint (by registry
/// GUID). Runs the bundled per-endpoint script via PowerShell — it reuses the
/// installer's proven ownership/slot recipe, is idempotent (a "already attached"
/// fast path), and never restarts audiosrv. Returns the script's status line.
/// Lets a hot-plugged DAC / Bluetooth device get the EQ without re-running the
/// installer (the daemon calls this for every newly-seen endpoint).
pub fn attach_apo_endpoint(guid: &str) -> String {
    use std::io::Write;
    let script = std::env::temp_dir().join("resonance_attach_endpoint.ps1");
    if let Ok(mut f) = std::fs::File::create(&script) {
        let _ = f.write_all(ATTACH_ENDPOINT_PS.as_bytes());
    }
    match std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .env("RESONANCE_ATTACH_GUID", guid)
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(e) => format!("attach spawn failed: {e}"),
    }
}

/// Set every active virtual-cable endpoint (render + capture) to `target_rate`
/// so the cable performs no internal sample-rate conversion. Best-effort: any
/// failure (no cable, format locked while streaming) is ignored.
pub fn match_cable_endpoints_to(target_rate: u32) {
    let _ = set_rates_where(target_rate, is_cable_name);
}

/// Set the shared-mode rate of every active endpoint whose friendly name
/// contains `name_contains` (case-insensitive) — used to pin the real output
/// device to the chain rate so it never downsamples below it.
pub fn set_endpoint_rate(name_contains: &str, target_rate: u32) {
    let want = name_contains.to_lowercase();
    let _ = set_rates_where(target_rate, |n| n.contains(&want));
}

/// Friendly-name substrings that identify a virtual-cable / loopback render
/// endpoint we can safely loopback-capture: VB-CABLE's own names, Voicemeeter,
/// or our renamed "Resonance EQ" device. ("vb-cable" is subsumed by "cable".)
/// Compared against an already-lowercased name.
pub const CABLE_HINTS: &[&str] = &["cable", "vb-audio", "voicemeeter", "resonance eq"];

/// Whether a (lowercased) friendly name is one of our virtual-cable endpoints.
pub fn is_cable_name(n: &str) -> bool {
    CABLE_HINTS.iter().any(|h| n.contains(h))
}

/// Current shared-mode sample rate of the endpoint whose friendly name contains
/// `name` (case-insensitive). This is the rate cpal streams MUST be opened at to
/// avoid WASAPI's internal shared-mode resampler — cpal's `default_*_config`
/// can report a different supported rate (e.g. 48000 when the mix is 44100),
/// which silently resamples and rolls off the highs.
pub fn endpoint_rate_by_name(name: &str) -> Option<u32> {
    unsafe { rate_by_name(name) }.ok().flatten()
}

fn rate_by_name(name: &str) -> windows::core::Result<Option<u32>> {
    let want = name.to_lowercase();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let policy: IPolicyConfigVista =
            CoCreateInstance(&CPOLICYCONFIG_VISTA_CLIENT, None, CLSCTX_ALL)?;
        let coll = enumerator.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE)?;
        let count = coll.GetCount()?;
        for i in 0..count {
            let dev = coll.Item(i)?;
            let store = dev.OpenPropertyStore(STGM_READ)?;
            let prop = store.GetValue(&PKEY_Device_FriendlyName)?;
            let friendly = propvariant_str(&prop).to_lowercase();
            if !friendly.contains(&want) {
                continue;
            }
            let id = dev.GetId()?;
            let mut pfmt: *mut WAVEFORMATEXTENSIBLE = std::ptr::null_mut();
            let mut rate = None;
            if policy.GetDeviceFormat(PCWSTR(id.0), 0, &mut pfmt).is_ok() && !pfmt.is_null() {
                rate = Some((*pfmt).Format.nSamplesPerSec);
                CoTaskMemFree(Some(pfmt as *const _));
            }
            CoTaskMemFree(Some(id.0 as *const _));
            return Ok(rate);
        }
        Ok(None)
    }
}

fn set_rates_where(target_rate: u32, want: impl Fn(&str) -> bool) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let policy: IPolicyConfigVista =
            CoCreateInstance(&CPOLICYCONFIG_VISTA_CLIENT, None, CLSCTX_ALL)?;
        let coll = enumerator.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE)?;
        let count = coll.GetCount()?;
        for i in 0..count {
            let dev = coll.Item(i)?;
            let store = dev.OpenPropertyStore(STGM_READ)?;
            let prop = store.GetValue(&PKEY_Device_FriendlyName)?;
            let name = propvariant_str(&prop).to_lowercase();
            if !want(&name) {
                continue;
            }
            let id = dev.GetId()?; // CoTaskMem PWSTR
            let idw = PCWSTR(id.0);

            let mut pfmt: *mut WAVEFORMATEXTENSIBLE = std::ptr::null_mut();
            // FALSE -> current format (not the OEM default).
            if policy.GetDeviceFormat(idw, 0, &mut pfmt).is_ok() && !pfmt.is_null() {
                if (*pfmt).Format.nSamplesPerSec != target_rate {
                    (*pfmt).Format.nSamplesPerSec = target_rate;
                    let ch = (*pfmt).Format.nChannels as u32;
                    let bits = (*pfmt).Format.wBitsPerSample as u32;
                    let block = (ch * bits / 8) as u16;
                    (*pfmt).Format.nBlockAlign = block;
                    (*pfmt).Format.nAvgBytesPerSec = target_rate * block as u32;
                    let _ = policy.SetDeviceFormat(idw, pfmt, std::ptr::null());
                }
                CoTaskMemFree(Some(pfmt as *const _));
            }
            CoTaskMemFree(Some(id.0 as *const _));
        }
        Ok(())
    }
}
