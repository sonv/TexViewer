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

## Layer 5 — Windowed typesetting (v0.1.95 → refined v0.1.96)

v0.1.95 typeset the whole document in the background after the visible flush.
v0.1.96 replaced that with a **viewport window**: only the region around the
viewport is typeset — the visible blocks plus a buffer above and below — and
the rest stays untypeset until scrolled to. Memory and CPU then track what you
actually read (a 3,300-equation paper keeps tens of typeset equations, not all
3,300), and the whole document is typeset only on demand by Cmd+P (layer 6).

Mechanism: an `IntersectionObserver` with a generous `rootMargin`
(`TYPESET_WINDOW`, ~1.5 viewports) reports each top-level block as it nears the
viewport; a drain worker typesets that block, **lifting its containment just
for the typeset** (layer 4's lesson — MathJax is ~3× slower inside a skipped
subtree) and restoring it after. The drain yields to the print flush and to any
in-progress typeset batch (`typesetBusy`). Blocks are re-observed after each
render, since a patch replaces them with new nodes.

Background filling the whole document is still available as an opt-in: the
`typeset-mode = "background"` config runs an idle loop (block-at-a-time,
containment lifted per block, yielding to typing and prints) after the
window settles. Default is `local` (window only).

Note the raw-math predicate: `.math-source` spans persist after typesetting,
so "has a `.math-source`" does NOT mean untypeset — `isRawMathNode` (no
`mjx-container` child) is the only correct check.

## Page guides that match print (v0.1.96)

The A4 page-break guides were a cosmetic overlay: a line every
`794 × 297/210 ≈ 1123px`, drawn over the text, unrelated to real pagination —
so lines crossed text and never matched Cmd+P. Now:

- A real **`@page { size: A4; margin: 17mm }`** plus print CSS maps the content
  1:1 onto the printable area (no outer frame, no zoom, `@page` margins supply
  the margins) and adds `break-inside: avoid` to atomic blocks. The 176mm
  printable column equals the on-screen A4 column (794px − 2×64px ≈ 176mm), so
  line wrapping — and therefore vertical flow and breaks — agree between screen
  and print. Print is now deterministic instead of using the browser default.
- The guide is computed by **simulating that pagination** (`pageBreakYs`):
  walk the top-level blocks at the printable height (263mm) and place a break
  in the GAP before any block that would overflow — the same rule as
  `break-inside: avoid`. The line lands in whitespace (never on a text line)
  at positions that match where print breaks. Block geometry comes from
  `offsetTop`/`offsetHeight`; `contain-intrinsic-size: auto` makes these exact
  for rendered regions, so guides are accurate where you read and are
  recomputed (signature-gated) as you scroll fresh regions into view.

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
