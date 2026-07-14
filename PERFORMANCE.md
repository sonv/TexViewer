# Large-document performance architecture

How mathpreview keeps a 60-page paper (measured against a real one: 7,582
lines, 3,302 equations, ~23,000 anchored DOM elements, 499 blocks) feeling
like a 2-page note. Built and measured across v0.1.90–v0.1.95; every number
below comes from a real browser driven against that paper.

The through-line: a live preview re-renders on every keystroke, so **any cost
proportional to document size becomes a per-keystroke cost**. Each layer below
removes one such cost. They compose — later layers depend on earlier ones.

| per-keystroke metric | before | after |
|---|---|---|
| patch payload | 1.65 MiB | ~0.7–30 KiB |
| client main-thread block | ~440 ms | **~57 ms** |
| server render + broadcast | ~500 ms worst | ~110 ms flat |
| cold-load typeset | 65 s (all 3,302 eqs) | **~0.2 s** (visible; rest in background) |
| Cmd+P after a few idle minutes | 65 s flush | **instant** |

## Layer 1 — Patch metadata deltas (v0.1.90)

Every patch carries a `blocks` list so the client can re-sync block ids,
hashes, `data-src`, and per-element anchors. It **must stay positional and
full-length** — block ids are positional (`blk-N`), so an insert/delete
renumbers the tail and the client re-labels by index. But shipping full
metadata for all 499 blocks meant ~375 KiB and ~19k `setAttribute` calls per
keystroke *even when nothing changed* (`ops: 0`).

Fix: `broadcast_render` compares each position against `last_blocks` and emits
a one-byte `0` for unchanged positions; `syncPatchBlockMetadata` skips those.
A within-line edit ships 1 real entry. A line-count change (Enter) still ships
the shifted tail — those anchors genuinely changed (`data-src` embeds absolute
`line:col`).

## Layer 2 — Block-scoped element ids (v0.1.91)

Layer 1 exposed the next problem: generated element ids (`srcw-`, `im-`, …)
were one **document-global sequence**. Typing a single word inserts one
anchor, which renumbers *every element after it* — so every later block's
metadata legitimately changed and layer 1's delta couldn't help (measured:
370/494 blocks full, 1.65 MiB, per keystroke).

Fix: `IdGen` emits `<prefix>-g<block>-<n>`, reset at each `push_block`.
Untouched blocks stay byte-identical across renders. Details that matter:

- **The `g` marker** keeps generated ids structurally disjoint from
  label-derived ids: `sanitize_id("thm:2.1")` → `thm-2-1`, which collided with
  the unmarked scheme (proven by test — duplicate DOM ids silently mistarget
  `\ref` links, source-jumps, and highlights).
- **Footnote ids are exempt** (`fn-N`/`fnpop-N` from their own counter): their
  number is *visible* and must stay document-ordered.
- The diff stabilizer (`stable_block_diff_source`) strips generated ids by
  prefix so they don't poison `diff_hash`; its list distinguishes g-marked
  idgen prefixes from bare-numeric counter ids and **must be kept in sync
  with the id scheme** (a missed prefix means block-ordinal shifts rebuild
  every later block of that kind — this bit sidenotes and quotes/callouts).
- The skipped-empty-block path deliberately does *not* reset the counter, so
  ids already recorded in the sync index can't collide with the next block's.

## Layer 3 — Real block boxes + CSS containment (v0.1.92)

With payloads small, profiling a real browser showed `applyPatch` at **4 ms**
and typeset at **25 ms** — yet each keystroke still blocked ~440 ms: two full
style/layout recalcs over the 23k-element page. Two causes:

1. Blocks were `display: contents` — invisible to layout — so *any* DOM change
   re-laid-out the entire page (and, latently, `blk-N` scroll targets had
   zero-size rects).
2. Per-patch passes ran page-wide: the proof re-fold walked every `.proof`
   (each with sibling-walking role resolution), and same-value
   `setAttribute('data-proof-mode')`/`data-refkeys` writes dirtied
   attribute-selector styles for the whole page on every keystroke.

Fix: `main#page .blk { display: block; content-visibility: auto;
contain-intrinsic-size: auto 180px }` — off-screen blocks skip style, layout,
and paint entirely; `auto` keeps the real height once a block has rendered.
Overlay clones (margin cards, hover previews, zoom dialog) and print media
force `content-visibility: visible`. `applyPatch` returns the blocks it
touched and the per-patch passes (`applyMode`, chip decoration) scope to them;
same-value attribute writes are guarded. Geometry reads on skipped content
return degenerate rects, so scroll paths fall back to native `scrollIntoView`
(which forces the target to render). Verified pixel-identical by screenshot.

## Layer 4 — Viewport-lazy typesetting (v0.1.92)

Containment alone made cold load *worse*: eagerly typesetting 3,302 equations
took 65 s without containment and **170 s with it** — MathJax's measurements
inside skipped subtrees are pathologically slow. So don't typeset off-screen
math at all: `queueTypeset` defers math whose block is skipped
(`checkVisibility({contentVisibilityAuto: true})`) and each block typesets the
moment it first becomes relevant (`contentvisibilityautostatechange` —
scrolled near, focused, `scrollIntoView`'d). Browsers without the event fall
back to eager. Cold load: the visible page typesets in ~0.2 s (29 equations);
jumping to 60% typesets its neighborhood (373 equations) in ~2.5 s once.

## Layer 5 — Local window vs. background fill (v0.1.95–0.1.97)

This is the mechanism behind the `typeset-mode` config. All of it lives in
[`patch.js`](crates/core/src/assets/client/patch.js).

### The starting point: everything is "raw" until typeset

Each equation is rendered by the server as a `<span class="math" data-hash>`
containing its LaTeX source in a `.math-source` span. MathJax replaces that with
an `<mjx-container>` SVG when the math is *typeset*. So the one true test for
"is this math still raw?" is **`isRawMathNode`** — a `.math[data-hash]` with **no
`mjx-container` child**. (Do NOT test for a `.math-source` span: those persist
after typesetting, so "has a `.math-source`" does not mean untypeset. An early
background loop used that wrong predicate and spun on block 0 forever.)

Typesetting a raw node is expensive, and — because blocks carry
`content-visibility: auto` (layer 3) — typesetting one inside a **skipped**
(off-screen) block is *pathologically* expensive: MathJax measures inside a
subtree the browser is trying not to lay out (65 s eager → 170 s eager-under-
containment for 3,300 equations). So every code path that typesets a
possibly-skipped block does the same dance: set `blk.style.contentVisibility =
'visible'` just for the typeset, then restore it to `''` (back to `auto`)
afterwards. This "lift-for-typeset" is the recurring trick below.

### The two modes

`typeset-mode` (config; default `local`) chooses how much gets typeset:

- **`local`** — only the region around the viewport, plus a buffer. The rest
  stays raw until you scroll to it. A 3,300-equation paper keeps *tens* of
  typeset equations in the DOM, not thousands — memory and CPU track what you
  actually read.
- **`background`** — the window first, then the rest is filled in while the tab
  is idle, so scrolling deep and printing never wait (at the cost of typesetting
  and holding the whole document in memory).

Either way, **Cmd+P typesets the whole document on demand** first (layer 6), so
a printout is never missing math regardless of mode.

### The paths that typeset math, and how they interlock

There are up to four producers of "typeset this block" work. They all funnel
through the engine and are serialized by one flag, **`typesetBusy`** (only one
MathJax batch runs at a time; each path sets it, awaits, clears it).

1. **The visible flush (every render).** `queueTypeset(nodes)` is called at load
   and after every patch with all raw math. For each node it checks
   `inSkippedBlock` (`checkVisibility({contentVisibilityAuto: true})`):
   - **not skipped** (on/near screen) → added to `pendingTypeset` and typeset by
     `flushTypeset` after a short debounce. This is the fast path — no lift
     needed, the block is already being laid out.
   - **skipped** → *deferred*: `deferTypesetUntilVisible` registers a one-shot
     `contentvisibilityautostatechange` listener on the block, so it typesets
     the instant the browser un-skips it. Nothing is typeset eagerly off-screen.
     The overlay pre-layout may also briefly un-skip a block to cache refkey and
     line-number geometry; its marker makes this listener ignore that synthetic
     event, preserving the selected `local` / `background` policy.

2. **The viewport window (always on).** At the end of `flushTypeset`,
   `observeTypesetWindow` (re-)observes every block that still holds raw math
   with an `IntersectionObserver` whose `rootMargin` is `TYPESET_WINDOW`
   (`'150% 0px'` — ~1.5 viewports of lookahead above and below). When a block
   enters that expanded band the observer unobserves it and adds it to
   `windowQueue`; `drainWindowTypeset` pops one block at a time, lifts
   containment, typesets its raw math, restores. This is what makes scrolling
   smooth — blocks are typeset a screen or two *before* you reach them. Blocks
   are re-observed after each render because a patch replaces them with new DOM
   nodes.

   (Paths 1-defer and 2 overlap on purpose — the observer fires ahead of the
   viewport with a bigger margin; the state-change listener is the backstop that
   also catches `focus`/`scrollIntoView` jumps. Whichever runs first typesets
   the block; the other finds no raw math (`isRawMathNode` is false) and no-ops.
   `typesetBusy` keeps them from running MathJax concurrently.)

3. **Background fill (background mode only).** `flushTypeset` also calls
   `scheduleBgFill`, which is a no-op unless `typesetMode() === 'background'`.
   When enabled, `bgFillStep` walks the blocks in document order, finds the
   first one still holding raw math, and typesets it (≤ 40 nodes per step so a
   giant proof can't jank the main thread), then reschedules itself ~120 ms
   later — marching to the end of the document. It **yields**: each step bails
   and retries later if a print flush is running, a typeset batch is in flight
   (`typesetBusy`), or the window queue is non-empty (`windowQueue.size` — your
   viewport always wins over the background march). It **self-gates**: the first
   line of `bgFillStep` returns if the mode is no longer `background`, so
   flipping the toggle to `local` stops it after the current block.

4. **The print flush (Cmd+P).** `typesetAllForPrint` (layer 6) batch-typesets
   *everything* before opening the print dialog. It sets `typesetBusy`, so paths
   2 and 3 yield to it while it runs.

### Switching modes live

`typeset-mode` is pure client behavior, so it applies with no reload:

- Server: `config.rs` `TypesetMode` enum → `window.__mpConfig.typesetMode` in the
  page head, and broadcast in every render's `viewer_config` JSON.
- Client: `applyViewerConfig` calls `setTypesetMode(mode)`, which updates
  `window.__mpConfig.typesetMode` and — if the new mode is `background` —
  kicks `scheduleBgFill`. `typesetMode()` reads that value everywhere, so paths
  3's gate flips immediately. The config dialog's "Math rendering" dropdown
  and the `.mathpreview.toml` file both route through the same broadcast.

## Print fidelity (v0.1.96)

A real **`@page { size: A4; margin: 17mm }`** plus print CSS maps the content
1:1 onto the printable area (no outer frame, no zoom, `@page` margins supply
the margins) and adds `break-inside: avoid` to atomic blocks. The 176mm
printable column equals the on-screen A4 column (794px − 2×64px ≈ 176mm), so
line wrapping — and therefore vertical flow and breaks — agree between screen
and print. Cmd+P is deterministic instead of using the browser default.

## Layer 6 — Print correctness (v0.1.93–94)

Lazy + background typesetting mean the browser's own Cmd+P could print raw
LaTeX for never-rendered math. Cmd/Ctrl+P is intercepted: a batched
full-document flush runs first (containment lifted via `body.print-preparing`;
`@media print` forces every block visible so print never relies on intrinsic
size estimates), fronted by a modal progress dialog ("Preparing to print…",
live equation count, Cancel/Esc aborts between batches and skips the print),
then `window.print()`. Once the background pass has finished, the flush is a
no-op and Cmd+P prints instantly. `File → Print` can't be delayed by the
browser, so it starts the same flush best-effort and the dialog says to print
again for a complete printout. The toolbar print button is unaffected — it
compiles real `latexmk` output.

## Invariants for future work

- The `blocks` patch list stays positional and full-length; unchanged
  positions ship as `0`.
- Generated ids are `<prefix>-g<block>-<n>`; label-derived ids are the
  sanitized label; footnotes are bare-numeric by design. Any new generated-id
  prefix must be added to `starts_generated_id_attr` (g-marked list).
- Any code that typesets math in bulk must lift containment for the subtree
  it's working on, or pay the ~3× skipped-subtree penalty.
- Any code reading geometry must tolerate degenerate rects from skipped
  content (fall back to `scrollIntoView`).
- `isRawMathNode`, not `.math-source` presence, decides whether math is
  typeset.
- The client's sub-diff (`blocksub`) container selector must cover every
  `write_chunked_children` caller (proof, theorem, callout, quote) — a miss
  silently drops edits.
- Per-patch client passes take the touched-blocks list from `applyPatch`;
  don't add new page-wide passes to the patch path.
