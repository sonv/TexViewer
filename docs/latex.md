# LaTeX support

[Back to README](../README.md) · [Macros](macros.md) ·
[Viewer controls](viewer.md) · [Markdown](markdown.md)

mathpreview turns LaTeX into an interactive HTML preview. It follows a
project's structure and common mathematical conventions without running a
TeX engine for ordinary preview. It does not reproduce every package or the
exact layout of a compiled PDF.

## Projects and macros

Open the root or a child file in a multi-file project. The renderer follows
`\input`, `\include`, and `\subfile`, and the Neovim plugin can update an
included buffer without saving it to disk. Local `.sty` files loaded through
`\usepackage` are scanned for declarations too.

Math macro extraction covers `\newcommand`, `\DeclareMathOperator`,
`\NewDocumentCommand`, `\def`, `\let`, `\DeclarePairedDelimiter`,
`\DeclarePairedDelimiterX` / `XPP`, and the `\newdelim` / `\newdelimX`
wrappers. Unsupported TeX-internal forms are filtered before reaching MathJax.
The [macro guide](macros.md) explains preview overrides, environment
replacements, paired-delimiter approximations, and the narrower set of macros
that expand in regular text.

## Theorems, equations, and references

Theorem environments follow the document's `\newtheorem` declarations,
including custom names, shared or independent counters, titles, and reset
levels. Declarations in scanned local files count too. An undeclared
`theorem` environment is not guessed from its name.

The viewer supports section and equation numbering, multi-row `align` and
`gather`, and alphabetic child numbers in `subequations`. With
`\usepackage[showonlyrefs]{mathtools}` or `\mathtoolsset{showonlyrefs}`,
only referenced equations receive numbers.

References resolve to readable text. For example, `\cref{thm:main}` can
become “Theorem 2.1” and `\eqref{eq:foo}` can become “(3.1)”. Hover previews
show the target, with source labels above equations. References can also be
pinned as margin cards. See [viewer controls](viewer.md).

Theorems can use colored boxes or a plain TeX-like style. Role annotations
`[role=main|supporting|standard|omitted]` support proof folding by role.
All proofs are expanded by default. See [configuration](configuration.md)
for theorem presentation and numbering overrides.

## Text and document structure

- Title blocks support `\title`, `\author`, `\date`, and `\maketitle`,
  along with repeated authors, `\and`, `\address`, `\curraddr`, `\email`,
  and AMS-style abstracts.
- Native `letter` layout positions the sender, recipient, opening, closing,
  and signature while continuing to parse prose and math.
- Lists include `enumerate`, `itemize`, `description`, and paralist variants.
- `tabular`, `tabular*`, `tabularx`, and `longtable` render as HTML tables,
  including alignment, paragraph-width columns, rules, `\multicolumn`,
  captions, references, and text/math formatting. Wide tables scroll locally.
- Blank source lines create paragraph breaks, including within theorems and
  proofs and around display math.
- Common emphasis, font switches, accents, references, and math work in prose,
  titles, theorem names, and `\omitref` payloads.
- Text colors include `\textcolor`, scoped `\color`, `\colorbox`,
  `\fcolorbox`, common xcolor models and mixes, and preamble color definitions.

## Bibliographies and figures

Bibliographies support `numeric`, `alphabetic`, and `authoryear` styles,
with `\addbibresource` or `\bibliography`. Alphabetic and author-year
entries are sorted by author and year. Body-level `\bibliographystyle{plain}`
is honored, and bibliography files resolve relative to the main TeX file.

Manual bibliographies use the same citation links, hover previews, and margin
cards. No `.bib` file or BibTeX installation is needed:

```tex
See \cite{knuth,notes}.

\begin{thebibliography}{99}
  \bibitem{knuth}
  Donald E. Knuth. \emph{The TeXbook}. Addison-Wesley, 1984.

  \bibitem[Notes]{notes}
  Course notes. \url{https://example.org/notes}.
\end{thebibliography}
```

Plain `\bibitem{key}` entries are numbered in bibliography order, not citation
order. Optional labels such as `\bibitem[Notes]{notes}` are shown as written
and do not advance that numeric counter. Every manual entry remains visible,
including uncited entries. The `{99}` argument is a LaTeX width hint, not text.

Entry bodies support normal text formatting, math, `\newblock`, and safe
`\url` and `\href` links. You can put the whole environment in an `\input`
file, or place `\input{entries}` inside it. Explicitly included `.bbl` files
using `thebibliography` work the same way. Unsaved edits and source jumps
retain the included file's location. When keys overlap, an inline entry takes
precedence over the `.bib` entry, and the first inline definition owns the link.

The preview approximates common BibTeX and biblatex style families. It does
not run arbitrary `.bst` files or interpret package-specific bibliography
programs. Natbib's structured optional labels are shown literally rather than
decoded into separate author and year fields.

`\includegraphics` supports project-local raster and SVG images and cached
PNG previews of PDF figures, with common width, height, and scale options.

Trusted projects can opt in to native previews of `tikzpicture`, `tikzcd`,
`circuitikz`, and `forest`, including diagrams inside figures and tables.
This uses the project's real preamble and a local TeX installation. It is off
by default. See [TikZ configuration](configuration.md#math-layout-and-tikz).

## Preview limits

This is not a full TeX interpreter. Complex class state, package internals,
and arbitrary layout may need a [preview override](macros.md).

An unsupported non-literal environment shows its begin/end markers in red
while still rendering its body. Code, floats, and diagrams retain their
specialized handling. Preview-only environment replacements can consume
arguments and supply a closer approximation when needed.

While an edit leaves a math delimiter unbalanced, the
live viewer keeps its last well-formed render. Updates reuse unchanged math
and patch document blocks. The implementation and measurements are in
[PERFORMANCE.md](../PERFORMANCE.md).
