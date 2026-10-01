# Viewer controls

[Back to the README](../README.md)

The browser viewer lets you read the document, inspect references, and jump
back to source while the preview updates. Most controls run in the browser.
Printing and daemon controls also contact the local server.

## Toolbar and page layout

| Control | Action |
| --- | --- |
| `toc` | Opens the navigation pane, with `Index` and `Pages` tabs |
| `A4` / `dynamic` | Selects a fixed A4-ratio sheet or a wider flow layout |
| `crop` | Trims the paper margins without changing text wrapping |
| `keys` | Shows LaTeX label keys in the page margin |
| `lines` | Shows numbers for wrapped prose lines |
| `margin` | Lets reference clicks pin cards beside the document |
| `☾` / `☀` | Switches dark and light themes |
| `print` | Compiles a native TeX PDF or opens the Markdown print dialog |
| `main only` / `+ supporting` / `all` | Filters proofs by their role |
| `hide` | Hides the banner, with a small `toolbar` button to restore it |

The A4 sheet scales with browser width and uses the configured page margin.
Dynamic mode uses a compact 10 mm margin. The keyboard bindings are `p4` for
A4 and `d` for dynamic. The `Pages` list comes from the viewer's page dividers.

Cropping hides line numbers because their gutter is removed. Label chips
normally sit in the margin. While cropped, they overlay the content edge and
stay translucent until hovered. The crop setting persists in the browser.

The theme follows the operating system on first load. A manual choice persists
in `localStorage["mathpreview.theme"]` and applies to the paper, controls,
theorem boxes, cards, labels, and MathJax glyphs.

Proof roles can come from nearby or postponed “Proof of ...” headings, or from
explicit options such as `\begin{proof}[role=main]`.

## Labels, hover previews, and reference cards

`keys` shows exact LaTeX label keys for sections, theorems, figures, display
equations, and loose labels. Multi-row displays have row-level keys. Long keys
extend into the gutter when there is space, with an ellipsis in narrow windows.
Keys are prepared for the whole document before scrolling, without requiring
MathJax to typeset every off-screen equation.

Hovering a generated displayed-equation number shows only its exact
`\label{...}` key, without repeating the equation. Each `align` or `gather`
row shows only its own keys, including aliases, and an unlabeled row has no
popup. The viewer does not infer prefixes: `\label{first}` shows `first`, not
`e:first`. Manual MathJax `\tag` values are not generated
equation numbers and do not receive this hover label.

Hovering a reference or citation shows a quick preview without the proof, with
the label or citation key at the top. References to multi-row equations retain
the full display and highlight the referenced row. This works whether or not
`margin` mode is on. **Hover preview size** in the config panel sets
magnification from 100–300%. By default the preview is at least as large as the
document font. It also follows page zoom above natural size.

With `margin` on, clicking a reference or citation pins the theorem, equation,
or bibliography entry beside the page. Click again, or use `×`, to unpin it.
Use `↔` to expand it across the text and drag the `⋮⋮` grip to reorder cards.
Clicking a visible label chip also toggles its card.

Press `:` for the command line. `:pin` and `:unpin` accept Tab fuzzy-completion,
and `:clear` removes the pinned cards. `:q` closes the viewer if the browser
allows it. Otherwise the viewer shows the appropriate Cmd+W or Ctrl+W hint.

## Line numbers

`lines` numbers each wrapped visual line of body text, including paragraphs
with inline math. Display equations are not included. The viewer prepares
off-screen line positions and recomputes them after layout changes. The setting
persists in `localStorage["mathpreview.lineNumbers"]`.

## Keyboard navigation

Bindings are ignored while typing in an editable control or while a dialog is
open. Configure them by action name in the global or project `[keybindings]`
table. Toolbar actions can also be bound even when they have no default key.

| Keys | Action |
| --- | --- |
| `j` / `k`, `Ctrl-e` / `Ctrl-y`, arrows | Move down/up one rendered prose line |
| `J` / `K` | Move five lines by default, through configurable aliases |
| `Ctrl-d` / `Ctrl-u` | Move down/up half a page |
| `Space`, `Ctrl-f`, Page Down | Move down a full page |
| `Shift+Space`, `b`, `Ctrl-b`, Page Up | Move up a full page |
| `gg` / `G` | Go to the top/bottom, or the source line given by a count |
| `{` / `}` | Move between paragraphs |
| `[[` / `]]` | Move between headings |
| `zt` / `zz` / `zb` | Align the reading anchor to the top, middle, or bottom |
| `/` / `?` | Search forward/backward |
| `n` / `N` | Repeat search in the original/opposite direction |
| `*` / `#` | Search the selected or anchored word forward/backward |
| `Ctrl-o` / `Ctrl-i` | Move backward/forward through the jump list |
| `m<char>` | Mark the current reading position for this session |
| `'<char>` or `` `<char> `` | Return to a mark |
| `t` | Toggle the index/pages pane |
| `B` | Toggle the banner |
| `c` | Toggle cropping |
| `p4` / `d` | Select A4/dynamic layout |
| `:` | Open the command line |
| `q` | Close the viewer where the browser permits it |

Counts compose with motions and aliases. For example, `10j`, `3n`, and
`2Ctrl-o` repeat their motions, while `3J` moves fifteen lines. Each counted
line motion becomes one immediate browser scroll, including across display
equations.

A pending mark command waits for its character until completed or cancelled
with Escape, a click, focus in a control, or browser blur. Spaces in TOML
binding strings separate key tokens. A binding written `m <char>` is typed
as `ma` for mark `a`, without pressing Space.

## Content zoom

`+` and `-` zoom the page while the header and sidebar stay fixed. `z0` resets
zoom, and `=` fits the page width to the viewport. Cmd/Ctrl with `+`, `-`, or
`0` provides the same controls for the paper.

The first visible line below the toolbar stays in place during zoom. A page
wider than the window can be panned with a trackpad, Shift+wheel, or `h`/`l`.
The zoom factor persists in `localStorage["mathpreview.userZoom"]`.

## Search

The viewer's `/` search tolerates small typing errors, including transposed
letters. For example, `coagualtion` finds `coagulation`, and `gaint clustr`
finds `giant cluster`. Matching document words appear above the search box.
Use Tab, arrow keys followed by Enter, or a click to complete a suggestion.
The current/total counter and `n`/`N` still work normally.

Prefix a query with `m:` or wrap it in dollar signs to search only math glyphs.
For example, `m:n` finds the italic `n` in `$n^2$`, while `$\alpha$` searches
for alpha. A single letter or command matches across MathJax style variants
such as italic, bold, and script.

Browser Cmd+F remains exact literal search on macOS. On Linux or Windows,
remove `Ctrl+f` from the `full-page-down` binding if you want to use the
browser's Ctrl+F search.

## Jump back to source

By default, Cmd/Ctrl-click a rendered token to jump to its source location.
Prose uses the word's location. A row in `align`, `gather`, or `multline` uses
that row's first token rather than the environment's opening line. Alternative
gestures, such as double-click or Alt-click, can be selected in
`[viewer.source-jump]`.

The Neovim plugin navigates in the existing editor. For another editor, set the
plugin's `editor` option or pass `--editor` to `mathpreview-cli serve`, for
example `code -g {file}:{line}`. See [Neovim setup](neovim.md) for cursor sync,
visual selections, and window focus.

## Print and daemon controls

For TeX, `print` runs `latexmk -pdf`, or `pdflatex` when appropriate, in the
project directory. This requires a local TeX toolchain. The viewer reads the
PDF location from the build log, including a custom `$out_dir` in `.latexmkrc`,
and opens the result in a new tab. For Markdown, `print` opens the browser's
print dialog without TeX. Neither path runs until you click the button.

`restart` relaunches the daemon with the same command-line arguments, waits
until it is ready, and reloads the page. `stop` exits the daemon and becomes
`start`. That button waits for a daemon to become available again, then reloads.

The status pill reports update timings. A one-block patch may show
`6ms · 1r / typeset 0`. A full-body update reports parse, diff, swap, and
typesetting work, including how much rendered math was reused.
