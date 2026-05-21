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

[Unreleased]: https://github.com/sonv/TexViewer/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sonv/TexViewer/releases/tag/v0.1.0
