//! AST → HTML renderer.
//!
//! Output is a single self-contained HTML document that loads MathJax v3
//! and feeds it the macros / extensions extracted from the project preamble.
//! Math expressions are emitted with `\(...\)` / `\[...\]` delimiters so
//! MathJax typesets them after the page loads.

use std::fmt::Write;
use std::path::PathBuf;

use serde::Serialize;

use std::collections::HashMap;

use crate::ast::{ListKind, Node, NodeKind, RefKind, Role, Span};
use crate::bibtex::{BibEntry, BibStyle};
use crate::macros::ExtractedPreamble;
use crate::numbering::LabelTable;
use crate::sync::SyncIndex;

#[derive(Debug, Clone)]
pub struct HtmlOptions {
    /// URL or relative path the page should load MathJax from. Default points
    /// at the project's vendored copy at `mathjax/es5/tex-chtml.js`; switch to
    /// a CDN URL for quick browser checks.
    pub mathjax_url: String,
    /// Document title.
    pub title: String,
    /// Whether to embed the default stylesheet inline. Off if you want to
    /// supply your own.
    pub inline_css: bool,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            mathjax_url: "https://cdn.jsdelivr.net/npm/mathjax@3/es5/tex-svg.js".into(),
            title: "mathpreview".into(),
            inline_css: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderOutput {
    /// Full standalone HTML page — what the `render` CLI subcommand writes.
    pub html: String,
    /// Just the `<main id="page">` inner content — what `serve` pushes over
    /// WebSocket on each file change. Avoids re-sending the head/shell/JS.
    pub body_html: String,
    /// Top-level blocks, each wrapped in `<article class="blk">` with a
    /// stable id + content hash. The server diffs old vs. new block
    /// sequences and pushes only the changed blocks over WebSocket.
    pub blocks: Vec<RenderedBlock>,
    pub sync: SyncIndex,
    pub root_file: PathBuf,
    pub preamble: ExtractedPreamble,
    pub included_files: Vec<PathBuf>,
}

/// Both forms of the rendered output, returned together so `render_project`
/// can populate `RenderOutput` without re-running the AST walk.
#[derive(Debug, Clone)]
pub struct RenderedHtml {
    pub full: String,
    pub body: String,
    pub blocks: Vec<RenderedBlock>,
}

/// One top-level block wrapped in `<article class="blk">`, ready for the
/// diffing pass to compare against the previous render.
#[derive(Debug, Clone, Serialize)]
pub struct RenderedBlock {
    pub id: String,
    pub hash: String,
    pub html: String,
}

pub fn render(
    nodes: &[Node],
    preamble: &ExtractedPreamble,
    labels: &LabelTable,
    bib: &HashMap<String, BibEntry>,
    bib_style: BibStyle,
    sync: &mut SyncIndex,
    opts: &HtmlOptions,
) -> RenderedHtml {
    let mut idgen = IdGen::default();
    let mut ctx = RenderCtx {
        sync,
        idgen: &mut idgen,
        labels,
        bib,
        bib_style,
        preamble,
    };

    // Top-level inline runs become paragraph blocks. Structural nodes
    // (sections, displays, theorem-likes, lists, etc.) stay as their own
    // blocks. The wrapper uses `display: contents` in CSS so it doesn't affect
    // visual layout — it exists purely so the diff/patch path can find and
    // replace blocks by id.
    let mut blocks: Vec<RenderedBlock> = Vec::with_capacity(nodes.len());
    let mut paragraph = String::new();
    let mut paragraph_start: Option<usize> = None;
    for (i, node) in nodes.iter().enumerate() {
        if is_blank_separator_node(node) {
            flush_paragraph(&mut blocks, &mut paragraph, &mut paragraph_start);
            continue;
        }

        if is_top_level_inline_node(node) {
            if paragraph_start.is_none() {
                paragraph_start = Some(i);
            }
            write_node(&mut paragraph, node, &mut ctx);
            continue;
        }

        flush_paragraph(&mut blocks, &mut paragraph, &mut paragraph_start);
        let mut inner = String::new();
        write_node(&mut inner, node, &mut ctx);
        // Skip empty emissions (e.g. discarded comments, no-op opaque cmds)
        // so we don't pollute the block sequence with phantoms whose hash
        // would still match across renders but waste id space.
        if inner.trim().is_empty() {
            continue;
        }
        push_block(&mut blocks, i, inner);
    }
    flush_paragraph(&mut blocks, &mut paragraph, &mut paragraph_start);

    let body: String = blocks.iter().map(|b| b.html.as_str()).collect();
    let full = wrap_in_shell(&body, preamble, opts);
    RenderedHtml { full, body, blocks }
}

fn flush_paragraph(
    blocks: &mut Vec<RenderedBlock>,
    paragraph: &mut String,
    paragraph_start: &mut Option<usize>,
) {
    let Some(start) = paragraph_start.take() else {
        return;
    };
    if paragraph.trim().is_empty() {
        paragraph.clear();
        return;
    }
    let inner = format!(r#"<p class="para">{}</p>"#, std::mem::take(paragraph));
    push_block(blocks, start, inner);
}

fn push_block(blocks: &mut Vec<RenderedBlock>, index: usize, inner: String) {
    let id = format!("blk-{}", index + 1);
    let hash = fnv_hash(&inner);
    let html = format!(
        r#"<article class="blk" id="{id}" data-blockhash="{hash}">{inner}</article>"#,
        id = id,
        hash = hash,
        inner = inner,
    );
    blocks.push(RenderedBlock { id, hash, html });
}

fn is_top_level_inline_node(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Text(_)
            | NodeKind::InlineMath(_)
            | NodeKind::Ref { .. }
            | NodeKind::Cite { .. }
            | NodeKind::OpaqueCmd { .. }
            | NodeKind::Comment(_)
    )
}

fn is_blank_separator_node(node: &Node) -> bool {
    matches!(&node.kind, NodeKind::Text(s) if is_blank_line_separator(s))
}

fn wrap_in_shell(body: &str, preamble: &ExtractedPreamble, opts: &HtmlOptions) -> String {
    let mathjax_config = mathjax_config(preamble);
    let warnings_html = warnings_panel(preamble);
    let css = if opts.inline_css { DEFAULT_CSS } else { "" };

    let mut out = String::new();
    let title = escape_html(&opts.title);
    let mathjax_url = escape_attr(&opts.mathjax_url);
    write!(
        out,
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<style>{css}</style>
<script>
{mathjax_config}
</script>
<script src="{mathjax_url}" async></script>
</head>
<body>
<header class="topbar">
  <strong>mathpreview</strong>
  <span class="status" id="ws-status" title="live-reload status"></span>
  <span class="topbar-spacer"></span>
  <button class="server-restart" id="server-restart" type="button" title="restart preview server">restart</button>
  <span class="proof-toggle" data-mode="all">
    <button data-mode="main">main only</button>
    <button data-mode="supporting">+ supporting</button>
    <button data-mode="all" class="active">all</button>
  </span>
</header>
{warnings_html}
<main id="page" data-proof-mode="all">
{body}
</main>
<aside id="margin"></aside>
<script>
{client_js}
</script>
</body>
</html>
"#,
        client_js = CLIENT_JS,
    )
    .unwrap();
    out
}

#[derive(Default)]
struct IdGen {
    counter: u32,
}
impl IdGen {
    fn next(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}-{}", self.counter)
    }
}

struct RenderCtx<'a> {
    sync: &'a mut SyncIndex,
    idgen: &'a mut IdGen,
    labels: &'a LabelTable,
    bib: &'a HashMap<String, BibEntry>,
    bib_style: BibStyle,
    preamble: &'a ExtractedPreamble,
}

fn record(ctx: &mut RenderCtx, id: &str, span: &Span, label: Option<&str>) {
    ctx.sync.record(
        id.to_string(),
        span.file.clone(),
        span.start,
        span.end,
        label.map(str::to_string),
    );
}

fn data_src(span: &Span) -> String {
    format!(
        "{}:{}:{}",
        span.file.display(),
        span.start.line,
        span.start.col,
    )
}

fn write_node(out: &mut String, n: &Node, ctx: &mut RenderCtx) {
    match &n.kind {
        NodeKind::Document => {
            for c in &n.children {
                write_node(out, c, ctx);
            }
        }
        NodeKind::Text(s) => {
            // Route through the inline parser so accents (`\'e` → é) and any
            // straggler inline commands get processed instead of leaking as
            // literal backslash sequences.
            if is_blank_line_separator(s) {
                out.push_str(r#"<div class="para-break" aria-hidden="true"></div>"#);
            } else {
                out.push_str(&render_inline_latex(s, ctx.labels));
            }
        }
        NodeKind::Comment(_) => { /* discard */ }
        NodeKind::Section {
            level,
            title,
            label,
            number,
        } => {
            let id = label
                .as_deref()
                .map(sanitize_id)
                .unwrap_or_else(|| ctx.idgen.next("sec"));
            record(ctx, &id, &n.span, label.as_deref());
            // \part=0, \chapter=1 → h1; \section=2 → h2; …; subparagraph=6 → h6.
            let h = (*level).clamp(1, 6);
            let num_html = number
                .as_deref()
                .map(|n| format!(r#"<span class="sec-num">{}</span> "#, escape_html(n)))
                .unwrap_or_default();
            writeln!(
                out,
                r#"<h{h} id="{id}" class="sec-h{level}" data-src="{src}">{num}{title}</h{h}>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                level = level,
                num = num_html,
                title = render_inline_latex(title, ctx.labels),
                h = h,
            )
            .unwrap();
        }
        NodeKind::Theorem {
            env,
            role,
            name,
            label,
            omit_ref,
            number,
            ..
        } => {
            let id = label
                .as_deref()
                .map(sanitize_id)
                .unwrap_or_else(|| ctx.idgen.next("thm"));
            record(ctx, &id, &n.span, label.as_deref());
            let role_class = role.as_css_class();
            let kind_label = capitalize(env);
            let num_html = number
                .as_deref()
                .map(|n| format!(r#" <span class="thm-num">{}</span>"#, escape_html(n)))
                .unwrap_or_default();
            let name_html = name
                .as_deref()
                .map(|s| {
                    format!(
                        r#" <span class="thm-name">({})</span>"#,
                        render_inline_latex(s, ctx.labels)
                    )
                })
                .unwrap_or_default();
            let role_pill = role_pill_html(*role);
            writeln!(
                out,
                r#"<div class="thm {env_class} {role_class}" id="{id}" data-src="{src}">"#,
                env_class = format_args!("env-{env}"),
                role_class = role_class,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            writeln!(
                out,
                r#"<div class="thm-head"><span class="thm-kind">{kind_label}</span>{num_html}{name_html}{role_pill}</div>"#,
            ).unwrap();
            out.push_str(r#"<div class="thm-body">"#);
            for c in &n.children {
                write_node(out, c, ctx);
            }
            out.push_str("</div>");
            if let Some(omit) = omit_ref {
                writeln!(
                    out,
                    r#"<div class="thm-omitref">See: {}</div>"#,
                    render_inline_latex(omit, ctx.labels)
                )
                .unwrap();
            }
            out.push_str("</div>\n");
        }
        NodeKind::Proof { of } => {
            let id = ctx.idgen.next("proof");
            record(ctx, &id, &n.span, None);
            let head = match of {
                Some(o) => format!(r#"<div class="proof-head" role="button" tabindex="0"><span class="fold-marker"></span>Proof <span class="proof-of">({})</span>.</div>"#, render_inline_latex(o, ctx.labels)),
                None => r#"<div class="proof-head" role="button" tabindex="0"><span class="fold-marker"></span>Proof.</div>"#.to_string(),
            };
            writeln!(
                out,
                r#"<div class="proof" id="{id}" data-src="{src}">{head}<div class="proof-body">"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            for c in &n.children {
                write_node(out, c, ctx);
            }
            out.push_str(r#"<span class="qed">∎</span></div></div>"#);
            out.push('\n');
        }
        NodeKind::InlineMath(s) => {
            let id = ctx.idgen.next("im");
            record(ctx, &id, &n.span, None);
            let hash = fnv_hash(&format!("i:{s}"));
            // Use \( \) so MathJax doesn't typeset the literal `$` text.
            write!(
                out,
                r#"<span class="math inline" id="{id}" data-src="{src}" data-hash="{hash}">\({}\)</span>"#,
                escape_math(s),
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                hash = hash,
            ).unwrap();
        }
        NodeKind::DisplayMath {
            body,
            env,
            label,
            number,
        } => {
            let id = label
                .as_deref()
                .map(sanitize_id)
                .unwrap_or_else(|| ctx.idgen.next("dm"));
            record(ctx, &id, &n.span, label.as_deref());
            // Strip `\label{...}` — we resolve refs through our own LabelTable
            // and MathJax otherwise warns "Label: multiply defined" when the
            // same equation is typeset across live-reload updates.
            let body_clean = strip_labels(body);
            let math = match env {
                Some(e) => format!(r"\begin{{{e}}}{}\end{{{e}}}", body_clean),
                None => format!(r"\[{}\]", body_clean),
            };
            let num_html = number
                .as_deref()
                .map(|n| format!(r#"<span class="eq-num">({})</span>"#, escape_html(n)))
                .unwrap_or_default();
            let hash = fnv_hash(&format!(
                "d:{}:{}:{}",
                env.as_deref().unwrap_or("[]"),
                number.as_deref().unwrap_or(""),
                math,
            ));
            writeln!(
                out,
                r#"<div class="math display" id="{id}" data-src="{src}" data-hash="{hash}">{math}{num_html}</div>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                math = escape_math(&math),
                hash = hash,
            ).unwrap();
        }
        NodeKind::Ref { kind, key } => {
            let target = sanitize_id(key);
            let label = ctx.labels.resolve_ref(*kind, key);
            write!(
                out,
                r##"<a class="ref" href="#{target}" data-target="{key}" data-kind="{kind_str}">{label}</a>"##,
                target = escape_attr(&target),
                key = escape_attr(key),
                kind_str = match kind {
                    RefKind::Ref => "ref",
                    RefKind::Eqref => "eqref",
                    RefKind::Cref => "cref",
                    RefKind::Autoref => "autoref",
                    RefKind::Pageref => "pageref",
                    RefKind::Nameref => "nameref",
                },
                label = escape_html(&label),
            ).unwrap();
        }
        NodeKind::Cite { keys } => {
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    let n = ctx.labels.citation_number.get(k).copied().unwrap_or(0);
                    match ctx.labels.citation_display.get(k) {
                        Some(disp) => format!(
                            r##"<a class="cite" href="#bib-{n}" data-key="{key}">{label}</a>"##,
                            n = n,
                            key = escape_attr(k),
                            label = escape_html(disp),
                        ),
                        None => format!(
                            r#"<span class="cite missing" data-key="{key}">{label}</span>"#,
                            key = escape_attr(k),
                            label = escape_html(k),
                        ),
                    }
                })
                .collect();
            // Author-year style uses parentheses, the rest use square brackets.
            let (l, r) = match ctx.bib_style {
                BibStyle::AuthorYear => ('(', ')'),
                _ => ('[', ']'),
            };
            write!(out, "{l}{}{r}", parts.join("; ")).unwrap();
        }
        NodeKind::Bibliography => {
            writeln!(
                out,
                r#"<section class="references" data-src="{src}">"#,
                src = escape_attr(&data_src(&n.span))
            )
            .unwrap();
            writeln!(out, "<h2>References</h2>").unwrap();
            let style_class = match ctx.bib_style {
                BibStyle::Numeric => "bib-style-numeric",
                BibStyle::Alphabetic => "bib-style-alphabetic",
                BibStyle::AuthorYear => "bib-style-authoryear",
            };
            writeln!(out, r#"<dl class="bib-list {style_class}">"#).unwrap();
            for key in &ctx.labels.cite_order {
                let num = ctx.labels.citation_number.get(key).copied().unwrap_or(0);
                let label = ctx
                    .labels
                    .citation_display
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| key.clone());
                let body = match ctx.bib.get(key) {
                    Some(entry) => format_bib_entry(entry, ctx.bib_style),
                    None => format!(
                        r#"<span class="bib-missing">(no entry for <code>{}</code>)</span>"#,
                        escape_html(key)
                    ),
                };
                let label_html = match ctx.bib_style {
                    BibStyle::AuthorYear => format!("{label}.", label = escape_html(&label)),
                    _ => format!("[{label}]", label = escape_html(&label)),
                };
                writeln!(
                    out,
                    r#"<dt id="bib-{n}" class="bib-label" data-key="{key}">{label_html}</dt><dd class="bib-entry">{body}</dd>"#,
                    n = num,
                    key = escape_attr(key),
                ).unwrap();
            }
            writeln!(out, "</dl></section>").unwrap();
        }
        NodeKind::List { kind } => {
            let (open, close) = match kind {
                ListKind::Enumerate => ("<ol class=\"latex-list enumerate\">", "</ol>"),
                ListKind::Itemize => ("<ul class=\"latex-list itemize\">", "</ul>"),
                ListKind::Description => ("<dl class=\"latex-list description\">", "</dl>"),
            };
            writeln!(out, r#"{open}"#).unwrap();
            for c in &n.children {
                write_node(out, c, ctx);
            }
            writeln!(out, "{close}").unwrap();
        }
        NodeKind::ListItem { marker } => {
            // Find enclosing list kind by looking at the previous open tag —
            // simpler: peek at the marker. description items have a marker;
            // others don't (the marker arg can still be supplied, but is rare
            // outside description).
            //
            // We don't have direct access to the parent's kind here, so we
            // emit using the marker as the cue: if a marker is present we
            // write <dt>/<dd>, otherwise <li>.
            if let Some(m) = marker {
                write!(
                    out,
                    "<dt class=\"item-marker\">{}</dt><dd class=\"item-body\">",
                    render_inline_latex(m, ctx.labels)
                )
                .unwrap();
                for c in &n.children {
                    write_node(out, c, ctx);
                }
                writeln!(out, "</dd>").unwrap();
            } else {
                write!(out, "<li class=\"item-body\">").unwrap();
                for c in &n.children {
                    write_node(out, c, ctx);
                }
                writeln!(out, "</li>").unwrap();
            }
        }
        NodeKind::MakeTitle => {
            let title = ctx.preamble.title.as_deref();
            let author = ctx.preamble.author.as_deref();
            let date = ctx.preamble.date.as_deref();
            if title.is_none() && author.is_none() && date.is_none() {
                return;
            }
            out.push_str(r#"<div class="title-block">"#);
            if let Some(t) = title {
                writeln!(
                    out,
                    r#"<h1 class="paper-title">{}</h1>"#,
                    render_inline_latex(t, ctx.labels)
                )
                .unwrap();
            }
            if let Some(a) = author {
                writeln!(
                    out,
                    r#"<div class="paper-author">{}</div>"#,
                    render_inline_latex(a, ctx.labels)
                )
                .unwrap();
            }
            if let Some(d) = date {
                writeln!(
                    out,
                    r#"<div class="paper-date">{}</div>"#,
                    render_inline_latex(d, ctx.labels)
                )
                .unwrap();
            }
            out.push_str("</div>\n");
        }
        NodeKind::OpaqueEnv { env, body } => {
            // Best-effort: render verbatim text content so the reader still
            // sees the words. Math inside opaque envs won't be typeset.
            writeln!(
                out,
                r#"<div class="opaque-env" data-env="{env}">{body}</div>"#,
                env = escape_attr(env),
                body = escape_html(body),
            )
            .unwrap();
        }
        NodeKind::OpaqueCmd { name, raw } => {
            match name.as_str() {
                "today" => out.push_str("(today)"),
                "LaTeX" => out.push_str("LaTeX"),
                "TeX" => out.push_str("TeX"),
                "ldots" | "dots" => out.push('…'),
                "label" | "vspace" | "hspace" | "smallskip" | "medskip" | "bigskip" | "newpage"
                | "clearpage" | "noindent" | "indent" | "linebreak" | "pagebreak" | "thanks" => {}
                _ => {
                    // Route the raw token through the inline parser so known
                    // text commands (\emph, \textbf, accents, refs that
                    // slipped past the body parser) render correctly and
                    // unknown ones fall back to their content argument.
                    out.push_str(&render_inline_latex(raw, ctx.labels));
                }
            }
        }
    }
}

/// FNV-1a 64-bit, hex-encoded. Stable across runs — used by the client to
/// dedupe math elements between re-renders so MathJax doesn't re-typeset
/// unchanged ones.
fn fnv_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

/// Remove `\label{anything}` from a math body. Brace-balanced so labels
/// containing nested braces (rare but legal) are handled.
fn strip_labels(body: &str) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\\label") {
            let mut j = i + b"\\label".len();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'{' {
                let mut depth = 1i32;
                let mut k = j + 1;
                while k < bytes.len() {
                    match bytes[k] {
                        b'\\' if k + 1 < bytes.len() => {
                            k += 2;
                            continue;
                        }
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                i = (k + 1).min(bytes.len());
                continue;
            }
        }
        if bytes[i].is_ascii() {
            out.push(bytes[i] as char);
            i += 1;
        } else {
            let ch = body[i..].chars().next().unwrap_or('\0');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn format_bib_entry(e: &BibEntry, style: BibStyle) -> String {
    let author = e
        .fields
        .get("author")
        .map(|a| escape_html(&format_authors(a)));
    let year = e.fields.get("year").map(|s| escape_html(s));
    let title = e.fields.get("title").map(|s| escape_html(s));
    let venue = e
        .fields
        .get("journal")
        .or_else(|| e.fields.get("booktitle"))
        .or_else(|| e.fields.get("publisher"));
    let edition = e.fields.get("edition").map(|s| escape_html(s));
    let series = e.fields.get("series").map(|s| escape_html(s));
    let volume = e.fields.get("volume").map(|s| escape_html(s));
    let number = e.fields.get("number").map(|s| escape_html(s));
    let pages = e.fields.get("pages").map(|s| escape_html(s));
    let address = e.fields.get("address").map(|s| escape_html(s));
    let doi = e.fields.get("doi");
    let url = e.fields.get("url");

    let mut parts: Vec<String> = Vec::new();
    match style {
        BibStyle::AuthorYear => {
            // (Author1, Author2). (Year). *Title*. Venue, …
            if let Some(a) = &author {
                parts.push(a.clone());
            }
            if let Some(y) = &year {
                parts.push(format!("({})", y));
            }
            if let Some(t) = &title {
                parts.push(format!("<em>{}</em>", t));
            }
        }
        _ => {
            // Author, A. (Year). *Title*. Venue, …  — same shape; year goes
            // after author for both numeric and alphabetic.
            if let Some(a) = &author {
                parts.push(a.clone());
            }
            if let Some(t) = &title {
                parts.push(format!("<em>{}</em>", t));
            }
            if let Some(y) = &year {
                parts.push(y.clone());
            }
        }
    }
    let mut venue_str = String::new();
    if let Some(v) = venue {
        venue_str.push_str(&escape_html(v));
    }
    if let Some(s) = &series {
        if !venue_str.is_empty() {
            venue_str.push_str(", ");
        }
        venue_str.push_str(s);
    }
    if let Some(v) = &volume {
        if !venue_str.is_empty() {
            venue_str.push(' ');
        }
        venue_str.push_str(&format!("vol. {v}"));
        if let Some(n) = &number {
            venue_str.push_str(&format!(" no. {n}"));
        }
    } else if let Some(n) = &number {
        if !venue_str.is_empty() {
            venue_str.push(' ');
        }
        venue_str.push_str(&format!("no. {n}"));
    }
    if let Some(a) = &address {
        if !venue_str.is_empty() {
            venue_str.push_str(", ");
        }
        venue_str.push_str(a);
    }
    if let Some(p) = &pages {
        if !venue_str.is_empty() {
            venue_str.push_str(", ");
        }
        venue_str.push_str(&format!("pp. {p}"));
    }
    if let Some(ed) = &edition {
        if !venue_str.is_empty() {
            venue_str.push_str(", ");
        }
        venue_str.push_str(&format!("{ed} ed."));
    }
    if !venue_str.is_empty() {
        parts.push(venue_str);
    }
    if let Some(d) = doi {
        parts.push(format!(
            r#"<a class="bib-doi" href="https://doi.org/{d}" target="_blank" rel="noopener">doi:{d}</a>"#,
            d = escape_attr(d),
        ));
    } else if let Some(u) = url {
        parts.push(format!(
            r#"<a class="bib-url" href="{u}" target="_blank" rel="noopener">{u}</a>"#,
            u = escape_attr(u),
        ));
    }
    let mut s = parts.join(". ");
    if !s.ends_with('.') {
        s.push('.');
    }
    s
}

fn format_authors(a: &str) -> String {
    // BibTeX "and"-separated author list. Keep raw "Last, First" form; just
    // swap " and " for "; " for readability.
    a.split(" and ")
        .map(|s| s.trim())
        .collect::<Vec<_>>()
        .join("; ")
}

fn is_blank_line_separator(s: &str) -> bool {
    if s.chars().any(|c| !c.is_whitespace()) {
        return false;
    }
    let mut newlines = 0u8;
    for ch in s.chars() {
        match ch {
            '\n' => {
                newlines += 1;
                if newlines >= 2 {
                    return true;
                }
            }
            ' ' | '\t' | '\r' => {}
            _ => newlines = 0,
        }
    }
    false
}

fn role_pill_html(role: Role) -> String {
    let (label, cls) = match role {
        Role::Main => ("main", "role-pill role-main"),
        Role::Supporting => ("supporting", "role-pill role-supporting"),
        Role::Standard => ("standard", "role-pill role-standard"),
        Role::Omitted => ("omitted", "role-pill role-omitted"),
    };
    format!(r#"<span class="{cls}">{label}</span>"#)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// LaTeX-text → HTML for strings extracted into AST fields (section titles,
/// theorem names, proof "of" args, omitref payloads). Handles a curated set
/// of inline commands so embedded `\ref` / `\emph` / `\textbf` etc. don't
/// reach MathJax or land in the output as raw `\name{...}` source.
fn render_inline_latex(s: &str, labels: &LabelTable) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'~' {
            out.push('\u{00a0}');
            i += 1;
            continue;
        }
        // Paragraph break: two or more consecutive newlines (with possible
        // intermediate whitespace) → `<br><br>`. A single newline is just
        // inter-word whitespace, as in LaTeX.
        if b == b'\n' {
            let mut j = i;
            let mut nl = 0;
            while j < bytes.len() {
                match bytes[j] {
                    b'\n' => {
                        nl += 1;
                        j += 1;
                    }
                    b' ' | b'\t' | b'\r' => j += 1,
                    _ => break,
                }
            }
            if nl >= 2 {
                out.push_str("<br><br>");
            } else if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            i = j;
            continue;
        }
        if b != b'\\' {
            // Inline escape of HTML-special chars only — math is unlikely here.
            if b.is_ascii() {
                match b {
                    b'<' => out.push_str("&lt;"),
                    b'>' => out.push_str("&gt;"),
                    b'&' => out.push_str("&amp;"),
                    _ => out.push(b as char),
                }
                i += 1;
            } else {
                let ch = s[i..].chars().next().unwrap_or('\0');
                out.push(ch);
                i += ch.len_utf8();
            }
            continue;
        }
        // We're at `\` — try to parse a command name.
        let cmd_start = i + 1;
        let mut cmd_end = cmd_start;
        while cmd_end < bytes.len()
            && (bytes[cmd_end].is_ascii_alphabetic() || bytes[cmd_end] == b'*')
        {
            cmd_end += 1;
        }
        if cmd_end == cmd_start {
            // `\` followed by punctuation — accent commands, spacing macros,
            // and a few escapes.
            if cmd_start < bytes.len() {
                let p = bytes[cmd_start];
                // Accent commands: \'e, \`a, \"o, \^u, \~n, \.z, \=a.
                let accent = match p {
                    b'\'' => Some('\u{0301}'),
                    b'`' => Some('\u{0300}'),
                    b'"' => Some('\u{0308}'),
                    b'^' => Some('\u{0302}'),
                    b'~' => Some('\u{0303}'),
                    b'.' => Some('\u{0307}'),
                    b'=' => Some('\u{0304}'),
                    _ => None,
                };
                if let Some(acc) = accent {
                    let mut q = cmd_start + 1;
                    while q < bytes.len() && bytes[q] == b' ' {
                        q += 1;
                    }
                    if q < bytes.len() && bytes[q] == b'{' {
                        // \'{e} — brace arg.
                        let start = q + 1;
                        let mut depth = 1i32;
                        let mut k = start;
                        while k < bytes.len() {
                            match bytes[k] {
                                b'\\' if k + 1 < bytes.len() => {
                                    k += 2;
                                    continue;
                                }
                                b'{' => depth += 1,
                                b'}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            k += 1;
                        }
                        for ch in s[start..k].chars() {
                            out.push(ch);
                            if ch.is_alphabetic() {
                                out.push(acc);
                            }
                        }
                        i = (k + 1).min(bytes.len());
                        continue;
                    } else if q < bytes.len() {
                        // \'e — single-char arg.
                        let ch = s[q..].chars().next().unwrap_or(' ');
                        out.push(ch);
                        if ch.is_alphabetic() {
                            out.push(acc);
                        }
                        i = q + ch.len_utf8();
                        continue;
                    }
                }
                match p {
                    b',' | b';' | b':' | b'!' | b' ' => {
                        i = cmd_start + 1;
                        continue;
                    }
                    b'\\' => {
                        out.push_str("<br>");
                        i = cmd_start + 1;
                        continue;
                    }
                    _ => {
                        out.push('\\');
                        i += 1;
                        continue;
                    }
                }
            }
            out.push('\\');
            i += 1;
            continue;
        }
        let name = &s[cmd_start..cmd_end];
        // Read one balanced brace arg if present.
        let mut j = cmd_end;
        while j < bytes.len() && bytes[j] == b' ' {
            j += 1;
        }
        let arg = if j < bytes.len() && bytes[j] == b'{' {
            let mut depth = 0i32;
            let mut k = j;
            let start = j + 1;
            let mut end = bytes.len();
            while k < bytes.len() {
                match bytes[k] {
                    b'\\' if k + 1 < bytes.len() => {
                        k += 2;
                        continue;
                    }
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = k;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            Some((&s[start..end], k + 1))
        } else {
            None
        };

        match (name, arg) {
            ("ref", Some((key, next))) | ("pageref", Some((key, next))) => {
                let text = labels.resolve_ref(crate::ast::RefKind::Ref, key);
                let target = sanitize_id(key);
                write!(
                    out,
                    r##"<a class="ref" href="#{t}">{label}</a>"##,
                    t = escape_attr(&target),
                    label = escape_html(&text)
                )
                .unwrap();
                i = next;
            }
            ("cref", Some((key, next)))
            | ("Cref", Some((key, next)))
            | ("autoref", Some((key, next))) => {
                let text = labels.resolve_ref(crate::ast::RefKind::Cref, key);
                let target = sanitize_id(key);
                write!(
                    out,
                    r##"<a class="ref" href="#{t}">{label}</a>"##,
                    t = escape_attr(&target),
                    label = escape_html(&text)
                )
                .unwrap();
                i = next;
            }
            ("eqref", Some((key, next))) => {
                let text = labels.resolve_ref(crate::ast::RefKind::Eqref, key);
                let target = sanitize_id(key);
                write!(
                    out,
                    r##"<a class="ref" href="#{t}">{label}</a>"##,
                    t = escape_attr(&target),
                    label = escape_html(&text)
                )
                .unwrap();
                i = next;
            }
            ("emph", Some((inner, next))) | ("textit", Some((inner, next))) => {
                write!(out, "<em>{}</em>", render_inline_latex(inner, labels)).unwrap();
                i = next;
            }
            ("textbf", Some((inner, next))) | ("bf", Some((inner, next))) => {
                write!(
                    out,
                    "<strong>{}</strong>",
                    render_inline_latex(inner, labels)
                )
                .unwrap();
                i = next;
            }
            ("texttt", Some((inner, next))) => {
                write!(out, "<code>{}</code>", render_inline_latex(inner, labels)).unwrap();
                i = next;
            }
            ("textsc", Some((inner, next))) => {
                write!(
                    out,
                    r#"<span class="sc">{}</span>"#,
                    render_inline_latex(inner, labels)
                )
                .unwrap();
                i = next;
            }
            (other, Some((inner, next))) => {
                // Unknown command with one arg — render the arg's text so the
                // reader still sees the words. Drop the command name.
                let _ = other;
                out.push_str(&render_inline_latex(inner, labels));
                i = next;
            }
            (other, None) => {
                // Unknown command without an arg — silently drop. Most are
                // text-mode helpers (\TeX, \LaTeX, \today, \xspace).
                let _ = other;
                i = cmd_end;
            }
        }
    }
    out
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    escape_html(s)
}

fn escape_math(s: &str) -> String {
    // Inside HTML, we still need to escape `<` and `&` (MathJax sees the
    // text content of the element). `<` shows up rarely in math; `&` is
    // common in `align`.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(c),
        }
    }
    out
}

fn warnings_panel(preamble: &ExtractedPreamble) -> String {
    if preamble.warnings.is_empty() && preamble.unmapped_packages.is_empty() {
        return String::new();
    }
    let mut html = String::from(r#"<details class="warnings"><summary>"#);
    let n = preamble.warnings.len();
    let u = preamble.unmapped_packages.len();
    write!(
        html,
        "{} macro warning{}, {} unmapped package{}",
        n,
        if n == 1 { "" } else { "s" },
        u,
        if u == 1 { "" } else { "s" },
    )
    .unwrap();
    html.push_str("</summary><ul>");
    for w in &preamble.warnings {
        write!(html, "<li>{}</li>", escape_html(w)).unwrap();
    }
    if !preamble.unmapped_packages.is_empty() {
        write!(
            html,
            "<li>unmapped: {}</li>",
            escape_html(&preamble.unmapped_packages.join(", "))
        )
        .unwrap();
    }
    html.push_str("</ul></details>");
    html
}

fn mathjax_config(preamble: &ExtractedPreamble) -> String {
    let mut macros = String::new();
    for (i, m) in preamble.macros.iter().enumerate() {
        if i > 0 {
            macros.push_str(",\n      ");
        }
        let name_json = json_string(&m.name);
        let body_json = json_string(&m.body);
        match (m.n_args, &m.default) {
            (0, _) => write!(macros, "{}: {}", name_json, body_json).unwrap(),
            (n, None) => write!(macros, "{}: [{}, {}]", name_json, body_json, n).unwrap(),
            (n, Some(d)) => {
                let d_json = json_string(d);
                write!(macros, "{}: [{}, {}, {}]", name_json, body_json, n, d_json).unwrap();
            }
        }
    }

    let package_short: Vec<String> = preamble
        .packages_short
        .iter()
        .map(|s| json_string(s))
        .collect();
    let package_long: Vec<String> = preamble
        .packages_long
        .iter()
        .map(|s| json_string(s))
        .collect();

    format!(
        r#"window.MathJax = {{
  tex: {{
    packages: {{ '[+]': [{packages_short}] }},
    inlineMath: [['\\(', '\\)']],
    displayMath: [['\\[', '\\]']],
    // We compute all equation numbers in Rust and emit them as <span
    // class="eq-num">. Leaving MathJax's auto-tagging on produces a second
    // number column and, when labels collide or fail, "(???)" placeholders.
    tags: 'none',
    macros: {{
      {macros}
    }}
  }},
  loader: {{ load: [{packages_long}] }},
  svg: {{ fontCache: 'global' }},
  startup: {{ typeset: true }}
}};"#,
        packages_short = package_short.join(", "),
        packages_long = package_long.join(", "),
        macros = macros,
    )
}

fn json_string(s: &str) -> String {
    // Conservative JSON string escape — enough for macro names/bodies, which
    // are dominated by backslashes and curly braces.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            c if (c as u32) < 0x20 => {
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Client-side script. Wires up:
///   * Event-delegated proof-toggle and proof-head click handlers (so they
///     keep working after `#page` content is swapped by the WebSocket update).
///   * A WebSocket connection to the same host that pushes `body-updated`
///     events with new `#page` HTML. After swapping, MathJax re-typesets.
///
/// When the page is loaded statically (CLI `render` output, no server), the
/// WebSocket fails silently and the page works as a static document.
const CLIENT_JS: &str = r#"
(function() {
  function precedingTheoremRole(proof) {
    var el = proof.previousElementSibling;
    while (el && !el.classList.contains('thm')) {
      el = el.previousElementSibling;
    }
    if (!el) return null;
    if (el.classList.contains('role-main')) return 'main';
    if (el.classList.contains('role-supporting')) return 'supporting';
    if (el.classList.contains('role-standard')) return 'standard';
    if (el.classList.contains('role-omitted')) return 'omitted';
    return null;
  }

  function applyMode(mode) {
    document.getElementById('page').setAttribute('data-proof-mode', mode);
    document.querySelectorAll('.proof').forEach(function(p) {
      var role = precedingTheoremRole(p);
      var folded;
      if (mode === 'all')        folded = false;
      else if (mode === 'main')  folded = (role !== 'main');
      else                       folded = (role !== 'main' && role !== 'supporting');
      if (role === null) folded = false;
      p.classList.toggle('folded', folded);
    });
  }

  async function restartServer() {
    var btn = document.getElementById('server-restart');
    if (btn) btn.disabled = true;
    setStatus('updating', '↻ restarting');
    try {
      var res = await fetch('/restart', { method: 'POST', cache: 'no-store' });
      if (!res.ok) throw new Error('restart failed');
    } catch (e) {
      if (btn) btn.disabled = false;
      setStatus('dead', '○ restart failed');
      return;
    }
    setTimeout(function() {
      var started = performance.now();
      function poll() {
        fetch('/?restart=' + Date.now(), { cache: 'no-store' })
          .then(function(res) {
            if (!res.ok) throw new Error('not ready');
            location.reload();
          })
          .catch(function() {
            if (performance.now() - started > 20000) {
              if (btn) btn.disabled = false;
              setStatus('dead', '○ restart timeout');
              return;
            }
            setTimeout(poll, 300);
          });
      }
      poll();
    }, 700);
  }

  // Event delegation survives `#page` innerHTML replacement.
  document.addEventListener('click', function(e) {
    var restart = e.target.closest('#server-restart');
    if (restart) {
      restartServer();
      return;
    }
    var btn = e.target.closest('.proof-toggle button');
    if (btn) {
      var mode = btn.getAttribute('data-mode');
      applyMode(mode);
      document.querySelectorAll('.proof-toggle button').forEach(function(x) {
        x.classList.toggle('active', x === btn);
      });
      return;
    }
    var head = e.target.closest('.proof-head');
    if (head) {
      head.closest('.proof').classList.toggle('folded');
    }
  });
  document.addEventListener('keydown', function(e) {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    var head = e.target.closest('.proof-head');
    if (head) {
      e.preventDefault();
      head.closest('.proof').classList.toggle('folded');
    }
  });

  // Apply a server-computed block-level patch. Each op references blocks
  // by id; unchanged blocks (and the typeset math inside them) are never
  // touched, so this is much cheaper than swapping the whole body.
  async function applyPatch(ops) {
    var tStart = performance.now();
    setStatus('updating', '↻ patching');
    var page = document.getElementById('page');
    var tpl = document.createElement('template');
    var needTypeset = [];
    var reusedMath = 0, totalMath = 0;
    var replacedBlocks = 0, appendedBlocks = 0, removedBlocks = 0;

    for (var i = 0; i < ops.length; i++) {
      var op = ops[i];
      if (op.type === 'replace') {
        var el = document.getElementById(op.id);
        if (!el) continue;

        // Reuse already-typeset math within the block when the math source
        // hash did not change. This keeps a prose edit near math from paying
        // MathJax's per-expression cost.
        var oldByHash = new Map();
        el.querySelectorAll('.math[data-hash]').forEach(function(oldEl) {
          var arr = oldByHash.get(oldEl.dataset.hash);
          if (!arr) { arr = []; oldByHash.set(oldEl.dataset.hash, arr); }
          arr.push(oldEl);
        });

        tpl.innerHTML = op.html;
        var frag = tpl.content;
        frag.querySelectorAll('.math[data-hash]').forEach(function(newEl) {
          totalMath++;
          var pool = oldByHash.get(newEl.dataset.hash);
          if (pool && pool.length > 0) {
            var oldEl = pool.shift();
            oldEl.id = newEl.id;
            if (newEl.dataset.src) oldEl.dataset.src = newEl.dataset.src;
            newEl.replaceWith(oldEl);
            reusedMath++;
          } else {
            needTypeset.push(newEl);
          }
        });
        el.replaceWith(frag);
        replacedBlocks++;
      } else if (op.type === 'append') {
        tpl.innerHTML = op.html;
        var frag2 = tpl.content;
        frag2.querySelectorAll('.math[data-hash]').forEach(function(m) {
          totalMath++;
          needTypeset.push(m);
        });
        page.appendChild(frag2);
        appendedBlocks++;
      } else if (op.type === 'remove') {
        var rm = document.getElementById(op.id);
        if (rm) rm.remove();
        removedBlocks++;
      }
    }

    // Re-bind the needTypeset references after they've been inserted into
    // the live page. The references stored above are still valid because
    // template fragment insertion moves the nodes, not copies them.
    var typesetMs = 0;
    if (needTypeset.length && window.MathJax && window.MathJax.typesetPromise) {
      if (window.MathJax.typesetClear) window.MathJax.typesetClear(needTypeset);
      var tT = performance.now();
      await window.MathJax.typesetPromise(needTypeset);
      typesetMs = Math.round(performance.now() - tT);
    }

    var total = Math.round(performance.now() - tStart);
    setStatus('live',
      '● ' + total + 'ms · ' + replacedBlocks + 'r' +
      (appendedBlocks ? '/+' + appendedBlocks : '') +
      (removedBlocks ? '/-' + removedBlocks : '') +
      ' / typeset ' + typesetMs +
      ' (' + needTypeset.length + ' math' +
      (reusedMath ? ', reused ' + reusedMath + '/' + totalMath : '') + ')' +
      memSuffix(window._lastRss));
  }

  // Shared memory tag. Server pushes its current resident size on every
  // event; we cache it so subsequent renders can re-print without waiting
  // for a fresh roundtrip.
  function memSuffix(mib) {
    if (typeof mib !== 'number' || isNaN(mib)) return '';
    return ' · ' + mib.toFixed(1) + ' MiB';
  }

  // Live-reload WebSocket. Reconnects with backoff if the server restarts.
  var status = document.getElementById('ws-status');
  function setStatus(cls, text) {
    if (!status) return;
    status.className = 'status ' + cls;
    status.textContent = text;
  }
  function connect() {
    if (!window.WebSocket) return;
    var url = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws';
    var ws;
    try { ws = new WebSocket(url); } catch (e) { return; }
    ws.onopen  = function() { setStatus('live', '● live'); };
    ws.onclose = function() {
      setStatus('dead', '○ disconnected');
      setTimeout(connect, 1000);
    };
    ws.onerror = function() { setStatus('dead', '○ error'); };
    ws.onmessage = async function(ev) {
      try {
        var msg = JSON.parse(ev.data);
        if (typeof msg.rss_mib === 'number') window._lastRss = msg.rss_mib;
        if (msg.event === 'patch') {
          await applyPatch(msg.ops);
        } else if (msg.event === 'body-updated') {
          var tStart = performance.now();
          setStatus('updating', '↻ updating');
          var page = document.getElementById('page');

          // Detach #page from the live document for the duration of the
          // mutations. Off-document mutations don't trigger layout/style
          // invalidation, so 300+ node transplants run an order of
          // magnitude faster than they would in-document.
          var pageParent = page.parentNode;
          var pageNextSibling = page.nextSibling;
          pageParent.removeChild(page);

          // Index existing math nodes by content hash.
          var oldByHash = new Map();
          var oldMath = page.querySelectorAll('.math[data-hash]');
          oldMath.forEach(function(el) {
            var arr = oldByHash.get(el.dataset.hash);
            if (!arr) { arr = []; oldByHash.set(el.dataset.hash, arr); }
            arr.push(el);
          });
          var tIndex = performance.now();

          // Parse new HTML into a detached <template> (faster than <div>).
          var tpl = document.createElement('template');
          tpl.innerHTML = msg.html;
          var buf = tpl.content;
          var tParse = performance.now();

          // For each new math element, transplant the already-typeset old
          // node when the hash matches. Both source and target are now
          // off-document, so each `replaceWith` is a cheap pointer swap.
          var needTypeset = [];
          var newMath = buf.querySelectorAll('.math[data-hash]');
          newMath.forEach(function(newEl) {
            var pool = oldByHash.get(newEl.dataset.hash);
            if (pool && pool.length > 0) {
              var oldEl = pool.shift();
              oldEl.id = newEl.id;
              if (newEl.dataset.src) oldEl.dataset.src = newEl.dataset.src;
              newEl.replaceWith(oldEl);
            } else {
              needTypeset.push(newEl);
            }
          });
          var tDiff = performance.now();

          page.replaceChildren(buf);

          // Reattach #page in its original position. One layout pass for
          // the whole update, not 300+.
          if (pageNextSibling) pageParent.insertBefore(page, pageNextSibling);
          else pageParent.appendChild(page);
          var tSwap = performance.now();

          var typesetMs = 0;
          if (needTypeset.length && window.MathJax && window.MathJax.typesetPromise) {
            if (window.MathJax.typesetClear) window.MathJax.typesetClear(needTypeset);
            var tTypesetStart = performance.now();
            await window.MathJax.typesetPromise(needTypeset);
            typesetMs = Math.round(performance.now() - tTypesetStart);
          }

          var tDone = performance.now();
          var total = Math.round(tDone - tStart);
          var reused = newMath.length - needTypeset.length;
          setStatus('live',
            '● ' + total + 'ms · idx ' + Math.round(tIndex - tStart) +
            ' / parse ' + Math.round(tParse - tIndex) +
            ' / diff ' + Math.round(tDiff - tParse) +
            ' / swap ' + Math.round(tSwap - tDiff) +
            ' / typeset ' + typesetMs +
            ' (reused ' + reused + '/' + newMath.length + ')' +
            memSuffix(window._lastRss));
        } else if (msg.event === 'full-reload') {
          location.reload();
        } else if (msg.event === 'error') {
          setStatus('dead', '○ ' + (msg.message || 'render error'));
        }
      } catch (e) { console.error('mathpreview WS:', e); }
    };
  }
  connect();
})();
"#;

const DEFAULT_CSS: &str = r#"
:root {
  --fg: #1c1c1c;
  --muted: #777;
  --bg: #fafafa;
  --paper: #ffffff;
  --accent: #5b3ea2;
  --supporting: #2b6cb0;
  --standard: #888;
  --omitted: #b25800;
  --border: #e0e0e0;
}
* { box-sizing: border-box; }
html, body {
  margin: 0;
  background: var(--bg);
  color: var(--fg);
  font: 16px/1.55 'Iowan Old Style', 'Palatino Linotype', Palatino, Georgia, serif;
}
.topbar {
  display: flex; align-items: center; gap: 12px;
  padding: 10px 18px;
  background: #fff;
  border-bottom: 1px solid var(--border);
  position: sticky; top: 0; z-index: 10;
  font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
  font-size: 13px;
}
.topbar-spacer { flex: 1; }
.server-restart,
.proof-toggle button {
  border: 1px solid var(--border); background: #fff; padding: 4px 10px;
  font: inherit; cursor: pointer;
}
.server-restart:disabled { opacity: 0.55; cursor: wait; }
.proof-toggle button.active { background: var(--accent); color: #fff; border-color: var(--accent); }
main#page {
  max-width: 760px; margin: 32px auto; padding: 40px 60px;
  background: var(--paper);
  border: 1px solid var(--border);
}
h1.sec-h0 { font-size: 2em; }
h2.sec-h1 { font-size: 1.7em; }
h3.sec-h2 { font-size: 1.4em; margin-top: 1.5em; }
h4.sec-h3 { font-size: 1.2em; }
h5.sec-h4 { font-size: 1.05em; }
.thm {
  margin: 1.2em 0;
  padding: 14px 18px;
  border: 1px solid var(--border);
  background: #fff;
  border-left: 3px solid var(--standard);
}
.thm.role-main       { border-left-color: var(--accent); }
.thm.role-supporting { border-left-color: var(--supporting); }
.thm.role-standard   { border-left-color: var(--standard); }
.thm.role-omitted    { border-left: 1px dashed var(--omitted); border-color: var(--omitted); }
.thm-head { font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif; font-weight: 600; }
.thm-kind { text-transform: uppercase; letter-spacing: 0.05em; font-size: 0.85em; color: var(--accent); }
.thm-num { font-variant-numeric: tabular-nums; color: var(--fg); }
.sec-num { color: var(--muted); font-variant-numeric: tabular-nums; margin-right: 0.4em; }

/* LaTeX-style display math spacing:
   \abovedisplayskip and \belowdisplayskip are ~12pt in standard classes,
   roughly 1em at body text size. Equation number sits on the right at the
   baseline of the math, not on its own line. */
.math.display {
  display: block;
  text-align: center;
  margin: 1em 0;
  position: relative;
  overflow-x: auto;
  overflow-y: hidden;
}
.math.display mjx-container[display="true"] {
  margin: 0 !important;
  display: inline-block !important;
}
.eq-num {
  position: absolute;
  right: 0;
  top: 50%;
  transform: translateY(-50%);
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
/* The first line of a paragraph after a display equation shouldn't indent
   (LaTeX's "this paragraph continues" rule). Without paragraph wrappers we
   approximate by tightening the display→text gap. */
.math.display + .blk,
.math.display + * {
  margin-top: 0.4em;
}
.thm.role-supporting .thm-kind { color: var(--supporting); }
.thm.role-standard .thm-kind   { color: var(--standard); }
.thm.role-omitted .thm-kind    { color: var(--omitted); }
.thm-name { color: var(--muted); font-weight: 400; }
.thm-body { margin-top: 6px; font-style: italic; }
.thm-omitref { margin-top: 8px; font-size: 0.9em; color: var(--omitted); }
.role-pill {
  float: right;
  font-size: 0.75em;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  padding: 2px 6px;
  border: 1px solid currentColor;
  border-radius: 3px;
  color: var(--muted);
  font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
  font-weight: 600;
}
.role-pill.role-main { color: var(--accent); }
.role-pill.role-supporting { color: var(--supporting); }
.role-pill.role-omitted { color: var(--omitted); }
.proof { margin: 1em 0; }
.proof-head {
  font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
  font-weight: 600; font-size: 0.95em; color: var(--muted);
  cursor: pointer; user-select: none;
  display: flex; align-items: center; gap: 0.4em;
}
.proof-head:focus { outline: 2px solid var(--accent); outline-offset: 2px; }
.proof-head:hover { color: var(--accent); }
.proof-of { color: var(--muted); font-weight: 400; }
.proof-body { margin-top: 4px; }
.qed { float: right; font-style: normal; }
.fold-marker { display: inline-block; width: 0.7em; transition: transform 0.1s ease; }
.fold-marker::before { content: "▾"; }
.proof.folded .fold-marker { transform: rotate(-90deg); }
.proof.folded .proof-body { display: none; }
.proof.folded .proof-head { color: var(--muted); }

.math.inline { white-space: nowrap; }
.math.display { margin: 1em 0; }
.para { margin: 0.85em 0; }
.para-break { display: block; height: 1.1em; }
.ref { color: var(--accent); text-decoration: none; border-bottom: 1px dotted var(--accent); }
.cite { color: var(--supporting); text-decoration: none; }
.cite:hover { text-decoration: underline; }
.cite.missing { color: #999; font-family: monospace; font-size: 0.9em; }
.title-block { text-align: center; margin: 1em 0 2.5em; }
.paper-title { font-size: 1.9em; font-weight: 700; margin: 0 0 0.4em; }
.paper-author { font-size: 1em; color: var(--fg); margin-bottom: 0.2em; }
.paper-date { font-size: 0.9em; color: var(--muted); }
.references { margin-top: 3em; padding-top: 1.5em; border-top: 1px solid var(--border); }
.references h2 { font-size: 1.4em; margin-bottom: 0.5em; }
.bib-list { display: grid; grid-template-columns: max-content 1fr; column-gap: 1em; row-gap: 0.45em; font-size: 0.93em; line-height: 1.45; padding: 0; margin: 0; }
.bib-list.bib-style-numeric    { grid-template-columns: 2.4em 1fr; }
.bib-list.bib-style-alphabetic { grid-template-columns: max-content 1fr; }
.bib-list.bib-style-authoryear { grid-template-columns: max-content 1fr; }
.bib-label { font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif; color: var(--muted); white-space: nowrap; }
.bib-entry { margin: 0; }
.bib-entry em { font-style: italic; }
.bib-doi, .bib-url { color: var(--supporting); font-size: 0.85em; word-break: break-all; }
.bib-missing { color: var(--omitted); font-style: italic; }
.blk { display: contents; }
.opaque-env { padding: 6px 10px; border-left: 2px solid var(--border); color: var(--muted); margin: 0.6em 0; }
.status { font-size: 11px; padding: 2px 6px; border-radius: 3px; color: var(--muted); }
.status.live { color: #1e7e1e; }
.status.dead { color: #b22222; }
.status.updating { color: var(--accent); }
.latex-list { margin: 0.6em 0; padding-left: 2em; }
.latex-list.itemize { list-style: disc; }
.latex-list.enumerate { list-style: decimal; }
.latex-list.description { display: grid; grid-template-columns: max-content 1fr; column-gap: 0.6em; padding-left: 0; }
.latex-list.description .item-marker { font-weight: 600; }
.latex-list .item-body { margin-bottom: 0.2em; }
details.warnings {
  max-width: 760px;
  margin: 16px auto 0;
  padding: 8px 16px;
  background: #fff5e6;
  border: 1px solid #f0c98a;
  font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
  font-size: 13px;
}
details.warnings summary { cursor: pointer; }
aside#margin { display: none; /* populated in Step 4 */ }
"#;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::HtmlOptions;

    #[test]
    fn inline_math_separated_by_blank_line_renders_as_paragraphs() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n$a^2$\n\n$b^2$\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert_eq!(out.body_html.matches(r#"<p class="para">"#).count(), 2);
        assert!(out.body_html.contains(r"\(a^2\)"));
        assert!(out.body_html.contains(r"\(b^2\)"));
    }
}
