# CHANGELOG-claude

## 2026-05-21

### Fixed

- Fixed `\ref{prop:foo}` (and other refs) that live inside a section title, theorem name, or float caption rendering as `<a class="ref" href="#…">…</a>` with no `data-target` / `data-kind`. Body refs have always had those attributes (set by `NodeKind::Ref` in `renderer.rs:1062-1083`), but the path through `render_inline_latex` (used for the title-like AST string fields) emitted a stripped-down anchor. The viewer's `buildMarginCard` reads the refkey chip from `link.getAttribute('data-target') || link.getAttribute('data-key')`; with no data-target a ref in a title produced a chip-less card with only the rendered number ("1") as fallback text. Worst-hit case: proposition / lemma refs in section titles, which is exactly what the user reported. Added `data-target` + `data-kind` to all three `render_inline_latex` ref emitters (`\ref`/`\pageref`, `\cref`/`\Cref`/`\autoref`, `\eqref`) — `crates/core/src/renderer.rs:1866-1903`. Regression test `refs_inside_inline_latex_fields_carry_data_target` renders `\section{See \ref{prop:foo} and \eqref{eq:x} and \autoref{prop:foo}}` and asserts each anchor carries both attributes.
- Fixed `\begin{align}` / `\begin{gather}` / `\begin{multline}` row refkey chips silently dropping clicks even though they looked clickable. Root cause: `.eq-refkey-list` (the vertical strip of chips next to the math) has `pointer-events: none` so the empty space between rows doesn't steal clicks meant for the math glyphs behind it — but `pointer-events: none` cascades to children, so the chips themselves were also ignored. Fixed by setting `pointer-events: auto` back on `.eq-refkey-chip` so just the chip rectangles receive clicks while the surrounding column still passes through. `crates/core/src/assets/default.css:566-588`.
- Fixed math inside theorem optional names (`\begin{lemma}[$Y$-energy]`) and inside section titles (`\section{The $L^p$ space}`) rendering as literal `$Y$` / `$L^p$` text instead of being typeset by MathJax. Both call sites in `write_node` were calling the math-blind `render_inline_latex` instead of the math-aware `render_latex_text_with_math` (which already existed as the established pattern for "this AST string field may contain inline math"). Two-line change in `renderer.rs:1088` and `:1117`, plus regression tests `theorem_optional_name_inline_math_is_typeset` and `section_title_inline_math_is_typeset`. (Tracked through the same session that did the renderer split — the fix landed under commit `41aa816`.)

### Added

- Added a `data-target` attribute to every server-rendered per-row equation refkey chip (`<span class="eq-refkey-chip" data-target="eq:foo" tabindex="0" title="pin eq:foo to margin">eq:foo</span>`) and dropped the `aria-hidden="true"` wrapper on `.eq-refkey-list`, since the chips are now interactive. `crates/core/src/renderer/math.rs:55-78`. Wired Enter/Space keydown activation on the `<span>` chips in the global keydown handler (`crates/core/src/assets/client/patch.js:330-338`) so keyboard users can still pin them.
- Added the **vim-style command line** that opens when the user presses `:` (outside an editable target). Hidden by default; an input strip pinned to the bottom of the viewport. Three commands today: `:pin <key>` pins the matching target as a margin card (or scrolls the existing card into view if already pinned), `:unpin <key>` removes the matching card, `:clear` empties the margin. `:p` / `:u` are aliases. Enter executes, Esc closes, empty-Backspace also closes. Errors surface in a feedback span ("no \\label by that name", "not pinned", "unknown command: …"). `crates/core/src/assets/client/viewer.js:818-1000` for the implementation; `crates/core/src/assets/client/viewer.js:393-396` wires the `:` keybind inside `handleVimNavigation`, alongside `/` for search.
- Added **wildmenu-style fuzzy completion** for the `:pin` / `:unpin` argument. As the user types `:pin <prefix>`, a strip above the input lights up with every refkey whose key fuzzy-matches the prefix. Sources unified: `[data-refkey]` on theorems / sections / equations / floats, `[data-target]` on per-row `.eq-refkey-chip`, `[data-key]` on `<dt>` bib entries. For `:unpin` the strip narrows to keys currently in `pinnedRefs`. Scoring is substring-beats-subsequence, prefix-beats-mid-string, ties broken by shorter then alphabetical. Top 12 shown with `+N more` tail. Tab cycles forward, Shift+Tab cycles backward, ArrowDown/ArrowUp also cycle; clicking a chip (mousedown so the input doesn't blur) commits and runs the command immediately. `crates/core/src/assets/client/viewer.js:825-975`.
- Added **drag-to-reorder margin cards**. Each card grows a `.margin-card-grip` indicator (`⋮⋮`) in its header as a visual signal; the entire card is the draggable source. Drop targets are indicated by an inset 2 px accent line at the appropriate edge of the card under the cursor (above vs below by which half of its bounding box the pointer is in). Dropping on empty space inside `#margin-cards` appends to the end. Implemented as event delegation on `#margin-cards` with an idempotent `data-dnd-init` guard (`initMarginDnd` in `viewer.js`). `pinnedRefs` is key → element; its iteration order doesn't drive layout (the DOM does), so a drop just moves the node and the Map keeps its mapping intact.
- Added **clickable refkey chips in the left margin**. The "keys" toggle previously painted refkey labels via a CSS `::after` pseudo-element on every `[data-refkey]:not(.label-anchor)`. Pseudo-elements aren't real DOM nodes, so the chip looked pinnable but wasn't. Converted to a real `<button class="refkey-chip" data-target="…">` injected by `decorateRefkeyChips()` (idempotent via a `data-refkey-decorated` flag on the parent so post-patch runs skip already-tagged elements). Called once at bootstrap and after every patch / body-updated WS event. The existing global click handler routes a click on `.refkey-chip` (or the per-row `.eq-refkey-chip[data-target]`) through `pinByRefkey`, the same path the typed `:pin` input uses. CSS migrated from `::after` rules to `.refkey-chip` rules with the same layout offsets (math.display = vertical center, thm = `top: 0.72em`, float-placeholder = `top: 0`). `crates/core/src/assets/client/viewer.js:819-844`, `crates/core/src/assets/default.css:780-824`.
- Added two HTTP-layer integration tests for multi-file editing: `buffer_push_with_child_path_splices_override_into_root_render` and `buffer_push_rejects_path_outside_project` in `crates/cli/src/serve.rs`. Both spin up a real temp `main.tex` + `child.tex` on disk, build an `AppState` manually, and call `serve_buffer_push` directly with `X-Mathpreview-Path`. First test asserts the override map is keyed by canonical child path, the rendered body contains the override token, the disk content is not in the body, surrounding root content is preserved, and the disk file is left alone. Second asserts a path outside the project gets `400` and the override map stays empty. (Prose is split into per-word source-sync spans, so the assertions check for the wrapped form `>Livechild<` rather than a contiguous substring.)

### Changed

- **Split `renderer.rs`** (~4300 lines) into focused submodules under `crates/core/src/renderer/`:
  - `util.rs` (~400 lines): `escape_*`, `sanitize_id`, role helpers, `latex_command_*` token parsing, brace/bracket helpers, latex dimension/number parsing, `fnv_hash`, `asset_url`, `data_src`.
  - `shell.rs` (~190 lines): `wrap_in_shell`, `warnings_panel`, embedded `CLIENT_JS` (`concat!(include_str!(…), …)`) / `DEFAULT_CSS` assets.
  - `bib.rs` (~300 lines): `format_bib_entry`, author/name formatting, `bib_text_html`, `normalize_bib_latex`/`whitespace`, `strip_bib_protective_braces`.
  - `math.rs` (~650 lines): equation numbering + row refkeys, `strip_labels`, `resolve_math_refs` + `math_ref_kind`, `label_alias_anchors`, math-row splitting (`split_math_rows` / `latex_env_command_end` / `skip_row_separator_spacing`), `render_latex_text_with_math`, `write_float_placeholder` + `includegraphics_attrs` + `latex_dimension_to_css` + `parse_graphics_options`, `write_flow_marker`.
  - `renderer.rs` (now ~2830 lines): keeps the public API (`HtmlOptions`, `RenderOutput`, `render`), `RenderCtx` + `IdGen`, the `write_node` dispatcher, paragraph/source-span helpers, `render_inline_latex` (marked `pub(super)` so `bib.rs` and `math.rs` can call back), and the test module.
- **Split `client.js`** (~2300 lines) into five files under `crates/core/src/assets/client/`, line-aligned (no rewrap) so the concatenation is byte-equivalent to the original:
  - `header.js` (~130 lines): IIFE open, all `var` state, DOM/scroll helpers, vim-pending tracking, `isEditableTarget`, viewer-place jump stack.
  - `viewer.js` (~1000 lines, since grown by the new features): search panel + math search, vim navigation, source-jump helpers, side panel, margin cards (clone/pin/hover), topbar hide/show, page mode + scale + guides, sidenote layout, navigation refresh, active-page tracking.
  - `proof.js` (~250 lines): initial-typeset refresh, theorem-role detection, `applyMode`, server stop/restart/start, `requestPrint`.
  - `patch.js` (~760 lines): math selection / copy-as-LaTeX, the global event-delegation block, `applyPatch` + block/math reuse helpers, mathjax-bridge typeset queue + observer.
  - `footer.js` (~180 lines): `memSuffix` + WebSocket connect (live / body-updated / source-cursor / full-reload / error events) + initial bootstrap + IIFE close.
- `renderer/shell.rs` glues the five JS pieces with `concat!(include_str!("../assets/client/header.js"), include_str!("../assets/client/viewer.js"), …)`. They share scope because they sit inside one outer `(function() { ... })()` IIFE (`header.js` opens it, `footer.js` closes it).
- `package.json` `lint` script splits into `lint:client` (which concatenates the five files in the same order `shell.rs` does and pipes the result to `eslint --stdin --stdin-filename=…/client.js` so the existing flat config still applies) and `lint:engine` (the MathJax adapter, single-file). `no-undef` still catches typos across files because ESLint sees the assembled bundle as one virtual file.
- Removed the always-visible "type a \\label key" input that was briefly at the top of the margin column (along with its `.margin-toolbar` / `.margin-pin-input` / `.margin-pin-feedback` HTML and CSS). The `:`-triggered command line replaces it.
- The README's `Viewer controls` section grew margin-card details: drag-from-grip reorder, click-the-left-margin-refkey-chip, and the `:`-command-line + Tab completion mechanics. The vim-bindings bullet now lists `:` alongside `/` and `Ctrl-o`. The Layout tree was updated to show `renderer/` (now a directory of submodules) and `assets/client/` (now five `.js` pieces). The Roadmap entry "Split `client.js` and `renderer.rs`" was flipped from ◯ to ✓, and the stale "Multi-file editing" entry was removed from the "What's not done yet" list (the substitution map and HTTP wiring were already in place; the new commit just adds test coverage).
- DESIGN.md `Layout` tree now shows `renderer/` (focused submodules) and `assets/client/` (five `.js` pieces sharing one IIFE, concat!'d).

### Removed

- Removed the always-visible margin-toolbar input (`#margin-pin-input` + `#margin-pin-feedback`) and its CSS, replaced by the `:`-triggered cmdline.
- Removed the CSS `body.refkey-visible [data-refkey]:not(.label-anchor)::after` chip-painting block (and its per-element offset rules); the same visual is now produced by a real `<button class="refkey-chip">` injected by JS so the chip is clickable.
- Removed the stale "Multi-file editing" entry from the README's "What's not done yet" section — the buffer-substitution code path was already wired end-to-end; the new HTTP-layer tests just lock it in.

### Verified

- `cargo test --workspace`: 19 passed (cli) + 73 → 74 passed (core) + 0 doctests, no failures.
- `cargo clippy --tests --workspace`: clean.
- `cargo fmt --check`: clean.
- `npm run lint`: clean (the concatenated bundle parses with `no-undef` enabled; no cross-file reference broken).
- `viewer_shell_contains_index_pages_and_a4_guides` asserts `id="cmdline"`, `id="cmdline-input"`, `id="margin-cards"`, `pinByRefkey`, `openCmdline`, `decorateRefkeyChips`, `margin-card-grip`, `initMarginDnd`, and the bumped `WS_PROTOCOL_VERSION = '51'` are all present in the rendered shell.
- `cargo run -q -p mathpreview-cli -- render` on a small `\begin{align} … \label{eq:a}\\ … \label{eq:b}\end{align}` document confirms both `<span class="eq-refkey-chip" data-target="eq:a" tabindex="0" title="pin eq:a to margin">eq:a</span>` and `<span class="eq-refkey-chip" id="eq-b" data-target="eq:b" tabindex="0" title="pin eq:b to margin">eq:b</span>` appear in the body.

### Committed

- `8d1f468` viewer: cover multi-file editing with HTTP tests
- `41aa816` viewer: split renderer.rs into focused submodules (bundles the math-in-titles fix)
- `2bf8c6d` viewer: split client.js into five scope-sharing pieces
- `a7a6323` viewer: tag inline-latex refs with data-target so margin chips show
- `8c6de28` viewer: typed-refkey input pins a card without scrolling to \\ref (now superseded by the `:`-cmdline; the underlying `pinByRefkey` was kept)
- `18322d2` viewer: drag margin cards to reorder
- `d00990d` viewer: click the left-margin refkey chip to pin its target
- `f26ca07` viewer: replace margin pin input with a vim-style : command line
- `d29f28f` viewer: let align row refkey chips receive clicks
- `c0ca6ef` viewer: fuzzy completion + Tab cycling for :pin / :unpin

### WS protocol version

- Bumped 45 → 51 across the session (one bump per user-visible JS behaviour change so existing tabs full-reload onto the new bundle).

## 2026-05-20

### Fixed

- Fixed math glyphs rendering as invisible (empty bounding boxes) after the recent `tex2svgPromise`-based engine adapter landed. Root cause: `MathJax.tex2svgPromise()` skips MathJax's `updateDocument()` → `addPageElements()` → `pageElements()` chain, which is the only code path that injects the `<svg id="MJX-SVG-global-cache">` element. With `fontCache: 'global'` (MathJax 4's default for SVG output), every glyph in every rendered equation pointed via `<use href="#MJX-NCM-...">` at an ID inside that cache — but the cache element was never in the DOM, so the references resolved to nothing. The page had 277 `mjx-container`s with width/height set but `pathCount: 0`, `useCount: 12319`, `hasGlobalCache: false`. Fixed by setting `fontCache: 'local'` in `mathjax_config()` so each math element inlines its glyph `<path>` definitions into its own `<defs>`. Verified by CDP probe: 277 maths → 3857 inlined paths, 277 local `<defs>`, `<use>` bounding boxes non-zero. `crates/core/src/engines/mathjax.rs:184-194`.

### Added

- Added a `print` toolbar button (`#print-button`) that compiles a real LaTeX PDF on demand and opens it in a new tab. Wired through a `POST /print` route on the daemon (`crates/cli/src/serve.rs`) that shells out to `latexmk -pdf -interaction=nonstopmode -halt-on-error -synctex=1` in the root file's directory and falls back to a single `pdflatex` pass when `latexmk` isn't on `$PATH`. Response is streamed as `application/pdf` with an `inline` content-disposition; the client wraps it in a blob URL and `window.open()`s a new tab (falling back to same-tab navigation if a popup blocker refuses). Failures return `500` with `{"error": "<log tail>"}` and the button surfaces the log message as a tooltip on the toolbar.
- Added `locate_compiled_pdf()` in `serve.rs` to discover where the compile actually wrote its output by parsing the run log for the two lines latexmk/pdflatex always emit: `Output written on <path>.pdf (...)` (fresh pdflatex run) and `Latexmk: All targets (<path>.pdf) are up-to-date` (no-op run). Honours every `$out_dir` / `$aux_dir` value a project's `.latexmkrc` (or `~/.latexmkrc`) sets without modelling latexmk's config language; falls back to a small set of common subdirectories (`./`, `build/`, `out/`, `_build/`, `_output/`) if neither log line matches. Verified against a project with `$out_dir = 'build'` set in `~/.latexmkrc`: the daemon returned the exact byte content of `build/new-main.pdf` (493 518 bytes match).
- Added New Computer Modern web fonts as the default body typeface so prose visually matches MathJax's SVG math glyphs (same NCM 10pt family by Antonis Tsolomitis). Vendored four `WebCM Serif 10` woff2 files (Regular, Italic, Bold, BoldItalic, ~800 KB total, OFL-1.1) under `crates/cli/vendor/newcm-text/`, with the LICENSE file. Files are embedded into the binary via `include_bytes!` in `serve.rs` so the release executable serves them without needing the surrounding source checkout at runtime; `vendor/mathjax/` stays on disk because the MathJax bundle is too large (~13 MB) to embed.
- Added a `GET /vendor/newcm-text/*path` route in `serve.rs` plus four `@font-face` declarations in `default.css` registering the family as `'NewCM Text'`. The font is placed first in the `html, body` font stack and in the `.sidenote-content` stack; system fallbacks (`'Latin Modern Roman'`, `'CMU Serif'`, `'STIX Two Text'`, ...) are preserved beneath it.
- Added `scripts/vendor-newcm-text.sh` mirroring `scripts/vendor-mathjax.sh`: `npm pack web-computer-modern`, keep only the four Serif 10 woff2 files plus the license, drop the unused Sans / Mono / Math / Devanagari / Uncial / 08pt variants. ~800 KB vendored instead of ~9 MB raw package.

### Changed

- Reorganised the toolbar from a single overflow-prone flex row into two rows that stay aligned at every viewport width. Row 1 holds doc title + path on the left and the live-reload status pill on the right (`margin-left: auto`). Row 2 holds the action buttons, with view toggles (`A4|dynamic`, `keys`, `margin`, `main|+supp|all`) on the left and state-changing actions (`print`, `restart`, `stop`) pushed to the right by a `.topbar-actions-spacer`. At ≤720 px the action row wraps onto multiple visual lines without spilling under the status pill. `crates/core/src/renderer.rs` topbar block + `crates/core/src/assets/default.css`.
- Introduced a `--topbar-height` CSS variable (78 px at default, 110 px at ≤720 px) and rewired everything that previously used hard-coded offsets to reference it: `.side-toggle` (`top: calc(var(--topbar-height) + 4px)`), `.side-panel` (`top: calc(var(--topbar-height) + 32px)`), `body.margin-mode aside#margin` (`top: var(--topbar-height)`). Adding or removing rows in the topbar is now a single-value edit.
- Beautified the toolbar buttons: 5 px rounded corners, 100 ms hover transition (`background-color`, `border-color`, `color`), `--border-strong` (`#c8c4ba`) hover border tint, `--button-bg-hover` (`#f6f4ee`) hover fill. Segmented controls (`.page-mode-toggle`, `.proof-toggle`) get a true connected-pill look via `overflow: hidden` on the wrapper plus shared first/last-child corner radii on the children. Active state unchanged (accent purple).
- Browser tab `<title>` now uses `\title[short]{long}`'s optional argument when present, falling back to the file stem (previous behaviour). The same value still drives the topbar's bold `.topbar-doc-title` chip. `crates/core/src/renderer.rs:783-795`.
- README "Viewer controls" gained a `print` paragraph documenting the latexmk/pdflatex behaviour and the log-parsing approach for custom `$out_dir`. "MathJax and offline setup" gained a paragraph documenting the NCM body font (vendored, embedded via `include_bytes!`, OFL-licensed). "Layout" tree now shows `vendor/newcm-text/` and `scripts/vendor-newcm-text.sh`.

### Removed

- Removed the editor-handoff path that an earlier iteration of `/print` had added (an `/print` GET poll endpoint, `PrintRequest`/`pending_print`/`last_print_poll` state in `AppState`, and a matching `print_handler` / `start_print_poll` / `default_print_handler` block in `examples/mathpreview.lua`). The nvim plugin is back to polling only `/jump`, and the daemon's `POST /print` always runs `latexmk` itself. The editor-handoff was working but added a 250 ms background poll that the user (correctly) didn't want for a click-only action; the simpler always-daemon path covers the use case without polling.
- Removed the brief accent-purple tint on the print button (`--accent-soft: #efeaf8` background, `#3a2773` text, `font-weight: 600`) — it read as pink against the rest of the toolbar and broke the visual uniformity the two-row layout was meant to restore. The button now matches the other action buttons (white background, gray border, same hover state). The unused `--accent-soft` palette entry was dropped from `:root`.
- Removed the unused `.topbar-spacer { flex: 1; }` rule along with the `<span class="topbar-spacer">` it served; the two-row layout uses `margin-left: auto` on the status pill and a dedicated `.topbar-actions-spacer` div instead.
- Removed the old hard-coded `top: 52px` / `top: 60px` / `top: 86px` constants from `.side-toggle`, `aside#margin`, and `.side-panel`. Replaced with `calc(var(--topbar-height) + N)` expressions so a topbar resize no longer requires hunting through CSS.

### Verified

- `cargo build --bin mathpreview-cli` — clean.
- `cargo test -p mathpreview-core` — 67 passed; 0 failed.
- Headless-Chrome CDP probe against the running daemon confirmed:
  - 277 `mjx-container` elements render with `pathCount: 3857` and `defsInMjx: 277` (was `pathCount: 0` before the `fontCache: 'local'` fix).
  - `NewCM Text` family registers in `document.fonts` with `normal/400`, `italic/400`, and `normal/700` loaded after page load.
  - Topbar height = 75 px at 1400 × 900 viewport; 104 px at 520 × 900 (action row wraps once); side-toggle pill anchors flush below the topbar in both.
  - `POST /print` returns 200 `application/pdf` with bytes byte-identical to `~/Work/LargeTimeLangevin/build/new-main.pdf` (the project's actual latexmk output dir, set via `~/.latexmkrc`'s `$out_dir = 'build'`). `GET /print` returns 405 (POST-only).
  - `GET /vendor/newcm-text/woff2/WebCM%20Serif%2010%20Regular.woff2` returns 200 / `font/woff2` / cache-control immutable. Verified the embedded-bytes path by moving `vendor/newcm-text/` aside on disk: the daemon still served the byte-identical font, confirming `include_bytes!` won.
- Browser `<title>` reads `Large time KFP` (the project's `\title[short]{long}` short arg) instead of the previous `new-main` (file stem).
- WS protocol version bumped 39 → 45 across the session (each user-visible behaviour or layout change bumped it once to force tab reload).

### Committed

- _Uncommitted at session close — user may bundle or split per preference._

## 2026-05-18

### Added

- Added an engine abstraction (`crates/core/src/engines/`) with a `MathEngine` trait, an `Engine` dispatch enum, and a concrete `MathJaxEngine` implementation, so the renderer no longer hard-codes MathJax-specific HTML, JS, or CSS.
- Added a `window.__mpEngine` browser-side shim — provided per-engine via `MathEngine::client_adapter_js()` — that the shared `CLIENT_JS` bundle calls into for `ready` / `isReady` / `typesetClear` / `typeset` instead of touching `window.MathJax` directly.
- Added `HtmlOptions.engine: Engine` (default `Engine::MathJax(MathJaxEngine::default())`) replacing the prior `mathjax_url: String` field, and re-exported `Engine`, `MathEngine`, and `MathJaxEngine` from the `mathpreview_core` crate root.

### Changed

- Extracted the 1705-line `CLIENT_JS` constant to `crates/core/src/assets/client.js` and pulled it in via `include_str!`, so the live-viewer frontend is now editable as real JavaScript with editor syntax highlighting, grep/jump-to-def, and a path for eslint/prettier integration.
- Extracted the 622-line `DEFAULT_CSS` constant to `crates/core/src/assets/default.css` via `include_str!`, on the same principle as `CLIENT_JS`.
- Extracted the MathJax-specific `ADAPTER_JS` and `EXTRA_CSS` constants to `crates/core/src/engines/assets/mathjax.{js,css}`, also via `include_str!`, keeping the engine bundle self-contained alongside the renderer assets.
- Moved `mathjax_config()` (the `window.MathJax = {...}` block builder) and its `json_string` helper out of `renderer.rs` and into `engines/mathjax.rs`, so the AST → HTML walk no longer assembles engine-specific config text.
- Rewired `wrap_in_shell` to consult `opts.engine.as_dyn()` for the `<head>` script tags, the adapter JS appended before `CLIENT_JS`, and the engine-specific CSS appended after `DEFAULT_CSS`. The shell template no longer references MathJax in code.
- Moved the `.math.display mjx-container[display="true"]` overflow/margin rule out of `DEFAULT_CSS` into the MathJax engine's `extra_css`, so swapping engines no longer leaves dead CSS in the page.
- Rewired the four MathJax-direct call sites in `CLIENT_JS` (`refreshAfterInitialMathJax`, `clearRemovedMath`, the `typesetPromise` guard, and the `typesetPromise` await) to use the `window.__mpEngine` shim and renamed the wait helper to `refreshAfterInitialEngine`.
- Renamed user-facing typeset status text from `"MathJax error"` and `"mathpreview MathJax:"` log prefixes to engine-neutral wording (`"engine error"`, `"mathpreview engine:"`) so error messages match the engine in use.
- Updated the CLI's `--mathjax-url` flag (preserved for back-compat) to construct `Engine::MathJax(MathJaxEngine::new(url))` instead of setting the removed `mathjax_url` field.

### Removed

- Removed the inline `mathjax_config` and `json_string` functions from `renderer.rs` after they were moved to `engines/mathjax.rs`.
- Removed the inline `CLIENT_JS`, `DEFAULT_CSS`, `ADAPTER_JS`, and `EXTRA_CSS` raw-string constants from `renderer.rs` / `engines/mathjax.rs` after extracting them to sibling asset files.

### Refactor

- Shrank `crates/core/src/renderer.rs` from 6235 lines to 3825 lines (-2410, -39%) by moving the frontend bundles out of Rust raw strings and the MathJax-specific Rust into the engine module.
- Concentrated MathJax knowledge into `crates/core/src/engines/mathjax.rs` and its sibling `assets/`. The only mentions of MathJax left in `renderer.rs` are descriptive doc comments explaining why certain HTML patterns are chosen.

### Verified

- `cargo fmt --check`.
- `cargo check`.
- `cargo clippy --all-targets --all-features -- -D warnings`.
- `cargo test` — 61 core tests + 8 CLI tests passing.
- `cargo run --bin mathpreview-cli -- render examples/paper.tex -o /tmp/...` produced an HTML page whose `window.__mpEngine`, `window.MathJax`, `mjx-container`, and `tex-svg.js` marker counts were byte-identical between before and after the asset extraction.
- Confirmed `git diff` shows three modified files (`crates/cli/src/main.rs`, `crates/core/src/lib.rs`, `crates/core/src/renderer.rs`) and six new files (`engines/mod.rs`, `engines/mathjax.rs`, `engines/assets/mathjax.js`, `engines/assets/mathjax.css`, `assets/client.js`, `assets/default.css`).

### Committed

- `1529a8c factor renderer into engine-neutral shell + extract frontend assets`
