# Markdown

[Back to README](../README.md) · [Configuration](configuration.md) · [Macros](macros.md)

The input format is selected automatically from the filename: `.md` and
`.markdown` use the Markdown frontend, while TeX extensions keep the existing
LaTeX project renderer. Neovim reports both Markdown extensions as the
`markdown` filetype, which is enabled in the plugin defaults. A Markdown
document supports CommonMark prose and headings plus useful GFM-style
additions: emphasis, strong text, strikethrough, inline and fenced code,
ordered and unordered lists, task lists, block quotes, links, tables, thematic
breaks, and document-local images.

Math uses the same embedded MathJax viewer as LaTeX. Inline math accepts
`$...$` and `\(...\)`; display math accepts `$$...$$` and `\[...\]`. Code
spans and fenced code blocks stay literal. Live reload, editor-to-browser and
browser-to-editor source sync, themes, sizing controls, search, and the rest of
the browser navigation work for Markdown too. The Markdown `print` control
opens the browser print dialog instead of invoking a TeX compiler.
Multi-row displays use the same row selection, row-only LaTeX copy, and exact
editor sync as TeX. Put the rows in a MathJax equation-row environment, for
example:

```markdown
$$
\begin{aligned}
f(x) &= x^2 \\
f'(x) &= 2x
\end{aligned}
$$
```

`aligned`, `alignedat`, `gathered`, `split`, and the standard
`align`/`gather`/`multline` family are recognized. A bare `\\` without a row
environment remains ordinary display math because MathJax does not create
individually selectable table rows for it.
Markdown prose, formatted text, tables, and fenced or indented code carry
token-level source anchors, so cursor follow, visual selections, and browser
click-back land on the visible word or code token instead of the whole block.
Same-document links resolve against GitHub-compatible heading fragments: for
example, `[jump](#my-heading)` targets `# My Heading`, while duplicate headings
receive deterministic `-1`, `-2`, … suffixes. The rendered/copyable URL uses
the reserved collision-free form `#mdh:my-heading`, keeping it separate from
viewer, source-sync, and footnote IDs. Footnotes follow Markdown's
case-insensitive label matching, and distinct or repeated definitions always
receive distinct HTML targets.

Raw HTML in Markdown is escaped and shown as source text; it is never injected
into the preview DOM. Local images are served only from beneath the Markdown
file's directory; parent-directory traversal and symlink escapes are rejected.
One-shot renders use absolute `file:` image URLs rooted at that directory, so
images also resolve when `-o` points elsewhere or HTML is redirected from
stdout. Keep the source images in place when opening the rendered HTML.

## Markdown theorems and references

MathPreview understands both Bookdown-style theorem classes and Quarto-style
typed IDs inside Pandoc fenced divs. They render through the same configurable
block formats, but keep the source dialect's reference spelling and visible
text.

A Bookdown theorem names its kind with a class and keeps the reference key in a
separate ID:

```markdown
::: {.theorem #pyth name="Pythagorean theorem"}
For a right triangle, $a^2+b^2=c^2$.
:::

The relation follows from Theorem \@ref(thm:pyth).
```

The heading is `Theorem 1 (Pythagorean theorem)`. The reference itself renders
as the bare number `1`, matching Bookdown; prose such as `Theorem` belongs in
the source around it.

A Quarto theorem encodes both the kind and key in its ID:

```markdown
:::: {#lem-unique}
## Unique factorization

Every integer has the expected factorization.
::::

See @lem-unique.
```

Here the first direct heading becomes the theorem title, so the block heading
is `Lemma 1 (Unique factorization)` and the reference renders as `Lemma 1`.
The supported kind and prefix pairs are:

| Kind | Bookdown class / reference key | Quarto ID / reference |
| --- | --- | --- |
| Theorem | `.theorem` / `thm:key` | `#thm-key` / `@thm-key` |
| Lemma | `.lemma` / `lem:key` | `#lem-key` / `@lem-key` |
| Corollary | `.corollary` / `cor:key` | `#cor-key` / `@cor-key` |
| Proposition | `.proposition` / `prp:key` | `#prp-key` / `@prp-key` |
| Conjecture | `.conjecture` / `cnj:key` | `#cnj-key` / `@cnj-key` |
| Definition | `.definition` / `def:key` | `#def-key` / `@def-key` |
| Example | `.example` / `exm:key` | `#exm-key` / `@exm-key` |
| Exercise | `.exercise` / `exr:key` | `#exr-key` / `@exr-key` |
| Hypothesis | `.hypothesis` / `hyp:key` | — |
| Remark | `.remark`, unnumbered | `#rem-key` / `@rem-key` |
| Solution | `.solution`, unnumbered | `#sol-key` / `@sol-key` |
| Algorithm | — | `#alg-key` / `@alg-key` |
| Proof | `.proof`, unnumbered | `.proof`, unnumbered |

Bookdown references use the complete spelling `\@ref(prefix:key)`. Bookdown
remarks, solutions, and proofs have no theorem number or theorem reference;
Quarto remarks, solutions, and algorithms are numbered. Proofs are unnumbered
in both styles and use the blurred `proof` format by default. Each prefix has
an independent document-wide counter, and Bookdown and Quarto blocks of the
same kind share that counter. Numbering does not reset at headings.

The two reference namespaces deliberately stay distinct: `\@ref(thm:key)`
resolves only a Bookdown `.theorem #key`, while `@thm-key` resolves only a
Quarto `#thm-key`. An unresolved reference remains visible as its original
source instead of silently linking to a similarly named target from the other
dialect.

Use `name="..."` for an explicit plain-text title; `title="..."` is also
accepted as a fallback. `name` wins when both are present. When neither is
present, Quarto consumes the first direct Markdown heading as the title;
Bookdown leaves such a heading in the block body. Fences may use three or more
colons, their closing run need not have the same length, and up to three
leading spaces are accepted. Semantic theorem syntax requires the Pandoc form
with whitespace after the opening colons, such as `::: {.theorem}` or
`::: {#thm-key}`. The compact `:::theorem Optional title` spelling remains a
generic MathPreview custom block for backward compatibility and is not
numbered. Theorem-looking fences and references inside code, math, links,
images, raw HTML, or leading YAML front matter stay literal.

Colon-fence recognition is enabled by default. Setting
`markdown.colon-fences = false` disables all of the colon forms in this
section, including Bookdown and Quarto theorem fences; alternate custom-block
boundaries can remain enabled independently as described below. The same
switch is available as **Recognize `:::` blocks** in the config dialog's
**Markdown config** tab.

## Custom Markdown blocks

The theorem formats above and the blurred `proof` format use the same safe
custom-block registry. Projects can restyle those built-ins or define their
own formats. MathPreview's compact custom-block syntax starts and ends on its
own line:

```markdown
:::proof Proof of the main estimate
The proof may contain **Markdown**, links, lists, and $math$.
:::
```

The proof body starts blurred. Its heading is a real button, so it can be
revealed with a mouse or keyboard. Reveal state survives live moves and
config-only restyling while the authored body is unchanged; editing or
replacing that body conceals it again so a new spoiler is never revealed by
stale UI state. Proofs print unblurred. Blur is only a visual reading aid—not a
way to hide secrets from the generated page or its source.

Restyle a built-in name or define an additional block in the config panel's
TOML editor, in the global config, or in a project's `.mathpreview.toml`. New
names are 1–32 lowercase ASCII characters matching `[a-z][a-z0-9_-]*`; this
example defines a `warning` block:

```toml
[markdown.blocks.warning]
label = "Warning"
appearance = "card"       # plain | bordered | card
reveal = "always"         # always | blur
accent = "#c92a2a"        # optional; #RGB or #RRGGBB only
background = "#fff5f5"   # optional; #RGB or #RRGGBB only
italic = false
```

### Alternate block boundaries

Three or more colons are Pandoc's fenced-div syntax, rather than a standard
Python-Markdown code fence, but Markdown extensions may still assign those
lines another meaning. MathPreview does not claim every `:::blah` block: a
paired colon fence is transformed only when its name is built in or configured,
and unknown names remain literal. If even that name-scoped behavior conflicts
with another renderer used by a project, turn colon fences off and describe the
project's own boundary lines instead:

```toml
[markdown]
colon-fences = false

[markdown.block-syntaxes.jinja-result]
start = [
  '{% call result("{name}", "{title}", "{label}", card={card}) %}',
  '{% call result("{name}", "{title}", "{label}") %}',
  '{% call result("{name}", "{title}", label="{label}", card={card}) %}',
  '{% call result("{name}", "{title}", label="{label}") %}',
  '{% call result("{name}", label="{label}", card={card}) %}',
  '{% call result("{name}", label="{label}", card="{card}") %}',
  '{% call result("{name}", label="{label}") %}',
  '{% call result("{name}", "{title}", card={card}) %}',
  '{% call result("{name}", card={card}) %}',
  '{% call result("{name}", "{title}") %}',
  '{% call result("{name}") %}',
]
end = '{% endcall %}'
```

With that configuration, these existing notes render as a card-style theorem
with a fragment target, followed by an untitled proposition:

```markdown
{% call result("theorem", "Poisson distribution", "t-poisson", card=true) %}
We say $X$ follows the Poisson distribution if
$$
  \P(X = k) = \frac{e^{-\lambda}\lambda^k}{k!}.
$$
{% endcall %}

{% call result("proposition") %}
For every $N$, suppose $X_N \sim \Binom(N, \lambda/N)$.
{% endcall %}
```

`start` accepts either one template string or an array. `{name}` is required
exactly once and selects an enabled entry from `[markdown.blocks]`. `{title}`
captures an optional plain-text title. `{label}` captures an explicit fragment
identifier, and `{card}` captures the lowercase boolean `true` or `false`.
Each optional placeholder may occur at most once, after `{name}`, with literal
text between captures. A true card value forces card presentation for that
invocation. A false or omitted value retains the block's configured
appearance.

Templates match complete lines literally after up to three leading spaces.
Trailing spaces and tabs are ignored. Internal whitespace, quote style,
function name, and uncaptured arguments must match. Notes using single quotes
therefore need corresponding single-quoted templates. The quoted `{card}`
form recognizes an existing `card="true"` call. Do not write `card="false"`
in Jinja: that nonempty string is truthy, while MathPreview's typed capture
would read its contents as false. Prefer unquoted Jinja booleans.

Captured labels use 1–128 ASCII characters. They must start with a letter or
number, followed only by letters, numbers, `_`, or `-`. A standard Markdown
link such as `[Green's theorem](#t-greens)` resolves to a block whose opener
captured `t-greens`. Duplicate labels remain collision-free, and the first
authored label is the target of the plain fragment link. The config field
`[markdown.blocks.NAME].label` is different. It controls the visible heading
for that block type.

To use a `problem` block, register it first so `{name}` can select it.
It is not built in:

```toml
[markdown.blocks.problem]
label = "Problem"
appearance = "bordered"
reveal = "always"
italic = false
```

Syntax IDs follow the same `[a-z][a-z0-9_-]*` grammar and 32-character limit
as block names. At most 32 syntaxes may be enabled, with 1–16 start templates
per syntax. A higher-priority config replaces a syntax declaration as a unit.
Use `enabled = false` under the same syntax ID to disable an inherited one.

This is boundary recognition, not a Jinja interpreter. MathPreview captures
only the configured fields and does not execute expressions. In particular,
raw `{{ ref(...) }}` calls and custom wiki-link syntax remain literal. Matched
boundary lines are omitted from the preview and the body is parsed as ordinary
Markdown. These alternate blocks use the selected custom presentation only.
They are generic and unnumbered, and their `{label}` targets are separate from
Bookdown or Quarto theorem references. Delimiter-looking lines inside code,
math, links, images, raw HTML, or leading YAML front matter remain literal. An
exact template match with an
unknown block name, a stray end marker, or a recognized opener without a
matching end remains literal. A line that does not match any listed start
template is ordinary Markdown: if such an unlisted opener is nested inside a
configured block and uses the same literal end marker, its next end marker
will close the configured block. List every opener shape that may be nested
under a shared closer. For example, add separate templates for positional and
keyword labels when the notes use both forms. Custom syntaxes retain the same
32-level nesting bound as colon-fenced blocks.

In the compact colon form, the text after the block name is an optional
plain-text title. Known blocks may be nested up to 32 levels, and up to three
leading spaces are accepted. Unknown or unclosed recognized fences remain
literal; an invalid colon-looking line is ordinary Markdown and does not add a
nesting level. Parsing beyond 32 nested levels fails closed. Set
`enabled = false` to disable a built-in theorem/proof name or a format inherited
from a global config. Configured labels must be trimmed, control-free plain text
of 1–80 characters. Labels and titles are escaped, colors are strictly
validated, and custom formats never accept HTML, CSS, URLs, or JavaScript.
Pandoc attributes other than the recognized class, ID, `name`, and `title` are
ignored rather than copied into the preview DOM. Raw HTML inside a block
remains inert just like raw HTML elsewhere.

Markdown support is intentionally single-file for now: each `.md` or
`.markdown` file is its own preview root. The Bookdown and Quarto spellings
above are compatibility syntax; MathPreview does not run R, knitr, Pandoc,
Bookdown, or Quarto, and `.Rmd` / `.qmd` are not Markdown preview extensions.
Markdown citations, general cross-references outside the theorem forms above,
LaTeX theorem/proof roles, heading-based theorem resets, and multi-file
includes are not interpreted yet. TikZ remains a LaTeX-frontend feature.

Markdown does not add another runtime dependency. It is parsed by the same
compiled `mathpreview-cli`, whether that binary was built with Cargo or
downloaded from GitHub Releases, and the live viewer serves MathJax from that
binary. Users do not install a Markdown renderer, MathJax, Node/npm, or TeX for
Markdown preview.
