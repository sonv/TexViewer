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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::{Html, IntoResponse},
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::new_debouncer;
use tokio::sync::{broadcast, RwLock};

use mathpreview_core::{
    bibtex::{self, BibEntry, BibStyle},
    macros::{self, ExtractedPreamble},
    numbering, parser, project, render_project, renderer,
    sync::SyncIndex,
    HtmlOptions, RenderOutput, RenderedBlock,
};

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
}

struct PreambleCache {
    hash: u64,
    preamble: ExtractedPreamble,
    bib: HashMap<String, BibEntry>,
    bib_style: BibStyle,
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
    };

    spawn_watcher(state.clone(), watch_rx);

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/ws", get(serve_ws))
        .route("/buffer", axum::routing::post(serve_buffer_push))
        .route("/restart", post(serve_restart))
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
    Html(current.html.clone())
}

async fn serve_ws(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
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
    if !is_buffer_renderable(&body) {
        eprintln!(
            "mathpreview: buffer-push {} bytes — incomplete, deferring",
            body_len
        );
        return axum::http::StatusCode::ACCEPTED;
    }

    match render_cached(&state, &root, body).await {
        Ok((out, timing)) => {
            update_watched(&state, &out).await;
            let (op_count, kind) = broadcast_render(&state, out).await;
            let mem = {
                let blocks = state.last_blocks.read().await;
                fmt_mem_log(&state, &blocks)
            };
            eprintln!(
                "mathpreview: buffer-push {body_len}b → total {tot} ms ({op_count} {kind}; parse {p}, preamble {pr}, body-parse {bp}, number {n}, render {r}; cache {cache}; {mem})",
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

async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
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
    let ops = {
        let prev = state.last_blocks.read().await;
        diff_blocks(&prev, &out.blocks)
    };
    let block_count = out.blocks.len();
    let fallback_full = ops.len() * 2 > block_count.max(1);

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
        let n = ops.len();
        let ops_json: Vec<_> = ops.iter().map(PatchOp::to_json).collect();
        let payload = serde_json::json!({
            "event": "patch",
            "ops": ops_json,
            "rss_mib": rss,
        })
        .to_string();
        (payload, n, "ops")
    };

    *state.last_blocks.write().await = out.blocks.clone();
    *state.current.write().await = out;
    let _ = state.tx.send(payload);
    (op_count, kind)
}

/// One element of a block-level patch. Position-based: ops apply in order,
/// referencing block ids that exist after prior ops have been applied.
#[derive(Debug)]
enum PatchOp {
    Replace { id: String, html: String },
    Append { html: String },
    Remove { id: String },
}

impl PatchOp {
    fn to_json(&self) -> serde_json::Value {
        match self {
            PatchOp::Replace { id, html } => serde_json::json!({
                "type": "replace", "id": id, "html": html,
            }),
            PatchOp::Append { html } => serde_json::json!({
                "type": "append", "html": html,
            }),
            PatchOp::Remove { id } => serde_json::json!({
                "type": "remove", "id": id,
            }),
        }
    }
}

/// Position-based diff. Replaces blocks whose hash differs at the same
/// position; appends extras at the end; removes trailing blocks no longer
/// present. Intentionally simple — a paragraph insertion in the middle
/// invalidates every subsequent block, which is fine for typing within a
/// single paragraph (the common case) and falls back to a full
/// `body-updated` event for larger structural changes.
fn diff_blocks(old: &[RenderedBlock], new: &[RenderedBlock]) -> Vec<PatchOp> {
    let mut ops = Vec::new();
    let common = old.len().min(new.len());
    for i in 0..common {
        if old[i].hash != new[i].hash {
            ops.push(PatchOp::Replace {
                id: old[i].id.clone(),
                html: new[i].html.clone(),
            });
        }
    }
    for block in new.iter().skip(common) {
        ops.push(PatchOp::Append {
            html: block.html.clone(),
        });
    }
    for block in old.iter().skip(common) {
        ops.push(PatchOp::Remove {
            id: block.id.clone(),
        });
    }
    ops
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
    let mut brace_depth = 0i32;
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
            b'{' => brace_depth += 1,
            b'}' => {
                brace_depth -= 1;
                if brace_depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    // \begin{...} / \end{...} count match (cheap check, doesn't enforce
    // nesting order — but the parser handles that, and a mismatched env
    // usually still renders something rather than crashes).
    let begins = source.matches("\\begin{").count();
    let ends = source.matches("\\end{").count();
    !in_inline && !in_display && brace_depth == 0 && begins == ends
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
            match render_project(&state.input, &state.opts) {
                Ok(new_output) => {
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
