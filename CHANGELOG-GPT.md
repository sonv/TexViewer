# CHANGELOG-GPT

## 2026-05-19

### Fixed

- Made live `/buffer` rendering accept both the root file and watched included `.tex` files. The daemon now keeps in-memory buffer overrides keyed by canonical path and re-renders the real root with those overrides spliced through `\input` / `\include` / `\subfile`.
- Filtered directory watcher events down to actual watched project files, so unrelated files such as swap files or local HTML exports in the same directory no longer trigger disk re-renders that can overwrite unsaved buffer previews.
- Made the nvim helper use `curl --fail-with-body` so HTTP-level `/buffer`, `/cursor`, and `/jump` failures show up in `:MathpreviewStatus` instead of being treated as successful no-op pushes.
- Prevented hidden sidenote chips from being measured during margin layout, and reran the sidenote stacker when margin mode is enabled so chip transforms do not go stale.
- Made initial math rendering deterministic by disabling MathJax's automatic head-script page scan and queueing the first typeset pass from the viewer client after the engine is ready.
- Bumped the WebSocket shell protocol so already-open tabs reload once and pick up the deterministic initial-typeset path.
- Vendored MathJax 4's New Computer Modern SVG font shards and pointed local `/vendor/mathjax/tex-svg.js` shells at them, fixing `\boldsymbol` / `\bm` rendering in `SDE.tex`.
- Added the missing NewCM `svg.js` font-package entrypoint; without it MathJax startup failed before dynamic font shards such as `latin-b.js` could load.
- Bumped the WebSocket shell protocol again so tabs that already hit the failed MathJax startup path reload onto the repaired font package.
- Hardened proof/theorem paragraph transplanting by removing reused paragraph math from the old-node hash pool and skipping those reused chunks during the later math transplant pass.
- Corrected the paragraph-transplant comment to describe the actual optimization: unchanged proof paragraphs avoid DOM replacement and MathJax work, while the changed block's incoming HTML is still parsed as one fragment.
- Updated stale MathJax documentation/comments from v3/`es5` paths to the current MathJax 4 package-root layout.
- Removed trailing whitespace from vendored MathJax markdown so `git diff --check` passes.

### Verified

- `cargo fmt --check`
- `cargo check`
- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `git diff --check`
- `node --check crates/core/src/assets/client.js`
- Render smoke tests for `/Users/tsv/Work/LargeTimeLangevin/new-main.tex` and `/Users/tsv/Work/KFP/SDE.tex`
- Live `/buffer` smoke test with a root file that `\input`s a child file: posting the child buffer produced a WebSocket update for the root, and an unrelated file event in the watched directory did not revert the preview to disk content.

## 2026-05-16

### Performance

- Reused unchanged math nodes during block-level patch replacement, so prose edits in blocks that contain math no longer call MathJax for identical expressions.
- Deferred MathJax typesetting for changed math until 300 ms after edit activity stops, so active typing is no longer blocked by MathJax's fixed per-call cost.
- Detached the transformed A4 page during live block patching and delayed page-guide recalculation after edits, avoiding forced layout work on every re-render.
- Avoided detaching and reattaching the whole A4 document for small live patches; one-block typing updates now replace only the changed block.
- Restored ASCII fast paths in parser advancement and inline text rendering after the UTF-8 correctness fix.
- Kept Unicode text preservation by decoding UTF-8 only for non-ASCII bytes instead of every byte in normal LaTeX prose and math scans.
- Verified cached `/buffer` timing on `examples/paper.tex` at 1 ms locally after an initial 8 ms preamble-cache miss.

### Added

- Added first-pass nvim-to-HTML source sync: rendered block and paragraph wrappers now carry `data-src="file:line:col"` metadata and are registered in the `SyncIndex`.
- Added `POST /cursor` so the nvim plugin can send the current source cursor; the daemon maps it to the nearest rendered element and broadcasts a `source-cursor` WebSocket event that scrolls and highlights the preview.
- Added HTML-to-nvim inverse sync: double-clicking or Alt/Cmd-clicking rendered content posts the nearest source location to `POST /jump`, and the nvim plugin polls `GET /jump?after=...` to move the editor cursor.
- Added `CursorMoved` / `CursorMovedI` integration, `:MathpreviewSync`, sync status counters, and configurable `/cursor` and `/jump` URLs to `examples/mathpreview.lua`.
- Added source-word anchors for prose text, plus exact `data-src` anchors for rendered refs and citation groups, so forward and inverse search can target words instead of only paragraphs or environments.
- Added per-block source-anchor metadata to websocket patches so reused blocks can retag inner word/math/ref anchors after source lines shift without forcing a full block replacement.

### Fixed

- Tightened the default viewer typography and spacing toward LaTeX/AMS PDF output: corrected section heading selectors, reduced title spacing, restored paragraph indentation, compacted display math/list spacing, and kept theorem/lemma blocks compact.
- Fixed proof visibility mode buttons after block wrapping: `main only` and `+ supporting` now find theorem roles inside preceding patch blocks and keep the selected proof mode across live updates.
- Made postponed proofs such as `Proof of Proposition \ref{...}` inherit the referenced theorem/proposition role before falling back to the immediately preceding theorem block or nearest `Proof of ...` section heading.
- Added manual proof roles via `\begin{proof}[role=main]`, `\begin{proof}[role=main, Proof of ...]`, `\begin{proof}[role=main,of={...}]`, or `\begin{proof}[role=main,name={...}]`; explicit proof roles override inference in the viewer and are consumed by `examples/mathpreview.sty` for PDF proof filtering.
- Kept optional proof headings such as `Proof of ...` bold by letting the title span inherit the proof heading weight.
- Added A4 page dividers to the viewer and a fixed left navigation rail that toggles between a section index and generated page jumps.
- Added viewer controls for A4 versus dynamic page sizing, plus a toggleable index/pages pane that can be opened on narrower browser widths.
- Made A4 mode scale a fixed A4-width sheet on browser resize instead of reflowing the document into a narrower page.
- Added a viewer topbar restart button backed by `POST /restart`; it launches a replacement server process with the same arguments, exits the old daemon, polls for readiness, and reloads the page.
- Added a viewer topbar stop button backed by `POST /stop`; it exits the daemon manually, turns into a start button after the intentional shutdown, and prevents the browser from reconnecting until start is clicked.
- Added a viewer topbar hide/restore toggle, persisted in local storage, so the control banner can stay out of the reading view while leaving a small restore button available.
- Added Vim-style viewer keyboard navigation: `h`/`j`/`k`/`l`, `Ctrl-d`/`Ctrl-u`, `gg`/`G`, `/` search, `n`/`N` search repeat, and `Ctrl-o` to return to the previous recorded place.
- Made rendered MathJax SVG equations selectable/copyable as LaTeX by storing the original TeX on math nodes, selecting exactly one math node on click, and substituting TeX into the clipboard when the selection includes math.
- Added a viewer topbar `keys` toggle for LaTeX refkeys on labeled sections, theorem boxes, display equations, floats, and loose or secondary labels; the setting persists and is re-applied after websocket updates.
- Rendered refkeys for multi-row `align`/`gather`-style displays as row-level chips instead of one combined display label, so labels appear on separate lines when the key overlay is enabled.
- Suppressed duplicate refkey chips for boxed theorem-like environments by assigning the primary `\label{...}` to the outer theorem/proposition/lemma box and removing that label command from the rendered theorem body.
- Moved visible refkey chips into the page margin instead of placing them in the text/equation column, and kept row-level equation refkeys from wrapping below the display.
- Bumped the websocket shell protocol so already-open tabs reload once and pick up the new refkey toggle and align-numbering CSS.
- Made source-sync scrolling less jumpy: the browser now leaves the page still while the active source element is between 25% and 75% of the viewport, and scrolls to the 25% line only when it leaves that band.
- Preserved blank-line separators between inline math nodes, e.g. `$a^2$\n\n$b^2$`, by grouping top-level inline runs into real paragraph blocks instead of loose inline nodes.
- Preserved LaTeX paragraph semantics for blank lines: they no longer render as visible `<br><br>` gaps, but still start an indented paragraph after display math and inside proof/theorem text.
- Preserved LaTeX inter-word spacing when a single source newline separates inline math/refs from following prose, avoiding joined output such as math immediately followed by `and`.
- Preserved LaTeX inter-word spacing when a single source newline separates prose from following inline math, avoiding joined output such as `function\(v\cdot...\)` after wrapping a source line in nvim.
- Collapsed renderer-inserted soft-newline spacing with the following leading source whitespace, avoiding doubled spaces around inline math after line wraps.
- Preserved multiple front-matter authors declared with repeated `\author{...}` commands or top-level `\and` inside one author command, including AMS-style `\address{...}`, `\curraddr{...}`, and `\email{...}` metadata attached to the preceding author.
- Rendered `abstract` as front matter after the title block, even when the source declares the `abstract` environment before `\maketitle`.
- Spliced `\input`, `\include`, and `\subfile` content at the command site instead of appending included files after the root body.
- Added source-position offsets for parsed project chunks so flattened includes keep meaningful file, line, column, and byte metadata.
- Preserved UTF-8 text during parsing and inline rendering; non-ASCII prose no longer renders as mojibake.
- Rendered `subequations`, `abstract`, and `center` as transparent containers instead of raw opaque LaTeX blocks, including group labels that resolve to the next numbered equation.
- Resolved labels inside multi-label display environments such as `align` and `gather`, so each `\label{...}` in the body gets the display number and anchor.
- Prevented child `\label{...}` commands inside theorem bodies from overwriting the theorem/proposition label when the same label is encountered again during body rendering.
- Suppressed warnings for layout/font/graphics packages that are intentionally no-ops in the HTML preview.
- Treated proof-flow macros such as `\step` and `\case` as structured markers instead of unsupported MathJax macros.
- Rendered KFP-style proof-flow macros more faithfully: `\step` now increments and prints `Step N:`, `\case` prints Roman-numbered cases, `\restartsteps` resets the step counter, and `proofsteps` / `proofcases` environments reset their local counters instead of leaking raw macro syntax.
- Replaced raw `figure`/`table` dumps with compact float placeholders that render captions, inline math in captions, asset filenames, anchors, and Figure/Table reference numbers.
- Rendered `\includegraphics` assets in the live viewer: raster/SVG figures use `<img>`, PDF figures use cached, trimmed PNG previews generated through ImageMagick, and the server exposes guarded project-local assets under `/assets/...`.
- Preserved common `\includegraphics` sizing options in the live viewer: `width=0.8\textwidth` maps to `width: 80%`, absolute TeX units map to CSS units, `scale=...` is respected, and images keep their natural aspect ratio unless explicit width and height ask otherwise.
- Numbered subsections hierarchically, so Section 2 subsections render as `2.1`, `2.2`, etc. instead of repeating `2`.
- Numbered top-level rows in `align`, `gather`, `alignat`, and `eqnarray` separately, while respecting `\notag` / `\nonumber`, so labels and visible equation numbers match multi-line LaTeX displays.
- Preserved `subequations` as a numbered group: the group label resolves to the parent number, while numbered child equations/rows render and reference as `a`, `b`, `c` suffixes such as `(1.1a)`.
- Resolved `\ref`, `\eqref`, `\cref`, `\autoref`, and related reference commands inside MathJax math bodies before typesetting, so references embedded in displays such as `\text{... \eqref{H3}}` show the Rust-computed number instead of MathJax's unresolved reference output.
- Loaded bibliography files referenced by `\bibliography{...}` in the document body, resolving them relative to the main `.tex` file directory just like preamble `\addbibresource{...}` entries.
- Honored body-level `\bibliographystyle{plain}` declarations and rendered numeric references closer to BibTeX plain: sorted bibliography order, renumbered citations, first-name-first author/editor names, italic journal/book titles, cleaned protective braces, and arXiv/DOI metadata formatting.
- Relaxed the live `/buffer` renderability guard so ordinary in-progress LaTeX with unmatched braces or open environments still updates; it now defers only unclosed math delimiters.
- Rejected `/buffer` pushes whose `X-Mathpreview-Path` does not match the live root file.
- Discarded stale out-of-order `/buffer` and file-watch render completions before they can update `current` or broadcast websocket patches, so older editor buffers cannot overwrite the latest preview.
- Replaced unsafe id-based websocket patches with positional range patches plus block-id resync, so inserting paragraphs above existing content preserves order without forcing a full body update.
- Added a websocket protocol version query so already-open tabs with old patch JavaScript receive a one-time `full-reload` after the daemon restarts.
- Fixed the protocol-version gate after the source-sync protocol bump: the server now accepts WebSocket shell protocol 7, preventing a reload loop where every reconnect received `full-reload`.
- Bumped the websocket shell protocol to 8 so already-open tabs reload once and pick up the topbar hide/restore controls.
- Bumped the websocket shell protocol to 9 so already-open tabs reload once and pick up Vim navigation/search bindings.
- Bumped the websocket shell protocol to 10 so already-open tabs reload once and pick up the corrected soft-newline spacing.
- Added source-space anchors for blank paragraph-break lines inside environments, so cursor sync on an empty proof/theorem line scrolls to that whitespace position instead of the top of the enclosing environment.
- Added source-space anchors for soft single-newline line wraps inside paragraphs, so cursor sync after pressing Enter lands near that intra-paragraph space instead of falling back to the previous word and making the viewer jump backward.
- Changed editor-to-viewer cursor sync to use only leaf content targets such as source words, whitespace anchors, math, refs, and cites; broad block/proof/theorem container spans remain available for metadata but no longer pull the viewer to the start of an environment.
- Made blank lines inside proof/theorem text emit an actual zero-height paragraph break plus indentation, so a double Enter starts a new LaTeX-style paragraph instead of only widening the inline gap.
- Bumped the websocket shell protocol to 11 so already-open tabs reload once and pick up the soft-line source-sync anchors.
- Added math-aware viewer search: TeX-looking searches such as `\theta` now scan math `data-tex`, map common TeX symbols to MathJax SVG glyphs, highlight exact glyph hits inside equations, and keep `n` / `N` navigation working across the math hits.
- Bumped the websocket shell protocol to 12 so already-open tabs reload once and pick up the math-aware search code.
- Made Escape from the viewer search panel clear math-search highlights and the active browser selection, and limited math-search highlight restoration to the period when the search panel is visible.
- Bumped the websocket shell protocol to 13 so already-open tabs reload once and pick up the search-highlight cleanup behavior.
- Stopped intercepting normal mouse-down and double-click events on MathJax SVG math nodes, so browser text/glyph selection can work at the finest granularity the SVG output permits; Shift-click still selects a whole math node for LaTeX copying.
- Bumped the websocket shell protocol to 14 so already-open tabs reload once and pick up the math-selection interaction change.
- Removed the experimental transparent SVG math text-selection layer because it made the viewer more complex without producing reliable per-character selection; normal math clicks remain non-intercepting, and Shift-click still selects/copies the whole original LaTeX math node.
- Bumped the websocket shell protocol to 16 so already-open tabs reload once and drop the removed SVG math text-selection layer.
- Removed the leftover `position: relative` styling from math nodes and made the served viewer HTML `no-store`, so browser reloads cannot keep an old viewer shell with the removed SVG selection overlay.
- Bumped the websocket shell protocol to 17 so already-open tabs reload once and pick up the final cleanup.
- Fixed inverse search from rendered math by letting double-clicks on MathJax nodes use the same nearest `data-src` jump path as prose; normal click still focuses math, and Shift-click still selects/copies the whole original LaTeX node.
- Bumped the websocket shell protocol to 18 so already-open tabs reload once and pick up math inverse-search clicks.
- Updated live-server file watching so newly introduced include directories are added to the watcher set after renders.
- Cleared the live-server preamble cache after file-watch renders so later buffer pushes do not reuse stale preamble or bibliography state.
- Made `examples/mathpreview.sty` actually honor `proofs=...` by capturing proof bodies and rendering them only when the preceding theorem role is enabled.
- Made `examples/mathpreview.sty` accept the documented `proofs=main+supporting` option form.
- Made the companion theorem wrapper accept `[role=...]`, `[name=...]`, normal amsthm optional titles, and the documented trailing `{name}` group without requiring a name on every theorem.
- Added `cleveref` to `examples/paper.tex` so the demo compiles with its `\cref` commands.
- Stopped ignoring `Cargo.lock` in `.gitignore`.
- Cleaned up formatting and Clippy findings.

### Tests Added

- Regression test for source-order include flattening.
- Regression test for preserving Unicode text in the parser.
- Regression test for multiple front-matter authors.
- Regression test for source-order `abstract` placement after `\maketitle`.
- Regression tests for manual proof roles in the parser and renderer.
- Regression test for the viewer index/pages rail and A4 page-guide shell.
- Regression coverage for the viewer topbar hide/restore shell controls and persisted state.
- Regression coverage for the viewer shell's Vim navigation, search prompt, and previous-place jump-list hooks.
- Regression test for math copy metadata in rendered inline and display equations.
- Regression test for labeled item refkey metadata and the viewer refkey toggle shell.
- Regression tests for blank-line paragraph indentation in top-level text, display-math continuations, and proof/theorem text.
- Regression test for single-newline spacing after inline math.
- Regression test for single-newline spacing before inline math.
- Regression test for transparent `subequations` parsing.
- Regression tests for KFP-style float placeholders, proof-flow markers, and subequation group labels.
- Regression tests for numbered `\step`, `\case`, `\restartsteps`, `proofsteps`, and `proofcases` flow-marker rendering.
- Regression tests for `\includegraphics` width/ratio options and body-level `\bibliography{...}` resolution relative to the main file directory.
- Regression test for body-level `\bibliographystyle{plain}` sorting, citation renumbering, and BibTeX-plain reference formatting.
- Regression tests for per-row `align` numbering, `\notag` rows, and row-level `\eqref` resolution.
- Regression tests for `subequations` group labels, alphabetic child equation labels, unnumbered starred children, and post-group equation counter restoration.
- Regression test for resolving `\eqref` inside display/inline math bodies while keeping original LaTeX copy metadata.
- Regression coverage for row-level align refkeys and for suppressing duplicate boxed theorem refkeys.
- Regression test ensuring display label-only edits change the math reuse hash, so live refkey overlays cannot keep stale labels.
- Expanded regression coverage for hierarchical subsection numbering and rendered PDF figure assets.
- Regression test for the live buffer guard so it defers unclosed math without blocking ordinary partial LaTeX edits.
- Regression test for render-attempt sequencing so newer live renders invalidate older in-flight renders.
- Regression tests for shifted block insertions/deletions, shifted source metadata, generated display-math ids, and single-block edits using compact websocket range patches.
- Regression test ensuring the server accepts the current WebSocket shell protocol version and only reloads missing/old versions.
- Regression test ensuring a blank line inside an environment resolves to a whitespace source-sync anchor rather than the enclosing environment.
- Regression test ensuring a soft source line break inside a paragraph resolves to a whitespace source-sync anchor.
- Regression test ensuring forward source sync ignores environment container spans and targets nearby leaf text instead.
- Regression test ensuring nested blank lines emit paragraph-break markup as well as the LaTeX-style indentation marker.
- Viewer shell regression coverage for math-aware search hooks, TeX-symbol lookup data, SVG glyph highlighting CSS, and the protocol 12 reload gate.
- Viewer shell regression coverage for search-panel visibility gating and explicit search-session cleanup.
- Viewer shell regression coverage for normal math clicks focusing without forced whole-node selection and Shift-click preserving whole-node LaTeX selection.
- Viewer shell regression coverage for the protocol 17 reload gate after removing the experimental SVG math text-selection layer and final leftover math-node styling.
- Viewer shell regression coverage ensuring rendered math double-clicks are not blocked from the inverse-search jump path.
- Regression tests for `SyncIndex` lookup by source file/line/column, including smallest containing span and nearest previous fallback behavior.
- Regression test ensuring split paragraph blocks and source words are source-sync targets with independent `data-src` anchors.
- Viewer shell regression coverage for `source-cursor`, dynamic source scrolling, source highlighting, `/jump` posting, and `data-src` / source-anchor preservation across websocket patches.

### Documentation

- Updated `DESIGN.md` with the completed viewer work, current remaining work, a TODO checklist that crosses out completed items, a refkey-toggle status item, and a plain-language explanation of the live-update race/id-shift bug plus the final range-patch solution.
- Documented the first-pass nvim/HTML source sync design, endpoints, plugin behavior, word-level anchors, dynamic scroll band, and remaining precision tradeoffs.
- Expanded `README.md` with current viewer controls, restart/stop/start behavior, A4/dynamic layout controls, refkey overlays, toolbar hide/restore, Vim navigation/search bindings, math-node LaTeX copy, bibliography/figure support, and blank-line source-sync behavior.
- Extended `DESIGN.md` with the toolbar hide/restore persistence behavior and the `source-space` strategy for syncing empty lines inside environments.
- Documented Vim-style viewer navigation, search, and `Ctrl-o` previous-place behavior in `DESIGN.md`.

### Verified

- `cargo fmt --check`
- `cargo check`
- `cargo test` - 8 CLI tests and 61 core tests passing
- `cargo clippy --all-targets --all-features -- -D warnings`
- `git diff --check`
- `node --check` on the embedded viewer JavaScript
- `nvim --headless -u NONE -i NONE +'luafile examples/mathpreview.lua' +'qall!'`
- LargeTimeLangevin live-server smoke test for `/Users/tsv/Work/LargeTimeLangevin/new-main.tex` confirmed `GET /` returns 200 with `Cache-Control: no-store`, rendered prose contains `src-word` anchors, math nodes carry `data-src` anchors, the viewer serves WebSocket protocol 18, dynamic source-scroll JS is present, math-aware search code is served, search cleanup code is served, the experimental SVG math selection-layer code is no longer served, `POST /cursor` returns 204, and `/jump` posts/polls a source location.
- KFP live-server source-sync smoke test confirmed `POST /cursor` returns 204, `POST /jump` returns 202, `GET /jump?after=0` returns the pending source jump, and rendered blocks carry `data-src` anchors.
- KFP render smoke test for `/Users/tsv/Work/KFP/main.tex` confirmed no warning panel, no rendered opaque environment blocks, no raw `subequations` blocks, no sampled unresolved equation refs, and resolved Figure/Table refs.
- KFP render smoke test confirmed multi-row displays include per-row refkey chips and theorem/proposition/lemma boxes do not emit duplicate loose refkey anchors.
- KFP live-server smoke test confirmed `/assets/figures/comparison-longtime.png` returns `image/png`, `/assets/figures/2025-01-08_g_u_weak_cos_bsinxsint.pdf` returns `application/pdf`, and Section 2 subsections render as `2.1` and `2.2`.
- KFP live-server smoke test confirmed `/assets/figures/2025-01-08_g_u_weak_cos_bsinxsint.pdf?preview=png` returns a cached `image/png` preview and `POST /buffer` for the root file returns `204 No Content`.
- KFP render smoke test confirmed `\bibliography{bibo}` in the document body loads entries from `/Users/tsv/Work/KFP/bibo.bib`, producing 34 bibliography entries, and figure previews carry `width=0.8\textwidth` / `width=0.95\textwidth` as 80% / 95% CSS widths.
- KFP render smoke test confirmed the `abstract` block appears after the title block and before the first section.
- KFP render smoke test confirmed `\step` markers render as numbered `Step N:` labels, `\restartsteps` is not leaked, and math/prose spacing after step markers remains separated.
- KFP render smoke test confirmed `\bibliographystyle{plain}` is detected from the body, references are sorted and renumbered in plain style, 34 bibliography entries still render, protective braces are cleaned, and arXiv-style entries render compactly.
- KFP live-server smoke test confirmed the restarted daemon no longer serves stale `Test test test` / `$a^2+b^2$` buffer content while still rendering the current `First, note that if ...` manuscript line.
- KFP unsaved-buffer smoke test confirmed inserting `Test test test` / `$a^2+b^2$` before `First, note that if ...` is served in the correct order before save, then the daemon was restored to the saved file buffer.
- KFP websocket smoke test confirmed the same unsaved insertion now logs `1 ops` instead of a full `376 blocks` update.
- Render smoke test confirmed the default viewer loads `tex-svg.js`.
- Render smoke test confirmed inline/display equations include LaTeX clipboard metadata and the copy handler is present.
- Render smoke test for the live manuscript confirmed blank-line paragraphs no longer emit `<br><br>` gaps.
- Temporary `pdflatex` smoke test for explicit proof roles with `proofs=main`.
- Restart smoke test: `POST /restart` returned 202, then `GET /` returned 200 from the relaunched server.
- Stop smoke test: temporary daemon on port 23637 returned 202 for `POST /stop` and exited.
- `cargo run --quiet --bin mathpreview-cli -- render examples/paper.tex -o /private/tmp/mathpreview-analysis-fixed.html`
- `pdflatex -interaction=nonstopmode -halt-on-error -output-directory=/private/tmp/mathpreview-pdflatex paper.tex` from `examples/`
- Temporary `pdflatex` smoke tests for `proofs=main` and `proofs=main+supporting`

Note: the example PDF build still reports an undefined `Rudin1976` citation because the demo source cites it without shipping a `.bib` entry. The compile succeeds.
