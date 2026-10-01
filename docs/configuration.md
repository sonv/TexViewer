# Viewer configuration

[Back to README](../README.md) · [Viewer controls](viewer.md) · [Markdown](markdown.md) · [Macros](macros.md)

## Configuration files

User preferences live in TOML — same discovery cascade as the macro
overrides, applied per field with last-wins semantics:

1. Built-in defaults
2. `~/.config/mathpreview/config.toml` (or `$XDG_CONFIG_HOME/...`)
3. `.mathpreview.toml` walking up from the input file
4. `--config <file>` on `serve` or `render` (repeatable)

```toml
# ~/.config/mathpreview/config.toml — applies to every paper
[viewer]
font-size = 18                  # body text size in CSS pixels
hover-preview-scale = 100       # 100..300% of body size for ref/cite previews
theorem-numbering = "auto"      # | "continuous" | "section" — see below
fancy-theorems = true           # false = plain, PDF-like theorem formatting
typeset-mode = "local"          # | "background" — see below
# render-tikz = true             # trusted projects only; invokes local TeX
# page-margin = 25              # A4 horizontal margin in mm; omit to follow the
                                # document's \usepackage[margin=…]{geometry}

[viewer.source-jump]
# Which click gesture sends `POST /reveal-source` to spawn `--editor`
# at the source line. "cmd-click" also matches Ctrl-click on Linux.
trigger = "cmd-click"           # | "ctrl-click" | "alt-click" | "double-click"

# The complete built-in keyboard map. Keep this in the global file to use the
# same keys for every paper; a project's [keybindings] table can override just
# the actions it mentions. One string or an array is accepted; [] disables.
# Counts compose with motions, and <char> waits for the following printable key.
# Spaces delimit successive key tokens: `m <char>` is typed `ma`, without Space.
[keybindings]
sequence-timeout-ms = 750 # Chord wait; counts/unambiguous final <char> do not expire. 100..5000.
scroll-left = ["h", "ArrowLeft"]
scroll-down = ["j", "Ctrl+e", "ArrowDown"]
scroll-up = ["k", "Ctrl+y", "ArrowUp"]
scroll-right = ["l", "ArrowRight"]
five-lines-down = [] # compatibility action; J is the alias below
five-lines-up = []
half-page-down = "Ctrl+d"
half-page-up = "Ctrl+u"
full-page-down = ["Space", "Ctrl+f", "PageDown"]
full-page-up = ["b", "Ctrl+b", "PageUp"]
jump-back = "Ctrl+o"
jump-forward = "Ctrl+i"
previous-place = [] # deprecated compatibility name for jump-back
go-top = ["g g", "Home"]
go-bottom = ["G", "End"]
horizontal-start = "0"
horizontal-end = "$"
previous-paragraph = "{"
next-paragraph = "}"
previous-heading = "[ ["
next-heading = "] ]"
align-anchor-top = "z t"
align-anchor-center = "z z"
align-anchor-bottom = "z b"
open-search = "/"
open-search-backward = "?"
open-command = ":"
search-next = "n"
search-previous = "N"
search-word-forward = "*"
search-word-backward = "#"
set-mark = "m <char>"
jump-mark-line = "' <char>"
jump-mark-exact = "` <char>"
toggle-toc = "t"
toggle-topbar = "B"
toggle-crop = "c"
close-viewer = "q"
page-a4 = "p 4"
page-dynamic = "d"
zoom-in = ["+", "Mod+=", "Mod++"]
zoom-out = ["-", "_", "Mod+-", "Mod+_"]
zoom-reset = ["z 0", "Mod+0"]
zoom-fit-width = "="
browser-print = "Mod+p"
toggle-margin = ["Ctrl+m", "Meta+m"]

# Fixed viewer buttons without built-in shortcuts. Assign any key to bind one.
toggle-keys = []
toggle-lines = []
open-macros = []
open-config = []
toggle-log = []
toggle-theme = []
proof-main = []
proof-supporting = []
proof-all = []
print-pdf = []
restart-server = []
stop-server = []

# Aliases resolve through the configured action bindings and multiply counts.
# `3J` therefore resolves to `15j`. Use [] to disable an inherited alias.
[keybindings.aliases]
J = "5j"
K = "5k"
"Shift+Space" = "b" # matching page-up distance; follows the configured b motion
```

## Keybindings

`Mod` means Command on macOS and Control elsewhere. `Ctrl`, `Meta`/`Cmd`,
`Alt`/`Option`, and `Shift` can also be named explicitly. Shifted printable
keys may be written as their glyph (`G`, `+`, `:`) or as a combination
(`Shift+g`, `Shift+=`, `Shift+;`); separate sequence steps with spaces, as in
`"g g"`. Leading digits form a count; a bare `0` remains a normal binding.
The built-in digit commands were therefore moved to `p 4` and `z 0`. A
`[keybindings.aliases]` entry maps its left-hand key sequence to another
configured sequence, optionally with a count (`J = "5j"`); aliases can chain,
and `[]` removes an inherited alias. Direct action bindings win if an alias
uses the same keys. Toolbar tooltips show the effective bindings after the
global, project, and `--config` layers have merged. If two layers mention
different actions or alias sources, both survive; a later layer replaces only
the entry it mentions. The `Shift+Space = "b"` alias makes shifted Space use
TexViewer's matching full-page-up distance—even with an older explicit
`full-page-up = "b"` map; set `"Shift+Space" = []` to retain the browser's
native behavior. On Linux/Windows, the Neovim `Ctrl-f` default takes over
the browser's Find shortcut; use `/` for viewer search or remove `Ctrl+f` from
`full-page-down` if you prefer the browser behavior.
`sequence-timeout-ms` controls pending multi-step mappings and ambiguous
complete-prefix fallbacks (for example custom `g` alongside `gg`); an
unfinished numeric count or unambiguous final `<char>` capture does not time out.

TexViewer only cancels keyboard events that match an effective binding. To
leave `j`/`k` (and uppercase `J`/`K`) entirely to the browser or an extension,
remove them from the action lists and disable their aliases:

```toml
[keybindings]
scroll-down = ["Ctrl+e", "ArrowDown"]
scroll-up = ["Ctrl+y", "ArrowUp"]

[keybindings.aliases]
J = []
K = []
```

Setting an action itself to `[]` disables all its shortcuts. To reuse a key for
another viewer action, disable or replace its old action first, then assign the
key to the new action.

## Config panel

The toolbar **config** dialog has dedicated **Viewer config**, **Markdown
config**, and **MathJax config** tabs. The Markdown tab's **Recognize `:::`
blocks** checkbox controls compact MathPreview, Bookdown, and Quarto colon
fences; configured alternate start/end markers remain active when it is off.
The checkbox shows the effective inherited value but writes an override only
after the user changes it, so merely inspecting another save scope cannot
shadow its lower-level setting. Custom block formats and boundary templates
remain available through TOML.

The **Viewer config** tab provides structured controls for the other common
settings, followed by a full TOML editor for advanced settings, Markdown
blocks, and keybindings. Pick **Project (local)** or **Global** and it loads
that file exactly when present; for a missing file it starts with a minimal
override. Omitted settings keep flowing from lower config layers and built-in
defaults—the dialog never materializes missing keybindings into a higher scope.
The editor appends the complete Neovim-style action map and alias table as a
commented reference. Copy entries into an existing table, or uncomment only the
table and bindings you want; an untouched reference is not written to disk.
If another editor changes the selected file after it loads, Save stops and asks
you to reload instead of overwriting that newer version.
Save merges the structured controls into the editor text, preserves the other
local content and comments, and validates the complete result before replacing
the selected file. The **MathJax config** tab keeps the generated-config
inspector and raw JavaScript override with the same save targets.

## Hover preview size

**Hover preview size** writes `hover-preview-scale` as a percentage from 100
to 300. At 100%, a floating `\ref`/`\eqref`/`\cite` preview matches the body
font and fit-to-width never shrinks it below that natural size. Larger values
magnify both prose and MathJax content proportionally; zooming the page above
100% enlarges the preview further. The control shows the effective inherited
value, but an untouched value is not copied into the selected config scope.

## Math layout and TikZ

MathJax follows prose and display conventions separately. Inline formulas may
break at TeX-valid operators so they can continue naturally onto the next text
line. Display formulas are not broken into artificial rows; when one is wider
than the page column, the equation itself can be scrolled horizontally.

`render-tikz = true` enables native previews for `tikzpicture`, `tikzcd`,
`circuitikz`, and `forest` in trusted projects. It is deliberately off by
default because it runs the project's preamble through a local TeX engine. The
server chooses XeLaTeX or LuaLaTeX when the preamble requests one (or uses
`fontspec`), otherwise pdfLaTeX, then converts the first PDF page to a
path-only SVG with `dvisvgm`. Compilation is lazy, serialized, cached,
time-limited, isolated in a temporary directory, and always uses
`-no-shell-escape`. Compiler failures appear as a small diagram placeholder
and in the viewer log. One-shot HTML rendering does not invoke TeX; it leaves
an explicit placeholder because there is no live server to serve the SVG.

## Typesetting mode

`typeset-mode` controls how much of the document is typeset (has its math
rendered) at once — it is also a structured **Viewer config** option.
Default `local` typesets only the region around the viewport plus a small
buffer, leaving the rest until you scroll to it; this keeps memory and CPU
low on a long paper. `background` typesets the visible region first, then
quietly fills in the rest while the tab is idle, so scrolling to deep sections
and printing never wait (at the cost of typesetting — and holding in memory —
the whole document). Either way, **Cmd/Ctrl+P** typesets the whole document on
demand before printing.

## Page margins

`page-margin` sets the A4 page's horizontal margin, in millimetres — the
**Viewer config** editor includes a commented example (leave it commented to
keep following the document), and the structured option can set it directly.
Omit it
and the viewer follows the document's own
`\usepackage[margin=…]{geometry}` (parsing `margin` / `hmargin` /
`left`+`right` / `textwidth`); with no geometry either, it uses the built-in
default (~17 mm). The setting moves the on-screen margin **and** the Cmd+P
print margin together from the same value, so the screen column keeps
matching the printed column — line wrapping and pagination stay in sync.
On narrow viewports dynamic mode still tightens to a compact reading
padding regardless.

## Theorem recognition, style, and numbering

MathPreview treats an
environment as a theorem only when it finds its `\newtheorem` declaration in
the document or a scanned local `.sty` / `.tex` file. An undeclared
`\begin{theorem}` is therefore handled like any other unsupported environment:
its red begin/end diagnostics remain visible while its contents render
normally. This avoids inventing structure that the TeX source itself has not
defined.

`fancy-theorems = true` (the compatibility default) adds MathPreview's colored
box treatment to those declared environments. Turn it off—also available as
**Fancy theorem boxes** in the config dialog—for a plain TeX-like heading and
body while retaining theorem numbers, labels, references, and semantic role
metadata.

By default (`theorem-numbering = "auto"`), detected theorem environments follow
their declarations: continuously (Theorem 1, 2, 3…) for a bare
`\newtheorem{theorem}{Theorem}`, or per-section (1.1, 1.2, 2.1…) when it carries
`[section]`. Set `"continuous"` or `"section"` to override the reset scheme for
recognized declarations—for example, when mutually exclusive conditional
branches declare the same environment and MathPreview can see both but cannot
evaluate which branch TeX selects. Put either setting in the project's
`.mathpreview.toml`; saving through the config dialog applies it on the next
live render.

## Raw MathJax config

To customize MathJax output, drop a snippet of
JavaScript in `mathjax-config`; it runs right after the generated
`window.MathJax = {…}` and before the library loads, so you can override any
MathJax option. **Mutate** `window.MathJax` — don't reassign it (the client
relies on the generated config). A TOML multi-line literal string keeps the JS
readable:

```toml
[viewer]
mathjax-config = '''
window.MathJax.svg.displayAlign = 'left';         // e.g. left-align displays
window.MathJax.tex.macros.RR = '\\mathbb{R}';     // add a macro
window.MathJax.tex.packages['[+]'].push('color'); // load an extra package
'''
```

`mathjax-config` lives in the MathJax `<head>`, so changing it reloads the
preview tab automatically. The MathJax tab's read-only view shows everything
in effect — macros, packages, output options — so you can see what to mutate.

## Project overrides

Drop a `.mathpreview.toml` in the project root to override per-paper:

```toml
# .mathpreview.toml — committed alongside the source
[viewer]
font-size = 20

[viewer.source-jump]
trigger = "double-click"
```

Unknown keys are an error so typos surface immediately instead of
silently doing nothing.
