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
    let engine_head = engine.head_html(preamble, &opts.viewer_config);
    let engine_adapter_js = engine.client_adapter_js();
    let engine_css = engine.extra_css();
    let warnings_html = warnings_panel(preamble);
    let css = if opts.inline_css { DEFAULT_CSS } else { "" };
    // Config-driven overrides emitted after the bundled CSS so they win
    // by source order, and as a separate `<script>` so client JS can read
    // the values at init without round-tripping through localStorage.
    let config_css = format!(
        ":root {{ --body-font-size: {}px; --ui-font-size: {}px; }}",
        opts.viewer_config.font_size, opts.viewer_config.ui_font_size,
    );
    // The trigger string comes from a finite enum (cmd-click / ctrl-click
    // / alt-click / double-click) with no JSON-special characters, so a
    // plain quote-wrap is sufficient and avoids dragging in an escape
    // helper from another module.
    // `mathjax-config` is arbitrary JS, so JSON-encode it into a valid JS
    // string literal (handles quotes / newlines / braces) for __mpConfig.
    let mathjax_config_js = serde_json::to_string(&opts.viewer_config.mathjax_config)
        .unwrap_or_else(|_| "\"\"".to_string());
    let config_js = format!(
        r#"window.__mpConfig = {{ sourceJumpTrigger: "{trigger}", defaultPageMode: "{page}", defaultTheme: "{theme}", wrapEquations: {wrap}, mathjaxConfig: {mjx} }};"#,
        trigger = opts.viewer_config.source_jump_trigger.as_str(),
        page = opts.viewer_config.default_page_mode.as_str(),
        theme = opts.viewer_config.default_theme.as_str(),
        wrap = opts.viewer_config.wrap_equations,
        mjx = mathjax_config_js,
    );

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
<style>{css}{engine_css}{config_css}</style>
<script>{config_js}</script>
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
    <button class="lineno-toggle" id="lineno-toggle" type="button" aria-pressed="false" title="toggle line numbers">lines</button>
    <button class="macros-toggle" id="macros-toggle" type="button" title="add a \\newcommand override for the viewer">macros</button>
    <button class="config-toggle" id="config-toggle" type="button" title="edit viewer config (font size, source-jump trigger, default mode/theme)">config</button>
    <button class="log-toggle" id="log-toggle" type="button" title="show daemon state + recent log entries">log</button>
    <button class="margin-toggle" id="margin-toggle" type="button" aria-pressed="false" title="toggle margin reference cards (click \\ref / \\cite to pin)">margin</button>
    <button class="theme-toggle" id="theme-toggle" type="button" aria-pressed="false" aria-label="dark mode" title="dark mode"><span class="theme-toggle-icon" aria-hidden="true">☾</span></button>
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
  <span class="search-help">Enter next · Shift+Enter previous · Esc close · prefix <code>m:</code> or <code>$</code> for math-only</span>
</div>
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
{warnings_html}
<aside id="margin">
  <div class="margin-cards" id="margin-cards"></div>
</aside>
<dialog class="macros-dialog" id="macros-dialog">
  <form method="dialog" class="macros-dialog-form" id="macros-dialog-form">
    <h2 class="macros-dialog-title">Add a macro</h2>
    <div class="macros-dialog-body">
      <div class="macros-dialog-sidebar">
        <div class="macros-tabs" role="tablist" aria-orientation="vertical" aria-label="Which file to edit">
          <label class="macros-tab"><input type="radio" name="scope" value="project" checked> Project</label>
          <label class="macros-tab"><input type="radio" name="scope" value="global"> Global</label>
          <label class="macros-tab"><input type="radio" name="scope" value="custom"> Custom</label>
        </div>
      </div>
      <div class="macros-dialog-main">
        <fieldset class="macros-dialog-mode">
          <legend>Type</legend>
          <label><input type="radio" name="macro-mode" value="tex" checked>
            TeX macro (<code>\newcommand</code>)</label>
          <label><input type="radio" name="macro-mode" value="html">
            Text → HTML</label>
        </fieldset>
        <div class="macros-scope-detail">
          <code class="macros-scope-file" id="macros-scope-file">.mathpreview-macros.tex</code>
          <input type="text" id="macros-dialog-custom-path" class="macros-dialog-custom-input"
                 placeholder="~/my-macros.tex or extras/macros.tex" disabled hidden>
        </div>
        <div id="macros-mode-tex">
          <p class="macros-dialog-hint">
            The chosen file's existing <code>\newcommand</code> lines load here for
            editing — <em>Save</em> writes the whole box back (it replaces the
            file, so re-saving won't duplicate). The viewer re-renders immediately.
            Or use <em>Use as override</em> to add a file as a live, watched layer
            without copying its text.
          </p>
          <div class="macros-dialog-load">
            <button type="button" class="macros-dialog-loadbtn" id="macros-dialog-loadbtn">Load file…</button>
            <button type="button" class="macros-dialog-usebtn" id="macros-dialog-usebtn">Use as override</button>
            <input type="file" id="macros-dialog-file" accept=".tex,.sty,text/plain" hidden>
          </div>
          <textarea class="macros-dialog-input" id="macros-dialog-input" rows="10"
                    spellcheck="false" autocomplete="off"
                    placeholder="\newcommand{{\st}}{{\mid}}"></textarea>
        </div>
        <div id="macros-mode-html" hidden>
          <p class="macros-dialog-hint">
            Don't know the syntax? Fill in a command name and an HTML template
            below and click <em>Add</em> — it inserts a correct
            <code>[text-macros]</code> line into the editor. Or edit the TOML
            directly. Use <code>#1</code>, <code>#2</code>… for the command's
            arguments. <em>Save</em> writes the whole file (validated) and
            re-renders.
          </p>
          <div class="macros-html-add">
            <input type="text" id="macros-html-name" class="macros-dialog-custom-input"
                   spellcheck="false" autocomplete="off" placeholder="name — e.g. SV">
            <input type="text" id="macros-html-template" class="macros-dialog-custom-input"
                   spellcheck="false" autocomplete="off"
                   placeholder="&lt;span style=&quot;color:red&quot;&gt;#1&lt;/span&gt;">
            <button type="button" class="macros-dialog-loadbtn" id="macros-html-add-btn">Add ↓</button>
          </div>
          <textarea class="macros-dialog-input" id="macros-toml-input" rows="9"
                    spellcheck="false" autocomplete="off"
                    placeholder="[text-macros]&#10;SV = '&lt;span style=&quot;color:red&quot;&gt;#1&lt;/span&gt;'"></textarea>
        </div>
        <div class="macros-dialog-feedback" id="macros-dialog-feedback" aria-live="polite"></div>
        <div class="macros-dialog-actions">
          <button type="button" class="macros-dialog-cancel" id="macros-dialog-cancel">Cancel</button>
          <button type="submit" class="macros-dialog-save" id="macros-dialog-save">Save</button>
        </div>
      </div>
    </div>
  </form>
</dialog>
<dialog class="macros-dialog config-dialog" id="config-dialog">
  <form method="dialog" class="macros-dialog-form" id="config-dialog-form">
    <h2 class="macros-dialog-title">Viewer config</h2>
    <p class="macros-dialog-hint">
      Saves to a TOML file in the same cascade as the macros dialog.
      Existing formatting and comments are preserved.
    </p>
    <fieldset class="macros-dialog-scope config-fields">
      <legend>Fields</legend>
      <label>Body font size (px)
        <input type="number" id="config-font-size" min="10" max="40" step="1">
      </label>
      <label>UI font size (px)
        <input type="number" id="config-ui-font-size" min="9" max="20" step="1">
      </label>
      <label>Source-jump trigger
        <select id="config-source-jump-trigger">
          <option value="cmd-click">Cmd-click (Ctrl on Linux)</option>
          <option value="ctrl-click">Ctrl-click only</option>
          <option value="alt-click">Alt-click</option>
          <option value="double-click">Double-click</option>
        </select>
      </label>
      <label>Default page mode
        <select id="config-default-page-mode">
          <option value="a4">A4</option>
          <option value="dynamic">Dynamic</option>
        </select>
      </label>
      <label>Default theme
        <select id="config-default-theme">
          <option value="system">System (match OS)</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
      </label>
      <label class="config-checkbox"><input type="checkbox" id="config-wrap-equations">
        Wrap long equations (off = scroll horizontally)</label>
    </fieldset>
    <fieldset class="macros-dialog-scope config-fields">
      <legend>MathJax config (advanced)</legend>
      <details class="config-mjx-current">
        <summary>Current generated config (read-only)</summary>
        <textarea id="config-mathjax-current" class="macros-dialog-input" rows="12"
                  spellcheck="false" readonly></textarea>
      </details>
      <label class="config-textarea-label">
        Your override JS — runs <em>after</em> the config above, before MathJax
        loads. <em>Mutate</em> <code>window.MathJax</code> (don't reassign it).
        <textarea id="config-mathjax-config" class="macros-dialog-input" rows="5"
                  spellcheck="false" autocomplete="off"
                  placeholder="window.MathJax.svg.displayOverflow = 'scroll';"></textarea>
      </label>
    </fieldset>
    <fieldset class="macros-dialog-scope">
      <legend>Save to</legend>
      <label><input type="radio" name="config-scope" value="project" checked>
        Project <code>.mathpreview.toml</code></label>
      <label><input type="radio" name="config-scope" value="global">
        Global <code>~/.config/mathpreview/config.toml</code></label>
      <label><input type="radio" name="config-scope" value="custom">
        Custom path
        <input type="text" id="config-dialog-custom-path" class="macros-dialog-custom-input"
               placeholder="~/my-config.toml or extras/config.toml" disabled></label>
    </fieldset>
    <div class="macros-dialog-feedback" id="config-dialog-feedback" aria-live="polite"></div>
    <div class="macros-dialog-actions">
      <button type="button" class="macros-dialog-cancel" id="config-dialog-cancel">Cancel</button>
      <button type="submit" class="macros-dialog-save" id="config-dialog-save">Save</button>
    </div>
  </form>
</dialog>
<aside class="log-panel" id="log-panel" hidden aria-label="daemon state and log">
  <header class="log-panel-head">
    <h2 class="log-panel-title">Daemon state + log</h2>
    <div class="log-panel-actions">
      <label class="log-panel-verbose" title="stream high-frequency events (buffer-pushes, file changes, watcher)">
        <input type="checkbox" id="log-panel-verbose" />
        <span>verbose</span>
      </label>
      <button type="button" class="log-panel-refresh" id="log-panel-refresh" title="re-fetch /debug">↻</button>
      <button type="button" class="log-panel-close" id="log-panel-close" title="close (or click `log` again)">×</button>
    </div>
  </header>
  <div class="log-panel-state" id="log-panel-state">Loading…</div>
  <pre class="log-panel-entries" id="log-panel-entries"></pre>
</aside>
<div class="cmdline" id="cmdline" hidden>
  <div class="cmdline-suggestions" id="cmdline-suggestions" hidden></div>
  <div class="cmdline-row">
    <span class="cmdline-prompt" aria-hidden="true">:</span>
    <input type="text" class="cmdline-input" id="cmdline-input" autocomplete="off" spellcheck="false" aria-label="command">
    <span class="cmdline-feedback" id="cmdline-feedback" aria-live="polite"></span>
  </div>
</div>
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
