//! MathJax v4 SVG engine.
//!
//! Picks up the renderer's `<span class="math" data-tex="...">` nodes and
//! typesets them in the browser. Macros and `\usepackage{...}` mappings come
//! from the project preamble extracted in `crates/core/src/macros.rs`.
//!
//! The engine is responsible for:
//!   * Emitting the `window.MathJax = {...}` config block (macros, package
//!     loader, tag mode) before the MathJax script tag.
//!   * Loading the MathJax bundle.
//!   * Providing a `window.__mpEngine` shim that the engine-neutral
//!     `CLIENT_JS` calls into for ready/typesetClear/typeset.
//!   * Engine-specific CSS rules (the `mjx-container` overflow handling that
//!     keeps displays from blowing out the A4 column).

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::engines::MathEngine;
use crate::macros::ExtractedPreamble;

#[derive(Debug, Clone)]
pub struct MathJaxEngine {
    /// URL or relative path the page should load MathJax from. Default points
    /// at the jsdelivr CDN (`mathjax@4/tex-svg.js`) for quick browser checks;
    /// switch to the vendored `/vendor/mathjax/tex-svg.js` for offline use.
    pub script_url: String,
}

impl MathJaxEngine {
    pub fn new(script_url: impl Into<String>) -> Self {
        Self {
            script_url: script_url.into(),
        }
    }
}

impl Default for MathJaxEngine {
    fn default() -> Self {
        Self::new("https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js")
    }
}

impl MathEngine for MathJaxEngine {
    fn name(&self) -> &'static str {
        "mathjax"
    }

    fn head_html(
        &self,
        preamble: &ExtractedPreamble,
        viewer: &crate::config::ResolvedViewerConfig,
    ) -> String {
        let config = mathjax_config(preamble, mathjax_local_font_path(&self.script_url));
        let url = escape_attr(&self.script_url);
        // User-supplied `mathjax-config` JS runs after the generated config and
        // before the (async) library load, so it can mutate `window.MathJax`.
        let extra = if viewer.mathjax_config.trim().is_empty() {
            String::new()
        } else {
            format!("\n<script>\n{}\n</script>", viewer.mathjax_config)
        };
        // The id lets the config dialog show the generated config read-only.
        format!("<script id=\"mp-mathjax-config\">\n{config}\n</script>{extra}\n<script src=\"{url}\" async></script>")
    }

    fn client_adapter_js(&self) -> String {
        ADAPTER_JS.to_string()
    }

    fn extra_css(&self) -> &'static str {
        EXTRA_CSS
    }
}

fn mathjax_config(preamble: &ExtractedPreamble, local_font_path: Option<String>) -> String {
    let mut macros = String::new();
    let mut first = true;
    let mut write_entry =
        |out: &mut String, name: &str, body: &str, n_args: u8, default: Option<&str>| {
            if !first {
                out.push_str(",\n      ");
            }
            first = false;
            let name_json = json_string(name);
            let body_json = json_string(body);
            match (n_args, default) {
                (0, _) => write!(out, "{}: {}", name_json, body_json).unwrap(),
                (n, None) => write!(out, "{}: [{}, {}]", name_json, body_json, n).unwrap(),
                (n, Some(d)) => {
                    let d_json = json_string(d);
                    write!(out, "{}: [{}, {}, {}]", name_json, body_json, n, d_json).unwrap();
                }
            }
        };
    // All macros — built-ins, paper preamble, and user overrides — live in
    // `preamble.macros` in cascade order (see `macros::extract_preamble_with_overrides`).
    // We emit them sequentially; the JSON object's last-key-wins semantics
    // matches the cascade's last-write-wins semantics.
    for m in preamble.macros.iter() {
        write_entry(
            &mut macros,
            &m.name,
            &m.body,
            m.n_args,
            m.default.as_deref(),
        );
    }

    let mut package_short_set = BTreeSet::from(["ams".to_string()]);
    package_short_set.extend(preamble.packages_short.iter().cloned());
    let package_short: Vec<String> = package_short_set.iter().map(|s| json_string(s)).collect();
    let mut package_long_set = BTreeSet::from(["[tex]/ams".to_string()]);
    package_long_set.extend(preamble.packages_long.iter().cloned());
    let package_long: Vec<String> = package_long_set.iter().map(|s| json_string(s)).collect();
    let loader_paths = local_font_path
        .as_deref()
        .map(|path| format!("paths: {{ 'mathjax-newcm': {} }}, ", json_string(path)))
        .unwrap_or_default();
    format!(
        r#"window.MathJax = {{
  tex: {{
    // `noerrors` is always loaded so undefined commands render as their
    // raw LaTeX source in gray text rather than scary red error boxes.
    // Real papers reach for project-specific .sty files we can't always
    // mirror (`\given`, `\st`, `\underaccent`, ...); the page stays
    // readable while the author sees which spots are unresolved.
    packages: {{ '[+]': ['noerrors', {packages_short}] }},
    inlineMath: [['\\(', '\\)']],
    displayMath: [['\\[', '\\]']],
    // Equation numbers are computed in Rust and emitted as <span
    // class="eq-num">. Leaving MathJax's auto-tagging on produces a second
    // number column and, when labels collide or fail, "(???)" placeholders.
    tags: 'none',
    macros: {{
      {macros}
    }}
  }},
  loader: {{ {loader_paths}load: ['[tex]/noerrors', {packages_long}] }},
  svg: {{
    // `local` (not `global`) because our client adapter renders math via
    // MathJax.tex2svgPromise(...) and inserts the returned SVG into the
    // page directly. `global` puts glyph paths in a separate
    // MJX-SVG-global-cache element that is only inserted by MathJax's
    // updateDocument() pipeline — which tex2svgPromise does NOT call.
    // The result is SVGs full of use-href pointers to MJX-NCM-* IDs in
    // a cache that never lands in the DOM, so glyphs render as blank.
    // `local` inlines each math's paths into its own <defs>, making the
    // returned SVG self-contained.
    fontCache: 'local',
    // Match MathJax 4's intended split: the browser may break inline math at
    // TeX-valid operators as prose flows, while display math remains one line.
    // Wide displays get a horizontal scroller instead of inserted math rows.
    displayOverflow: 'scroll',
    linebreaks: {{ inline: true }}
  }},
  // `tex-svg.js` in MathJax 4 bakes in the contextual menu + a11y
  // pipeline (Speech Rule Engine), which fires off `sre/speech-worker.js`
  // on the very first typeset and otherwise blocks the render promise
  // from resolving. Our vendored bundle deliberately ships without the
  // `sre/` directory (saves ~14 MB), so that worker 404s and every
  // `tex2svgPromise` call hangs forever. Turning the `enrich` menu
  // setting off cascades to `speech`, `braille`, `complexity`, and
  // `explorer` (see MathJax 4's `enableEnrichment` wiring) so none of
  // them try to load SRE.
  options: {{
    menuOptions: {{ settings: {{ enrich: false }} }}
  }},
  startup: {{ typeset: false }}
}};"#,
        packages_short = package_short.join(", "),
        packages_long = package_long.join(", "),
        loader_paths = loader_paths,
        macros = macros,
    )
}

fn mathjax_local_font_path(script_url: &str) -> Option<String> {
    if script_url.contains("://") {
        return None;
    }
    let base = script_url.rsplit_once('/').map(|(base, _)| base)?;
    if base.is_empty() {
        Some("mathjax-newcm-font".to_string())
    } else {
        Some(format!("{base}/mathjax-newcm-font"))
    }
}

fn json_string(s: &str) -> String {
    // Conservative JSON string escape — enough for macro names/bodies, which
    // are dominated by backslashes and curly braces.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            c if (c as u32) < 0x20 => {
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

const ADAPTER_JS: &str = include_str!("assets/mathjax.js");
const EXTRA_CSS: &str = include_str!("assets/mathjax.css");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_math_breaks_and_wide_displays_scroll() {
        let preamble = ExtractedPreamble {
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
        };
        let config = mathjax_config(&preamble, None);
        assert!(
            config.contains("displayOverflow: 'scroll'"),
            "wide displays must use MathJax's horizontal scrolling mode"
        );
        assert!(
            config.contains("linebreaks: { inline: true }"),
            "inline math must allow browser-based MathJax line breaking"
        );
        assert!(
            ADAPTER_JS.contains("opts.containerWidth = cw"),
            "standalone conversions need the paragraph width for inline breaks and display scroll"
        );
    }

    #[test]
    fn full_width_displays_are_block_level() {
        // `multline` (and equations MathJax line-breaks to the column) get
        // marked `width="full"` and right-align their last line to the full
        // line width. The base rule shrink-wraps displays to `inline-block`,
        // which clips that last line — so full-width containers must override
        // to block-level. Guard the fix against regressions.
        assert!(
            EXTRA_CSS.contains(r#"mjx-container[display="true"][width="full"]"#),
            "mathjax.css must special-case full-width displays"
        );
        let full_rule = EXTRA_CSS.split(r#"[width="full"]"#).nth(1).unwrap_or("");
        assert!(
            full_rule.contains("display: block"),
            "full-width displays must be block-level, not shrink-wrapped"
        );
    }
}
