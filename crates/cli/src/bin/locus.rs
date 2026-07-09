//! `locus` — the native-window LaTeX viewer, as its own command. `locus
//! <file>` opens the preview in a dedicated window (the same thing as
//! `mathpreview-cli view <file>`, just a shorter name). Only built with the
//! `gui` cargo feature.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "locus",
    version,
    about = "Locus — a native-window live LaTeX preview"
)]
struct Cli {
    /// Input .tex file (starts its own daemon). Omit when using --attach.
    input: Option<PathBuf>,
    /// Attach to an already-running mathpreview daemon at this URL and just
    /// open the window — no new daemon. Mutually exclusive with <input>.
    #[arg(long, value_name = "URL", conflicts_with = "input")]
    attach: Option<String>,
    /// Window title. Defaults to the file name.
    #[arg(long)]
    title: Option<String>,
    /// Daemon port. Defaults to a free ephemeral port.
    #[arg(long)]
    port: Option<u16>,
    /// URL or path for MathJax (defaults to the vendored bundle).
    #[arg(long)]
    mathjax_url: Option<String>,
    /// Shell command for Cmd/Ctrl-click "reveal source".
    #[arg(long, default_value = mathpreview_cli::DEFAULT_EDITOR)]
    editor: String,
    /// Extra macro override file(s), same cascade as the daemon.
    #[arg(long = "macros", value_name = "FILE")]
    macros: Vec<PathBuf>,
    /// Extra TOML config file(s), same cascade as the daemon.
    #[arg(long = "config", value_name = "FILE")]
    config: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    mathpreview_cli::run_view(mathpreview_cli::ViewArgs {
        input: cli.input,
        attach: cli.attach,
        title: cli.title,
        port: cli.port,
        mathjax_url: cli.mathjax_url,
        editor: cli.editor,
        macros: cli.macros,
        config: cli.config,
    })
}
