//! mathpreview-cli — Step 1 surface: render a `.tex` project to a single
//! self-contained HTML file.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use mathpreview_core::{render_project, Engine, HtmlOptions, MathJaxEngine};

mod serve;

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
        /// Host to bind. Use 127.0.0.1 by default; 0.0.0.0 for LAN preview.
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
        /// the source line inside a running nvim instance that listens on
        /// the socket in `$NVIM_LISTEN_ADDRESS` (set automatically when
        /// you launch nvim with `--listen` or inside a Neovim terminal).
        #[arg(long, default_value = r#"nvim --server "$NVIM_LISTEN_ADDRESS" --remote-send "<C-\\><C-N>:e +{line} {file}<CR>""#)]
        editor: String,
        /// Extra macro override file(s), appended to the cascade after
        /// the global `~/.config/mathpreview/macros.tex` and the
        /// project-local `.mathpreview-macros.tex`. Repeat the flag to
        /// stack overrides; later flags win on name collision.
        #[arg(long = "macros", value_name = "FILE")]
        macros: Vec<PathBuf>,
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
        } => {
            // serve-mode default: use the vendored bundle so the page works offline.
            // Render-mode keeps the CDN default since its output is a standalone file.
            let url = mathjax_url.unwrap_or_else(|| "/vendor/mathjax/tex-svg.js".to_string());
            // Resolve the macro-override cascade once at startup. The
            // discovery walks upward from the input file looking for a
            // project-local `.mathpreview-macros.tex`, then prepends the
            // global file (if it exists) and appends any `--macros`
            // flags. Result is in cascade order (lowest → highest
            // priority); HtmlOptions hands it to the extractor on every
            // render.
            let macro_overrides = mathpreview_core::discover_macro_overrides(
                input.parent().unwrap_or_else(|| std::path::Path::new(".")),
                &extra_macros,
            );
            let opts = HtmlOptions {
                title: input
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("mathpreview")
                    .to_string(),
                engine: Engine::MathJax(MathJaxEngine::new(url)),
                macro_overrides,
                ..HtmlOptions::default()
            };
            if let Ok(ms) = std::env::var("MATHPREVIEW_RESTART_DELAY_MS") {
                if let Ok(ms) = ms.parse::<u64>() {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
            }
            let rt = tokio::runtime::Runtime::new()?;
            return rt.block_on(serve::run(input, host, port, opts, editor));
        }
        Cmd::Render {
            input,
            output,
            mathjax_url,
            title,
            macros: extra_macros,
        } => {
            let title = title.unwrap_or_else(|| {
                input
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("mathpreview")
                    .to_string()
            });
            let macro_overrides = mathpreview_core::discover_macro_overrides(
                input.parent().unwrap_or_else(|| std::path::Path::new(".")),
                &extra_macros,
            );
            let mut opts = HtmlOptions {
                title,
                macro_overrides,
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
