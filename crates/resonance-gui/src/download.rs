//! squig.link measurement downloader.
//!
//! Fetches the federation site list (`squig.link/squigsites.json`), then each
//! enabled database's `data/phone_book.json`, flattening every model into a
//! searchable catalog. A chosen model's `<file> L.txt` / `R.txt` curves are
//! fetched on demand and parsed into [`RefCurve`]s. Everything runs on a
//! background thread (UI never blocks); results arrive as [`DlEvent`]s. Data is
//! fetched from the origin at runtime (never bundled) and disk-cached under
//! `curve_cache_dir()` so it works offline; "Refresh" forces a re-fetch.

use resonance_ipc::curve::RefCurve;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender};

const SITES_URL: &str = "https://squig.link/squigsites.json";
const USER_AGENT: &str = concat!("resonance-gui/", env!("CARGO_PKG_VERSION"));

/// Databases enabled out of the box (by squig username) so the browser has
/// content on first open without fetching all ~118 sites' indexes.
const DEFAULT_ENABLED: &[&str] = &["dhrme", "precog", "dl", "kr0mka", "harpo", "achoreviews"];

// ── squig.link JSON shapes ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct SiteEntry {
    username: String,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "urlType")]
    url_type: String,
    #[serde(default)]
    dbs: Vec<DbEntry>,
}

#[derive(Deserialize, Clone)]
struct DbEntry {
    #[serde(default, rename = "type")]
    db_type: String,
    #[serde(default)]
    folder: String,
}

#[derive(Deserialize)]
struct Brand {
    #[serde(default)]
    name: String,
    #[serde(default)]
    phones: Vec<Phone>,
}

#[derive(Deserialize)]
struct Phone {
    #[serde(default)]
    name: String,
    #[serde(default)]
    file: StrOrVec,
    #[serde(default)]
    suffix: StrOrVec,
}

/// A phone_book `file`/`suffix` field is a single string or an array of them.
#[derive(Deserialize, Default)]
#[serde(untagged)]
enum StrOrVec {
    Many(Vec<String>),
    One(String),
    #[default]
    None,
}

impl StrOrVec {
    fn as_vec(&self) -> Vec<String> {
        match self {
            StrOrVec::Many(v) => v.clone(),
            StrOrVec::One(s) => vec![s.clone()],
            StrOrVec::None => Vec::new(),
        }
    }
}

// ── UI-facing catalog ───────────────────────────────────────────────────────

/// One selectable measurement in the flattened catalog.
#[derive(Clone)]
pub(crate) struct ModelEntry {
    /// Owning database (for grouping/display).
    pub source: String,
    /// "Brand Model [variant]".
    pub display: String,
    /// Base file name (channel files are `<file> L.txt` / `R.txt`).
    pub file: String,
    /// Full DB root incl. folder, ending in `/` (so `+ "data/<file> L.txt"`).
    pub base_url: String,
    /// Rig/category (IEMs, Headphones, …).
    pub kind: String,
}

/// Per-source toggle metadata for the browser's source list.
#[derive(Clone)]
pub(crate) struct SourceMeta {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub loaded: bool,
}

/// Snapshot the UI renders: source toggles + the flattened model list.
#[derive(Clone, Default)]
pub(crate) struct Catalog {
    pub sources: Vec<SourceMeta>,
    pub models: Vec<ModelEntry>,
}

/// A fetched measurement, ready to install as the active reference measurement.
pub(crate) struct Fetched {
    pub name: String,
    /// In-ear rig (drives AutoEQ's smoothing profile).
    pub iem: bool,
    pub left: RefCurve,
    pub right: Option<RefCurve>,
}

pub(crate) enum DlCmd {
    /// Build the catalog from cache (or fetch if absent).
    Init,
    /// Force a fresh network fetch of everything enabled.
    Refresh,
    /// Enable/disable a source by id; loads it on first enable.
    SetEnabled(String, bool),
    /// Enable or disable every source at once.
    SetAll(bool),
    /// Restore the built-in default source selection.
    SetDefault,
    /// Fetch + parse a model's channel curves.
    Fetch(ModelEntry),
}

pub(crate) enum DlEvent {
    Catalog(Catalog),
    Status(String),
    Busy(bool),
    Fetched(Fetched),
}

/// Worker-side source state (owns the truth; the UI renders snapshots).
struct Source {
    id: String,
    name: String,
    base: String, // e.g. https://dhrme.squig.link/
    dbs: Vec<DbEntry>,
    enabled: bool,
    loaded: bool,
    models: Vec<ModelEntry>,
}

/// Spawn the download worker. Returns the command sender + event receiver.
pub(crate) fn spawn(ctx: eframe::egui::Context) -> (Sender<DlCmd>, Receiver<DlEvent>) {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<DlCmd>();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel::<DlEvent>();
    std::thread::Builder::new()
        .name("resonance-curves".into())
        .spawn(move || worker(cmd_rx, ev_tx, ctx))
        .expect("spawn curve downloader");
    (cmd_tx, ev_rx)
}

fn worker(rx: Receiver<DlCmd>, tx: Sender<DlEvent>, ctx: eframe::egui::Context) {
    let mut sites: Vec<Source> = Vec::new();
    while let Ok(cmd) = rx.recv() {
        let _ = tx.send(DlEvent::Busy(true));
        ctx.request_repaint();
        match cmd {
            DlCmd::Init | DlCmd::Refresh => {
                let force = matches!(cmd, DlCmd::Refresh);
                if sites.is_empty() || force {
                    match fetch_sites(force) {
                        Ok(s) => sites = s,
                        Err(e) => {
                            let _ = tx.send(DlEvent::Status(format!("site list: {e}")));
                        }
                    }
                }
                let idx: Vec<usize> = (0..sites.len())
                    .filter(|&i| sites[i].enabled && (!sites[i].loaded || force))
                    .collect();
                load_sources_parallel(&mut sites, &idx, force, &tx, &ctx);
            }
            DlCmd::SetEnabled(id, on) => {
                if let Some(src) = sites.iter_mut().find(|s| s.id == id) {
                    src.enabled = on;
                    if on && !src.loaded {
                        load_source(src, false);
                    }
                }
                save_enabled(&sites);
                let _ = tx.send(DlEvent::Catalog(snapshot(&sites)));
            }
            DlCmd::SetAll(on) => {
                // Flip every flag and report it IMMEDIATELY so the toggles react
                // at once; then load all newly-enabled indexes in parallel.
                for src in sites.iter_mut() {
                    src.enabled = on;
                }
                save_enabled(&sites);
                let _ = tx.send(DlEvent::Catalog(snapshot(&sites)));
                ctx.request_repaint();
                if on {
                    let idx: Vec<usize> = (0..sites.len()).filter(|&i| !sites[i].loaded).collect();
                    load_sources_parallel(&mut sites, &idx, false, &tx, &ctx);
                }
            }
            DlCmd::SetDefault => {
                for src in sites.iter_mut() {
                    src.enabled = DEFAULT_ENABLED.contains(&src.id.as_str());
                }
                save_enabled(&sites);
                let _ = tx.send(DlEvent::Catalog(snapshot(&sites)));
                ctx.request_repaint();
                let idx: Vec<usize> = (0..sites.len())
                    .filter(|&i| sites[i].enabled && !sites[i].loaded)
                    .collect();
                load_sources_parallel(&mut sites, &idx, false, &tx, &ctx);
            }
            DlCmd::Fetch(m) => match fetch_model(&m) {
                Ok(f) => {
                    let _ = tx.send(DlEvent::Status(format!("loaded {}", f.name)));
                    let _ = tx.send(DlEvent::Fetched(f));
                }
                Err(e) => {
                    let _ = tx.send(DlEvent::Status(format!("fetch failed: {e}")));
                }
            },
        }
        let _ = tx.send(DlEvent::Busy(false));
        ctx.request_repaint();
    }
}

fn snapshot(sites: &[Source]) -> Catalog {
    let mut models = Vec::new();
    for s in sites.iter().filter(|s| s.enabled) {
        models.extend(s.models.iter().cloned());
    }
    Catalog {
        sources: sites
            .iter()
            .map(|s| SourceMeta {
                id: s.id.clone(),
                name: s.name.clone(),
                enabled: s.enabled,
                loaded: s.loaded,
            })
            .collect(),
        models,
    }
}

fn fetch_sites(force: bool) -> Result<Vec<Source>, String> {
    let body = cached_get(SITES_URL, "squigsites.json", force)?;
    let entries: Vec<SiteEntry> = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    // Remembered selection (if the user has ever changed it) overrides the
    // built-in defaults; a present-but-empty file means "all off".
    let saved = load_enabled();
    let mut out = Vec::new();
    for e in entries {
        // Resolve the database base URL from the federation's url type.
        let base = match e.url_type.as_str() {
            "root" => "https://squig.link/".to_string(),
            "altDomain" if e.username == "crinacle" => "https://graph.hangout.audio/".to_string(),
            "altDomain" => continue, // unknown off-domain host
            _ => format!("https://{}.squig.link/", e.username), // subdomain / labFolder / ""
        };
        let name = if e.name.is_empty() {
            e.username.clone()
        } else {
            e.name.clone()
        };
        let enabled = match &saved {
            Some(set) => set.contains(&e.username),
            None => DEFAULT_ENABLED.contains(&e.username.as_str()),
        };
        out.push(Source {
            enabled,
            id: e.username,
            name,
            base,
            dbs: e.dbs,
            loaded: false,
            models: Vec::new(),
        });
    }
    Ok(out)
}

/// File that remembers which sources are enabled (one username per line).
fn enabled_file() -> std::path::PathBuf {
    resonance_ipc::paths::config_dir().join("curve_sources.txt")
}

/// `Some(set)` once the user has changed the selection; `None` if untouched.
fn load_enabled() -> Option<HashSet<String>> {
    let body = std::fs::read_to_string(enabled_file()).ok()?;
    Some(
        body.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

fn save_enabled(sites: &[Source]) {
    let dir = resonance_ipc::paths::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let body: String = sites
        .iter()
        .filter(|s| s.enabled)
        .map(|s| format!("{}\n", s.id))
        .collect();
    let _ = std::fs::write(enabled_file(), body);
}

/// Full DB root for a source's db entry (base + folder, ending in `/`).
fn db_base_url(src: &Source, db: &DbEntry) -> String {
    let folder = if db.folder.is_empty() {
        "/".to_string()
    } else {
        db.folder.clone()
    };
    let mut base_url = format!("{}{}", src.base.trim_end_matches('/'), folder);
    if !base_url.ends_with('/') {
        base_url.push('/');
    }
    base_url
}

/// Parse a phone_book.json body into model entries appended to `out`.
fn parse_phone_book(
    body: &str,
    base_url: &str,
    kind: &str,
    src_name: &str,
    out: &mut Vec<ModelEntry>,
) {
    let Ok(brands) = serde_json::from_str::<Vec<Brand>>(body) else {
        return;
    };
    for b in brands {
        for p in &b.phones {
            let files = p.file.as_vec();
            let sufs = p.suffix.as_vec();
            // No `file` field → the model name is itself the file base name.
            let variants: Vec<(String, String)> = if files.is_empty() {
                vec![(p.name.clone(), String::new())]
            } else {
                files
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.clone(), sufs.get(i).cloned().unwrap_or_default()))
                    .collect()
            };
            for (file, suffix) in variants {
                let display = if suffix.is_empty() {
                    format!("{} {}", b.name, p.name)
                } else {
                    format!("{} {} {}", b.name, p.name, suffix)
                };
                out.push(ModelEntry {
                    source: src_name.to_string(),
                    display: display.trim().to_string(),
                    file,
                    base_url: base_url.to_string(),
                    kind: kind.to_string(),
                });
            }
        }
    }
}

fn load_source(src: &mut Source, force: bool) {
    src.models.clear();
    for db in &src.dbs {
        let base_url = db_base_url(src, db);
        let url = format!("{base_url}data/phone_book.json");
        let key = format!("pb_{}_{}.json", src.id, sanitize(&db.folder));
        if let Ok(body) = cached_get(&url, &key, force) {
            parse_phone_book(&body, &base_url, &db.db_type, &src.name, &mut src.models);
        }
    }
    src.loaded = true;
}

/// Fetch many `(url, cache_key)` bodies concurrently (8-way) — squig indexes are
/// network-bound, so parallel GETs are far faster than one-at-a-time.
fn parallel_bodies(tasks: &[(String, String)], force: bool) -> Vec<Option<String>> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<String>>> = (0..tasks.len()).map(|_| Mutex::new(None)).collect();
    let n_threads = tasks.len().clamp(1, 8);
    std::thread::scope(|s| {
        for _ in 0..n_threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= tasks.len() {
                        break;
                    }
                    let (url, key) = &tasks[i];
                    let body = cached_get(url, key, force).ok();
                    *results[i].lock().unwrap() = body;
                }
            });
        }
    });
    results
        .into_iter()
        .map(|m| m.into_inner().unwrap())
        .collect()
}

/// Load several sources' indexes in parallel, then emit one snapshot.
fn load_sources_parallel(
    sites: &mut [Source],
    indices: &[usize],
    force: bool,
    tx: &Sender<DlEvent>,
    ctx: &eframe::egui::Context,
) {
    struct Task {
        si: usize,
        base_url: String,
        kind: String,
    }
    let mut tasks = Vec::new();
    let mut urls = Vec::new();
    for &si in indices {
        sites[si].models.clear();
        for db in &sites[si].dbs {
            let base_url = db_base_url(&sites[si], db);
            let url = format!("{base_url}data/phone_book.json");
            let key = format!("pb_{}_{}.json", sites[si].id, sanitize(&db.folder));
            urls.push((url, key));
            tasks.push(Task {
                si,
                base_url,
                kind: db.db_type.clone(),
            });
        }
    }
    let bodies = parallel_bodies(&urls, force);
    for (t, body) in tasks.iter().zip(bodies) {
        if let Some(body) = body {
            let name = sites[t.si].name.clone();
            parse_phone_book(&body, &t.base_url, &t.kind, &name, &mut sites[t.si].models);
        }
    }
    for &si in indices {
        sites[si].loaded = true;
    }
    let _ = tx.send(DlEvent::Catalog(snapshot(sites)));
    ctx.request_repaint();
}

fn fetch_model(m: &ModelEntry) -> Result<Fetched, String> {
    // squig file names contain spaces, parens, commas… — percent-encode the
    // file segment so the URL is valid (raw spaces make the request fail).
    let l_url = format!("{}data/{}", m.base_url, pct(&format!("{} L.txt", m.file)));
    let r_url = format!("{}data/{}", m.base_url, pct(&format!("{} R.txt", m.file)));
    let mono_url = format!("{}data/{}", m.base_url, pct(&format!("{}.txt", m.file)));

    // Stereo first; fall back to a mono file named `<file>.txt`.
    let left = match cached_get(&l_url, &cache_key(&l_url), false) {
        Ok(t) => RefCurve::parse(&t),
        Err(_) => cached_get(&mono_url, &cache_key(&mono_url), false)
            .ok()
            .and_then(|t| RefCurve::parse(&t)),
    }
    .ok_or_else(|| "no parseable measurement file".to_string())?;

    let right = cached_get(&r_url, &cache_key(&r_url), false)
        .ok()
        .and_then(|t| RefCurve::parse(&t));

    let k = m.kind.to_ascii_lowercase();
    let iem = k.contains("iem") || k.contains("earbud") || k.contains("711");
    Ok(Fetched {
        name: m.display.clone(),
        iem,
        left,
        right,
    })
}

// ── HTTP + cache ────────────────────────────────────────────────────────────

fn http_get(url: &str) -> Result<String, String> {
    ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
}

/// GET with a disk cache under `curve_cache_dir()`. Unless `force`, a cached copy
/// short-circuits the network; on a network error a stale cache is used as a
/// fallback so the browser keeps working offline.
fn cached_get(url: &str, key: &str, force: bool) -> Result<String, String> {
    let dir = resonance_ipc::paths::curve_cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(key);
    if !force {
        if let Ok(s) = std::fs::read_to_string(&path) {
            return Ok(s);
        }
    }
    match http_get(url) {
        Ok(body) => {
            let _ = std::fs::write(&path, &body);
            Ok(body)
        }
        Err(e) => std::fs::read_to_string(&path).map_err(|_| e),
    }
}

fn cache_key(url: &str) -> String {
    sanitize(url.trim_start_matches("https://"))
}

/// Percent-encode a URL path segment, leaving only RFC 3986 unreserved chars.
/// squig file names carry spaces, parens, commas and ampersands.
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Filesystem-safe cache key from an arbitrary string.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}
