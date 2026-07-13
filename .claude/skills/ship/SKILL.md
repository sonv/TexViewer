---
name: ship
description: TexViewer/mathpreview's ship-and-release discipline — the per-change checklist (verify e2e, quality gates, lockstep version bump, WS-protocol pairing, CHANGELOG, push to dev) and the release flow (FF main, tag, CI, publish). Use this skill whenever making ANY code change in this repo — bug fix, feature, perf work, or plugin edit — and whenever the user asks to release, publish, cut a version, or reports a bug that "still" happens after a fix (stale-binary diagnosis). Consult it BEFORE committing, bumping versions, or touching WS message shapes.
---

# Shipping a change to mathpreview

Rationale and the full verification cookbook live in
[DEVELOPMENT.md](../../../DEVELOPMENT.md) — read it when something here needs
the *why* or a measurement recipe. This file is the operational path.

## Before assuming a code bug

A bug that "still" happens after a fix is usually a **stale binary**, not code:

1. `~/.cargo/bin/mathpreview-cli --version` — does it predate the fix?
2. Is the *running* daemon current? `ps aux | grep mathpreview-cli` for the
   port, then `curl -s http://127.0.0.1:<port>/debug -H 'Host: 127.0.0.1:<port>'`
   — check `ws_protocol` (matches serve.rs?) and `clients` (is a tab even
   connected?). The daemon-reuse path skips the plugin's auto-rebuild, so an
   old process can outlive many plugin updates.
3. Fix: `cargo install --path crates/cli --force` (from the checkout), then the
   user runs `:MathPreviewRestart`.

Only after the running binary is proven current, debug the code — and
reproduce against the user's **real file** when one is implicated, not just a
scratch doc.

## Per-change checklist

Work happens on `dev`. For every change:

1. **Verify the fix end-to-end** at whatever layer is reachable headlessly:
   - Server behavior: serve a scratch doc (`--port 277xx`, always send
     `Host: 127.0.0.1:<port>`; poll `curl -m1` until up; kill by PID +
     `lsof -ti tcp:<port>`).
   - Protocol behavior: python `websocket-client` against
     `ws://…/ws?v=<WS_PROTOCOL_VERSION>` (matching `host=`/`origin=`), assert
     on the broadcast events themselves.
   - Client JS/CSS: grep the served page for the new wiring (functions, CSS
     rules, handlers) and the absence of removed paths.
   - Write test `.tex` with `printf '%s\n' '…'` — **never `echo`** (it eats
     `\n`/`\a` inside LaTeX). `POST /buffer` takes the **raw buffer text** with
     the path in `x-mathpreview-path` — not JSON.
   - Perf claims: `--release` build, end-to-end timing (POST → WS event),
     dissect payloads before optimizing.
2. **For nontrivial client/plugin logic** (unrunnable here): adversarial
   multi-lens review via the Workflow tool — finders per concern, then a
   verifier per finding instructed to *refute* it. Fix what survives.
   **Do not `git add -A` while a review workflow is running** — verifier
   scratch edits can ride into the commit. Stage files explicitly, or wait.
3. **Quality gates** (all must pass; there is no PR CI — local gates are the
   only net):
   ```
   cargo test --workspace
   cargo clippy --workspace          # zero warnings
   npm run lint
   luajit -bl lua/mathpreview/init.lua
   ```
   rustfmt is **targeted**: some files carry pre-existing drift. Confirm
   `cargo fmt --check` flags none of *your* identifiers; hand-format your own
   lines until it doesn't. Never reformat whole drifted files.
4. **WS protocol**: if any WebSocket message's shape/semantics changed, bump
   `WS_PROTOCOL_VERSION` in **both** `crates/cli/src/serve.rs` and
   `crates/core/src/assets/client/footer.js` (the
   `client_ws_protocol_matches_server` test guards the pair). This is what
   makes stale tabs hard-reload once onto the new client.
5. **Version bump, lockstep** — for every user-visible change, even
   plugin-only:
   - workspace `version` in `Cargo.toml`
   - `PLUGIN_VERSION` in `lua/mathpreview/init.lua`
   - `cargo build` to refresh `Cargo.lock` (release CI uses `--locked`)
6. **CHANGELOG.md**: user-facing entry under the new version, dated with
   `date -u +%Y-%m-%d` (check it — sessions cross midnight).
7. **Commit** (root cause + mechanism + what was verified in the body;
   `Co-Authored-By` trailer) and **push to `dev`**.
8. If the user is actively testing, offer to `cargo install` the new binary
   for them (+ `:MathPreviewRestart`) — pushing alone changes nothing on
   their machine.

## Release checklist

Only on explicit user request. `dev → main` is always a fast-forward.

1. **Pre-flight** (all must hold):
   - `git status --porcelain` clean
   - `git merge-base --is-ancestor origin/main origin/dev`
   - tag `vX.Y.Z` absent locally and on origin
   - Cargo.toml = Cargo.lock = PLUGIN_VERSION = the version being tagged
   - WS protocol constants match (serve.rs ↔ footer.js)
   - CHANGELOG entry exists and is dated
2. `git push origin dev:main`
3. `git tag -a vX.Y.Z <sha> -m "release: vX.Y.Z — <summary>"` then push the
   tag. Transient SSH failures happen: verify with
   `git ls-remote --tags origin vX.Y.Z`, retry if missing.
4. CI (`release.yml`, fires on `v*` tags) builds + tests 4 targets and creates
   a **draft** release. Find the run
   (`gh run list --workflow=release.yml --event=push`), watch it in the
   background (`gh run watch <id> --exit-status`).
5. While CI runs, write curated release notes: theme-grouped highlights
   distilled from the CHANGELOG span since the previous release, ending with
   the standard binaries/checksums line (and a WS-bump note when applicable).
6. On success: confirm the draft has **14 assets** (4 daemon tarballs +
   3 `-locus-` gui tarballs, each with a `.sha256`), then
   `gh release edit vX.Y.Z --notes-file <notes> --draft=false --latest`,
   and give the release a title (`--title "vX.Y.Z — <short theme list>"`).
7. Verify: `isDraft=false`, `Latest` marker in `gh release list`, and report
   the release URL.

## Invariants worth re-reading before touching them

The performance-critical ones (patch `blocks` deltas, the `<prefix>-g<block>-<n>`
id scheme, containment/lazy-typeset rules, `isRawMathNode`) live with their
measurements in [PERFORMANCE.md](../../../PERFORMANCE.md) — read it before
touching `broadcast_render`, `IdGen`, the typeset queue, or per-patch client
passes.

The `keys`/line-number margin overlays have their own hard-won invariants in
[DEVELOPMENT.md § "The margin overlays"](../../../DEVELOPMENT.md) — read that
section before touching `.refkey-layer` / `.lineno-layer`, `layoutRefkeys`,
`scheduleRefkeys`, the `--page-pad-x*` variable chain, crop width math
(`cropDxNow`), or anything that renders in the page margin. Short version:
no ink outside a `.blk` (paint containment clips it), margin content lives in
page-level layers, element overrides of derived CSS vars must re-declare the
derivation, and verification asserts computed end properties — never the
variable.

- Patch `blocks` metadata is **positional and full-length** (`blk-N` ids
  renumber on insert/delete); unchanged positions ship as `0` — keep the delta
  when editing `broadcast_render` / `syncPatchBlockMetadata`.
- `SyncKind::Block` is excluded from cursor point-lookup *on purpose*
  (headings must not flash); cursor *follow* relies on the scroll-only and
  nearest-element-on-jump fallbacks in `serve_cursor`.
- `render_inline_latex` has no `ctx`: state there means thread-locals;
  `data-src` there means the caller wraps the output.
- Preamble extractors regex-scan **comment-stripped** source
  (`macros::strip_line_comments`) — never raw.
