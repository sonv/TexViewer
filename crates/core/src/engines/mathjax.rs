//! MathJax v3 SVG engine.
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

use std::fmt::Write;

use crate::engines::MathEngine;
use crate::macros::ExtractedPreamble;

#[derive(Debug, Clone)]
pub struct MathJaxEngine {
    /// URL or relative path the page should load MathJax from. Default points
    /// at the jsdelivr CDN for quick browser checks; switch to a vendored
    /// copy (e.g. `mathjax/es5/tex-svg.js`) for offline distribution.
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
        Self::new("https://cdn.jsdelivr.net/npm/mathjax@3/es5/tex-svg.js")
    }
}

impl MathEngine for MathJaxEngine {
    fn name(&self) -> &'static str {
        "mathjax"
    }

    fn head_html(&self, preamble: &ExtractedPreamble) -> String {
        let config = mathjax_config(preamble);
        let url = escape_attr(&self.script_url);
        format!("<script>\n{config}\n</script>\n<script src=\"{url}\" async></script>")
    }

    fn client_adapter_js(&self) -> String {
        ADAPTER_JS.to_string()
    }

    fn extra_css(&self) -> &'static str {
        EXTRA_CSS
    }
}

fn mathjax_config(preamble: &ExtractedPreamble) -> String {
    let mut macros = String::new();
    for (i, m) in preamble.macros.iter().enumerate() {
        if i > 0 {
            macros.push_str(",\n      ");
        }
        let name_json = json_string(&m.name);
        let body_json = json_string(&m.body);
        match (m.n_args, &m.default) {
            (0, _) => write!(macros, "{}: {}", name_json, body_json).unwrap(),
            (n, None) => write!(macros, "{}: [{}, {}]", name_json, body_json, n).unwrap(),
            (n, Some(d)) => {
                let d_json = json_string(d);
                write!(macros, "{}: [{}, {}, {}]", name_json, body_json, n, d_json).unwrap();
            }
        }
    }

    let package_short: Vec<String> = preamble
        .packages_short
        .iter()
        .map(|s| json_string(s))
        .collect();
    let package_long: Vec<String> = preamble
        .packages_long
        .iter()
        .map(|s| json_string(s))
        .collect();

    format!(
        r#"window.MathJax = {{
  tex: {{
    packages: {{ '[+]': [{packages_short}] }},
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
  loader: {{ load: [{packages_long}] }},
  svg: {{ fontCache: 'global' }},
  startup: {{ typeset: true }}
}};"#,
        packages_short = package_short.join(", "),
        packages_long = package_long.join(", "),
        macros = macros,
    )
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
