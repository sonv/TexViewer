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
  highlight the matching rendered word/math/ref element, a visual-mode
  selection tints the whole matching region in the preview, and
  double-click or Alt/Cmd-click in the browser can jump nvim back to the
  source line.
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
- **Numbering driven by your `\newtheorem` declarations** — counters
  (shared vs independent), reset level, and titles, including custom and
  `.sty`-declared environments — plus sections, equation envs, multi-row
  `align`/`gather`, and `subequations` (alphabetic child suffixes). Falls
  back to the AMS-modern default when nothing is declared.
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
- **Macros in regular text**: your `\newcommand`s (from the preamble or a
  local `.sty`) expand in body text, not just math — plus a built-in
  `\textcolor` and a config-driven `[text-macros]` HTML-template table for
  commands the previewer can't otherwise see. See [Macros in regular
  text](#macros-in-regular-text).
- **Equation wrapping** of long display math, on by default (MathJax
  line-breaking) and toggleable, plus a raw `mathjax-config` escape hatch
  to override any MathJax option. See [Configure the
  viewer](#configure-the-viewer).
- **Fast incremental updates**: each math node carries a content hash, so
  already-typeset SVG is transplanted and only genuinely new expressions
  are typeset; the parser caches the preamble; and the server diffs
  top-level blocks and pushes compact range patches over the WebSocket.
  Net: single-digit-millisecond keystroke latency on a 300-equation paper
  (a naive full re-render is ~250 ms). See [How it stays
  fast](#how-it-stays-fast).

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
- `margin` toggles a right-hand column of pinned reference cards: with it
  on, clicking a `\ref`/`\cite` pins the referenced theorem/equation/bib
  entry (typeset math preserved) instead of scrolling to it — click again
  or the `×` to unpin, drag the `⋮⋮` grip to reorder. Hovering a
  `\ref`/`\cite` shows a quick, proof-less preview regardless of mode. You
  can also pin from a left-margin refkey chip (needs `keys` on) or the `:`
  command line (`:pin`/`:unpin`/`:clear`, with Tab fuzzy-completion).
- `☾` / `☀` toggles dark mode. The choice is persisted in
  `localStorage["mathpreview.theme"]`; on first load the viewer follows
  your OS `prefers-color-scheme`. The toggle re-skins the topbar, side
  panel, paper surface, theorem boxes, refkey chips, margin cards,
  command line, sidenotes, and warnings; MathJax SVG glyphs use
  `currentColor` and follow the body text colour automatically.
- `print` runs `latexmk -pdf` (or `pdflatex`) in the project directory and
  opens the produced PDF in a new tab. It reads the output path from the
  build log, so a custom `$out_dir` in `.latexmkrc` is found
  automatically. Nothing runs until you click it.
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
- **Cmd/Ctrl-click → source.** Modifier-click any rendered token to jump
  the editor to that source line. Under the nvim plugin this navigates in
  place; for other editors set the plugin's `editor = '…'` option (or
  `--editor` on a hand-run `serve`), e.g. `code -g {file}:{line}`.
- **Math-only search.** Prefix the `/` query with `m:` (or wrap it like
  `$\alpha$`) to match only SVG math glyphs, skipping body text. A single
  letter or `\command` auto-widens across MathJax's style variants
  (italic, bold, script, …), so `m:n` finds the italic-`n` in `$n^2$`.
- **Fuzzy search with word suggestions.** The `/` search is typo-tolerant by
  default — each query word matches document words within a small edit distance
  (including transpositions), so `coagualtion` finds `coagulation` and
  `gaint clustr` finds `giant cluster`. As you type, a strip of matching
  document words appears above the box; `Tab` or `↑`/`↓`+`Enter` completes to
  one (or click it). Cycling and the current/total counter work as usual.
  Prefix `m:` or `$…$` to search math instead; use the browser's **Cmd/Ctrl+F**
  for exact literal search.

The status dot in the toolbar reports each update — e.g. `● 6ms · 1r /
typeset 0` for a one-block patch, or a fuller `parse / diff / swap /
typeset (reused N/N)` line on the rare full-body rebuild. Normal text
edits stay on the single-digit-millisecond patch path.

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
wrap-equations = true           # wrap long display math (MathJax line-breaking);
                                # set false to let it overflow + scroll instead
theorem-numbering = "auto"      # | "continuous" | "section" — see below
typeset-mode = "local"          # | "background" — see below

[viewer.source-jump]
# Which click gesture sends `POST /reveal-source` to spawn `--editor`
# at the source line. "cmd-click" also matches Ctrl-click on Linux.
trigger = "cmd-click"           # | "ctrl-click" | "alt-click" | "double-click"
```

`wrap-equations` toggles MathJax's automatic line-breaking of long display
equations in the preview. Default `true` (the preview wraps overlong math at
low-priority operators so it fits the column). Set `false` if you'd rather the
math overflow and scroll horizontally — closer to how a non-`breqn` PDF
typesets it on one line. It's also a checkbox in the **config** toolbar dialog.
This affects the **preview only** — it can't change how `latexmk` breaks lines
in the PDF.

`typeset-mode` controls how much of the document is typeset (has its math
rendered) at once — it's also a dropdown in the **config** toolbar dialog.
Default `local` typesets only the region around the viewport plus a small
buffer, leaving the rest until you scroll to it; this keeps memory and CPU
low on a long paper. `background` typesets the visible region first, then
quietly fills in the rest while the tab is idle, so scrolling to deep sections
and printing never wait (at the cost of typesetting — and holding in memory —
the whole document). Either way, **Cmd/Ctrl+P** typesets the whole document on
demand before printing.

**Theorem numbering.** By default (`theorem-numbering = "auto"`) theorem-likes
are numbered from the document's `\newtheorem` declarations — continuously
(Theorem 1, 2, 3…) for a bare `\newtheorem{theorem}{Theorem}`, or per-section
(1.1, 1.2, 2.1…) when it carries `[section]`. Set `"continuous"` or `"section"`
to force one scheme regardless. Use it when the declarations aren't visible to
the viewer — e.g. inside a conditional `\if…\newtheorem…\else…\fi` block (which
the viewer can't evaluate, so it falls back to a default) or a package it can't
resolve. Put it in the project's `.mathpreview.toml` so it applies to that paper
only; it takes effect on the next render (save the file, or `:MathPreviewRestart`).

**Raw MathJax config.** For anything `wrap-equations` doesn't cover, drop a
snippet of JavaScript in `mathjax-config`; it runs right after the generated
`window.MathJax = {…}` and before the library loads, so you can override any
MathJax option. **Mutate** `window.MathJax` — don't reassign it (the client
relies on the generated config). A TOML multi-line literal string keeps the JS
readable:

```toml
[viewer]
mathjax-config = '''
window.MathJax.svg.displayOverflow = 'scroll';   // e.g. scroll instead of wrap
window.MathJax.tex.macros.RR = '\\mathbb{R}';     // add a macro
window.MathJax.tex.packages['[+]'].push('color'); // load an extra package
'''
```

Both `wrap-equations` and `mathjax-config` live in the MathJax `<head>`, so a
change reloads the preview tab automatically (the daemon signals it) — however
you edit them: the **config** toolbar dialog (a checkbox for wrapping and a
"MathJax config (advanced)" section with a read-only view of the full generated
config plus a box for your override JS), the `.mathpreview.toml` file, or the
macros dialog's Text→HTML editor. The read-only view shows everything in effect
— macros, packages, output options — so you can see what to mutate.

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
| `:MathPreview` | Spawn the daemon for the current buffer on the first free port in `23636..23651`. Open the browser tab. Attach `TextChanged` / `CursorMoved` / `ModeChanged` autocmds and start the `/jump` poll. If the daemon is already running, just reopen the browser tab. |
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

Selecting a region in **visual mode** highlights the matching region in
the preview. As you extend the selection, the plugin POSTs the source
range to `/selection`; the daemon resolves it — server-side, via the same
sync index — to every rendered element the range overlaps and the browser
tints them as one continuous block. Leaving visual mode clears the
highlight. Linewise (`V`) covers whole rows and blockwise (`Ctrl-V`) is
treated as its bounding rectangle. This reuses the `sync` and
`cursor_debounce_ms` settings — no extra configuration.

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
  sync = true,                            -- false to disable cursor/jump roundtrip
  -- Browser → editor source-jumps use a long-poll (one parked request),
  -- so an idle preview costs ~no CPU. These tune that loop; defaults are fine.
  jump_wait_ms = 25000,                   -- how long the daemon holds an idle /jump
  jump_retry_ms = 1000,                   -- back-off before re-parking after empty/err

  -- raise_on_jump (default true): bring nvim's window to the front on a
  -- source-jump — the focus a PDF viewer gives you via SyncTeX. Best-effort
  -- and platform-aware. On Linux/BSD it raises THIS nvim's own window by
  -- walking up the process tree from nvim's PID (nvim → shell → terminal) and
  -- focusing the ancestor that owns a window, so two terminals each running
  -- nvim+mathpreview raise the right one. macOS uses `osascript … activate`.
  -- Set false to stop focus-stealing.
  raise_on_jump = true,

  -- Linux class / app_id used only as the FALLBACK when the PID walk above
  -- can't find nvim's window. Default "nvim" matches nvim-qt / Neovide; for
  -- TERMINAL nvim set your terminal's class (e.g. "kitty", "foot",
  -- "Alacritty"). Ignored on macOS.
  jump_window = "nvim",

  -- on_jump: extra hook run AFTER a Cmd/Ctrl-click has moved nvim's cursor
  -- (and after the built-in raise). Use it when raise_on_jump can't reach
  -- your setup (e.g. GNOME/Mutter). jump = { file, line, col }.
  -- on_jump = function(jump) ... end,
})
```

> **Source-jump focus** (Cmd/Ctrl-click in the preview → jump to that spot in
> nvim) raises the editor automatically via `raise_on_jump`:
> - **macOS** — detects your terminal (`Terminal`, `iTerm`, `WezTerm`,
>   `Ghostty`, kitty, Alacritty) or GUI (`Neovide`, `nvim-qt`); inside tmux it
>   falls back to `$LC_TERMINAL`.
> - **Linux/BSD** — raises *this* nvim's own window by PID: it walks up the
>   process tree from nvim and focuses the ancestor that owns a window, via
>   Hyprland (`hyprctl`), Sway (`swaymsg`), KDE (`kdotool`), or X11 (`xdotool`).
>   Install the matching tool. So two terminals each running nvim+mathpreview
>   raise the right one.
>
> `jump_window` is only the fallback when the PID walk finds no window. **GNOME/
> Mutter** has no general activation API — use `on_jump` with a shell-extension
> bridge. The cursor jump itself always works regardless.

### Source-jump focus (raise the editor)

Cmd/Ctrl-click (or double-click) a rendered token in the preview and the
plugin moves nvim's cursor to that `(file, line, col)`. Moving the cursor is
cross-platform; **bringing nvim's window to the front** is not — so the
built-in `raise_on_jump` (default `true`) does it the way each platform
allows. This is the SyncTeX focus PDF viewers give you.

On Linux/BSD the plugin raises **this nvim's own window**, not just any window
of a given class: it starts from nvim's PID and walks up the process tree
(`nvim → shell → terminal`), raising the first ancestor that actually owns a
window. So if you have two terminals each running nvim+mathpreview, a click
raises the correct one. `jump_window` (below) is only the fallback used when
the PID walk can't find a window.

| Environment | How it raises (by PID) | Needs installed |
| --- | --- | --- |
| **macOS** | `osascript … activate` on the detected terminal (`Terminal`, `iTerm`, `WezTerm`, `Ghostty`, kitty, Alacritty) or GUI (`Neovide`, `nvim-qt`) | built in (tmux: `$LC_TERMINAL` fallback) |
| **Hyprland** | `hyprctl dispatch focuswindow pid:<ancestor>` | `hyprctl` (ships with Hyprland) |
| **Sway / wlroots** | `swaymsg [pid=<ancestor>] focus` | `swaymsg` (ships with Sway) |
| **KDE/KWin (Wayland)** | `kdotool search --pid <ancestor> … windowactivate` | `kdotool` |
| **X11** | `xdotool windowactivate $WINDOWID`, else `xdotool search --pid <ancestor>` | `xdotool` |
| **GNOME/Mutter** | no general activation API — use `on_jump` | — |

Each falls back to matching `jump_window` by class/app_id if the PID walk
turns up nothing.

**Using it:**

1. It's on by default. After `:MathPreviewRestart`, a preview click should
   focus nvim — provided the matching CLI above is installed.
2. No per-window config is needed — PID targeting finds your window
   automatically. `jump_window` only matters as the fallback; set it to your
   terminal's class for terminal nvim (e.g. `jump_window = "kitty"`, `"foot"`,
   `"Alacritty"`, `"org.wezfurlong.wezterm"`), or leave the `"nvim"` default
   for GUI nvim (Neovide / nvim-qt).
3. To find your class for that fallback: focus the terminal and run `kdotool
   getactivewindow getwindowclassname` (KDE), `hyprctl activewindow` (Hyprland,
   see the `class:` line), or `swaymsg -t get_tree` (Sway, find the focused
   node's `app_id`).
4. Set `raise_on_jump = false` to stop the focus change on every click.

On macOS the first activation may trigger a one-time **Automation** permission
prompt ("… wants to control …"); approve it once. macOS raises the terminal
*app* (not a specific window/tab — PID targeting there is per-app and harder).
On compositors not covered above (or GNOME), drop in an `on_jump =
function(jump) … end` hook — it runs right after the cursor moves.

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
