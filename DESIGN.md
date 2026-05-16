# mathpreview — Design Document

A live LaTeX paper preview server: keystroke-level updates, role-aware
proof rendering, clickable cross-references, and two-way sync to the
editor. Renders LaTeX through MathJax in a browser tab the daemon serves
locally — no native window, no LaTeX engine on the user's machine, no
PDF roundtrip.

## 0. Status and pivot

The original draft of this document (preserved in conceptual sections
below) proposed a **Tauri desktop app** with a `notify-rs` file watcher
and Tauri-specific IPC. After studying how
[tinymist](https://github.com/Myriad-Dreamin/tinymist) is structured we
pivoted to a **WebSocket-based daemon** with the same `mathpreview-core`
library underneath. The new factoring lets any frontend (browser tab,
embedded webview, future Tauri shell, future VSCode extension) connect
to the same backend, which mirrors what tinymist achieves with Zed and
VSCode reusing the same compile pipeline.

What's built:

- **Step 1 (CLI render).** Parser, macro extractor, numbering, HTML
  renderer, bibliography. Stress-tested on a real ~40 KB math paper with
  96 user macros, 4 MathJax-mapped packages, 19 theorem-likes, 35
  proofs, 320 math elements.
- **Step 2′ (WebSocket server, replacing original Tauri shell).**
  `mathpreview-cli serve` exposes HTTP + WebSocket on `127.0.0.1:23636`.
  The HTTP page is the shell; WebSocket pushes body deltas on every
  re-render. The shell also exposes `POST /restart` and `POST /stop`
  through toolbar controls.
- **Step 3 (live reload).** File watcher (`notify-debouncer-full`) + an
  in-memory **buffer-push endpoint** (`POST /buffer`) that the editor
  plugin pings on `TextChanged`. Mid-edit guard: unbalanced `$$…$$` or
  `\begin{…}` defers the push so the page never flashes a broken state.
- **Incremental SVG MathJax rendering.** Every math node carries a
  content hash and original `data-tex`; the client transplants
  already-typeset SVG DOM nodes by hash and defers typesetting for
  actually-changed expressions. Per-keystroke updates run in ~5-10 ms
  total on the test paper when the edited math is unchanged.
- **Paper-like viewer layout.** The browser shell has A4 page mode,
  dynamic page mode, page dividers, a toggleable index/pages rail, and
  responsive controls for narrow browser widths.
- **LaTeX paragraph semantics.** Blank lines create paragraph breaks
  with indentation instead of visible `<br><br>` gaps, including around
  display math and inside theorem/proof text.
- **Proof-flow macros.** KFP-style `\step`, `\case`,
  `\restartsteps`, `proofsteps`, and `proofcases` render as numbered
  proof markers with LaTeX-like resets and no-indent flow.
- **Front matter, roles, and proof metadata.** Repeated authors,
  `\and`, `\address`, `\curraddr`, and `\email` render in the title
  block, and `abstract` renders after the title block even when the
  source places it before `\maketitle`. Theorems/propositions/lemmas
  carry `main`, `supporting`, `standard`, or `omitted` roles, and
  proofs can infer or manually declare their role.
- **Equation copy path.** Inline and display SVG equations can be
  selected as a single math node and copied as their original LaTeX
  source.
- **Project-local bibliography and figure sizing.** `\bibliography{...}`
  and `\addbibresource{...}` are resolved relative to the main `.tex`
  file directory, including bibliography commands in the document body.
  Body-level `\bibliographystyle{plain}` is honored with sorted numeric
  references and BibTeX-plain-like entry formatting.
  Rendered figures preserve common `\includegraphics` sizing options
  such as `width=0.8\textwidth`, absolute widths, `height`, `scale`, and
  `keepaspectratio`.

### Implementation TODO

- [x] ~~CLI render pipeline for real LaTeX projects.~~
- [x] ~~WebSocket daemon and browser shell.~~
- [x] ~~Live file watching and editor buffer-push updates.~~
- [x] ~~Block-level patch protocol and MathJax node reuse.~~
- [x] ~~Deferred SVG MathJax typesetting and A4 patch optimization.~~
- [x] ~~Root/input/include/subfile splicing with source offsets.~~
- [x] ~~Macro/preamble extraction from root and local `.sty` files.~~
- [x] ~~BibTeX/biblatex-style references and friendly cross-references.~~
- [x] ~~Body-level `\bibliographystyle{plain}` sorting and reference formatting.~~
- [x] ~~Title/authors/address/email front matter.~~
- [x] ~~Abstract placement after the title block.~~
- [x] ~~Role-tagged theorems and proof filtering.~~
- [x] ~~Postponed proof role inference from `Proof of ...` headings.~~
- [x] ~~Manual proof roles and companion `mathpreview.sty` proof filtering.~~
- [x] ~~Numbered `\step` / `\case` proof-flow macros with counter resets.~~
- [x] ~~LaTeX-like paragraph, display, and blank-line spacing.~~
- [x] ~~A4/dynamic page layout, page dividers, and index/pages rail.~~
- [x] ~~Viewer restart and stop/start buttons.~~
- [x] ~~Selectable/copyable SVG MathJax equations that copy LaTeX.~~
- [ ] Margin / popup reference previews with click-to-pin and nested references.
- [ ] Forward / inverse search between preview and editor.
- [ ] Vendored/offline MathJax distribution.
- [ ] Better parser coverage for more LaTeX text constructs and packages.
- [ ] Browser-level interaction tests for copy/selection, page layout, and proof filters.

What's not built yet — sections §8, §9 below describe these as forward
goals:

- Forward / inverse search (preview ↔ editor cursor jump).
- Margin / popup cross-references with hover and click-to-pin.
- Vendored MathJax for offline distribution.
- Broader browser-level interaction testing for the viewer controls.

The build-order plan in §11 is updated to reflect this.

## 1. Relationship to `latex-preview.nvim`

This project is the **side-by-side companion** to [sonv/latex-preview.nvim](https://github.com/sonv/latex-preview.nvim), not a replacement for it. The two answer different questions:

- `latex-preview.nvim` answers "what does this equation look like?" — hover preview, terminal-native, popup auto-closes when the cursor leaves the math. It's the right answer when you're focused on writing and occasionally want to verify a single expression.

- `mathpreview` answers "what does the whole paper look like, and how do its pieces connect?" — continuous rendering of the full document in a separate window, with clickable cross-references that bring statements into the margin without scrolling. It's the right answer when you're reviewing structure, navigating proofs, or showing the paper to someone.

The two are designed to coexist. A user might keep `mathpreview` open on a second monitor while writing, hit `<leader>ih` for occasional in-buffer hover, and only look over at the side-by-side window for context.

**Shared philosophy.** Both tools render math through MathJax (not KaTeX), for the same reason: real LaTeX papers contain custom `\newcommand`s, project-specific `.sty` files, and packages like `physics` / `mhchem` / `mathtools` that MathJax handles and KaTeX doesn't. The macro extraction approach, the multi-file project resolution, the normalization quirks (`\providecommand` → `\newcommand`, `\edef` → `\def`) — all of this should be implemented the same way, and ideally factored so the logic is portable.

## 2. Product scope

**What this is.** A browser-served preview tool for LaTeX documents
authored in nvim + vimtex. The daemon resolves a project root, parses
every file in the project, extracts user macros, and renders an
interactive document view that any local browser tab can connect to.
On every keystroke (or file save, depending on which integration is
active) the daemon pushes a diff of the body over a local WebSocket.
The preview is the entire user-facing surface; there is no editor, no
file picker, no settings UI beyond a small toolbar.

**What this is not.**
- Not a LaTeX-to-PDF compiler. The user still runs `latexmk`, `tectonic`, or whatever they prefer for the actual PDF output.
- Not a replacement for `latex-preview.nvim`. The hover-in-buffer use case is served better by that plugin.
- Not a general LaTeX engine. Constructs the renderer doesn't understand are passed through as opaque text rather than crashing.
- Not bound to a single frontend. The wire format is WebSocket-of-HTML-deltas — a browser tab is the default, but the same daemon could feed a Tauri window, a VSCode webview, or an nvim popup with no backend changes.

**The differentiating features.**
- Role-tagged theorems (`main`, `supporting`, `standard`, `omitted`) with reader-selectable proof visibility.
- Clickable cross-references that bring statements into a margin column or popup without scrolling away from the reader's place.
- Hover previews on references that show just the statement; click pins the full proof.
- Nested reference expansion (clicking a `\ref` inside a margin card opens a child card).
- Two-way sync with nvim: click an element in the preview to jump the cursor; hit a vimtex keybind to flash the corresponding element in the preview.

**Critical constraint.** Real LaTeX papers are the input. Arbitrary preambles, `\usepackage` declarations, hundreds of `\newcommand`s, project-specific `.sty` files, multi-file projects with `\include` / `\input` / `\subfile`. Authors should not have to modify their existing project structure to use this tool, except for adding `\usepackage{mathpreview}` and optional `[role=...]` annotations on theorems.

## 3. Architecture overview

```
                       ┌─────────────────────────────────────────┐
                       │   mathpreview daemon (axum + tokio)     │
 ┌──────────────┐      │                                         │
 │ nvim plugin  │      │   ┌─────────────────────────────────┐   │
 │ (init.lua)   │──────┼──▶│  mathpreview-core               │   │
 │ TextChanged  │ POST │   │   • root resolver               │   │
 │  + 40ms      │/buffer   │   • parser (hand-rolled LaTeX)  │   │
 │  debounce    │      │   │   • macro extractor             │   │
 └──────────────┘      │   │   • numbering pass              │   │
                       │   │   • bib loader (biblatex styles)│   │
                       │   │   • renderer (HTML + data-hash) │   │
                       │   │   • sync index                  │   │
                       │   └────────────────┬────────────────┘   │
 ┌──────────────┐      │                    │                    │
 │ filesystem   │      │       broadcast::Sender<String>         │
 │ *.tex *.sty  │──────┤                    │                    │
 │ *.bib        │ notify-rs                 │                    │
 └──────────────┘  (FSEvents)               │                    │
                       │   ┌────────────────▼────────────────┐   │
                       │   │ axum router:                    │   │
                       │   │   GET  /        (shell HTML)    │   │
                       │   │   GET  /ws      (broadcast)──┐  │   │
                       │   │   POST /buffer  (push)       │  │   │
                       │   └──────────────────────────────┼──┘   │
                       └──────────────────────────────────┼──────┘
                                                          │ WS text frames
                                                          │ {event,html}
                                                          ▼
                       ┌──────────────────────────────────────────┐
                       │ Browser tab (or any future frontend)     │
                       │  • inline shell HTML + CSS + JS          │
                       │  • MathJax v3 (SVG output)               │
                       │  • incremental update by content hash    │
                       │  • toolbar + per-proof fold              │
                       │  • (future) margin popups, hover preview │
                       └──────────────────────────────────────────┘
```

One process, many possible frontends. The daemon talks to the frontend
over WebSocket text frames carrying JSON events:

- `{event: "patch", ops: [{type: "range", index, remove, insert, html}, …],
  blocks: [{id, hash}, …]}` — the common case for keystroke edits.
  Each range op replaces `remove` top-level render blocks starting at
  `index` with the already-rendered `html`. The `blocks` list lets the
  browser retag shifted DOM blocks after insertion/deletion edits.
- `{event: "body-updated", html: …}` — full-body re-render, sent when
  the block diff would be larger than ~50% of the document anyway
  (typical for a structural edit that shifts every block's position).
- `{event: "full-reload"}` and `{event: "error", message}` for rare
  cases where in-place patching can't recover.

The same daemon can serve a browser tab today and a Tauri-wrapped
webview, VSCode webview, or nvim popup later — all are WebSocket
clients of the same protocol.

The editor talks to the daemon two ways:

- **File watcher path.** Default for any editor.
  `notify-debouncer-full` catches every save and triggers a re-render.
  Coarse but zero-config.
- **Buffer-push path** (`POST /buffer`). The editor sends the current
  buffer content with an `X-Mathpreview-Path` header. The daemon
  substitutes that content for the named file's disk copy when
  rendering. No file write happens; git stays clean; updates fire on
  every keystroke (debounced 40 ms in the nvim plugin). This is the
  approach `latex-preview.nvim` uses to talk to its MathJax-Node
  daemon; we copied the shape.

### Live-update correctness and speed

Two separate bugs showed up while testing unsaved buffer updates on the
KFP manuscript.

First, async renders can finish out of order. If the editor sends buffer
A, then buffer B, buffer A may still finish last and overwrite the
preview with stale HTML. The daemon now assigns every file-watch and
`/buffer` render a monotonic render sequence number. Only the newest
sequence may update `current` or broadcast websocket patches; older
successful renders and older render errors are discarded.

Second, a paragraph insertion above existing text shifts generated block
ids. The old patch format said "replace DOM element `blk-519`", which is
fine for typing inside a paragraph but unsafe when the new render has
inserted a new `blk-519` before the old one. The visible symptom was an
unsaved insertion such as:

```tex
Test test test
$a^2+b^2$

First, note that if ...
```

appearing below the `First, note...` paragraph. A brute-force full-body
update fixed the order but forced the browser to diff and swap hundreds
of MathJax nodes, producing multi-second updates on A4 pages.

The current protocol uses position-based range patches instead. The
server finds the unchanged prefix and suffix of top-level render blocks,
sends only the changed middle range, and the browser applies that range
by child position rather than by old DOM id. After the range is applied,
the browser retags block ids from the server's `blocks` metadata so the
next patch starts from a consistent DOM. The diff uses a semantic block
hash that ignores volatile source line metadata and generated MathJax ids
(`im-*`, `dm-*`, etc.), so inserting a few lines above a large unchanged
display section remains a one-op patch instead of becoming a full body
swap. Already-open tabs with the old JavaScript are forced through a
one-time reload by a websocket protocol version query.

### Why HTML over WebSocket, not SVG

Tinymist pushes rendered SVG pages to the browser. We push HTML+MathJax
instead, because the differentiating features (margin popups, clickable
refs, role-based proof folding, hover previews) require a structured
DOM. Rendering source-LaTeX to SVG also requires a real LaTeX engine
(`dvisvgm` after compilation, or a Rust-native typesetter) which
contradicts the "no LaTeX install required" constraint. So we kept the
**factoring** from tinymist (compile pipeline ⊥ render pipeline ⊥
transport) but swapped the wire format from SVG to HTML.

## 4. Multi-file projects and TeX root resolution

This logic should match `latex-preview.nvim`'s approach exactly. The algorithm to find the project root for any opened file:

1. Look for a `% !TEX root = path/to/main.tex` magic comment at the top of the buffer.
2. Query vimtex's root metadata via the nvim RPC connection (if the user has vimtex active and we're connected to nvim).
3. Walk up parent directories looking for a `.tex` file that contains `\begin{document}` and that reaches the current file through `\input{...}`, `\include{...}`, or `\subfile{...}` (nested includes followed).
4. If none found, treat the opened file as standalone.

The root file does not have to be named `main.tex` — papers commonly use `paper.tex`, `thesis.tex`, or arbitrary names.

Once resolved, the daemon builds a dependency graph rooted at the main file and watches every file in it. When any watched file changes:
- if it's the main file or a preamble file, re-extract macros and re-parse everything;
- if it's an included body file, re-parse just that file's AST subtree;
- if it's a `.bib` file, re-parse bibliography entries.

The sync index tracks the *true source file and line* of every rendered element, not just an offset into a flattened version. Inverse search must jump to the correct file, not just the correct line number.

## 5. Source language and the `mathpreview.sty` package

This tool reads standard LaTeX with one extension: theorem-like environments may carry a `[role=...]` optional argument.

```latex
\usepackage{mathpreview}

\begin{theorem}[role=main]{Main result}
  \label{thm:main}
  The series $\sum 1/n^s$ converges iff $s > 1$.
\end{theorem}

\begin{lemma}[role=supporting]
  \label{lem:harmonic}
  For every $N \ge 1$, $H_N \ge \log(N+1)$.
\end{lemma}

\begin{lemma}[role=standard]{Integral test}
  ...
\end{lemma}

\begin{theorem}[role=omitted]{Cauchy condensation}
  ...
\end{theorem}
\omitref{Rudin, \emph{PMA}, §3.27}
```

**Role semantics.**

- **`main`** — central results. Proof always visible in preview, never foldable. Distinguished visually with a subtle purple accent.
- **`supporting`** — lemmas the main proofs actually use. Foldable; visibility controlled by user.
- **`standard`** — textbook results cited for completeness. Foldable; hidden by default in the cleanest view.
- **`omitted`** — results stated but with proofs deferred. Renders with dashed border, shows the `\omitref{...}` citation, no fold control.

**Default behavior.** A theorem without `[role=...]` is treated as `standard`. Existing papers without any annotations render correctly; the role system is opt-in.

**The `.sty` file.** Ship `mathpreview.sty` as a single file in `examples/`. It defines the `[role=...]` argument so `pdflatex` / `tectonic` accept the syntax, and reads a package option:

```latex
\usepackage[proofs=main]{mathpreview}             % submission version
\usepackage[proofs=main+supporting]{mathpreview}  % tech report
\usepackage[proofs=all]{mathpreview}              % everything
```

The package option controls whether each role's proof body is rendered in the PDF output. **One source compiles to three PDFs.** This is the feature that justifies the role system existing — without it, role tags are just metadata for the preview.

## 6. Macro and preamble extraction (the `latex-preview.nvim` approach)

This is the most important section. Real adoption depends on the preview rendering papers correctly *as they are written*, with whatever custom macros and packages the author already uses. The strategy mirrors `latex-preview.nvim`:

**Step 1: Find the root.** As described in §4.

**Step 2: Scan the root preamble.** Everything before `\begin{document}` is searched for definition-shaped commands:

- `\newcommand`, `\renewcommand`, `\providecommand`
- `\DeclareMathOperator`, `\DeclareMathOperator*`
- `\NewDocumentCommand`, `\RenewDocumentCommand`, `\ProvideDocumentCommand`
- `\def`, `\let`

If the currently-viewed file is a chapter/include file with its own preamble-like definitions before any `\begin{...}` environment, include those too.

**Step 3: Scan local `.sty` and `.tex` macro files.** Follow `\usepackage{name}`, `\RequirePackage{name}`, `\input{name}`, and `\include{name}` directives in the root preamble. When a matching local file exists within the configured parent search depth (default 4 directories up from the project root), scan it for definitions too. This is how project-specific macro packages get picked up automatically.

**Step 4: Normalize for MathJax.** MathJax's macro support has quirks:
- `\providecommand` becomes a no-op when MathJax already has a built-in with the same name. Rewrite to `\newcommand` to force the user's definition.
- `\edef` does expand-at-definition, which MathJax doesn't implement. Rewrite to `\def`.
- Both rewrites must be opt-in config flags (defaulting to on), matching `latex-preview.nvim`'s `extract.rewrite_providecommand` and `extract.rewrite_edef` options.

**Step 5: Map packages to MathJax extensions.** Known `\usepackage{...}` declarations trigger MathJax extension loading:

```rust
const PACKAGE_TO_MATHJAX: &[(&str, &str)] = &[
    ("amsmath",    "[tex]/ams"),
    ("amssymb",    "[tex]/ams"),
    ("mathtools",  "[tex]/mathtools"),
    ("physics",    "[tex]/physics"),
    ("mhchem",     "[tex]/mhchem"),
    ("cancel",     "[tex]/cancel"),
    ("color",      "[tex]/color"),
    ("xcolor",     "[tex]/color"),
    ("braket",     "[tex]/braket"),
    ("upgreek",    "[tex]/upgreek"),
    ("siunitx",    "[tex]/textmacros"),
    // ...
];
```

Packages not in the table are silently ignored — MathJax doesn't need to know about `geometry` or `fancyhdr`.

**Step 6: Send to MathJax.** Macros become entries in MathJax's `tex.macros` config; package mappings become entries in `loader.load`. Pass everything when initializing or reconfiguring MathJax for each render.

**Failure mode.** When a macro can't be extracted (because the definition uses a form the extractor doesn't recognize, or it's defined inside a complex `\ifx` block, etc.), surface a warning in the status indicator: "3 macros could not be loaded — click for details." Affected math falls back to verbatim source rendering in monospace. **Never crash the renderer.**

**Built filters.** Beyond the §6 plan, the extractor rejects a few
patterns that look extractable but would cause MathJax to fail at
expansion time — typically with a "buffer size exceeded; recursive
macro call" error from MathJax:

- Bodies referencing `@`-namespaced internals (`\SV@given` etc.) when
  those internals were themselves filtered out — the wrapper would
  dangle and either error or loop.
- Bodies containing `##` (TeX nested-def parameter substitution),
  which is meaningless at MathJax's top level.
- Bodies invoking TeX primitives MathJax can't expand: `\expandafter`,
  `\csname`/`\endcsname`, `\if`/`\else`/`\fi`, `\kern`, `\nonscript`,
  `\setkeys`, `\renewenvironment`, etc.
- Names containing `@` (LaTeX-private by convention) are dropped
  entirely without warning.

Filtered macros emit a one-line warning that surfaces in the page's
warnings panel.

**Debug command.** Match `latex-preview.nvim`'s `:LatexPreview debug` — provide a way for the user to see exactly what preamble is being sent to MathJax for the current document. This is essential for diagnosing why a custom macro isn't being picked up. In a Tauri app, this is a menu item that opens a panel showing the extracted preamble.

**Caching.** Cache the extracted preamble keyed on `(root_file, root_mtime, scanned_macro_files)`. Invalidate when any input file changes.

## 7. Rendering pipeline

```
buffer push or file save
    │
    ▼
serve.rs is_buffer_renderable() — skip if mid-edit
    │
    ▼
project::load_project_from_source(root, body)         ~1ms
    │
    ▼
PREAMBLE CACHE (keyed on fnv-hash of preamble source)
    ├─ hit  → reuse extracted macros / bib / style    0ms
    └─ miss → extract + bib + style                  ~30ms
    │
    ▼
parser::parse_body(&project)                          ~2ms (regex cache hot)
    │
    ▼
numbering::assign_numbers(&mut body, &bib, style)     0ms
    │
    ▼
renderer::render(...) → (full_html, body_html)        ~2ms
    │  every math node carries data-hash
    │  every block carries data-src="file:line:col"
    │
    ▼
diff_blocks(prev_blocks, new_blocks)                  <1ms
    │  position-based; emits Replace/Append/Remove ops
    │  falls back to full body-updated if >50% blocks changed
    ▼
broadcast::Sender<String>.send(JSON)                  <1ms
    │
    │  WebSocket text frame: {event:"patch", ops:[…]}
    │  OR (rarely) {event:"body-updated", html}
    ▼
[frontend] patch path:
    │   for each op:
    │     replace → getElementById(id).replaceWith(parsed html fragment)
    │     append  → page.appendChild(fragment)
    │     remove  → element.remove()
    │   collect any .math[data-hash] inside inserted fragments
    │   MathJax.typesetPromise(only those new math nodes)
    │
[frontend] body-updated fallback path (only when patch isn't worth it):
        build new HTML in detached <template>
        hash-match math nodes from current #page → transplant
        page.replaceChildren(buf)
        MathJax.typesetPromise(only the genuinely-new math)
```

Steady-state per-push cost on the test paper (40 KB, 96 macros, 320
math elements, one-character body edit on the patch path): ~5–10 ms
total wall clock, typically `1r / typeset 0 (0 math)` in the status
pill. The combination of preamble cache, regex cache, block diff,
and incremental MathJax accounts for ~335 ms of the ~340 ms a naïve
full re-render would cost.

**Parser.** A LaTeX-aware parser, not a full LaTeX engine. Understands a curated set of constructs (document structure, theorem-like environments with `[role=...]`, sectioning, math delimiters, common formatting, `\ref`/`\eqref`/`\cref`/`\autoref`, `\cite`/`\citet`/`\citep`/`\parencite`/`\textcite`, `\label`); passes everything else through as opaque tokens. Every AST node carries `{ file, line, col, byte_range }`.

For implementation, `texlab`'s parser (the LaTeX language server's grammar) is a defensible choice. A hand-rolled Lezer-style grammar gives finer control. Either way, ensure source-position metadata survives to every leaf.

**Renderer.** AST → HTML. Each block-level element emits with `data-src="file:line:col"` and a unique `id`. Math expressions are left as `\(...\)` and `\[...\]` for MathJax to typeset in the webview. Theorem environments render with their role as a CSS class and a small pill indicating the role. References render as `<span class="ref" data-target="thm:main">Theorem 3</span>`, with click handlers attached after MathJax completes.

**Frontend.** Plain HTML/CSS/JS, no framework. The page is a static shell with `<div id="page">` for the rendered document and `<aside id="margin">` for the cross-reference cards. When `document-updated` arrives, replace contents, run MathJax, swap, attach handlers. The render-into-hidden-then-swap pattern eliminates flicker that would otherwise occur during MathJax's async typesetting.

**MathJax configuration template.**

```javascript
window.MathJax = {
  tex: {
    packages: { '[+]': ['ams', 'physics', 'mhchem', 'cancel', 'color', 'mathtools'] },
    inlineMath: [['$', '$'], ['\\(', '\\)']],
    displayMath: [['$$', '$$'], ['\\[', '\\]']],
    macros: { /* injected from Rust on each update */ },
    tags: 'ams'
  },
  loader: { load: ['[tex]/physics', '[tex]/mhchem', '[tex]/cancel', '[tex]/mathtools'] },
  chtml: { fontURL: 'mathjax-fonts/' },  // bundled, not CDN
  startup: { typeset: false }
};
```

The `fontURL` must point at bundled fonts shipped inside the Tauri app's assets. **No CDN dependencies.** The app must work offline.

## 8. Interactive features (preview-side)

**Role-based proof visibility.** A toolbar selector with three options: `main only` / `+ supporting` / `everything`. Affects CSS classes on proof `<div>`s; no Rust roundtrip. Default is `main only`. `main`-tagged theorems' proofs are always visible regardless.

**Click a reference, pin to margin.** Clicking a `\ref{lem:harmonic}` rendered as a styled link looks up the target by label, builds a margin card showing its statement and proof, animates it into the right-hand margin column. Cards stack; clicking the same reference again unpins it. Multiple cards can be open simultaneously for cross-comparison.

**Hover preview.** Hovering a reference for 250ms shows a small floating popup near the cursor with just the statement (no proof), and a hint "click to pin." Move the mouse away, it disappears. Click, it commits to the margin (or popup, depending on mode).

**Margin vs popup mode.** Toolbar toggle. Margin mode reserves the right-hand column; popup mode keeps the page full-width and anchors clicked references as floating panels near the click. Margin is better for desktop with wide screens and dense cross-referencing; popup is better for narrower windows.

**Nested expansion.** Clicking a reference inside a margin card opens a child card indented and bordered in a contrasting color, preserving the dependency trail. Closing a parent closes its children.

**Citation previews.** `\cite{Smith2024}` and friends are previewable the same way as theorem references — hover shows the BibTeX entry, click pins it to the margin. Citations are resolved from `.bib` files listed in `\bibliography{...}` or `\addbibresource{...}`. This matches `latex-preview.nvim`'s citation handling.

## 9. nvim integration

**Built today: buffer-push live updates.** `examples/mathpreview.lua` is
a self-contained Lua plugin (no plugin manager required). Source it
from `init.lua`:

```lua
vim.cmd("luafile /path/to/mathpreview/examples/mathpreview.lua")
```

It registers `TextChanged` / `TextChangedI` autocmds on `.tex` filetypes
and POSTs the current buffer to `http://127.0.0.1:23636/buffer` with a
40 ms debounce. Header `X-Mathpreview-Path` identifies the file. nvim
0.10+ uses `vim.system` + `vim.uv`; older nvims fall back to
`jobstart` + `vim.loop`. User commands `:MathpreviewStatus`,
`:MathpreviewPush`, `:MathpreviewEnable`, `:MathpreviewDisable` expose
the runtime state for debugging.

User launches the daemon separately:
`mathpreview-cli serve path/to/any-file-in-project.tex`. The daemon
resolves the root, opens HTTP on `127.0.0.1:23636`, and accepts both
disk-watch (any editor) and buffer-push (nvim plugin) sources of
truth.

**Planned: forward / inverse search over the same WebSocket.** Both
directions ride the existing WS connection — no separate Unix socket
needed.

- **Forward search path** (vimtex keybind → preview): nvim plugin
  sends `{event: "forward-search", file, line, col}` to the daemon
  over either an existing `/ws` connection or a fresh fire-and-forget
  POST. Daemon looks up the sync index, broadcasts
  `{event: "flash-element", id, ...}` to every connected client.
  Frontend scrolls to and briefly highlights the element.
- **Inverse search path** (preview click → nvim cursor): frontend
  click handler reads `data-src="file:line:col"` from the clicked
  block, sends `{event: "jump-to-source", file, line, col}` over WS.
  Daemon dispatches via `nvim-rs` to the running nvim's listen socket
  (`NVIM_LISTEN_ADDRESS` or `--nvim-socket`).

This is unbuilt as of writing — see §11 Step 6.

## 10. Project layout

```
mathpreview/
├── Cargo.toml                       # workspace
├── README.md
├── DESIGN.md                        # this document
├── examples/
│   ├── paper.tex                    # demo paper with role annotations
│   ├── mathpreview.sty              # companion LaTeX style file
│   └── mathpreview.lua              # nvim plugin (TextChanged → POST)
├── crates/
│   ├── core/                        # the rendering library
│   │   ├── src/
│   │   │   ├── lib.rs               # render_project / render_project_from_source
│   │   │   ├── ast.rs               # Node, NodeKind, ListKind, Role, Span
│   │   │   ├── parser.rs            # hand-rolled LaTeX parser w/ source pos
│   │   │   ├── root.rs              # TeX root resolution
│   │   │   ├── project.rs           # multi-file load (disk or buffer source)
│   │   │   ├── macros.rs            # preamble macro extraction + filters
│   │   │   ├── packages.rs          # \usepackage → MathJax mapping
│   │   │   ├── numbering.rs         # section / theorem / equation counters
│   │   │   ├── bibtex.rs            # .bib parser + style detect + labels
│   │   │   ├── renderer.rs          # AST → HTML (full + body) + client JS
│   │   │   └── sync.rs              # sync index (forward/inverse search)
│   │   └── Cargo.toml
│   └── cli/
│       ├── src/
│       │   ├── main.rs              # render / debug / serve subcommands
│       │   └── serve.rs             # axum server + watcher + cache
│       └── Cargo.toml
```

Crate `core` is pure rendering — no async, no IO beyond reading files
the caller passes paths to. Crate `cli` is the only place tokio / axum
/ notify live. This keeps the door open for embedding `core` in a
future Tauri shell or LSP server without dragging in a runtime.

## 11. Build order

Steps marked ✅ are done; numbering follows the revised plan after the
Tauri → WebSocket pivot.

**Step 1 ✅ CLI parser + renderer + macro extractor.** Crate `core` +
`cli`. `mathpreview-cli render paper.tex -o out.html` produces a
self-contained HTML file. Macro extraction has been hardened against a
real ~40 KB math paper (96 user macros across 3 files,
`\DeclarePairedDelimiter` wrappers, biblatex alphabetic citations,
multi-environment lists, LaTeX accents). Filters reject TeX-internal
bodies (`\expandafter`, `\csname`, `@`-namespaced commands, `##`
parameter patterns) that would cause MathJax to loop.

**Step 2 ✅ WebSocket live-reload server** (replaces original
"Tauri shell" plan). `mathpreview-cli serve` runs `axum` on
`127.0.0.1:23636`. HTTP returns the shell page; `/ws` broadcasts JSON
events to every connected frontend. The shell page includes ~80 lines
of inline JS that connects to `/ws`, swaps `#page` content on
`body-updated` messages, and shows a live status pill in the toolbar.

**Step 3 ✅ File watcher + buffer-push.** Two paths to update content:

- `notify-debouncer-full` watches the directories containing every
  project file and triggers a re-render on save (120 ms debounce).
- `POST /buffer` accepts an in-memory buffer override from an editor
  plugin. `examples/mathpreview.lua` ships the nvim side: a
  `TextChanged` autocmd debounced 40 ms that curls the buffer.

Both paths feed the same broadcast channel. Mid-edit guard rejects
unbalanced math (`$$…`, `\begin{...}` without close, unmatched braces)
so half-typed expressions don't trigger a broken render.

**Step 3.5 ✅ Incremental rendering — three layers.**

1. **Math-level hash + transplant** (fallback path). Every math node
   carries a stable content hash (FNV-1a of the math body). On a full
   `body-updated` event the client builds the new HTML in a detached
   `<template>`, hash-matches math nodes from the live DOM, transplants
   the already-typeset ones in place of their counterparts in the new
   body, swaps `#page` contents, and only asks MathJax to typeset the
   genuinely-new math expressions. Saves ~280 ms of MathJax work on a
   300-equation paper.

2. **Server-side block diff** (common path). Each top-level AST node
   becomes a `<article class="blk" id="blk-N" data-blockhash="…">`. The
   daemon tracks the last broadcast block sequence and computes a
   position-based diff against the new render. Output is
   `{event:"patch", ops:[{type:"replace", id, html}, …]}` containing
   only the changed blocks. A single-paragraph text edit becomes one
   `replace` op; the client does `outerHTML` swap on that one block,
   never touching the 300+ surrounding blocks or any of their typeset
   math. When more than half the blocks change (e.g. inserting a new
   section near the top shifts every subsequent block's id), the
   daemon falls back to the `body-updated` path described in (1).

3. **Server-side preamble + regex cache.** Editing the body keeps the
   preamble identical, so we hash the preamble source and skip
   `extract_preamble` + `load_project_bib` + `detect_bib_style` on a
   cache hit. Every regex in the parser lives in a `LazyLock<Regex>`
   static so it compiles once per process, not once per theorem env.

Steady-state cost on the 40 KB / 300-equation test paper for a
keystroke that edits one paragraph: ~5 ms daemon-side render + 1 op
on the wire + ~3 ms client-side patch + 0 ms MathJax = single-digit
milliseconds wall clock from keypress to re-render.

**Step 3.6 ⏳ Better block-level diff (keyed LCS).** The current
position-based diff handles in-place edits perfectly (one `replace` op,
sub-10 ms wall clock) but falls back to a full `body-updated` push
whenever an insertion in the middle of the document shifts every
subsequent block's position. A first attempt at "stable IDs across
renders" — match new blocks to prior blocks by content hash so the IDs
don't shift on insertion — was reverted because the naive
hash-popping-front heuristic scrambles duplicate-hash blocks (two
structurally identical paragraphs, two equations with the same body,
etc.) and the position-only diff doesn't detect block reorders. The
right fix when revisited:

1. Compute a real LCS of (hash, position) pairs between old and new
   block sequences. Anything in LCS keeps its ID. Anything outside LCS
   on the new side is an insert; outside on the old side is a remove.
2. Detect reorders explicitly. Where a block matches by content but
   its position differs by more than its neighbours moved, emit an
   explicit `move` op instead of leaving the DOM stale.
3. Keep IDs stable across consecutive renders (a block keeps its ID
   from its previous appearance) but re-canonicalize on file save
   (file-watcher event) so the ID counter doesn't grow unbounded over
   long editing sessions.
4. The fallback to full `body-updated` stays as a safety net for
   catastrophic edits (more than ~50% of blocks change at once), but
   in-place insertions become a single `insert` op rather than the
   current full-body fallback.

The investment is real (~150 lines of LCS + move detection + ID
counter + reset-on-save), so it's worth doing only when the
full-body fallback on insertions starts being noticeable in daily use.

**Step 3.7 ✅ Viewer fidelity and editing ergonomics.** Several
workflow-polish items landed before the interactive reference work:

- SVG MathJax remains the default output, but math nodes now retain the
  original TeX source in `data-tex`. Clicking an inline or display
  equation selects just that math node, and copy events substitute the
  original LaTeX source instead of SVG text.
- A4 mode renders a fixed A4-ratio sheet that scales with the browser
  width; dynamic mode preserves readability without strict paper
  scaling. Page dividers and generated page jumps share the left rail
  with the section index.
- Blank LaTeX lines map to paragraph breaks with indentation, not
  visible blank vertical gaps. This applies to top-level prose, display
  math continuations, and text inside theorem/proof environments.
- Front matter handles repeated `\author{...}`, `\and`,
  `\address{...}`, `\curraddr{...}`, `\email{...}`, and `abstract`
  closely enough for AMS-style paper heads. Abstracts render after the
  title block even when declared before `\maketitle`.
- Proof roles can be explicit (`\begin{proof}[role=main]`) or inferred
  from postponed headings such as `Proof of Proposition \ref{...}`.
  `Proof of ...` headings render bold, and the companion
  `mathpreview.sty` uses the same proof-role metadata for PDF filtering.
- KFP-style proof-flow macros render as semantic markers:
  `\step` increments `Step N:`, `\case` increments Roman cases, and
  `\restartsteps` plus `proofsteps` / `proofcases` reset counters.
- Body-level `\bibliographystyle{plain}` is detected for legacy BibTeX
  documents. Numeric citations are renumbered after author/editor sorting,
  and bibliography entries use first-name-first names, italic venues, cleaned
  BibTeX capitalization braces, and compact DOI/arXiv metadata.
- The browser toolbar has restart and stop buttons backed by
  `POST /restart` and `POST /stop`. Restart relaunches the daemon with
  the same arguments and reloads the page after the replacement server
  is ready; stop exits the daemon, turns into a start button, and leaves
  the browser in an intentional stopped state until the server is
  available again.

**Step 4 ⏳ Interactive references.** Margin column, click-to-pin,
hover previews, nested expansion, citation previews. All frontend work
on top of the rendered DOM; the data is already there (refs carry
`data-target`, citations carry `data-key`, sync index is populated).

**Step 5 ⏳ Inverse search.** `nvim-rs` connection to the editor's
listen socket. Frontend click handler sends
`{event:"jump-to-source", file, line, col}` over the existing
WebSocket; daemon dispatches `nvim_command(...)` to move the cursor.

**Step 6 ⏳ Forward search.** vimtex keybind → nvim plugin sends
`{event:"forward-search", ...}` → daemon looks up the sync index → WS
broadcast `{event:"flash-element", id, ...}` → frontend scrolls and
briefly highlights.

**Step 7 ⏳ Distribution polish.** Vendored MathJax (no CDN). Margin
warnings UI for filtered macros. Multi-buffer push (editing an
`\input`-ed chapter). Server-side per-project preamble cache shared
across reconnects. Distributable binaries.

Each ⏳ step is bounded by hours-to-days, not the weeks the original
plan reserved, because the architecture pivot removed most of the
plumbing (Tauri command/event wiring, native window persistence,
cross-platform webview testing).

### Step 8 ⏳ Tauri shell (the eventual destination)

Picking WebSocket first was a starting-point decision, not a permanent
rejection of Tauri. Tauri returns later as **one more frontend among
many**, not as the architecture itself. Migration sketch when we're
ready:

1. Add `crates/app/` alongside `crates/cli/`. Both depend on
   `mathpreview-core`.
2. Choose IPC shape:
   - **Easy path** — the Tauri window's webview connects to
     `localhost:23636` exactly like a browser tab does. Zero protocol
     change. Pays a WebSocket roundtrip even within one process; usually
     fine.
   - **Tight path** — drop `axum` from this binary, expose
     `mathpreview-core` to the webview via Tauri commands/events
     directly. ~half a day of plumbing; saves the loopback.
3. The frontend (`renderer.rs`'s embedded HTML+JS) is unchanged. Same
   DOM, same MathJax-or-PDF.js swap point, same interactive overlay.
4. `mathpreview-cli serve` does not go away. It stays as the headless
   option for users who want the preview in a browser tab on a second
   monitor, or for future VSCode/Zed integration. WebSocket and Tauri
   are siblings, not predecessor and successor.

### Pluggable rendering engine

The same pluggability principle applies to the engine choice. The
boundary between AST and pixels lives in `renderer::render(...)`. To
swap in a different engine later (e.g. Texpresso for pixel-perfect
output for users with a TeX install):

1. Add a new renderer variant alongside the current MathJax-friendly
   HTML output — say `renderer::render_texpresso(...)` that produces a
   manifest (PDF URL + synctex map + AST bounding boxes) instead of
   raw HTML.
2. Extend the WebSocket event shape: `{event:"body-updated", html}`
   becomes one variant; `{event:"pdf-updated", url, synctex_map,
   ast_overlays}` becomes another.
3. The frontend swaps MathJax for PDF.js when it receives the latter;
   the interactive overlay (margin popups, ref hover, proof folding)
   continues to drive off the AST overlay data, unchanged.
4. A `--engine mathjax|texpresso` CLI flag selects the path. Users
   without a TeX install stay on MathJax with sub-10ms updates; users
   with Texpresso installed get faithful rendering with its incremental
   compile speed.

The interactive features are deliberately built on the AST layer
(`mathpreview-core`), not on the renderer's specific output shape, so
they survive any rendering swap.

## 12. Key dependencies

Decided in favor of a hand-rolled parser (per the §13 discussion); no
texlab or tree-sitter dependency.

```toml
# core
anyhow     = "1"
thiserror  = "1"
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
regex      = "1"
walkdir    = "2"

# cli (server side only — keeps core async-free)
clap                  = { version = "4", features = ["derive"] }
axum                  = { version = "0.7", features = ["ws"] }
tokio                 = { version = "1", features = ["full"] }
notify                = "6"
notify-debouncer-full = "0.3"
futures-util          = "0.3"
```

Frontend, served inline from the daemon (no build step, no node_modules):

- MathJax v3 with `tex-svg` output (default; `tex-chtml` is a
  one-line flip in `HtmlOptions`).
- MathJax extensions enabled dynamically based on the project's
  `\usepackage{...}` set (see `packages.rs`).
- MathJax fonts loaded by MathJax itself from the same source URL.
- No JS framework. Inline `<script>` only; ~80 lines for proof folding
  + WebSocket client + incremental swap.

When Step 7 vendors MathJax, the same shell HTML works with a local
file URL — the `--mathjax-url` flag swaps the source.

## 13. Things that will be tricky

**Macro extraction edge cases.** This is where `latex-preview.nvim`'s existing approach really earns its keep — it's been hardened against real preambles. Port the same set of patterns it recognizes, the same normalization rules (`\providecommand` → `\newcommand`, `\edef` → `\def`), and the same fallback behavior. When something fails, surface a warning, don't crash.

**The preamble can be huge.** Some research groups have shared preambles with hundreds of macros and thousands of lines. Parser performance on the preamble matters more than on the body. Measure early; if regex-based extraction is too slow, switch to a streaming approach.

**`\input` cycles and missing files.** Cyclic includes shouldn't infinite-loop. Missing files should produce warnings, not crashes. The sync index should cover whatever did parse successfully.

**MathJax async typesetting with rapid saves.** If the user saves three times in 200ms, three typeset passes can queue up and finish out of order. Maintain a "latest version" counter; when a typeset completes, only swap visible if it's still the latest. Otherwise discard and let the next pass take over.

**Cross-platform paths.** Windows separators, macOS case-insensitive filesystems, symlinks. Pay attention in the project resolution code; bugs here are user-visible and annoying.

**Tauri WebView differences** (deferred — no longer load-bearing).
WebKitGTK on Linux, WKWebView on macOS, WebView2 on Windows render
slightly differently. With WebSocket-to-browser as the default
frontend, this is a Step 7 concern, not a Step 1 one. The user's
local browser picks one engine.

**Bundled MathJax size.** With fonts and the relevant extensions,
MathJax is ~5 MB on disk. Default today is CDN; vendored shipping is
Step 7. Lazy-load extensions per `\usepackage{...}` if size matters.

**Incremental DOM swap edge cases.** The hash-based transplant trusts
that math expressions with the same `data-hash` render identically.
That's true if and only if the macro set didn't change. A preamble
edit invalidates that assumption — we don't currently send a
"macros-changed" event; the user reloads. Adding a preamble-source
hash to the WebSocket payload (and a `full-reload` event when it
changes) is the proper fix.

**Block diff stability across renders.** The position-based diff is
correct but pays a full-body fallback cost on insertions in the middle
of the document. A naive "match by hash to keep IDs stable" replacement
was tried and reverted — it scrambles duplicate-hash blocks (e.g.
two structurally identical paragraphs swap their IDs when only one is
edited) and doesn't detect block reorders, leaving the DOM in stale
order with no patch ops emitted. **Lessons for the next attempt:** a
real keyed-LCS diff is the right shape, with explicit `move` ops when
content swaps positions; same-position+same-hash matches must beat
out-of-position matches in the matching priority; and the test
fixture should include duplicate-hash blocks (two empty paragraphs,
two identical references) to catch the scrambling regression before
it reaches a user. See Step 3.6 in §11.

**Mid-edit balance heuristic.** Counts `$`, `$$`, `\(`/`\)`, `\[`/`\]`,
`{`/`}`, and `\begin`/`\end`. Doesn't enforce env-name matching on
begin/end pairs — a `\begin{equation}` against `\end{equation*}`
passes the count check. The parser tolerates this, but it's a known
sharp edge.

---

Original handoff note (preserved): Step 1 was the make-or-break — if
the macro extraction worked on real papers, the rest of the project
would follow naturally. It did. Once Step 3 ran, priorities for the
remaining steps reordered around the user's actual editing workflow
rather than the original §8 feature list. The role/margin/popup work
in §8 remains as the next slot.
