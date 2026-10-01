# Macros and text formatting

[Back to README](../README.md) · [Configuration](configuration.md) · [Markdown](markdown.md)

## Override macros for the viewer

Some macro definitions don't translate cleanly to MathJax — typically
complex `\xparse` / `\NewDocumentCommand` signatures or anything whose body
still reaches for unsupported `@`-internal TeX primitives. Drop a plain
`\newcommand` replacement into a macros file and the viewer will use it
instead of the source's version.

Cascade (lowest → highest priority — later definitions override earlier
ones by name):

1. Bundled built-ins (`crates/core/src/assets/builtin-macros.tex` — the
   amber-printed unmapped-package warnings hint at which macros need
   coverage; the bundled file is what's already provided).
2. The paper's own preamble macros.
3. `~/.config/mathpreview/macros.tex` (or
   `$XDG_CONFIG_HOME/mathpreview/macros.tex`) — your personal overrides
   applied to every paper.
4. `.mathpreview-macros.tex` walking up from the input file — repo-
   specific overrides that ship alongside the source.
5. `--macros <file>` on the `serve` or `render` subcommand (repeatable).

A typical file:

```tex
% .mathpreview-macros.tex — markdown-friendly approximations
\newcommand{\st}{\mid}
\newcommand{\set}[1]{\{#1\}}
\newcommand{\given}{\mid}
```

The standard `letter` environment is handled natively. The viewer lays out
`\address` and `\date` as a left-aligned sender block anchored at the right,
keeps the recipient, `\opening`, and message on the main left edge, and places
`\closing` plus `\signature` on the right half of the letter, while continuing
to parse prose and math normally. An older preview-only
`\renewenvironment{letter}` workaround is still authoritative when present;
remove that override to use the native layout.

The same override files can provide preview-only replacements for other
environments defined by a document class or system package. Without a
replacement, the viewer marks an unknown non-literal environment's begin/end
boundaries in red and still parses its body as ordinary TeX. A replacement
removes that diagnostic and supplies a closer approximation of the
environment's intended layout. For example:

```tex
% .mathpreview-macros.tex
\renewenvironment{recommendation}[1]
  {\begin{quote}\textbf{To: #1}\\}
  {\end{quote}}
```

`\newenvironment` and `\renewenvironment` support up to nine arguments,
including an optional first-argument default. The replacement's begin/end code
and the original body are parsed normally, so nested environments, references,
and math still render. These are deliberately preview approximations: class
state such as `exam.cls` counters, points, choice markers, and answer modes is
not recreated automatically. Because the viewer cannot infer an unknown
environment's argument signature, any optional or braced arguments after its
begin marker are also rendered as ordinary content; define a replacement when
those arguments should instead be consumed or reformatted.

For an exam, prose and math are visible automatically between the red
unsupported-environment markers. Empty replacements can suppress those markers
for wrapper environments whose bodies need no additional layout:

```tex
\renewenvironment{questions}{}{}
\renewenvironment{parts}{}{}
\renewenvironment{choices}{}{}
```

Commands inside those environments keep the viewer's generic meaning:
`\question[5]` will not acquire exam numbering or points, choice markers are
not synthesized, and `\part` is still interpreted as a document-level heading.
Environment replacements apply before the generic unsupported-environment
fallback and native environment handling. An explicit replacement can
deliberately override a built-in semantic environment, otherwise opaque float,
TikZ diagram, or specialized display-math environment when you want
preview-only semantics instead.

You can also add overrides without leaving the viewer: click the
`macros` button in the toolbar. The chosen scope's existing
command macros and environment replacements **load into the editor** so you can
see and edit what's already there; add or change definitions, pick **Project**
or **Global**, and Save — the daemon validates them and writes the file back
(so re-saving never duplicates), then the page re-renders.
(A *Type* toggle switches to **Text → HTML** for writing a
`[text-macros]` template instead — see [Macros in regular
text](#macros-in-regular-text).) Edits to the file made directly in
your editor live-reload the same way — the file watcher tracks all
override paths. If the file changes in nvim while the dialog is still open, a
dialog Save stops with a conflict instead of overwriting the newer disk copy.

Paired-delimiter declarations are translated to always-scaled, fixed-arity
MathJax macros. Plain calls such as `\set{x \given P(x)}` work for
`\DeclarePairedDelimiter`, `\DeclarePairedDelimiterX`,
`\DeclarePairedDelimiterXPP`, `\newdelim`, and `\newdelimX`; X/XPP argument
counts, `\delimsize`, empty fences, and XPP pre/post code are preserved.
This remains a preview approximation: starred calls such as `\set*{...}` and
explicit-size calls such as `\set[\Big]{...}` do not yet retain mathtools'
special call syntax.

## Macros in regular text

Math macros are expanded by MathJax inside `$…$`. In **regular text**, the
renderer also handles macros, in three ways:

1. **Your `\newcommand`s expand in text.** A macro defined in the preamble,
   in a **local `\usepackage`'d / `\input`'d `.sty` or `.tex`** (these are
   scanned — see [Override macros](#override-macros-for-the-viewer)), or in
   any override file is substituted with its body, arguments and all, then
   re-rendered — so `\newcommand{\hello}{world}` makes `\hello` render as
   *world*, and `\newcommand{\SV}[1]{\textcolor{red}{#1}}` makes `\SV{note}`
   render as a red *note*. (Previously, unknown text macros were dropped.)
   Only `\newcommand`-style definitions are picked up; `\def`,
   `\DeclareRobustCommand`, `\NewDocumentCommand`, etc. are not — use the
   `[text-macros]` table for those.
2. **Built-in text colors.** `\textcolor{red}{x}` produces a colored span,
   while `{\color{ForestGreen} x}` applies a TeX-scoped color switch.
   `HTML`, `RGB`, normalized `rgb`, `gray`, and `cmyk` models are converted to
   safe RGB CSS values; xcolor mixes such as `red!50!blue` work too. Preamble
   `\definecolor`, `\providecolor`, and `\colorlet` declarations are available
   in regular text. `\colorbox` and `\fcolorbox` preserve nested emphasis and
   inline math; math-specific color commands continue through MathJax.
3. **The `[text-macros]` config table — for macros expansion can't reach.**
   For a command defined with `\def` / `\NewDocumentCommand` /
   `\DeclarePairedDelimiter` (not extracted), one from a system package that
   isn't a local file on disk, or just for a preview-only look, map it to an
   **HTML** template (not TeX — TeX-valued macros go in `macros.tex`). Keys are
   command names (with or without a leading `\`); `#1`..`#9` are filled by the
   rendered arguments. Lives in the same `.mathpreview.toml` cascade as
   `[viewer]`. Each value is either a **string** template or, MathJax-style, an
   **array** `[template, n_args, default]` to set the argument count and an
   optional first-argument default explicitly:

   ```toml
   # .mathpreview.toml
   [text-macros]            # `[text_macros]` is accepted too
   hello = "world"                                              # 0 args
   SV    = '<span class="margin-note" style="color:red">#1</span>'  # 1 arg (inferred)
   nb    = '<mark>#1</mark>'
   # [template, n_args, default-of-#1] — like MathJax's tex.macros:
   hl    = ['<mark style="background:#1">#2</mark>', 2, 'yellow']
   #   \hl{x}        -> yellow background   (uses the default)
   #   \hl[pink]{x}  -> pink background     (overrides the optional 1st arg)
   ```

   With a string value the argument count is **inferred** from the highest
   `#n`; the array form sets `n_args` (and the `default`) explicitly, which is
   how you get an optional first argument. A `[text-macros]` entry overrides a
   `\newcommand` of the same name. The template HTML is emitted as-is (it's
   your own local config), and the arguments are rendered through the normal
   pipeline (so math/emphasis inside
   them work and are escaped).

**What's handled in regular text:**

| LaTeX | Result |
|---|---|
| `\emph{x}`, `\textit{x}`, `{\em x}`, `{\it x}` | italic |
| `\textbf{x}`, `{\bf x}` | bold |
| `\texttt{x}`, `{\tt x}` | monospace |
| `\textsc{x}`, `{\sc x}` | small caps |
| `\textcolor{name}{x}`, `\textcolor[model]{spec}{x}` | colored span |
| `{\color{name} x}`, `{\color[model]{spec} x}` | scoped colored text |
| `\colorbox{name}{x}`, `\fcolorbox{frame}{background}{x}` | colored inline box |
| `\ref` / `\cref` / `\Cref` / `\autoref` / `\eqref` / `\pageref` | resolved cross-reference link |
| `$ … $` | inline math (MathJax) |
| `\'e \`a \"o \^o \~n \=a \.z` | accented letters |
| `~` | non-breaking space; `\\` | line break; `\, \; \: \!` | (thin spaces, dropped) |
| your `\newcommand` (preamble or local `.sty`) | expanded with its arguments |
| a name in `[text-macros]` | your HTML template |
| any other `\foo{bar}` | `bar` shown, `\foo` dropped |
| any other `\foo` (no arg) | dropped |

**Not handled:** macros defined with `\def` / `\NewDocumentCommand` /
`\DeclarePairedDelimiter` (use `[text-macros]`) and arbitrary layout. It's a
fast-preview approximation, not a TeX engine — a macro whose body is pure math
used in *text* renders crudely; keep those in math mode or give them a
`[text-macros]` template.

### Map a macro to HTML

To give any command a preview rendering — including one defined with `\def`,
shipped by a package, or that you simply want to look different in the preview:

1. Open (or create) `.mathpreview.toml` in your project root (or
   `~/.config/mathpreview/config.toml` to apply everywhere).
2. Add a `[text-macros]` table. Each key is the command name **without** the
   leading backslash; the value is an HTML template. Use `#1`, `#2`, … for the
   command's arguments (they're rendered and HTML-escaped before substitution):

   ```toml
   [text-macros]
   # \hello              -> world
   hello = "world"
   # \SV{some text}      -> red inline note (a margin macro shown inline)
   SV = '<span style="color:red">#1</span>'
   # \todo{fix this}     -> highlighted
   todo = '<mark>#1</mark>'
   # \keyword{X}{Y}      -> two args
   keyword = '<b>#1</b> (<i>#2</i>)'
   ```

3. Save. The preview reloads and applies it immediately (no restart). An entry
   here overrides a `\newcommand` of the same name, so it's also the way to make
   the preview *differ* from the PDF on purpose.

#### From the toolbar (no TOML knowledge needed)

You don't have to hand-edit the file. Click the **macros** button in the
toolbar and flip the *Type* toggle to **Text → HTML**. The chosen scope's
config TOML loads into the editor on the right, and you have **two ways** to
add a mapping:

- **The quick-add form** (top) — type a **command name** and an **HTML
  template**, then click **Add ↓**. It builds a correct `[text-macros]` line
  (quoting the template for you) and inserts it into the editor under a
  `[text-macros]` table, creating the table if needed. Good when you don't
  know the TOML syntax.
- **The editor** (below) — type or tweak `[text-macros]` lines directly. The
  Add form just writes into this same box.

Either way, click **Save**: the daemon validates the whole file parses as TOML,
writes it back, and re-renders immediately (no restart). Pick the file with the
**Project / Global / Custom** tabs on the left. (The default *TeX macro* toggle
edits the `\newcommand` override file instead — see [Override
macros](#override-macros-for-the-viewer).)

#### By hand

Editing `.mathpreview.toml` in your own editor works identically (the daemon
live-reloads it). Use single-quoted TOML strings so backslashes/quotes in the
HTML are literal. The template HTML is emitted as-is (it's your own local file,
same trust level as a vimrc); only the `#n` arguments are escaped. If a command
takes an optional `[…]` argument, set it via the array form's `default` (see
above) rather than relying on bracket parsing.

> **Don't need raw HTML?** If your mapping is expressible as LaTeX — e.g.
> `\SV{x}` → red text — you don't need the TOML table at all. In the **macros**
> button keep the *TeX macro* toggle (or edit `.mathpreview-macros.tex`) and add
> `\newcommand{\SV}[1]{\textcolor{red}{#1}}`. Those `\newcommand` overrides now
> apply to body text too, so it renders inline. Use **Text → HTML** when you
> want literal HTML or the command isn't a `\newcommand`.

## Inspect what MathJax sees

Mirrors `latex-preview.nvim`'s `:LatexPreview debug`:

```sh
./target/release/mathpreview-cli debug examples/paper.tex
```

Prints the resolved root, included files, MathJax extension list, the full
macro table (name, arity, body, source file), and any warnings about
macros that were filtered out.
