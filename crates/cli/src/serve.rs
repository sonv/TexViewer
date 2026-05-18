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
    atomic::{AtomicU64, Ordering},
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
use tokio::sync::{broadcast, RwLock};

use mathpreview_core::{
    bibtex::{self, BibEntry, BibStyle},
    macros::{self, ExtractedPreamble},
    numbering, parser, project, render_project, renderer,
    sync::SyncIndex,
    HtmlOptions, RenderOutput, RenderedBlock,
};

const WS_PROTOCOL_VERSION: &str = "25";

#[derive(Clone)]
struct AppState {
    input: PathBuf,
    opts: HtmlOptions,
    current: Arc<RwLock<RenderOutput>>,
    tx: broadcast::Sender<String>,
    watched: Arc<RwLock<HashSet<PathBuf>>>,
    watch_tx: std_mpsc::Sender<HashSet<PathBuf>>,
    /// Cached preamble + bib state, keyed on a hash of the preamble source.
    preamble_cache: Arc<RwLock<Option<PreambleCache>>>,
    /// Last broadcast block sequence — diff target for the next render.
    /// Updated atomically with each broadcast so reconnects and patches
    /// stay in sync.
    last_blocks: Arc<RwLock<Vec<RenderedBlock>>>,
    /// Monotonic render attempt id. Buffer pushes can complete out of order;
    /// only the newest attempt is allowed to update the preview.
    render_seq: Arc<AtomicU64>,
    jump_seq: Arc<AtomicU64>,
    pending_jump: Arc<RwLock<Option<SourceJump>>>,
}

struct PreambleCache {
    hash: u64,
    preamble: ExtractedPreamble,
    bib: HashMap<String, BibEntry>,
    bib_style: BibStyle,
}

#[derive(Debug, Deserialize)]
struct SourceRequest {
    file: PathBuf,
    line: u32,
    col: Option<u32>,
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
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub async fn run(input: PathBuf, host: String, port: u16, opts: HtmlOptions) -> Result<()> {
    let initial = render_project(&input, &opts)
        .with_context(|| format!("initial render of {}", input.display()))?;
    let mut watched: HashSet<PathBuf> = HashSet::new();
    watched.insert(initial.root_file.clone());
    for f in &initial.included_files {
        watched.insert(f.clone());
    }
    let mem_at_start = resident_mib()
        .map(|m| format!("{m:.1} MiB rss"))
        .unwrap_or_else(|| "rss ?".into());
    eprintln!(
        "mathpreview: rendered {} ({} macros, {} packages, {} files watched; {})",
        initial.root_file.display(),
        initial.preamble.macros.len(),
        initial.preamble.packages_long.len(),
        watched.len(),
        mem_at_start,
    );

    let (tx, _rx) = broadcast::channel::<String>(16);
    let (watch_tx, watch_rx) = std_mpsc::channel::<HashSet<PathBuf>>();
    let last_blocks = initial.blocks.clone();
    let state = AppState {
        input,
        opts,
        current: Arc::new(RwLock::new(initial)),
        tx,
        watched: Arc::new(RwLock::new(watched)),
        watch_tx,
        preamble_cache: Arc::new(RwLock::new(None)),
        last_blocks: Arc::new(RwLock::new(last_blocks)),
        render_seq: Arc::new(AtomicU64::new(0)),
        jump_seq: Arc::new(AtomicU64::new(0)),
        pending_jump: Arc::new(RwLock::new(None)),
    };

    spawn_watcher(state.clone(), watch_rx);

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/assets/*path", get(serve_asset))
        .route("/vendor/mathjax/*path", get(serve_vendor_mathjax))
        .route("/ws", get(serve_ws))
        .route("/buffer", axum::routing::post(serve_buffer_push))
        .route("/cursor", post(serve_cursor))
        .route("/jump", get(serve_jump_poll).post(serve_jump))
        .route("/restart", post(serve_restart))
        .route("/stop", post(serve_stop))
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(state);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    eprintln!("mathpreview serving on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
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

/// Compile-time path to the vendored MathJax bundle. MathJax 4 ships its
/// bundles at the package root (no more `es5/` subdirectory like in v3).
/// Resolved from `CARGO_MANIFEST_DIR` so the daemon finds the bundle
/// whether it's run via `cargo run` or as a release binary in the same
/// checkout.
const MATHJAX_VENDOR_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/vendor/mathjax");

async fn serve_vendor_mathjax(AxumPath(path): AxumPath<String>) -> Response {
    let Some(rel) = clean_asset_path(&path) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let root = match tokio::fs::canonicalize(MATHJAX_VENDOR_ROOT).await {
        Ok(p) => p,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let candidate = root.join(rel);
    let canonical = match tokio::fs::canonicalize(&candidate).await {
        Ok(p) => p,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    if !canonical.starts_with(&root) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let bytes = match tokio::fs::read(&canonical).await {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let content_type = match canonical.extension().and_then(|e| e.to_str()) {
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    };
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
    let file = normalize_source_path(req.file);
    let line = req.line.max(1);
    let col = req.col.unwrap_or(1).max(1);
    let element_id = {
        let current = state.current.read().await;
        current
            .sync
            .lookup_leaf_by_source_position(&file, line, col)
            .map(|entry| entry.element_id.clone())
    };
    let payload = serde_json::json!({
        "event": "source-cursor",
        "file": file,
        "line": line,
        "col": col,
        "element_id": element_id,
    })
    .to_string();
    let _ = state.tx.send(payload);
    axum::http::StatusCode::NO_CONTENT
}

async fn serve_jump(
    State(state): State<AppState>,
    Json(req): Json<SourceRequest>,
) -> axum::http::StatusCode {
    let seq = state.jump_seq.fetch_add(1, Ordering::AcqRel) + 1;
    let jump = SourceJump {
        seq,
        file: normalize_source_path(req.file),
        line: req.line.max(1),
        col: req.col.unwrap_or(1).max(1),
    };
    *state.pending_jump.write().await = Some(jump.clone());
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
    let after = query
        .get("after")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let jump = state.pending_jump.read().await.clone();
    match jump.filter(|jump| jump.seq > after) {
        Some(jump) => Json(jump).into_response(),
        None => axum::http::StatusCode::NO_CONTENT.into_response(),
    }
}

fn normalize_source_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

async fn serve_restart() -> axum::http::StatusCode {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("mathpreview: restart failed: cannot resolve current exe: {e}");
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
            eprintln!("mathpreview: restarting server");
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                std::process::exit(0);
            });
            axum::http::StatusCode::ACCEPTED
        }
        Err(e) => {
            eprintln!("mathpreview: restart failed: {e}");
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn serve_stop() -> axum::http::StatusCode {
    eprintln!("mathpreview: stopping server");
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

/// `POST /buffer` — editor pushes the current buffer content. The path of
/// the root file is identified via the `X-Mathpreview-Path` header. If absent,
/// the daemon assumes the launched root. If present and it doesn't match the
/// launched root, the request is rejected; multi-buffer substitution needs a
/// project-aware protocol.
async fn serve_buffer_push(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::http::StatusCode {
    let path_header = headers
        .get("x-mathpreview-path")
        .and_then(|v| v.to_str().ok())
        .map(PathBuf::from);

    let current_root = {
        let current = state.current.read().await;
        current.root_file.clone()
    };
    let root = match path_header.clone() {
        Some(p) => p.canonicalize().unwrap_or(p),
        None => current_root.clone(),
    };
    if root != current_root {
        eprintln!(
            "mathpreview: rejected buffer-push for {}; server root is {}",
            root.display(),
            current_root.display(),
        );
        return axum::http::StatusCode::BAD_REQUEST;
    }

    let t0 = std::time::Instant::now();
    let body_len = body.len();
    let seq = begin_render_attempt(&state);
    if !is_buffer_renderable(&body) {
        eprintln!(
            "mathpreview: buffer-push #{seq} {} bytes — incomplete, deferring",
            body_len
        );
        return axum::http::StatusCode::ACCEPTED;
    }

    match render_cached(&state, &root, body).await {
        Ok((out, timing)) => {
            if !is_latest_render_attempt(&state, seq) {
                eprintln!("mathpreview: buffer-push #{seq} {body_len}b → stale render discarded");
                return axum::http::StatusCode::NO_CONTENT;
            }
            update_watched(&state, &out).await;
            let (op_count, kind) = broadcast_render(&state, out).await;
            let mem = {
                let blocks = state.last_blocks.read().await;
                fmt_mem_log(&state, &blocks)
            };
            eprintln!(
                "mathpreview: buffer-push #{seq} {body_len}b → total {tot} ms ({op_count} {kind}; parse {p}, preamble {pr}, body-parse {bp}, number {n}, render {r}; cache {cache}; {mem})",
                tot = t0.elapsed().as_millis(),
                p = timing.parse_ms,
                pr = timing.preamble_ms,
                bp = timing.body_parse_ms,
                n = timing.number_ms,
                r = timing.render_ms,
                cache = if timing.cache_hit { "hit" } else { "miss" },
            );
            axum::http::StatusCode::NO_CONTENT
        }
        Err(e) => {
            if !is_latest_render_attempt(&state, seq) {
                eprintln!("mathpreview: buffer-push #{seq} stale render error discarded: {e:#}");
                return axum::http::StatusCode::NO_CONTENT;
            }
            eprintln!("mathpreview: buffer-push render error: {e:#}");
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

fn watched_set(out: &RenderOutput) -> HashSet<PathBuf> {
    let mut watched = HashSet::new();
    watched.insert(out.root_file.clone());
    for f in &out.included_files {
        watched.insert(f.clone());
    }
    watched
}

async fn update_watched(state: &AppState, out: &RenderOutput) {
    let watched = watched_set(out);
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
    source: String,
) -> anyhow::Result<(RenderOutput, RenderTiming)> {
    let mut t = RenderTiming::default();
    let t0 = std::time::Instant::now();
    let project = project::load_project_from_source(root, source)?;
    t.parse_ms = t0.elapsed().as_millis();

    // Preamble + bib + style — cached on the hash of the preamble source.
    // Editing the body keeps the preamble identical, so this is a clean hit.
    let pre_hash = fnv_hash(&project.preamble.source);
    let t1 = std::time::Instant::now();
    let (preamble, bib, bib_style) = {
        let guard = state.preamble_cache.read().await;
        if let Some(c) = guard.as_ref().filter(|c| c.hash == pre_hash) {
            t.cache_hit = true;
            (c.preamble.clone(), c.bib.clone(), c.bib_style)
        } else {
            drop(guard);
            let preamble = macros::extract_preamble(&project)?;
            let bib = bibtex::load_project_bib(&project)?;
            let bib_style = bibtex::detect_bib_style(&preamble.raw_preamble);
            *state.preamble_cache.write().await = Some(PreambleCache {
                hash: pre_hash,
                preamble: preamble.clone(),
                bib: bib.clone(),
                bib_style,
            });
            (preamble, bib, bib_style)
        }
    };
    t.preamble_ms = t1.elapsed().as_millis();

    let t2 = std::time::Instant::now();
    let mut body = parser::parse_body(&project)?;
    t.body_parse_ms = t2.elapsed().as_millis();

    let t3 = std::time::Instant::now();
    let labels = numbering::assign_numbers(&mut body, &bib, bib_style);
    t.number_ms = t3.elapsed().as_millis();

    let t4 = std::time::Instant::now();
    let mut sync = SyncIndex::new();
    let rendered = renderer::render(
        &body,
        &preamble,
        &labels,
        &bib,
        bib_style,
        &mut sync,
        &state.opts,
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
    };
    Ok((out, t))
}

async fn handle_ws(socket: WebSocket, state: AppState, needs_reload: bool) {
    let (mut sender, mut receiver) = socket.split();
    if needs_reload {
        let payload = serde_json::json!({ "event": "full-reload" }).to_string();
        let _ = sender.send(Message::Text(payload)).await;
    }
    let mut rx = state.tx.subscribe();
    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(payload) => {
                    if sender.send(Message::Text(payload)).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
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
async fn broadcast_render(state: &AppState, out: RenderOutput) -> (usize, &'static str) {
    let (ops, patch_cost) = {
        let prev = state.last_blocks.read().await;
        let ops = diff_blocks(&prev, &out.blocks);
        let patch_cost = ops.iter().map(PatchOp::cost).sum::<usize>();
        (ops, patch_cost)
    };
    let block_count = out.blocks.len();
    let fallback_full = patch_cost * 2 > block_count.max(1);

    // Sample memory after the render so the client sees the cost at the
    // point the page actually displays the update.
    let rss = resident_mib();

    let (payload, op_count, kind) = if fallback_full {
        let payload = serde_json::json!({
            "event": "body-updated",
            "html": out.body_html,
            "rss_mib": rss,
        })
        .to_string();
        (payload, block_count, "blocks (full)")
    } else {
        let ops_json: Vec<_> = ops.iter().map(PatchOp::to_json).collect();
        let blocks_json: Vec<_> = out
            .blocks
            .iter()
            .map(|block| {
                serde_json::json!({
                    "id": block.id,
                    "hash": block.hash,
                    "src": block.src,
                    "anchors": block.source_anchors,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "event": "patch",
            "ops": ops_json,
            "blocks": blocks_json,
            "rss_mib": rss,
        })
        .to_string();
        (payload, patch_cost, "ops")
    };

    *state.last_blocks.write().await = out.blocks.clone();
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
    let html = new_mid[new_start..new_end]
        .iter()
        .map(|block| block.html.as_str())
        .collect::<String>();
    let _ = old_mid;
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
fn is_buffer_renderable(source: &str) -> bool {
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

fn spawn_watcher(state: AppState, watch_rx: std_mpsc::Receiver<HashSet<PathBuf>>) {
    // notify-debouncer is sync; bridge into tokio via a std::sync::mpsc-style
    // channel polled from a dedicated thread that posts work back to a Tokio
    // task via an unbounded channel.
    let (file_tx, mut file_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let watched_for_thread = state.watched.clone();

    std::thread::spawn(move || {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel();
        let mut debouncer = match new_debouncer(Duration::from_millis(120), None, raw_tx) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("mathpreview: cannot start file watcher: {e}");
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
        let mut sync_watched_dirs = |files: HashSet<PathBuf>| {
            for f in files {
                let dir = f.parent().unwrap_or(Path::new(".")).to_path_buf();
                if watched_dirs.insert(dir.clone()) {
                    match debouncer.watcher().watch(&dir, RecursiveMode::NonRecursive) {
                        Ok(()) => eprintln!("mathpreview: watching {}", dir.display()),
                        Err(e) => eprintln!("mathpreview: failed to watch {}: {e}", dir.display()),
                    }
                }
            }
        };
        sync_watched_dirs(initial.into_iter().collect());

        loop {
            while let Ok(files) = watch_rx.try_recv() {
                sync_watched_dirs(files);
            }
            match raw_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(events) => match events {
                    Ok(evs) => {
                        if evs.is_empty() {
                            continue;
                        }
                        eprintln!("mathpreview: change detected ({} events)", evs.len());
                        if file_tx.send(()).is_err() {
                            break;
                        }
                    }
                    Err(errs) => {
                        for e in errs {
                            eprintln!("mathpreview: watcher: {e}");
                        }
                    }
                },
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            };
        }
    });

    tokio::spawn(async move {
        while file_rx.recv().await.is_some() {
            // Drain any queued ticks — we only need the latest state.
            while file_rx.try_recv().is_ok() {}
            let seq = begin_render_attempt(&state);
            match render_project(&state.input, &state.opts) {
                Ok(new_output) => {
                    if !is_latest_render_attempt(&state, seq) {
                        eprintln!("mathpreview: file-change #{seq} stale render discarded");
                        continue;
                    }
                    update_watched(&state, &new_output).await;
                    *state.preamble_cache.write().await = None;
                    let (op_count, kind) = broadcast_render(&state, new_output).await;
                    let mem = {
                        let blocks = state.last_blocks.read().await;
                        fmt_mem_log(&state, &blocks)
                    };
                    eprintln!("mathpreview: file-change → {op_count} {kind}; {mem}");
                }
                Err(e) => {
                    if !is_latest_render_attempt(&state, seq) {
                        eprintln!(
                            "mathpreview: file-change #{seq} stale render error discarded: {e:#}"
                        );
                        continue;
                    }
                    eprintln!("mathpreview: render error: {e:#}");
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

#[cfg(test)]
mod tests {
    use super::{
        begin_render_attempt, diff_blocks, is_buffer_renderable, is_latest_render_attempt,
        websocket_needs_reload, AppState, PatchOp, PlanSlot, WS_PROTOCOL_VERSION,
    };
    use mathpreview_core::{
        renderer::{HtmlOptions, RenderedBlock},
        sync::SyncIndex,
        ExtractedPreamble, RenderOutput,
    };
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::{atomic::AtomicU64, mpsc as std_mpsc, Arc};
    use tokio::sync::{broadcast, RwLock};

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
    fn buffer_guard_only_defers_unclosed_math() {
        assert!(is_buffer_renderable(r"\begin{document}\section{A"));
        assert!(is_buffer_renderable(r"\begin{document}\begin{proof} text"));
        assert!(!is_buffer_renderable(r"\begin{document} $x"));
        assert!(!is_buffer_renderable(r"\begin{document} \[x"));
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
            input: PathBuf::from("main.tex"),
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
                },
                included_files: Vec::new(),
            })),
            tx,
            watched: Arc::new(RwLock::new(HashSet::new())),
            watch_tx,
            preamble_cache: Arc::new(RwLock::new(None)),
            last_blocks: Arc::new(RwLock::new(Vec::new())),
            render_seq: Arc::new(AtomicU64::new(0)),
            jump_seq: Arc::new(AtomicU64::new(0)),
            pending_jump: Arc::new(RwLock::new(None)),
        };

        let older = begin_render_attempt(&state);
        let newer = begin_render_attempt(&state);
        assert!(!is_latest_render_attempt(&state, older));
        assert!(is_latest_render_attempt(&state, newer));
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
            })
            .collect();
        assert_eq!(indices, vec![7, 4, 1]);
    }
}
