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
- **nvim integration** via a small Lua file: `TextChanged` autocmd → curl
  POST → daemon. No disk writes, no git pollution.
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
- **Numbering** for sections, theorem-likes (shared counter scoped to
  section, AMS-modern style), and equation envs.
- **Cross-references** resolve to their friendly form: `\cref{thm:main}`
  becomes "Theorem 2.1", `\eqref{eq:foo}` becomes "(3.1)".
- **`\title` / `\author` / `\date` / `\maketitle`** produce a centered
  title block.
- **Lists** (enumerate / itemize / description / paralist variants) parse
  to `<ol>` / `<ul>` / `<dl>` with each `\item` recursively re-parsed.
- **Role-tagged theorems** (`[role=main|supporting|standard|omitted]`)
  with per-proof fold/unfold and a toolbar that bulk-sets fold state by
  role. Default: "all expanded".
- **Inline LaTeX** in titles / theorem names / `\omitref` payloads:
  `\emph`, `\textbf`, `\textit`, `\texttt`, `\textsc`, `\ref`/`\cref`/
  `\eqref`/`\autoref`, and accent commands (`\'e` → é, `\"o` → ö, etc.).
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
  keeps the last broadcast's block sequence and pushes only the changed
  blocks as `{event: "patch", ops: [{type: "replace", id, html}, …]}`.
  A one-character edit inside a paragraph becomes a single `replace`
  op; the client just sets that block's `outerHTML` and never touches
  the surrounding blocks or any of their typeset math. Brings
  end-to-end keystroke latency on a 300-equation paper from ~250 ms
  (full body swap + 324 math transplants) to single-digit milliseconds.

## Quick start

```sh
cargo build --release

# Static one-shot HTML
./target/release/mathpreview-cli render examples/paper.tex -o out.html
open out.html

# Live-reload server (defaults to 127.0.0.1:23636)
./target/release/mathpreview-cli serve examples/paper.tex
# Open http://127.0.0.1:23636 in any browser.
```

The toolbar shows a status pill that reports timing on each update.
The format depends on whether the daemon could send a patch or had to
fall back to a full body re-render.

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

### Inspect what MathJax sees

Mirrors `latex-preview.nvim`'s `:LatexPreview debug`:

```sh
./target/release/mathpreview-cli debug examples/paper.tex
```

Prints the resolved root, included files, MathJax extension list, the full
macro table (name, arity, body, source file), and any warnings about
macros that were filtered out.

## nvim setup

Source `examples/mathpreview.lua` from your `init.lua`:

```lua
vim.cmd("luafile /absolute/path/to/mathpreview/examples/mathpreview.lua")
```

Or, if you keep the file on your runtimepath:

```lua
require("mathpreview").setup({
  url         = "http://127.0.0.1:23636/buffer",
  debounce_ms = 40,
  filetypes   = { "tex", "plaintex", "latex" },
})
```

This registers `TextChanged` / `TextChangedI` autocmds on `.tex` filetypes
and POSTs the buffer to the daemon, debounced at 40 ms. nvim 0.10+ uses
`vim.system` + `vim.uv`; older versions fall back to `jobstart` +
`vim.loop`.

**User commands the plugin exposes:**

```
:MathpreviewStatus    -- push count, last error, autocmd state, filetype
:MathpreviewPush      -- force one push now (bypasses debounce / filter)
:MathpreviewDisable   -- pause auto-pushes
:MathpreviewEnable    -- resume
```

If `MathpreviewStatus` shows zero `autocmds_count`, the file wasn't
loaded. If `last_error` is set, that's the curl side (server not running,
wrong URL, etc.).

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
crates/core   parser + macro extractor + numbering + renderer (the library)
crates/cli    mathpreview-cli binary (render / debug / serve)
examples/     demo paper + companion .sty + nvim Lua plugin
DESIGN.md     full design document
```

## What's not done yet

- **Forward/inverse search.** No editor-cursor ↔ preview-element jumps yet.
  The sync index is populated server-side and every rendered block carries
  a `data-src="file:line:col"` attribute, so adding it is a frontend +
  small protocol extension on the existing WebSocket.
- **Margin/popup cross-references.** Clicking a ref currently jumps to
  the target via `<a href="#id">`. The "pin into the margin" interaction
  from DESIGN §8 isn't built.
- **Vendored MathJax.** Defaults to jsdelivr CDN. For offline use, pass
  `--mathjax-url path/to/vendored/tex-svg.js`.
- **Multi-file editing.** The buffer-push path replaces the *root* file's
  content; if you edit a `\input`-ed child, you'd need the editor plugin
  to send each buffer keyed by path. The server-side substitution map
  exists in the architecture but isn't exercised.
- **Insertion in the middle of the document falls back to a full body
  re-render.** The position-based block diff treats every block after an
  insertion point as "changed" because their IDs shift. We fall back to
  full-body for any push where ops would touch >50% of blocks, which
  catches that case but pays the full-body cost (~40–250 ms depending on
  paper size). Fix is a real keyed-LCS diff with explicit move/insert
  ops — see DESIGN §11 ⏳ for the proper approach and the gotchas of the
  naive heuristic.
- **Tauri shell as a native window option.** WebSocket was picked as the
  starting transport because it decouples backend from frontend — not as
  a permanent rejection of Tauri. The intended destination is to add a
  `crates/app/` Tauri binary that uses the same `mathpreview-core`
  underneath, with the existing `mathpreview-cli serve` remaining as the
  headless option for browser-tab users. See DESIGN §11 step 7 for the
  migration sketch.
- **Pluggable rendering engine.** The current renderer emits HTML +
  MathJax; the architecture (AST → renderer → WebSocket → frontend) is
  set up so we can swap in PDF-based rendering (via Texpresso) later
  without disturbing the AST or interactive-overlay layers. A future
  `--engine mathjax|texpresso` flag would let users with a TeX install
  pick faithful rendering, others keep the fast MathJax default.
