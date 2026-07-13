# Development, verification & release workflow

How changes to mathpreview get built, verified, shipped to `dev`, and released.
This is the narrative version — the operational checklists live in the
[`ship` skill](.claude/skills/ship/SKILL.md), which Claude Code loads
automatically when working in this repo. Keep the two in sync.

## The delivery model (why version discipline matters)

The nvim plugin (`lua/mathpreview/init.lua`) and the `mathpreview-cli` binary
install **separately**: the plugin updates with a `:Lazy update`, the binary
only when something rebuilds it. The plugin auto-reinstalls a stale binary on a
*fresh* `:MathPreview` start, but the daemon-reuse path (which lets
`:MathPreview` reuse an open tab) skips that check — so a running daemon can
keep serving an old binary long after the plugin updated. Every mechanism below
exists to keep those two halves — plus the *third* half-installed thing, the
browser tab's cached client JS — from skewing silently:

- **`PLUGIN_VERSION` (init.lua) ↔ workspace `version` (Cargo.toml)** are bumped
  in lockstep for every user-visible change, even plugin-only ones, so
  `--version` comparisons stay meaningful and releases are coherent.
- **`WS_PROTOCOL_VERSION`** is duplicated in `crates/cli/src/serve.rs` and
  `crates/core/src/assets/client/footer.js`. They must match (the
  `client_ws_protocol_matches_server` test guards it). Bump **both** whenever a
  WebSocket message's shape or semantics change — the mismatch makes the server
  full-reload stale tabs exactly once, which is the upgrade mechanism. A missed
  bump means old client JS misinterprets new messages; mismatched constants
  mean an infinite reload loop.

## Per-change workflow

1. **Reproduce first, against reality.** Prefer the user's actual document and,
   for "it still doesn't work" reports, the *running* daemon (`/debug` reports
   `ws_protocol` and connected `clients`; `ps` + binary mtime tell you what's
   actually executing). The most common phantom bug is a **stale binary**:
   check `~/.cargo/bin/mathpreview-cli --version` before touching code. Fix:
   `cargo install --path crates/cli --force` + `:MathPreviewRestart`.
2. **Fix, then verify end-to-end** (see cookbook below). Client/plugin code
   can't be exercised headlessly here — compensate with served-bundle greps,
   protocol-level WS observation, and adversarial review for anything
   nontrivial.
3. **Quality gates** — all of them, every time:
   - `cargo test --workspace`
   - `cargo clippy --workspace` (zero warnings)
   - `npm run lint` (client JS)
   - `luajit -bl lua/mathpreview/init.lua` (plugin parses)
   - **rustfmt, targeted**: several files carry pre-existing format drift
     (renderer.rs, serve.rs, parser.rs historically). Do not reformat the
     world; instead check `cargo fmt --check` output touches none of *your*
     identifiers, and hand-format your own lines until it doesn't.
4. **Bump + changelog**: workspace `Cargo.toml` and `PLUGIN_VERSION` to the
   next patch version; `cargo build` refreshes `Cargo.lock` (CI builds with
   `--locked` — a stale lock fails the release). Add a user-facing
   `CHANGELOG.md` entry dated with **`date -u +%Y-%m-%d`** (never assume the
   date; long sessions cross midnight).
5. **Commit and push to `dev`.** Explain root cause and mechanism in the commit
   body, note what was verified. There is **no PR CI** — only release-time CI
   on `v*` tags — so the local gates are the only net.

## Releases

Only on explicit request — releases are outward-facing. The flow:

1. **Pre-flight**: clean tree; `origin/main` is an ancestor of `origin/dev`
   (the merge is always a fast-forward: `git push origin dev:main`); the tag is
   free; Cargo.toml = Cargo.lock = PLUGIN_VERSION; WS protocol pair matches;
   CHANGELOG has the entry.
2. **Tag**: annotated `vX.Y.Z` on the dev head; push it. Transient SSH failures
   happen — verify with `git ls-remote --tags` and retry.
3. **CI** (`.github/workflows/release.yml`) builds and tests 4 targets (macOS
   arm64/x86_64, Linux x86_64/arm64) and creates a **draft** release with
   tarballs + SHA-256 checksums. Watch it: `gh run watch <id> --exit-status`.
4. **Publish**: replace the auto-notes with curated, theme-grouped notes
   distilled from the CHANGELOG span since the previous release, then
   `gh release edit vX.Y.Z --notes-file … --draft=false --latest`. Verify
   `isDraft=false`, the Latest marker, and 8 assets.

## Verification cookbook

Patterns that have proven out, with the traps that motivated them:

- **Serve a scratch doc**: write to a scratch dir, `mathpreview-cli serve … --port 277xx`,
  poll `curl -m1` with the `Host: 127.0.0.1:<port>` header until up (the daemon
  has a host guard). Kill by PID *and* `lsof -ti tcp:<port>` afterward.
- **Generate test .tex with `printf '%s\n'`, never `echo`** — shell `echo`
  interprets `\n`/`\a` inside LaTeX like `\newcommand{\alpha…}` and silently
  mangles the file (this produced a spectacular false lead once).
- **Observe the protocol, not the pixels**: connect a `websocket-client`
  Python client to `ws://…/ws?v=<WS_PROTOCOL_VERSION>` (with matching
  `host=`/`origin=`) and assert on broadcast events (`source-cursor`,
  `search-sync`, `patch` payload composition, timing).
- **`POST /buffer` takes the RAW buffer text** with the path in the
  `x-mathpreview-path` header — it is *not* JSON. Pushing JSON silently renders
  the JSON as a one-line LaTeX document and produces garbage measurements.
- **Perf numbers come from `--release` builds**, timed end-to-end (POST →
  WS event arrival), and payloads get *dissected* (which field is big, per
  block?) before optimizing anything.
- **Served-bundle grep**: client JS/CSS correctness at minimum means the new
  functions/rules appear in the served page (`curl / | grep -F`), and removed
  paths don't.
- **`timeout(1)` does not exist on macOS**; sub-second sleeps via
  `perl -e 'select(undef,undef,undef,0.2)'`.
- **Adversarial review for unrunnable code**: nontrivial client/plugin changes
  get a multi-lens review (find → adversarially verify each finding, reviewers
  told to *refute*). It has repeatedly caught real bugs (wrong-window focus,
  clipped popovers, comment-handling in extractors). Trap learned the hard way:
  **never `git add -A` while review agents are running** — a verifier's live
  scratch edits once rode into a commit and broke `cargo test` at HEAD. Stage
  explicitly, or commit only after the workflow completes.

## Architecture notes that keep biting

The large-document performance architecture (patch deltas, block-scoped ids,
containment, lazy/background typesetting, print flushing) has its own document
with measurements and invariants: **[PERFORMANCE.md](PERFORMANCE.md)**. Read it
before touching `broadcast_render`, `IdGen`, the typeset queue, or anything in
the per-patch client path.

- **Positions are absolute `file:line:col`** in `data-src`/sync anchors. Any
  edit that changes the line *count* legitimately shifts every later anchor;
  within-line edits shift only their own block. The patch protocol deltas
  anchor metadata against `last_blocks` (unchanged positions ship as `0`) —
  keep that invariant when touching `broadcast_render`/`syncPatchBlockMetadata`.
- **Block ids are positional** (`blk-N`): inserts/deletes renumber the tail, so
  block metadata lists must stay positional and full-length.
- **`SyncKind`**: `Leaf` (point-lookup + flash), `Block` (in ranges, excluded
  from point-lookup so headings don't flash — cursor *follow* uses the
  scroll-only / nearest-element fallbacks), `Container` (excluded from both).
- **The parser's inline vs block split**: unknown commands become `OpaqueCmd`
  nodes; anything rendered through `render_inline_latex` has **no `ctx`** —
  features needing state there (footnote numbering) use thread-locals, and
  anything needing `data-src` must be wrapped by the caller that has the span.
- **Extractors must strip `%` comments** (`macros::strip_line_comments`) before
  regex-scanning preamble source — trailing-`%` continuation is the standard
  multi-line def style, and commented-out defs must not win.

### The margin overlays (`keys` chips, line numbers) — why they're built the way they are

The `keys` feature broke three times before landing on this architecture.
Every rule below exists because violating it produced a user-visible bug.

- **No ink outside a `.blk`'s box — ever.** Render blocks use
  `content-visibility: auto`, whose paint containment **clips any ink outside
  the block's border box** (that's what turned margin-hanging chips into
  sliver stubs, and what amputated the amsart "Abstract" heading pulled up by
  a negative margin). Layout is unaffected, so `getBoundingClientRect` /
  computed-style checks **cannot detect** this clipping — only looking at
  pixels (or reasoning about containment) can. Anything that must render in
  the page margin — or draw ANY ink outside an element's own box (an
  `outline` / `box-shadow` / `outline-offset` on something that fills its
  block: the cursor flash box rendered with zero edges this way) —
  therefore lives in a **page-level layer**: a direct child of `main#page`
  (`.refkey-layer`, `.lineno-layer`, `.flash-layer`), outside every block's
  containment.
- **The layers are measured, not styled, into position.** `layoutRefkeys()`
  reads each anchor's client rect and divides by the zoom scale
  (`pageRect.height / page.offsetHeight` — `main#page` is CSS-`zoom`ed, so
  rendered coords ≠ local coords; computed-style lengths are already local).
  In-block markup (`.eq-refkey-list`, `[data-refkey]`) is a hidden **data
  carrier only** — texts come from it, geometry never does. The layer is
  rebuilt whole; there is no incremental path to get subtly stale.
- **Rebuild cadence is two-tier — keep it that way.** One layer pass costs
  ~80 ms of forced layout on a long paper, and every keystroke triggers two
  rebuild requests (patch apply + its typeset landing). Render-path callers
  go through the trailing 180 ms timer in `scheduleRefkeys()` (coalesces to
  one pass); crop/mode changes pass `0` for a pre-paint (rAF) rebuild so chips
  move in the same frame as the page. Repeated zoom keys are different:
  `previewUserZoom()` compositor-scales the existing page immediately, then
  commits CSS `zoom` and refreshes both overlays once after the key burst. The
  compositor transform and the real-layout commit must preserve one shared
  viewport anchor. That anchor is the first visible line immediately below the
  toolbar: a viewport-centre anchor is geometrically stable but still lets a
  WebKit repaint replace the top line by half the zoom displacement, while a
  page-relative `top center` origin makes displacement grow with scroll depth.
  Committing without a compensating scroll produces a rebound. On macOS,
  WKWebView does not expose the committed CSS-zoom geometry reliably in the
  setter task: restore the anchor in the next pre-paint rAF and verify it once
  more on the following frame, cancelling both when a new zoom burst starts.
  A synchronous-only restore passes Chromium but lets the macOS layout snap
  afterward. The anchor must retain a live content element in every mode:
  dynamic width changes reflow text, while A4's `content-visibility` can replace
  estimated block heights above the viewport during the real layout even
  though line wrapping is unchanged. Applying CSS `zoom` or walking either
  overlay per key re-creates long-paper jank; making crop/mode entirely trailing
  leaves chips visibly misplaced.
- **The margin variables are a derivation chain — override the *used* var,
  not just the base.** `:root { --page-pad-x: var(--page-pad-x-base) }`
  substitutes **at `:root`**; descendants inherit the *resolved* value. An
  element-level `--page-pad-x-base` override alone is dead CSS (the shipped
  dynamic-mode 10 mm pin silently didn't work until the rule also re-declared
  `--page-pad-x: var(--page-pad-x-base)` at element level). Corollary for
  verification: **assert computed end properties** (`paddingLeft`, column
  width), never the custom-property value.
- **Crop must never change line wrapping.** CSS drops `--page-pad-x` to
  `--crop-pad` while JS narrows the page by `cropDxNow()` = 2 × (base − crop);
  the two must read the **same base** (the element's computed
  `--page-pad-x-base`) or the text column reflows on every crop toggle. Any
  new mode/margin override must keep `cropDxNow()` and the CSS crop rule in
  agreement — and the crop override must stay **later in source** than
  mode-level `--page-pad-x` declarations (equal specificity; order decides).
- **What guards it:** `viewer_shell_contains_index_and_page_modes` (renderer
  tests) asserts the layer + scheduling wiring and the CSS invariants above
  stay in the served page; the e2e recipe is the same in every session —
  chips whole and inside the page bounds, column width constant across crop
  toggles, computed-end-property assertions in both page modes.

### CSS `zoom` × MathJax `ex` — the macOS-WebKit-only trap

`main#page` scales with CSS `zoom` (`--page-scale`). MathJax sizes every SVG
in **`ex` units** (`width="50.242ex"`, `vertical-align:-0.566ex`). On macOS
Locus (WKWebView) the spec-compliant `zoom` resolves those `ex` against a
**zoom-inflated font** *and* geometrically scales the box — a double count, so
equations grew by `zoom²` while text grew by `zoom`. Chromium (browser tab)
and WebKitGTK (Linux window) don't double-count, so they were fine — **the bug
is invisible in the Chromium preview you test in.** Reason about the engine,
don't rely on reproducing it here.

- **Fix:** the macOS native shell adds `html.locus-macos` at document start;
  `engines/assets/mathjax.css` uses that marker to give only the outer MathJax
  SVG an explicit inherited `font-size:1em`. WebKit then keeps the SVG's `ex`
  box in local CSS pixels and the page `zoom` supplies the one visual scale.
  Do not pin width/height/baseline to px: that looks correct during zoom but
  freezes already-typeset equations when the live document font size changes.
- **Verification:** in WKWebView, assert the SVG's rendered width/height scales
  exactly ×`--page-scale`, its `ex` attributes remain responsive, and doubling
  `--body-font-size` also doubles an already-typeset SVG. Browser Chromium is
  the no-regression control; it cannot reproduce the macOS double count.
