//! mathpreview-cli — render LaTeX or Markdown documents to self-contained
//! HTML, or serve them with live updates.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use mathpreview_core::{render_document, DocumentFormat, Engine, HtmlOptions, MathJaxEngine};

use mathpreview_cli::{build_serve_opts, serve, static_markdown_asset_base};

#[derive(Parser, Debug)]
#[command(
    name = "mathpreview-cli",
    version,
    about = "Live LaTeX and Markdown preview renderer"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Render a LaTeX or Markdown document as a single HTML preview.
    Render {
        /// Input document. LaTeX project roots are auto-detected.
        input: PathBuf,
        /// Output HTML path. Use `-` for stdout.
        #[arg(short, long, default_value = "-")]
        output: PathBuf,
        /// URL or relative path the page should load MathJax from.
        /// Defaults to the jsdelivr CDN for quick browser checks; switch to a
        /// vendored path for an offline or self-hosted preview.
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
    /// Print resolved document metadata and, for LaTeX, the extracted
    /// preamble (macros + packages) MathJax will see.
    Debug { input: PathBuf },
    /// Serve a live-reloading preview over HTTP + WebSocket. Re-renders on
    /// document change; pushes updated `#page` HTML to every
    /// connected browser tab.
    Serve {
        /// Input document. LaTeX project roots are auto-detected.
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
        #[arg(
            long,
            default_value = r#"nvim --server "${NVIM_LISTEN_ADDRESS:-$NVIM}" --remote-send "<C-\\><C-N>:e +{line} {file}<CR>""#
        )]
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
                local_asset_base: static_markdown_asset_base(&input)?,
                ..HtmlOptions::default()
            };
            if let Some(url) = mathjax_url {
                opts.engine = Engine::MathJax(MathJaxEngine::new(url));
            }
            let result = render_document(&input, &opts)
                .with_context(|| format!("rendering {}", input.display()))?;

            if output.as_os_str() == "-" {
                io::stdout().write_all(result.html.as_bytes())?;
            } else {
                fs::write(&output, &result.html)
                    .with_context(|| format!("writing {}", output.display()))?;
                match result.format {
                    DocumentFormat::Latex => eprintln!(
                        "rendered {} ({} macros, {} packages) → {}",
                        result.root_file.display(),
                        result.preamble.macros.len(),
                        result.preamble.packages_long.len(),
                        output.display(),
                    ),
                    DocumentFormat::Markdown => eprintln!(
                        "rendered Markdown {} → {}",
                        result.root_file.display(),
                        output.display(),
                    ),
                }
            }
        }
        Cmd::Debug { input } => {
            let opts = HtmlOptions::default();
            let result = render_document(&input, &opts)
                .with_context(|| format!("rendering {}", input.display()))?;
            println!("# document format");
            println!("{}", result.format.as_str());
            println!();
            println!("# root file");
            println!("{}", result.root_file.display());
            println!();
            println!("# included files");
            for f in &result.included_files {
                println!("  {}", f.display());
            }
            if result.format == DocumentFormat::Latex {
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
            }
            println!("\n# MathJax macros ({})", result.preamble.macros.len());
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
