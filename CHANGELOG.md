# Changelog

All notable changes to mathpreview are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
the project versions itself with [Semantic Versioning](https://semver.org/).
Pre-1.0 means the public surface (CLI flags, plugin commands, HTTP routes,
WebSocket protocol) may still shift between minor versions; breakages will
be called out under **Changed** / **Removed** when they happen.

Per-session implementation notes — what was tried, what failed, what got
reverted — live in [`CHANGELOG-claude.md`](./CHANGELOG-claude.md) and
[`CHANGELOG-GPT.md`](./CHANGELOG-GPT.md). This file is the user-facing
summary.

## [Unreleased]

Nothing yet.

## [0.1.18] — 2026-06-02

### Fixed

- **Config edits / `POST /config/set` now refresh the open tab.**
  The daemon was re-reading `.mathpreview.toml` correctly on every
  render, but the rendered HTML's `<head>` (where the
  `--body-font-size` CSS variable and `__mpConfig` JS object live)
  is only sent on the initial `GET /`. Body-updated / patch
  WebSocket messages don't include it, so `viewer.font-size`
  changes took effect only after a manual reload. Each WS render
  message now carries the resolved `viewer_config`, and the client
  re-applies `--body-font-size` + `__mpConfig` live.

### Changed (protocol)

- **WS protocol bumped to v60** so v0.1.17 tabs auto-reload on the
  next reconnect.

## [0.1.17] — 2026-06-02

### Added

- **`log` toolbar button + Daemon-state dialog.** Click it to see
  exactly where the daemon is reading config / macro overrides from,
  which files are applied vs. missing, the currently-resolved
  `viewer.font-size` / `source-jump.trigger` / page mode / theme,
  the active `--editor` template, the WS protocol version, and a
  scrolling tail of recent server events (config writes, macro
  appends + registrations, config reloads, parse errors). Refresh
  button re-fetches.
- **`GET /debug`** HTTP endpoint backing the dialog — JSON
  snapshot, safe to poll, identical info you'd see in the dialog.
- **Server log ring buffer** capped at 400 entries that mirrors the
  most useful `eprintln!` lines so you can read them in the
  browser without having to find the terminal that started the
  daemon.

### Changed (protocol)

- **WS protocol bumped to v59** so v0.1.16 tabs auto-reload on the
  next reconnect.

## [0.1.16] — 2026-06-02

### Changed

- **Configured source-jump trigger now also fires the polling `/jump`
  path** in parallel with `/reveal-source`. Previously a Cmd-click
  only hit `/reveal-source`, which fails silently if your editor
  template needs `$NVIM_LISTEN_ADDRESS` and that's not set. Now the
  same gesture fires both endpoints: the nvim plugin polling `/jump`
  picks the request up regardless of whether the spawn template
  works, matching what double-click was already doing.
- **`/reveal-source` failures are silenced on the status pill** when
  fired from a click trigger and downgraded to a `console.warn`,
  since the `/jump` path running in parallel is enough to land the
  navigation. The pill keeps showing the successful "● source jump"
  message.

### Changed (protocol)

- **WS protocol bumped to v58** so v0.1.15 tabs auto-reload on the
  next reconnect.

## [0.1.15] — 2026-06-02

### Changed

- **Mtime-cached override + config file reads.** v0.1.13 routed every
  render through `std::fs::read_to_string` for each override file and
  TOML config file in the cascade, on the buffer-push hot path. The
  reads themselves are fast on macOS, but the work adds up under
  per-keystroke typing. Replaced with a `(path → mtime, content)`
  cache: on each render we `stat()` once per file, and only re-read
  if the mtime changed. Identity hits stay zero-syscall.
- **Release builds use `lto = "thin"` and `codegen-units = 1`.** Frees
  up cross-crate inlining for the parser + renderer hot path; modest
  speedup at the cost of slightly slower release builds locally.
- **One small allocation removed from the body parser inner loop.**
  Previously `parse_block_into` called `format!("\\end{{{env}}}")`
  on every byte iteration when a stop-env was set; now formatted once
  outside the loop.

### Changed (protocol)

- **WS protocol bumped to v57** so v0.1.14 tabs auto-reload on the
  next reconnect.

## [0.1.14] — 2026-06-01

### Fixed

- **Content zoom (`+` / `-` keys) left a tall dead strip below the
  document.** `main#page` was scaled with `transform: scale`, which is
  visual-only — the layout box stayed at full size and overflowed the
  shell, inflating `html.scrollHeight` so the user could scroll past
  the visible content. At `userZoom = 0.5` on a long paper, that was
  ~18 000 px of trailing whitespace. Replaced with the CSS `zoom`
  property, which scales the *layout* box too: `html.scrollHeight`
  now tracks `body.scrollHeight` at every zoom level, with the
  trailing space reduced to the warnings panel's natural margin
  (~24 px). The JS no longer has to compute an explicit
  `shell.style.height` either — CSS auto-sizes the shell to the
  zoomed content.

### Changed

- **WS protocol bumped to v56** so v0.1.13 tabs auto-reload on the
  next reconnect.

## [0.1.13] — 2026-06-01

### Added

- **Macros dialog: Load file + Use as override + Custom save path.**
  - *Load file…* opens a browser file picker; the contents land in
    the textarea so you can review/edit before saving.
  - *Use as override* registers a path as a live override layer — the
    daemon watches the file for hot-reload and includes it in the
    override cascade for the rest of the session. Type the filesystem
    path in the *Custom path* field (`~/...` is expanded; relative
    paths anchor at the document root) and click the button.
  - *Custom* save scope writes to an arbitrary path of your choosing.
- **`config` toolbar button + Edit-config dialog.** Same shape as the
  macros dialog: typed-input fields for `viewer.font-size`,
  `viewer.source-jump.trigger`, `viewer.default-page-mode`, and
  `viewer.default-theme`; Project / Global / Custom save scopes. The
  daemon parses the existing TOML via `toml_edit` (preserves
  formatting and comments), updates the keys, writes back, and
  re-renders so the new defaults flow into the next reload.
- **`viewer.default-page-mode`** (`"a4"` | `"dynamic"`) and
  **`viewer.default-theme`** (`"system"` | `"light"` | `"dark"`) new
  config fields. Applied to fresh tabs whose localStorage hasn't set
  the corresponding key yet — the user's in-browser toggle still
  wins for tabs they've actively customized.
- **`POST /macros/register`** and **`POST /config/set`** HTTP routes
  backing the two new dialog actions.

### Fixed

- **Serve-mode macros override cascade.** v0.1.10's cascade quietly
  bypassed `serve` mode because `render_cached` called the
  no-overrides `extract_preamble` and cached on a key that didn't
  include the override fingerprint. After `POST /macros/append`
  (v0.1.12) the file was written but the rendered HTML still showed
  the old preamble. `render_cached` now uses
  `extract_preamble_with_overrides` and the cache key includes a
  hash of the override files' contents, so any edit invalidates the
  cache cleanly.
- **Override + config files that don't exist yet are still part of
  the cascade discovery.** Previously `discover_macro_overrides` and
  `discover_config_files` returned only existing files, so a
  `.mathpreview-macros.tex` created mid-session via the dialog
  wasn't picked up until the daemon restarted. Both now include the
  "would-be" project path so the watcher tracks it from the start.

### Changed

- **WS protocol bumped to v55** so v0.1.12 tabs auto-reload on the
  next reconnect.

## [0.1.12] — 2026-06-01

### Added

- **`macros` toolbar button + Add-override dialog.** Click the new
  toolbar button between `lines` and `margin` to open a dialog. Paste
  a `\newcommand` line, pick "Project" (writes to
  `.mathpreview-macros.tex` in the document root, creating the file
  if missing) or "Global" (writes to
  `~/.config/mathpreview/macros.tex`, creating the dir + file if
  missing), and click Save. The daemon validates the input through
  the macro extractor before writing; invalid lines surface an error
  inline. After a successful write the page re-renders so the
  override takes effect immediately.
- **`POST /macros/append` HTTP route** powering the dialog.
- **Macro override files are now part of the file watcher**, so
  manual edits in your editor live-reload the same way edits to the
  paper itself do.
- **New core helpers:** `MacrosScope`, `resolve_override_path`,
  `validate_override_line` — usable from any front-end (the Tauri
  shell, plugin, custom UI) that wants its own dialog.

### Changed

- **WS protocol bumped to v54** so v0.1.11 tabs auto-reload on the
  next reconnect.

## [0.1.11] — 2026-06-01

### Added

- **TOML config cascade.** Personal preferences in
  `~/.config/mathpreview/config.toml`; per-project overrides in
  `.mathpreview.toml` (walks up from the input file); one-off
  `--config <file>` CLI flag on both `serve` and `render`. Later
  layers win per field. First two settings flowing through:
  - `[viewer] font-size = N` — body font size in CSS pixels (default
    18). Overrides the `--body-font-size` variable in the rendered
    page.
  - `[viewer.source-jump] trigger = "..."` — `"cmd-click"` |
    `"ctrl-click"` | `"alt-click"` | `"double-click"`. Picks which
    gesture sends a `POST /reveal-source` to spawn the configured
    editor. Default `"cmd-click"` (which also matches Ctrl-click on
    Linux, the previous hardcoded behaviour).
- **New core API:** `Config`, `ResolvedConfig`, `SourceJumpTrigger`,
  `discover_config_files`, `load_and_merge_config`.

### Changed

- **WS protocol bumped to v53** so v0.1.10 tabs auto-reload on the
  next reconnect.

## [0.1.10] — 2026-05-31

### Added

- **Macro override cascade.** Define your own `\newcommand` replacements
  for any macro the paper uses — for example, swapping a
  `\DeclarePairedDelimiterX`-defined `\set` for a plain
  `\newcommand{\set}[1]{\{#1\}}` so MathJax can render it. Files are
  discovered in cascade order, with later definitions winning by name:
  1. Bundled built-ins
  2. The paper's preamble (including local `.sty` / `.tex`)
  3. `~/.config/mathpreview/macros.tex` (or `$XDG_CONFIG_HOME/...`) —
     personal overrides applied to every paper
  4. `.mathpreview-macros.tex` walking up from the input file — repo-
     specific overrides that can ship alongside the source
  5. `--macros <file>` CLI flag (repeatable) — one-off overrides
- **New `discover_macro_overrides` core API** wiring the same cascade
  for any library caller (Tauri shell, plugin, custom front-end).

### Changed

- **Hardcoded `FALLBACK_MACROS` moved into a bundled
  `assets/builtin-macros.tex`** parsed through the same extractor as
  the paper preamble. Adding a new built-in stub is now a one-line
  `.tex` edit instead of a Rust source change; users can read the
  bundled file to see exactly what's silently shimmed.

## [0.1.9] — 2026-05-31

### Fixed

- **Tall trailing white space below the document on long papers.** The
  page-shell's JS-computed height was based on `page.scrollHeight`,
  which is inflated by the absolutely-positioned page-guide markers
  inside `main#page`. On a long paper that adds hundreds of pixels of
  dead strip after the content. Switched to `page.offsetHeight`
  (visible content only) for the shell sizing and for the page-guide
  count, so the shell now ends right at the visible page bottom and
  guides don't extend past it.
- **Warnings panel reads as part of the paper frame.** Moved
  `<details class="warnings">` out of `<main id="page">` so the
  amber notice sits below the white paper, on the backdrop, instead
  of inside the reading frame. Tightened the gap between the paper
  and the warnings panel via `:has(+ details.warnings)`, and added a
  matching `margin-mode.margin-has-cards` rule so the panel tracks
  the shell's offset when the margin column is pinned.

### Changed

- **WS protocol bumped to v52** so any tab still attached to a v0.1.8
  daemon picks up the new HTML/CSS automatically on the next
  reconnect.

## [0.1.8] — 2026-05-31

### Added

- **Content-only zoom.** `+` / `-` zoom the page (and `0` resets, `=`
  auto-fits to viewport width) without scaling the header or sidebar.
  `Cmd`/`Ctrl` + `+`/`-`/`0` also work. Applies the user zoom on top of
  the existing A4 auto-fit so the same scale value behaves intuitively
  in both page modes. Persisted under
  `localStorage["mathpreview.userZoom"]`.
- **Capital `B` toggles the top banner.** Keyboard counterpart to the
  thin stripe on the left edge — useful for filling the viewport with
  paper content side-by-side with an editor.
- **Cmd/Ctrl-click → open source in editor.** Modifier-click on any
  rendered token spawns the configured editor at the source line.
  Configurable via a new `--editor` serve flag (default: a `nvim
  --server "$NVIM_LISTEN_ADDRESS" --remote-send` invocation that lands
  inside the nvim instance whose listen socket is in your env).
  Alt-click still posts to `/jump` for users running the polling-based
  nvim plugin.

### Changed

- **Default body text bumped from 16 px to 18 px** so the paper is
  readable at native browser zoom — friends reported the previous size
  required browser zoom, which also magnified the toolbar.
- **Compact top banner.** Reduced padding and inter-row gap; reference
  `--topbar-height` from 78 px to 60 px.

### Fixed

- **Old-style font switches `{\bf foo}`, `{\em foo}`, `{\it foo}`,
  `{\tt foo}`, `{\sc foo}` no longer drop the styling.** The parser
  was emitting `\bf` etc. as opaque commands without arguments, which
  the renderer then dropped silently — only the surrounded text
  survived. Keep these no-arg switches inline in the text buffer so
  the inline-latex pass can detect the brace group and wrap the body
  in `<strong>` / `<em>` / `<code>` / `<span class="sc">`.

## [0.1.7] — 2026-05-26

### Fixed

- **Line numbers counted the render-warnings panel.** v0.1.6 moved the
  warnings `<details>` inside `#page`, but the line-number walker numbers
  every text node in `#page`, so the panel's text picked up line numbers.
  Added `.warnings` to the line-numbering skip list.

## [0.1.6] — 2026-05-26

### Added

- **Typeset line numbers.** New `lines` toolbar toggle numbers every
  wrapped visual line of body text in the left margin, LaTeX
  `lineno`-style. Computed client-side from each line's client rect and
  recomputed on render, MathJax typeset, resize, zoom, and A4 ⇄ dynamic
  switches. Display equations are skipped (SVG has no text nodes),
  matching `lineno`'s default; inline-math paragraphs still number.
  Persisted under `localStorage["mathpreview.lineNumbers"]`.

### Changed

- **Render-warnings panel moved to the end.** The macro/unmapped-package
  `<details>` notice now renders as the last element inside the page
  instead of above the document, so it no longer pushes the paper down.

### Fixed

- **Dark mode: light strip at the end of the document.** `<html>`'s
  background stayed light in dark mode because `--bg` was overridden on
  `body.theme-dark` while `html { background: var(--bg) }` resolves the
  variable at the root level. `setTheme()` now toggles `theme-dark` on
  `<html>` too, and the token block matches `.theme-dark`, so the root
  background flips and over-scroll / trailing margin no longer shows
  cream.
- **Dark mode: invisible `:` command-line text.** `.cmdline-input` (and
  suggestion hover) used `var(--text, …)`, an undefined variable, so the
  text always fell back to near-black and vanished on the dark command
  line. Switched to `var(--fg)`.

## [0.1.5] — 2026-05-25

### Fixed

- **Inline math in text-like fields now renders.** `$…$` inside
  `\title`, `\author`, `\date`, list-item markers, and the inner
  content of `\emph` / `\textbf` / `\texttt` / `\textsc` was leaking
  through as literal source instead of being typeset, because
  `render_inline_latex` had no `$…$` branch (only the section-title /
  theorem-name path did). It now emits the same MathJax span the rest
  of the document uses.

## [0.1.4] — 2026-05-23

### Added

- **`t` keybinding** toggles the index/pages side panel from the
  viewer (same effect as clicking the `toc` pill, and persists to
  `localStorage["mathpreview.sideOpen"]`). Inert while focus is in an
  editable control.

### Changed

- **`Ctrl-o` now ping-pongs.** Previously it walked back through the
  jump stack one entry at a time and you could not return to where
  you came from. It now swaps the current scroll position with the
  top of the stack, so pressing `Ctrl-o` repeatedly bounces between
  the two most recent places.

## [0.1.3] — 2026-05-23

### Changed

- **Search panel layout.** The `/` panel is now a two-row grid: the `/`
  label + input occupy the full width on row 1; the shortcut hint
  (`Enter next · Shift+Enter previous · Esc close · prefix m: / $ for
  math-only`) wraps onto row 2 instead of competing with the input for
  horizontal space. Panel max-width is 720 px (was 520 px), input
  padding is 7 × 10 px with a 15 px font, and the input shows a purple
  focus ring.

### Added

- **Dark theme.** Topbar `☾` / `☀` button toggles `body.theme-dark`,
  persisted under `localStorage["mathpreview.theme"]`. First-load
  default follows the OS `prefers-color-scheme`. CSS overrides re-skin
  the topbar, side panel, paper surface, theorem boxes, refkey chips,
  command line, margin cards, hover preview, sidenotes, and warnings.
  MathJax 4 SVG glyphs use `fill="currentColor"` and follow the body
  text colour automatically.
- **Math-search sigil.** Prefix the `/` query with `m:` (e.g. `m:n`)
  or wrap LaTeX-style (`$n$` / `$n`) to force math-only mode. The
  search router skips `window.find` for sigil queries so single-letter
  searches can no longer get stuck cycling through body matches before
  reaching the equations.
- **Math-search glyph widening.** A single-character Latin or Greek
  query, or a `\command` whose Unicode mapping is known, now matches
  every stylistic variant MathJax may emit — italic, bold, bold-italic,
  script, fraktur, double-struck, sans, sans-bold, sans-italic,
  sans-bold-italic, monospace — by expanding the codepoint through the
  Mathematical Alphanumeric Symbols block (U+1D400..U+1D7FF) plus the
  irregular BMP holes (italic-h at U+210E, ℝ at U+211D, ℕ at U+2115,
  etc.). Searching `n` or `α` now hits the italic variant inside an
  equation; previously it silently missed because the SVG glyph's
  `data-c` was the math-italic codepoint, not the ASCII / base Greek
  codepoint.

## [0.1.0] — 2026-05-21

Initial public release. Two-component ship: the `mathpreview-cli` Rust
binary (the daemon) and the `mathpreview.nvim` plugin (`lua/mathpreview/`
plus `plugin/mathpreview.lua`) live in this same repo; install the
binary with `cargo install --git` or a Releases tarball, point your
nvim plugin manager at the repo, run `:MathPreview` in a `.tex` buffer.

### Added — daemon (`mathpreview-cli`)

- `serve <file>` — HTTP + WebSocket live-reload server, binds 127.0.0.1
  by default. Re-renders on disk change (via `notify-debouncer-full`)
  and on `POST /buffer` from an editor plugin. Pushes block-level
  patches to every connected browser tab.
- `render <file> -o out.html` — one-shot static HTML for sharing /
  archiving.
- `debug <file>` — print the extracted preamble + package mapping
  MathJax will see (mirrors `latex-preview.nvim` `:LatexPreview debug`).
- `POST /print` — compile the project with `latexmk -pdf` (falls back
  to `pdflatex` if latexmk isn't on `$PATH`) and stream the produced
  PDF back as `application/pdf`. Output path is parsed from the
  compile log so projects that customize `$out_dir` in `.latexmkrc`
  (`build/`, `out/`, etc.) work without configuration.
- `POST /buffer` — accept an in-memory editor buffer for the root file
  or any watched `\input` / `\include` / `\subfile` child. The daemon
  splices the override at the include site and re-renders the real
  root, so editing a chapter file updates the rendered output without
  writing to disk.
- `POST /cursor` + `GET /jump?after=<seq>` — bidirectional source
  sync: cursor in the editor highlights the nearest rendered element;
  double-click / Alt-click in the browser jumps the editor cursor to
  the source line.
- `POST /restart` + `POST /stop` — toolbar-driven daemon lifecycle.
- Self-contained binary. MathJax 4 (tex-svg, ~14 MB) and New Computer
  Modern body fonts (~800 KB) are embedded via `include_dir!` and
  `include_bytes!`. No source checkout, no separate asset download,
  and no internet access is needed at runtime.
- `--mathjax-url` flag overrides the embedded bundle. Defaults to
  `/vendor/mathjax/tex-svg.js`; pass `https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js`
  (or any MathJax 4 build URL) to pull MathJax from the network instead.

### Added — viewer (browser-side)

- Paper-like document layout: A4 sheet mode (scales with viewport) and
  dynamic flow mode. Index/Pages side pane, hideable top banner,
  generated page dividers.
- Role-tagged theorems (`[role=main|supporting|standard|omitted]`) with
  per-proof fold/unfold and a toolbar that bulk-sets fold state by
  role. Proof role can be inferred from preceding `Proof of …`
  headings or set explicitly with `\begin{proof}[role=…]`.
- Cross-references: `\ref` / `\cref` / `\eqref` / `\autoref` resolve to
  their friendly form ("Theorem 2.1", "(3.1)").
- Margin column with **three** pin entry points:
  1. Click any `\ref` / `\cite` link with the `margin` toggle on.
  2. Click the left-margin refkey chip (any labeled
     theorem/section/figure/equation, including per-row chips on
     multi-row `align`).
  3. `:pin <key>` from the vim-style command line, with Tab cycling
     through fuzzy matches in a wildmenu strip.
- Drag-to-reorder margin cards via the `⋮⋮` grip in each card header.
- Hover preview on `\ref` / `\cite` regardless of margin mode (~250 ms
  delay; preview omits proofs so you see the statement alone).
- Vim-style keyboard navigation: `hjkl`, `gg`/`G`, `Ctrl-d`/`Ctrl-u`,
  `/` for text search, `:` for the command line (`:pin`/`:unpin`/
  `:clear`), `n`/`N` to step through matches, `Ctrl-o` for previous
  place. Inert inside editable fields.
- Selectable MathJax equations: shift-click selects only the math
  node; copy returns the original LaTeX source instead of SVG text.
- `keys` toggle reveals every `\label{…}` as a clickable refkey chip
  pinned to the left margin (and multi-row equation chips per row).
- `print` button compiles a real PDF via the daemon and opens it.

### Added — nvim plugin (`mathpreview.nvim`)

- `:MathPreview` in a `.tex` buffer scans for a free port in
  23636..23651, spawns `mathpreview-cli serve <buffer> --port <port>`
  as a background `jobstart`, opens the system browser, and starts
  pushing the buffer on every `TextChanged` (40 ms debounce). Forward
  source sync via `CursorMoved`; inverse sync via a `/jump` poll.
  `VimLeavePre` reaps the daemon so quitting nvim doesn't leave it
  bound.
- `:MathPreviewStop` / `:MathPreviewRestart` / `:MathPreviewStatus` for
  explicit lifecycle.
- `setup({ cmd, mathjax_url, auto_open_browser, filetypes, debounce_ms, … })`
  for overrides. All optional — defaults work without it. `mathjax_url`
  is forwarded as `--mathjax-url` to the daemon spawn.
- nvim 0.10+ uses `vim.system` + `vim.uv`; older nvims fall back to
  `jobstart` + `vim.loop`. Tested against nvim 0.9 and 0.10.

### Added — content support

- biblatex output styles: `numeric`, `numeric-sorted`, `alphabetic`
  (`[SV06]` / `[BGL14]`), `authoryear`. `\addbibresource` and body-level
  `\bibliography{…}` both work; entries are sorted by author/year for
  alphabetic and author-year styles.
- AMS front matter: `\title` (with `\title[short]{long}` short form),
  `\author` (multiple, with `\and`), `\address`, `\curraddr`, `\email`,
  `abstract`. Browser tab title uses the short form when present.
- Macro extraction: `\newcommand`, `\DeclareMathOperator`,
  `\NewDocumentCommand`, `\def`, `\let`, `\DeclarePairedDelimiter`,
  `\newdelim`. Multi-file scan follows `\usepackage{name}` to local
  `.sty` files. TeX-internal forms (`\expandafter`, `\csname`,
  `@`-namespaced bodies, `##` parameters) are filtered.
- Numbering for sections, theorem-likes (AMS-modern shared counter
  scoped to section), equation envs, multi-row `align` / `gather`
  with `\notag` / `\nonumber` honored, `subequations` groups with
  alphabetic child suffixes, appendix renumbering.
- Float placeholders for `figure` / `table` with `\caption` and
  `\includegraphics`. Common width / height / scale / `keepaspectratio`
  options preserved; raster + SVG assets served from the project
  directory; PDF figures get a cached PNG preview.
- Lists: `enumerate`, `itemize`, `description`, paralist variants;
  each `\item` is recursively re-parsed for nested structure.
- LaTeX paragraph semantics: blank source lines create paragraph breaks
  with indentation (not `<br><br>` gaps), including inside
  theorem/proof environments and around display math.
- Inline LaTeX in title-like fields (section titles, theorem names,
  captions, `\omitref` payloads): `\emph`, `\textbf`, `\textit`,
  `\texttt`, `\textsc`, `\ref` family, accent commands (`\'e` → é,
  `\"o` → ö, …), and inline math (`$Y$` in `\begin{lemma}[$Y$-energy]`
  gets MathJax-typeset properly).

### Added — performance

- Block-level diffing on the wire. Every top-level block carries a
  `data-blockhash`; the server keeps the last broadcast and pushes
  compact `range` patches as `{event: "patch", ops: [{type: "range",
  index, remove, html}, …]}`. A one-character paragraph edit becomes
  a single-block range patch and lands in single-digit milliseconds
  on a 300-equation paper.
- Incremental MathJax typesetting. Each math node carries a stable
  content hash; the client transplants already-typeset DOM nodes
  from the previous render and only asks MathJax to typeset truly
  new expressions. A single-character text edit reuses 100% of math
  nodes (typeset cost ≈ 0 ms).
- Preamble caching + regex caching in the parser. Body-only edits on
  a 40 KB paper render in ~5 ms server-side.
- Mid-edit guard: the daemon defers buffer pushes that contain
  unclosed `$…$` / `\[…\]` / `\begin{…}`. The viewer keeps the last
  well-formed render instead of flashing a broken one.

### Internal — for contributors

- Workspace split: `crates/core` (pure library, no async/IO beyond
  caller-passed paths) and `crates/cli` (axum + tokio + notify
  daemon). The `MathEngine` trait + `Engine` dispatch enum keep the
  door open for non-MathJax engines without changing `renderer.rs`.
- `renderer.rs` split into `renderer/{util,shell,math,bib}.rs`.
- `client.js` split into `assets/client/{header,viewer,proof,patch,
  footer}.js` — five pieces sharing one outer IIFE, concatenated by
  `renderer/shell.rs` via `concat!(include_str!(…), …)`. ESLint
  pipes the assembled bundle through `--stdin` so `no-undef` catches
  cross-file typos.
- 93 cargo tests; `cargo clippy --tests --workspace` clean.

[Unreleased]: https://github.com/sonv/TexViewer/compare/v0.1.18...HEAD
[0.1.18]: https://github.com/sonv/TexViewer/releases/tag/v0.1.18
[0.1.17]: https://github.com/sonv/TexViewer/releases/tag/v0.1.17
[0.1.16]: https://github.com/sonv/TexViewer/releases/tag/v0.1.16
[0.1.15]: https://github.com/sonv/TexViewer/releases/tag/v0.1.15
[0.1.14]: https://github.com/sonv/TexViewer/releases/tag/v0.1.14
[0.1.13]: https://github.com/sonv/TexViewer/releases/tag/v0.1.13
[0.1.12]: https://github.com/sonv/TexViewer/releases/tag/v0.1.12
[0.1.11]: https://github.com/sonv/TexViewer/releases/tag/v0.1.11
[0.1.10]: https://github.com/sonv/TexViewer/releases/tag/v0.1.10
[0.1.9]: https://github.com/sonv/TexViewer/releases/tag/v0.1.9
[0.1.8]: https://github.com/sonv/TexViewer/releases/tag/v0.1.8
[0.1.7]: https://github.com/sonv/TexViewer/releases/tag/v0.1.7
[0.1.6]: https://github.com/sonv/TexViewer/releases/tag/v0.1.6
[0.1.5]: https://github.com/sonv/TexViewer/releases/tag/v0.1.5
[0.1.4]: https://github.com/sonv/TexViewer/releases/tag/v0.1.4
[0.1.3]: https://github.com/sonv/TexViewer/releases/tag/v0.1.3
[0.1.0]: https://github.com/sonv/TexViewer/releases/tag/v0.1.0
