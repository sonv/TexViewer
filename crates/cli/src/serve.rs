//! `mathpreview-cli serve <file>` — HTTP page + WebSocket live-reload.
//!
//! Architecture:
//!   * `axum` serves `GET /` (rendered page) and `GET /ws` (live updates).
//!   * `notify-debouncer-full` watches every file in the resolved project
//!     and queues re-renders.
//!   * A `tokio::sync::broadcast` channel carries `body-updated` payloads
//!     from the watcher task to every connected WebSocket.
//!
//! Updates fire on file save (i.e. when the editor writes to disk). For
//! per-keystroke live updating without editor config, see the planned nvim
//! RPC integration — that's a separate Step, not part of this subcommand.

use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc as std_mpsc, Arc,
};
use std::time::{Duration, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket},
        Json, Path as AxumPath, Query, State, WebSocketUpgrade,
    },
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::new_debouncer;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, Notify, RwLock};

use mathpreview_core::{
    bibtex::{self, BibEntry, BibStyle},
    macros::{self, ExtractedPreamble},
    numbering, parser, project, render_project, render_project_from_source, renderer,
    sync::SyncIndex,
    theorems::TheoremRegistry,
    HtmlOptions, RenderOutput, RenderedBlock,
};

const WS_PROTOCOL_VERSION: &str = "74";

/// stderr logging that survives a closed pipe. The nvim plugin can spawn the
/// daemon detached (`close_on_exit = false`) so the preview outlives the
/// editor — but the daemon keeps nvim's stderr pipe, whose read end closes
/// when nvim exits. From then on `eprintln!` gets EPIPE (Rust ignores SIGPIPE)
/// and PANICS, unwinding whatever request task was logging — Print, panel
/// saves, reveal-source would silently fail. Ignore the write error instead:
/// losing a log line beats losing the request.
macro_rules! elog {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

/// Browser-ready semantics for the editor's active Vim search. The plugin
/// resolves Vim-only atoms into these flags; the browser still searches the
/// rendered text, but no longer loses whole-word or case behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct EditorSearch {
    query: String,
    whole_start: bool,
    whole_end: bool,
    case_sensitive: bool,
}

fn search_sync_payload(search: &EditorSearch) -> String {
    serde_json::json!({
        "event": "search-sync",
        "query": search.query,
        "whole_start": search.whole_start,
        "whole_end": search.whole_end,
        "case_sensitive": search.case_sensitive,
    })
    .to_string()
}

#[derive(Clone)]
struct AppState {
    opts: HtmlOptions,
    current: Arc<RwLock<RenderOutput>>,
    tx: broadcast::Sender<String>,
    watched: Arc<RwLock<HashSet<PathBuf>>>,
    watch_tx: std_mpsc::Sender<HashSet<PathBuf>>,
    /// Cached preamble + bib state, keyed on a hash of the preamble source.
    preamble_cache: Arc<RwLock<Option<PreambleCache>>>,
    /// Unsaved editor buffers, keyed by canonical project file path.
    /// `/buffer` can target the root or any watched included file; the
    /// renderer then splices these sources into the real root project.
    buffer_overrides: Arc<RwLock<HashMap<PathBuf, String>>>,
    /// Last broadcast block sequence — diff target for the next render.
    /// Updated atomically with each broadcast so reconnects and patches
    /// stay in sync.
    last_blocks: Arc<RwLock<Vec<RenderedBlock>>>,
    /// Monotonic render attempt id. Buffer pushes can complete out of order;
    /// only the newest attempt is allowed to update the preview.
    render_seq: Arc<AtomicU64>,
    jump_seq: Arc<AtomicU64>,
    pending_jump: Arc<RwLock<Option<SourceJump>>>,
    /// Notified whenever a new `pending_jump` is set, so a long-polling
    /// `GET /jump` can wake immediately instead of returning empty.
    jump_notify: Arc<Notify>,
    /// Count of `GET /jump` requests currently parked (long-polling). The
    /// nvim plugin keeps exactly one in flight while attached, so a nonzero
    /// count means an editor is handling navigation in place — reveal-source
    /// uses this (not just the `last_jump_poll_ms` timestamp, which under
    /// long-polling only updates every ~25 s) to skip the duplicate spawn.
    active_jump_pollers: Arc<AtomicU64>,
    /// `sh -c` template for the Cmd/Ctrl-click reveal-source feature.
    /// `{file}` (shell-quoted), `{line}`, and `{col}` are substituted
    /// in before exec. Empty string disables the feature.
    editor_cmd: Arc<String>,
    /// Macro override files registered at runtime via `POST /macros/register`
    /// (the dialog's "Use as override" button). Live alongside the
    /// startup-discovered cascade in `opts.macro_overrides` and feed
    /// into every render's effective override list. Watched for
    /// live-reload the same way the startup files are.
    session_macros: Arc<RwLock<Vec<PathBuf>>>,
    /// TOML config cascade — the discovered file paths checked again
    /// on every render so a fresh `POST /config/set` (or an in-editor
    /// edit to one of the files) immediately takes effect on the next
    /// render. `opts.viewer_config` is treated as a startup snapshot;
    /// the *live* value lives here.
    config_paths: Arc<Vec<PathBuf>>,
    viewer_config: Arc<RwLock<mathpreview_core::ResolvedViewerConfig>>,
    /// The editor's active `/` search semantics (last `POST /search` payload).
    /// Replayed to each newly-connected WS client so a reloaded tab (or one
    /// opened late) shows the highlight — the broadcast alone would be lost,
    /// and the plugin's dedup won't re-send an unchanged pattern.
    search_query: Arc<RwLock<EditorSearch>>,
    /// Last cursor position received on `POST /cursor`, for jump detection:
    /// the nearest-element follow fallback (cursor on unrendered source, e.g.
    /// the preamble) fires only on a LARGE move — a search wrap, `gg`/`G` — so
    /// ordinary editing up there doesn't yank the preview to the top.
    last_cursor: Arc<RwLock<Option<(PathBuf, u32)>>>,
    /// Per-render hot path: cache of `(path → content + mtime)` so
    /// override / config file reads on every render skip disk I/O
    /// when nothing changed. Invalidated by mtime on the next render
    /// the file is touched.
    file_content_cache: Arc<RwLock<HashMap<PathBuf, CachedFile>>>,
    /// Lazily compiled TikZ SVGs. A mutex intentionally serializes TeX jobs:
    /// loading a page with many diagrams must not fork a compiler storm.
    tikz_cache: Arc<Mutex<HashMap<String, TikzCacheEntry>>>,
    /// Ring buffer of recent server log entries surfaced in the
    /// viewer's "log" dialog. Capped at `LOG_BUFFER_CAP` newest
    /// entries. Std `Mutex` rather than tokio's so the file-watcher
    /// thread (which is sync) can call `log_event` directly.
    /// Mirrors what's also eprintln!'d to the terminal.
    log_buffer: Arc<std::sync::Mutex<std::collections::VecDeque<LogEntry>>>,
    /// Whether high-frequency events (per-keystroke buffer-push
    /// timings, every watched-file save, watcher status, etc.) are
    /// added to the log buffer. Toggled by the viewer's log panel.
    /// Default off so the buffer stays focused on the events users
    /// usually care about (config writes, macros, errors).
    debug_logging: Arc<AtomicBool>,
    /// Unix-ms timestamp of the last `GET /jump` request. The nvim plugin
    /// keeps one long-poll parked here while attached and applies jumps in
    /// place, so a recent value (or a nonzero `active_jump_pollers`) means an
    /// editor is already handling navigation and `/reveal-source` should NOT
    /// also spawn an editor (which would reopen the file in a second buffer).
    /// 0 = never polled.
    last_jump_poll_ms: Arc<AtomicU64>,
    /// Unix-ms timestamp of the last editor-originated request (buffer push,
    /// cursor, selection, search — endpoints only the plugin calls). Feeds
    /// /debug's `editor_active` so the plugin's stale-daemon sweep can tell
    /// an abandoned daemon from one whose editor simply has sync disabled
    /// (no jump poll) but is still pushing edits. 0 = never contacted.
    last_editor_contact_ms: Arc<AtomicU64>,
}

const LOG_BUFFER_CAP: usize = 400;
const TIKZ_CACHE_CAP: usize = 32;
const TIKZ_COMPILE_TIMEOUT: Duration = Duration::from_secs(20);
const TIKZ_CONVERT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct TikzCacheEntry {
    svg: Vec<u8>,
    success: bool,
}

/// Grace window after a `/jump` long-poll returns: if the editor re-issued
/// its poll within this window we still treat it as attached, covering the
/// brief gap between one long-poll returning and the next being parked.
/// (The primary signal is `active_jump_pollers`; this is the backstop.)
const JUMP_POLLER_ACTIVE_MS: u64 = 2000;

/// How long `GET /jump?wait=…` may park before returning empty when no jump
/// arrives. Caps the request lifetime; the plugin re-issues immediately.
const JUMP_LONGPOLL_MAX_MS: u64 = 30_000;

/// RAII counter: increments `active_jump_pollers` on construction and
/// decrements on drop — including when the client disconnects mid-park and
/// axum cancels the handler future, so the count never leaks.
struct PollerGuard(Arc<AtomicU64>);
impl PollerGuard {
    fn new(counter: &Arc<AtomicU64>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        PollerGuard(counter.clone())
    }
}
impl Drop for PollerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Record that an editor-only endpoint was just called (see
/// `last_editor_contact_ms`). Called at the top of /buffer, /cursor,
/// /selection and /search — the requests only the plugin makes.
fn touch_editor_contact(state: &AppState) {
    state
        .last_editor_contact_ms
        .store(now_ms(), Ordering::Release);
}

/// One server-side log entry. `level` is one of `"info"`, `"warn"`,
/// `"error"`. `ts_ms` is the Unix-millisecond timestamp at capture
/// time so the client can render relative ages.
#[derive(Debug, Clone, Serialize)]
struct LogEntry {
    level: &'static str,
    ts_ms: u128,
    message: String,
}

struct PreambleCache {
    hash: u64,
    /// Combined fingerprint of the override-file paths and (when they
    /// exist on disk) their contents. Invalidates the cache when any
    /// layer of the override cascade changes — including a fresh
    /// `POST /macros/append` writing to the file.
    overrides_hash: u64,
    preamble: ExtractedPreamble,
    bib: HashMap<String, BibEntry>,
    bib_style: BibStyle,
}

/// Build the effective override-cascade for the current render. Returns
/// the startup-discovered paths from `opts.macro_overrides` first
/// (lowest priority), then the session-registered paths from
/// `/macros/register` (higher priority — most recently added wins on
/// name collision). Dedupes so the same path doesn't appear twice if a
/// session register matches the project file.
async fn effective_override_paths(state: &AppState) -> Vec<PathBuf> {
    let session = state.session_macros.read().await.clone();
    let mut out = Vec::with_capacity(state.opts.macro_overrides.len() + session.len());
    let mut seen = std::collections::HashSet::new();
    for p in state.opts.macro_overrides.iter().chain(session.iter()) {
        if seen.insert(p.clone()) {
            out.push(p.clone());
        }
    }
    out
}

/// Read every override file in `paths` whose path resolves to an
/// existing file. Missing files are skipped silently (matches what
/// `finish_render` in core does), so the daemon can list "would-be"
/// paths like a `.mathpreview-macros.tex` that hasn't been created
/// yet without failing the render.
///
/// Hot path: called on every render. We avoid re-reading from disk
/// when the file's mtime hasn't changed by consulting a small per-
/// daemon cache (`AppState.file_content_cache`). Misses fall back to
/// a normal read + insert.
async fn load_override_layers(
    state: &AppState,
    paths: &[PathBuf],
) -> Vec<mathpreview_core::MacroOverride> {
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        if let Some(source) = read_cached(state, p).await {
            out.push(mathpreview_core::MacroOverride {
                label: p.clone(),
                source,
            });
        }
    }
    out
}

/// Same as `log_event` but a no-op unless debug logging is enabled
/// (toggled by `POST /debug/mode` from the viewer's log panel).
/// Use for high-frequency events that would drown out the always-on
/// channel — per-keystroke render timings, every watcher event, etc.
fn log_event_verbose(state: &AppState, level: &'static str, message: String) {
    if state.debug_logging.load(Ordering::Acquire) {
        log_event(state, level, message);
    }
}

/// Push one log entry into the ring buffer + emit to stderr. Trims
/// the oldest entries when the buffer exceeds `LOG_BUFFER_CAP`.
/// Mirrors plain `eprintln!` so behaviour from the terminal is
/// unchanged; the buffer just makes the same lines reachable from
/// the viewer's "log" panel. Sync (std `Mutex`) so the file-watcher
/// thread can call it too.
fn log_event(state: &AppState, level: &'static str, message: String) {
    elog!("mathpreview: {message}");
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let Ok(mut buf) = state.log_buffer.lock() else { return };
    if buf.len() >= LOG_BUFFER_CAP {
        let drop = buf.len() + 1 - LOG_BUFFER_CAP;
        for _ in 0..drop {
            buf.pop_front();
        }
    }
    buf.push_back(LogEntry {
        level,
        ts_ms,
        message,
    });
}

/// `GET /debug` — JSON snapshot of the daemon's current state for the
/// viewer's log dialog. Includes the config / macro cascade
/// (resolved paths + whether each file exists on disk), the
/// currently-applied viewer config, the editor template, the WS
/// protocol version, and the tail of the in-memory log ring buffer.
/// Read-only; safe to call at any time.
async fn serve_debug(State(state): State<AppState>) -> Response {
    let viewer_config = state.viewer_config.read().await.clone();
    let session_macros = state.session_macros.read().await.clone();
    // The document's geometry margin (if any), for the effective-margin echo.
    let geometry_margin_mm = state
        .preamble_cache
        .read()
        .await
        .as_ref()
        .and_then(|c| c.preamble.geometry_margin_mm);
    let log: Vec<LogEntry> = state
        .log_buffer
        .lock()
        .map(|buf| buf.iter().cloned().collect())
        .unwrap_or_default();

    let config_paths: Vec<_> = state
        .config_paths
        .iter()
        .map(|p| {
            serde_json::json!({
                "path": p,
                "exists": p.is_file(),
            })
        })
        .collect();

    let macro_paths: Vec<_> = state
        .opts
        .macro_overrides
        .iter()
        .map(|p| {
            serde_json::json!({
                "path": p,
                "exists": p.is_file(),
                "source": "startup",
            })
        })
        .chain(session_macros.iter().map(|p| {
            serde_json::json!({
                "path": p,
                "exists": p.is_file(),
                "source": "session",
            })
        }))
        .collect();

    // Project root + the files this daemon watches (root + \input/\include +
    // bib). The nvim plugin uses this to route an edit in any project file to
    // the daemon that actually owns it (one daemon per file), so editing an
    // \input updates the right tab even with several projects open.
    let root_file = state.current.read().await.root_file.display().to_string();
    let watched: Vec<String> = {
        let w = state.watched.read().await;
        let mut v: Vec<String> = w.iter().map(|p| p.display().to_string()).collect();
        v.sort();
        v
    };

    Json(serde_json::json!({
        "ws_protocol": WS_PROTOCOL_VERSION,
        // The RUNNING daemon's version. The plugin compares this on daemon
        // reuse: a long-lived daemon survives plugin/binary upgrades (reuse
        // skips the reinstall path), silently lacking newer features — the
        // "stale daemon" trap. The nag it enables says: :MathPreviewRestart.
        "version": env!("CARGO_PKG_VERSION"),
        "root": root_file,
        "watched": watched,
        // Number of connected browser tabs (live WebSocket subscribers). The
        // nvim plugin reads this to reuse an already-open tab instead of opening
        // a duplicate on a repeat :MathPreview.
        "clients": state.tx.receiver_count(),
        "pid": std::process::id(),
        // An editor is attached: a /jump long-poll is parked (or was within
        // the last 45s — polls re-park within ~31s), or an editor-only
        // endpoint (/buffer, /cursor, /selection, /search) was called within
        // the last 45s (covers plugins running with sync = false, which
        // never poll). The plugin's stale-daemon sweep treats
        // editor_active == false && clients == 0 as abandoned.
        "editor_active": state.active_jump_pollers.load(Ordering::Acquire) > 0
            || now_ms().saturating_sub(state.last_jump_poll_ms.load(Ordering::Acquire)) < 45_000
            || now_ms().saturating_sub(state.last_editor_contact_ms.load(Ordering::Acquire)) < 45_000,
        "debug_logging": state.debug_logging.load(Ordering::Acquire),
        "editor_cmd": state.editor_cmd.as_ref(),
        "viewer_config": {
            "font_size": viewer_config.font_size,
            "ui_font_size": viewer_config.ui_font_size,
            "default_page_mode": viewer_config.default_page_mode.as_str(),
            "default_theme": viewer_config.default_theme.as_str(),
            "source_jump_trigger": viewer_config.source_jump_trigger.as_str(),
            "render_tikz": viewer_config.render_tikz,
            "mathjax_config": viewer_config.mathjax_config,
            "keybindings": viewer_config.keybindings,
            "page_margin_mm": mathpreview_core::effective_page_margin_mm(
                &viewer_config, geometry_margin_mm),
        },
        "config_paths": config_paths,
        "macro_paths": macro_paths,
        "log": log,
    }))
    .into_response()
}

/// `POST /debug/mode` — flip verbose logging on/off. Body:
/// `{"enabled": true|false}`. Returns the new state.
#[derive(Debug, Deserialize)]
struct DebugModeRequest {
    enabled: bool,
}

async fn serve_debug_mode(
    State(state): State<AppState>,
    Json(req): Json<DebugModeRequest>,
) -> Response {
    state.debug_logging.store(req.enabled, Ordering::Release);
    log_event(
        &state,
        "info",
        format!(
            "debug logging {}",
            if req.enabled { "ENABLED" } else { "disabled" }
        ),
    );
    Json(serde_json::json!({ "debug_logging": req.enabled })).into_response()
}

/// Mtime-cached equivalent of `mathpreview_core::load_and_merge_config`.
/// Reads each path through `read_cached` so unchanged TOML files skip
/// the disk on the render hot path.
async fn load_and_merge_config_cached(
    state: &AppState,
) -> anyhow::Result<mathpreview_core::ResolvedConfig> {
    let mut merged = mathpreview_core::Config::default();
    for p in state.config_paths.iter() {
        if let Some(src) = read_cached(state, p).await {
            let layer = mathpreview_core::Config::parse(&src, p)?;
            merged.merge(layer);
        }
    }
    Ok(merged.resolve())
}

/// Mtime-keyed file-content cache entry. `mtime = None` means we cached
/// a "file does not exist" lookup; on subsequent calls we still
/// re-stat (cheap) and only re-read if the existence/mtime changed.
#[derive(Debug, Clone)]
struct CachedFile {
    mtime: Option<std::time::SystemTime>,
    content: Option<String>,
}

/// Read `path`'s contents, going through the daemon's file-content
/// cache. Returns `None` if the file does not exist (matches the
/// previous `fs::read_to_string(p).ok()` behaviour).
async fn read_cached(state: &AppState, path: &Path) -> Option<String> {
    let mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
    {
        let cache = state.file_content_cache.read().await;
        if let Some(entry) = cache.get(path) {
            if entry.mtime == mtime {
                return entry.content.clone();
            }
        }
    }
    let content = match mtime {
        Some(_) => std::fs::read_to_string(path).ok(),
        None => None,
    };
    let mut cache = state.file_content_cache.write().await;
    cache.insert(
        path.to_path_buf(),
        CachedFile {
            mtime,
            content: content.clone(),
        },
    );
    content
}

/// Hash the override-file paths AND the contents of the files that
/// were actually read. Cheap fingerprint included in the cache key.
fn fingerprint_overrides(
    paths: &[PathBuf],
    loaded: &[mathpreview_core::MacroOverride],
) -> u64 {
    let mut acc = String::new();
    for p in paths {
        acc.push_str(&p.display().to_string());
        acc.push('\n');
    }
    acc.push_str("---\n");
    for ov in loaded {
        acc.push_str(&ov.label.display().to_string());
        acc.push('\n');
        acc.push_str(&ov.source);
        acc.push('\n');
    }
    fnv_hash(&acc)
}

#[derive(Debug, Deserialize)]
struct SourceRequest {
    file: PathBuf,
    line: u32,
    col: Option<u32>,
    /// Backward search inside a multi-row math block (POST /jump and
    /// /reveal-source only): the block's DOM id plus which rendered `mtr` row
    /// the click landed on, out of how many rendered rows. `file`/`line`/`col`
    /// above stay the block's own anchor — the fallback when the sync index
    /// disagrees (mid-edit skew) or these fields are absent (prose clicks).
    #[serde(default)]
    element_id: Option<String>,
    #[serde(default)]
    math_row: Option<usize>,
    #[serde(default)]
    row_count: Option<usize>,
    /// POST /cursor only: this cursor move was caused by an edit (the plugin
    /// saw a TextChanged within the last half second). The viewer follows the
    /// cursor but skips the flash box — typing shouldn't strobe.
    #[serde(default)]
    typing: Option<bool>,
}

/// Body for `POST /search` — the editor's active `/` search pattern, mirrored
/// into the preview as an hlsearch-style highlight. An empty/omitted `query`
/// (or `clear: true`) drops the highlight (e.g. after `:nohlsearch`).
#[derive(Debug, Deserialize)]
struct SearchRequest {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    whole_start: bool,
    #[serde(default)]
    whole_end: bool,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    clear: Option<bool>,
}

/// Body for `POST /selection` — the editor's current visual selection as an
/// inclusive 1-based source range. `clear: true` (sent when the user leaves
/// visual mode) drops the highlight. Missing bounds also clear.
#[derive(Debug, Deserialize)]
struct RangeRequest {
    file: PathBuf,
    #[serde(default)]
    clear: bool,
    start_line: Option<u32>,
    start_col: Option<u32>,
    end_line: Option<u32>,
    end_col: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct SourceJump {
    seq: u64,
    file: PathBuf,
    line: u32,
    col: u32,
}

/// Resident memory of the daemon process in MiB, or `None` if unavailable.
fn resident_mib() -> Option<f64> {
    memory_stats::memory_stats().map(|s| s.physical_mem as f64 / 1024.0 / 1024.0)
}

/// Snapshot of bytes held by the caches the server keeps between renders.
/// Used to verify we don't quietly grow unbounded.
fn cache_size_bytes(state: &AppState, last_blocks: &[RenderedBlock]) -> usize {
    let blocks: usize = last_blocks
        .iter()
        .map(|b| b.id.len() + b.hash.len() + b.html.len())
        .sum();
    // Preamble cache: count just the strings we hold; the macro Vec etc.
    // is a small constant relative to the body.
    let preamble = {
        // Approximate — we just read the option non-blocking. If contended,
        // skip and return 0.
        match state.preamble_cache.try_read() {
            Ok(g) => g
                .as_ref()
                .map(|c| {
                    c.preamble.raw_preamble.len()
                        + c.preamble
                            .macros
                            .iter()
                            .map(|m| m.name.len() + m.body.len() + m.source.len())
                            .sum::<usize>()
                })
                .unwrap_or(0),
            Err(_) => 0,
        }
    };
    blocks + preamble
}

fn fmt_mem_log(state: &AppState, last_blocks: &[RenderedBlock]) -> String {
    let rss = resident_mib()
        .map(|m| format!("{m:.1} MiB rss"))
        .unwrap_or_else(|| "rss ?".into());
    let cache_kib = cache_size_bytes(state, last_blocks) as f64 / 1024.0;
    format!("{rss}, cache {cache_kib:.1} KiB")
}

fn begin_render_attempt(state: &AppState) -> u64 {
    state.render_seq.fetch_add(1, Ordering::AcqRel) + 1
}

fn is_latest_render_attempt(state: &AppState, seq: u64) -> bool {
    state.render_seq.load(Ordering::Acquire) == seq
}

fn fnv_hash(s: &str) -> u64 {
    fnv_update(0xcbf29ce484222325, s.as_bytes())
}

fn fnv_update(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Cache fingerprint for every source that contributes preamble metadata.
/// Including paths as separators makes dependency additions/removals visible
/// even when two files happen to contain the same bytes.
fn preamble_fingerprint(project: &project::Project) -> u64 {
    let mut h = fnv_update(0xcbf29ce484222325, project.preamble.source.as_bytes());
    for file in &project.preamble_files {
        h = fnv_update(h, &[0xff]);
        h = fnv_update(h, file.path.as_os_str().as_encoded_bytes());
        h = fnv_update(h, &[0]);
        h = fnv_update(h, file.source.as_bytes());
    }
    h
}

pub async fn run(
    input: PathBuf,
    host: String,
    port: u16,
    opts: HtmlOptions,
    editor_cmd: String,
    config_paths: Vec<PathBuf>,
) -> Result<()> {
    let initial = if input.exists() {
        render_project(&input, &opts)
            .with_context(|| format!("initial render of {}", input.display()))?
    } else {
        // A root that isn't on disk yet — an editor buffer that's never been
        // saved. Serve a placeholder render of the empty document instead of
        // dying: the plugin pushes the buffer content right after startup,
        // and the first save lands the file on the watcher's dir watch.
        // (Dying here used to be misread by the plugin as a port-bind race,
        // which retried the doomed spawn across the whole port scan range.)
        elog!(
            "mathpreview: {} is not on disk yet — serving a placeholder until the editor pushes the buffer",
            input.display()
        );
        render_project_from_source(&input, String::new(), &opts)
            .with_context(|| format!("initial render of empty {}", input.display()))?
    };
    let mut watched: HashSet<PathBuf> = HashSet::new();
    watched.insert(initial.root_file.clone());
    for f in &initial.included_files {
        watched.insert(f.clone());
    }
    // Macro override + config files participate in live-reload from the
    // very first render, not just after the first buffer push. Editing
    // ~/.config/mathpreview/macros.tex (or the project .mathpreview*.tex /
    // .toml) should re-render even if you haven't touched the document yet —
    // otherwise a fresh `serve` ignores macro edits until the first keystroke,
    // which reads as "macros don't take effect unless I restart".
    for f in &opts.macro_overrides {
        watched.insert(f.clone());
    }
    for f in &config_paths {
        watched.insert(f.clone());
    }
    let mem_at_start = resident_mib()
        .map(|m| format!("{m:.1} MiB rss"))
        .unwrap_or_else(|| "rss ?".into());
    let initial_summary = format!(
        "initial render of {} ({} macros, {} packages, {} files watched; {})",
        initial.root_file.display(),
        initial.preamble.macros.len(),
        initial.preamble.packages_long.len(),
        watched.len(),
        mem_at_start,
    );

    // Drop the initial receiver immediately so `tx.receiver_count()` reflects
    // exactly the number of connected WebSocket clients (browser tabs) — the
    // `/debug` `clients` field the plugin uses to decide whether to reuse an
    // open tab. New tabs subscribe via `tx.subscribe()` regardless, and every
    // `tx.send` already ignores the "no receivers" error.
    let (tx, _) = broadcast::channel::<String>(16);
    let (watch_tx, watch_rx) = std_mpsc::channel::<HashSet<PathBuf>>();
    let last_blocks = initial.blocks.clone();
    let initial_viewer_config = opts.viewer_config.clone();
    let state = AppState {
        opts,
        current: Arc::new(RwLock::new(initial)),
        tx,
        watched: Arc::new(RwLock::new(watched)),
        watch_tx,
        preamble_cache: Arc::new(RwLock::new(None)),
        buffer_overrides: Arc::new(RwLock::new(HashMap::new())),
        last_blocks: Arc::new(RwLock::new(last_blocks)),
        render_seq: Arc::new(AtomicU64::new(0)),
        jump_seq: Arc::new(AtomicU64::new(0)),
        pending_jump: Arc::new(RwLock::new(None)),
        jump_notify: Arc::new(Notify::new()),
        active_jump_pollers: Arc::new(AtomicU64::new(0)),
        editor_cmd: Arc::new(editor_cmd),
        session_macros: Arc::new(RwLock::new(Vec::new())),
        config_paths: Arc::new(config_paths),
        viewer_config: Arc::new(RwLock::new(initial_viewer_config)),
        search_query: Arc::new(RwLock::new(EditorSearch::default())),
        last_cursor: Arc::new(RwLock::new(None)),
        file_content_cache: Arc::new(RwLock::new(HashMap::new())),
        tikz_cache: Arc::new(Mutex::new(HashMap::new())),
        log_buffer: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
        debug_logging: Arc::new(AtomicBool::new(false)),
        last_jump_poll_ms: Arc::new(AtomicU64::new(0)),
        last_editor_contact_ms: Arc::new(AtomicU64::new(0)),
    };

    // Seed the log buffer with startup info so the panel always has
    // something to show even before the first user action.
    log_event(&state, "info", initial_summary);

    spawn_watcher(state.clone(), watch_rx);

    // When bound to loopback (the default), enforce a `Host`-header allow-list
    // so a malicious web page cannot drive the unauthenticated control
    // endpoints via DNS-rebinding or cross-origin requests. For an explicit
    // non-loopback `--host` (LAN preview) we can't know the expected hostname,
    // so the guard is disabled and we warn loudly instead.
    let bind_is_loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/assets/*path", get(serve_asset))
        .route("/tikz/*path", get(serve_tikz))
        .route("/vendor/mathjax/*path", get(serve_vendor_mathjax))
        .route("/vendor/newcm-text/*path", get(serve_vendor_newcm_text))
        .route("/ws", get(serve_ws))
        .route("/buffer", axum::routing::post(serve_buffer_push))
        .route("/cursor", post(serve_cursor))
        .route("/selection", post(serve_selection))
        .route("/search", post(serve_search))
        .route("/jump", get(serve_jump_poll).post(serve_jump))
        .route("/reveal-source", post(serve_reveal_source))
        .route("/macros/append", post(serve_macros_append))
        .route("/macros/read", post(serve_macros_read))
        .route("/macros/register", post(serve_macros_register))
        .route("/config/set", post(serve_config_set))
        .route("/config/read", post(serve_config_read))
        .route("/config/write", post(serve_config_write))
        .route("/debug", get(serve_debug))
        .route("/debug/mode", post(serve_debug_mode))
        .route("/print", post(serve_print))
        .route("/restart", post(serve_restart))
        .route("/stop", post(serve_stop))
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| async move {
                host_guard(bind_is_loopback, req, next).await
            },
        ))
        .with_state(state.clone());

    // Log the bind once we know the address — separate from the
    // `eprintln!` below so the panel can show it too.
    log_event(&state, "info", format!("serving on http://{host}:{port}"));
    if !bind_is_loopback {
        let warn = format!(
            "binding non-loopback host {host} exposes unauthenticated control \
             endpoints (file writes, editor spawn, /stop, /print) to the \
             network and disables the Host-header guard — only use on a \
             trusted network",
        );
        elog!("mathpreview: WARNING: {warn}");
        log_event(&state, "warn", warn);
    }

    // Deliberate teardown (the nvim plugin's jobstop sends SIGTERM on
    // :MathPreviewStop / :bd / quitting nvim; SIGHUP covers a dying parent
    // session; SIGINT covers Ctrl-C on a terminal-run daemon): tell every
    // connected viewer goodbye BEFORE exiting, so a browser tab can close
    // itself (peek.nvim-style — window.close() is allowed for a tab whose
    // session history has one entry). A short
    // grace lets the WS forwarding tasks flush over loopback. Crashes skip
    // this path, so the client's reconnect UX still covers them.
    #[cfg(unix)]
    {
        let tx = state.tx.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let (Ok(mut term), Ok(mut hup), Ok(mut int), Ok(mut usr1)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::hangup()),
                signal(SignalKind::interrupt()),
                signal(SignalKind::user_defined1()),
            ) else {
                return;
            };
            tokio::select! {
                _ = term.recv() => {},
                _ = hup.recv() => {},
                _ = int.recv() => {},
                // SIGUSR1 = SILENT exit, for :MathPreviewRestart. No goodbye:
                // the viewer must SURVIVE a restart (reconnect to the rebound
                // port and hard-reload) — the restart path deliberately skips
                // re-opening a tab because it counts on the old one living.
                // Exiting immediately also frees the port inside the plugin's
                // rebind grace.
                _ = usr1.recv() => std::process::exit(0),
            }
            let _ = tx.send(r#"{"event":"bye"}"#.to_string());
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            std::process::exit(0);
        });
    }

    let addr = format!("{host}:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            // Exit code 12 = "port bind failed", the ONLY failure the plugin
            // retries on the next port. Before this code existed the plugin
            // treated ANY fast nonzero exit as a lost bind race and re-spawned
            // a doomed daemon across the whole scan range (e.g. 17 spawns for
            // a root file that didn't exist).
            elog!("mathpreview: binding {addr}: {e}");
            std::process::exit(12);
        }
    };
    elog!("mathpreview serving on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Guard the unauthenticated control endpoints against browser-driven attacks
/// (the daemon has no auth token). Two checks, both only when bound to loopback:
///
///   * `Host` must be a loopback name — defeats DNS-rebinding (the attacker's
///     domain re-resolved to 127.0.0.1 still carries `Host: attacker.com`).
///   * `Origin`, if present, must be loopback — defeats classic cross-origin
///     CSRF on the no-preflight endpoints (`/stop`, `/restart`, `/print`,
///     `/buffer`), where the browser sends `Host: 127.0.0.1` but
///     `Origin: http://evil.example`.
///
/// A missing `Host` or `Origin` is allowed: browsers always send `Host`, and
/// same-origin/top-level non-`Origin` requests (the served page itself, the
/// nvim plugin's `curl`) are legitimate. The combination blocks the browser
/// attack surface without breaking local non-browser clients.
async fn host_guard(
    enforce: bool,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if enforce {
        let headers = req.headers();
        let host_ok = headers
            .get(header::HOST)
            .is_none_or(|v| v.to_str().is_ok_and(host_is_loopback));
        if !host_ok {
            return (
                StatusCode::FORBIDDEN,
                "forbidden: request Host is not loopback",
            )
                .into_response();
        }
        let origin_ok = headers
            .get(header::ORIGIN)
            .is_none_or(|v| v.to_str().is_ok_and(origin_is_loopback));
        if !origin_ok {
            return (
                StatusCode::FORBIDDEN,
                "forbidden: cross-origin request rejected",
            )
                .into_response();
        }
    }
    next.run(req).await
}

/// True when the `Host` header names a loopback address (`localhost`,
/// `127.0.0.0/8`, `::1`), ignoring the optional `:port` suffix.
fn host_is_loopback(host_header: &str) -> bool {
    let host = host_header.trim();
    let hostname = if let Some(rest) = host.strip_prefix('[') {
        // Bracketed IPv6 literal: `[::1]` or `[::1]:port`.
        rest.split(']').next().unwrap_or("")
    } else {
        // `host` or `host:port` — strip a trailing `:port` if present.
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    if hostname.eq_ignore_ascii_case("localhost") {
        return true;
    }
    hostname
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// True when an `Origin` header (`scheme://host[:port]`) is a loopback origin.
/// A `null` origin (sandboxed iframe, `file://`, etc.) is not loopback, so it
/// is rejected.
fn origin_is_loopback(origin: &str) -> bool {
    let authority = origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or("");
    !authority.is_empty() && host_is_loopback(authority)
}

async fn serve_index(State(state): State<AppState>) -> impl IntoResponse {
    let current = state.current.read().await;
    (
        [
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, max-age=0, must-revalidate"),
            ),
            (header::PRAGMA, HeaderValue::from_static("no-cache")),
        ],
        Html(current.html.clone()),
    )
}

async fn serve_asset(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let root_dir = {
        let current = state.current.read().await;
        current
            .root_file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let preview_png = query.get("preview").is_some_and(|value| value == "png");
    match read_project_asset(&root_dir, &path, preview_png).await {
        Ok((bytes, content_type)) => {
            let mut response = Body::from(bytes).into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, max-age=0"),
            );
            response
        }
        Err(status) => status.into_response(),
    }
}

fn tikz_hash_from_path(path: &str) -> Option<&str> {
    let hash = path.strip_suffix(".svg")?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hash)
}

fn tikz_svg_response(entry: TikzCacheEntry) -> Response {
    let mut response = Body::from(entry.svg).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/svg+xml"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if entry.success {
            "public, max-age=31536000, immutable"
        } else {
            "no-store, max-age=0"
        }),
    );
    response
}

async fn serve_tikz(State(state): State<AppState>, AxumPath(path): AxumPath<String>) -> Response {
    if !state.viewer_config.read().await.render_tikz {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(hash) = tikz_hash_from_path(&path).map(str::to_string) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (asset, root_dir) = {
        let current = state.current.read().await;
        let Some(asset) = current.tikz_assets.get(&hash).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let root_dir = current
            .root_file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        (asset, root_dir)
    };

    // Hold the cache mutex across compilation on purpose. TikZ jobs are
    // relatively expensive; serial execution avoids a compiler storm when a
    // page with several lazy images becomes visible at once and also dedupes
    // simultaneous requests for the same hash.
    let mut cache = state.tikz_cache.lock().await;
    if let Some(entry) = cache.get(&hash).cloned() {
        return tikz_svg_response(entry);
    }

    let entry = match compile_tikz_svg(&asset, &root_dir, &hash).await {
        Ok(svg) => {
            log_event_verbose(&state, "info", format!("TikZ {hash}: compiled SVG"));
            TikzCacheEntry { svg, success: true }
        }
        Err(error) => {
            log_event(&state, "error", format!("TikZ {hash}: {error}"));
            TikzCacheEntry {
                svg: tikz_error_svg(&error),
                success: false,
            }
        }
    };
    // Successful SVGs are immutable and safe to retain. Do not retain error
    // placeholders: installing a missing engine/package and refreshing should
    // retry without requiring a daemon restart.
    if entry.success {
        if cache.len() >= TIKZ_CACHE_CAP {
            if let Some(oldest) = cache.keys().find(|key| *key != &hash).cloned() {
                cache.remove(&oldest);
            }
        }
        cache.insert(hash, entry.clone());
    }
    tikz_svg_response(entry)
}

struct TikzWorkDir(PathBuf);

impl Drop for TikzWorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn select_tikz_engine(preamble: &str) -> &'static str {
    let compact_magic = preamble
        .lines()
        .filter(|line| line.trim_start().starts_with('%'))
        .map(|line| line.to_ascii_lowercase().replace([' ', '\t'], ""))
        .collect::<Vec<_>>()
        .join("\n");
    if compact_magic.contains("!texprogram=xelatex") {
        "xelatex"
    } else if compact_magic.contains("!texprogram=lualatex")
        || preamble.contains(r"\usepackage{fontspec}")
        || preamble.contains(r"\usepackage[") && preamble.contains("]{fontspec}")
        || preamble.contains(r"\setmainfont")
    {
        "lualatex"
    } else {
        "pdflatex"
    }
}

fn tikz_document(asset: &mathpreview_core::renderer::TikzAsset) -> String {
    let class = if asset.preamble.contains(r"\documentclass") {
        ""
    } else {
        "\\documentclass{article}\n"
    };
    format!(
        "{class}{}\n\\usepackage[active,tightpage]{{preview}}\n\
         \\PreviewEnvironment{{{}}}\n\\pagestyle{{empty}}\n\
         \\begin{{document}}\n\\begin{{{}}}{}\\end{{{}}}\n\\end{{document}}\n",
        asset.preamble, asset.environment, asset.environment, asset.body, asset.environment,
    )
}

async fn command_output_with_timeout(
    mut command: tokio::process::Command,
    timeout: Duration,
    label: &str,
) -> std::result::Result<std::process::Output, String> {
    command.kill_on_drop(true);
    tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("{label} timed out after {}s", timeout.as_secs()))?
        .map_err(|e| format!("could not start {label}: {e}"))
}

async fn compile_tikz_svg(
    asset: &mathpreview_core::renderer::TikzAsset,
    root_dir: &Path,
    hash: &str,
) -> std::result::Result<Vec<u8>, String> {
    let work_path =
        std::env::temp_dir().join(format!("mathpreview-tikz-{}-{hash}", std::process::id()));
    if work_path.exists() {
        std::fs::remove_dir_all(&work_path)
            .map_err(|e| format!("clearing stale TikZ work directory: {e}"))?;
    }
    std::fs::create_dir_all(&work_path)
        .map_err(|e| format!("creating TikZ work directory: {e}"))?;
    let work = TikzWorkDir(work_path);
    let tex_path = work.0.join("diagram.tex");
    let pdf_path = work.0.join("diagram.pdf");
    let svg_path = work.0.join("diagram.svg");
    tokio::fs::write(&tex_path, tikz_document(asset))
        .await
        .map_err(|e| format!("writing isolated TikZ document: {e}"))?;

    let engine = select_tikz_engine(&asset.preamble);
    let mut tex = tokio::process::Command::new(engine);
    tex.args([
        "-interaction=nonstopmode",
        "-halt-on-error",
        "-no-shell-escape",
    ])
    .arg(format!("-output-directory={}", work.0.display()))
    .arg(&tex_path)
    .current_dir(root_dir)
    .stdin(Stdio::null());
    let tex_output = command_output_with_timeout(tex, TIKZ_COMPILE_TIMEOUT, engine).await?;
    if !tex_output.status.success() {
        return Err(format!(
            "{engine} failed:\n{}",
            tail_log(&tex_output.stdout, &tex_output.stderr)
        ));
    }
    if !pdf_path.is_file() {
        return Err(format!("{engine} succeeded but produced no diagram.pdf"));
    }

    let mut converter = tokio::process::Command::new("dvisvgm");
    converter
        .args(["--pdf", "--page=1", "--no-fonts", "--exact-bbox"])
        .arg(format!("--output={}", svg_path.display()))
        .arg(&pdf_path)
        .current_dir(root_dir)
        .stdin(Stdio::null());
    let converted = command_output_with_timeout(converter, TIKZ_CONVERT_TIMEOUT, "dvisvgm").await?;
    if !converted.status.success() {
        return Err(format!(
            "dvisvgm failed:\n{}",
            tail_log(&converted.stdout, &converted.stderr)
        ));
    }
    let svg = tokio::fs::read(&svg_path)
        .await
        .map_err(|e| format!("reading generated TikZ SVG: {e}"))?;
    let lower = String::from_utf8_lossy(&svg).to_ascii_lowercase();
    if [
        "<script",
        "javascript:",
        "onload=",
        "onerror=",
        "href=\"http",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return Err("generated SVG contained active or remote content".to_string());
    }
    Ok(svg)
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn tikz_error_svg(error: &str) -> Vec<u8> {
    let summary = error
        .lines()
        .find(|line| line.trim_start().starts_with('!'))
        .or_else(|| error.lines().rev().find(|line| !line.trim().is_empty()))
        .unwrap_or("TikZ compilation failed")
        .chars()
        .filter(|ch| !ch.is_control())
        .take(140)
        .collect::<String>();
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="760" height="92" viewBox="0 0 760 92" role="img" aria-label="TikZ preview unavailable"><rect x="1" y="1" width="758" height="90" rx="5" fill="#fff7f7" stroke="#c92a2a" stroke-dasharray="5 4"/><text x="18" y="34" font-family="system-ui,sans-serif" font-size="16" font-weight="600" fill="#a61e1e">TikZ preview unavailable</text><text x="18" y="62" font-family="ui-monospace,monospace" font-size="12" fill="#5f2120">{}</text></svg>"##,
        xml_escape(&summary),
    )
    .into_bytes()
}

/// Vendored MathJax 4 bundle, embedded into the binary at compile time
/// (~14 MB across ~100 files). `include_dir!` walks the directory at
/// build time and emits a static tree; `MATHJAX.get_file(rel)` returns
/// the file's bytes at runtime. This removes the previous runtime
/// dependency on `crates/cli/vendor/mathjax/` being reachable from the
/// binary's working directory — release binaries are now self-contained.
static MATHJAX: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/vendor/mathjax");

/// Body-text font woff2 files baked into the binary so the release
/// executable doesn't need its source checkout at runtime. NCM is small
/// (~800 KB total) and the explicit list survives editor renames better
/// than the directory walk used for MathJax above. See
/// `crates/cli/vendor/newcm-text/` for provenance.
const NEWCM_TEXT_FONTS: &[(&str, &[u8])] = &[
    (
        "woff2/WebCM Serif 10 Regular.woff2",
        include_bytes!("../vendor/newcm-text/woff2/WebCM Serif 10 Regular.woff2"),
    ),
    (
        "woff2/WebCM Serif 10 Italic.woff2",
        include_bytes!("../vendor/newcm-text/woff2/WebCM Serif 10 Italic.woff2"),
    ),
    (
        "woff2/WebCM Serif 10 Bold.woff2",
        include_bytes!("../vendor/newcm-text/woff2/WebCM Serif 10 Bold.woff2"),
    ),
    (
        "woff2/WebCM Serif 10 BoldItalic.woff2",
        include_bytes!("../vendor/newcm-text/woff2/WebCM Serif 10 BoldItalic.woff2"),
    ),
];

/// Content-type lookup keyed on file extension. Shared between the
/// embedded MathJax tree and the on-disk project asset path so both
/// serve `.js` / `.woff2` / `.svg` with the same MIME.
fn content_type_for_ext(ext: Option<&str>) -> &'static str {
    match ext {
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn vendor_response(bytes: &'static [u8], content_type: &'static str) -> Response {
    let mut response = Body::from(bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400, immutable"),
    );
    response
}

async fn serve_vendor_mathjax(AxumPath(path): AxumPath<String>) -> Response {
    let Some(rel) = clean_asset_path(&path) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(file) = MATHJAX.get_file(&rel) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = content_type_for_ext(rel.extension().and_then(|e| e.to_str()));
    vendor_response(file.contents(), content_type)
}

async fn serve_vendor_newcm_text(AxumPath(path): AxumPath<String>) -> Response {
    let Some(rel) = clean_asset_path(&path) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let rel_str = rel.to_string_lossy();
    let Some((_, bytes)) = NEWCM_TEXT_FONTS.iter().find(|(name, _)| *name == rel_str) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    vendor_response(bytes, "font/woff2")
}

async fn read_project_asset(
    root_dir: &Path,
    request_path: &str,
    preview_png: bool,
) -> std::result::Result<(Vec<u8>, &'static str), StatusCode> {
    let rel = clean_asset_path(request_path).ok_or(StatusCode::BAD_REQUEST)?;
    let root = tokio::fs::canonicalize(root_dir)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let path = root.join(rel);
    let canonical = tokio::fs::canonicalize(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if !canonical.starts_with(&root) {
        return Err(StatusCode::FORBIDDEN);
    }
    if preview_png && is_pdf(&canonical) {
        let preview = render_pdf_preview(&canonical).await?;
        let bytes = tokio::fs::read(&preview)
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;
        return Ok((bytes, "image/png"));
    }
    let content_type = asset_content_type(&canonical);
    let bytes = tokio::fs::read(&canonical)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((bytes, content_type))
}

fn clean_asset_path(path: &str) -> Option<PathBuf> {
    let path = path.trim_start_matches('/');
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

async fn render_pdf_preview(path: &Path) -> std::result::Result<PathBuf, StatusCode> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || render_pdf_preview_blocking(&path))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
}

fn render_pdf_preview_blocking(path: &Path) -> std::result::Result<PathBuf, StatusCode> {
    let metadata = std::fs::metadata(path).map_err(|_| StatusCode::NOT_FOUND)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    modified.hash(&mut hasher);
    "pdf-preview-png-v1-density-180".hash(&mut hasher);
    let hash = hasher.finish();

    let cache_dir = std::env::temp_dir().join("mathpreview-figure-cache");
    std::fs::create_dir_all(&cache_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let out = cache_dir.join(format!("{hash:016x}.png"));
    if out.exists() {
        return Ok(out);
    }
    let tmp = cache_dir.join(format!("{hash:016x}.{}.tmp.png", std::process::id()));
    let input = format!("{}[0]", path.display());
    let args = [
        "-density",
        "180",
        &input,
        "-trim",
        "+repage",
        "-background",
        "white",
        "-alpha",
        "remove",
        "-alpha",
        "off",
    ];
    let status = Command::new("magick")
        .args(args)
        .arg(&tmp)
        .status()
        .or_else(|_| Command::new("convert").args(args).arg(&tmp).status())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    match std::fs::rename(&tmp, &out) {
        Ok(()) => Ok(out),
        Err(_) if out.exists() => Ok(out),
        Err(_) => {
            let _ = std::fs::remove_file(&tmp);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

fn asset_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

async fn serve_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let needs_reload = websocket_needs_reload(&query);
    ws.on_upgrade(move |socket| handle_ws(socket, state, needs_reload))
}

fn websocket_needs_reload(query: &HashMap<String, String>) -> bool {
    query.get("v").is_none_or(|v| v != WS_PROTOCOL_VERSION)
}

async fn serve_cursor(
    State(state): State<AppState>,
    Json(req): Json<SourceRequest>,
) -> axum::http::StatusCode {
    touch_editor_contact(&state);
    let file = normalize_source_path(req.file);
    let line = req.line.max(1);
    let col = req.col.unwrap_or(1).max(1);
    // Jump detection for the nearest-element fallback below: a LARGE move (a
    // search wrapping to a preamble match, gg/G) is a deliberate relocation the
    // preview should follow; small moves on unrendered lines are just editing
    // (following those would yank the view to the top while tweaking macros).
    let is_jump = {
        let mut last = state.last_cursor.write().await;
        let jump = match last.as_ref() {
            Some((f, l)) => *f != file || l.abs_diff(line) >= 25,
            None => true,
        };
        *last = Some((file.clone(), line));
        jump
    };
    let current = state.current.read().await;
    // On a multi-row math block, track the cursor's ROW with the persistent band
    // (and per-row math) so you can see which align/gather line you're on.
    // Everywhere else (prose, sections, single equations), keep the original
    // flashing point highlight on the element under the cursor.
    let payload = if current.sync.math_rows_in_range(&file, line, line).is_empty() {
        let element_id = current
            .sync
            .lookup_leaf_by_source_position(&file, line, col)
            .map(|entry| entry.element_id.clone());
        // The point lookup deliberately excludes block-level elements (section
        // headings — SyncKind::Block) so they don't flash on a bare cursor
        // move. But the viewer must still FOLLOW the cursor onto them (e.g. a
        // search wrapping to a heading match), so fall back to the first
        // element on the line — and, on a jump, to the NEAREST element in the
        // file (a preamble line has nothing rendered on it at all; snap forward
        // to the document's first element) — scrolling without the flash.
        let (element_id, scroll_only) = match element_id {
            Some(id) => (Some(id), false),
            None => {
                let hit = current
                    .sync
                    .leaves_in_range(&file, line, 1, line, u32::MAX)
                    .into_iter()
                    .next()
                    .or_else(|| {
                        if is_jump {
                            current
                                .sync
                                .lookup_by_source_position(&file, line, col)
                                .map(|entry| entry.element_id.clone())
                        } else {
                            None
                        }
                    });
                (hit, true)
            }
        };
        serde_json::json!({
            "event": "source-cursor",
            "file": file,
            "line": line,
            "col": col,
            "element_id": element_id,
            "scroll_only": scroll_only,
            // The editor tags cursor moves caused by edits: the client then
            // follows the cursor WITHOUT the flash box (and without tearing
            // down a math row band that the mid-edit stale index may have
            // momentarily failed to resolve).
            "typing": req.typing.unwrap_or(false),
        })
    } else {
        let (element_ids, math_rows) =
            source_range_highlight(&current.sync, &file, line, 1, line, u32::MAX);
        serde_json::json!({
            "event": "source-range",
            "file": file,
            "element_ids": element_ids,
            "math_rows": math_rows,
            "typing": req.typing.unwrap_or(false),
        })
    };
    drop(current);
    let _ = state.tx.send(payload.to_string());
    axum::http::StatusCode::NO_CONTENT
}

/// `POST /selection` — map the editor's visual selection (a source range) to
/// every rendered leaf element it overlaps and broadcast a `source-range` event
/// so the browser highlights them. The range generalization of `serve_cursor`.
/// Resolve the leaf ids and per-row math hits for a source line/col range — the
/// shared core of the visual-selection highlight and the cursor-line highlight.
/// Drops the whole-block id of any multi-row math block we have per-row info on,
/// so the client highlights the individual rows instead.
fn source_range_highlight(
    sync: &SyncIndex,
    file: &std::path::Path,
    sl: u32,
    sc: u32,
    el: u32,
    ec: u32,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let mut ids = sync.leaves_in_range(file, sl, sc, el, ec);
    let hits = sync.math_rows_in_range(file, sl, el);
    let row_ids: std::collections::HashSet<&str> = hits.iter().map(|(id, _, _)| id.as_str()).collect();
    ids.retain(|id| !row_ids.contains(id.as_str()));
    let math_rows = hits
        .iter()
        .map(|(id, count, rows)| serde_json::json!({ "id": id, "count": count, "rows": rows }))
        .collect();
    (ids, math_rows)
}

/// `POST /search` — mirror the editor's active `/` pattern into the preview as
/// an hlsearch-style highlight (all matches). The plugin has already resolved
/// Vim's boundary/case atoms into explicit flags; the browser applies them
/// while matching the rendered DOM. Empty query / `clear` removes it.
async fn serve_search(
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> axum::http::StatusCode {
    touch_editor_contact(&state);
    let search = if req.clear.unwrap_or(false) {
        EditorSearch::default()
    } else {
        EditorSearch {
            query: req.query.unwrap_or_default(),
            whole_start: req.whole_start,
            whole_end: req.whole_end,
            case_sensitive: req.case_sensitive,
        }
    };
    // Remember it for WS-connect replay: a reloaded tab misses the broadcast,
    // and the plugin's dedup won't re-send an unchanged pattern.
    *state.search_query.write().await = search.clone();
    let payload = search_sync_payload(&search);
    let _ = state.tx.send(payload);
    axum::http::StatusCode::NO_CONTENT
}

async fn serve_selection(
    State(state): State<AppState>,
    Json(req): Json<RangeRequest>,
) -> axum::http::StatusCode {
    touch_editor_contact(&state);
    let file = normalize_source_path(req.file);
    let bounds = (req.start_line, req.start_col, req.end_line, req.end_col);
    let (element_ids, math_rows): (Vec<String>, Vec<serde_json::Value>) = match bounds {
        // A real range — resolve overlapping leaves. `end_col` defaults to "end
        // of line" (u32::MAX), not 1, so a linewise (V) selection covers the row.
        (Some(sl), sc, Some(el), ec) if !req.clear => {
            let current = state.current.read().await;
            source_range_highlight(
                &current.sync,
                &file,
                sl.max(1),
                sc.unwrap_or(1).max(1),
                el.max(1),
                ec.unwrap_or(u32::MAX),
            )
        }
        // clear == true, or incomplete bounds → empty lists dismiss the highlight.
        _ => (Vec::new(), Vec::new()),
    };
    let payload = serde_json::json!({
        "event": "source-range",
        "file": file,
        "element_ids": element_ids,
        "math_rows": math_rows,
    })
    .to_string();
    let _ = state.tx.send(payload);
    axum::http::StatusCode::NO_CONTENT
}

/// Resolve a backward-search click to its precise source position. A click on
/// a specific row of a multi-row math block arrives with the block's DOM id +
/// rendered row index/count; the always-fresh SyncIndex maps that to the
/// row's own line (and the column of its first token) via `math_row_pos`.
/// Everything else — prose clicks, unknown ids, mid-edit row-count skew, the
/// col-less row that starts on the `\begin` line — falls back to the block
/// anchor the client already sent, i.e. exactly the pre-row behavior.
async fn resolve_jump_pos(state: &AppState, req: &SourceRequest, file: &Path) -> (u32, u32) {
    let anchor = (req.line.max(1), req.col.unwrap_or(1).max(1));
    let (Some(id), Some(row), Some(count)) = (&req.element_id, req.math_row, req.row_count) else {
        return anchor;
    };
    let current = state.current.read().await;
    match current.sync.math_row_pos(id, file, row, count) {
        // A row can never sit ABOVE its own block's anchor (the `\begin` line
        // the clicked element's data-src points at). If it does, the id
        // matched a different block — duplicate `\label`s yield duplicate
        // element ids and the lookup returns the FIRST one — so trust the
        // clicked element's own anchor instead.
        Some((line, _)) if line < anchor.0 => anchor,
        // col 0 = row starts on the \begin line — same line as the anchor, so
        // the anchor's column is still the best position we know.
        Some((line, col)) => (line, if col > 0 { col } else { anchor.1 }),
        None => anchor,
    }
}

async fn serve_jump(
    State(state): State<AppState>,
    Json(req): Json<SourceRequest>,
) -> axum::http::StatusCode {
    let seq = state.jump_seq.fetch_add(1, Ordering::AcqRel) + 1;
    let file = normalize_source_path(req.file.clone());
    let (line, col) = resolve_jump_pos(&state, &req, &file).await;
    let jump = SourceJump {
        seq,
        file,
        line,
        col,
    };
    *state.pending_jump.write().await = Some(jump.clone());
    // Wake any parked long-poll so the editor jumps without waiting.
    state.jump_notify.notify_waiters();
    log_event(
        &state,
        "info",
        format!("source-jump → {}:{}", jump.file.display(), jump.line),
    );
    let payload = serde_json::json!({
        "event": "source-jump",
        "file": jump.file,
        "line": jump.line,
        "col": jump.col,
    })
    .to_string();
    let _ = state.tx.send(payload);
    axum::http::StatusCode::ACCEPTED
}

async fn serve_jump_poll(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // Mark that an editor is actively polling for jumps; reveal-source uses
    // both this timestamp and `active_jump_pollers` to avoid double-handling.
    state.last_jump_poll_ms.store(now_ms(), Ordering::Release);
    let _guard = PollerGuard::new(&state.active_jump_pollers);
    let after = query
        .get("after")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    // `wait` (ms) turns this into a long-poll: park until a newer jump
    // arrives or the deadline passes, instead of returning empty right away.
    // Old clients omit `wait` (=> 0) and keep the immediate check, so the
    // endpoint stays backward compatible. Clamp to a sane ceiling.
    let wait_ms = query
        .get("wait")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(JUMP_LONGPOLL_MAX_MS);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
    loop {
        // Arm the notification *before* checking, so a jump set between the
        // check and the await isn't lost (Notify holds one permit).
        let notified = state.jump_notify.notified();
        if let Some(jump) = state
            .pending_jump
            .read()
            .await
            .clone()
            .filter(|jump| jump.seq > after)
        {
            return Json(jump).into_response();
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return axum::http::StatusCode::NO_CONTENT.into_response();
        }
        tokio::select! {
            _ = notified => {}
            _ = tokio::time::sleep(remaining) => {
                return axum::http::StatusCode::NO_CONTENT.into_response();
            }
        }
    }
}

/// `POST /reveal-source` — spawn the configured editor command so the
/// user's editor jumps to a given `(file, line)`. Triggered from the
/// browser by Cmd/Ctrl-clicking on a rendered token. The command
/// template is whatever the daemon was started with (`--editor`); we
/// just substitute `{file}` / `{line}` / `{col}` and shell out via
/// `sh -c`. Returns 204 on success, 502 if the command exits non-zero
/// or fails to spawn (so the browser can surface a status pill).
async fn serve_reveal_source(
    State(state): State<AppState>,
    Json(req): Json<SourceRequest>,
) -> Response {
    let template = state.editor_cmd.as_str().trim();
    if template.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "reveal-source disabled (empty --editor)" })),
        )
            .into_response();
    }
    // If an nvim plugin is actively polling `/jump`, it already moves the
    // editor to the click target IN PLACE. Spawning the `--editor` command
    // too would reopen the file in a second buffer, so skip it. The browser
    // fires `/jump` and `/reveal-source` together; the poller path wins.
    let parked = state.active_jump_pollers.load(Ordering::Acquire) > 0;
    let since_poll = now_ms().saturating_sub(state.last_jump_poll_ms.load(Ordering::Acquire));
    if parked || since_poll < JUMP_POLLER_ACTIVE_MS {
        log_event_verbose(
            &state,
            "info",
            "reveal-source skipped — nvim /jump poller active".to_string(),
        );
        return StatusCode::NO_CONTENT.into_response();
    }
    let file = normalize_source_path(req.file.clone());
    let (line, col) = resolve_jump_pos(&state, &req, &file).await;
    let file_str = file.to_string_lossy();
    let cmd = template
        .replace("{file}", &shell_quote(&file_str))
        .replace("{line}", &line.to_string())
        .replace("{col}", &col.to_string());
    let result = tokio::process::Command::new("sh")
        .args(["-c", &cmd])
        .output()
        .await;
    match result {
        Ok(out) if out.status.success() => {
            log_event(
                &state,
                "info",
                format!("reveal-source → {}:{}", file_str, line),
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            log_event(
                &state,
                "warn",
                format!(
                    "reveal-source command exited {} ({}:{}): {stderr}",
                    out.status, file_str, line
                ),
            );
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("editor exited {}: {stderr}", out.status),
                })),
            )
                .into_response()
        }
        Err(e) => {
            log_event(&state, "warn", format!("reveal-source spawn failed: {e}"));
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("spawn failed: {e}") })),
            )
                .into_response()
        }
    }
}

/// Single-quote a string for use as one shell argument. Closes the
/// single-quoted run around any embedded `'` and re-opens it so the
/// result still parses as one POSIX shell token regardless of contents.
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Request body for `POST /macros/append`. `scope` is `"global"`,
/// `"project"`, or `"custom"`. When `scope == "custom"`, `path` must
/// hold the target file path; for the other two scopes the daemon
/// resolves the path itself (XDG / project root walk).
#[derive(Debug, Deserialize)]
struct MacrosAppendRequest {
    scope: String,
    line: String,
    /// Required when `scope == "custom"`; ignored otherwise. May be
    /// absolute or relative to the document root.
    #[serde(default)]
    path: Option<String>,
    /// When true, `line` is the *full* file contents and replaces the target
    /// file rather than being appended. Used by the dialog when it has loaded
    /// the existing macros for editing.
    #[serde(default)]
    replace: bool,
}

/// Request body for `POST /macros/read` — returns the current contents of the
/// macros override file for a scope so the dialog can pre-fill its editor.
#[derive(Debug, Deserialize)]
struct MacrosReadRequest {
    scope: String,
    #[serde(default)]
    path: Option<String>,
}

/// `POST /macros/read` — return the current macros override file's text (empty
/// if it doesn't exist yet) so the dialog can load existing definitions for
/// editing instead of starting blank.
async fn serve_macros_read(
    State(state): State<AppState>,
    Json(req): Json<MacrosReadRequest>,
) -> Response {
    let root_file = state.current.read().await.root_file.clone();
    let root_dir = root_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let target = match resolve_save_target(&req.scope, req.path.as_deref(), &root_dir) {
        Ok(p) => p,
        Err((code, msg)) => {
            return (code, Json(serde_json::json!({ "error": msg }))).into_response();
        }
    };
    let (content, exists) = match std::fs::read_to_string(&target) {
        Ok(s) => (s, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    Json(serde_json::json!({ "content": content, "file": target, "exists": exists }))
        .into_response()
}

/// Validate every command-shaped line in `content` (lines starting with `\`);
/// blank lines and `%` comments pass through. Returns the first rejection.
fn validate_override_content(content: &str) -> Result<(), String> {
    for (i, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        if line.starts_with('\\') {
            if let Err(e) = mathpreview_core::validate_override_line(line) {
                return Err(format!("line {}: {e}", i + 1));
            }
        }
    }
    Ok(())
}

/// Write `content` to `target`, creating parent dirs + the file as needed.
fn write_macro_content(target: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Normalize to a single trailing newline.
    let body = format!("{}\n", content.trim_end_matches(['\n', '\r']));
    std::fs::write(target, body)
}

/// `POST /macros/append` — append a `\newcommand` definition to the
/// global or project macros override file. Validates the line through
/// the extractor before writing so typos surface immediately; creates
/// parent directories + the file itself if either is missing.
///
/// After a successful write the daemon re-renders the current document
/// and broadcasts the result — same path the file watcher uses on save,
/// so the page picks up the new macro without a manual reload.
async fn serve_macros_append(
    State(state): State<AppState>,
    Json(req): Json<MacrosAppendRequest>,
) -> Response {
    let content = req.line.trim_end_matches(['\n', '\r']);
    if content.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "empty macro line" })),
        )
            .into_response();
    }

    // Validate: a single line when appending, every command line when
    // replacing the whole file.
    let macro_name = if req.replace {
        if let Err(msg) = validate_override_content(content) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response();
        }
        None
    } else {
        match mathpreview_core::validate_override_line(content) {
            Ok(n) => Some(n),
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e.to_string() })),
                )
                    .into_response();
            }
        }
    };

    let root_file = state.current.read().await.root_file.clone();
    let root_dir = root_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let target = match resolve_save_target(&req.scope, req.path.as_deref(), &root_dir) {
        Ok(p) => p,
        Err((code, msg)) => {
            return (code, Json(serde_json::json!({ "error": msg }))).into_response();
        }
    };

    let write_result = if req.replace {
        write_macro_content(&target, content)
    } else {
        append_macro_line(&target, content)
    };
    if let Err(e) = write_result {
        log_event(&state, "error", format!("macros write failed: {e:#}"));
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    // Trigger a re-render so the change is picked up live.
    if let Err(e) = trigger_rerender(&state, &root_file).await {
        log_event(&state, "warn", format!("macros write re-render failed: {e:#}"));
    }

    let verb = if req.replace { "wrote" } else { "appended" };
    log_event(
        &state,
        "info",
        format!("macros: {verb} {} (scope={})", target.display(), req.scope),
    );
    Json(serde_json::json!({
        "name": macro_name,
        "file": target,
        "replaced": req.replace,
    }))
    .into_response()
}

/// Request body for `POST /config/set`. `scope` is `"project"` |
/// `"global"` | `"custom"`; `path` is required only when `scope ==
/// "custom"`. `values` is a flat map of dotted keys to JSON-typed
/// values: `{"viewer.font-size": 22, "viewer.source-jump.trigger":
/// "double-click"}`. Numbers serialize as TOML integers, strings as
/// TOML strings, booleans as TOML booleans.
#[derive(Debug, Deserialize)]
struct ConfigSetRequest {
    scope: String,
    #[serde(default)]
    path: Option<String>,
    values: std::collections::BTreeMap<String, serde_json::Value>,
}

/// `POST /config/set` — write one or more config fields to the chosen
/// TOML file (global / project / custom path), preserving any existing
/// formatting + comments via `toml_edit`. After a successful write the
/// daemon re-renders so the new defaults flow into the HTML on the
/// next reload.
async fn serve_config_set(
    State(state): State<AppState>,
    Json(req): Json<ConfigSetRequest>,
) -> Response {
    if req.values.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "no values to set" })),
        )
            .into_response();
    }
    let root_file = state.current.read().await.root_file.clone();
    let root_dir = root_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let target = match resolve_config_save_target(&req.scope, req.path.as_deref(), &root_dir) {
        Ok(p) => p,
        Err((code, msg)) => {
            return (code, Json(serde_json::json!({ "error": msg }))).into_response();
        }
    };

    let existing = match std::fs::read_to_string(&target) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("read {} failed: {e}", target.display())
                })),
            )
                .into_response();
        }
    };
    let mut doc: toml_edit::DocumentMut = match existing.parse() {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("parse {} failed: {e}", target.display())
                })),
            )
                .into_response();
        }
    };
    for (dotted, value) in &req.values {
        if let Err(msg) = set_dotted_key(&mut doc, dotted, value) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response();
        }
    }

    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("mkdir {} failed: {e}", parent.display())
                })),
            )
                .into_response();
        }
    }
    if let Err(e) = std::fs::write(&target, doc.to_string()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("write {} failed: {e}", target.display())
            })),
        )
            .into_response();
    }

    // Re-read the config cascade from disk so the live `viewer_config`
    // reflects the value we just wrote. Failures here are non-fatal:
    // the file was saved successfully; we just couldn't refresh in
    // place. The user can restart the daemon to pick it up.
    match mathpreview_core::load_and_merge_config(state.config_paths.as_ref()) {
        Ok((resolved, _)) => {
            *state.viewer_config.write().await = resolved.viewer;
        }
        Err(e) => {
            log_event(&state, "error", format!("config reload failed: {e:#}"));
        }
    }

    if let Err(e) = trigger_rerender(&state, &root_file).await {
        log_event(&state, "warn", format!("config-set re-render failed: {e:#}"));
    }

    log_event(
        &state,
        "info",
        format!(
            "config: wrote {} field(s) to {}",
            req.values.len(),
            target.display(),
        ),
    );
    Json(serde_json::json!({ "file": target, "fields": req.values.len() })).into_response()
}

/// Request body for `POST /config/read` / `POST /config/write`.
#[derive(Debug, Deserialize)]
struct ConfigFileRequest {
    scope: String,
    #[serde(default)]
    path: Option<String>,
    /// Whole-file contents, for `/config/write` only.
    #[serde(default)]
    content: String,
    /// Structured viewer controls merged into `content` before validation.
    /// This keeps the convenient form and the advanced TOML editor on one
    /// atomic, comment-preserving save path.
    #[serde(default)]
    values: std::collections::BTreeMap<String, serde_json::Value>,
}

/// `POST /config/read` — return the config TOML file's text for a scope
/// (empty if absent), so the macros dialog's Text→HTML mode can load it.
async fn serve_config_read(
    State(state): State<AppState>,
    Json(req): Json<ConfigFileRequest>,
) -> Response {
    let root_file = state.current.read().await.root_file.clone();
    let root_dir = root_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let target = match resolve_config_save_target(&req.scope, req.path.as_deref(), &root_dir) {
        Ok(p) => p,
        Err((code, msg)) => {
            return (code, Json(serde_json::json!({ "error": msg }))).into_response();
        }
    };
    let (content, exists) = match std::fs::read_to_string(&target) {
        Ok(s) => (s, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    Json(serde_json::json!({ "content": content, "file": target, "exists": exists }))
        .into_response()
}

/// `POST /config/write` — replace the whole config TOML file with `content`
/// after checking it parses as TOML, then reload + re-render. Used by the
/// macros dialog's Text→HTML mode editing the `.mathpreview.toml` directly.
async fn serve_config_write(
    State(state): State<AppState>,
    Json(req): Json<ConfigFileRequest>,
) -> Response {
    let root_file = state.current.read().await.root_file.clone();
    let root_dir = root_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let target = match resolve_config_save_target(&req.scope, req.path.as_deref(), &root_dir) {
        Ok(p) => p,
        Err((code, msg)) => {
            return (code, Json(serde_json::json!({ "error": msg }))).into_response();
        }
    };
    // Merge the common controls into the user's editor text in memory, then
    // validate and write once. Existing project-local comments, text macros,
    // advanced fields, and keybindings therefore survive a form-based save.
    let merged_content = match merge_config_editor_values(&req.content, &req.values, &target) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("mkdir {} failed: {e}", parent.display()) })),
            )
                .into_response();
        }
    }
    let body = format!("{}\n", merged_content.trim_end_matches(['\n', '\r']));
    if let Err(e) = std::fs::write(&target, body) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("write {} failed: {e}", target.display()) })),
        )
            .into_response();
    }
    // Refresh the live viewer config (text-macros reload per render anyway).
    if let Ok((resolved, _)) = mathpreview_core::load_and_merge_config(state.config_paths.as_ref())
    {
        *state.viewer_config.write().await = resolved.viewer;
    }
    if let Err(e) = trigger_rerender(&state, &root_file).await {
        log_event(
            &state,
            "warn",
            format!("config-write re-render failed: {e:#}"),
        );
    }
    let target_canonical = target.canonicalize().ok();
    let active = state.config_paths.iter().any(|path| {
        path == &target
            || target_canonical
                .as_ref()
                .is_some_and(|target_path| path.canonicalize().ok().as_ref() == Some(target_path))
    });
    log_event(
        &state,
        "info",
        format!("config: wrote {}", target.display()),
    );
    Json(serde_json::json!({ "file": target, "active": active })).into_response()
}

fn validate_config_file(content: &str, label: &Path) -> Result<(), String> {
    mathpreview_core::Config::parse(content, label)
        .map(|_| ())
        .map_err(|e| format!("invalid config: {e:#}"))
}

fn merge_config_editor_values(
    content: &str,
    values: &std::collections::BTreeMap<String, serde_json::Value>,
    label: &Path,
) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|e| format!("invalid config: parsing TOML {}: {e}", label.display()))?;
    for (dotted, value) in values {
        set_dotted_key(&mut doc, dotted, value)?;
    }
    let merged = doc.to_string();
    validate_config_file(&merged, label)?;
    Ok(merged)
}

/// Resolve the target file for a config-save action. Same scope rules
/// as macros, but the path falls back to the relevant `*.toml` file
/// when one isn't found by walking up.
fn resolve_config_save_target(
    scope: &str,
    custom_path: Option<&str>,
    root_dir: &Path,
) -> Result<PathBuf, (StatusCode, String)> {
    match scope {
        "global" => global_config_path().ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot resolve global config path (HOME / XDG_CONFIG_HOME unset)".to_string(),
        )),
        "project" => Ok(find_project_config_path(root_dir)
            .unwrap_or_else(|| root_dir.join(mathpreview_core::config::PROJECT_CONFIG_FILENAME))),
        // "custom" and any unrecognized scope resolve the caller-supplied path.
        _ => resolve_save_target(scope, custom_path, root_dir),
    }
}

fn global_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("mathpreview").join("config.toml"))
}

fn find_project_config_path(start: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let candidate = dir.join(mathpreview_core::config::PROJECT_CONFIG_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    None
}

/// Walk `doc` along `dotted` (e.g. `"viewer.source-jump.trigger"`) and
/// set the leaf key to the TOML value derived from the JSON `value`.
/// Creates intermediate tables as needed. Returns `Err` on
/// type-mismatch (JSON null, array, or object), which the dialog
/// hasn't been designed to send.
fn set_dotted_key(
    doc: &mut toml_edit::DocumentMut,
    dotted: &str,
    value: &serde_json::Value,
) -> Result<(), String> {
    let segments: Vec<&str> = dotted.split('.').collect();
    if segments.len() < 2 {
        return Err(format!("expected a dotted key like \"viewer.font-size\"; got {dotted:?}"));
    }
    let (leaf, parents) = segments.split_last().unwrap();
    let mut cur: &mut toml_edit::Item = doc.as_item_mut();
    for seg in parents {
        let table = cur.as_table_like_mut().ok_or_else(|| {
            format!("path through {seg} is not a TOML table")
        })?;
        if !table.contains_key(seg) {
            table.insert(seg, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        cur = table.get_mut(seg).expect("just inserted");
    }
    let table = cur
        .as_table_like_mut()
        .ok_or_else(|| format!("path through {} is not a TOML table", parents.join(".")))?;
    let toml_value = json_to_toml_value(value)?;
    table.insert(leaf, toml_edit::Item::Value(toml_value));
    Ok(())
}

fn json_to_toml_value(v: &serde_json::Value) -> Result<toml_edit::Value, String> {
    use serde_json::Value::*;
    match v {
        String(s) => Ok(s.clone().into()),
        Bool(b) => Ok((*b).into()),
        Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into())
            } else {
                Err(format!("unsupported numeric value: {n}"))
            }
        }
        Null | Array(_) | Object(_) => Err(format!(
            "config values must be string / number / bool; got {v}",
        )),
    }
}

/// Request body for `POST /macros/register`. `path` may be absolute,
/// `~/...`, or relative to the document root; the daemon resolves it
/// to an absolute path before storing.
#[derive(Debug, Deserialize)]
struct MacrosRegisterRequest {
    path: String,
}

/// `POST /macros/register` — add `path` to the runtime override cascade.
/// The file is watched (so edits hot-reload), included in the cache
/// fingerprint, and merged into the override list on every render.
/// Idempotent: registering the same path twice is a no-op.
async fn serve_macros_register(
    State(state): State<AppState>,
    Json(req): Json<MacrosRegisterRequest>,
) -> Response {
    let raw = req.path.trim();
    if raw.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "empty path" })),
        )
            .into_response();
    }
    let root_file = state.current.read().await.root_file.clone();
    let root_dir = root_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    // Reuse the same path-resolution rules as scope=custom so the
    // user can paste the exact same string into either input.
    let target = match resolve_save_target("custom", Some(raw), &root_dir) {
        Ok(p) => p,
        Err((code, msg)) => {
            return (code, Json(serde_json::json!({ "error": msg }))).into_response();
        }
    };

    {
        let mut session = state.session_macros.write().await;
        if !session.iter().any(|p| p == &target) {
            session.push(target.clone());
        }
    }

    if let Err(e) = trigger_rerender(&state, &root_file).await {
        log_event(&state, "warn", format!("macros register re-render failed: {e:#}"));
    }

    log_event(
        &state,
        "info",
        format!("macros: registered override {}", target.display()),
    );
    Json(serde_json::json!({ "file": target })).into_response()
}

/// Resolve a save target from a wire `scope` + optional `path`. Shared
/// by both the macros and the config dialogs since both surface the
/// same Project / Global / Custom radio. Returns an HTTP-status-paired
/// error message ready to be turned into a 4xx response.
fn resolve_save_target(
    scope: &str,
    custom_path: Option<&str>,
    root_dir: &Path,
) -> Result<PathBuf, (StatusCode, String)> {
    match scope {
        "global" => mathpreview_core::resolve_override_path(
            root_dir,
            mathpreview_core::MacrosScope::Global,
        )
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot resolve global path (HOME / XDG_CONFIG_HOME unset)".to_string(),
        )),
        "project" => mathpreview_core::resolve_override_path(
            root_dir,
            mathpreview_core::MacrosScope::Project,
        )
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot resolve project path".to_string(),
        )),
        "custom" => {
            let raw = custom_path
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or((
                    StatusCode::BAD_REQUEST,
                    "scope=custom requires a non-empty path".to_string(),
                ))?;
            // Expand `~` to $HOME so the dialog accepts the same shape
            // the user types in a shell.
            let expanded: PathBuf = if let Some(rest) = raw.strip_prefix("~/") {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join(rest)
            } else if raw == "~" {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default()
            } else {
                PathBuf::from(raw)
            };
            // Relative paths anchor at the document root so a user can
            // type "macros/extra.tex" without thinking about CWD.
            let absolute = if expanded.is_absolute() {
                expanded
            } else {
                root_dir.join(expanded)
            };
            Ok(absolute)
        }
        other => Err((
            StatusCode::BAD_REQUEST,
            format!(r#"unknown scope: {other}; expected "global", "project", or "custom""#),
        )),
    }
}

/// Append a macro line to `target`, creating parent dirs + the file
/// itself if missing. Adds a trailing newline so the next append lands
/// on a fresh line even if the user's text editor didn't add one.
fn append_macro_line(target: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(target)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Re-render `root_file` and broadcast the result. Mirrors the
/// file-change handler but driven by an HTTP action (the dialog's Save
/// button) instead of a filesystem event.
async fn trigger_rerender(state: &AppState, root_file: &Path) -> anyhow::Result<()> {
    let seq = begin_render_attempt(state);
    let (out, _timing) = render_cached(state, root_file).await?;
    update_watched(state, &out).await;
    let _ = broadcast_render(state, out, seq).await;
    Ok(())
}

/// `POST /print` — runs `latexmk -pdf` on the project's root file and
/// streams the produced PDF back. We trust `.latexmkrc` for build
/// settings (engine choice, `$out_dir`, `$aux_dir`, etc.) and parse the
/// run log to discover where it actually wrote the PDF, so a project
/// with a custom output directory works the same as a default layout.
async fn serve_print(State(state): State<AppState>) -> Response {
    let root_file = state.current.read().await.root_file.clone();
    elog!("mathpreview: print latexmk ({})", root_file.display());
    match compile_pdf_via_latexmk(&root_file).await {
        Ok(bytes) => {
            let filename = root_file
                .file_stem()
                .map(|s| format!("{}.pdf", s.to_string_lossy()))
                .unwrap_or_else(|| "output.pdf".to_string());
            let mut response = Body::from(bytes).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/pdf"),
            );
            // `inline` so the browser opens it in its PDF viewer rather
            // than triggering a download dialog.
            response.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!("inline; filename=\"{filename}\""))
                    .unwrap_or_else(|_| HeaderValue::from_static("inline")),
            );
            response
        }
        Err(msg) => {
            elog!("mathpreview: print compile failed: {msg}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response()
        }
    }
}

/// Run `latexmk -pdf` on the root file in its own directory. Returns the
/// PDF bytes on success or a human-readable error string on failure.
/// We rely on `latexmk` (rather than calling `pdflatex` directly) because
/// it handles bib runs, multi-pass references, and any `.latexmkrc` in
/// the project that may redirect output via `$out_dir` / `$aux_dir`.
/// The output PDF path is discovered by parsing the run log rather than
/// guessing common subdirectories: `.latexmkrc` is free to put the PDF
/// anywhere (`build/`, `out/`, `_artifacts/2026-05/`, …) and the log
/// always tells us where it actually landed. If `latexmk` isn't
/// installed we fall back to a single `pdflatex` pass — enough for
/// trivial papers; won't resolve `\cite{}` correctly.
async fn compile_pdf_via_latexmk(root: &Path) -> Result<Vec<u8>, String> {
    let dir = root
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let file = root
        .file_name()
        .ok_or_else(|| "root file has no name component".to_string())?;
    let stem = root
        .file_stem()
        .ok_or_else(|| "root file has no stem".to_string())?;

    // Try latexmk first.
    let latexmk = tokio::process::Command::new("latexmk")
        .args([
            "-pdf",
            "-interaction=nonstopmode",
            "-halt-on-error",
            "-synctex=1",
        ])
        .arg(file)
        .current_dir(&dir)
        .output()
        .await;

    let used_fallback;
    let status_ok;
    let stdout_bytes;
    let stderr_bytes;
    match latexmk {
        Ok(out) => {
            used_fallback = false;
            status_ok = out.status.success();
            stdout_bytes = out.stdout;
            stderr_bytes = out.stderr;
        }
        Err(_) => {
            // latexmk missing — try pdflatex.
            let pdflatex = tokio::process::Command::new("pdflatex")
                .args(["-interaction=nonstopmode", "-halt-on-error"])
                .arg(file)
                .current_dir(&dir)
                .output()
                .await
                .map_err(|e| format!("neither latexmk nor pdflatex is on $PATH: {e}"))?;
            used_fallback = true;
            status_ok = pdflatex.status.success();
            stdout_bytes = pdflatex.stdout;
            stderr_bytes = pdflatex.stderr;
        }
    }

    if !status_ok {
        let tool = if used_fallback { "pdflatex" } else { "latexmk" };
        return Err(format!(
            "{tool} failed:\n{}",
            tail_log(&stdout_bytes, &stderr_bytes)
        ));
    }

    let stdout_str = String::from_utf8_lossy(&stdout_bytes);
    let stem_str = stem.to_string_lossy();
    let pdf = locate_compiled_pdf(&stdout_str, &dir, &stem_str).ok_or_else(|| {
        format!(
            "compile succeeded but no output PDF found (parsed log + checked {}/, {}/build/, {}/out/, {}/_build/, {}/_output/)",
            dir.display(),
            dir.display(),
            dir.display(),
            dir.display(),
            dir.display(),
        )
    })?;

    tokio::fs::read(&pdf)
        .await
        .map_err(|e| format!("reading {}: {e}", pdf.display()))
}

/// Discover the produced PDF path. Strategy, in order:
///   1. Scan the latexmk/pdflatex stdout for the lines they always emit
///      with the resolved output path:
///        - `Latexmk: All targets (<path>.pdf) are up-to-date`  (no-op run)
///        - `Output written on <path>.pdf (N pages, ...).`      (pdflatex)
///          These honour every `$out_dir` / `$aux_dir` setting the user
///          put in `.latexmkrc`, so we don't have to model latexmk's
///          config language ourselves.
///   2. Fall back to a small set of common subdirectories
///      (`./`, `build/`, `out/`, `_build/`, `_output/`) for the case
///      where the log was empty or the regex missed.
fn locate_compiled_pdf(stdout: &str, dir: &Path, stem: &str) -> Option<PathBuf> {
    let try_path = |raw: &str| -> Option<PathBuf> {
        let trimmed = raw.trim().trim_matches('\'').trim_matches('"').trim();
        if trimmed.is_empty() || !trimmed.ends_with(".pdf") {
            return None;
        }
        let p = Path::new(trimmed);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        };
        abs.is_file().then_some(abs)
    };

    for line in stdout.lines() {
        let line = line.trim();
        // pdflatex's terminal status line is the most authoritative
        // ground truth: it names the exact file it just wrote.
        if let Some(rest) = line.strip_prefix("Output written on ") {
            let path_part = rest.split(" (").next().unwrap_or(rest);
            if let Some(found) = try_path(path_part) {
                return Some(found);
            }
        }
        // latexmk's no-op message when everything is already up to date —
        // pdflatex isn't run, so the "Output written" line is missing,
        // but latexmk still names the target.
        if let Some(rest) = line.strip_prefix("Latexmk: All targets (") {
            if let Some(end) = rest.find(')') {
                if let Some(found) = try_path(&rest[..end]) {
                    return Some(found);
                }
            }
        }
    }

    for sub in &["", "build", "out", "_build", "_output"] {
        let candidate = if sub.is_empty() {
            dir.join(format!("{stem}.pdf"))
        } else {
            dir.join(sub).join(format!("{stem}.pdf"))
        };
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn tail_log(stdout: &[u8], stderr: &[u8]) -> String {
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);
    let combined = format!("{out}{err}");
    let lines: Vec<&str> = combined.lines().collect();
    let tail = if lines.len() > 40 {
        &lines[lines.len() - 40..]
    } else {
        &lines[..]
    };
    tail.join("\n")
}

fn normalize_source_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn normalize_watch_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

async fn serve_restart(State(state): State<AppState>) -> axum::http::StatusCode {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            log_event(&state, "error", format!("restart failed: cannot resolve current exe: {e}"));
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    match restart_command(&exe, &args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => {
            log_event(&state, "info", "restarting server".to_string());
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                std::process::exit(0);
            });
            axum::http::StatusCode::ACCEPTED
        }
        Err(e) => {
            log_event(&state, "error", format!("restart failed: {e}"));
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn serve_stop(State(state): State<AppState>) -> axum::http::StatusCode {
    log_event(&state, "info", "stopping server".to_string());
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(100));
        std::process::exit(0);
    });
    axum::http::StatusCode::ACCEPTED
}

#[cfg(unix)]
fn restart_command(exe: &Path, args: &[String]) -> Command {
    let mut command = Command::new(exe);
    command
        .args(args)
        .env("MATHPREVIEW_RESTART_DELAY_MS", "350");
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

#[cfg(not(unix))]
fn restart_command(exe: &Path, args: &[String]) -> Command {
    let mut command = Command::new(exe);
    command
        .args(args)
        .env("MATHPREVIEW_RESTART_DELAY_MS", "350");
    command
}

/// `POST /buffer` — editor pushes the current buffer content. The pushed file
/// is identified via the `X-Mathpreview-Path` header. If absent, the daemon
/// assumes the root file. Root and watched included-file pushes are kept as
/// in-memory overrides, then the real root project is re-rendered with those
/// overrides spliced through `\input` / `\include` / `\subfile`.
async fn serve_buffer_push(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::http::StatusCode {
    touch_editor_contact(&state);
    let path_header = headers
        .get("x-mathpreview-path")
        .and_then(|v| v.to_str().ok())
        .map(PathBuf::from);

    let current_root = {
        let current = state.current.read().await;
        current.root_file.clone()
    };
    let pushed_path = match path_header.clone() {
        Some(p) => p.canonicalize().unwrap_or(p),
        None => current_root.clone(),
    };
    let is_known_project_file = {
        let watched = state.watched.read().await;
        pushed_path == current_root || watched.contains(&pushed_path)
    };
    if !is_known_project_file {
        elog!(
            "mathpreview: rejected buffer-push for {}; server root is {}",
            pushed_path.display(),
            current_root.display(),
        );
        return axum::http::StatusCode::BAD_REQUEST;
    }

    let t0 = std::time::Instant::now();
    let body_len = body.len();
    let seq = begin_render_attempt(&state);
    let cursor = buffer_cursor_from_headers(&headers);
    if !is_buffer_renderable(&body, cursor) {
        elog!(
            "mathpreview: buffer-push #{seq} {} bytes — incomplete, deferring",
            body_len
        );
        return axum::http::StatusCode::ACCEPTED;
    }

    {
        let mut overrides = state.buffer_overrides.write().await;
        overrides.insert(pushed_path.clone(), body);
    }

    match render_cached(&state, &current_root).await {
        Ok((out, timing)) => {
            if !is_latest_render_attempt(&state, seq) {
                elog!("mathpreview: buffer-push #{seq} {body_len}b → stale render discarded");
                return axum::http::StatusCode::NO_CONTENT;
            }
            update_watched(&state, &out).await;
            let (op_count, kind) = broadcast_render(&state, out, seq).await;
            let mem = {
                let blocks = state.last_blocks.read().await;
                fmt_mem_log(&state, &blocks)
            };
            log_event_verbose(
                &state,
                "info",
                format!(
                    "buffer-push #{seq} {body_len}b → {tot}ms ({op_count} {kind}; body-parse {bp}, render {r}; cache {cache}; {mem})",
                    tot = t0.elapsed().as_millis(),
                    bp = timing.body_parse_ms,
                    r = timing.render_ms,
                    cache = if timing.cache_hit { "hit" } else { "miss" },
                ),
            );
            axum::http::StatusCode::NO_CONTENT
        }
        Err(e) => {
            if !is_latest_render_attempt(&state, seq) {
                log_event_verbose(
                    &state,
                    "warn",
                    format!("buffer-push #{seq} stale render error discarded: {e}"),
                );
                return axum::http::StatusCode::NO_CONTENT;
            }
            log_event(&state, "error", format!("buffer-push render: {e:#}"));
            let payload = serde_json::json!({
                "event": "error",
                "message": format!("{e}"),
            })
            .to_string();
            let _ = state.tx.send(payload);
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        }
    }
}

fn watched_set(out: &RenderOutput, extras: &[PathBuf]) -> HashSet<PathBuf> {
    let mut watched = HashSet::new();
    watched.insert(out.root_file.clone());
    for f in &out.included_files {
        watched.insert(f.clone());
    }
    // Macro override files (global + project + --macros) participate in
    // live-reload too: editing them in your editor — or appending via
    // the macros dialog — should re-render the paper. We watch even
    // paths that don't exist yet, so the very first `Save` from the
    // dialog (which creates the file) is still seen by the watcher.
    for f in extras {
        watched.insert(f.clone());
    }
    watched
}

async fn update_watched(state: &AppState, out: &RenderOutput) {
    let effective = effective_override_paths(state).await;
    let mut extras = effective;
    extras.extend(state.config_paths.iter().cloned());
    let watched = watched_set(out, &extras);
    {
        let mut guard = state.watched.write().await;
        *guard = watched.clone();
    }
    let _ = state.watch_tx.send(watched);
}

#[derive(Default, Debug)]
struct RenderTiming {
    parse_ms: u128,
    preamble_ms: u128,
    body_parse_ms: u128,
    number_ms: u128,
    render_ms: u128,
    cache_hit: bool,
}

async fn render_cached(
    state: &AppState,
    root: &Path,
) -> anyhow::Result<(RenderOutput, RenderTiming)> {
    let mut t = RenderTiming::default();
    let t0 = std::time::Instant::now();
    let overrides = state.buffer_overrides.read().await.clone();
    let project = if overrides.is_empty() {
        project::load_project(root)?
    } else {
        project::load_project_with_overrides(root, &overrides)?
    };
    t.parse_ms = t0.elapsed().as_millis();

    // Preamble + bib + style — cached on every root/included preamble source
    // *and* the override-file fingerprint, so an edit to an `\input` preamble
    // or a fresh `POST /macros/append` invalidates the cache cleanly. Editing
    // only the body keeps both hashes identical, which is the common-case hit.
    let pre_hash = preamble_fingerprint(&project);
    let effective_paths = effective_override_paths(state).await;
    let overrides = load_override_layers(state, &effective_paths).await;
    let overrides_hash = fingerprint_overrides(&effective_paths, &overrides);
    let t1 = std::time::Instant::now();
    let (preamble, bib, bib_style) = {
        let guard = state.preamble_cache.read().await;
        if let Some(c) = guard
            .as_ref()
            .filter(|c| c.hash == pre_hash && c.overrides_hash == overrides_hash)
        {
            t.cache_hit = true;
            (c.preamble.clone(), c.bib.clone(), c.bib_style)
        } else {
            drop(guard);
            let preamble =
                macros::extract_preamble_with_overrides(&project, &overrides)?;
            let bib = bibtex::load_project_bib(&project)?;
            let bib_style = bibtex::detect_bib_style(&preamble.raw_preamble);
            *state.preamble_cache.write().await = Some(PreambleCache {
                hash: pre_hash,
                overrides_hash,
                preamble: preamble.clone(),
                bib: bib.clone(),
                bib_style,
            });
            (preamble, bib, bib_style)
        }
    };
    t.preamble_ms = t1.elapsed().as_millis();

    let thms = TheoremRegistry::from_project(&project);

    let t2 = std::time::Instant::now();
    let mut body = parser::parse_body(&project, &thms)?;
    t.body_parse_ms = t2.elapsed().as_millis();

    let t3 = std::time::Instant::now();
    // mathtools `showonlyrefs`: collect referenced keys so numbering can
    // suppress numbers on unreferenced equations (mirrors lib.rs
    // finish_render — this is the daemon's live-render twin of that path).
    let referenced = preamble.show_only_refs.then(|| {
        let mut keys = std::collections::HashSet::new();
        for f in &project.files {
            numbering::collect_referenced_keys(&f.source, &mut keys);
        }
        keys
    });
    let labels = numbering::assign_numbers(&mut body, &bib, bib_style, &thms, referenced);
    t.number_ms = t3.elapsed().as_millis();

    let t4 = std::time::Instant::now();
    let mut sync = SyncIndex::new();
    // Reload the config cascade from disk on every render so an edit
    // to `.mathpreview.toml` in your editor (or a `POST /config/set`
    // from the dialog) takes effect without restarting the daemon.
    // Routed through the per-daemon mtime cache so unchanged files
    // skip disk I/O on the hot path.
    // Reloaded each render so an edit to `[text-macros]` takes effect live.
    let mut live_text_macros = state.opts.text_macros.clone();
    match load_and_merge_config_cached(state).await {
        Ok(resolved) => {
            live_text_macros = resolved.text_macros.clone();
            let mut guard = state.viewer_config.write().await;
            if *guard != resolved.viewer {
                *guard = resolved.viewer.clone();
                drop(guard);
                log_event(
                    state,
                    "info",
                    format!(
                        "config reloaded: font-size={}, ui-font-size={}, source-jump-trigger={}, default-page-mode={}, default-theme={}",
                        resolved.viewer.font_size,
                        resolved.viewer.ui_font_size,
                        resolved.viewer.source_jump_trigger.as_str(),
                        resolved.viewer.default_page_mode.as_str(),
                        resolved.viewer.default_theme.as_str(),
                    ),
                );
            }
        }
        Err(e) => {
            log_event(state, "error", format!("config parse failed: {e:#}"));
        }
    }
    let live_viewer_config = state.viewer_config.read().await.clone();
    let mut render_opts = state.opts.clone();
    render_opts.viewer_config = live_viewer_config;
    render_opts.text_macros = live_text_macros;
    render_opts.latex_preamble = Some(project.preamble.source.clone());
    let rendered = renderer::render(
        &body,
        &preamble,
        &labels,
        &bib,
        bib_style,
        &mut sync,
        &render_opts,
    );
    t.render_ms = t4.elapsed().as_millis();

    let included_files = project.included_files().map(PathBuf::from).collect();
    let out = RenderOutput {
        html: rendered.full,
        body_html: rendered.body,
        blocks: rendered.blocks,
        sync,
        root_file: root.to_path_buf(),
        preamble,
        included_files,
        tikz_assets: rendered.tikz_assets,
    };
    Ok((out, t))
}

async fn handle_ws(socket: WebSocket, state: AppState, needs_reload: bool) {
    let (mut sender, mut receiver) = socket.split();
    if needs_reload {
        let payload = serde_json::json!({ "event": "full-reload" }).to_string();
        let _ = sender.send(Message::Text(payload)).await;
    }
    // Replay the editor's active search so a freshly-(re)loaded tab shows the
    // highlight — it missed the original broadcast, and the plugin's dedup
    // won't re-send an unchanged pattern.
    {
        let search = state.search_query.read().await.clone();
        if !search.query.is_empty() {
            let payload = search_sync_payload(&search);
            let _ = sender.send(Message::Text(payload)).await;
        }
    }
    let mut rx = state.tx.subscribe();
    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(payload) => {
                    if sender.send(Message::Text(payload)).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // The client fell behind and missed delta patches. Applying
                    // later patches against its now-stale DOM would corrupt it,
                    // so force a full reload to resync from scratch instead of
                    // silently continuing.
                    let payload = serde_json::json!({ "event": "full-reload" }).to_string();
                    if sender.send(Message::Text(payload)).await.is_err() {
                        break;
                    }
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = receiver.next() => match msg {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => continue, // forward-search reception lands here in a future step
                Some(Err(_)) => break,
            },
        }
    }
}

/// Diff the new render against `last_blocks`, broadcast either a patch
/// event (when the change is small relative to the document) or a full
/// `body-updated` event (when too much changed to be worth a patch), and
/// update `last_blocks` and `current`. Returns `(op_count, "ops"|"blocks (full)")`
/// for logging.
async fn broadcast_render(state: &AppState, out: RenderOutput, seq: u64) -> (usize, &'static str) {
    // The current viewer config rides along with every render so the
    // client can re-apply `--body-font-size` and `__mpConfig` values
    // live — `.mathpreview.toml` edits or `POST /config/set` no
    // longer require a tab reload to take effect. Snapshot it before taking
    // the broadcast lock to avoid nesting lock acquisitions.
    let viewer_config = state.viewer_config.read().await.clone();

    // Hold the `last_blocks` write lock across the whole diff → commit →
    // broadcast critical section. This serializes concurrent renders so one
    // can't diff against a stale base, commit state out of order, or send a
    // patch the client applies against a DOM a newer render already moved.
    // Re-check the render sequence under the lock and drop this render if a
    // newer attempt has already taken over.
    let mut last_blocks = state.last_blocks.write().await;
    if !is_latest_render_attempt(state, seq) {
        return (0, "stale");
    }
    let ops = diff_blocks(&last_blocks, &out.blocks);
    let patch_cost = ops.iter().map(PatchOp::cost).sum::<usize>();
    let block_count = out.blocks.len();
    let fallback_full = patch_cost * 2 > block_count.max(1);

    // Sample memory after the render so the client sees the cost at the
    // point the page actually displays the update.
    let rss = resident_mib();
    let viewer_config_json = serde_json::json!({
        "font_size": viewer_config.font_size,
        "ui_font_size": viewer_config.ui_font_size,
        "default_page_mode": viewer_config.default_page_mode.as_str(),
        "default_theme": viewer_config.default_theme.as_str(),
        "source_jump_trigger": viewer_config.source_jump_trigger.as_str(),
        "mathjax_config": viewer_config.mathjax_config,
        // MathJax extensions are fixed when the page's <head> initializes.
        // The client reloads only when this deterministic list changes.
        "mathjax_packages": &out.preamble.packages_long,
        "typeset_mode": viewer_config.typeset_mode.as_str(),
        "render_tikz": viewer_config.render_tikz,
        "keybindings": viewer_config.keybindings,
        // The EFFECTIVE page margin baked into config_css (config > geometry >
        // default). The margin lives in the <head>, so a live change can only
        // take effect via reload — the client watches this value for that.
        "page_margin_mm": mathpreview_core::effective_page_margin_mm(
            &viewer_config, out.preamble.geometry_margin_mm),
    });

    let (payload, op_count, kind) = if fallback_full {
        let payload = serde_json::json!({
            "event": "body-updated",
            "html": out.body_html,
            "rss_mib": rss,
            "viewer_config": viewer_config_json,
        })
        .to_string();
        (payload, block_count, "blocks (full)")
    } else {
        let ops_json: Vec<_> = ops.iter().map(PatchOp::to_json).collect();
        // The client applies this list POSITIONALLY (it renumbers block ids
        // after inserts/deletes), so it must cover every block — but shipping
        // full metadata for all of them made every keystroke on a large
        // document a ~400 KiB payload plus tens of thousands of client-side
        // setAttribute calls (a 60-page doc has ~19k word anchors). Send a
        // compact `0` for positions whose metadata is unchanged from the last
        // broadcast; the client skips those entirely. Typing within a line
        // then ships ONE real entry; only a line-count change (which shifts
        // every later anchor) pays the full cost.
        let blocks_json: Vec<_> = out
            .blocks
            .iter()
            .enumerate()
            .map(|(i, block)| {
                let unchanged = last_blocks.get(i).is_some_and(|old| {
                    old.id == block.id
                        && old.hash == block.hash
                        && old.src == block.src
                        && old.source_anchors == block.source_anchors
                });
                if unchanged {
                    serde_json::json!(0)
                } else {
                    serde_json::json!({
                        "id": block.id,
                        "hash": block.hash,
                        "src": block.src,
                        "anchors": block.source_anchors,
                    })
                }
            })
            .collect();
        let payload = serde_json::json!({
            "event": "patch",
            "ops": ops_json,
            "blocks": blocks_json,
            "rss_mib": rss,
            "viewer_config": viewer_config_json,
        })
        .to_string();
        (payload, patch_cost, "ops")
    };

    *last_blocks = out.blocks.clone();
    *state.current.write().await = out;
    let _ = state.tx.send(payload);
    (op_count, kind)
}

/// One element of a block-level patch.
///
/// * `ReplaceRange` is the compact common case: replace `remove` blocks
///   starting at `index` with the already-rendered `html`. The diff emits
///   these in reverse-position order so the client can apply them
///   sequentially without rebasing indices.
/// * `Rebuild` covers the structural case the diff cannot encode as a
///   sequence of disjoint ranges — typically when blocks have moved. It
///   replaces a contiguous slice of `old_count` blocks (starting at
///   `start`) with the layout described by `plan`, where each slot either
///   reuses an existing block by absolute old-index (`Reuse`) or inserts a
///   newly-rendered block (`Insert`). Reuse preserves the existing DOM
///   subtree, so moved blocks keep their typeset MathJax SVG intact.
#[derive(Debug)]
enum PatchOp {
    ReplaceRange {
        index: usize,
        remove: usize,
        insert: usize,
        html: String,
    },
    Rebuild {
        start: usize,
        old_count: usize,
        plan: Vec<PlanSlot>,
    },
    /// In-block sub-diff for a theorem/proof block whose scaffolding is
    /// unchanged: replace `remove` body-child elements starting at
    /// `child_index` (within the block at top-level position `index`) with
    /// `html`. Keeps the block element — and the typeset MathJax in every
    /// unchanged proof paragraph — in place, so a one-paragraph edit inside a
    /// long proof no longer re-sends or re-parses the whole proof.
    BlockSub {
        index: usize,
        child_index: usize,
        remove: usize,
        insert: usize,
        html: String,
    },
}

#[derive(Debug)]
enum PlanSlot {
    /// Reuse the block at this absolute index in the old (pre-patch) layout.
    Reuse(usize),
    /// Insert a freshly-rendered block.
    Insert(String),
}

impl PatchOp {
    fn to_json(&self) -> serde_json::Value {
        match self {
            PatchOp::ReplaceRange {
                index,
                remove,
                insert,
                html,
            } => serde_json::json!({
                "type": "range", "index": index, "remove": remove, "insert": insert, "html": html,
            }),
            PatchOp::BlockSub {
                index,
                child_index,
                remove,
                insert,
                html,
            } => serde_json::json!({
                "type": "blocksub",
                "index": index,
                "child_index": child_index,
                "remove": remove,
                "insert": insert,
                "html": html,
            }),
            PatchOp::Rebuild {
                start,
                old_count,
                plan,
            } => {
                let plan_json: Vec<_> = plan
                    .iter()
                    .map(|slot| match slot {
                        PlanSlot::Reuse(src) => serde_json::json!({ "src": src }),
                        PlanSlot::Insert(html) => serde_json::json!({ "html": html }),
                    })
                    .collect();
                serde_json::json!({
                    "type": "rebuild",
                    "start": start,
                    "old_count": old_count,
                    "plan": plan_json,
                })
            }
        }
    }

    fn cost(&self) -> usize {
        match self {
            PatchOp::ReplaceRange { remove, insert, .. } => *remove + *insert,
            PatchOp::BlockSub { remove, insert, .. } => *remove + *insert,
            // Rebuild cost = number of fresh inserts (reused blocks are essentially free).
            PatchOp::Rebuild { plan, .. } => plan
                .iter()
                .filter(|slot| matches!(slot, PlanSlot::Insert(_)))
                .count(),
        }
    }
}

/// Keyed-LCS block diff.
///
/// Computes the longest common subsequence of `diff_hash` values between the
/// previous and new block sequences and emits one `ReplaceRange` op per
/// non-LCS gap. Compared to the prior single-range diff this surfaces
/// multiple surgical edits when the user makes disjoint changes (e.g. fix a
/// typo in §2 while also editing the bibliography), and it keeps unrelated
/// duplicate-hash blocks anchored in place when only one of them is edited.
///
/// Ops are emitted in reverse-position order so the client can apply them
/// sequentially against the live DOM without rebasing indices — each op's
/// `index` is valid in the document state at the moment it is applied.
///
/// Common prefix and suffix are trimmed up front as a fast path. The shared
/// LCS work is bounded by `O(n * m)` on the trimmed middle; for the
/// 300-block test paper that is sub-millisecond in practice.
fn diff_blocks(old: &[RenderedBlock], new: &[RenderedBlock]) -> Vec<PatchOp> {
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix].diff_hash == new[prefix].diff_hash
    {
        prefix += 1;
    }

    let mut old_suffix = old.len();
    let mut new_suffix = new.len();
    while old_suffix > prefix
        && new_suffix > prefix
        && old[old_suffix - 1].diff_hash == new[new_suffix - 1].diff_hash
    {
        old_suffix -= 1;
        new_suffix -= 1;
    }

    let old_mid = &old[prefix..old_suffix];
    let new_mid = &new[prefix..new_suffix];

    if old_mid.is_empty() && new_mid.is_empty() {
        return Vec::new();
    }

    // For very small middles, skip LCS — a single range op is optimal anyway.
    if old_mid.len() <= 1 || new_mid.len() <= 1 {
        return single_range(old_mid, new_mid, prefix);
    }

    let alignment = lcs_align(old_mid, new_mid);

    // Move detection: pair up unmatched-on-old with unmatched-on-new blocks
    // that share a diff_hash (FIFO). Any such pair means the block content
    // moved positions and can be reused via a Rebuild plan slot.
    let lcs_old: std::collections::HashSet<usize> = alignment.iter().map(|&(o, _)| o).collect();
    let lcs_new: std::collections::HashSet<usize> = alignment.iter().map(|&(_, n)| n).collect();
    let mut move_to_old: HashMap<usize, usize> = HashMap::new();
    {
        let unmatched_old: Vec<usize> = (0..old_mid.len())
            .filter(|i| !lcs_old.contains(i))
            .collect();
        let mut used_old: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for ni in (0..new_mid.len()).filter(|i| !lcs_new.contains(i)) {
            let hash = &new_mid[ni].diff_hash;
            for &oi in &unmatched_old {
                if used_old.contains(&oi) {
                    continue;
                }
                if &old_mid[oi].diff_hash == hash {
                    move_to_old.insert(ni, oi);
                    used_old.insert(oi);
                    break;
                }
            }
        }
    }

    if !move_to_old.is_empty() {
        // Structural rearrangement detected. Emit a single Rebuild op
        // covering the trimmed middle. Anchored (LCS) blocks and moved
        // blocks become Reuse slots that preserve their DOM (and typeset
        // MathJax). Truly-new blocks become Insert slots with rendered HTML.
        let mut lcs_to_old: HashMap<usize, usize> = HashMap::new();
        for &(oi, ni) in alignment.iter() {
            lcs_to_old.insert(ni, oi);
        }
        let mut plan = Vec::with_capacity(new_mid.len());
        for (ni, block) in new_mid.iter().enumerate() {
            if let Some(&oi) = lcs_to_old.get(&ni) {
                plan.push(PlanSlot::Reuse(prefix + oi));
            } else if let Some(&oi) = move_to_old.get(&ni) {
                plan.push(PlanSlot::Reuse(prefix + oi));
            } else {
                plan.push(PlanSlot::Insert(block.html.clone()));
            }
        }
        return vec![PatchOp::Rebuild {
            start: prefix,
            old_count: old_mid.len(),
            plan,
        }];
    }

    // No moves: emit one ReplaceRange per non-LCS gap.
    let mut ops = Vec::new();
    let mut o = 0usize;
    let mut n = 0usize;
    for &(ai, bi) in alignment.iter() {
        if ai > o || bi > n {
            emit_range_op(&mut ops, old_mid, new_mid, o, ai, n, bi, prefix);
        }
        o = ai + 1;
        n = bi + 1;
    }
    if o < old_mid.len() || n < new_mid.len() {
        emit_range_op(
            &mut ops,
            old_mid,
            new_mid,
            o,
            old_mid.len(),
            n,
            new_mid.len(),
            prefix,
        );
    }

    // Reverse so the client can process ops front-to-back without index
    // rebasing: each op's index targets a slice of the document that hasn't
    // been touched by any later op in the sequence.
    ops.reverse();
    ops
}

fn single_range(
    old_mid: &[RenderedBlock],
    new_mid: &[RenderedBlock],
    prefix: usize,
) -> Vec<PatchOp> {
    if old_mid.len() == 1 && new_mid.len() == 1 {
        if let Some(op) = try_sub_diff(&old_mid[0], &new_mid[0], prefix) {
            return vec![op];
        }
    }
    let html = new_mid
        .iter()
        .map(|block| block.html.as_str())
        .collect::<String>();
    vec![PatchOp::ReplaceRange {
        index: prefix,
        remove: old_mid.len(),
        insert: new_mid.len(),
        html,
    }]
}

/// Try to express a single changed theorem/proof block as an in-block
/// sub-diff. Returns `None` (caller falls back to a full block replace) when
/// either side lacks captured sub-structure, the scaffolding (head / number)
/// changed, or the entire body changed (no prefix/suffix to reuse, so a
/// `BlockSub` would save nothing).
fn try_sub_diff(old: &RenderedBlock, new: &RenderedBlock, index: usize) -> Option<PatchOp> {
    let (o, n) = (old.sub_blocks.as_ref()?, new.sub_blocks.as_ref()?);
    if o.prefix_diff != n.prefix_diff || o.suffix_diff != n.suffix_diff {
        return None;
    }

    let mut p = 0;
    while p < o.children.len()
        && p < n.children.len()
        && o.children[p].diff_hash == n.children[p].diff_hash
    {
        p += 1;
    }
    let mut o_end = o.children.len();
    let mut n_end = n.children.len();
    while o_end > p && n_end > p && o.children[o_end - 1].diff_hash == n.children[n_end - 1].diff_hash
    {
        o_end -= 1;
        n_end -= 1;
    }

    let remove = o_end - p;
    let insert = n_end - p;
    // Nothing reused on either side → a full replace is simpler and no larger.
    if remove == o.children.len() && insert == n.children.len() {
        return None;
    }
    let html: String = n.children[p..n_end]
        .iter()
        .map(|c| c.html.as_str())
        .collect();
    Some(PatchOp::BlockSub {
        index,
        child_index: p,
        remove,
        insert,
        html,
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_range_op(
    ops: &mut Vec<PatchOp>,
    old_mid: &[RenderedBlock],
    new_mid: &[RenderedBlock],
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
    prefix: usize,
) {
    if old_end - old_start == 1 && new_end - new_start == 1 {
        if let Some(op) =
            try_sub_diff(&old_mid[old_start], &new_mid[new_start], prefix + old_start)
        {
            ops.push(op);
            return;
        }
    }
    let html = new_mid[new_start..new_end]
        .iter()
        .map(|block| block.html.as_str())
        .collect::<String>();
    ops.push(PatchOp::ReplaceRange {
        index: prefix + old_start,
        remove: old_end - old_start,
        insert: new_end - new_start,
        html,
    });
}

/// Standard O(n·m) LCS keyed on `diff_hash`. Returns aligned `(old_idx,
/// new_idx)` pairs in ascending order.
///
/// Backtrack tie-breaks consistently (prefer moving in `old` when LCS
/// lengths are equal) so that runs of duplicate-hash blocks align in the
/// natural order rather than scrambling — the regression that sank the
/// previous attempt described in DESIGN.md §13.
fn lcs_align(old: &[RenderedBlock], new: &[RenderedBlock]) -> Vec<(usize, usize)> {
    let n = old.len();
    let m = new.len();
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let mut dp = vec![0u32; (n + 1) * (m + 1)];
    let stride = m + 1;
    let idx = |i: usize, j: usize| i * stride + j;
    for i in 0..n {
        for j in 0..m {
            dp[idx(i + 1, j + 1)] = if old[i].diff_hash == new[j].diff_hash {
                dp[idx(i, j)] + 1
            } else {
                dp[idx(i + 1, j)].max(dp[idx(i, j + 1)])
            };
        }
    }
    let mut out = Vec::with_capacity(dp[idx(n, m)] as usize);
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        if old[i - 1].diff_hash == new[j - 1].diff_hash {
            out.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[idx(i - 1, j)] >= dp[idx(i, j - 1)] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    out.reverse();
    out
}

/// Heuristic: is the source in a renderable state, or is the user
/// mid-expression? Conservative — when in doubt, return true and let the
/// parser/MathJax handle it. Skipping when not renderable preserves the
/// previous rendered output rather than flashing a broken one.
fn is_buffer_renderable(source: &str, cursor: Option<(usize, usize)>) -> bool {
    // MathJax keeps TeX definitions between tex2svgPromise calls. If the
    // exact transient control word `\def` reaches it while the user is still
    // typing a longer macro such as `\defeq`, TeX may consume and redefine
    // the equation's `\end` (or the next existing macro), poisoning every
    // later typeset until reload. The plugin sends the caret with the buffer,
    // which lets us defer only an unfinished definition at the edit position.
    // Stable, intentional `\def` definitions and literal text remain valid.
    if cursor.is_some_and(|(line, col)| cursor_has_unfinished_def_in_math(source, line, col)) {
        return false;
    }

    let bytes = source.as_bytes();
    // Track distinct math-delimiter parities. `$$` and `$` are NOT the same
    // delimiter — typing `$$` should leave us in unbalanced display state,
    // not balanced because two `$`s.
    let mut in_inline = false;
    let mut in_display = false;
    let mut in_comment = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            in_comment = false;
            i += 1;
            continue;
        }
        if in_comment {
            i += 1;
            continue;
        }
        if b == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next == b'(' || next == b')' {
                in_inline = !in_inline;
                i += 2;
                continue;
            }
            if next == b'[' || next == b']' {
                in_display = !in_display;
                i += 2;
                continue;
            }
            i += 2;
            continue;
        }
        match b {
            b'%' => in_comment = true,
            b'$' => {
                if bytes.get(i + 1) == Some(&b'$') {
                    in_display = !in_display;
                    i += 2;
                    continue;
                }
                in_inline = !in_inline;
            }
            _ => {}
        }
        i += 1;
    }
    !in_inline && !in_display
}

/// Optional `line:column` caret attached by the nvim plugin to `/buffer`.
/// Both coordinates are 1-based; the column is a byte column, matching
/// `nvim_win_get_cursor`.
fn buffer_cursor_from_headers(headers: &axum::http::HeaderMap) -> Option<(usize, usize)> {
    let raw = headers.get("x-mathpreview-cursor")?.to_str().ok()?;
    let (line, col) = raw.split_once(':')?;
    let line = line.parse::<usize>().ok()?;
    let col = col.parse::<usize>().ok()?;
    (line > 0 && col > 0).then_some((line, col))
}

/// True when the edit-time caret is inside rendered math and the prefix up to
/// that caret contains an exact `\def` whose replacement body is not complete
/// yet. Looking only at the prefix is deliberate: in `\def\label{x}`, the
/// source after a newly typed bare `\def` may make the whole buffer look like a
/// valid definition even though those tokens were already present and would
/// be redefined accidentally.
fn cursor_has_unfinished_def_in_math(source: &str, line: usize, col: usize) -> bool {
    let Some(limit) = source_offset_at_cursor(source, line, col) else {
        return false;
    };
    let bytes = source.as_bytes();
    let mut i = 0usize;
    let mut in_comment = false;
    let mut verb_delimiter = None;
    let mut in_inline = false;
    let mut in_display = false;
    let mut math_env_depth = 0usize;

    while i < limit {
        let byte = bytes[i];
        if byte == b'\n' {
            in_comment = false;
            verb_delimiter = None;
            i += 1;
            continue;
        }
        if in_comment {
            i += 1;
            continue;
        }
        if let Some(delimiter) = verb_delimiter {
            if byte == delimiter {
                verb_delimiter = None;
            }
            i += 1;
            continue;
        }
        if byte == b'%' {
            in_comment = true;
            i += 1;
            continue;
        }
        if byte == b'$' {
            if bytes.get(i + 1) == Some(&b'$') {
                in_display = !in_display;
                i += 2;
            } else {
                in_inline = !in_inline;
                i += 1;
            }
            continue;
        }
        if byte != b'\\' || i + 1 >= limit {
            i += 1;
            continue;
        }

        let next = bytes[i + 1];
        match next {
            b'(' => in_inline = true,
            b')' => in_inline = false,
            b'[' => in_display = true,
            b']' => in_display = false,
            _ => {}
        }
        if !next.is_ascii_alphabetic() {
            i += 2;
            continue;
        }

        let mut command_end = i + 2;
        while command_end < bytes.len() && bytes[command_end].is_ascii_alphabetic() {
            command_end += 1;
        }
        let command = &bytes[i + 1..command_end];

        if command == b"verb" {
            let delimiter_at = command_end + usize::from(bytes.get(command_end) == Some(&b'*'));
            if delimiter_at < limit {
                verb_delimiter = Some(bytes[delimiter_at]);
                i = delimiter_at + 1;
            } else {
                i = limit;
            }
            continue;
        }

        if matches!(command, b"begin" | b"end") {
            if let Some((env, after_env)) = braced_environment_name(bytes, command_end) {
                if command == b"begin" && is_literal_environment(env) && after_env <= limit {
                    let close = [b"\\end{".as_slice(), env, b"}".as_slice()].concat();
                    if let Some(offset) = find_bytes(&bytes[after_env..limit], &close) {
                        i = after_env + offset + close.len();
                        continue;
                    }
                    return false;
                }
                if after_env <= limit && is_math_environment(env) {
                    if command == b"begin" {
                        math_env_depth += 1;
                    } else {
                        math_env_depth = math_env_depth.saturating_sub(1);
                    }
                }
                i = after_env.min(limit);
                continue;
            }
        }

        let in_math = math_env_depth > 0 || in_inline || in_display;
        if command == b"def" && command_end <= limit && in_math {
            match definition_prefix_state(bytes, command_end, limit) {
                DefinitionPrefix::Incomplete => return true,
                DefinitionPrefix::OpenReplacement { at, depth } => {
                    if replacement_closes_before_math_end(bytes, at, depth) {
                        return false;
                    }
                    return true;
                }
                DefinitionPrefix::Complete(after) => {
                    i = after;
                    continue;
                }
                DefinitionPrefix::Invalid => {}
            }
        }
        i = command_end.min(limit);
    }
    false
}

fn source_offset_at_cursor(source: &str, line: usize, col: usize) -> Option<usize> {
    if line == 0 || col == 0 {
        return None;
    }
    let bytes = source.as_bytes();
    let mut line_start = 0usize;
    for _ in 1..line {
        let newline = bytes[line_start..].iter().position(|byte| *byte == b'\n')?;
        line_start += newline + 1;
    }
    let line_len = bytes[line_start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(bytes.len() - line_start);
    Some(line_start + col.min(line_len))
}

#[derive(Debug, Eq, PartialEq)]
enum DefinitionPrefix {
    Incomplete,
    OpenReplacement { at: usize, depth: usize },
    Complete(usize),
    Invalid,
}

fn definition_prefix_state(bytes: &[u8], mut i: usize, limit: usize) -> DefinitionPrefix {
    skip_prefix_space_and_comments(bytes, &mut i, limit);
    if i >= limit {
        return DefinitionPrefix::Incomplete;
    }
    if bytes[i] != b'\\' {
        return DefinitionPrefix::Invalid;
    }
    i += 1;
    if i >= limit {
        return DefinitionPrefix::Incomplete;
    }
    if bytes[i].is_ascii_alphabetic() {
        while i < limit && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
    } else {
        i += 1;
    }

    // TeX's parameter text runs until the opening brace of the replacement.
    loop {
        skip_prefix_space_and_comments(bytes, &mut i, limit);
        if i >= limit {
            return DefinitionPrefix::Incomplete;
        }
        match bytes[i] {
            b'{' => {
                i += 1;
                break;
            }
            b'\\' => {
                i += 1;
                if i < limit && bytes[i].is_ascii_alphabetic() {
                    while i < limit && bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                } else if i < limit {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    let mut depth = 1usize;
    while i < limit {
        match bytes[i] {
            b'%' => {
                while i < limit && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'\\' => {
                i += 1;
                if i < limit && bytes[i].is_ascii_alphabetic() {
                    while i < limit && bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                } else if i < limit {
                    i += 1;
                }
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return DefinitionPrefix::Complete(i);
                }
            }
            _ => i += 1,
        }
    }
    DefinitionPrefix::OpenReplacement { at: i, depth }
}

fn skip_prefix_space_and_comments(bytes: &[u8], i: &mut usize, limit: usize) {
    loop {
        while *i < limit && bytes[*i].is_ascii_whitespace() {
            *i += 1;
        }
        if *i >= limit || bytes[*i] != b'%' {
            return;
        }
        while *i < limit && bytes[*i] != b'\n' {
            *i += 1;
        }
    }
}

/// When the caret is editing inside a replacement body, accept an already
/// balanced definition whose closing brace is still to the right. Stop at the
/// surrounding math terminator so braces in later prose cannot make an
/// actually incomplete definition appear safe.
fn replacement_closes_before_math_end(bytes: &[u8], mut i: usize, mut depth: usize) -> bool {
    let mut in_comment = false;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'\n' {
            in_comment = false;
            i += 1;
            continue;
        }
        if in_comment {
            i += 1;
            continue;
        }
        match byte {
            b'%' => {
                in_comment = true;
                i += 1;
            }
            b'$' => return false,
            b'\\' if i + 1 < bytes.len() => {
                if matches!(bytes[i + 1], b')' | b']') {
                    return false;
                }
                if bytes[i + 1].is_ascii_alphabetic() {
                    let mut command_end = i + 2;
                    while command_end < bytes.len() && bytes[command_end].is_ascii_alphabetic() {
                        command_end += 1;
                    }
                    if &bytes[i + 1..command_end] == b"end"
                        && braced_environment_name(bytes, command_end)
                            .is_some_and(|(env, _)| is_math_environment(env))
                    {
                        return false;
                    }
                    i = command_end;
                } else {
                    i += 2;
                }
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    false
}

fn braced_environment_name(bytes: &[u8], mut i: usize) -> Option<(&[u8], usize)> {
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    let start = i + 1;
    let close = bytes[start..].iter().position(|byte| *byte == b'}')?;
    Some((&bytes[start..start + close], start + close + 1))
}

fn is_math_environment(env: &[u8]) -> bool {
    matches!(
        env,
        b"equation"
            | b"equation*"
            | b"align"
            | b"align*"
            | b"gather"
            | b"gather*"
            | b"multline"
            | b"multline*"
            | b"displaymath"
            | b"eqnarray"
            | b"eqnarray*"
            | b"alignat"
            | b"alignat*"
            | b"split"
    )
}

fn is_literal_environment(env: &[u8]) -> bool {
    matches!(
        env,
        b"verbatim" | b"verbatim*" | b"Verbatim" | b"lstlisting" | b"minted"
    )
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    (!needle.is_empty())
        .then(|| {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        })
        .flatten()
}

fn spawn_watcher(state: AppState, watch_rx: std_mpsc::Receiver<HashSet<PathBuf>>) {
    // notify-debouncer is sync; bridge into tokio via a std::sync::mpsc-style
    // channel polled from a dedicated thread that posts work back to a Tokio
    // task via an unbounded channel.
    let (file_tx, mut file_rx) = tokio::sync::mpsc::unbounded_channel::<HashSet<PathBuf>>();
    let watched_for_thread = state.watched.clone();
    let watcher_state = state.clone();

    std::thread::spawn(move || {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel();
        let mut debouncer = match new_debouncer(Duration::from_millis(120), None, raw_tx) {
            Ok(d) => d,
            Err(e) => {
                log_event(
                    &watcher_state,
                    "error",
                    format!("cannot start file watcher: {e}"),
                );
                return;
            }
        };
        // Watch every initial file's parent directory non-recursively so we
        // catch edits even when editors do save-via-rename.
        let initial: Vec<PathBuf> = {
            let g = watched_for_thread.blocking_read();
            g.iter().cloned().collect()
        };
        let mut watched_dirs: HashSet<PathBuf> = HashSet::new();
        let initial: HashSet<PathBuf> = initial.into_iter().collect();
        let mut watched_files: HashSet<PathBuf> =
            initial.iter().map(|f| normalize_watch_path(f)).collect();
        for f in initial {
            let dir = f.parent().unwrap_or(Path::new(".")).to_path_buf();
            if watched_dirs.insert(dir.clone()) {
                match debouncer.watcher().watch(&dir, RecursiveMode::NonRecursive) {
                    Ok(()) => log_event_verbose(
                        &watcher_state,
                        "info",
                        format!("watching {}", dir.display()),
                    ),
                    Err(e) => log_event(
                        &watcher_state,
                        "warn",
                        format!("failed to watch {}: {e}", dir.display()),
                    ),
                }
            }
        }
        // Top-level "we're watching N dirs" log so the panel sees one
        // line summarising the watcher even with verbose mode off.
        log_event(
            &watcher_state,
            "info",
            format!("watcher initialised — {} dir(s) on watch", watched_dirs.len()),
        );

        loop {
            while let Ok(files) = watch_rx.try_recv() {
                watched_files = files.iter().map(|f| normalize_watch_path(f)).collect();
                for f in files {
                    let dir = f.parent().unwrap_or(Path::new(".")).to_path_buf();
                    if watched_dirs.insert(dir.clone()) {
                        match debouncer.watcher().watch(&dir, RecursiveMode::NonRecursive) {
                            Ok(()) => log_event_verbose(
                                &watcher_state,
                                "info",
                                format!("watching {}", dir.display()),
                            ),
                            Err(e) => log_event(
                                &watcher_state,
                                "warn",
                                format!("failed to watch {}: {e}", dir.display()),
                            ),
                        }
                    }
                }
            }
            match raw_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(events) => match events {
                    Ok(evs) => {
                        let changed = watched_event_paths(&evs, &watched_files);
                        if changed.is_empty() {
                            continue;
                        }
                        let paths_summary = changed
                            .iter()
                            .take(3)
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        let more = if changed.len() > 3 {
                            format!(" +{} more", changed.len() - 3)
                        } else {
                            String::new()
                        };
                        log_event_verbose(
                            &watcher_state,
                            "info",
                            format!(
                                "watcher: change detected ({} events) — {paths_summary}{more}",
                                evs.len(),
                            ),
                        );
                        if file_tx.send(changed).is_err() {
                            break;
                        }
                    }
                    Err(errs) => {
                        for e in errs {
                            log_event(&watcher_state, "warn", format!("watcher: {e}"));
                        }
                    }
                },
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            };
        }
    });

    tokio::spawn(async move {
        while let Some(mut changed_paths) = file_rx.recv().await {
            // Drain any queued ticks — we only need the latest state.
            while let Ok(more) = file_rx.try_recv() {
                changed_paths.extend(more);
            }
            let seq = begin_render_attempt(&state);
            {
                let mut overrides = state.buffer_overrides.write().await;
                for path in &changed_paths {
                    // Only drop the in-memory buffer override if the file on disk
                    // now matches it — i.e. the editor saved and there is no
                    // newer unsaved push. Otherwise a save racing a keystroke
                    // would revert the preview to the on-disk version and lose
                    // the just-typed edit.
                    let drop_override = match std::fs::read_to_string(path) {
                        Ok(disk) => overrides.get(path).is_none_or(|buf| *buf == disk),
                        Err(_) => true,
                    };
                    if drop_override {
                        overrides.remove(path);
                    }
                }
            }
            *state.preamble_cache.write().await = None;
            let root = {
                let current = state.current.read().await;
                current.root_file.clone()
            };
            match render_cached(&state, &root).await.map(|(out, _)| out) {
                Ok(new_output) => {
                    if !is_latest_render_attempt(&state, seq) {
                        log_event_verbose(
                            &state,
                            "warn",
                            format!("file-change #{seq} stale render discarded"),
                        );
                        continue;
                    }
                    update_watched(&state, &new_output).await;
                    let (op_count, kind) = broadcast_render(&state, new_output, seq).await;
                    log_event_verbose(
                        &state,
                        "info",
                        format!("file-change → {op_count} {kind}"),
                    );
                }
                Err(e) => {
                    if !is_latest_render_attempt(&state, seq) {
                        log_event_verbose(
                            &state,
                            "warn",
                            format!("file-change #{seq} stale render error discarded: {e}"),
                        );
                        continue;
                    }
                    log_event(&state, "error", format!("render error: {e:#}"));
                    let payload = serde_json::json!({
                        "event": "error",
                        "message": format!("{e}"),
                    })
                    .to_string();
                    let _ = state.tx.send(payload);
                }
            }
        }
    });
}

fn watched_event_paths(
    events: &[notify_debouncer_full::DebouncedEvent],
    watched_files: &HashSet<PathBuf>,
) -> HashSet<PathBuf> {
    let mut changed = HashSet::new();
    for event in events {
        for path in &event.paths {
            let normalized = normalize_watch_path(path);
            if watched_files.contains(&normalized) {
                changed.insert(normalized);
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::{
        begin_render_attempt, diff_blocks, host_is_loopback, is_buffer_renderable,
        is_latest_render_attempt, merge_config_editor_values, origin_is_loopback,
        preamble_fingerprint, search_sync_payload, select_tikz_engine, serve_buffer_push,
        tikz_document, tikz_error_svg, tikz_hash_from_path, watched_event_paths,
        websocket_needs_reload, AppState, EditorSearch, PatchOp, PlanSlot, SearchRequest,
        WS_PROTOCOL_VERSION,
    };

    #[test]
    fn tikz_asset_paths_and_documents_are_bounded_and_isolated() {
        assert_eq!(
            tikz_hash_from_path("0123456789abcdef.svg"),
            Some("0123456789abcdef")
        );
        assert_eq!(tikz_hash_from_path("../diagram.svg"), None);
        assert_eq!(tikz_hash_from_path("not-hex-hash.svg"), None);

        let asset = mathpreview_core::renderer::TikzAsset {
            environment: "tikzpicture".to_string(),
            body: "[scale=2]\n\\draw (0,0) -- (1,1);".to_string(),
            preamble: "\\documentclass{article}\n\\usepackage{tikz}".to_string(),
        };
        let document = tikz_document(&asset);
        assert_eq!(document.matches(r"\documentclass").count(), 1);
        assert!(document.contains(r"\usepackage[active,tightpage]{preview}"));
        assert!(document.contains(r"\PreviewEnvironment{tikzpicture}"));
        assert!(document.contains(r"\begin{tikzpicture}[scale=2]"));
        assert!(document.contains(r"\end{tikzpicture}"));
        assert!(!document.contains("\n\n\\end{tikzpicture}"));

        let tikzcd = mathpreview_core::renderer::TikzAsset {
            environment: "tikzcd".to_string(),
            body: "\nA \\arrow[r] & B\n".to_string(),
            preamble: "\\documentclass{article}\n\\usepackage{tikz-cd}".to_string(),
        };
        let tikzcd_document = tikz_document(&tikzcd);
        assert!(tikzcd_document.contains("A \\arrow[r] & B\n\\end{tikzcd}"));
        assert!(!tikzcd_document.contains("\n\n\\end{tikzcd}"));
    }

    #[test]
    fn tikz_engine_selection_and_error_svg_are_safe() {
        assert_eq!(
            select_tikz_engine("% !TEX program = xelatex\n\\documentclass{article}"),
            "xelatex"
        );
        assert_eq!(
            select_tikz_engine("\\documentclass{article}\n\\usepackage{fontspec}"),
            "lualatex"
        );
        assert_eq!(select_tikz_engine("\\documentclass{article}"), "pdflatex");

        let svg = String::from_utf8(tikz_error_svg(
            "pdflatex failed:\n! Undefined control sequence <script>alert(1)</script>",
        ))
        .unwrap();
        assert!(svg.contains("TikZ preview unavailable"));
        assert!(svg.contains("&lt;script&gt;"));
        assert!(!svg.contains("<script>"));
    }

    #[test]
    fn preamble_fingerprint_includes_dependency_contents() {
        use mathpreview_core::project::{Preamble, PreambleFile, Project};

        let make_project = |source: &str| Project {
            root: PathBuf::from("main.tex"),
            preamble: Preamble {
                source: "\\input{preamble}".to_string(),
                file: PathBuf::from("main.tex"),
            },
            preamble_files: vec![PreambleFile {
                path: PathBuf::from("preamble.tex"),
                source: source.to_string(),
            }],
            files: vec![],
            warnings: vec![],
        };

        let before = preamble_fingerprint(&make_project("\\usepackage{amsmath}"));
        let after = preamble_fingerprint(&make_project(
            "\\usepackage{mathtools}\\mathtoolsset{showonlyrefs=true}",
        ));
        assert_ne!(before, after);
    }

    #[test]
    fn host_guard_accepts_loopback_only() {
        // Loopback names (with or without port) are allowed.
        assert!(host_is_loopback("127.0.0.1:23636"));
        assert!(host_is_loopback("127.0.0.1"));
        assert!(host_is_loopback("localhost:23636"));
        assert!(host_is_loopback("LocalHost"));
        assert!(host_is_loopback("127.5.6.7"));
        assert!(host_is_loopback("[::1]:23636"));
        assert!(host_is_loopback("[::1]"));
        // A rebinding/cross-origin Host is rejected.
        assert!(!host_is_loopback("evil.example.com:23636"));
        assert!(!host_is_loopback("192.168.1.5:23636"));
        assert!(!host_is_loopback("169.254.1.1"));
    }

    #[test]
    fn origin_guard_rejects_cross_origin() {
        // The served page's own origin (classic CSRF carries the attacker's).
        assert!(origin_is_loopback("http://127.0.0.1:23636"));
        assert!(origin_is_loopback("http://localhost:23636"));
        assert!(origin_is_loopback("https://[::1]:23636"));
        // Cross-origin / null origins are rejected.
        assert!(!origin_is_loopback("https://evil.example"));
        assert!(!origin_is_loopback("http://192.168.1.5:23636"));
        assert!(!origin_is_loopback("null"));
        assert!(!origin_is_loopback(""));
    }
    use mathpreview_core::{
        renderer::{HtmlOptions, RenderedBlock},
        sync::SyncIndex,
        ExtractedPreamble, RenderOutput,
    };
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::{
        atomic::{AtomicBool, AtomicU64},
        mpsc as std_mpsc, Arc,
    };
    use tokio::sync::{broadcast, Mutex, Notify, RwLock};

    #[test]
    fn websocket_protocol_accepts_current_shell_version() {
        let query =
            std::collections::HashMap::from([("v".to_string(), WS_PROTOCOL_VERSION.to_string())]);

        assert!(!websocket_needs_reload(&query));
        assert!(websocket_needs_reload(&std::collections::HashMap::from([
            ("v".to_string(), "old".to_string(),)
        ])));
        assert!(websocket_needs_reload(&std::collections::HashMap::new()));
    }

    #[test]
    fn client_ws_protocol_matches_server() {
        // The browser builds `/ws?v=<this>` from a hardcoded JS copy of the
        // protocol version; if it doesn't equal the server const, every connect
        // triggers a full-reload (infinite reload loop). Keep them in lockstep.
        let footer = include_str!("../../core/src/assets/client/footer.js");
        let needle = format!("WS_PROTOCOL_VERSION = '{WS_PROTOCOL_VERSION}'");
        assert!(
            footer.contains(&needle),
            "footer.js WS_PROTOCOL_VERSION must equal the server's {WS_PROTOCOL_VERSION:?} — \
             update crates/core/src/assets/client/footer.js"
        );
    }

    #[test]
    fn search_sync_payload_preserves_vim_boundary_and_case_semantics() {
        let search = EditorSearch {
            query: "f".to_string(),
            whole_start: true,
            whole_end: true,
            case_sensitive: true,
        };
        let payload: serde_json::Value =
            serde_json::from_str(&search_sync_payload(&search)).unwrap();
        assert_eq!(payload["event"], "search-sync");
        assert_eq!(payload["query"], "f");
        assert_eq!(payload["whole_start"], true);
        assert_eq!(payload["whole_end"], true);
        assert_eq!(payload["case_sensitive"], true);

        // Older plugin payloads remain valid and retain the former
        // case-insensitive substring behavior until the version handshake
        // upgrades that plugin/browser pair.
        let old: SearchRequest = serde_json::from_value(serde_json::json!({ "query": "f" }))
            .expect("legacy search payload parses");
        assert!(!old.whole_start);
        assert!(!old.whole_end);
        assert!(!old.case_sensitive);
    }

    #[test]
    fn whole_file_config_editor_rejects_semantic_typos() {
        let label = PathBuf::from("config-dialog.toml");
        let values = std::collections::BTreeMap::new();
        merge_config_editor_values(
            mathpreview_core::config::DEFAULT_CONFIG_TEMPLATE,
            &values,
            &label,
        )
        .expect("the dialog's built-in template must be writable");

        let error =
            merge_config_editor_values("[viewer]\nfont-sze = 18\n", &values, &label).unwrap_err();
        assert!(error.contains("unknown field"), "unexpected error: {error}");

        let error =
            merge_config_editor_values("[keybindings]\ntoogle-theme = \"T\"\n", &values, &label)
                .unwrap_err();
        assert!(
            error.contains("unknown keybinding action"),
            "unexpected error: {error}",
        );
    }

    #[test]
    fn structured_editor_save_preserves_existing_local_config_content() {
        let label = PathBuf::from(".mathpreview.toml");
        let local = r#"# Keep this project-local explanation.
[viewer]
font-size = 16
wrap-equations = false
mathjax-config = "window.MathJax.svg.displayOverflow = 'scroll';"

[text-macros]
SV = "<strong>#1</strong>"

[keybindings]
toggle-theme = "T"
"#;
        let values = std::collections::BTreeMap::from([
            ("viewer.font-size".to_string(), serde_json::json!(21)),
            (
                "viewer.default-page-mode".to_string(),
                serde_json::json!("dynamic"),
            ),
        ]);

        let merged = merge_config_editor_values(local, &values, &label).unwrap();
        assert!(merged.contains("# Keep this project-local explanation."));
        assert!(merged.contains("wrap-equations = false"));
        assert!(merged.contains("mathjax-config = \"window.MathJax"));
        assert!(merged.contains("SV = \"<strong>#1</strong>\""));
        assert!(merged.contains("toggle-theme = \"T\""));

        let resolved = mathpreview_core::Config::parse(&merged, &label)
            .unwrap()
            .resolve();
        assert_eq!(resolved.viewer.font_size, 21);
        assert_eq!(resolved.viewer.default_page_mode.as_str(), "dynamic");
        assert_eq!(resolved.viewer.keybindings["toggle-theme"], ["T"]);
    }

    #[test]
    fn buffer_guard_defers_unclosed_math_and_unfinished_def_at_cursor() {
        assert!(is_buffer_renderable(r"\begin{document}\section{A", None));
        assert!(is_buffer_renderable(
            r"\begin{document}\begin{proof} text",
            None
        ));
        assert!(!is_buffer_renderable(r"\begin{document} $x", None));
        assert!(!is_buffer_renderable(r"\begin{document} \[x", None));

        let bare_def =
            "\\begin{document}\n\\begin{equation}\na \\def\n\\end{equation}\n\\end{document}";
        assert!(!is_buffer_renderable(bare_def, Some((3, 6))));
        assert!(!is_buffer_renderable(bare_def, Some((3, 7))));

        let before_macro = r"\begin{document}
\begin{equation}
a \def\label{x}
\end{equation}
\end{document}";
        assert!(!is_buffer_renderable(before_macro, Some((3, 6))));

        let longer_macro = r"\begin{document}
\begin{equation}
a \defeq b
\end{equation}
\end{document}";
        assert!(is_buffer_renderable(longer_macro, Some((3, 8))));

        let partial_definition = r"\begin{document}
\begin{equation}
\def\foo#1
\end{equation}
\end{document}";
        assert!(!is_buffer_renderable(
            partial_definition,
            Some((3, r"\def\foo#1".len()))
        ));

        let open_replacement = r"\begin{document}
\begin{equation}
\def\foo#1{#1+1
\end{equation}
\end{document}";
        assert!(!is_buffer_renderable(
            open_replacement,
            Some((3, r"\def\foo#1{#1+1".len()))
        ));

        let complete_definition = r"\begin{document}
\begin{equation}
\def\foo#1{#1+1}\foo{2}
\end{equation}
\end{document}";
        assert!(is_buffer_renderable(
            complete_definition,
            Some((3, r"\def\foo#1{#1+1}".len()))
        ));

        let edit_inside_complete_definition =
            r"\begin{document}$\def\foo#1{#1+1}\foo{2}$\end{document}";
        let edit_col = edit_inside_complete_definition.find('+').unwrap() + 1;
        assert!(is_buffer_renderable(
            edit_inside_complete_definition,
            Some((1, edit_col))
        ));

        // Literal/comment examples remain renderable even with the caret on
        // the exact bytes `\def`.
        let inline_literal = r"\begin{document}\verb|\def\foo{|\end{document}";
        let inline_literal_col = inline_literal.find(r"\def").unwrap() + 4;
        assert!(is_buffer_renderable(
            inline_literal,
            Some((1, inline_literal_col))
        ));

        let block_literal =
            "\\begin{document}\n\\begin{verbatim}\n\\def\\foo{\n\\end{verbatim}\n\\end{document}";
        assert!(is_buffer_renderable(block_literal, Some((3, 4))));

        let commented =
            "\\begin{document}\n\\begin{equation}\n% \\def\nx=1\n\\end{equation}\n\\end{document}";
        assert!(is_buffer_renderable(commented, Some((3, 6))));

        assert!(is_buffer_renderable(
            r"\begin{document}\\def\end{document}",
            Some((1, 21))
        ));
    }

    #[test]
    fn shifted_block_insertion_stays_one_range_patch() {
        let old = vec![
            rendered_block("blk-10", "old first"),
            rendered_block("blk-14", "old second"),
        ];
        let new = vec![
            rendered_block("blk-10", "inserted"),
            rendered_block("blk-12", "old first"),
            rendered_block("blk-14", "old second"),
        ];

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            PatchOp::ReplaceRange {
                index: 0,
                remove: 0,
                insert: 1,
                ..
            }
        ));
    }

    #[test]
    fn single_block_edit_stays_one_range_patch() {
        let old = vec![
            rendered_block("blk-10", "old first"),
            rendered_block("blk-14", "old second"),
        ];
        let new = vec![
            rendered_block("blk-10", "new first"),
            rendered_block("blk-14", "old second"),
        ];

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            PatchOp::ReplaceRange {
                index: 0,
                remove: 1,
                insert: 1,
                ..
            }
        ));
    }

    #[test]
    fn shifted_block_deletion_stays_one_range_patch() {
        let old = vec![
            rendered_block("blk-10", "inserted"),
            rendered_block("blk-12", "old first"),
            rendered_block("blk-14", "old second"),
        ];
        let new = vec![
            rendered_block("blk-10", "old first"),
            rendered_block("blk-14", "old second"),
        ];

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            PatchOp::ReplaceRange {
                index: 0,
                remove: 1,
                insert: 0,
                ..
            }
        ));
    }

    #[test]
    fn shifted_source_metadata_does_not_force_large_patch() {
        let old = vec![
            rendered_block_with_diff("blk-519", "first line old metadata", "first line semantic"),
            rendered_block_with_diff("blk-524", "equation old metadata", "equation semantic"),
        ];
        let new = vec![
            rendered_block_with_diff("blk-519", "inserted", "inserted"),
            rendered_block_with_diff("blk-521", "first line new metadata", "first line semantic"),
            rendered_block_with_diff("blk-526", "equation new metadata", "equation semantic"),
        ];

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            PatchOp::ReplaceRange {
                index: 0,
                remove: 0,
                insert: 1,
                ..
            }
        ));
    }

    /// End-to-end check: render a real LaTeX document with three sections,
    /// then re-render with the sections in a different order.
    ///
    /// What we want: a single Rebuild op whose plan reuses every paragraph
    /// body (and therefore every typeset math node inside them) verbatim
    /// from the old layout. Section headers themselves rerender because
    /// they contain auto-numbered counters (`\section` 1, 2, 3) that
    /// genuinely change when sections move — but those headers carry no
    /// math, so the cost is in the noise. The expensive parts (proofs,
    /// equations, theorem statements) stay reused.
    #[test]
    fn rendered_section_swap_emits_rebuild_that_reuses_paragraph_bodies() {
        let original = "\
\\documentclass{article}
\\begin{document}
\\section{Alpha}
First paragraph here.
\\section{Beta}
Second paragraph here.
\\section{Gamma}
Third paragraph here.
\\end{document}
";
        let swapped = "\
\\documentclass{article}
\\begin{document}
\\section{Gamma}
Third paragraph here.
\\section{Alpha}
First paragraph here.
\\section{Beta}
Second paragraph here.
\\end{document}
";
        let old = mathpreview_core::render_project_from_source(
            &PathBuf::from("main.tex"),
            original.to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();
        let new = mathpreview_core::render_project_from_source(
            &PathBuf::from("main.tex"),
            swapped.to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        let ops = diff_blocks(&old.blocks, &new.blocks);
        assert_eq!(ops.len(), 1, "expected a single Rebuild for the reorder");
        let PatchOp::Rebuild { plan, .. } = &ops[0] else {
            panic!("expected Rebuild after section swap, got {:?}", ops[0]);
        };
        let reuses = plan
            .iter()
            .filter(|s| matches!(s, PlanSlot::Reuse(_)))
            .count();
        // Three paragraph bodies must be reused. (Section headers may
        // rerender because their auto-numbers change.)
        assert!(
            reuses >= 3,
            "expected at least three Reuse slots (one per paragraph body), got {reuses}: {plan:?}"
        );
    }

    const MULTI_PARA_PROOF: &str = "\
\\documentclass{article}
\\begin{document}
\\begin{proof}
First paragraph with math $a+b$.

Second paragraph here.

Third paragraph with $x^2$.
\\end{proof}
\\end{document}
";

    fn render_src(src: &str) -> RenderOutput {
        mathpreview_core::render_project_from_source(
            &PathBuf::from("main.tex"),
            src.to_string(),
            &HtmlOptions::default(),
        )
        .unwrap()
    }

    /// Editing one paragraph inside a multi-paragraph proof must produce a
    /// single in-block `BlockSub` op whose payload carries only the changed
    /// paragraph — not the whole proof. This is the proof-edit latency fix.
    #[test]
    fn proof_paragraph_edit_emits_blocksub_with_only_changed_para() {
        let old = render_src(MULTI_PARA_PROOF);
        let new = render_src(&MULTI_PARA_PROOF.replace(
            "Second paragraph here.",
            "Second paragraph edited.",
        ));

        // Sanity: the proof is one top-level block with captured sub-structure.
        assert_eq!(old.blocks.len(), 1, "proof should be a single block");
        assert!(
            old.blocks[0].sub_blocks.is_some(),
            "proof block should capture sub-block structure"
        );

        let ops = diff_blocks(&old.blocks, &new.blocks);
        assert_eq!(ops.len(), 1, "expected one op, got {ops:?}");
        let PatchOp::BlockSub {
            index,
            child_index,
            remove,
            insert,
            html,
        } = &ops[0]
        else {
            panic!("expected BlockSub, got {:?}", ops[0]);
        };
        assert_eq!(*index, 0);
        assert!(*child_index > 0, "leading paragraphs should be reused");
        assert!(*remove >= 1 && *insert >= 1);
        assert!(html.contains("edited"), "payload must carry the new text");
        assert!(
            !html.contains("First paragraph"),
            "unchanged leading paragraph must not be re-sent: {html}"
        );
        assert!(
            !html.contains("Third paragraph"),
            "unchanged trailing paragraph must not be re-sent: {html}"
        );
    }

    /// Changing the proof's head (`\\begin{proof}[of ...]`) alters the
    /// scaffolding, so the sub-diff must bail and fall back to a full block
    /// replace rather than silently leaving a stale head on the page.
    #[test]
    fn proof_head_change_falls_back_to_full_replace() {
        let old = render_src(
            "\\documentclass{article}\n\\begin{document}\n\\begin{proof}[of Lemma 1]\nBody text here.\n\\end{proof}\n\\end{document}\n",
        );
        let new = render_src(
            "\\documentclass{article}\n\\begin{document}\n\\begin{proof}[of Lemma 2]\nBody text here.\n\\end{proof}\n\\end{document}\n",
        );
        let ops = diff_blocks(&old.blocks, &new.blocks);
        assert_eq!(ops.len(), 1);
        assert!(
            matches!(ops[0], PatchOp::ReplaceRange { .. }),
            "head change should fall back to ReplaceRange, got {:?}",
            ops[0]
        );
    }

    /// Ordinary paragraph blocks carry no sub-structure and keep using the
    /// compact whole-block ReplaceRange path.
    #[test]
    fn plain_paragraph_has_no_sub_blocks() {
        let out = render_src(
            "\\documentclass{article}\n\\begin{document}\nJust a plain paragraph.\n\\end{document}\n",
        );
        assert!(out.blocks.iter().all(|b| b.sub_blocks.is_none()));
    }

    #[test]
    fn rendered_line_insertion_before_math_paragraph_is_compact_patch() {
        let old = mathpreview_core::render_project_from_source(
            &PathBuf::from("main.tex"),
            "\\begin{document}\nFirst, note that if $(x_t, v_t)$ is a solution\nof~\\eqref{eq:Langevin}, then\n\\[\ny=z\n\\]\nAfter.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();
        let new = mathpreview_core::render_project_from_source(
            &PathBuf::from("main.tex"),
            "\\begin{document}\nTest test test\n$a^2+b^2$\n\nFirst, note that if $(x_t, v_t)$ is a solution\nof~\\eqref{eq:Langevin}, then\n\\[\ny=z\n\\]\nAfter.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        let ops = diff_blocks(&old.blocks, &new.blocks);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            PatchOp::ReplaceRange {
                index: 0,
                remove: 0,
                insert: 1,
                ..
            }
        ));
    }

    #[test]
    fn newer_render_attempt_invalidates_older_attempts() {
        let (tx, _) = broadcast::channel(1);
        let (watch_tx, _) = std_mpsc::channel();
        let state = AppState {
            opts: HtmlOptions::default(),
            current: Arc::new(RwLock::new(RenderOutput {
                html: String::new(),
                body_html: String::new(),
                blocks: Vec::new(),
                sync: SyncIndex::new(),
                root_file: PathBuf::from("main.tex"),
                preamble: ExtractedPreamble {
                    macros: Vec::new(),
                    packages_short: Vec::new(),
                    packages_long: Vec::new(),
                    unmapped_packages: Vec::new(),
                    warnings: Vec::new(),
                    raw_preamble: String::new(),
                    title: None,
                    title_short: None,
                    author: None,
                    authors: Vec::new(),
                    author_details: Vec::new(),
                    date: None,
                    sidenote_wrappers: Vec::new(),
                    show_only_refs: false,
                    geometry_margin_mm: None,
                },
                included_files: Vec::new(),
                tikz_assets: HashMap::new(),
            })),
            tx,
            watched: Arc::new(RwLock::new(HashSet::new())),
            watch_tx,
            preamble_cache: Arc::new(RwLock::new(None)),
            buffer_overrides: Arc::new(RwLock::new(HashMap::new())),
            last_blocks: Arc::new(RwLock::new(Vec::new())),
            render_seq: Arc::new(AtomicU64::new(0)),
            jump_seq: Arc::new(AtomicU64::new(0)),
            pending_jump: Arc::new(RwLock::new(None)),
            jump_notify: Arc::new(Notify::new()),
            active_jump_pollers: Arc::new(AtomicU64::new(0)),
            editor_cmd: Arc::new(String::new()),
            session_macros: Arc::new(RwLock::new(Vec::new())),
            config_paths: Arc::new(Vec::new()),
            viewer_config: Arc::new(RwLock::new(
                mathpreview_core::ResolvedConfig::default().viewer,
            )),
            search_query: Arc::new(RwLock::new(EditorSearch::default())),
            last_cursor: Arc::new(RwLock::new(None)),
            file_content_cache: Arc::new(RwLock::new(HashMap::new())),
            tikz_cache: Arc::new(Mutex::new(HashMap::new())),
            log_buffer: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            debug_logging: Arc::new(AtomicBool::new(false)),
            last_jump_poll_ms: Arc::new(AtomicU64::new(0)),
            last_editor_contact_ms: Arc::new(AtomicU64::new(0)),
        };

        let older = begin_render_attempt(&state);
        let newer = begin_render_attempt(&state);
        assert!(!is_latest_render_attempt(&state, older));
        assert!(is_latest_render_attempt(&state, newer));
    }

    #[test]
    fn watcher_ignores_unwatched_directory_events() {
        let watched_path = PathBuf::from("/tmp/mathpreview-main.tex");
        let other_path = PathBuf::from("/tmp/mathpreview-main.tex.swp");
        let mut watched = HashSet::new();
        watched.insert(watched_path.clone());

        let mut ignored = notify::Event::new(notify::EventKind::Any);
        ignored.paths.push(other_path);
        assert!(watched_event_paths(&[ignored.into()], &watched).is_empty());

        let mut accepted = notify::Event::new(notify::EventKind::Any);
        accepted.paths.push(watched_path.clone());
        let changed = watched_event_paths(&[accepted.into()], &watched);
        assert!(changed.contains(&watched_path));
    }

    fn rendered_block(id: &str, html: &str) -> RenderedBlock {
        rendered_block_with_diff(id, html, html)
    }

    fn rendered_block_with_diff(id: &str, html: &str, diff_hash: &str) -> RenderedBlock {
        RenderedBlock {
            id: id.to_string(),
            hash: html.to_string(),
            src: None,
            source_anchors: Vec::new(),
            diff_hash: diff_hash.to_string(),
            html: html.to_string(),
            sub_blocks: None,
        }
    }

    /// Two structurally identical paragraphs (same `diff_hash`); editing the
    /// second one must not pull the first into the patch. This is the
    /// duplicate-hash regression captured in DESIGN.md §13.
    #[test]
    fn duplicate_hash_blocks_dont_scramble_when_one_is_edited() {
        let old = vec![
            rendered_block_with_diff("blk-1", "<p>A</p>", "A"),
            rendered_block_with_diff("blk-2", "<p>P</p>", "P"),
            rendered_block_with_diff("blk-3", "<p>B</p>", "B"),
            rendered_block_with_diff("blk-4", "<p>P</p>", "P"),
            rendered_block_with_diff("blk-5", "<p>C</p>", "C"),
        ];
        let new = vec![
            rendered_block_with_diff("blk-1", "<p>A</p>", "A"),
            rendered_block_with_diff("blk-2", "<p>P</p>", "P"),
            rendered_block_with_diff("blk-3", "<p>B</p>", "B"),
            rendered_block_with_diff("blk-4", "<p>P!</p>", "P-edited"),
            rendered_block_with_diff("blk-5", "<p>C</p>", "C"),
        ];

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 1, "expected exactly one surgical range op");
        assert!(matches!(
            &ops[0],
            PatchOp::ReplaceRange {
                index: 3,
                remove: 1,
                insert: 1,
                ..
            }
        ));
    }

    /// Two disjoint edits in the same render — the previous prefix/suffix
    /// diff collapsed these into one big span covering the unchanged middle;
    /// LCS keeps them surgical.
    #[test]
    fn two_disjoint_edits_emit_two_range_ops() {
        let old = vec![
            rendered_block_with_diff("blk-1", "h-old", "h-old"),
            rendered_block_with_diff("blk-2", "p1", "p1"),
            rendered_block_with_diff("blk-3", "p2", "p2"),
            rendered_block_with_diff("blk-4", "p3", "p3"),
            rendered_block_with_diff("blk-5", "p4", "p4"),
            rendered_block_with_diff("blk-6", "t-old", "t-old"),
        ];
        let new = vec![
            rendered_block_with_diff("blk-1", "h-new", "h-new"),
            rendered_block_with_diff("blk-2", "p1", "p1"),
            rendered_block_with_diff("blk-3", "p2", "p2"),
            rendered_block_with_diff("blk-4", "p3", "p3"),
            rendered_block_with_diff("blk-5", "p4", "p4"),
            rendered_block_with_diff("blk-6", "t-new", "t-new"),
        ];

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 2, "expected two surgical range ops");
        // Ops are emitted in reverse-position order so the client doesn't
        // need to rebase indices.
        assert!(matches!(
            &ops[0],
            PatchOp::ReplaceRange {
                index: 5,
                remove: 1,
                insert: 1,
                ..
            }
        ));
        assert!(matches!(
            &ops[1],
            PatchOp::ReplaceRange {
                index: 0,
                remove: 1,
                insert: 1,
                ..
            }
        ));
    }

    /// Mid-document insertion of a new paragraph — the old single-range diff
    /// already handled this, but as a structural sanity check confirm LCS
    /// still emits one tight insert.
    #[test]
    fn mid_document_insertion_is_single_insert_op() {
        let old = vec![
            rendered_block_with_diff("blk-1", "A", "A"),
            rendered_block_with_diff("blk-2", "B", "B"),
            rendered_block_with_diff("blk-3", "C", "C"),
            rendered_block_with_diff("blk-4", "D", "D"),
        ];
        let new = vec![
            rendered_block_with_diff("blk-1", "A", "A"),
            rendered_block_with_diff("blk-2", "B", "B"),
            rendered_block_with_diff("blk-3", "X", "X"),
            rendered_block_with_diff("blk-4", "C", "C"),
            rendered_block_with_diff("blk-5", "D", "D"),
        ];

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            PatchOp::ReplaceRange {
                index: 2,
                remove: 0,
                insert: 1,
                ..
            }
        ));
    }

    /// Reorder: swap two paragraphs. The diff now emits a single Rebuild op
    /// whose plan consists entirely of Reuse slots — no fresh HTML is sent
    /// because every block in the new layout exists somewhere in the old
    /// layout. This is the path that preserves typeset MathJax across moves.
    #[test]
    fn reorder_emits_rebuild_with_pure_reuse_slots() {
        let old = vec![
            rendered_block_with_diff("blk-1", "<p>A</p>", "A"),
            rendered_block_with_diff("blk-2", "<p>B</p>", "B"),
            rendered_block_with_diff("blk-3", "<p>C</p>", "C"),
            rendered_block_with_diff("blk-4", "<p>D</p>", "D"),
        ];
        // Swap B and C.
        let new = vec![
            rendered_block_with_diff("blk-1", "<p>A</p>", "A"),
            rendered_block_with_diff("blk-2", "<p>C</p>", "C"),
            rendered_block_with_diff("blk-3", "<p>B</p>", "B"),
            rendered_block_with_diff("blk-4", "<p>D</p>", "D"),
        ];

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 1, "reorder should collapse to one Rebuild op");
        let PatchOp::Rebuild {
            start,
            old_count,
            plan,
        } = &ops[0]
        else {
            panic!("expected Rebuild op, got {ops:?}");
        };
        // Prefix/suffix trim — A and D anchor at positions 0 and 3, so the
        // rebuilt slice is just the middle two.
        assert_eq!(*start, 1);
        assert_eq!(*old_count, 2);
        assert_eq!(plan.len(), 2);
        // Both slots must be Reuse pointing back into the OLD slice; no
        // fresh HTML is shipped over the wire.
        for slot in plan {
            assert!(
                matches!(slot, PlanSlot::Reuse(_)),
                "reorder should not emit any Insert slots: {plan:?}"
            );
        }
        // The two reuse indices must be exactly the two old positions, but
        // referenced in the new (swapped) order — i.e. they cover positions
        // 1 and 2 of the old layout, not the same one twice.
        let srcs: Vec<usize> = plan
            .iter()
            .filter_map(|s| {
                if let PlanSlot::Reuse(i) = s {
                    Some(*i)
                } else {
                    None
                }
            })
            .collect();
        let mut sorted = srcs.clone();
        sorted.sort();
        assert_eq!(sorted, vec![1, 2]);
        assert_ne!(srcs, vec![1, 2], "expected swapped order, got {srcs:?}");
    }

    /// A section moves AND an unrelated paragraph is edited in the same
    /// render. Both changes should be coalesced into a single Rebuild op —
    /// the moved blocks become Reuse slots (preserving typeset math) and
    /// the edited paragraph becomes an Insert slot with fresh HTML.
    #[test]
    fn move_plus_typo_emits_single_rebuild_with_mixed_plan() {
        let old = vec![
            rendered_block_with_diff("blk-1", "<p>head</p>", "head"),
            rendered_block_with_diff("blk-2", "<p>typo</p>", "typo"),
            rendered_block_with_diff("blk-3", "<sec>S</sec>", "section"),
            rendered_block_with_diff("blk-4", "<p>body1</p>", "body1"),
            rendered_block_with_diff("blk-5", "<p>tail</p>", "tail"),
        ];
        // Move "section" past "body1" AND fix the typo.
        let new = vec![
            rendered_block_with_diff("blk-1", "<p>head</p>", "head"),
            rendered_block_with_diff("blk-2", "<p>fixed</p>", "fixed"),
            rendered_block_with_diff("blk-3", "<p>body1</p>", "body1"),
            rendered_block_with_diff("blk-4", "<sec>S</sec>", "section"),
            rendered_block_with_diff("blk-5", "<p>tail</p>", "tail"),
        ];

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 1);
        let PatchOp::Rebuild { plan, .. } = &ops[0] else {
            panic!("expected Rebuild op, got {ops:?}");
        };
        let inserts = plan
            .iter()
            .filter(|s| matches!(s, PlanSlot::Insert(_)))
            .count();
        let reuses = plan
            .iter()
            .filter(|s| matches!(s, PlanSlot::Reuse(_)))
            .count();
        // Exactly one fresh insert (the typo fix). The rest are reuse,
        // including the moved section block.
        assert_eq!(inserts, 1, "only the edited paragraph should be Insert");
        assert!(
            reuses >= 1,
            "moved + anchored blocks should produce Reuse slots: {plan:?}"
        );
    }

    /// Pure inserts/deletes/edits with no positional reuse must keep
    /// emitting range ops — the Rebuild path costs more on the wire and
    /// should only fire when there is something to reuse out of position.
    #[test]
    fn pure_inserts_and_deletes_do_not_trigger_rebuild() {
        let old = vec![
            rendered_block_with_diff("blk-1", "A", "A"),
            rendered_block_with_diff("blk-2", "B", "B"),
            rendered_block_with_diff("blk-3", "C", "C"),
        ];
        // Insert a new "X" between B and C; no moves.
        let new = vec![
            rendered_block_with_diff("blk-1", "A", "A"),
            rendered_block_with_diff("blk-2", "B", "B"),
            rendered_block_with_diff("blk-3", "X", "X"),
            rendered_block_with_diff("blk-4", "C", "C"),
        ];

        let ops = diff_blocks(&old, &new);
        for op in &ops {
            assert!(
                matches!(op, PatchOp::ReplaceRange { .. }),
                "pure insert should stay on the range path, got Rebuild: {op:?}"
            );
        }
    }

    /// Reverse-order emission contract for the range path: when multiple
    /// range ops are emitted, the indices must be monotonically
    /// non-increasing so the client can apply them sequentially against the
    /// live DOM without rebasing.
    #[test]
    fn range_ops_are_emitted_in_reverse_position_order() {
        let old: Vec<_> = (0..10)
            .map(|i| {
                rendered_block_with_diff(&format!("blk-{i}"), &format!("o{i}"), &format!("h{i}"))
            })
            .collect();
        // Edit blocks 1, 4, 7. Hashes are all unique (no moves).
        let mut new = old.clone();
        new[1] = rendered_block_with_diff("blk-1", "n1", "h1-new");
        new[4] = rendered_block_with_diff("blk-4", "n4", "h4-new");
        new[7] = rendered_block_with_diff("blk-7", "n7", "h7-new");

        let ops = diff_blocks(&old, &new);
        assert_eq!(ops.len(), 3);
        let indices: Vec<usize> = ops
            .iter()
            .map(|op| match op {
                PatchOp::ReplaceRange { index, .. } => *index,
                PatchOp::Rebuild { .. } => panic!("unexpected Rebuild in pure-edit case"),
                PatchOp::BlockSub { .. } => panic!("unexpected BlockSub in pure-edit case"),
            })
            .collect();
        assert_eq!(indices, vec![7, 4, 1]);
    }

    fn empty_preamble() -> ExtractedPreamble {
        ExtractedPreamble {
            macros: Vec::new(),
            packages_short: Vec::new(),
            packages_long: Vec::new(),
            unmapped_packages: Vec::new(),
            warnings: Vec::new(),
            raw_preamble: String::new(),
            title: None,
            title_short: None,
            author: None,
            authors: Vec::new(),
            author_details: Vec::new(),
            date: None,
            sidenote_wrappers: Vec::new(),
            show_only_refs: false,
            geometry_margin_mm: None,
        }
    }

    /// Multi-file editing: `POST /buffer` with `X-Mathpreview-Path` pointing at
    /// an `\input`-ed child must store the body keyed by canonical child path
    /// and have the next render splice it in instead of the disk content.
    #[tokio::test]
    async fn buffer_push_with_child_path_splices_override_into_root_render() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mp-buffer-push-child-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("main.tex");
        let child = dir.join("child.tex");
        std::fs::write(
            &root,
            "\\documentclass{article}\n\\begin{document}\nBefore.\n\\input{child}\nAfter.\n\\end{document}\n",
        )
        .unwrap();
        std::fs::write(&child, "Diskchild\n").unwrap();

        let root_canon = root.canonicalize().unwrap();
        let child_canon = child.canonicalize().unwrap();

        let (tx, _rx) = broadcast::channel(8);
        let (watch_tx, _watch_rx) = std_mpsc::channel();
        let mut watched_set = HashSet::new();
        watched_set.insert(root_canon.clone());
        watched_set.insert(child_canon.clone());

        let state = AppState {
            opts: HtmlOptions::default(),
            current: Arc::new(RwLock::new(RenderOutput {
                html: String::new(),
                body_html: String::new(),
                blocks: Vec::new(),
                sync: SyncIndex::new(),
                root_file: root_canon.clone(),
                preamble: empty_preamble(),
                included_files: vec![child_canon.clone()],
                tikz_assets: HashMap::new(),
            })),
            tx,
            watched: Arc::new(RwLock::new(watched_set)),
            watch_tx,
            preamble_cache: Arc::new(RwLock::new(None)),
            buffer_overrides: Arc::new(RwLock::new(HashMap::new())),
            last_blocks: Arc::new(RwLock::new(Vec::new())),
            render_seq: Arc::new(AtomicU64::new(0)),
            jump_seq: Arc::new(AtomicU64::new(0)),
            pending_jump: Arc::new(RwLock::new(None)),
            jump_notify: Arc::new(Notify::new()),
            active_jump_pollers: Arc::new(AtomicU64::new(0)),
            editor_cmd: Arc::new(String::new()),
            session_macros: Arc::new(RwLock::new(Vec::new())),
            config_paths: Arc::new(Vec::new()),
            viewer_config: Arc::new(RwLock::new(
                mathpreview_core::ResolvedConfig::default().viewer,
            )),
            search_query: Arc::new(RwLock::new(EditorSearch::default())),
            last_cursor: Arc::new(RwLock::new(None)),
            file_content_cache: Arc::new(RwLock::new(HashMap::new())),
            tikz_cache: Arc::new(Mutex::new(HashMap::new())),
            log_buffer: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            debug_logging: Arc::new(AtomicBool::new(false)),
            last_jump_poll_ms: Arc::new(AtomicU64::new(0)),
            last_editor_contact_ms: Arc::new(AtomicU64::new(0)),
        };

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-mathpreview-path",
            axum::http::HeaderValue::from_str(child_canon.to_str().unwrap()).unwrap(),
        );

        let status = serve_buffer_push(
            axum::extract::State(state.clone()),
            headers,
            "Livechild\n".to_string(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

        let overrides = state.buffer_overrides.read().await;
        assert_eq!(
            overrides.get(&child_canon).map(String::as_str),
            Some("Livechild\n"),
            "override map must be keyed by the canonical child path",
        );
        drop(overrides);

        let current = state.current.read().await;
        // Prose is split into per-word source-sync spans; check for the unique
        // override token and the absence of the on-disk token instead of a
        // contiguous substring.
        assert!(
            current.body_html.contains(">Livechild<"),
            "rendered body should splice the override; got: {}",
            current.body_html,
        );
        assert!(
            !current.body_html.contains("Diskchild"),
            "rendered body should not contain disk content; got: {}",
            current.body_html,
        );
        assert!(
            current.body_html.contains(">Before<") && current.body_html.contains(">After<"),
            "rendered body should still contain surrounding root content; got: {}",
            current.body_html,
        );
        drop(current);

        assert_eq!(
            std::fs::read_to_string(&child).unwrap(),
            "Diskchild\n",
            "buffer-push must not write the override to disk",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `POST /buffer` with a path that's neither the root nor a watched
    /// included file must be rejected without poisoning the override map.
    #[tokio::test]
    async fn buffer_push_rejects_path_outside_project() {
        let (tx, _rx) = broadcast::channel(8);
        let (watch_tx, _watch_rx) = std_mpsc::channel();
        let mut watched_set = HashSet::new();
        let root = PathBuf::from("/tmp/mathpreview-stranger-root.tex");
        watched_set.insert(root.clone());

        let state = AppState {
            opts: HtmlOptions::default(),
            current: Arc::new(RwLock::new(RenderOutput {
                html: String::new(),
                body_html: String::new(),
                blocks: Vec::new(),
                sync: SyncIndex::new(),
                root_file: root,
                preamble: empty_preamble(),
                included_files: Vec::new(),
                tikz_assets: HashMap::new(),
            })),
            tx,
            watched: Arc::new(RwLock::new(watched_set)),
            watch_tx,
            preamble_cache: Arc::new(RwLock::new(None)),
            buffer_overrides: Arc::new(RwLock::new(HashMap::new())),
            last_blocks: Arc::new(RwLock::new(Vec::new())),
            render_seq: Arc::new(AtomicU64::new(0)),
            jump_seq: Arc::new(AtomicU64::new(0)),
            pending_jump: Arc::new(RwLock::new(None)),
            jump_notify: Arc::new(Notify::new()),
            active_jump_pollers: Arc::new(AtomicU64::new(0)),
            editor_cmd: Arc::new(String::new()),
            session_macros: Arc::new(RwLock::new(Vec::new())),
            config_paths: Arc::new(Vec::new()),
            viewer_config: Arc::new(RwLock::new(
                mathpreview_core::ResolvedConfig::default().viewer,
            )),
            search_query: Arc::new(RwLock::new(EditorSearch::default())),
            last_cursor: Arc::new(RwLock::new(None)),
            file_content_cache: Arc::new(RwLock::new(HashMap::new())),
            tikz_cache: Arc::new(Mutex::new(HashMap::new())),
            log_buffer: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            debug_logging: Arc::new(AtomicBool::new(false)),
            last_jump_poll_ms: Arc::new(AtomicU64::new(0)),
            last_editor_contact_ms: Arc::new(AtomicU64::new(0)),
        };

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-mathpreview-path",
            axum::http::HeaderValue::from_static("/tmp/mathpreview-not-in-project.tex"),
        );

        let status = serve_buffer_push(
            axum::extract::State(state.clone()),
            headers,
            "stranger body\n".to_string(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            state.buffer_overrides.read().await.is_empty(),
            "rejected buffer-push must not insert into the override map",
        );
    }
}
