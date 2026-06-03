# mathpreview

A live, browser-based preview server for LaTeX papers — keystroke-level
updates, no PDF roundtrip, no LaTeX engine on the user's machine.


https://github.com/user-attachments/assets/3b17927b-9769-4b5d-85a4-be38bb80dca8



The full design rationale (including the architectural pivot from the
original Tauri sketch) lives in [`DESIGN.md`](./DESIGN.md).

## What works today

- **One-shot render** of a `.tex` project (any file in a multi-file project)
  to a self-contained HTML file.
- **Live-reload server** (`mathpreview-cli serve`) — HTTP page + WebSocket
  push. Browser tab reflects edits within ~5–10 ms of a keystroke pause.
- **nvim integration** via the bundled `mathpreview.nvim` plugin
  (`lua/mathpreview/`, `plugin/mathpreview.lua`): `:MathPreview` in a
  `.tex` buffer spawns the daemon on a free port (default 23636,
  scanning up to 23651), opens the browser tab, and pushes the buffer
  on every `TextChanged`. No disk writes, no git pollution. `VimLeavePre`
  reaps the daemon so quitting nvim doesn't leave it bound.
- **nvim ↔ HTML source sync**: cursor movement in nvim can scroll and
  highlight the matching rendered word/math/ref element, and double-click
  or Alt/Cmd-click in the browser can jump nvim back to the source line.
- **Macro extraction** from real preambles — `\newcommand`,
  `\DeclareMathOperator`, `\NewDocumentCommand`, `\def`, `\let`,
  `\DeclarePairedDelimiter`, and the `\newdelim` wrapper. Multi-file scan
  follows `\usepackage{name}` to local `.sty` files. TeX-internal forms
  (`\expandafter`, `\csname`, `@`-namespaced bodies, `##` parameters) are
  filtered to keep MathJax from looping on unsupported expansions.
- **biblatex output** styles: `numeric` (default), `alphabetic`
  (`[SV06]` / `[BGL14]`), and `authoryear`. `\addbibresource` and
  `\bibliography{...}` both work; entries are sorted by author/year for
  alphabetic and author-year styles.
- **Bibliography and figure handling for real projects**:
  body-level `\bibliographystyle{plain}` is honored, `.bib` files are
  resolved relative to the main `.tex` file, and `\includegraphics`
  renders project-local raster/SVG assets plus cached PNG previews for
  PDF figures while preserving common width/height/scale options.
- **Numbering** for sections, theorem-likes (shared counter scoped to
  section, AMS-modern style), equation envs, multi-row `align`/`gather`
  displays, and `subequations` groups with alphabetic child suffixes.
- **Cross-references** resolve to their friendly form: `\cref{thm:main}`
  becomes "Theorem 2.1", `\eqref{eq:foo}` becomes "(3.1)".
- **`\title` / `\author` / `\date` / `\maketitle`** produce a centered
  title block. Repeated authors, `\and`, `\address`, `\curraddr`,
  `\email`, and `abstract` are handled for AMS-style front matter.
- **Lists** (enumerate / itemize / description / paralist variants) parse
  to `<ol>` / `<ul>` / `<dl>` with each `\item` recursively re-parsed.
- **Role-tagged theorems** (`[role=main|supporting|standard|omitted]`)
  with per-proof fold/unfold and a toolbar that bulk-sets fold state by
  role. Default: "all expanded".
- **Paper-like browser viewer** with A4 and dynamic page modes, generated
  page dividers, an Index/Pages side pane, restart and stop/start daemon
  controls, a hideable top toolbar, and a `keys` overlay for LaTeX labels
  in the margin.
- **Selectable SVG MathJax equations**: click an inline or display
  equation to select only that math node; copying returns the original
  LaTeX source instead of SVG text.
- **Inline LaTeX** in titles / theorem names / `\omitref` payloads:
  `\emph`, `\textbf`, `\textit`, `\texttt`, `\textsc`, `\ref`/`\cref`/
  `\eqref`/`\autoref`, and accent commands (`\'e` → é, `\"o` → ö, etc.).
- **LaTeX paragraph semantics**: blank source lines create paragraph
  breaks with indentation, not visible `<br><br>` gaps, including inside
  theorem/proof environments and around display math.
- **Mid-edit guard**: while you're typing inside an open `$$…$$` or
  `\begin{…}` and the buffer is unbalanced, the daemon defers the push.
  Page keeps the last well-formed render instead of flashing a broken one.
- **Incremental MathJax typesetting**: every math node carries a stable
  content hash; the client transplants already-typeset DOM nodes from
  the previous render and only asks MathJax to typeset the actually-new
  expressions. A single-character text edit reuses 100% of math nodes
  (typeset cost ≈ 0 ms) on a 300-equation paper.
- **Preamble caching** + **regex caching** in the parser: a body-only
  edit on a 40 KB paper renders in ~5 ms on the daemon side.
- **Block-level diffing on the wire**: every top-level block is wrapped
  in `<article class="blk" id="blk-N" data-blockhash="…">`. The server
  keeps the last broadcast's block sequence and pushes compact range
  patches as `{event: "patch", ops: [{type: "range", index, remove, html},
  …]}`. A one-character edit inside a paragraph becomes a single-block
  range patch; an inserted paragraph is applied at its child position
  and the browser retags shifted block ids from server metadata. The
  same patch metadata retags inner source anchors for word-level sync.
  The client never touches surrounding blocks or their typeset math. This
  brings end-to-end keystroke latency on a 300-equation paper from
  ~250 ms to single-digit milliseconds for normal text edits.

## Install

mathpreview ships as two pieces that live in this same repo: a Rust
binary (`mathpreview-cli`) and an nvim plugin (`mathpreview.nvim`). The
plugin auto-spawns the binary, so for the standard nvim workflow you
install both, run `:MathPreview` in a `.tex` buffer, and ignore the
daemon thereafter.

### 1. The binary

> **Shortcut:** if you have a Rust toolchain, you can skip this section
> entirely. On first `:MathPreview` with no binary found, the plugin runs
> **`cargo install --path crates/cli`** from its own checkout (you'll get an
> "installing… please wait" notice; ~30s once), dropping `mathpreview-cli` in
> your cargo bin dir (`~/.cargo/bin`, which rustup puts on `$PATH`). It
> reinstalls automatically when the plugin updates ahead of the binary. If
> the install dir isn't on your `$PATH`, the plugin still runs the binary by
> absolute path and warns you once with the line to add (or set `install_root`
> to choose where it goes). This section is for installing the binary yourself
> — no Rust toolchain, or you prefer the tarball.

Pick whichever fits your toolchain:

**Pre-built tarball** (no Rust toolchain needed). Download the matching
archive from the [Releases page](https://github.com/sonv/TexViewer/releases),
extract it, and put `mathpreview-cli` somewhere on your `$PATH`:

```sh
# macOS arm64 example — substitute your platform / version
curl -sSL https://github.com/sonv/TexViewer/releases/download/v0.1.0/mathpreview-cli-v0.1.0-darwin-arm64.tar.gz \
  | tar xz -C /usr/local/bin/
mathpreview-cli --version
```

On first run macOS Gatekeeper will quarantine the binary; either right-click
→ Open in Finder once, or run `xattr -d com.apple.quarantine $(which mathpreview-cli)`.

**`cargo install`** (Rust users): builds from source, picks up the
embedded MathJax + NCM font assets the same way the release binary does.

```sh
cargo install --git https://github.com/sonv/TexViewer mathpreview-cli
```

**Build + install from this checkout** (contributors / the plugin's own path):

```sh
cargo install --path crates/cli --force   # → ~/.cargo/bin/mathpreview-cli (on $PATH)
```

> **Why `cargo install`, not `cargo build`.** `cargo build` only writes to
> `target/release/` — it does **not** put `mathpreview-cli` on your `$PATH`,
> so `mathpreview-cli` won't work in a terminal and other tools can't find
> it. `cargo install --path crates/cli --force` builds *and* drops the binary
> in your cargo bin dir (`$CARGO_HOME/bin`, default `~/.cargo/bin`), which
> rustup keeps on `$PATH`. `--force` lets it overwrite an older install on
> update. To install elsewhere, add `--root <prefix>` (binary lands in
> `<prefix>/bin`), matching the plugin's `install_root` option.
>
> **Binary resolution order** (how the plugin decides which binary to run):
> explicit `cmd` in `setup()` → the cargo-installed binary (by absolute path,
> so it works even if its dir isn't on `$PATH`) → `mathpreview-cli` on
> `$PATH` → a leftover `target/release/` build. `:MathPreviewStatus` shows the
> resolved path, version, and whether the install dir is on `$PATH`; a failed
> daemon spawn prints the binary path plus the daemon's stderr — check those
> first if you ever see the "old version."

### 2. The nvim plugin

The plugin lives at `lua/mathpreview/init.lua` + `plugin/mathpreview.lua`
in this repo. Any plugin manager that puts a checkout's `lua/` and
`plugin/` on `runtimepath` works. Pick the snippet for whatever
manager you already use; if you're not using one, the **Manual
install** path at the bottom of this section works too.

#### lazy.nvim

The minimal version — drop into your `lazy` spec:

```lua
{
  "sonv/TexViewer",
  ft = { "tex", "plaintex", "latex" },
  -- Install/update the binary on every plugin update (Rust toolchain
  -- required). Optional: without it the plugin still auto-installs on first
  -- :MathPreview, but the binary then stays put until the next plugin update
  -- (a version-skew warning nudges you). With it, `:Lazy update` keeps the
  -- binary in lockstep — no skew.
  build = "cargo install --path crates/cli --force",
}
```

The hook (and the auto-install fallback) run `cargo install`, which drops
`mathpreview-cli` in your cargo bin dir (`~/.cargo/bin`, on `$PATH` via
rustup). The `build` hook just moves the install to update time so your first
`:MathPreview` is instant; leaving it off only means the binary is installed
lazily on first use.

The fuller version with lazy-load triggers and an explicit `opts` table:

```lua
{
  "sonv/TexViewer",
  ft  = { "tex", "plaintex", "latex" },
  cmd = { "MathPreview", "MathPreviewStop", "MathPreviewRestart", "MathPreviewStatus", "MathPreviewDebug" },
  -- Install/update the binary on install/update (Rust toolchain required).
  -- With this, you can skip the manual binary install in §1 entirely. Omit it
  -- if you install mathpreview-cli yourself (tarball / cargo install).
  build = "cargo install --path crates/cli --force",
  -- All `opts` keys are optional; the defaults work for the standard case.
  opts = {
    -- Absolute path to the binary if it isn't on $PATH.
    -- cmd = "/usr/local/bin/mathpreview-cli",

    -- Where the auto-install / `build` hook puts the binary. nil = cargo
    -- default (~/.cargo/bin). A prefix like "~/.local" installs to
    -- "~/.local/bin/mathpreview-cli" (passed to `cargo install --root`).
    -- install_root = nil,

    -- Set to false if you don't want :MathPreview to also open a browser tab.
    -- auto_open_browser = true,

    -- Use a CDN-hosted MathJax instead of the embedded bundle. nil = embedded.
    -- mathjax_url = "https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js",

    -- Filetypes that trigger automatic buffer pushes on TextChanged.
    -- filetypes = { "tex", "plaintex", "latex" },

    -- Debounces (ms). The push debounce is the keystroke→render latency
    -- floor; the cursor debounce throttles forward-sync POSTs.
    -- debounce_ms = 40,
    -- cursor_debounce_ms = 80,

    -- Set sync = false to disable cursor/jump bidirectional sync entirely.
    -- sync = true,
  },
  config = function(_, opts)
    require("mathpreview").setup(opts)
  end,
}
```

#### packer.nvim

```lua
use {
  "sonv/TexViewer",
  ft = { "tex", "plaintex", "latex" },
  -- Install/update the binary on install/update (Rust toolchain required).
  -- Omit if you install mathpreview-cli yourself (see §1).
  run = "cargo install --path crates/cli --force",
  config = function()
    require("mathpreview").setup({
      -- See the lazy.nvim block above for the full options list.
      -- mathjax_url = "https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js",
    })
  end,
}
```

#### vim-plug

In your `init.vim` (or wherever your plug block lives):

```vim
" The `do` hook installs/updates the binary on install/update (Rust toolchain
" required); drop it if you install mathpreview-cli yourself (see §1).
Plug 'sonv/TexViewer', { 'do': 'cargo install --path crates/cli --force' }
```

Then in `init.lua` (or a `lua << EOF` block in `init.vim`), if you want
to override any defaults:

```lua
require("mathpreview").setup({
  -- mathjax_url = "https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js",
})
```

Plain `Plug 'sonv/TexViewer'` is enough — `setup()` is only needed for
overrides.

#### Manual install (no plugin manager)

nvim's [native package mechanism](https://neovim.io/doc/user/repeat.html#packages)
loads anything under `~/.config/nvim/pack/*/start/*` at startup.
Clone this repo to that path and nvim picks the plugin up on the
next launch:

```sh
mkdir -p ~/.config/nvim/pack/sonv/start
git clone https://github.com/sonv/TexViewer ~/.config/nvim/pack/sonv/start/mathpreview
# Optional: pre-install so the first :MathPreview is instant. If you skip
# this, the plugin auto-installs on first use (Rust toolchain required).
# Either way the binary lands in ~/.cargo/bin (on $PATH via rustup).
( cd ~/.config/nvim/pack/sonv/start/mathpreview && cargo install --path crates/cli --force )
```

The four `:MathPreview*` commands become available without any
`init.lua` edit. To pin to a release tag instead of tracking `main`:

```sh
cd ~/.config/nvim/pack/sonv/start/mathpreview
git checkout v0.1.0
```

To override defaults, add a `require("mathpreview").setup({ … })` call
to your `init.lua` (any time after nvim startup is fine; the plugin
defers daemon work until you actually run `:MathPreview`).

To update later: `git pull` from inside that directory, then re-run the
`cargo install --path crates/cli --force` step so the binary tracks the
plugin (or just run `:MathPreview` — it reinstalls on detecting skew). To
remove: `rm -rf` it.

#### Updating the binary

The plugin and binary version together (`PLUGIN_VERSION` is bumped in lockstep
with the crate). There are two ways the binary tracks a plugin update:

1. **With a `build` hook** (`cargo install …` in your spec) — your plugin
   manager reinstalls the binary whenever it updates the plugin:

   ```vim
   :Lazy update TexViewer   " pulls new commits AND runs the build hook
   :Lazy build TexViewer    " force the build hook now, without a new commit
   ```

   (packer: `:PackerSync`; vim-plug: `:PlugUpdate sonv/TexViewer`.)

2. **Without a hook** — on the next `:MathPreview`, if the plugin is newer
   than the binary, the plugin reinstalls it automatically (you'll see
   `binary X older than plugin Y — reinstalling…`). It also auto-installs the
   first time when no binary is found.

Either way, confirm what's live with:

```vim
:MathPreviewStatus   " check plugin_version == binary_version, and install_dir
```

then `:MathPreviewRestart` so a running daemon picks up the new binary. If
`install_dir_on_path` is `false`, the binary still works (run by absolute
path) but won't be on your shell `$PATH` until you add that dir.

### 3. Use it

Open any `.tex` file and run:

```vim
:MathPreview
```

The plugin spawns `mathpreview-cli serve <buffer> --port <free>` in the
background, opens your default browser at `http://127.0.0.1:<port>/`,
and starts pushing the buffer on every `TextChanged`. The browser tab
shows the rendered document with the toolbar described in
[Viewer controls](#viewer-controls); `:` opens a vim-style command line
for `:pin`/`:unpin`/`:clear`.

Other commands:

- `:MathPreviewStop` — kill the daemon. (Also fires automatically on
  `VimLeavePre`.)
- `:MathPreviewRestart` — stop, then start. Useful after preamble
  changes the daemon's macro cache misses.
- `:MathPreviewStatus` — echoes daemon PID/port, last push time, push
  counters, and the resolved binary path.
- `:MathPreviewDebug` — echoes the resolved viewer settings, the
  reveal-source `editor_cmd` in effect, and the config / macro paths the
  daemon consulted (with a `*` next to the files that actually exist), so
  you can see what's loaded and where from. Reads the daemon's `/debug`
  endpoint, which you can also open in the browser
  (`http://127.0.0.1:<port>/debug`) for the full JSON including the log.

### CLI directly (no plugin)

If you'd rather skip the plugin (e.g. previewing a paper without nvim
open), invoke the binary directly:

```sh
# Static one-shot HTML — uses jsdelivr MathJax CDN by default.
mathpreview-cli render path/to/paper.tex -o out.html
open out.html

# Live-reload server (default 127.0.0.1:23636). Edits to the file on
# disk trigger re-renders; without the plugin pushing buffers,
# you'll re-render on save rather than per-keystroke.
mathpreview-cli serve path/to/paper.tex
```

### MathJax and offline setup

The default works offline. MathJax 4 (`tex-svg.js`, TeX extensions, and
the New Computer Modern SVG font shards needed by `\boldsymbol` / `\bm`
— ~14 MB total) is **embedded into the binary** via `include_dir!`
along with the four NCM body-text woff2 files. `mathpreview-cli serve`
defaults to:

```sh
--mathjax-url /vendor/mathjax/tex-svg.js
```

and the daemon serves that bundle at
`http://127.0.0.1:23636/vendor/mathjax/...` directly from the embedded
in-memory tree. No `npm install`, no CDN access, no LaTeX install, and
no separate MathJax setup is needed for the HTML/SVG preview, even
after you move the binary somewhere else on disk.

The body prose font (New Computer Modern 10pt — the same family
MathJax's SVG glyphs are generated from, so prose and math share a
typeface) is OFL-1.1 licensed
(`crates/cli/vendor/newcm-text/LICENSE.txt`).

**Loading MathJax from the network instead.** If you'd rather pull
MathJax over the network — for instance to pick up a newer MathJax
release without rebuilding the binary, or because your corporate proxy
caches CDN assets aggressively — pass `--mathjax-url` to the daemon
with any MathJax 4 build URL:

```sh
mathpreview-cli serve path/to/paper.tex \
  --mathjax-url https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js
```

From the nvim plugin, set `mathjax_url` in `setup()`:

```lua
require("mathpreview").setup({
  mathjax_url = "https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js",
})
```

`mathpreview-cli render` is different: it writes a standalone HTML file
intended to be opened directly, so it uses the jsdelivr MathJax CDN by
default (`file://` pages can't hit the daemon's `/vendor/mathjax/`).
For fully offline static HTML, point `--mathjax-url` at a MathJax bundle
served by an HTTP server you control.

The toolbar shows a status pill that reports timing on each update.
The format depends on whether the daemon could send a patch or had to
fall back to a full body re-render.

### Viewer controls

The browser toolbar is intentionally small and all controls work without
a Rust roundtrip unless they are controlling the daemon itself.

- `toc` toggles the left navigation pane. The pane has `Index` and
  `Pages` tabs; the page list is generated from A4/dynamic page dividers.
- `A4` / `dynamic` switches between a fixed A4-ratio sheet that scales
  with the browser width and a wider readable flow layout.
- `keys` toggles LaTeX refkeys for labeled sections, theorem boxes,
  floats, display equations, and loose labels. Visible keys sit in the
  page margin, and multi-row displays show row-level keys.
- `lines` toggles typeset line numbers (LaTeX `lineno`-style): every
  *wrapped* visual line of body text gets a number in the left margin,
  recomputed on render, resize, zoom, and A4 ⇄ dynamic switches. Display
  equations are not numbered (MathJax emits SVG with no text), matching
  `lineno`'s default; a paragraph with inline math still numbers
  normally. Persisted in `localStorage["mathpreview.lineNumbers"]`.
- `margin` toggles a right-hand column of pinned reference cards. With
  margin mode on, clicking a `\ref` or `\cite` link pins the referenced
  theorem / equation / bibliography entry into the margin (typeset
  MathJax preserved) instead of scrolling to the anchor; click again to
  unpin, or use the `×` button on the card. Hovering any `\ref` /
  `\cite` for ~250 ms shows a quick floating preview regardless of
  margin mode — the preview omits proofs so you see the statement
  alone. Cards can be reordered by dragging from the `⋮⋮` grip in
  each card's header (drop indicator is an accent line above or below
  the target card). Two more pin entry points:
  - Click any **refkey chip in the left margin** (the `keys` toggle
    must be on to make the chips visible) — including the per-row
    chips on multi-row `align` / `gather` displays — to pin that
    target without touching the body.
  - Press `:` to open a vim-style command line at the bottom; `:pin
    <key>` pins, `:unpin <key>` removes, `:clear` empties the margin.
    `:p` / `:u` abbreviations work. Tab cycles fuzzy matches in a
    wildmenu strip above the input (substring beats subsequence,
    prefix beats mid-string; `:unpin` narrows to currently-pinned
    keys); ArrowDown/ArrowUp also cycle; clicking a chip commits
    immediately. Esc closes; empty-Backspace also closes.
- `☾` / `☀` toggles dark mode. The choice is persisted in
  `localStorage["mathpreview.theme"]`; on first load the viewer follows
  your OS `prefers-color-scheme`. The toggle re-skins the topbar, side
  panel, paper surface, theorem boxes, refkey chips, margin cards,
  command line, sidenotes, and warnings; MathJax SVG glyphs use
  `currentColor` and follow the body text colour automatically.
- `print` calls `POST /print`. The daemon runs `latexmk -pdf` (falling
  back to `pdflatex` if latexmk isn't on `$PATH`) in the root file's
  directory and streams the produced PDF back as `application/pdf`,
  which the browser opens in a new tab. The output PDF path is read out
  of the latexmk/pdflatex stdout (the "Output written on …" and "All
  targets (…) are up-to-date" lines), so a project that sets `$out_dir`
  in `.latexmkrc` (project-local or `~/.latexmkrc`) — `build/`, `out/`,
  `_artifacts/2026-05/`, anything — is found without configuration. No
  background polling: nothing runs until you click the button.
- `restart` calls `POST /restart`, relaunches the daemon with the same
  command-line arguments, polls until the replacement server is ready,
  then reloads the page.
- `stop` calls `POST /stop`, intentionally exits the daemon, and turns
  into `start`. `start` waits for a daemon to become available again and
  reloads the page.
- `main only` / `+ supporting` / `all` filters proofs by theorem/proof
  role. Proof roles can be inferred from nearby or postponed "Proof of
  ..." headings, or set explicitly with proof options such as
  `\begin{proof}[role=main]`.
- `hide` hides the top banner and persists that preference in the
  browser. A small `toolbar` button appears at the top right to restore
  the banner.
- Vim-style keyboard navigation works in the viewer: `h`/`j`/`k`/`l`
  scroll left/down/up/right, `Ctrl-d` and `Ctrl-u` move by half pages,
  `gg` and `G` jump to the top/bottom, `/` opens search, `n`/`N` move
  between search matches, `:` opens the command line (see `margin`
  above), `t` toggles the index/pages side panel, `B` toggles the top
  banner (keyboard counterpart to the thin stripe), and `Ctrl-o` jumps
  back and forth between the current place and the previous one
  (pressing it repeatedly ping-pongs between the two). These bindings
  are ignored while typing in editable controls.
- **Content zoom.** `+` / `-` zoom the page (header and sidebar stay
  put), `0` resets, and `=` auto-fits the page width to the viewport.
  `Cmd`/`Ctrl` + `+`/`-`/`0` mirror the browser zoom shortcuts but
  only scale the paper. The zoom factor is persisted in
  `localStorage["mathpreview.userZoom"]`.
- **Cmd/Ctrl-click → source.** Modifier-click on any rendered token
  jumps the editor to that source line — the IDE-style "reveal in
  source" gesture. The click fires both `/jump` (which the nvim plugin
  polls and applies *in place*, opening the file in the current window)
  and `/reveal-source` (which spawns `--editor`). With the plugin's
  cursor `sync` on, the in-place `/jump` already handles navigation, so
  the plugin disables the editor spawn to avoid pulling you into a
  second buffer; with `sync` off it passes an `--editor` targeting the
  running nvim via `v:servername`. Override with the plugin's
  `editor = '…'` option (or `--editor` when running `serve` by hand),
  e.g. `code -g {file}:{line}`; a hand-run `serve` defaults to nvim via
  `$NVIM_LISTEN_ADDRESS` or `$NVIM`.
- **Math-only search.** Prefix the `/` query with `m:` (`m:n`,
  `m:\alpha`, `m:α`) or wrap it LaTeX-style (`$n$`, `$\alpha$`) to
  skip body text and only match SVG math glyphs. A single Latin or
  Greek character, or a known `\command`, auto-widens to every
  stylistic variant MathJax may emit — italic, bold, bold-italic,
  script, fraktur, double-struck, sans, sans-bold, sans-italic,
  sans-bold-italic, monospace — so `m:n` reliably finds the italic-`n`
  inside `$n^2$` even though its SVG codepoint is U+1D45B and not
  U+006E. The same widening covers the BMP fallbacks (italic-h at
  U+210E, ℝ at U+211D, ℕ at U+2115, etc.).
- **Search panel layout.** The `/` panel is a two-row grid sitting
  centered against the bottom of the viewport: the `/` label + input
  take the full width on row 1, and the shortcut hint wraps onto
  row 2 so it never squeezes the input. Panel maxes out at 720 px;
  the input is 15 px with a purple focus ring.

**Patch path** (small change, the common case):

```
● 6ms · 1r / typeset 0 (0 math)
```

- **Nr** — count of `replace` ops applied
- **+M** — count of `append` ops (if any)
- **-K** — count of `remove` ops (if any)
- **typeset** — `MathJax.typesetPromise` time on math inside replaced blocks
- **(N math)** — count of math elements that needed fresh typesetting

A single-paragraph text edit on the test paper is consistently `1r /
typeset 0 (0 math)` and lands in 5–10 ms wall clock.

**Full-body path** (more than half the blocks changed at once):

```
● 38ms · idx 3 / parse 22 / diff 9 / swap 2 / typeset 0 (reused 324/324)
```

The daemon falls back to this when a single edit invalidates more
blocks than it's worth patching (e.g. inserting a new section near the
top — every block below shifts position). The client builds the new
body in a detached template, hash-matches math nodes from the live DOM,
transplants the reused ones, swaps `#page` contents in one op, and
re-typesets only the truly-new math.

### Lint the embedded JS

The viewer JavaScript is split across `crates/core/src/assets/client/`
(`header.js` → `viewer.js` → `proof.js` → `patch.js` → `footer.js`,
all engine-neutral, concatenated into one IIFE at compile time by
`renderer/shell.rs`) and `crates/core/src/engines/assets/mathjax.js`
(the MathJax adapter). Everything is pulled into the binary via
`include_str!`, so they are real `.js` files you can lint with ESLint:

```sh
npm install               # one-time
npm run lint
```

This `npm install` is only for linting the embedded viewer JavaScript; it
is not required to run `mathpreview-cli serve`.

The flat config in `eslint.config.js` runs `no-undef` (catches typo'd
identifiers / forgotten renames), `no-unused-vars` (warning),
`no-unreachable`, and dupe-key/dupe-arg checks. Browser globals plus
`window.__mpEngine` are declared so the bundle parses clean. The five
`client/*.js` files share scope through one outer IIFE, so the lint
script concatenates them in `concat!` order and pipes the result to
ESLint via `--stdin` — that lets `no-undef` see all cross-file
references as if it were the original single bundle.

### Inspect what MathJax sees

Mirrors `latex-preview.nvim`'s `:LatexPreview debug`:

```sh
./target/release/mathpreview-cli debug examples/paper.tex
```

Prints the resolved root, included files, MathJax extension list, the full
macro table (name, arity, body, source file), and any warnings about
macros that were filtered out.

### Configure the viewer

User preferences live in TOML — same discovery cascade as the macro
overrides, applied per field with last-wins semantics:

1. Built-in defaults
2. `~/.config/mathpreview/config.toml` (or `$XDG_CONFIG_HOME/...`)
3. `.mathpreview.toml` walking up from the input file
4. `--config <file>` on `serve` or `render` (repeatable)

```toml
# ~/.config/mathpreview/config.toml — applies to every paper
[viewer]
font-size = 18                  # body text size in CSS pixels

[viewer.source-jump]
# Which click gesture sends `POST /reveal-source` to spawn `--editor`
# at the source line. "cmd-click" also matches Ctrl-click on Linux.
trigger = "cmd-click"           # | "ctrl-click" | "alt-click" | "double-click"
```

Drop a `.mathpreview.toml` in the project root to override per-paper:

```toml
# .mathpreview.toml — committed alongside the source
[viewer]
font-size = 20

[viewer.source-jump]
trigger = "double-click"
```

Unknown keys are an error so typos surface immediately instead of
silently doing nothing.

### Override macros for the viewer

Some macro definitions don't translate cleanly to MathJax — typically
anything using `\DeclarePairedDelimiter`, `\xparse`, or
`\NewDocumentCommand`, or anything whose body reaches for `@`-internal
TeX primitives. Drop a plain `\newcommand` replacement into a macros
file and the viewer will use it instead of the source's version.

Cascade (lowest → highest priority — later definitions override earlier
ones by name):

1. Bundled built-ins (`crates/core/src/assets/builtin-macros.tex` — the
   amber-printed unmapped-package warnings hint at which macros need
   coverage; the bundled file is what's already provided).
2. The paper's own preamble macros.
3. `~/.config/mathpreview/macros.tex` (or
   `$XDG_CONFIG_HOME/mathpreview/macros.tex`) — your personal overrides
   applied to every paper.
4. `.mathpreview-macros.tex` walking up from the input file — repo-
   specific overrides that ship alongside the source.
5. `--macros <file>` on the `serve` or `render` subcommand (repeatable).

A typical file:

```tex
% .mathpreview-macros.tex — markdown-friendly approximations
\newcommand{\st}{\mid}
\newcommand{\set}[1]{\{#1\}}
\newcommand{\given}{\mid}
```

You can also add overrides without leaving the viewer: click the
`macros` button in the toolbar. The chosen scope's existing
`\newcommand` file **loads into the editor** so you can see and edit
what's already there; add or change lines, pick **Project** or
**Global**, and Save — the daemon validates the lines and writes the
file back (so re-saving never duplicates), then the page re-renders.
(A *Type* toggle switches to **Text → HTML** for writing a
`[text-macros]` template instead — see [Macros in regular
text](#macros-in-regular-text).) Edits to the file made directly in
your editor live-reload the same way — the file watcher tracks all
override paths.

The override's signature has to match how the macro is called in the
body. `\DeclarePairedDelimiter[..size..]{..body..}` calls become plain
`\set{..body..}` calls if you express the override as
`\newcommand{\set}[1]{...}` — the optional `[size]` argument is
silently dropped (an "approximate output" tradeoff).

### Macros in regular text

Math macros are expanded by MathJax inside `$…$`. In **regular text**, the
renderer also handles macros, in three ways:

1. **Your `\newcommand`s expand in text.** A macro defined in the preamble,
   in a **local `\usepackage`'d / `\input`'d `.sty` or `.tex`** (these are
   scanned — see [Override macros](#override-macros-for-the-viewer)), or in
   any override file is substituted with its body, arguments and all, then
   re-rendered — so `\newcommand{\hello}{world}` makes `\hello` render as
   *world*, and `\newcommand{\SV}[1]{\textcolor{red}{#1}}` makes `\SV{note}`
   render as a red *note*. (Previously, unknown text macros were dropped.)
   Only `\newcommand`-style definitions are picked up; `\def`,
   `\DeclareRobustCommand`, `\NewDocumentCommand`, etc. are not — use the
   `[text-macros]` table for those.
2. **Built-in `\textcolor`.** `\textcolor{red}{x}` → a colored span;
   `\textcolor[HTML]{FF8800}{x}` uses a hex color. Color names pass through to
   CSS. (The `\color{…}` *switch* form isn't supported yet — use `\textcolor`,
   which wraps its argument.)
3. **The `[text-macros]` config table — for macros expansion can't reach.**
   For a command defined with `\def` / `\NewDocumentCommand` /
   `\DeclarePairedDelimiter` (not extracted), one from a system package that
   isn't a local file on disk, or just for a preview-only look, map it to an
   **HTML** template (not TeX — TeX-valued macros go in `macros.tex`). Keys are
   command names (with or without a leading `\`); `#1`..`#9` are filled by the
   rendered arguments. Lives in the same `.mathpreview.toml` cascade as
   `[viewer]`. Each value is either a **string** template or, MathJax-style, an
   **array** `[template, n_args, default]` to set the argument count and an
   optional first-argument default explicitly:

   ```toml
   # .mathpreview.toml
   [text-macros]            # `[text_macros]` is accepted too
   hello = "world"                                              # 0 args
   SV    = '<span class="margin-note" style="color:red">#1</span>'  # 1 arg (inferred)
   nb    = '<mark>#1</mark>'
   # [template, n_args, default-of-#1] — like MathJax's tex.macros:
   hl    = ['<mark style="background:#1">#2</mark>', 2, 'yellow']
   #   \hl{x}        -> yellow background   (uses the default)
   #   \hl[pink]{x}  -> pink background     (overrides the optional 1st arg)
   ```

   With a string value the argument count is **inferred** from the highest
   `#n`; the array form sets `n_args` (and the `default`) explicitly, which is
   how you get an optional first argument. A `[text-macros]` entry overrides a
   `\newcommand` of the same name. The template HTML is emitted as-is (it's
   your own local config), and the arguments are rendered through the normal
   pipeline (so math/emphasis inside
   them work and are escaped).

**What's handled in regular text:**

| LaTeX | Result |
|---|---|
| `\emph{x}`, `\textit{x}`, `{\em x}`, `{\it x}` | italic |
| `\textbf{x}`, `{\bf x}` | bold |
| `\texttt{x}`, `{\tt x}` | monospace |
| `\textsc{x}`, `{\sc x}` | small caps |
| `\textcolor{name}{x}`, `\textcolor[HTML]{RRGGBB}{x}` | colored span |
| `\ref` / `\cref` / `\Cref` / `\autoref` / `\eqref` / `\pageref` | resolved cross-reference link |
| `$ … $` | inline math (MathJax) |
| `\'e \`a \"o \^o \~n \=a \.z` | accented letters |
| `~` | non-breaking space; `\\` | line break; `\, \; \: \!` | (thin spaces, dropped) |
| your `\newcommand` (preamble or local `.sty`) | expanded with its arguments |
| a name in `[text-macros]` | your HTML template |
| any other `\foo{bar}` | `bar` shown, `\foo` dropped |
| any other `\foo` (no arg) | dropped |

**Not handled:** the `\color{…}` switch form (use `\textcolor`); macros defined
with `\def` / `\NewDocumentCommand` / `\DeclarePairedDelimiter` (use
`[text-macros]`); and arbitrary layout. It's a fast-preview approximation, not
a TeX engine — a macro whose body is pure math used in *text* renders crudely;
keep those in math mode or give them a `[text-macros]` template.

#### Map a macro to HTML

To give any command a preview rendering — including one defined with `\def`,
shipped by a package, or that you simply want to look different in the preview:

1. Open (or create) `.mathpreview.toml` in your project root (or
   `~/.config/mathpreview/config.toml` to apply everywhere).
2. Add a `[text-macros]` table. Each key is the command name **without** the
   leading backslash; the value is an HTML template. Use `#1`, `#2`, … for the
   command's arguments (they're rendered and HTML-escaped before substitution):

   ```toml
   [text-macros]
   # \hello              -> world
   hello = "world"
   # \SV{some text}      -> red inline note (a margin macro shown inline)
   SV = '<span style="color:red">#1</span>'
   # \todo{fix this}     -> highlighted
   todo = '<mark>#1</mark>'
   # \keyword{X}{Y}      -> two args
   keyword = '<b>#1</b> (<i>#2</i>)'
   ```

3. Save. The preview reloads and applies it immediately (no restart). An entry
   here overrides a `\newcommand` of the same name, so it's also the way to make
   the preview *differ* from the PDF on purpose.

##### From the toolbar (no TOML knowledge needed)

You don't have to hand-edit the file. Click the **macros** button in the
toolbar and flip the *Type* toggle to **Text → HTML**. The chosen scope's
config TOML loads into the editor on the right, and you have **two ways** to
add a mapping:

- **The quick-add form** (top) — type a **command name** and an **HTML
  template**, then click **Add ↓**. It builds a correct `[text-macros]` line
  (quoting the template for you) and inserts it into the editor under a
  `[text-macros]` table, creating the table if needed. Good when you don't
  know the TOML syntax.
- **The editor** (below) — type or tweak `[text-macros]` lines directly. The
  Add form just writes into this same box.

Either way, click **Save**: the daemon validates the whole file parses as TOML,
writes it back, and re-renders immediately (no restart). Pick the file with the
**Project / Global / Custom** tabs on the left. (The default *TeX macro* toggle
edits the `\newcommand` override file instead — see [Override
macros](#override-macros-for-the-viewer).)

##### By hand

Editing `.mathpreview.toml` in your own editor works identically (the daemon
live-reloads it). Use single-quoted TOML strings so backslashes/quotes in the
HTML are literal. The template HTML is emitted as-is (it's your own local file,
same trust level as a vimrc); only the `#n` arguments are escaped. If a command
takes an optional `[…]` argument, set it via the array form's `default` (see
above) rather than relying on bracket parsing.

> **Don't need raw HTML?** If your mapping is expressible as LaTeX — e.g.
> `\SV{x}` → red text — you don't need the TOML table at all. In the **macros**
> button keep the *TeX macro* toggle (or edit `.mathpreview-macros.tex`) and add
> `\newcommand{\SV}[1]{\textcolor{red}{#1}}`. Those `\newcommand` overrides now
> apply to body text too, so it renders inline. Use **Text → HTML** when you
> want literal HTML or the command isn't a `\newcommand`.

## nvim setup

The plugin lives at `lua/mathpreview/init.lua` + `plugin/mathpreview.lua`
in this repo. Any plugin manager that adds a checkout's `lua/` and
`plugin/` directories to `runtimepath` works — see [Install](#install)
above for lazy.nvim / packer snippets.

The four commands `plugin/mathpreview.lua` registers on startup:

| Command | What it does |
| --- | --- |
| `:MathPreview` | Spawn the daemon for the current buffer on the first free port in `23636..23651`. Open the browser tab. Attach `TextChanged` / `CursorMoved` autocmds and start the `/jump` poll. If the daemon is already running, just reopen the browser tab. |
| `:MathPreviewStop` | Kill the daemon, detach autocmds, stop the poll. Also fires from `VimLeavePre`. |
| `:MathPreviewRestart` | Stop, then start after a 200 ms grace period (so the OS can release the port). Handy after preamble changes the daemon's macro cache misses. |
| `:MathPreviewStatus` | `print(vim.inspect(...))` of the runtime state: PID/port, root file, push and cursor counts, last error, resolved binary path, nvim version. |
| `:MathPreviewDebug` | Fetch the daemon's `/debug` and print the resolved viewer settings, the reveal-source `editor_cmd`, and the config / macro paths consulted (`*` marks files that exist). Shows what's loaded and from where. |

The daemon takes the root file's path on its command line and walks the
project from there. The plugin then POSTs the *current buffer's* path on
every `TextChanged` via `X-Mathpreview-Path`, and the daemon splices it
in as an in-memory override against the real root project — so editing
a `\input{chapter1}` child file updates the rendered root document
without writing to disk. nvim 0.10+ uses `vim.system` + `vim.uv`; older
versions fall back to `jobstart` + `vim.loop`.

`CursorMoved` / `CursorMovedI` POST the current source position to
`/cursor`. The browser scrolls to and flashes the nearest rendered
word/math/ref element. It does not scroll while that element is between
25% and 75% of the viewport; once it leaves that band, it lands the
element at the 25% line. To jump the other direction, double-click or
Alt/Cmd-click rendered content in the browser; the plugin polls `/jump`
and moves the editor cursor to that source location. Blank
paragraph-separator lines inside theorem/proof environments also have
invisible source anchors, so placing the cursor on an empty line syncs
to that whitespace instead of jumping to the top of the enclosing
environment.

**`setup()` is optional.** The defaults in `lua/mathpreview/init.lua`
are fine for the standard case. Override only if you need to:

```lua
require("mathpreview").setup({
  cmd = "/usr/local/bin/mathpreview-cli", -- non-$PATH binary location
  auto_open_browser = false,              -- skip the browser launch on :MathPreview
  -- nil = use the binary's embedded MathJax bundle (offline default).
  -- Set to any MathJax 4 build URL to load over the network instead —
  -- the plugin forwards it to the daemon as --mathjax-url.
  mathjax_url = "https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js",
  filetypes = { "tex", "plaintex", "latex" },
  debounce_ms = 40,
  cursor_debounce_ms = 80,
  jump_poll_ms = 120,
  sync = true,                            -- false to disable cursor/jump roundtrip
})
```

**Troubleshooting.** If `:MathPreviewStatus` shows `daemon_running =
false` after `:MathPreview`, check `:messages` for the spawn error
(usually "binary not found" — set `cmd` in `setup()` to an absolute
path). If `last_error` is set, that's curl's view of the daemon (port
mismatch, daemon crashed, etc.); `:MathPreviewRestart` usually fixes it.

## How it stays fast

The naive "re-render and replace innerHTML" path was ~340 ms for a
300-equation paper, dominated by MathJax re-typesetting every expression
on every keystroke. Three optimizations bring it under 10 ms:

1. **Preamble cache.** The daemon hashes the preamble source and skips
   the macro-extraction + `.bib`-load pipeline when it's unchanged
   (which is every body edit). ~30 ms saved per push.
2. **Regex cache.** Parser regexes live in `LazyLock<Regex>` statics so
   they compile once per process, not once per theorem env. ~70 ms saved
   on a body parse.
3. **Incremental MathJax.** Each math node carries `data-hash`. On a
   buffer push, the client builds the new HTML in a detached `<div>`,
   transplants existing typeset nodes by hash match, and only calls
   `MathJax.typesetPromise` on the actually-changed ones. ~280 ms saved.

Per-stage timing is logged on stderr for every push:

```
mathpreview: buffer-push 39781b → total 5 ms
  (parse 0, preamble 0, body-parse 1, number 0, render 2; cache hit)
```

## Layout

```
crates/core             parser + macro extractor + numbering + renderer (the library)
  ├ engines/            MathEngine trait + Engine enum (dispatch)
  │   ├ mathjax.rs        MathJaxEngine: head config, adapter shim, extra CSS
  │   └ assets/           engine-specific frontend bits (window.__mpEngine, mjx CSS)
  ├ renderer.rs         AST → HTML dispatcher + RenderCtx + render_inline_latex
  ├ renderer/           focused submodules called by the dispatcher above
  │   ├ util.rs           escape/sanitize/role helpers, latex token parsing
  │   ├ shell.rs          wrap_in_shell + warnings panel + CLIENT_JS concat!
  │   ├ math.rs           math rows, eq numbers, refs, floats, includegraphics
  │   └ bib.rs            bibliography formatting (numeric / author-year)
  └ assets/             shared engine-neutral frontend bundle
      ├ client/           five .js pieces sharing one IIFE; concat! in shell.rs
      │   ├ header.js       state + DOM/scroll/vim-pending helpers + IIFE open
      │   ├ viewer.js       search, vim nav, side panel, margin cards, layout
      │   ├ proof.js        theorem-role detection, applyMode, server controls
      │   ├ patch.js        math sel/copy, event delegation, applyPatch, typeset queue
      │   └ footer.js       WebSocket + bootstrap + IIFE close
      └ default.css       page stylesheet
crates/cli              mathpreview-cli binary (render / debug / serve)
  ├ vendor/mathjax/     trimmed MathJax 4 (tex-svg) served at /vendor/mathjax/*
  └ vendor/newcm-text/  NewCM 10pt woff2 body font served at /vendor/newcm-text/*
lua/mathpreview/        nvim plugin implementation (start/stop/restart/status)
  └ init.lua              daemon-spawn, port scan, browser launch, push/poll
plugin/                 auto-sourced by nvim — registers :MathPreview commands
  └ mathpreview.lua       command stubs that lazy-require lua/mathpreview
examples/               demo .tex paper + companion .sty
scripts/
  ├ vendor-mathjax.sh     refresh vendor/mathjax/ from npm
  └ vendor-newcm-text.sh  refresh vendor/newcm-text/ from npm
CHANGELOG-GPT.md        codex session changelog
CHANGELOG-claude.md     claude session changelog
DESIGN.md               full design document
```

The `client/*.js` and `default.css` files are pulled into the Rust binary via
`include_str!` (`client/` files concatenated via `concat!` in `renderer/shell.rs`),
so editing them is just editing the file — no Rust string escaping, full editor
support, and lint-friendly.

## Roadmap

Ordered roughly by what we plan to tackle next. ◯ marks queued items.
See `DESIGN.md` for the full backlog.

- **✓ Split client.js and renderer.rs.** `renderer.rs` (~4300 lines)
  split into `renderer/{util,shell,math,bib}.rs` plus a thinner
  dispatcher (~2800 lines). `client.js` (~2300 lines) split into five
  scope-sharing pieces under `assets/client/` (`header → viewer →
  proof → patch → footer`) concatenated by `concat!(include_str!(…),
  …)` in `renderer/shell.rs`. ESLint runs through `--stdin` on the
  assembled bundle so `no-undef` still catches typos across files.
- **✓ Multi-buffer chapter editing.** `POST /buffer` accepts the root file
  or any watched included `.tex` file. The daemon stores pushed buffers in
  memory, re-renders the real root, and splices overrides through
  `\input` / `\include` / `\subfile` without touching disk.
- **◯ Nested margin-card expansion.** Clicking a `\ref` inside an
  already-pinned margin card should open a child card indented underneath,
  preserving the dependency trail. Closing a parent closes its children.
- **◯ Trim vendored MathJax further.** Current vendor bundle is ~13 MB
  after keeping the New Computer Modern SVG font shards needed by
  `\boldsymbol` / `\bm` and removing alternate output / input engines.
  The SVG font shard set could be further audited against actual usage.
- **◯ SyncTeX-precision source sync.** Editor-cursor ↔ preview jumps work
  at source-word granularity for prose and element granularity for math
  and refs. Full SyncTeX-style precision for exact display rows or
  glyph-level positions is still future work.
- **◯ CI: GitHub Actions.** `cargo fmt --check`, `cargo clippy -D
  warnings`, `cargo test`, `npm run lint`, and a headless nvim plugin
  smoke test. No `.github/workflows/` today.
- **◯ Browser-level interaction tests.** Today's regression coverage is
  cargo tests + eslint. A small headless-browser harness (Playwright) on
  the rendered shell would catch CSS regressions, toolbar interactions,
  and cross-tab WS reconnect flows that the unit tests can't see.

## What's not done yet

- **Nested margin-card expansion + popup mode.** First-pass margin
  cards (click-to-pin + hover preview) work, but clicking a `\ref`
  *inside* an already-pinned card doesn't yet open a child card, and
  there's no floating-popup alternative to the right-margin column.
- **Tauri shell as a native window option.** WebSocket was picked as the
  starting transport because it decouples backend from frontend — not as
  a permanent rejection of Tauri. The intended destination is to add a
  `crates/app/` Tauri binary that uses the same `mathpreview-core`
  underneath, with the existing `mathpreview-cli serve` remaining as the
  headless option for browser-tab users. See DESIGN §11 step 7 for the
  migration sketch.
- **Second rendering engine.** The `MathEngine` trait and the
  engine-neutral `window.__mpEngine` shim are in place; today the only
  implementation is `MathJaxEngine`. Adding e.g. a `PdfjsEngine` (page
  images from a real TeX compile) or a `TexpressoEngine` (driver mode)
  is a new variant on the `Engine` enum plus a `MathEngine` impl, no
  changes to `renderer.rs`. See `crates/core/src/engines/mod.rs` for the
  contract.
