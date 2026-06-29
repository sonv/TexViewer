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

## [0.1.78] — 2026-06-29

### Fixed

- **Pinning one margin note no longer expands the others.** The 📌 pin was
  implemented per-column, so expanding one card widened every card sharing that
  margin column. Pinning is now per-card: only the clicked card grows out over
  the text; its siblings stay at gutter width, hugging the page edge. (The column
  widens just enough to host the pinned card.) Each pin button now reflects its
  own card's state, and the expand follows a card when it's dragged to the other
  margin or removed when it's closed.

## [0.1.77] — 2026-06-29

### Fixed

- **Math now renders inside `quote` / `quotation` environments.** They weren't
  recognized, so they were captured as opaque blocks and their entire body —
  including any `$…$` or display math — was emitted as escaped text. They're now
  parsed like other block environments: the body is fully rendered (math
  typeset, `\ref`/`\eqref` resolved, display equations numbered) inside a
  `<blockquote>`.

## [0.1.76] — 2026-06-29

### Fixed

- **Source-jump focuses the right editor window under Hyprland, Sway, and tmux
  too.** A cross-platform audit of the window-raise turned up the same class of
  bug the KDE fix addressed, in three more spots:
  - **Hyprland:** `hyprctl dispatch focuswindow pid:…` prints `ok` even when the
    PID matched no window, so the process-tree walk stopped at nvim's own PID and
    a *terminal* nvim jump focused nothing. It now confirms a client actually owns
    the PID (`hyprctl -j clients`) before focusing, so the walk climbs to the
    terminal and the class fallback is reachable.
  - **Sway:** `swaymsg "[pid=…] focus"` returns `success:true` even when the
    criteria matched no container — same stall. It now checks the tree owns the
    PID first.
  - **X11 under tmux/screen:** `$WINDOWID` is inherited from whichever terminal
    started the multiplexer, so after reattaching elsewhere it focused the wrong
    (or a closed) window. The `$WINDOWID` fast-path is now skipped under
    `$TMUX`/`$STY`, falling through to the PID/class search.

  macOS was audited in the same pass and found correct — the terminal/GUI app
  detection and `osascript` activation resolve the right app (including VS Code
  via its `Code` bundle name).

## [0.1.75] — 2026-06-29

### Fixed

- **Source-jump now focuses the correct nvim window on KDE/KWin (Wayland).** The
  inverse-search window-raise ran `kdotool search --pid` (and the `--class`
  fallback) without `--all`, so kdotool only searched the *current* virtual
  desktop. When the editor sat on a different desktop than the focused viewer,
  the PID match missed and the class fallback activated the wrong nvim window.
  Both searches now pass `--all` (every desktop/activity), and the PID match is
  validated as a real `{uuid}` window id before activating, so the process-tree
  walk keeps climbing on a miss instead of grabbing a stray window. Also
  corrected a stale comment — KDE *is* reliably detectable via
  `XDG_CURRENT_DESKTOP=KDE`. Thanks to @gi1242 for the diagnosis and the
  reference implementation (`nvim-tex-inv-search`).

## [0.1.74] — 2026-06-28

### Added

- **Color-coded theorem boxes by statement type.** Theorem-like boxes (theorem,
  lemma, proposition, corollary, definition, remark, example, claim, conjecture)
  now carry a distinct accent color on their left border and heading word. The
  type is read from the `\newtheorem` title word, so an abbreviated environment
  (`\newtheorem{lem}{Lemma}`) still color-codes correctly. An explicit role
  (main/supporting/omitted) still overrides the per-type color. Light and dark
  themes each have their own palette.

## [0.1.73] — 2026-06-28

### Changed

- **Equation row highlight: one box around the block, a fill per row.** Replaces
  v0.1.72's per-row outline — whose edges clipped against each row's glyph bounds,
  leaving "missing lines" — with a single clean box around the whole equation (an
  HTML outline on its SVG) plus a fill tint on each highlighted row.

## [0.1.72] — 2026-06-28

### Changed

- **Highlighted equation rows are boxed.** The per-row highlight (cursor or
  selection on an `align`/`gather` row) now draws a crisp outline around the row
  plus a faint fill, instead of just a fill tint — making the active line clearer.
  The border stays a constant width at any zoom (`non-scaling-stroke`).

## [0.1.71] — 2026-06-28

### Changed

- **Cursor tracking no longer flashes whole-line block elements.** Moving the
  cursor onto a section heading (a block-level leaf) no longer lights up the
  entire line — outside equations only the inline content under the cursor (a
  word, inline math, a ref) flashes, and multi-row equations still band the
  cursor's row. Headings are still highlighted when you visually *select* them.
  (Implementation: a new `SyncKind::Block` that's part of a selection range but
  excluded from the single-point cursor lookup.)

## [0.1.70] — 2026-06-28

### Changed

- **Cursor tracking: flashing highlight on prose, persistent band on math rows.**
  Refines v0.1.69 — a cursor on a multi-row `align`/`gather` row highlights that
  row with the persistent band (so you can see which line you're on), while on
  prose, sections, or a single equation it restores the original brief *flash* on
  the element under the cursor instead of a constant band. Moving between the two
  clears the other.

## [0.1.69] — 2026-06-28

### Changed

- **The preview highlight follows the cursor's line, not just selections.**
  Moving the cursor in the editor now highlights the element(s) on its current
  line in the preview — and the specific `align`/`gather` row, reusing the
  per-row highlight — with the same band a visual selection uses, gently
  scrolling to keep it in view. Previously a normal-mode cursor move only briefly
  flashed the single element under it. (Cursor sync stays gated by the existing
  `sync` option.)

## [0.1.68] — 2026-06-28

### Added

- **Editor selection highlights individual rows of multi-line math.** Selecting
  lines of an `align` / `gather` (etc.) in the editor now highlights exactly
  those rows in the preview — a translucent band behind each selected row —
  instead of the whole block. The daemon maps the selected source lines to row
  indices (recorded in the sync index per multi-row block) and the viewer tints
  the matching MathJax table rows by inserting an SVG `<rect>` inside each row,
  so it stays aligned under page zoom. It accounts for a trailing `\\` and for
  rows containing nested `matrix`/`cases`, and falls back to a whole-block
  highlight when the rendered row structure can't be matched.

## [0.1.67] — 2026-06-28

### Changed

- **Edits route to the daemon that owns the file — robust across multiple open
  projects.** Building on per-file daemons (0.1.66): each daemon now reports its
  watched-file set (root + `\input`/`\include` + bib) via `/debug`, and the
  plugin routes an edit (and `:MathStop`/`:MathRestart`) in any project file to
  the daemon that watches it — matched on canonical, symlink-resolved paths. So
  with several projects open at once, editing an `\input` of one always updates
  that project's tab. This removes the prior "an `\include` of a non-active
  project could mis-route" caveat.

## [0.1.66] — 2026-06-28

### Added

- **One preview tab per file.** `:MathPreview` now runs a separate daemon (and
  browser tab) per root `.tex` file, so opening another file with `:e` and
  running `:MathPreview` gives it its OWN viewer instead of reusing the first
  file's. The plugin keeps a registry of daemons and routes edits / cursor /
  source-jumps to the file you're in — including a project's `\input`/`\include`
  children, which the root daemon watches. Re-running `:MathPreview` on a file
  you already opened reuses its tab; `:MathPreviewStop`/`Restart` act on the
  current buffer's daemon; quitting nvim stops them all.

### Changed

- **Reuse a stale tab across nvim restarts.** After you quit and reopen nvim and
  run `:MathPreview`, a previous session's tab (still open in the browser,
  retrying its WebSocket) reconnects to the rebound port and hard-reloads; the
  plugin now waits briefly for that and opens a new tab only if none reconnected.
  Tunable via `stale_tab_wait_ms` (default 1500 ms; set 0 to open immediately).
  Reliable when the new daemon rebinds the same port (the common single-file
  case); orphaned tabs on a now-different port can't be revived.

## [0.1.65] — 2026-06-28

### Fixed

- **Repeat `:MathPreview` reuses the open tab even against an older daemon.**
  v0.1.64 decided reuse from the daemon's connected-client count; if the running
  binary predated that field, it fell back to opening a new tab — so a repeat
  `:MathPreview` in the same session could still duplicate. The plugin now also
  tracks, per daemon session, whether it already opened a tab, and reuses on
  that when the client count isn't available. (The count still takes over when
  present, so a tab you closed is reopened.) Two different files in two nvim
  windows still get their own tabs.

## [0.1.64] — 2026-06-28

### Changed

- **`:MathPreview` reuses an already-open preview tab** instead of opening a new
  one each time the daemon is already running. The plugin asks the daemon how
  many browser tabs are connected (new `/debug` `clients` count, the live
  WebSocket subscriber count) and reuses the open one — it live-reloads, so it's
  already current — opening a fresh tab only when none is connected (e.g. you
  closed it). (`:MathPreviewRestart` already reused the tab by rebinding the
  same port.)



### Changed

- **Margin notes match the main text size.** They now scale with the document's
  page scale (`--page-scale` — which includes `Cmd/Ctrl` +/- zoom and the A4
  fit-to-width), so a note reads at the same size as the body text in every mode
  (previously a fixed 0.75×).
- **Margin columns no longer overlap the text by default.** Each column fits the
  whitespace gutter beside the centered page (computed from the page width). A
  new per-card **📌 pin** button expands that column out over the text when you
  want to read a note that's too narrow in the gutter; click again to dock it
  back. (On a very narrow window the column keeps a small minimum width, so it
  can still slightly overlap there.)

## [0.1.62] — 2026-06-28

### Added

- **Margin notes dock left or right.** Drag any pinned card to a left or right
  gutter column; each card remembers its side (persisted). A dashed drop zone
  appears while dragging so the destination is obvious.
- **Magnify a margin note.** Each card has a ⤢ button that opens the note
  centered on screen, enlarged, for comfortable reading; Esc, a backdrop click,
  or × dismiss it.
- **`Cmd/Ctrl+M` toggles margin mode** (in addition to the toolbar button).

### Changed

- **The reading frame no longer shifts when notes are pinned** — it stays
  centered and the columns overlay the gutters (click-through except on the
  cards themselves).
- **Pinned notes survive turning margin mode off**, so you can click through to
  the document without losing them. `:clear` (or each card's ×) removes them.
- **Long notes scroll inside their card** instead of stretching the column, and
  an over-wide equation scrolls horizontally.

### Fixed

- **Margin notes scale with `Cmd/Ctrl` +/- zoom.** The document zoom is applied
  to the page via CSS `zoom`, which the fixed margin columns didn't inherit; they
  now track the zoom so the notes match the document text size.
- A right-edge gutter sidenote no longer forces a horizontal scrollbar now that
  the page stays centered.
- A left-docked column yields to the open TOC side panel instead of covering it.

## [0.1.61] — 2026-06-28

### Changed

- **Margin cards now scale with the document text.** In margin mode, the
  equations / theorems / annotations pinned to the margin column derive their
  font size from `--body-font-size` instead of fixed pixels, so they grow and
  shrink with the body font-size setting and stay proportional to the main text.
  The whole card (content, title, `\label` key, close button) scales as one
  unit; the column keeps its width, and over-wide equations still scroll inside
  the card.

## [0.1.60] — 2026-06-28

### Fixed

- **`\ref` / `\eqref` to a `\tag`'d equation now resolve to the tag, not the
  label key.** An equation with a manual `\tag{a}` (e.g.
  `\begin{equation}\label{eq:a} … \tag{a}\end{equation}`) is correctly excluded
  from the automatic counter, but its label was never mapped to the tag — so
  `\ref{eq:a}` rendered `eq:a` and `\eqref{eq:a}` rendered `(eq:a)`. The tag
  value is now recorded as the label's number, so they resolve to `a` / `(a)`.
  Covers single displays and `\tag` rows inside `align` / `gather`, and both
  `\tag` and `\tag*`. (`\notag` / `\nonumber` equations stay unnumbered, so a
  label on one still falls back to the key — matching LaTeX.)

## [0.1.59] — 2026-06-28

### Added

- **Adjustable toolbar / TOC font size.** A new `[viewer] ui-font-size = N`
  setting (default 12) scales the toolbar (topbar) and the index/pages side
  panel (TOC) independently of the document body font. Adjustable from the
  config dialog ("UI font size (px)"), the `.mathpreview.toml` file, and live
  over the WebSocket without a reload. The chrome's font sizes now derive from a
  single `--ui-font-size` CSS variable (descendants via `em`), so the default
  reproduces the previous pixel sizes exactly and scales as one when changed.

### Fixed

- The floating side controls (the "toc" pill, the index/pages panel, the search
  panel, and the margin column) now stay anchored to the toolbar's **actual**
  height. `--topbar-height` was a hard-coded constant; it is now measured from
  the rendered toolbar and kept in sync on load, resize, banner show/hide, and
  UI-font-size changes — so a larger `ui-font-size` (or responsive wrapping) no
  longer leaves those controls overlapping or floating off the toolbar's edge.

## [0.1.58] — 2026-06-25

### Added

- **Annotation / callout environments render with math.** Review-package
  environments — `todo`, `note`, `note*`, `added`, `removed`, `marked`,
  `markedleft`, `markedright`, `highlighted`, `quoted` — are now parsed
  *recursively* and shown as titled, color-tinted boxes, so math and nested
  content inside them typeset. Previously they fell through to the opaque path
  and their bodies (including `$…$`) were dumped as raw text. Each box takes an
  optional `[title]` where the environment defines one.
- **Inline review commands.** `\add`, `\remove`, `\highlight` render their
  argument (text *or* math) with an underline / strikethrough / highlight (the
  optional `[color]` is honored), and `\replace{old}{new}` shows struck-through
  `old` followed by underlined `new`. `\sidenote` continues to render as a
  margin chip. Only this recognized set is treated as callouts; other unknown
  environments (`verbatim`, `lstlisting`, floats, …) keep the existing opaque
  path, so the change is scoped and doesn't disturb them.

## [0.1.57] — 2026-06-25

### Fixed

- Line-number gutter alignment at non-default font sizes and page scales. Each
  number is now vertically centered on its line using the measured line height
  (so it tracks the text as the body font grows, instead of sitting at the top
  of a tall line), positions are converted out of the page's CSS-`zoom` space so
  they stay aligned when the page is scaled (A4 fit / zoom), and changing the
  font size in the config now re-lays-out the gutter.

## [0.1.56] — 2026-06-25

### Fixed

- The viewer's line-number gutter now re-measures when the layout reflows —
  window resize, page-mode switch (A4 ↔ dynamic), zoom, and topbar hide/show.
  Previously it was only recomputed on render, so resizing the window left the
  numbers misaligned against the wrapped lines.

### Added

- The nvim plugin shows live progress (spinner, elapsed time, current crate)
  while it builds the daemon binary on first run or a version-skew reinstall,
  instead of appearing to hang.

## [0.1.55] — 2026-06-25

### Fixed

- **Viewer reload loop.** 0.1.54 bumped the WebSocket protocol to 66 on the
  server but left the browser's hardcoded copy at 65, so every connection
  failed the version check and the page reloaded in a tight loop (constant
  flashing). The client now reports 66, and a `client_ws_protocol_matches_server`
  test keeps the two copies in lockstep so this can't drift again.

## [0.1.54] — 2026-06-25

### Added

- **Editor selection → preview highlight.** A visual-mode selection in nvim
  tints the matching region in the preview (the range generalization of the
  existing cursor sync) — live as you extend it, cleared on leaving visual
  mode. New `POST /selection` route and `source-range` WebSocket event;
  linewise `V` covers whole rows, blockwise `Ctrl-V` is its bounding rectangle.

### Fixed

- The live render no longer panics on a `\` immediately before a multibyte
  character (accented text, Greek, em-dash, …) — fixed across the parser,
  bibliography normalizer, equation-row splitter, and command scanners. This
  was reachable per-keystroke on perfectly ordinary input.
- Deeply nested environments no longer overflow the stack (nesting is capped
  and the excess is captured as an opaque block).
- Equation numbering matches LaTeX more closely: `\tag` / `\tag*` rows no longer
  consume the automatic counter, `\appendix` preserves continuously-numbered
  theorem counters, and alphabetic bibliography labels no longer overflow with
  many colliding keys.
- Macro extraction: xparse argument counts ignore `m` inside defaults,
  `\newcommand[N]` is clamped to 9, optional defaults are read brace-balanced,
  and commented-out `% \input` / `% \usepackage` lines no longer pull in files.
- The preview no longer desyncs or drops edits under concurrent renders, a
  lagging WebSocket client, or a save racing a keystroke.
- nvim: a second `:MathPreview` during startup can no longer spawn a duplicate
  daemon; a port-bind race now retries on the next port; the Windows browser
  opener works (`cmd /c start`).
- Viewer: the print PDF's object URL is released after use; the macro-save TOML
  encoder handles control characters.

### Security

- The daemon now rejects cross-origin and DNS-rebinding requests (`Host` +
  `Origin` checks) against its unauthenticated control endpoints (`/stop`,
  `/restart`, `/print`, `/buffer`, …).
- HTML/script injection via `\newtheorem` names and `.bib` `url` fields is
  closed (escaping + URL-scheme allow-list).
- `\input` / `\include` / `\subfile`, `% !TEX root`, and `\bibliography` /
  `\addbibresource` path resolution is bounded to the project, so an untrusted
  document can't read arbitrary local files into the preview.
- `--host` documents the exposure and the daemon warns on non-loopback binds.

### Changed

- WebSocket protocol **65 → 66**; open tabs hard-reload to pick up the new
  `source-range` event.
- Release CI hardened: least-privilege `GITHUB_TOKEN`, SHA-pinned actions,
  `cargo --locked` builds. Vendoring scripts gain opt-in SHA-256 verification.

## [0.1.53] — 2026-06-05

### Changed

- `raise_on_jump` now targets **this** nvim's own window instead of the first
  window matching a class. On Linux/BSD the plugin walks up the process tree
  from nvim's PID (`nvim → shell → terminal`) and raises the ancestor that
  actually owns a window, via each backend's PID selector — Hyprland
  `focuswindow pid:`, Sway `[pid=…] focus`, KDE `kdotool search --pid`, X11
  `xdotool search --pid`. So when two terminals are each running
  nvim+mathpreview, a source-jump raises the correct one. `jump_window`
  (class/app_id) is now used only as the fallback when the PID walk finds no
  window; X11 still prefers `$WINDOWID` when the host terminal exports it.
  Thanks to Gautam Iyer (gi1242) for the approach (`nvim-tex-inv-search`).

## [0.1.52] — 2026-06-05

### Added

- **`raise_on_jump` now handles Wayland (and X11 by class).** The built-in
  source-jump focus learned the compositor-specific paths Wayland requires:
  Hyprland (`hyprctl dispatch focuswindow`), Sway (`swaymsg [app_id=…] focus`),
  and KDE/KWin (`kdotool`), selected via each compositor's env marker. X11
  gained a fallback that activates the first window matching the class when
  `$WINDOWID` isn't set. New `jump_window` option (default `"nvim"`) sets the
  class / app_id to focus — point it at your terminal's class for terminal
  nvim (e.g. `"kitty"`, `"foot"`, `"Alacritty"`). GNOME/Mutter still has no
  general activation API; use the `on_jump` hook there. (macOS unchanged.)

## [0.1.51] — 2026-06-05

### Added

- **Source-jump now raises the editor window (SyncTeX-style focus), on by
  default.** Cmd/Ctrl-clicking in the preview already moved nvim's cursor;
  now it also brings nvim's host window to the front — the focus a PDF viewer
  gives you, which on macOS the cursor-move alone didn't do. New
  `raise_on_jump` option (default `true`), best-effort and platform-aware:
  macOS `osascript … activate` on the detected terminal (`Terminal`, `iTerm`,
  `WezTerm`, `Ghostty`, kitty, Alacritty; `$LC_TERMINAL` fallback inside tmux)
  or GUI (`Neovide`, `nvim-qt`); X11 `xdotool windowactivate $WINDOWID`.
  Wayland is compositor-specific — use the `on_jump` hook (e.g. kdotool on
  KDE). Set `raise_on_jump = false` to stop focus-stealing on every click.

## [0.1.50] — 2026-06-04

### Fixed

- **Idle CPU drop: source-jump polling is now a long-poll.** The nvim plugin
  used to `curl GET /jump` every 120 ms forever — ~8 process spawns a second
  even when nothing was happening, which showed up as a few % CPU on an idle
  preview (gone the moment you `:MathPreviewStop`). The daemon's `/jump` now
  accepts `?wait=<ms>` and parks the request until a jump actually arrives (or
  ~25 s elapses); the plugin keeps exactly one request in flight and re-issues
  on return. Idle cost drops from 8 spawns/s to ~one parked request per ~26 s,
  with no change to jump latency (a click still wakes the parked poll
  instantly). Backward compatible: `/jump` without `wait` behaves as before.

### Added

- **`on_jump` plugin hook.** Runs after a preview Cmd/Ctrl-click has moved
  nvim's cursor, so you can raise/focus the editor window the way a PDF viewer
  does — something most HTML previewers can't. Signature `function(jump)` with
  `jump = { file, line, col }`. README shows a KDE/Plasma + Wayland + nvim-qt
  example using `kdotool windowactivate`.

### Internal

- Reveal-source now detects an attached editor via a live in-flight poller
  count (`active_jump_pollers`), not just the last-poll timestamp — correct
  under long-polling, where the timestamp only refreshes every ~25 s.

## [0.1.49] — 2026-06-04

### Fixed

- **`:MathPreviewRestart` no longer piles up browser tabs.** Restart now
  rebinds the *same* port the previous daemon held, and the plugin skips the
  browser-open when it does — the already-open tab's live-reload WebSocket
  reconnects on its own (1s backoff). Previously every restart spawned a fresh
  tab, so a handful of restarts left a handful of orphaned tabs. (If the old
  port can't be reclaimed and the restart lands on a different port, the old
  tab is stale, so it does open a new one.)
- **Reconnecting after a restart now hard-reloads the page.** A WS reconnect
  alone only resumes body patches; the `<head>` (MathJax config, baked-in
  macros, client assets) stays as the *old* daemon rendered it. The client now
  does a `location.reload()` on its second-and-later `onopen`, so a restart
  actually pulls the freshly served page. (The protocol-version `full-reload`
  only fired on upgrades, not same-version restarts.)
- **Macro / config edits take effect on the first save, not just after a
  restart.** Macro-override files (`~/.config/mathpreview/macros.tex`, project
  `.mathpreview-macros.tex`) and TOML config files are now in the file-watch
  set from the *initial* render. Before, they only joined the watch set after
  the first buffer push, so editing `macros.tex` before typing in the document
  was silently ignored until you restarted.

## [0.1.48] — 2026-06-04

### Changed

- **Decluttered the generated MathJax config's wrap settings.** With
  `wrap-equations` on, the `svg:` block now emits just
  `displayOverflow: 'linebreak'` (plus a short comment) instead of also
  setting `linebreaks: { inline: true, width: '100%' }` — those values were
  already MathJax's defaults, so the line was redundant noise in the read-only
  config view. The break *width* was never a config value anyway: each
  equation is rendered standalone via `tex2svg`, so the client adapter
  measures the column and passes it as `containerWidth` per call. No behaviour
  change — wrapping works exactly as before.

## [0.1.47] — 2026-06-04

### Fixed

- **`[text-macros]` now apply on the first render, not just after an edit.**
  The CLI built the daemon's startup `HtmlOptions` from the resolved config's
  `viewer` settings but dropped its `text-macros`, so the initial page (what
  you see when the browser tab opens) rendered with no text macros; they only
  kicked in after the first buffer push re-loaded the full config. The startup
  options now carry `text_macros` too. (The `[template, n_args, default]`
  MathJax-style array form already parsed — it just wasn't reaching the first
  render.)

## [0.1.46] — 2026-06-03

### Added

- **The config dialog shows the full generated MathJax config (read-only).**
  Under "MathJax config (advanced)" there's now a collapsible read-only view of
  the entire `window.MathJax = {…}` the daemon generates — macros, packages,
  output settings, everything in effect — so you can see what's there before
  writing an override (the editable override box is unchanged). The engine's
  config `<script>` got an id so the dialog can read it directly (no
  duplication).

## [0.1.45] — 2026-06-03

### Fixed

- **Previewing files from a second nvim no longer collides on the port.** The
  free-port probe only `bind`-ed the socket, but libuv sets `SO_REUSEADDR`, so
  the bind succeeded even when another daemon was actively listening on that
  port (notably on macOS) — the second nvim thought 23636 was free, spawned a
  daemon there, and the daemon's own bind failed with "address already in use".
  The probe now `bind`s **and** `listen`s, so it correctly detects the live
  daemon and `find_free_port` advances to 23637, 23638, … (scanning up to 16
  ports from 23636).

## [0.1.44] — 2026-06-03

### Added

- **Edit `mathjax-config` from the config dialog.** The toolbar **config**
  dialog now has a "MathJax config (advanced)" text box that loads the current
  raw-JS value and writes it back to `[viewer] mathjax-config` on Save (only
  when changed), instead of having to hand-edit the TOML. The value is exposed
  to the client via `window.__mpConfig.mathjaxConfig` (JSON-encoded so the JS
  round-trips safely).

## [0.1.43] — 2026-06-03

### Fixed

- **Long display equations now actually wrap.** `displayOverflow: 'linebreak'`
  was set, but the client renders each equation standalone via
  `tex2svgPromise`, which defaults `containerWidth` to `null` — so MathJax had
  no width to break against and never wrapped. The adapter now measures the
  column width (`clientWidth` of the math's block, walking ancestors) and
  passes it as `containerWidth` when wrapping is on, and the config sets
  `linebreaks.width: '100%'`. (Skipped when `wrap-equations = false`, so the
  overflow/scroll path is unchanged.)

## [0.1.42] — 2026-06-03

### Fixed

- **Toggling `wrap-equations` now reliably takes effect.** The setting lives in
  the MathJax `<head>` config, which live body pushes can't refresh, so the
  preview kept the old wrapping until a manual reload. The daemon now ships
  `wrap_equations` in the per-render `viewer_config` it pushes, and the client
  reloads the tab when it changes — covering every edit path (config dialog,
  the `.mathpreview.toml` file, the macros dialog's TOML editor), not just the
  dialog.

### Added

- **`mathjax-config` raw-JS option.** A `[viewer] mathjax-config` string of
  JavaScript is spliced in right after the generated `window.MathJax = {…}` and
  before the library loads, so you can override any MathJax option (output
  settings, extra macros/packages, …) by mutating `window.MathJax`. Changes
  reload the preview like `wrap-equations`. `head_html` now takes the resolved
  viewer config.

## [0.1.41] — 2026-06-03

### Added

- **`wrap-equations` config option** (issue #1). Long display equations'
  line-wrapping in the preview — previously hardcoded on — is now a
  `[viewer] wrap-equations` setting (default `true`). `true` keeps MathJax's
  automatic line-breaking (`displayOverflow: 'linebreak'`); `false` lets long
  math overflow and scroll horizontally (`displayOverflow: 'overflow'`),
  closer to a non-`breqn` PDF. Also a checkbox in the config toolbar dialog
  (changing it reloads, since the setting lives in the MathJax `<head>`
  config). Preview-only — it can't change how the PDF breaks lines.

## [0.1.40] — 2026-06-03

### Added

- **The macros dialog's Text→HTML mode regains a quick-add form, alongside the
  TOML editor.** Type a command name + HTML template and click **Add ↓** and it
  builds a correctly-quoted `[text-macros]` line and inserts it into the editor
  (creating the table if needed) — for users who don't know the TOML syntax —
  while the loaded editor below stays available for direct edits. Save still
  writes the whole file. README documents both paths and the
  string/array entry syntax in detail.

## [0.1.39] — 2026-06-03

### Changed

- **The macros dialog's Text→HTML mode now loads and edits the config TOML
  file directly** (symmetric with TeX mode editing the `.tex` override). The
  active scope's `.mathpreview.toml` loads into the editor; Save writes the
  whole file back after validating it parses as TOML, then re-renders.
  Replaces the previous single name+template form. New `POST /config/read`
  and `POST /config/write` endpoints back it.

## [0.1.38] — 2026-06-03

### Changed

- **The macros dialog is now a two-column layout** — the Project / Global /
  Custom tabs sit in a vertical rail on the left, giving the editor the full
  width (and a taller text box) on the right. The dialog is a bit wider to
  suit.

## [0.1.37] — 2026-06-03

### Changed

- **The macros dialog's scope picker is now a tab bar** (Project / Global /
  Custom) instead of a radio list. Each tab is an editor of that file: picking
  a tab loads its contents and shows the resolved filename, the Custom tab
  reveals its path input, and Save writes the active tab. Same behavior as the
  radios, clearer that each is a distinct, editable file.

## [0.1.36] — 2026-06-03

### Added

- **The macros dialog loads the existing override file for editing.** Opening
  the toolbar `macros` dialog (TeX mode) now pre-fills the editor with the
  current scope's `\newcommand` file instead of a blank box, and switching
  scope reloads to show that file. Saving writes the whole editor back
  (replacing the file) so re-saving never duplicates lines; each command line
  is validated first. Backed by a new `POST /macros/read` endpoint and a
  `replace` flag on `POST /macros/append`.

## [0.1.35] — 2026-06-03

### Added

- **`[text-macros]` entries take an explicit arg count and default, MathJax-
  style.** A value can now be either a bare string (`name = "<b>#1</b>"`,
  argument count inferred from the highest `#n`) or an array
  `name = [template, n_args, default]` — e.g.
  `hl = ['<mark style="background:#1">#2</mark>', 2, 'yellow']`, so `\hl{x}`
  uses the default first argument and `\hl[pink]{x}` overrides it. Mirrors
  MathJax's `tex.macros` shape. The template is HTML (TeX-valued macros still
  belong in a `macros.tex` `\newcommand`).

## [0.1.34] — 2026-06-03

### Added

- **The toolbar `macros` dialog can now write text→HTML mappings, not just
  `\newcommand`s.** A *Type* toggle switches between **TeX macro** (appends a
  `\newcommand` to a `.tex` override file, as before) and **Text → HTML**
  (writes a `[text-macros]` entry — command name + HTML template with
  `#1`..`#9` — to the chosen `.mathpreview.toml`). The scope labels update to
  show the right target file per mode, and the page re-renders on save. The
  HTML path reuses the existing `/config/set` writer, so no document or config
  formatting is clobbered.

## [0.1.33] — 2026-06-03

### Changed

- **Theorem numbering now reads `\newtheorem` from local packages too.** The
  registry scans the same local `\usepackage`'d / `\input`'d `.sty` / `.tex`
  files the macro extractor does (e.g. a sibling `svmacro.sty`), not just the
  root preamble — so theorem environments and counters defined in a package
  are honored. An environment declared more than once (which only happens
  across mutually-exclusive `\if…\else…\fi` branches, since LaTeX forbids
  redeclaring otherwise) is left at the built-in AMS default rather than
  letting an arbitrary branch win — so a conditional definition can't change
  numbering the wrong way.

### Documentation

- README "Macros in regular text" now lists exactly what's handled in text
  and gives step-by-step instructions for mapping a command to an HTML
  template via `[text-macros]`.

## [0.1.32] — 2026-06-03

### Added

- **User macros now expand in regular text, not just math.** Previously a
  `\newcommand` was only honored by MathJax inside `$…$`; in body text an
  unknown macro like `\hello` was silently dropped. The text renderer now
  expands the document's `\newcommand` definitions (and any override-file
  macros) with their arguments, re-rendering the result — so
  `\newcommand{\hello}{world}` makes `\hello` render as "world", and
  `\newcommand{\GI}[1]{\textcolor{red}{#1}}` makes `\GI{x}` a red span.
  Expansion is depth-limited to guard against recursive definitions.
- **Built-in `\textcolor`.** `\textcolor{name}{text}` renders a colored
  span; `\textcolor[HTML]{RRGGBB}{text}` takes a hex color. Color values are
  sanitized before going into the `style` attribute. (The `\color{…}` switch
  form is not supported yet — use `\textcolor`.)
- **`[text-macros]` config table for macros expansion can't reach.** Map a
  command name to an HTML template (`#1`..`#9` filled by the rendered
  arguments) in `.mathpreview.toml` (or the global config) — useful for
  commands defined with `\def` / `\NewDocumentCommand` / `\DeclarePairedDelimiter`
  (which aren't extracted), commands from a system package that isn't a local
  file on disk, or just for preview-only looks. (Plain `\newcommand`s — in the
  preamble *or* a local `\usepackage`'d `.sty` — already expand on their own.)
  Accepts the table name `[text-macros]` or `[text_macros]`; an entry overrides
  a `\newcommand` of the same name, and the cascade reloads live like the rest
  of the config.

## [0.1.31] — 2026-06-03

### Changed

- **The plugin's auto-build now uses `cargo install` instead of
  `cargo build`, so the binary lands on your `$PATH`.** On first
  `:MathPreview` (or after a plugin update moves ahead of the binary) the
  plugin runs `cargo install --path crates/cli --force`, dropping
  `mathpreview-cli` in your cargo bin dir (`$CARGO_HOME/bin`, default
  `~/.cargo/bin`) so it's usable in a terminal and by other tools — not just
  buried in `target/release/`. The documented `build`/`run`/`do` plugin-manager
  hooks switch to `cargo install --path crates/cli --force` to match.
- **Install location is detected, and a not-on-`$PATH` install is called
  out.** A new `install_root` option chooses where to install (passed to
  `cargo install --root`, binary lands in `<root>/bin`). The plugin runs the
  binary by its absolute installed path regardless, and if the install dir
  isn't on `$PATH` it warns once with the exact `export PATH=…` line to add.
  Resolution order is now: explicit `cmd` → cargo-installed binary (absolute
  path) → `mathpreview-cli` on `$PATH` → leftover `target/release/` build, so
  a fresh install can't be shadowed by a stale one.

### Added

- **Failed daemon spawns now report why.** Instead of a bare "exited with
  code N", the notification includes the binary path that was launched and the
  daemon's captured stderr. `:MathPreviewStatus` also reports `install_dir`
  and whether it's on `$PATH`.

## [0.1.30] — 2026-06-03

### Changed

- **Theorem/lemma numbering now follows the document's `\newtheorem`
  declarations instead of assuming one fixed AMS convention.** The preamble
  is parsed for `\newtheorem`, `\newtheorem*`, and `\numberwithin`, and that
  drives: which environment names are treated as theorem-like (so custom
  environments like `assumption` or `DL` are recognized and numbered), the
  heading word shown (`Theorem`, `Lemma`, `Satz`, a custom title…), whether
  an environment is numbered (`\newtheorem*` → unnumbered), and how its
  number is computed — shared vs independent counters, the sectioning level
  it resets under (or continuous numbering with no `[section]`). This makes
  the preview's numbers and `\ref`/`\cref` text match a real `latexmk`
  build. When the preamble declares nothing, the previous AMS default
  (all environments share one `theorem` counter, reset per `\section`) is
  used unchanged.

  Scope: declarations are read from the root file's preamble (and
  `\input`'d preamble). `\newtheorem` inside a `\usepackage`'d `.sty`
  package is not scanned (it can sit behind `\if/\else/\fi` the renderer
  can't evaluate), so such documents fall back to the AMS default.

## [0.1.29] — 2026-06-02

### Added

- **The plugin builds and rebuilds the binary itself — no plugin-manager
  build hook required.** On `:MathPreview`, if no `mathpreview-cli` is found
  and the checkout has the Rust sources + `cargo`, the plugin compiles it
  in-place (`<checkout>/target/release/`) and starts once done. If the
  in-checkout binary is older than the plugin (e.g. after `:Lazy update`
  pulls a newer plugin), it rebuilds it first. Both cases show a
  "building mathpreview-cli… please wait" notification so you know to wait
  out the one-time ~20s compile, then start automatically. A current binary
  starts immediately with no recompile; an explicit `cmd` or a `$PATH`
  binary you manage yourself is never rebuilt (it only gets the skew
  warning). The `build`/`run`/`do` hooks in the README are now optional —
  their only edge is moving the compile to update time.

## [0.1.28] — 2026-06-02

### Added

- **Auto-rebuild the binary on plugin update.** The README plugin-manager
  specs now carry a build hook (`build` for lazy.nvim, `run` for packer,
  `do` for vim-plug) that runs `cargo build --release -p mathpreview-cli`
  on install/update. The plugin resolves a binary compiled inside its own
  checkout (`<checkout>/target/release/mathpreview-cli`) automatically, so
  no separate `$PATH` install is needed — and since plugin and binary come
  from the same checkout, they can't drift. Precedence is explicit
  `cmd` → in-checkout build → `$PATH`. Requires a Rust toolchain; users who
  install the binary themselves can omit the hook.
- **Binary/plugin version-skew warning.** On `:MathPreview` the plugin runs
  `mathpreview-cli --version` and warns once per session if it doesn't match
  the version this plugin checkout expects. The plugin never updates the
  binary on its own, so this is the signal that a "released" fix isn't the
  binary actually running. `:MathPreviewStatus` now reports both
  `plugin_version` and `binary_version`.

## [0.1.27] — 2026-06-02

### Fixed

- **Reveal-source no longer opens a second buffer when an nvim plugin is
  attached — now fixed in the daemon itself.** The v0.1.26 fix only
  helped if the plugin passed `--editor ""`; a daemon spawned by nvim
  inherits `$NVIM`, so the default editor template still resolved and
  reopened the file via `:e` on top of the in-place `/jump`. The daemon
  now tracks `/jump` polling and skips the `/reveal-source` editor spawn
  whenever a poller has been seen in the last 2s (returns 204), so the
  plugin's in-place navigation is the only thing that runs. Browser-only
  users (no poller) still get the editor spawn as before.

### Fixed

- **Modifier-click no longer yanks you into a second buffer.** A
  reveal-source click fires both `/jump` (which the plugin polls and
  applies *in place*) and `/reveal-source` (which spawns `--editor`).
  Once v0.1.24 made the spawn actually work under the plugin, both paths
  ran, so `nvim --remote-send :e …` opened the file again on top of the
  in-place jump. The plugin now disables the editor spawn whenever cursor
  `sync` is on (the polled `/jump` already handles navigation), and only
  passes a `v:servername` editor command when `sync` is off. Set the
  plugin's `editor = '…'` to force a specific command, or `""` to always
  disable it.

## [0.1.25] — 2026-06-02

### Added

- **`:MathPreviewDebug`** — prints the daemon's resolved viewer settings,
  the reveal-source `editor_cmd` in effect, and the config / macro paths
  it consulted (with a `*` next to files that exist), so you can see what
  settings are loaded and where from without leaving the editor. Reads
  the existing `/debug` HTTP endpoint, which is also viewable in the
  browser at `http://127.0.0.1:<port>/debug`.

## [0.1.24] — 2026-06-02

### Fixed

- **Reveal-source (Cmd/Ctrl-click → editor) no longer errors under the
  nvim plugin.** The daemon's default editor command targets
  `$NVIM_LISTEN_ADDRESS`, which modern Neovim no longer exports, so the
  spawned `nvim --server "" …` failed with `E247: No server specified`
  and logged a recurring warning on every click. (Source-jump still
  worked because that's the plugin's separate polling path.) The bundled
  plugin now passes an explicit `--editor` built from `v:servername`, so
  reveal-source targets the running nvim and logs `reveal-source →
  file:line` instead. New `editor = '…'` plugin option to override it.

### Changed

- **Hand-run `serve` falls back to `$NVIM` for reveal-source.** The
  default `--editor` template now uses `${NVIM_LISTEN_ADDRESS:-$NVIM}`,
  so launching the daemon yourself from inside a Neovim `:terminal`
  (which exports `$NVIM`) also reaches the right instance.

## [0.1.23] — 2026-06-02

### Changed

- **Live updates inside a proof are much faster.** Editing one
  paragraph of a long proof or theorem used to re-send and re-parse
  the *entire* environment on every keystroke, because a proof is a
  single render block. The server now diffs the proof body at the
  paragraph level and pushes only the changed paragraph(s), so the
  per-keystroke cost scales with the edit, not the size of the proof.
  Typeset math in untouched paragraphs is preserved, and — as a side
  effect of patching the block in place — a proof's fold state now
  survives edits instead of resetting.

### Changed (protocol)

- **WS protocol bumped to v65** so v0.1.22 tabs auto-reload on the
  next reconnect and pick up the client side of the proof sub-block
  patch (a new `blocksub` patch op).

## [0.1.22] — 2026-06-02

### Changed

- **Log panel pushes the page over instead of floating on top.**
  When you open the panel the page-shell shifts right to keep the
  reading column visible; closing it puts the page back. Slides
  further right when the TOC is also open so neither overlaps the
  paper.

### Changed (protocol)

- **WS protocol bumped to v64** so v0.1.21 tabs auto-reload on the
  next reconnect.

## [0.1.21] — 2026-06-02

### Fixed

- **Configured source-jump trigger is now exclusive.** Previously
  picking `cmd-click` left double-click also firing the polling
  `/jump` path (and Alt-click did the same when the trigger wasn't
  alt-click), so the trigger setting felt non-exclusive. Removed
  the fallbacks — only the gesture you configured fires the
  source jump. The configured-trigger path itself still hits both
  `/jump` and `/reveal-source` from v0.1.16 so the nvim-plugin
  route is covered.

### Added

- **Source-jump and reveal-source events now log.** Clicking your
  trigger logs a `source-jump → file:line` line and (when the
  editor template runs) a `reveal-source → file:line` line.
  Editor failures still log a `warn` with the command's stderr.
  Useful for "did my click even register?".
- **Startup, restart, stop, watcher state logged through the
  buffer.** The panel now seeds with the initial render summary +
  watcher init line, so it's not empty before any user action.

### Changed (protocol)

- **WS protocol bumped to v63** so v0.1.20 tabs auto-reload on the
  next reconnect.

## [0.1.20] — 2026-06-02

### Added

- **Verbose-mode toggle in the log panel** — check the `verbose`
  box at the top right of the panel to stream high-frequency
  events that were previously only logged to the terminal:
  per-keystroke buffer-push timings, file-watcher change
  detections, file-change re-render outcomes, stale-render
  discards. Always-on entries (config writes, macros append +
  register, render errors) are unchanged, so the default mode
  stays focused.
- **`POST /debug/mode`** HTTP endpoint backing the toggle. `GET
  /debug` now also reports the current `debug_logging` state so
  the checkbox can sync across tabs.
- **More backend events now flow through the log buffer.** Render
  errors, watcher errors, and the file-watcher change notification
  are visible in the panel — same lines you'd see in the terminal
  but reachable from the browser.

### Changed (protocol)

- **WS protocol bumped to v62** so v0.1.19 tabs auto-reload on the
  next reconnect.

## [0.1.19] — 2026-06-02

### Changed

- **Log panel moved from modal dialog to a non-modal side panel.**
  The viewer stays interactive while the panel is open — read /
  click / type without dismissing it. Slides to the right of the
  TOC when the side panel is also visible so the two don't stack.
  Auto-refreshes whenever the daemon pushes a WS render update, so
  config changes and other events stream in live.

### Changed (protocol)

- **WS protocol bumped to v61** so v0.1.18 tabs auto-reload on the
  next reconnect.

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

[Unreleased]: https://github.com/sonv/TexViewer/compare/v0.1.22...HEAD
[0.1.22]: https://github.com/sonv/TexViewer/releases/tag/v0.1.22
[0.1.21]: https://github.com/sonv/TexViewer/releases/tag/v0.1.21
[0.1.20]: https://github.com/sonv/TexViewer/releases/tag/v0.1.20
[0.1.19]: https://github.com/sonv/TexViewer/releases/tag/v0.1.19
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
