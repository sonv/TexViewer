//! Shared library for the mathpreview-cli serve daemon and HTML-render options.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mathpreview_core::{Engine, HtmlOptions, MathJaxEngine};

pub mod convert;
pub mod serve;

/// Default `--editor` template for Cmd/Ctrl-click "reveal source": jump to the
/// source line in a running nvim (uses `$NVIM_LISTEN_ADDRESS`, else `$NVIM`).
pub const DEFAULT_EDITOR: &str = r#"nvim --server "${NVIM_LISTEN_ADDRESS:-$NVIM}" --remote-send "<C-\\><C-N>:e +{line} {file}<CR>""#;

/// Resolve the image URL base used by a one-shot Markdown render. This is
/// intentionally independent of the output path: HTML written elsewhere (or
/// redirected from stdout) still resolves local images beside the source.
pub fn static_markdown_asset_base(input: &Path) -> Result<Option<String>> {
    if mathpreview_core::DocumentFormat::from_path(input)
        != Some(mathpreview_core::DocumentFormat::Markdown)
    {
        return Ok(None);
    }
    let canonical = std::fs::canonicalize(input)
        .with_context(|| format!("resolving Markdown source {}", input.display()))?;
    let directory = canonical
        .parent()
        .context("Markdown source has no containing directory")?;
    mathpreview_core::renderer::file_url_base_for_directory(directory)
        .map(Some)
        .context("Markdown source directory is not representable as a file URL")
}

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
    let (viewer_config, text_macros, markdown_config, applied_configs) =
        match mathpreview_core::load_and_merge_config(&config_files) {
            Ok((resolved, applied)) => (
                resolved.viewer,
                resolved.text_macros,
                resolved.markdown,
                applied,
            ),
            Err(e) => {
                eprintln!("mathpreview: config load failed, using defaults — {e:#}");
                let d = mathpreview_core::ResolvedConfig::default();
                (d.viewer, d.text_macros, d.markdown, Vec::new())
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
        tikz_asset_base: Some("/tikz/".to_string()),
        local_asset_base: Some("/assets/".to_string()),
        text_macros,
        markdown_config,
        ..HtmlOptions::default()
    };
    (opts, config_files)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn static_markdown_base_is_source_rooted_for_file_and_stdout_output() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mathpreview notes 100%20real λ-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("notes.md");
        fs::write(&input, "![figure](fig.png)\n").unwrap();

        let base = static_markdown_asset_base(&input).unwrap().unwrap();
        assert!(base.starts_with("file:"));
        assert!(base.contains("mathpreview%20notes%20100%2520real%20%CE%BB-"));
        assert!(base.ends_with('/'));
        // Output destination is deliberately not an input to the helper, so
        // `-o elsewhere/out.html` and stdout redirection behave identically.
        assert_eq!(static_markdown_asset_base(&input).unwrap(), Some(base));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn static_asset_base_is_not_set_for_latex() {
        assert_eq!(
            static_markdown_asset_base(Path::new("missing.tex")).unwrap(),
            None
        );
    }

    #[test]
    fn serve_options_include_resolved_markdown_block_formats() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mathpreview-block-config-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("notes.md");
        let config = dir.join("blocks.toml");
        fs::write(&input, ":::exercise\nBody.\n:::\n").unwrap();
        fs::write(
            &config,
            concat!(
                "[markdown.blocks.exercise]\n",
                "label = \"Try this\"\n",
                "appearance = \"card\"\n",
                "reveal = \"blur\"\n",
                "accent = \"#8a5cd0\"\n",
            ),
        )
        .unwrap();

        let (opts, _) = build_serve_opts(&input, None, &[], std::slice::from_ref(&config));
        let exercise = opts
            .markdown_config
            .blocks
            .get("exercise")
            .expect("explicit Markdown block config");
        assert_eq!(exercise.label, "Try this");
        assert_eq!(exercise.appearance.as_str(), "card");
        assert_eq!(exercise.reveal.as_str(), "blur");
        assert_eq!(exercise.accent.as_deref(), Some("#8a5cd0"));

        fs::remove_dir_all(&dir).unwrap();
    }
}
