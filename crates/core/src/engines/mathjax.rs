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

/// Best-effort stubs for macros that real papers reach for but which the
/// preamble extractor cannot translate — typically because the author's
/// definition delegates to an `@`-internal helper (e.g. `\given` →
/// `\SV@given{\delimsize}` from `svmacro.sty`) that uses TeX primitives
/// MathJax cannot expand. Without a stub, every equation using these
/// commands fails to render. With these defaults, the math is readable;
/// authors who declare their own `\newcommand{\given}{...}` cleanly in
/// the preamble override these because user macros are emitted later in
/// the JSON object (last-key-wins).
/// Fallback macro entry: (name, body, total_args, optional_default).
/// When `optional_default` is `Some(default)`, the FIRST of `total_args`
/// is the optional `[...]` argument; the remaining `total_args - 1` are
/// required `{...}` arguments. Mirrors MathJax's `tex.macros` shape.
type FallbackMacro = (&'static str, &'static str, u8, Option<&'static str>);

const FALLBACK_MACROS: &[FallbackMacro] = &[
    // `\given` and `\st` from `svmacro.sty` style packages — typically
    // used as the spaced pipe in `P(A \given B)` / `\{x \st x > 0\}`.
    ("given", r"\,|\,", 0, None),
    ("st", r"\,|\,", 0, None),
    // `\bm` from the `bm` package. MathJax's `[tex]/boldsymbol` extension
    // provides `\boldsymbol` but does NOT alias it to `\bm`, so author
    // macros like `\newcommand{\E}{\bm{E}}` (from svmacro.sty) break
    // every equation they touch. Alias here.
    ("bm", r"\boldsymbol{#1}", 1, None),
    // `\underaccent` from the `accents` package: no MathJax extension,
    // but `\underset` is the nearest pure-MathJax fit.
    ("underaccent", r"\underset{#1}{#2}", 2, None),
    // `\xspace` is a no-op in math contexts.
    ("xspace", r"", 0, None),
    // `\defeq` (svmacro / mathtools-style) — render as a stylized `:=`.
    (
        "defeq",
        r"\stackrel{\scriptscriptstyle\mathrm{def}}{=}",
        0,
        None,
    ),
    // Sidenote / annotation commands authors use for review comments:
    //   `\sidenote[opts]{text}` from a typical svmacro.sty / tcolorbox
    //   bridge. The wrapping `\SV{n}{text}` / `\AB{n}{text}` macros (also
    //   from svmacro.sty) extract fine but their bodies call `\sidenote`,
    //   so stubbing `\sidenote` to render as empty makes the whole
    //   annotation invisible in the viewer — the right call for live
    //   preview where author-private review notes are noise.
    ("sidenote", r"", 2, Some("")),
    // Edit-tracking commands from marktext-style packages: render only
    // the post-edit text (`\add` shows what was added; `\replace{a}{b}`
    // shows `b`; `\remove{...}` hides the deletion entirely).
    ("add", r"#1", 1, None),
    ("remove", r"", 1, None),
    ("highlight", r"#1", 1, None),
    ("replace", r"#2", 2, None),
];

fn mathjax_config(preamble: &ExtractedPreamble) -> String {
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
    // Built-in fallbacks first; user-extracted macros below override on
    // matching keys (JS object literals: last key wins).
    for (name, body, n_args, default) in FALLBACK_MACROS {
        write_entry(&mut macros, name, body, *n_args, *default);
    }
    for m in preamble.macros.iter() {
        write_entry(
            &mut macros,
            &m.name,
            &m.body,
            m.n_args,
            m.default.as_deref(),
        );
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
  loader: {{ load: ['[tex]/noerrors', {packages_long}] }},
  svg: {{
    fontCache: 'global',
    // Auto-break long display equations at low-priority operators when the
    // container is narrower than the rendered math. With MathJax 4's
    // improved linebreak heuristics this also handles inline math that
    // overflows its paragraph width — useful for narrow margin previews
    // and the sidenote chips.
    displayOverflow: 'linebreak',
    linebreaks: {{ inline: true }}
  }},
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
