//! Shared library for the mathpreview-cli serve daemon and HTML-render options.

use std::path::{Path, PathBuf};

use mathpreview_core::{Engine, HtmlOptions, MathJaxEngine};

pub mod serve;

/// Default `--editor` template for Cmd/Ctrl-click "reveal source": jump to the
/// source line in a running nvim (uses `$NVIM_LISTEN_ADDRESS`, else `$NVIM`).
pub const DEFAULT_EDITOR: &str = r#"nvim --server "${NVIM_LISTEN_ADDRESS:-$NVIM}" --remote-send "<C-\\><C-N>:e +{line} {file}<CR>""#;

/// Build the serve-mode `HtmlOptions` and resolved config-file list shared by
/// the `serve` daemon and browser viewer.
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
