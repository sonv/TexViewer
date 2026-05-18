# CHANGELOG-claude

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
