//! Shared library for the mathpreview-cli binaries — the serve daemon, the
//! HTML-render options, and (behind the `gui` feature) the native Locus window.
//! Both the `mathpreview-cli` binary and the `locus` binary build on this.

use std::path::{Path, PathBuf};

// `Context`/`Result` are only used by `run_view` (gui feature).
#[cfg(feature = "gui")]
use anyhow::{Context, Result};
use mathpreview_core::{Engine, HtmlOptions, MathJaxEngine};

pub mod serve;
#[cfg(feature = "gui")]
pub mod view;

/// Default `--editor` template for Cmd/Ctrl-click "reveal source": jump to the
/// source line in a running nvim (uses `$NVIM_LISTEN_ADDRESS`, else `$NVIM`).
pub const DEFAULT_EDITOR: &str = r#"nvim --server "${NVIM_LISTEN_ADDRESS:-$NVIM}" --remote-send "<C-\\><C-N>:e +{line} {file}<CR>""#;

/// Build the serve-mode `HtmlOptions` and resolved config-file list shared by
/// the `serve` daemon and the `view`/`locus` window.
pub fn build_serve_opts(
    input: &Path,
    mathjax_url: Option<String>,
    extra_macros: &[PathBuf],
    extra_configs: &[PathBuf],
) -> (HtmlOptions, Vec<PathBuf>) {
    // serve-mode default: use the vendored bundle so the page works offline.
    let url = mathjax_url.unwrap_or_else(|| "/vendor/mathjax/tex-svg.js".to_string());
    let input_dir = input.parent().unwrap_or_else(|| Path::new("."));
    let macro_overrides = mathpreview_core::discover_macro_overrides(input_dir, extra_macros);
    let config_files = mathpreview_core::discover_config_files(input_dir, extra_configs);
    let (viewer_config, text_macros, applied_configs) =
        match mathpreview_core::load_and_merge_config(&config_files) {
            Ok((resolved, applied)) => (resolved.viewer, resolved.text_macros, applied),
            Err(e) => {
                eprintln!("mathpreview: config load failed, using defaults — {e:#}");
                let d = mathpreview_core::ResolvedConfig::default();
                (d.viewer, d.text_macros, Vec::new())
            }
        };
    for p in &applied_configs {
        eprintln!("mathpreview: applied config {}", p.display());
    }
    let opts = HtmlOptions {
        title: input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("mathpreview")
            .to_string(),
        engine: Engine::MathJax(MathJaxEngine::new(url)),
        macro_overrides,
        viewer_config,
        text_macros,
        ..HtmlOptions::default()
    };
    (opts, config_files)
}

/// Inputs for opening the native window (the `view` subcommand and the `locus`
/// binary share these).
#[cfg(feature = "gui")]
#[derive(Debug, Default)]
pub struct ViewArgs {
    /// Input file (standalone: starts its own daemon). `None` with `attach`.
    pub input: Option<PathBuf>,
    /// Attach to an existing daemon at this URL (no new daemon).
    pub attach: Option<String>,
    /// Window title / document label. Defaults to the file stem.
    pub title: Option<String>,
    /// Daemon port. `None` picks a free ephemeral port.
    pub port: Option<u16>,
    pub mathjax_url: Option<String>,
    pub editor: String,
    pub macros: Vec<PathBuf>,
    pub config: Vec<PathBuf>,
}

/// Open the native Locus window and run its event loop (blocks until closed).
/// In `attach` mode it just points a window at an existing daemon; otherwise it
/// starts a daemon for `input` on a background thread and shows it.
#[cfg(feature = "gui")]
pub fn run_view(args: ViewArgs) -> Result<()> {
    let ViewArgs {
        input,
        attach,
        title,
        port,
        mathjax_url,
        editor,
        macros,
        config,
    } = args;

    // Attach mode: window only, at an existing daemon (the plugin's path).
    if let Some(url) = attach {
        // `run_window` brands the title as "<doc> — Locus"; empty → just "Locus".
        let doc = title.unwrap_or_default();
        return view::run_window(&url, &doc);
    }

    // No input on macOS (e.g. Locus.app double-clicked from the dock — Finder
    // passes no argv): ask with a native open panel instead of erroring.
    #[cfg(target_os = "macos")]
    let input = input.or_else(view::pick_tex_file);
    let input = input.context("provide an input file, or --attach <url> to a running daemon")?;
    let (opts, config_files) = build_serve_opts(&input, mathjax_url, &macros, &config);
    let doc = title.unwrap_or_else(|| opts.title.clone());
    let port = port.unwrap_or_else(view::free_port);
    // The webview event loop must own the main thread (required on macOS), so
    // run the daemon on a background thread and point the window at it once it
    // is listening. When the window closes, the process (and thread) exit.
    let server_input = input.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("mathpreview: tokio init failed: {e}");
                return;
            }
        };
        if let Err(e) = rt.block_on(serve::run(
            server_input,
            "127.0.0.1".to_string(),
            port,
            opts,
            editor,
            config_files,
        )) {
            eprintln!("mathpreview: server error: {e:#}");
        }
    });
    view::wait_for_listen(port, std::time::Duration::from_secs(10));
    view::run_window(&format!("http://127.0.0.1:{port}/"), &doc)
}
