# CHANGELOG-claude

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
