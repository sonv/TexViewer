# CHANGELOG-GPT

## 2026-05-16

### Performance

- Reused unchanged math nodes during block-level patch replacement, so prose edits in blocks that contain math no longer call MathJax for identical expressions.
- Restored ASCII fast paths in parser advancement and inline text rendering after the UTF-8 correctness fix.
- Kept Unicode text preservation by decoding UTF-8 only for non-ASCII bytes instead of every byte in normal LaTeX prose and math scans.
- Verified cached `/buffer` timing on `examples/paper.tex` at 1 ms locally after an initial 8 ms preamble-cache miss.

### Fixed

- Added a viewer topbar restart button backed by `POST /restart`; it launches a replacement server process with the same arguments, exits the old daemon, polls for readiness, and reloads the page.
- Preserved blank-line separators between inline math nodes, e.g. `$a^2$\n\n$b^2$`, by grouping top-level inline runs into real paragraph blocks instead of loose inline nodes.
- Spliced `\input`, `\include`, and `\subfile` content at the command site instead of appending included files after the root body.
- Added source-position offsets for parsed project chunks so flattened includes keep meaningful file, line, column, and byte metadata.
- Preserved UTF-8 text during parsing and inline rendering; non-ASCII prose no longer renders as mojibake.
- Rejected `/buffer` pushes whose `X-Mathpreview-Path` does not match the live root file.
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

### Verified

- `cargo fmt --check`
- `cargo check`
- `cargo test` - 27 core tests passing
- `cargo clippy --all-targets --all-features -- -D warnings`
- Restart smoke test: `POST /restart` returned 202, then `GET /` returned 200 from the relaunched server.
- `cargo run --quiet --bin mathpreview-cli -- render examples/paper.tex -o /private/tmp/mathpreview-analysis-fixed.html`
- `pdflatex -interaction=nonstopmode -halt-on-error -output-directory=/private/tmp/mathpreview-pdflatex paper.tex` from `examples/`
- Temporary `pdflatex` smoke tests for `proofs=main` and `proofs=main+supporting`

Note: the example PDF build still reports an undefined `Rudin1976` citation because the demo source cites it without shipping a `.bib` entry. The compile succeeds.
