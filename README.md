# mathpreview

A live, browser-based preview server for LaTeX papers — keystroke-level
updates, no PDF roundtrip, no LaTeX engine on the user's machine.

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

**Build from this checkout** (contributors):

```sh
cargo build --release -p mathpreview-cli
# binary at target/release/mathpreview-cli
```

### 2. The nvim plugin

Point your plugin manager at this repo. With **lazy.nvim**:

```lua
{
  "sonv/TexViewer",
  ft = { "tex", "plaintex", "latex" },
  cmd = { "MathPreview", "MathPreviewStop", "MathPreviewRestart", "MathPreviewStatus" },
  -- optional: only needed if mathpreview-cli isn't on $PATH or you
  -- want to disable browser auto-open / change debounces.
  -- opts = { cmd = "/usr/local/bin/mathpreview-cli", auto_open_browser = true },
}
```

With **packer.nvim**:

```lua
use { "sonv/TexViewer", ft = { "tex", "plaintex", "latex" } }
```

The plugin registers four commands; no `require("mathpreview").setup()`
is needed unless you want to override defaults.

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

For the live viewer, MathJax works locally out of the box. The repository
vendors MathJax 4's `tex-svg.js`, the TeX extensions used by real papers,
and the New Computer Modern SVG font shards needed by `\boldsymbol` /
`\bm` under `crates/cli/vendor/mathjax/`. `mathpreview-cli serve`
defaults to:

```sh
--mathjax-url /vendor/mathjax/tex-svg.js
```

and the daemon serves that bundle at
`http://127.0.0.1:23636/vendor/mathjax/...`. For normal use, the install
path is just: build the Rust binary, run `serve`, open the browser. No
`npm install`, CDN access, LaTeX install, or separate MathJax setup is
needed for the HTML/SVG preview.

The body prose font (New Computer Modern 10pt — the same family
MathJax's SVG glyphs are generated from, so prose and math share a
typeface) is vendored too: four woff2 files under
`crates/cli/vendor/newcm-text/woff2/`, served at
`http://127.0.0.1:23636/vendor/newcm-text/...`. Bundled, not installed:
no system font install is required. These four woff2 files are pulled
into the binary via `include_bytes!`, so the release executable serves
them without needing the surrounding source checkout at runtime — useful
if you ship just the binary somewhere. (MathJax itself is too large to
embed and still reads from `vendor/mathjax/` on disk.) The font is
OFL-1.1 licensed (`crates/cli/vendor/newcm-text/LICENSE.txt`).

`mathpreview-cli render` is different: it writes a standalone HTML file
intended to be opened directly, so it uses the jsdelivr MathJax CDN by
default. For fully offline preview, prefer `serve`. If you do want a
static offline HTML setup, pass `--mathjax-url` to a MathJax bundle that
your own HTTP server also serves; a `file://` HTML file cannot load
`/vendor/mathjax/...` unless a local server is providing that path.

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
  above), and `Ctrl-o` returns to the previous recorded place. These
  bindings are ignored while typing in editable controls.

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
