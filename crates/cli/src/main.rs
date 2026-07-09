//! mathpreview-cli — Step 1 surface: render a `.tex` project to a single
//! self-contained HTML file.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use mathpreview_core::{render_project, Engine, HtmlOptions, MathJaxEngine};

mod serve;
#[cfg(feature = "gui")]
mod view;

#[derive(Parser, Debug)]
#[command(name = "mathpreview-cli", version, about = "LaTeX preview renderer")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Resolve the project root for any file and emit a single HTML preview.
    Render {
        /// Input file. Project root is auto-detected.
        input: PathBuf,
        /// Output HTML path. Use `-` for stdout.
        #[arg(short, long, default_value = "-")]
        output: PathBuf,
        /// URL or relative path the page should load MathJax from.
        /// Defaults to the jsdelivr CDN for quick browser checks; switch to a
        /// vendored path when integrating into the Tauri app.
        #[arg(long)]
        mathjax_url: Option<String>,
        /// Document <title>. Defaults to the input file's stem.
        #[arg(long)]
        title: Option<String>,
        /// Extra macro override file(s), appended to the cascade after
        /// the global `~/.config/mathpreview/macros.tex` and the
        /// project-local `.mathpreview-macros.tex`.
        #[arg(long = "macros", value_name = "FILE")]
        macros: Vec<PathBuf>,
        /// Extra TOML config file(s), appended to the cascade after the
        /// global `~/.config/mathpreview/config.toml` and the project-
        /// local `.mathpreview.toml`.
        #[arg(long = "config", value_name = "FILE")]
        config: Vec<PathBuf>,
    },
    /// Print the extracted preamble (macros + packages) MathJax will see —
    /// mirrors `latex-preview.nvim`'s debug command.
    Debug { input: PathBuf },
    /// Serve a live-reloading preview over HTTP + WebSocket. Re-renders on
    /// any change to project files; pushes updated `#page` HTML to every
    /// connected browser tab.
    Serve {
        /// Input file. Project root is auto-detected.
        input: PathBuf,
        /// Host to bind. Defaults to loopback (127.0.0.1). A non-loopback host
        /// such as 0.0.0.0 exposes the unauthenticated control endpoints (file
        /// writes, editor spawn, /stop, /print) to the network and disables the
        /// Host-header guard — only use it on a trusted network.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port. Default mirrors tinymist's convention.
        #[arg(long, default_value_t = 23636)]
        port: u16,
        /// URL or path for MathJax. Same flag as `render`.
        #[arg(long)]
        mathjax_url: Option<String>,
        /// Shell command run for Cmd/Ctrl-click "reveal source" requests.
        /// `{file}` (shell-quoted), `{line}`, and `{col}` are substituted
        /// before the command is handed to `sh -c`. The default jumps to
        /// the source line inside a running nvim instance: it uses
        /// `$NVIM_LISTEN_ADDRESS` if set, else `$NVIM` (which Neovim exports
        /// to `:terminal` children). The bundled nvim plugin passes an
        /// explicit `--editor` built from `v:servername`, so this default
        /// only matters when you run `serve` by hand.
        #[arg(long, default_value = r#"nvim --server "${NVIM_LISTEN_ADDRESS:-$NVIM}" --remote-send "<C-\\><C-N>:e +{line} {file}<CR>""#)]
        editor: String,
        /// Extra macro override file(s), appended to the cascade after
        /// the global `~/.config/mathpreview/macros.tex` and the
        /// project-local `.mathpreview-macros.tex`. Repeat the flag to
        /// stack overrides; later flags win on name collision.
        #[arg(long = "macros", value_name = "FILE")]
        macros: Vec<PathBuf>,
        /// Extra TOML config file(s), appended to the cascade after the
        /// global `~/.config/mathpreview/config.toml` and the project-
        /// local `.mathpreview.toml`. Later files win per field.
        #[arg(long = "config", value_name = "FILE")]
        config: Vec<PathBuf>,
    },
    /// Open the live preview in a native window (WebKit/WebView2/WebKitGTK)
    /// instead of a browser tab. Starts the same daemon internally, so it works
    /// standalone (`mathpreview-cli view paper.tex`). Requires the `gui`
    /// feature at build time.
    #[cfg(feature = "gui")]
    View {
        /// Input file. Project root is auto-detected.
        input: PathBuf,
        /// Port for the internal daemon. Defaults to a free ephemeral port so
        /// it never clashes with an nvim-managed daemon.
        #[arg(long)]
        port: Option<u16>,
        /// URL or path for MathJax. Same flag as `serve`.
        #[arg(long)]
        mathjax_url: Option<String>,
        /// Shell command for Cmd/Ctrl-click "reveal source" (same as `serve`).
        #[arg(long, default_value = r#"nvim --server "${NVIM_LISTEN_ADDRESS:-$NVIM}" --remote-send "<C-\\><C-N>:e +{line} {file}<CR>""#)]
        editor: String,
        /// Extra macro override file(s), same cascade as `serve`.
        #[arg(long = "macros", value_name = "FILE")]
        macros: Vec<PathBuf>,
        /// Extra TOML config file(s), same cascade as `serve`.
        #[arg(long = "config", value_name = "FILE")]
        config: Vec<PathBuf>,
    },
}

/// Build the serve-mode `HtmlOptions` and resolved config-file list shared by
/// the `serve` and `view` subcommands.
fn build_serve_opts(
    input: &std::path::Path,
    mathjax_url: Option<String>,
    extra_macros: &[PathBuf],
    extra_configs: &[PathBuf],
) -> (HtmlOptions, Vec<PathBuf>) {
    // serve-mode default: use the vendored bundle so the page works offline.
    let url = mathjax_url.unwrap_or_else(|| "/vendor/mathjax/tex-svg.js".to_string());
    let input_dir = input.parent().unwrap_or_else(|| std::path::Path::new("."));
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Serve {
            input,
            host,
            port,
            mathjax_url,
            editor,
            macros: extra_macros,
            config: extra_configs,
        } => {
            let (opts, config_files) =
                build_serve_opts(&input, mathjax_url, &extra_macros, &extra_configs);
            if let Ok(ms) = std::env::var("MATHPREVIEW_RESTART_DELAY_MS") {
                if let Ok(ms) = ms.parse::<u64>() {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
            }
            let rt = tokio::runtime::Runtime::new()?;
            return rt.block_on(serve::run(input, host, port, opts, editor, config_files));
        }
        #[cfg(feature = "gui")]
        Cmd::View {
            input,
            port,
            mathjax_url,
            editor,
            macros: extra_macros,
            config: extra_configs,
        } => {
            let (opts, config_files) =
                build_serve_opts(&input, mathjax_url, &extra_macros, &extra_configs);
            let title = opts.title.clone();
            let port = port.unwrap_or_else(view::free_port);
            // The webview event loop must own the main thread (required on
            // macOS), so run the daemon on a background thread and point the
            // window at it once it's listening. When the window closes, main
            // returns and the process (and its server thread) exit.
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
            return view::run_window(&format!("http://127.0.0.1:{port}/"), &title);
        }
        Cmd::Render {
            input,
            output,
            mathjax_url,
            title,
            macros: extra_macros,
            config: extra_configs,
        } => {
            let title = title.unwrap_or_else(|| {
                input
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("mathpreview")
                    .to_string()
            });
            let input_dir = input.parent().unwrap_or_else(|| std::path::Path::new("."));
            let macro_overrides =
                mathpreview_core::discover_macro_overrides(input_dir, &extra_macros);
            let viewer_config = mathpreview_core::load_and_merge_config(
                &mathpreview_core::discover_config_files(input_dir, &extra_configs),
            )
            .map(|(r, _)| r.viewer)
            .unwrap_or_else(|_| mathpreview_core::ResolvedConfig::default().viewer);
            let mut opts = HtmlOptions {
                title,
                macro_overrides,
                viewer_config,
                ..HtmlOptions::default()
            };
            if let Some(url) = mathjax_url {
                opts.engine = Engine::MathJax(MathJaxEngine::new(url));
            }
            let result = render_project(&input, &opts)
                .with_context(|| format!("rendering {}", input.display()))?;

            if output.as_os_str() == "-" {
                io::stdout().write_all(result.html.as_bytes())?;
            } else {
                fs::write(&output, &result.html)
                    .with_context(|| format!("writing {}", output.display()))?;
                eprintln!(
                    "rendered {} ({} macros, {} packages) → {}",
                    result.root_file.display(),
                    result.preamble.macros.len(),
                    result.preamble.packages_long.len(),
                    output.display(),
                );
            }
        }
        Cmd::Debug { input } => {
            let opts = HtmlOptions::default();
            let result = render_project(&input, &opts)
                .with_context(|| format!("rendering {}", input.display()))?;
            println!("# root file");
            println!("{}", result.root_file.display());
            println!();
            println!("# included files");
            for f in &result.included_files {
                println!("  {}", f.display());
            }
            println!();
            println!("# packages → MathJax extensions");
            for (name, ext) in result
                .preamble
                .packages_short
                .iter()
                .zip(result.preamble.packages_long.iter())
            {
                println!("  {name}  →  {ext}");
            }
            if !result.preamble.unmapped_packages.is_empty() {
                println!("\n# unmapped packages (ignored by MathJax)");
                for p in &result.preamble.unmapped_packages {
                    println!("  {p}");
                }
            }
            println!("\n# macros ({})", result.preamble.macros.len());
            for m in &result.preamble.macros {
                if let Some(d) = &m.default {
                    println!(
                        "  \\{:<20} args={} default={:?} body={}",
                        m.name, m.n_args, d, m.body
                    );
                } else {
                    println!("  \\{:<20} args={} body={}", m.name, m.n_args, m.body);
                }
            }
            if !result.preamble.warnings.is_empty() {
                println!("\n# warnings");
                for w in &result.preamble.warnings {
                    println!("  {w}");
                }
            }
        }
    }
    Ok(())
}
