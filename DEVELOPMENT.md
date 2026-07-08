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
