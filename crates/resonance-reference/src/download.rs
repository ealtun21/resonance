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
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

/// UI-framework-agnostic "wake the event loop" callback. The downloader runs on
/// a background thread and calls this whenever it emits a [`DlEvent`], so an
/// event-driven client (the egui GUI) repaints promptly; a polling client (the
/// TUI) passes a no-op. Replaces the former hard dependency on
/// `egui::Context::request_repaint`.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

const SITES_URL: &str = "https://squig.link/squigsites.json";
const USER_AGENT: &str = concat!("resonance/", env!("CARGO_PKG_VERSION"));

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

/// A `phone_book` `file`/`suffix` field is a single string or an array of them.
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
pub struct ModelEntry {
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

/// One selectable *target curve* in the catalog (parsed from a site's
/// `config.js` `targets` array — there's no JSON index for targets the way
/// `phone_book.json` lists measurements).
#[derive(Clone)]
pub struct TargetEntry {
    /// Owning database (for grouping/display).
    pub source: String,
    /// File basename, also the display name (e.g. "Harman 2019", "JM-1").
    pub name: String,
    /// Full DB root incl. folder, ending in `/`.
    pub base_url: String,
}

/// Per-source toggle metadata for the browser's source list.
#[derive(Clone)]
pub struct SourceMeta {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub loaded: bool,
}

/// Snapshot the UI renders: source toggles + the flattened model + target lists.
#[derive(Clone, Default)]
pub struct Catalog {
    pub sources: Vec<SourceMeta>,
    pub models: Vec<ModelEntry>,
    pub targets: Vec<TargetEntry>,
}

/// A fetched measurement, ready to install as the active reference measurement.
pub struct Fetched {
    pub name: String,
    /// In-ear rig (drives `AutoEQ`'s smoothing profile).
    pub iem: bool,
    pub left: RefCurve,
    pub right: Option<RefCurve>,
}

pub enum DlCmd {
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
    /// Fetch + parse a model's channel curves (as the active measurement).
    Fetch(ModelEntry),
    /// Fetch + parse a target curve to add to the target library.
    FetchTarget(TargetEntry),
    /// Fetch a measurement and add it (L+R averaged) to the target library —
    /// "use this headphone's response as the target to EQ toward".
    AddMeasurementTarget(ModelEntry),
}

pub enum DlEvent {
    Catalog(Catalog),
    Status(String),
    Busy(bool),
    Fetched(Fetched),
    /// A curve to add to the target library (from `FetchTarget` /
    /// `AddMeasurementTarget`).
    FetchedTarget {
        name: String,
        curve: RefCurve,
    },
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
    targets: Vec<TargetEntry>,
}

/// Spawn the download worker. Returns the command sender + event receiver.
///
/// # Panics
///
/// Panics if the OS refuses to spawn the background worker thread.
pub fn spawn(ctx: Wake) -> (Sender<DlCmd>, Receiver<DlEvent>) {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<DlCmd>();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel::<DlEvent>();
    std::thread::Builder::new()
        .name("resonance-curves".into())
        .spawn(move || worker(cmd_rx, ev_tx, &ctx))
        .expect("spawn curve downloader");
    (cmd_tx, ev_rx)
}

/// How long a cached catalog index (`squigsites.json`, each db's
/// `phone_book.json` / `config.js`) is served before `DlCmd::Init` silently
/// re-fetches it. Catalogs change rarely (new measurements/targets land
/// occasionally), so a week balances freshness against pointless network hits on
/// every dialog-open. Manual "Refresh" ignores this and re-fetches everything.
const CATALOG_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60 * 24 * 7);

// rx/tx are moved into and owned for the lifetime of the spawned worker thread,
// so they must be owned, not borrowed.
#[allow(clippy::needless_pass_by_value)]
fn worker(rx: Receiver<DlCmd>, tx: Sender<DlEvent>, ctx: &Wake) {
    let mut sites: Vec<Source> = Vec::new();
    while let Ok(cmd) = rx.recv() {
        let _ = tx.send(DlEvent::Busy(true));
        ctx();
        match cmd {
            DlCmd::Init => {
                // "Remember + keep updated": the first warm of the session loads
                // the catalog from the on-disk cache and publishes it IMMEDIATELY
                // (instant, even if stale), then does a second background pass that
                // re-fetches only index files older than CATALOG_TTL and republishes
                // — so the dialogs open populated and silently freshen. Subsequent
                // opens within the session just re-publish the in-memory catalog
                // (no disk re-read, no network); manual Refresh forces a re-fetch.
                if sites.is_empty() {
                    match fetch_sites(Freshness::CacheFirst) {
                        Ok(s) => sites = s,
                        Err(e) => {
                            let _ = tx.send(DlEvent::Status(format!("site list: {e}")));
                        }
                    }
                    let idx: Vec<usize> = (0..sites.len()).filter(|&i| sites[i].enabled).collect();
                    // Phase 1 — instant snapshot from cache.
                    load_sources_parallel(&mut sites, &idx, Freshness::CacheFirst, &tx, ctx);
                    // Phase 2 — freshen stale indexes in the background, republish.
                    load_sources_parallel(
                        &mut sites,
                        &idx,
                        Freshness::IfStale(CATALOG_TTL),
                        &tx,
                        ctx,
                    );
                } else {
                    let _ = tx.send(DlEvent::Catalog(snapshot(&sites)));
                }
            }
            DlCmd::Refresh => {
                // Manual refresh: force a full network re-fetch of everything.
                match fetch_sites(Freshness::Refresh) {
                    Ok(s) => sites = s,
                    Err(e) => {
                        let _ = tx.send(DlEvent::Status(format!("site list: {e}")));
                    }
                }
                let idx: Vec<usize> = (0..sites.len()).filter(|&i| sites[i].enabled).collect();
                load_sources_parallel(&mut sites, &idx, Freshness::Refresh, &tx, ctx);
            }
            DlCmd::SetEnabled(id, on) => {
                if let Some(src) = sites.iter_mut().find(|s| s.id == id) {
                    src.enabled = on;
                    // Snappy first-load from cache; freshness is handled by the
                    // periodic Init (IfStale) / manual Refresh passes.
                    if on && !src.loaded {
                        load_source(src, Freshness::CacheFirst);
                    }
                }
                save_enabled(&sites);
                let _ = tx.send(DlEvent::Catalog(snapshot(&sites)));
            }
            DlCmd::SetAll(on) => {
                // Flip every flag and report it IMMEDIATELY so the toggles react
                // at once; then load all newly-enabled indexes in parallel.
                for src in &mut sites {
                    src.enabled = on;
                }
                save_enabled(&sites);
                let _ = tx.send(DlEvent::Catalog(snapshot(&sites)));
                ctx();
                if on {
                    let idx: Vec<usize> = (0..sites.len()).filter(|&i| !sites[i].loaded).collect();
                    load_sources_parallel(&mut sites, &idx, Freshness::CacheFirst, &tx, ctx);
                }
            }
            DlCmd::SetDefault => {
                for src in &mut sites {
                    src.enabled = DEFAULT_ENABLED.contains(&src.id.as_str());
                }
                save_enabled(&sites);
                let _ = tx.send(DlEvent::Catalog(snapshot(&sites)));
                ctx();
                let idx: Vec<usize> = (0..sites.len())
                    .filter(|&i| sites[i].enabled && !sites[i].loaded)
                    .collect();
                load_sources_parallel(&mut sites, &idx, Freshness::CacheFirst, &tx, ctx);
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
            DlCmd::FetchTarget(t) => match fetch_target(&t) {
                Ok((name, curve)) => {
                    let _ = tx.send(DlEvent::Status(format!("added target: {name}")));
                    let _ = tx.send(DlEvent::FetchedTarget { name, curve });
                }
                Err(e) => {
                    let _ = tx.send(DlEvent::Status(format!("target fetch failed: {e}")));
                }
            },
            DlCmd::AddMeasurementTarget(m) => match fetch_measurement_target(&m) {
                Ok((name, curve)) => {
                    let _ = tx.send(DlEvent::Status(format!("added target: {name}")));
                    let _ = tx.send(DlEvent::FetchedTarget { name, curve });
                }
                Err(e) => {
                    let _ = tx.send(DlEvent::Status(format!("target fetch failed: {e}")));
                }
            },
        }
        let _ = tx.send(DlEvent::Busy(false));
        ctx();
    }
}

fn snapshot(sites: &[Source]) -> Catalog {
    let mut models = Vec::new();
    let mut targets = Vec::new();
    let mut seen_targets = HashSet::new();
    for s in sites.iter().filter(|s| s.enabled) {
        models.extend(s.models.iter().cloned());
        // Target names are per-site and overlap across sites ("Harman 2019"
        // exists on many); dedup by name so the picker isn't full of dupes.
        for t in &s.targets {
            if seen_targets.insert(t.name.to_lowercase()) {
                targets.push(t.clone());
            }
        }
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
        targets,
    }
}

fn fetch_sites(fresh: Freshness) -> Result<Vec<Source>, String> {
    let body = cached_get(SITES_URL, "squigsites.json", fresh)?;
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
            targets: Vec::new(),
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
    let body = sites
        .iter()
        .filter(|s| s.enabled)
        .fold(String::new(), |mut acc, s| {
            let _ = writeln!(acc, "{}", s.id);
            acc
        });
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

/// Parse a `phone_book.json` body into model entries appended to `out`.
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

fn load_source(src: &mut Source, fresh: Freshness) {
    src.models.clear();
    src.targets.clear();
    for db in &src.dbs {
        let base_url = db_base_url(src, db);
        let folder_key = sanitize(&db.folder);
        let url = format!("{base_url}data/phone_book.json");
        let key = format!("pb_{}_{}.json", src.id, folder_key);
        if let Ok(body) = cached_get(&url, &key, fresh) {
            parse_phone_book(&body, &base_url, &db.db_type, &src.name, &mut src.models);
        }
        let t = config_targets(&src.name, &base_url, fresh);
        src.targets.extend(t);
    }
    src.loaded = true;
}

/// Fetch a db's `config.js` (or a fork's `assets/js/config.js`) and parse its
/// `targets` array into entries. Targets have no JSON index — they live only in
/// the site's config script — so we extract the file basenames from there.
fn config_targets(src_name: &str, base_url: &str, fresh: Freshness) -> Vec<TargetEntry> {
    for sub in ["config.js", "assets/js/config.js"] {
        let url = format!("{base_url}{sub}");
        // Key off the URL (the canonical cache key used everywhere) so the body
        // is shared with the parallel-load path, not re-fetched under a second
        // name.
        let key = cache_key(&url);
        if let Ok(body) = cached_get(&url, &key, fresh) {
            let names = parse_target_names(&body);
            if !names.is_empty() {
                return names
                    .into_iter()
                    .map(|n| TargetEntry {
                        source: src_name.to_string(),
                        name: n,
                        base_url: base_url.to_string(),
                    })
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Fetch many `(url, cache_key)` bodies concurrently (8-way) — squig indexes are
/// network-bound, so parallel GETs are far faster than one-at-a-time.
fn parallel_bodies(tasks: &[(String, String)], fresh: Freshness) -> Vec<Option<String>> {
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
                    let body = cached_get(url, key, fresh).ok();
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

/// Load several sources' indexes in parallel, then emit one snapshot. Each db
/// contributes two parallel GETs: `phone_book.json` (measurements) and
/// `config.js` (targets). Sources whose `config.js` yields no targets fall back
/// to the fork `assets/js/config.js` path sequentially (rare; small set).
fn load_sources_parallel(
    sites: &mut [Source],
    indices: &[usize],
    fresh: Freshness,
    tx: &Sender<DlEvent>,
    ctx: &Wake,
) {
    #[derive(Clone, Copy)]
    enum What {
        PhoneBook,
        Config,
    }
    struct Task {
        si: usize,
        base_url: String,
        kind: String,
        what: What,
    }
    let mut tasks = Vec::new();
    let mut urls = Vec::new();
    for &si in indices {
        sites[si].models.clear();
        sites[si].targets.clear();
        for db in &sites[si].dbs {
            let base_url = db_base_url(&sites[si], db);
            let folder_key = sanitize(&db.folder);
            urls.push((
                format!("{base_url}data/phone_book.json"),
                format!("pb_{}_{}.json", sites[si].id, folder_key),
            ));
            tasks.push(Task {
                si,
                base_url: base_url.clone(),
                kind: db.db_type.clone(),
                what: What::PhoneBook,
            });
            let cfg_url = format!("{base_url}config.js");
            let cfg_key = cache_key(&cfg_url);
            urls.push((cfg_url, cfg_key));
            tasks.push(Task {
                si,
                base_url,
                kind: String::new(),
                what: What::Config,
            });
        }
    }
    let bodies = parallel_bodies(&urls, fresh);
    for (t, body) in tasks.iter().zip(bodies) {
        let Some(body) = body else { continue };
        match t.what {
            What::PhoneBook => {
                let name = sites[t.si].name.clone();
                parse_phone_book(&body, &t.base_url, &t.kind, &name, &mut sites[t.si].models);
            }
            What::Config => {
                let name = sites[t.si].name.clone();
                for n in parse_target_names(&body) {
                    sites[t.si].targets.push(TargetEntry {
                        source: name.clone(),
                        name: n,
                        base_url: t.base_url.clone(),
                    });
                }
            }
        }
    }
    // Fork fallback for sources whose primary config.js yielded nothing. The
    // root config.js was already fetched above and is cached under the same key
    // (config_targets keys off the URL), so this only adds the assets/js probe.
    for &si in indices {
        if !sites[si].targets.is_empty() {
            continue;
        }
        let name = sites[si].name.clone();
        let bases: Vec<String> = sites[si]
            .dbs
            .iter()
            .map(|db| db_base_url(&sites[si], db))
            .collect();
        for base_url in bases {
            let t = config_targets(&name, &base_url, fresh);
            sites[si].targets.extend(t);
        }
    }
    for &si in indices {
        sites[si].loaded = true;
    }
    let _ = tx.send(DlEvent::Catalog(snapshot(sites)));
    ctx();
}

fn fetch_model(m: &ModelEntry) -> Result<Fetched, String> {
    // squig file names contain spaces, parens, commas… — percent-encode the
    // file segment so the URL is valid (raw spaces make the request fail).
    let l_url = format!("{}data/{}", m.base_url, pct(&format!("{} L.txt", m.file)));
    let r_url = format!("{}data/{}", m.base_url, pct(&format!("{} R.txt", m.file)));
    let mono_url = format!("{}data/{}", m.base_url, pct(&format!("{}.txt", m.file)));

    // Stereo first; fall back to a mono file named `<file>.txt`. Curve files are
    // immutable, so caching them forever (CacheFirst) is correct.
    let left = match cached_get(&l_url, &cache_key(&l_url), Freshness::CacheFirst) {
        Ok(t) => RefCurve::parse(&t),
        Err(_) => cached_get(&mono_url, &cache_key(&mono_url), Freshness::CacheFirst)
            .ok()
            .and_then(|t| RefCurve::parse(&t)),
    }
    .ok_or_else(|| "no parseable measurement file".to_string())?;

    let right = cached_get(&r_url, &cache_key(&r_url), Freshness::CacheFirst)
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

/// Fetch + parse a target curve. HarutoHiroki-style forks keep targets under
/// `data/targets/`; everyone else puts them directly in `data/`. Try both.
fn fetch_target(t: &TargetEntry) -> Result<(String, RefCurve), String> {
    let fname = pct(&format!("{} Target.txt", t.name));
    let url_a = format!("{}data/targets/{}", t.base_url, fname);
    let url_b = format!("{}data/{}", t.base_url, fname);
    let body = cached_get(&url_a, &cache_key(&url_a), Freshness::CacheFirst)
        .or_else(|_| cached_get(&url_b, &cache_key(&url_b), Freshness::CacheFirst))?;
    let curve = RefCurve::parse(&body).ok_or_else(|| "unparseable target file".to_string())?;
    Ok((t.name.clone(), curve))
}

/// Fetch a measurement and collapse it to a single curve (L+R averaged) to use
/// as a target — "EQ toward this headphone's response".
fn fetch_measurement_target(m: &ModelEntry) -> Result<(String, RefCurve), String> {
    let f = fetch_model(m)?;
    let curve = match &f.right {
        Some(r) => RefCurve::average(&f.left, r),
        None => f.left,
    };
    Ok((f.name, curve))
}

// ── config.js target parsing ──────────────────────────────────────────────────

/// Extract target file basenames from a `CrinGraph` `config.js`. Finds the
/// `targets = [ … ]` array and collects every quoted string inside each
/// `files:[ … ]` (a target's basename → `<base>data/[targets/]<name> Target.txt`).
/// Comments are stripped first (sites comment out dead targets); the whole scan
/// is string-aware so a keyword, bracket or comment marker inside a quoted value
/// can't confuse it, and `files` is matched only as an object key.
fn parse_target_names(js: &str) -> Vec<String> {
    let clean = strip_comments(js);
    let b = clean.as_bytes();
    let Some((start, end)) = targets_array_bounds(b) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut i = start;
    let mut in_str = false;
    let mut q = b'"';
    let mut esc = false;
    while i < end {
        let ch = b[i];
        if in_str {
            if esc {
                esc = false;
            } else if ch == b'\\' {
                esc = true;
            } else if ch == q {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            in_str = true;
            q = ch;
            i += 1;
            continue;
        }
        // A `files:` key (not a "files" substring inside a value): harvest its
        // array's quoted basenames using quote-aware bracket matching.
        if is_files_key(b, i) {
            let mut j = i + 5;
            while j < end && b[j] != b'[' && b[j] != b'}' && b[j] != b',' {
                j += 1;
            }
            if j < end && b[j] == b'[' {
                if let Some(close) = matching_bracket(b, j) {
                    for s in quoted_strings(&clean[j + 1..close]) {
                        let name = s.trim().to_string();
                        if !name.is_empty() && seen.insert(name.clone()) {
                            names.push(name);
                        }
                    }
                    i = close + 1;
                    continue;
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    names
}

/// Byte range `(inner_start, close_index)` of the first `targets = [ … ]` /
/// `targets: [ … ]` array, located string-aware (a literal "targets" inside a
/// quoted value is skipped) with word boundaries (not "myTargets").
fn targets_array_bounds(b: &[u8]) -> Option<(usize, usize)> {
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    let mut in_str = false;
    let mut q = b'"';
    let mut esc = false;
    while i < b.len() {
        let ch = b[i];
        if in_str {
            if esc {
                esc = false;
            } else if ch == b'\\' {
                esc = true;
            } else if ch == q {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            in_str = true;
            q = ch;
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"targets")
            && (i == 0 || !ident(b[i - 1]))
            && b.get(i + 7).is_none_or(|&c| !ident(c))
        {
            // Next non-space must be `=` (assignment) or `:` (object key), then
            // the array `[`. Skip strings while seeking the `[`.
            let mut k = i + 7;
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
            if k < b.len() && (b[k] == b'=' || b[k] == b':') {
                if let Some(open) = next_bracket(b, k + 1) {
                    if let Some(close) = matching_bracket(b, open) {
                        return Some((open + 1, close));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Index of the next `[` at/after `from`, skipping string literals.
fn next_bracket(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let mut in_str = false;
    let mut q = b'"';
    let mut esc = false;
    while i < b.len() {
        let ch = b[i];
        if in_str {
            if esc {
                esc = false;
            } else if ch == b'\\' {
                esc = true;
            } else if ch == q {
                in_str = false;
            }
        } else if ch == b'"' || ch == b'\'' || ch == b'`' {
            in_str = true;
            q = ch;
        } else if ch == b'[' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Index of the `]` matching the `[` at `open`, scanning string-aware so
/// brackets inside quoted names don't unbalance the count.
fn matching_bracket(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut q = b'"';
    let mut esc = false;
    let mut k = open;
    while k < b.len() {
        let ch = b[k];
        if in_str {
            if esc {
                esc = false;
            } else if ch == b'\\' {
                esc = true;
            } else if ch == q {
                in_str = false;
            }
        } else {
            match ch {
                b'"' | b'\'' | b'`' => {
                    in_str = true;
                    q = ch;
                }
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(k);
                    }
                }
                _ => {}
            }
        }
        k += 1;
    }
    None
}

/// True if byte `i` begins a `files` object key — the identifier `files`
/// (word-bounded on the left) followed by optional whitespace then `:`. Rejects
/// "profiles", "filesFoo", and a "files" substring inside a value.
fn is_files_key(b: &[u8], i: usize) -> bool {
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    if !b[i..].starts_with(b"files") {
        return false;
    }
    if i > 0 && ident(b[i - 1]) {
        return false;
    }
    let mut k = i + 5;
    while k < b.len() && b[k].is_ascii_whitespace() {
        k += 1;
    }
    b.get(k) == Some(&b':')
}

/// Strip `//` line and `/* */` block comments, leaving string literals intact
/// (so `//` inside a URL string isn't treated as a comment). UTF-8 safe (target
/// names carry Δ/∆, parens, accents).
fn strip_comments(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    let mut q = '"';
    let mut esc = false;
    while i < n {
        let c = chars[i];
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                in_str = false;
            }
            i += 1;
        } else if c == '"' || c == '\'' || c == '`' {
            in_str = true;
            q = c;
            out.push(c);
            i += 1;
        } else if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Collect every quoted-string literal in `s` (handles `"`, `'`, `` ` ``).
fn quoted_strings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut q = '"';
    let mut esc = false;
    for c in s.chars() {
        if in_str {
            if esc {
                cur.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                out.push(std::mem::take(&mut cur));
                in_str = false;
            } else {
                cur.push(c);
            }
        } else if c == '"' || c == '\'' || c == '`' {
            in_str = true;
            q = c;
            cur.clear();
        }
    }
    out
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

/// How a `cached_get` should treat an existing disk copy.
#[derive(Clone, Copy)]
enum Freshness {
    /// Use any cached copy regardless of age; only hit the network on a miss.
    /// Curve files are immutable and a session-warm catalog is correct, so this
    /// is the snappy default.
    CacheFirst,
    /// Always re-fetch from the network; fall back to a stale cache only if the
    /// network fails (the manual "Refresh" behaviour).
    Refresh,
    /// Serve the cache while its file mtime is younger than `max_age`; otherwise
    /// re-fetch (falling back to the stale copy on network error). This is the
    /// catalog auto-update policy: warm instantly, refresh transparently.
    IfStale(std::time::Duration),
}

/// Decide whether a cached copy of the given `age` counts as fresh under
/// `IfStale(max)`. `None` (no copy / unreadable mtime) is never fresh. Split out
/// as a pure helper so the age→freshness decision is unit-testable without
/// touching the filesystem or network.
fn is_fresh(age: Option<std::time::Duration>, max: std::time::Duration) -> bool {
    matches!(age, Some(a) if a < max)
}

/// Age of the cache file at `path` (now − mtime), or `None` if it is missing or
/// its mtime can't be read (treated as "stale").
fn cache_age(path: &std::path::Path) -> Option<std::time::Duration> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    std::time::SystemTime::now().duration_since(modified).ok()
}

/// GET with a disk cache under `curve_cache_dir()`. The [`Freshness`] policy
/// decides whether an existing copy short-circuits the network; on a network
/// error a stale cache is used as a fallback so the browser keeps working
/// offline.
fn cached_get(url: &str, key: &str, fresh: Freshness) -> Result<String, String> {
    let dir = resonance_ipc::paths::curve_cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(key);
    // Whether the cached copy (if any) is good enough to return without a fetch.
    let use_cache = match fresh {
        Freshness::CacheFirst => true,
        Freshness::Refresh => false,
        Freshness::IfStale(max) => is_fresh(cache_age(&path), max),
    };
    if use_cache {
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
                out.push(b as char);
            }
            _ => write!(out, "%{b:02X}").unwrap(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_targets() {
        let js = r#"
            const DIR = "data/";
            const targets = [
                { type:"Headphone", files:["Harman 2018"] },
                { type:"Over-ear",  files:["Harman over-ear 2018"] },
                { files:["IEF Neutral"] }
            ];
        "#;
        let names = parse_target_names(js);
        assert_eq!(
            names,
            vec!["Harman 2018", "Harman over-ear 2018", "IEF Neutral"]
        );
    }

    #[test]
    fn skips_commented_out_targets() {
        // Sites comment out dead targets with `//`; those must not be returned.
        let js = r#"
            var targets = [
                { files:["Live Target"] },
                // { files:["Dead Target"] },
                { files:["Another"] } /* { files:["Block Dead"] } */
            ];
        "#;
        let names = parse_target_names(js);
        assert_eq!(names, vec!["Live Target", "Another"]);
    }

    #[test]
    fn handles_unicode_and_urls_in_strings() {
        // `//` inside a URL string must NOT be treated as a comment, and unicode
        // target names must round-trip intact.
        let js = r#"
            const info = "see https://example.com/x";
            const targets = [
                { name:"Δ Diffuse", files:["Δ Diffuse Field"] },
                { files:["Harman (2019)"] }
            ];
        "#;
        let names = parse_target_names(js);
        assert_eq!(names, vec!["Δ Diffuse Field", "Harman (2019)"]);
    }

    #[test]
    fn no_targets_array_yields_empty() {
        assert!(parse_target_names("const x = 1; let targetsCount = 3;").is_empty());
        assert!(parse_target_names("").is_empty());
    }

    #[test]
    fn dedups_repeated_names() {
        let js = r#"targets = [ {files:["A"]}, {files:["A"]}, {files:["B"]} ];"#;
        assert_eq!(parse_target_names(js), vec!["A", "B"]);
    }

    #[test]
    fn keeps_bracket_in_name() {
        // A ']' inside a quoted name must not end the files array early.
        let js = r#"targets = [ { files:["A [v2]"] }, { files:["B"] } ];"#;
        assert_eq!(parse_target_names(js), vec!["A [v2]", "B"]);
    }

    #[test]
    fn ignores_files_substring_in_value() {
        // "files" inside a value, and an unrelated array, must not be harvested.
        let js = r#"targets = [ { note:"see other files", links:["http://x"], files:["Real"] } ];"#;
        assert_eq!(parse_target_names(js), vec!["Real"]);
    }

    #[test]
    fn ignores_targets_keyword_in_string() {
        // The decoy contains a full `targets = [...]` *inside a string literal*, so
        // this passes only if the keyword scan is genuinely string-aware (a naive
        // scan would latch onto the in-string array and return nothing).
        let js = r#"var help = "set targets = [bad] here"; const targets = [ {files:["Real"]} ];"#;
        assert_eq!(parse_target_names(js), vec!["Real"]);
    }

    #[test]
    fn freshness_decision() {
        use std::time::Duration;
        let week = Duration::from_secs(60 * 60 * 24 * 7);
        // No cache copy (or unreadable mtime) is never fresh → always re-fetch.
        assert!(!is_fresh(None, week));
        // Younger than the TTL → fresh (serve from cache).
        assert!(is_fresh(Some(Duration::from_secs(60 * 60 * 24)), week));
        // Older than the TTL → stale (re-fetch).
        assert!(!is_fresh(Some(Duration::from_secs(60 * 60 * 24 * 8)), week));
        // Exactly the TTL is stale (boundary is `<`, not `<=`).
        assert!(!is_fresh(Some(week), week));
    }
}
