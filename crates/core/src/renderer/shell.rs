//! HTML document shell: the static `<head>` + topbar + side panel scaffolding
//! that wraps the rendered body, plus the warnings panel below the topbar and
//! the bundled CSS / client JS assets.

use std::fmt::Write;

use crate::macros::ExtractedPreamble;

use super::util::{escape_attr, escape_html, shorten_home_path};
use super::HtmlOptions;

/// Client-side script. Wires up:
///   * Event-delegated proof-toggle and proof-head click handlers (so they
///     keep working after `#page` content is swapped by the WebSocket update).
///   * A WebSocket connection to the same host that pushes `body-updated`
///     events with new `#page` HTML. After swapping, the active engine
///     re-typesets via `window.__mpEngine`.
///
/// Math-engine calls go through the `window.__mpEngine` shim injected by
/// [`crate::engines::MathEngine::client_adapter_js`] so this bundle stays
/// engine-neutral.
///
/// When the page is loaded statically (CLI `render` output, no server), the
/// WebSocket fails silently and the page works as a static document.
///
/// Assembled from `assets/client/{header,viewer,proof,patch,footer}.js`. The
/// pieces share scope because they sit inside one outer `(function() { ...
/// })()` IIFE: `header.js` opens it (with all the closure-shared `var`
/// declarations) and `footer.js` closes it. Splits are line-aligned, not
/// re-wrapped, so the concatenation is byte-equivalent to the single
/// `client.js` file it replaces.
pub(super) const CLIENT_JS: &str = concat!(
    include_str!("../assets/client/header.js"),
    include_str!("../assets/client/viewer.js"),
    include_str!("../assets/client/proof.js"),
    include_str!("../assets/client/patch.js"),
    include_str!("../assets/client/footer.js"),
);

pub(super) const DEFAULT_CSS: &str = include_str!("../assets/default.css");

pub(super) fn wrap_in_shell(
    body: &str,
    preamble: &ExtractedPreamble,
    opts: &HtmlOptions,
) -> String {
    let engine = opts.engine.as_dyn();
    let engine_head = engine.head_html(preamble);
    let engine_adapter_js = engine.client_adapter_js();
    let engine_css = engine.extra_css();
    let warnings_html = warnings_panel(preamble);
    let css = if opts.inline_css { DEFAULT_CSS } else { "" };

    let mut out = String::new();
    // The topbar's bold short-title slot is filled from the optional
    // argument of `\title[short]{long}`. We reuse the same value for the
    // browser tab `<title>` when it's present (so the OS task switcher
    // and tab strip surface the human-chosen short name); falling back
    // to `opts.title` (the file stem) so the tab is never blank.
    let topbar_short = preamble
        .title_short
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let head_title = escape_html(topbar_short.unwrap_or(&opts.title));
    let topbar_title_html = match topbar_short {
        Some(s) => format!(
            r#"<strong class="topbar-doc-title">{s}</strong>"#,
            s = escape_html(s),
        ),
        None => String::new(),
    };
    let path_html = match opts.source_path.as_ref() {
        Some(p) => {
            let full = p.display().to_string();
            let short = shorten_home_path(p);
            format!(
                r#"<span class="topbar-doc-path" title="{full}">{short}</span>"#,
                full = escape_attr(&full),
                short = escape_html(&short),
            )
        }
        None => String::new(),
    };
    write!(
        out,
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{head_title}</title>
<style>{css}{engine_css}</style>
{engine_head}
</head>
<body class="page-mode-a4">
<header class="topbar">
  <!-- Row 1: identity (doc title + source path) on the left, live-reload
       status pill on the right. Keeps the most-glanced-at info clean and
       single-line even when the action row below wraps. -->
  <div class="topbar-row topbar-row-info">
    <div class="topbar-doc">
      {topbar_title_html}
      {path_html}
    </div>
    <span class="status" id="ws-status" title="live-reload status"></span>
  </div>
  <!-- Row 2: view/proof toggles, then actions (print/restart/stop) pushed
       to the right with margin-left:auto. On narrow widths the row wraps;
       each pair stays grouped because segmented controls share a wrapper. -->
  <div class="topbar-row topbar-row-actions">
    <span class="page-mode-toggle" data-page-mode="a4">
      <button data-page-mode="a4" class="active" type="button">A4</button>
      <button data-page-mode="dynamic" type="button">dynamic</button>
    </span>
    <button class="refkey-toggle" id="refkey-toggle" type="button" aria-pressed="false" title="toggle LaTeX refkeys">keys</button>
    <button class="margin-toggle" id="margin-toggle" type="button" aria-pressed="false" title="toggle margin reference cards (click \\ref / \\cite to pin)">margin</button>
    <span class="proof-toggle" data-mode="all">
      <button data-mode="main">main only</button>
      <button data-mode="supporting">+ supporting</button>
      <button data-mode="all" class="active">all</button>
    </span>
    <span class="topbar-actions-spacer"></span>
    <button class="print-button" id="print-button" type="button" title="compile and open the PDF">print</button>
    <button class="server-restart" id="server-restart" type="button" title="restart preview server">restart</button>
    <button class="server-stop" id="server-stop" type="button" title="stop preview server">stop</button>
  </div>
  <!-- The topbar hide/show toggle lives as a thin stripe on the left edge
       of the viewport (see #topbar-stripe below) so it stays reachable
       when the margin column covers the right side of the screen.
       The `toc` toggle is a fixed pill on the left edge (`#side-toggle`)
       so it's reachable independent of the top-banner visibility. -->
</header>
<button class="topbar-stripe" id="topbar-stripe" type="button" aria-expanded="true" aria-controls="topbar-banner" title="toggle top banner"></button>
<button class="side-toggle" id="side-toggle" type="button" aria-controls="viewer-side" aria-expanded="false" title="toggle index and pages pane">toc</button>
<div class="search-panel" id="search-panel" hidden>
  <label for="search-input">/</label>
  <input id="search-input" type="search" autocomplete="off" spellcheck="false" placeholder="search">
  <span class="search-help">Enter next · Shift+Enter previous · Esc close</span>
</div>
{warnings_html}
<aside class="side-panel" id="viewer-side" aria-label="document navigation">
  <div class="side-tabs" role="tablist" aria-label="navigation mode">
    <button class="side-tab active" type="button" data-side-tab="index" role="tab" aria-selected="true">Index</button>
    <button class="side-tab" type="button" data-side-tab="pages" role="tab" aria-selected="false">Pages</button>
  </div>
  <nav class="side-list" id="side-index" aria-label="document index"></nav>
  <nav class="side-list" id="side-pages" aria-label="A4 pages" hidden></nav>
</aside>
<div id="page-shell">
  <main id="page" data-proof-mode="all" data-refkeys="hidden">
{body}
  </main>
</div>
<aside id="margin">
  <div class="margin-toolbar">
    <input type="text" class="margin-pin-input" id="margin-pin-input" placeholder="type a \label key, Enter to pin" autocomplete="off" spellcheck="false" aria-label="pin a reference by typing its \label key">
    <span class="margin-pin-feedback" id="margin-pin-feedback" aria-live="polite"></span>
  </div>
  <div class="margin-cards" id="margin-cards"></div>
</aside>
<script>
{engine_adapter_js}
{client_js}
</script>
</body>
</html>
"#,
        client_js = CLIENT_JS,
    )
    .unwrap();
    out
}

fn warnings_panel(preamble: &ExtractedPreamble) -> String {
    if preamble.warnings.is_empty() && preamble.unmapped_packages.is_empty() {
        return String::new();
    }
    let mut html = String::from(r#"<details class="warnings"><summary>"#);
    let n = preamble.warnings.len();
    let u = preamble.unmapped_packages.len();
    write!(
        html,
        "{} macro warning{}, {} unmapped package{}",
        n,
        if n == 1 { "" } else { "s" },
        u,
        if u == 1 { "" } else { "s" },
    )
    .unwrap();
    html.push_str("</summary><ul>");
    for w in &preamble.warnings {
        write!(html, "<li>{}</li>", escape_html(w)).unwrap();
    }
    if !preamble.unmapped_packages.is_empty() {
        write!(
            html,
            "<li>unmapped: {}</li>",
            escape_html(&preamble.unmapped_packages.join(", "))
        )
        .unwrap();
    }
    html.push_str("</ul></details>");
    html
}
