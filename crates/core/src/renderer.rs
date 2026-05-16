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
    /// at the project's vendored copy at `mathjax/es5/tex-svg.js`; switch to
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
    #[serde(skip)]
    pub diff_hash: String,
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
        step_counter: 0,
        case_counter: 0,
    };

    // Top-level inline runs become paragraph blocks. Structural nodes
    // (sections, displays, theorem-likes, lists, etc.) stay as their own
    // blocks. The wrapper uses `display: contents` in CSS so it doesn't affect
    // visual layout — it exists purely so the diff/patch path can find and
    // replace blocks by id.
    let mut blocks: Vec<RenderedBlock> = Vec::with_capacity(nodes.len());
    let mut paragraph = String::new();
    let mut paragraph_start: Option<usize> = None;
    let mut paragraph_force_indent = false;
    let mut paragraph_no_indent = false;
    let mut paragraph_flow_marker = false;
    let mut paragraph_trim_after_flow_marker = false;
    let mut previous_block_was_display = false;
    let mut blank_after_display = false;
    let ordered = front_matter_order(nodes);
    for (i, node) in ordered.iter().enumerate() {
        if is_blank_separator_node(node) {
            flush_paragraph(
                &mut blocks,
                &mut paragraph,
                &mut paragraph_start,
                &mut paragraph_force_indent,
                &mut paragraph_no_indent,
                &mut paragraph_flow_marker,
                &mut paragraph_trim_after_flow_marker,
            );
            if previous_block_was_display {
                blank_after_display = true;
            }
            continue;
        }

        if let Some(name) = flow_command_name(node) {
            if is_flow_reset_command(name) {
                let mut sink = String::new();
                write_node(&mut sink, node, &mut ctx);
                continue;
            }
            flush_paragraph(
                &mut blocks,
                &mut paragraph,
                &mut paragraph_start,
                &mut paragraph_force_indent,
                &mut paragraph_no_indent,
                &mut paragraph_flow_marker,
                &mut paragraph_trim_after_flow_marker,
            );
            paragraph_start = Some(i);
            paragraph_force_indent = false;
            paragraph_no_indent = true;
            paragraph_flow_marker = true;
            paragraph_trim_after_flow_marker = true;
            write_node(&mut paragraph, node, &mut ctx);
            previous_block_was_display = false;
            blank_after_display = false;
            continue;
        }

        if is_top_level_inline_node(node) {
            if matches!(&node.kind, NodeKind::Comment(_)) {
                continue;
            }
            if let NodeKind::Text(s) = &node.kind {
                for part in paragraph_text_parts(s) {
                    match part {
                        ParagraphTextPart::Text(segment) => {
                            let text = if paragraph.trim().is_empty()
                                || paragraph_trim_after_flow_marker
                            {
                                trim_leading_paragraph_space(segment)
                            } else {
                                segment
                            };
                            if text.is_empty() {
                                continue;
                            }
                            if paragraph_start.is_none() {
                                paragraph_start = Some(i);
                                paragraph_force_indent =
                                    previous_block_was_display && blank_after_display;
                                paragraph_no_indent = false;
                                paragraph_flow_marker = false;
                            }
                            if !paragraph.trim().is_empty()
                                && !starts_with_blank_line(text)
                                && text.starts_with(char::is_whitespace)
                                && !paragraph.ends_with(char::is_whitespace)
                            {
                                paragraph.push(' ');
                            }
                            write_text(&mut paragraph, text, ctx.labels);
                            paragraph_trim_after_flow_marker = false;
                            previous_block_was_display = false;
                            blank_after_display = false;
                        }
                        ParagraphTextPart::Break => {
                            flush_paragraph(
                                &mut blocks,
                                &mut paragraph,
                                &mut paragraph_start,
                                &mut paragraph_force_indent,
                                &mut paragraph_no_indent,
                                &mut paragraph_flow_marker,
                                &mut paragraph_trim_after_flow_marker,
                            );
                            if previous_block_was_display {
                                blank_after_display = true;
                            }
                        }
                    }
                }
                continue;
            }
            if paragraph_start.is_none() {
                paragraph_start = Some(i);
                paragraph_force_indent = previous_block_was_display && blank_after_display;
                paragraph_no_indent = false;
                paragraph_flow_marker = false;
            }
            write_node(&mut paragraph, node, &mut ctx);
            paragraph_trim_after_flow_marker = false;
            previous_block_was_display = false;
            blank_after_display = false;
            continue;
        }

        flush_paragraph(
            &mut blocks,
            &mut paragraph,
            &mut paragraph_start,
            &mut paragraph_force_indent,
            &mut paragraph_no_indent,
            &mut paragraph_flow_marker,
            &mut paragraph_trim_after_flow_marker,
        );
        let mut inner = String::new();
        write_node(&mut inner, node, &mut ctx);
        // Skip empty emissions (e.g. discarded comments, no-op opaque cmds)
        // so we don't pollute the block sequence with phantoms whose hash
        // would still match across renders but waste id space.
        if inner.trim().is_empty() {
            continue;
        }
        push_block(&mut blocks, i, inner);
        previous_block_was_display = matches!(&node.kind, NodeKind::DisplayMath { .. });
        blank_after_display = false;
    }
    flush_paragraph(
        &mut blocks,
        &mut paragraph,
        &mut paragraph_start,
        &mut paragraph_force_indent,
        &mut paragraph_no_indent,
        &mut paragraph_flow_marker,
        &mut paragraph_trim_after_flow_marker,
    );

    let body: String = blocks.iter().map(|b| b.html.as_str()).collect();
    let full = wrap_in_shell(&body, preamble, opts);
    RenderedHtml { full, body, blocks }
}

fn front_matter_order(nodes: &[Node]) -> Vec<&Node> {
    let Some(title_index) = nodes
        .iter()
        .position(|node| matches!(node.kind, NodeKind::MakeTitle))
    else {
        return nodes.iter().collect();
    };
    let delayed_abstracts: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            (index < title_index && matches!(node.kind, NodeKind::Abstract)).then_some(index)
        })
        .collect();
    if delayed_abstracts.is_empty() {
        return nodes.iter().collect();
    }

    let mut ordered = Vec::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        if delayed_abstracts.contains(&index) {
            continue;
        }
        ordered.push(node);
        if index == title_index {
            ordered.extend(delayed_abstracts.iter().map(|i| &nodes[*i]));
        }
    }
    ordered
}

fn flush_paragraph(
    blocks: &mut Vec<RenderedBlock>,
    paragraph: &mut String,
    paragraph_start: &mut Option<usize>,
    paragraph_force_indent: &mut bool,
    paragraph_no_indent: &mut bool,
    paragraph_flow_marker: &mut bool,
    paragraph_trim_after_flow_marker: &mut bool,
) {
    let Some(start) = paragraph_start.take() else {
        return;
    };
    if paragraph.trim().is_empty() {
        paragraph.clear();
        *paragraph_force_indent = false;
        *paragraph_no_indent = false;
        *paragraph_flow_marker = false;
        *paragraph_trim_after_flow_marker = false;
        return;
    }
    let mut classes = vec!["para"];
    if *paragraph_force_indent && !*paragraph_no_indent {
        classes.push("para-indent");
    }
    if *paragraph_no_indent {
        classes.push("para-noindent");
    }
    if *paragraph_flow_marker {
        classes.push("para-flow");
    }
    let class = classes.join(" ");
    let inner = format!(r#"<p class="{class}">{}</p>"#, std::mem::take(paragraph));
    *paragraph_force_indent = false;
    *paragraph_no_indent = false;
    *paragraph_flow_marker = false;
    *paragraph_trim_after_flow_marker = false;
    push_block(blocks, start, inner);
}

fn push_block(blocks: &mut Vec<RenderedBlock>, index: usize, inner: String) {
    let id = format!("blk-{}", index + 1);
    let hash = fnv_hash(&inner);
    let diff_hash = fnv_hash(&stable_block_diff_source(&inner));
    let html = format!(
        r#"<article class="blk" id="{id}" data-blockhash="{hash}">{inner}</article>"#,
        id = id,
        hash = hash,
        inner = inner,
    );
    blocks.push(RenderedBlock {
        id,
        hash,
        diff_hash,
        html,
    });
}

fn stable_block_diff_source(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with(r#" data-src=""#) || starts_generated_id_attr(rest) {
            if let Some(end) = quoted_attr_end(s, i) {
                i = end;
                continue;
            }
        }
        let ch = rest.chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn starts_generated_id_attr(rest: &str) -> bool {
    const PREFIXES: [&str; 7] = [
        r#" id="im-"#,
        r#" id="dm-"#,
        r#" id="eq-"#,
        r#" id="sec-"#,
        r#" id="thm-"#,
        r#" id="proof-"#,
        r#" id="fn-"#,
    ];
    PREFIXES.iter().any(|prefix| {
        rest.starts_with(prefix)
            && rest
                .as_bytes()
                .get(prefix.len())
                .is_some_and(u8::is_ascii_digit)
    })
}

fn quoted_attr_end(s: &str, start: usize) -> Option<usize> {
    let first_quote = s[start..].find('"')?;
    let value_start = start + first_quote + 1;
    let second_quote = s[value_start..].find('"')?;
    Some(value_start + second_quote + 1)
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

fn flow_command_name(node: &Node) -> Option<&str> {
    match &node.kind {
        NodeKind::OpaqueCmd { name, .. }
            if matches!(
                name.as_str(),
                "step" | "case" | "restartsteps" | "restartcases"
            ) =>
        {
            Some(name.as_str())
        }
        _ => None,
    }
}

fn is_flow_reset_command(name: &str) -> bool {
    matches!(name, "restartsteps" | "restartcases")
}

fn is_flow_marker_command(name: &str) -> bool {
    matches!(name, "step" | "case")
}

fn is_blank_separator_node(node: &Node) -> bool {
    matches!(&node.kind, NodeKind::Text(s) if is_blank_line_separator(s))
}

fn trim_leading_paragraph_space(s: &str) -> &str {
    s.trim_start_matches(char::is_whitespace)
}

fn starts_with_blank_line(s: &str) -> bool {
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
            _ => return false,
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParagraphTextPart<'a> {
    Text(&'a str),
    Break,
}

fn paragraph_text_parts(s: &str) -> Vec<ParagraphTextPart<'_>> {
    let bytes = s.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            if let Some(end) = paragraph_break_end(bytes, i) {
                if start < i {
                    parts.push(ParagraphTextPart::Text(&s[start..i]));
                }
                parts.push(ParagraphTextPart::Break);
                i = end;
                start = end;
                continue;
            }
        }
        if bytes[i].is_ascii() {
            i += 1;
        } else {
            let ch = s[i..].chars().next().unwrap_or('\0');
            i += ch.len_utf8();
        }
    }
    if start < s.len() {
        parts.push(ParagraphTextPart::Text(&s[start..]));
    }
    parts
}

fn paragraph_break_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start;
    let mut newlines = 0u8;
    while j < bytes.len() {
        match bytes[j] {
            b'\n' => {
                newlines += 1;
                j += 1;
            }
            b' ' | b'\t' | b'\r' => j += 1,
            _ => break,
        }
    }
    (newlines >= 2).then_some(j)
}

fn write_text(out: &mut String, s: &str, labels: &LabelTable) {
    if is_blank_line_separator(s) {
        out.push_str(r#"<div class="para-break" aria-hidden="true"></div>"#);
    } else {
        out.push_str(&render_inline_latex(s, labels));
    }
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
<body class="page-mode-a4">
<header class="topbar">
  <strong>mathpreview</strong>
  <span class="status" id="ws-status" title="live-reload status"></span>
  <span class="topbar-spacer"></span>
  <button class="side-toggle" id="side-toggle" type="button" aria-controls="viewer-side" aria-expanded="false" title="toggle index and pages pane">toc</button>
  <span class="page-mode-toggle" data-page-mode="a4">
    <button data-page-mode="a4" class="active" type="button">A4</button>
    <button data-page-mode="dynamic" type="button">dynamic</button>
  </span>
  <button class="server-restart" id="server-restart" type="button" title="restart preview server">restart</button>
  <button class="server-stop" id="server-stop" type="button" title="stop preview server">stop</button>
  <span class="proof-toggle" data-mode="all">
    <button data-mode="main">main only</button>
    <button data-mode="supporting">+ supporting</button>
    <button data-mode="all" class="active">all</button>
  </span>
</header>
{warnings_html}
<aside class="side-panel" id="viewer-side" aria-label="document navigation">
  <div class="side-tabs" role="tablist" aria-label="navigation mode">
    <button class="side-tab active" type="button" data-side-tab="index" role="tab" aria-selected="true">Index</button>
    <button class="side-tab" type="button" data-side-tab="pages" role="tab" aria-selected="false">Pages</button>
  </div>
  <nav class="side-list" id="side-index" aria-label="document index"></nav>
  <nav class="side-list" id="side-pages" aria-label="A4 pages" hidden></nav>
</aside>
<div id="page-shell">
  <main id="page" data-proof-mode="all">
{body}
  </main>
</div>
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
    step_counter: usize,
    case_counter: usize,
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
            write_children(out, &n.children, ctx);
        }
        NodeKind::Text(s) => {
            // Route through the inline parser so accents (`\'e` → é) and any
            // straggler inline commands get processed instead of leaking as
            // literal backslash sequences.
            write_text(out, s, ctx.labels);
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
            write_children(out, &n.children, ctx);
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
        NodeKind::Proof { of, role } => {
            let id = ctx.idgen.next("proof");
            record(ctx, &id, &n.span, None);
            let head = match of {
                Some(o) => proof_head_html(o, ctx.labels),
                None => r#"<div class="proof-head" role="button" tabindex="0"><span class="fold-marker"></span>Proof.</div>"#.to_string(),
            };
            let role_class = role
                .map(|r| format!(" {}", r.as_css_class()))
                .unwrap_or_default();
            let role_attr = role
                .map(|r| format!(r#" data-role="{}""#, role_label(r)))
                .unwrap_or_default();
            writeln!(
                out,
                r#"<div class="proof{role_class}" id="{id}"{role_attr} data-src="{src}">{head}<div class="proof-body">"#,
                role_class = role_class,
                id = escape_attr(&id),
                role_attr = role_attr,
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            out.push_str(r#"<span class="qed">∎</span></div></div>"#);
            out.push('\n');
        }
        NodeKind::InlineMath(s) => {
            let id = ctx.idgen.next("im");
            record(ctx, &id, &n.span, None);
            let hash = fnv_hash(&format!("i:{s}"));
            let copy_tex = format!(r"\({s}\)");
            // Use \( \) so MathJax doesn't typeset the literal `$` text.
            write!(
                out,
                r#"<span class="math inline" id="{id}" data-src="{src}" data-hash="{hash}" data-tex="{copy_tex}" tabindex="0" title="Copy as LaTeX">\({}\)</span>"#,
                escape_math(s),
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                hash = hash,
                copy_tex = escape_attr(&copy_tex),
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
            let copy_tex = match env {
                Some(e) => format!(r"\begin{{{e}}}{}\end{{{e}}}", body),
                None => format!(r"\[{}\]", body),
            };
            let alias_html = label_alias_anchors(body, label.as_deref());
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
                r#"<div class="math display" id="{id}" data-src="{src}" data-hash="{hash}" data-tex="{copy_tex}" tabindex="0" title="Copy as LaTeX">{aliases}{math}{num_html}</div>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                aliases = alias_html,
                math = escape_math(&math),
                hash = hash,
                copy_tex = escape_attr(&copy_tex),
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
                BibStyle::Numeric | BibStyle::NumericSorted => "bib-style-numeric",
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
                    Some(entry) => format_bib_entry(entry, ctx.bib_style, ctx.labels),
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
            write_children(out, &n.children, ctx);
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
                write_children(out, &n.children, ctx);
                writeln!(out, "</dd>").unwrap();
            } else {
                write!(out, "<li class=\"item-body\">").unwrap();
                write_children(out, &n.children, ctx);
                writeln!(out, "</li>").unwrap();
            }
        }
        NodeKind::MakeTitle => {
            let title = ctx.preamble.title.as_deref();
            let author_details = &ctx.preamble.author_details;
            let authors = &ctx.preamble.authors;
            let fallback_author = ctx.preamble.author.as_deref();
            let date = ctx.preamble.date.as_deref();
            if title.is_none() && authors.is_empty() && fallback_author.is_none() && date.is_none()
            {
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
            if !author_details.is_empty() {
                out.push_str(r#"<div class="paper-authors">"#);
                for author in author_details {
                    out.push_str(r#"<div class="paper-author">"#);
                    writeln!(
                        out,
                        r#"<div class="paper-author-name">{}</div>"#,
                        render_inline_latex(&author.name, ctx.labels)
                    )
                    .unwrap();
                    for address in &author.addresses {
                        writeln!(
                            out,
                            r#"<div class="paper-address">{}</div>"#,
                            render_inline_latex(address, ctx.labels)
                        )
                        .unwrap();
                    }
                    for email in &author.emails {
                        let display = escape_html(email);
                        let href = escape_attr(email);
                        writeln!(
                            out,
                            r#"<div class="paper-email"><a href="mailto:{href}">{display}</a></div>"#
                        )
                        .unwrap();
                    }
                    out.push_str("</div>");
                }
                out.push_str("</div>");
            } else if !authors.is_empty() {
                out.push_str(r#"<div class="paper-authors">"#);
                for a in authors {
                    writeln!(
                        out,
                        r#"<div class="paper-author">{}</div>"#,
                        render_inline_latex(a, ctx.labels)
                    )
                    .unwrap();
                }
                out.push_str("</div>");
            } else if let Some(a) = fallback_author {
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
        NodeKind::Abstract => {
            writeln!(
                out,
                r#"<section class="paper-abstract" data-src="{src}"><h2>Abstract</h2><div class="paper-abstract-body">"#,
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            writeln!(out, "</div></section>").unwrap();
        }
        NodeKind::OpaqueEnv { env, body } => {
            match env.as_str() {
                "figure" | "table" => write_float_placeholder(out, env, body, ctx.labels),
                _ => {
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
            }
        }
        NodeKind::OpaqueCmd { name, raw } => {
            match name.as_str() {
                "today" => out.push_str("(today)"),
                "LaTeX" => out.push_str("LaTeX"),
                "TeX" => out.push_str("TeX"),
                "step" => {
                    ctx.step_counter += 1;
                    write_flow_marker(
                        out,
                        "proof-step",
                        "Step",
                        &ctx.step_counter.to_string(),
                        raw,
                        ctx.labels,
                    );
                }
                "case" => {
                    ctx.case_counter += 1;
                    write_flow_marker(
                        out,
                        "proof-case",
                        "Case",
                        &roman_upper(ctx.case_counter),
                        raw,
                        ctx.labels,
                    );
                }
                "ldots" | "dots" => out.push('…'),
                "label" => {
                    if let Some(label) = latex_command_arg(raw, "label") {
                        write!(
                            out,
                            r#"<span class="label-anchor" id="{}"></span>"#,
                            escape_attr(&sanitize_id(&label))
                        )
                        .unwrap();
                    }
                }
                "vspace" | "hspace" | "smallskip" | "medskip" | "bigskip" | "newpage"
                | "clearpage" | "noindent" | "indent" | "linebreak" | "pagebreak" | "thanks"
                | "restartsteps" => {
                    ctx.step_counter = latex_optional_usize(raw).unwrap_or(0);
                }
                "restartcases" => {
                    ctx.case_counter = 0;
                }
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

fn write_children(out: &mut String, children: &[Node], ctx: &mut RenderCtx) {
    let mut trim_next_text = true;
    let mut previous_was_display = false;
    let mut pending_paragraph_indent = false;
    let mut seen_content = false;
    for child in children {
        if matches!(&child.kind, NodeKind::Comment(_)) {
            continue;
        }
        if let Some(name) = flow_command_name(child) {
            if is_flow_reset_command(name) {
                let mut sink = String::new();
                write_node(&mut sink, child, ctx);
                continue;
            }
            if is_flow_marker_command(name) {
                if seen_content || previous_was_display {
                    out.push_str(r#"<span class="flow-marker-break"></span>"#);
                }
                write_node(out, child, ctx);
                previous_was_display = false;
                trim_next_text = true;
                pending_paragraph_indent = false;
                seen_content = true;
                continue;
            }
        }
        if let NodeKind::Text(s) = &child.kind {
            for part in paragraph_text_parts(s) {
                match part {
                    ParagraphTextPart::Text(segment) => {
                        let starts_blank_line = starts_with_blank_line(segment);
                        let text = if trim_next_text {
                            trim_leading_paragraph_space(segment)
                        } else {
                            segment
                        };
                        if text.is_empty() {
                            if previous_was_display && starts_blank_line {
                                pending_paragraph_indent = true;
                            }
                            continue;
                        }
                        if pending_paragraph_indent || previous_was_display && starts_blank_line {
                            out.push_str(
                                r#"<span class="para-indent-marker" aria-hidden="true"></span>"#,
                            );
                        }
                        if !trim_next_text
                            && !starts_blank_line
                            && text.starts_with(char::is_whitespace)
                            && !out.ends_with(char::is_whitespace)
                        {
                            out.push(' ');
                        }
                        write_text(out, text, ctx.labels);
                        trim_next_text = false;
                        previous_was_display = false;
                        pending_paragraph_indent = false;
                        seen_content = true;
                    }
                    ParagraphTextPart::Break => {
                        if seen_content || previous_was_display {
                            pending_paragraph_indent = true;
                        }
                        trim_next_text = true;
                    }
                }
            }
            continue;
        }

        if pending_paragraph_indent && is_inline_like_node(child) {
            out.push_str(r#"<span class="para-indent-marker" aria-hidden="true"></span>"#);
        }
        write_node(out, child, ctx);
        previous_was_display = matches!(&child.kind, NodeKind::DisplayMath { .. });
        trim_next_text = previous_was_display;
        pending_paragraph_indent = false;
        seen_content = true;
    }
}

fn is_inline_like_node(node: &Node) -> bool {
    matches!(
        &node.kind,
        NodeKind::InlineMath(_)
            | NodeKind::Ref { .. }
            | NodeKind::Cite { .. }
            | NodeKind::OpaqueCmd { .. }
    )
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

fn write_float_placeholder(out: &mut String, env: &str, body: &str, labels: &LabelTable) {
    let kind = if env.trim_end_matches('*') == "table" {
        "Table"
    } else {
        "Figure"
    };
    let float_labels = latex_command_args(body, "label");
    let primary_label = float_labels.first().map(String::as_str);
    let id_attr = primary_label
        .map(|label| format!(r#" id="{}""#, escape_attr(&sanitize_id(label))))
        .unwrap_or_default();
    let alias_html = label_alias_anchors(body, primary_label);
    let kind_label = primary_label
        .and_then(|label| labels.number.get(label))
        .map(|number| format!("{kind} {}.", escape_html(number)))
        .unwrap_or_else(|| format!("{kind}."));
    let caption = latex_command_arg(body, "caption");
    let asset = latex_command_call(body, "includegraphics");
    let caption_html = caption
        .as_deref()
        .map(|c| render_latex_text_with_math(c.trim(), labels))
        .unwrap_or_else(|| "content omitted from preview".to_string());
    let asset_html = asset
        .as_ref()
        .map(|call| render_float_asset(&call.arg, call.optional.as_deref()))
        .unwrap_or_default();
    writeln!(
        out,
        r#"<figure class="float-placeholder float-{env}"{id} data-env="{env}">{aliases}{asset}<figcaption><span class="float-kind">{kind_label}</span> {caption}</figcaption></figure>"#,
        env = escape_attr(env),
        id = id_attr,
        aliases = alias_html,
        kind_label = kind_label,
        asset = asset_html,
        caption = caption_html,
    )
    .unwrap();
}

fn render_float_asset(asset: &str, options: Option<&str>) -> String {
    let asset = asset.trim();
    if asset.is_empty() {
        return String::new();
    }
    let url = asset_url(asset);
    let attrs = includegraphics_attrs(options);
    let ext = std::path::Path::new(asset)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => format!(
            r#"<div class="float-asset"><img class="float-image" src="{url}" alt="{alt}"{attrs}></div>"#,
            url = escape_attr(&url),
            alt = escape_attr(asset),
            attrs = attrs,
        ),
        "pdf" => format!(
            r#"<div class="float-asset"><a href="{url}" title="Open original PDF"><img class="float-image float-pdf-preview" src="{url}?preview=png" alt="{alt}"{attrs}></a></div>"#,
            url = escape_attr(&url),
            alt = escape_attr(asset),
            attrs = attrs,
        ),
        _ => format!(
            r#"<div class="float-asset"><a href="{url}">{label}</a></div>"#,
            url = escape_attr(&url),
            label = escape_html(asset),
        ),
    }
}

fn includegraphics_attrs(options: Option<&str>) -> String {
    let Some(options) = options.map(str::trim).filter(|s| !s.is_empty()) else {
        return String::new();
    };
    let parsed = parse_graphics_options(options);
    let width = parsed
        .get("width")
        .and_then(|value| latex_dimension_to_css(value));
    let height = parsed
        .get("height")
        .and_then(|value| latex_dimension_to_css(value));
    let scale = parsed
        .get("scale")
        .and_then(|value| parse_latex_number(value));
    let keep_aspect = parsed.contains_key("keepaspectratio");
    let mut styles = Vec::<String>::new();

    match (width.as_deref(), height.as_deref(), keep_aspect) {
        (Some(w), Some(h), true) => {
            styles.push(format!("max-width: {w}"));
            styles.push(format!("max-height: {h}"));
            styles.push("width: auto".to_string());
            styles.push("height: auto".to_string());
            styles.push("object-fit: contain".to_string());
        }
        (Some(w), Some(h), false) => {
            styles.push(format!("width: {w}"));
            styles.push(format!("height: {h}"));
            styles.push("max-width: none".to_string());
        }
        (Some(w), None, _) => {
            styles.push(format!("width: {w}"));
            styles.push("max-width: none".to_string());
            styles.push("height: auto".to_string());
        }
        (None, Some(h), _) => {
            styles.push(format!("height: {h}"));
            styles.push("width: auto".to_string());
        }
        (None, None, _) => {
            if let Some(scale) = scale {
                styles.push(format!("width: {}%", css_number(scale * 100.0)));
                styles.push("max-width: none".to_string());
                styles.push("height: auto".to_string());
            }
        }
    }

    let mut attrs = format!(r#" data-tex-options="{}""#, escape_attr(options));
    if !styles.is_empty() {
        attrs.push_str(&format!(r#" style="{}""#, escape_attr(&styles.join("; "))));
    }
    attrs
}

fn parse_graphics_options(options: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for part in split_top_level_commas(options) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((key, value)) = part.split_once('=') {
            out.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        } else {
            out.insert(part.to_ascii_lowercase(), String::new());
        }
    }
    out
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut brace_depth = 0i32;
    let mut bracket_depth = 0i32;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => brace_depth += 1,
            '}' => brace_depth -= 1,
            '[' => bracket_depth += 1,
            ']' => bracket_depth -= 1,
            ',' if brace_depth == 0 && bracket_depth == 0 => {
                parts.push(&s[start..i]);
                start = i + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn latex_dimension_to_css(raw: &str) -> Option<String> {
    let compact = strip_wrapping_braces(raw.trim())
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    if compact.is_empty() {
        return None;
    }

    for unit in [r"\textwidth", r"\linewidth", r"\columnwidth", r"\hsize"] {
        if let Some(prefix) = compact.strip_suffix(unit) {
            let factor = if prefix.is_empty() {
                1.0
            } else {
                parse_latex_number(prefix.trim_end_matches('*'))?
            };
            return Some(format!("{}%", css_number(factor * 100.0)));
        }
    }

    let (number, unit) = parse_number_prefix(&compact)?;
    match unit {
        "in" | "cm" | "mm" | "pt" | "pc" | "em" | "ex" | "px" => {
            Some(format!("{}{}", css_number(number), unit))
        }
        "bp" => Some(format!("{}pt", css_number(number * 72.0 / 72.27))),
        _ => None,
    }
}

fn strip_wrapping_braces(s: &str) -> &str {
    if s.starts_with('{') && s.ends_with('}') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn parse_number_prefix(s: &str) -> Option<(f64, &str)> {
    let bytes = s.as_bytes();
    let mut end = 0usize;
    let mut saw_digit = false;
    if matches!(bytes.first(), Some(b'+') | Some(b'-')) {
        end += 1;
    }
    while end < bytes.len() {
        match bytes[end] {
            b'0'..=b'9' => {
                saw_digit = true;
                end += 1;
            }
            b'.' => end += 1,
            _ => break,
        }
    }
    if !saw_digit {
        return None;
    }
    let number = s[..end].parse().ok()?;
    Some((number, &s[end..]))
}

fn parse_latex_number(s: &str) -> Option<f64> {
    let compact = strip_wrapping_braces(s.trim())
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    compact.parse().ok()
}

fn css_number(value: f64) -> String {
    let mut s = format!("{value:.4}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

fn asset_url(path: &str) -> String {
    let mut out = String::from("/assets/");
    for b in path.trim().as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b'~' => {
                out.push(*b as char)
            }
            b => write!(out, "%{b:02X}").unwrap(),
        }
    }
    out
}

fn write_flow_marker(
    out: &mut String,
    class: &str,
    label: &str,
    number: &str,
    raw: &str,
    labels: &LabelTable,
) {
    let label_text = format!("{label} {number}");
    if let Some(title) = latex_optional_arg(raw).filter(|s| !is_relax_option(s)) {
        write!(
            out,
            r#"<span class="{class} flow-marker"><strong>{label_text}:</strong> {title}.</span> "#,
            class = escape_attr(class),
            label_text = escape_html(&label_text),
            title = render_latex_text_with_math(title.trim(), labels),
        )
        .unwrap();
    } else {
        write!(
            out,
            r#"<span class="{class} flow-marker"><strong>{label_text}:</strong></span> "#,
            class = escape_attr(class),
            label_text = escape_html(&label_text),
        )
        .unwrap();
    }
}

fn is_relax_option(s: &str) -> bool {
    let trimmed = s.trim();
    trimmed.is_empty() || trimmed == r"\relax"
}

fn latex_optional_usize(raw: &str) -> Option<usize> {
    latex_optional_arg(raw)?.trim().parse().ok()
}

fn roman_upper(mut n: usize) -> String {
    if n == 0 {
        return "0".into();
    }
    let values = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, symbol) in values {
        while n >= value {
            out.push_str(symbol);
            n -= value;
        }
    }
    out
}

fn render_latex_text_with_math(s: &str, labels: &LabelTable) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut text_start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += if bytes[i].is_ascii() {
                1
            } else {
                s[i..].chars().next().unwrap_or('\0').len_utf8()
            };
            continue;
        }
        if i > text_start {
            out.push_str(&render_inline_latex(&s[text_start..i], labels));
        }
        i += 1;
        let math_start = i;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if bytes[i] == b'$' {
                break;
            }
            i += if bytes[i].is_ascii() {
                1
            } else {
                s[i..].chars().next().unwrap_or('\0').len_utf8()
            };
        }
        if i >= bytes.len() {
            out.push('$');
            out.push_str(&render_inline_latex(&s[math_start..], labels));
            text_start = s.len();
            break;
        }
        let body = &s[math_start..i];
        let copy_tex = format!(r"\({body}\)");
        write!(
            out,
            r#"<span class="math inline" data-hash="{hash}" data-tex="{copy_tex}" tabindex="0" title="Copy as LaTeX">\({math}\)</span>"#,
            hash = fnv_hash(&format!("i:{body}")),
            copy_tex = escape_attr(&copy_tex),
            math = escape_math(body),
        )
        .unwrap();
        i += 1;
        text_start = i;
    }

    if text_start < s.len() {
        out.push_str(&render_inline_latex(&s[text_start..], labels));
    }
    out
}

fn latex_optional_arg(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b'[' {
        i += 1;
    }
    parse_balanced_bracket(raw, i, b'[', b']')
}

fn latex_command_arg(src: &str, command: &str) -> Option<String> {
    latex_command_args(src, command).into_iter().next()
}

struct LatexCommandCall {
    optional: Option<String>,
    arg: String,
}

fn latex_command_call(src: &str, command: &str) -> Option<LatexCommandCall> {
    let needle = format!("\\{command}");
    let bytes = src.as_bytes();
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if src[i..].starts_with(&needle) {
            let after = i + needle.len();
            if bytes
                .get(after)
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'*')
            {
                i += 1;
                continue;
            }

            let mut j = after;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }

            let optional = if j < bytes.len() && bytes[j] == b'[' {
                let end = balanced_group_end(src, j, b'[', b']')?;
                let optional = parse_balanced_bracket(src, j, b'[', b']');
                j = end;
                optional
            } else {
                None
            };

            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'{' {
                return parse_balanced_bracket(src, j, b'{', b'}')
                    .map(|arg| LatexCommandCall { optional, arg });
            }
        }
        i += 1;
    }
    None
}

fn label_alias_anchors(body: &str, primary: Option<&str>) -> String {
    let mut seen = Vec::<String>::new();
    let mut out = String::new();
    for label in latex_command_args(body, "label") {
        if primary == Some(label.as_str()) || seen.iter().any(|s| s == &label) {
            continue;
        }
        seen.push(label.clone());
        write!(
            out,
            r#"<span class="label-anchor" id="{}"></span>"#,
            escape_attr(&sanitize_id(&label))
        )
        .unwrap();
    }
    out
}

fn latex_command_args(src: &str, command: &str) -> Vec<String> {
    let needle = format!("\\{command}");
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if src[i..].starts_with(&needle) {
            let after = i + needle.len();
            if bytes
                .get(after)
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'*')
            {
                i += 1;
                continue;
            }
            let mut j = after;
            loop {
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'[' {
                    let Some(end) = balanced_group_end(src, j, b'[', b']') else {
                        return out;
                    };
                    j = end;
                    continue;
                }
                if j < bytes.len() && bytes[j] == b'{' {
                    if let Some(arg) = parse_balanced_bracket(src, j, b'{', b'}') {
                        out.push(arg);
                    }
                    break;
                }
                break;
            }
        }
        i += 1;
    }
    out
}

fn parse_balanced_bracket(src: &str, start: usize, open: u8, close: u8) -> Option<String> {
    let end = balanced_group_end(src, start, open, close)?;
    Some(src[start + 1..end - 1].to_string())
}

fn balanced_group_end(src: &str, start: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(start).copied() != Some(open) {
        return None;
    }
    let mut depth = 1i32;
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                i += 2;
                continue;
            }
            b if b == open => depth += 1,
            b if b == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn format_bib_entry(e: &BibEntry, style: BibStyle, labels: &LabelTable) -> String {
    let author = match e.fields.get("author") {
        Some(a) => Some(format_authors(a, labels)),
        None => e.fields.get("editor").map(|ed| {
            let formatted = format_authors(ed, labels);
            let count = ed
                .split(" and ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .count();
            if count == 1 {
                format!("{formatted}, editor")
            } else {
                format!("{formatted}, editors")
            }
        }),
    };
    let year = e.fields.get("year").map(|s| bib_text_html(s, labels));
    let title = e.fields.get("title").map(|s| bib_text_html(s, labels));
    let eprint = e.fields.get("eprint").map(|s| bib_text_html(s, labels));
    let raw_publisher = e.fields.get("publisher");
    let arxiv_eprint =
        eprint.is_some() && raw_publisher.is_some_and(|s| s.trim().eq_ignore_ascii_case("arxiv"));
    let venue = e
        .fields
        .get("journal")
        .or_else(|| e.fields.get("booktitle"))
        .or_else(|| (!arxiv_eprint).then_some(raw_publisher).flatten());
    let edition = e.fields.get("edition").map(|s| bib_text_html(s, labels));
    let series = e.fields.get("series").map(|s| bib_text_html(s, labels));
    let volume = e.fields.get("volume").map(|s| bib_text_html(s, labels));
    let number = e
        .fields
        .get("number")
        .filter(|_| !arxiv_eprint)
        .map(|s| bib_text_html(s, labels));
    let pages = e.fields.get("pages").map(|s| bib_text_html(s, labels));
    let address = e.fields.get("address").map(|s| bib_text_html(s, labels));
    let publisher = raw_publisher
        .filter(|_| !arxiv_eprint)
        .map(|s| bib_text_html(s, labels));
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
        BibStyle::Numeric | BibStyle::NumericSorted | BibStyle::Alphabetic => {
            if let Some(a) = &author {
                parts.push(a.clone());
            }
            if let Some(t) = &title {
                let title_html = if matches!(e.entry_type.as_str(), "book" | "booklet" | "manual") {
                    format!("<em>{}</em>", t)
                } else {
                    t.clone()
                };
                parts.push(title_html);
            }
        }
    }
    let mut venue_str = String::new();
    if let Some(v) = venue {
        let venue_html = bib_text_html(v, labels);
        if e.entry_type == "article" {
            venue_str.push_str(&format!("<em>{venue_html}</em>"));
        } else if e.entry_type == "inproceedings" || e.entry_type == "incollection" {
            venue_str.push_str("In ");
            venue_str.push_str(&venue_html);
        } else {
            venue_str.push_str(&venue_html);
        }
    }
    if let Some(s) = &series {
        if !venue_str.is_empty() {
            venue_str.push_str(", ");
        }
        venue_str.push_str(s);
    }
    if let Some(v) = &volume {
        if e.entry_type == "article" {
            if !venue_str.is_empty() {
                venue_str.push_str(", ");
            }
            venue_str.push_str(v);
        } else if !venue_str.is_empty() {
            venue_str.push(' ');
            venue_str.push_str(&format!("vol. {v}"));
        } else {
            venue_str.push_str(&format!("vol. {v}"));
        }
        if let Some(n) = &number {
            venue_str.push_str(&format!("({n})"));
        }
    } else if let Some(n) = &number {
        if !venue_str.is_empty() {
            venue_str.push_str(", ");
        }
        venue_str.push_str(&format!("no. {n}"));
    }
    if let Some(p) = &pages {
        if !venue_str.is_empty() {
            if volume.is_some() {
                venue_str.push(':');
            } else {
                venue_str.push_str(", pp. ");
            }
        }
        venue_str.push_str(p);
    }
    if let Some(publi) = &publisher {
        if !venue_str.is_empty() && !venue_str.contains(publi) {
            venue_str.push_str(", ");
        }
        if !venue_str.contains(publi) {
            venue_str.push_str(publi);
        }
    }
    if let Some(a) = &address {
        if !venue_str.is_empty() {
            venue_str.push_str(", ");
        }
        venue_str.push_str(a);
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
    if let Some(ep) = &eprint {
        let prefix = e
            .fields
            .get("archiveprefix")
            .map(String::as_str)
            .unwrap_or("arXiv");
        parts.push(format!("{}:{}", bib_text_html(prefix, labels), ep));
    }
    if !matches!(style, BibStyle::AuthorYear) {
        if let Some(y) = &year {
            if let Some(last) = parts.last_mut() {
                last.push_str(", ");
                last.push_str(y);
            } else {
                parts.push(y.clone());
            }
        }
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

fn format_authors(a: &str, labels: &LabelTable) -> String {
    let authors: Vec<String> = a
        .split(" and ")
        .map(|s| format_author_name(s.trim(), labels))
        .filter(|s| !s.is_empty())
        .collect();
    match authors.len() {
        0 => String::new(),
        1 => authors[0].clone(),
        2 => format!("{} and {}", authors[0], authors[1]),
        _ => {
            let mut out = authors[..authors.len() - 1].join(", ");
            out.push_str(", and ");
            out.push_str(authors.last().unwrap());
            out
        }
    }
}

fn format_author_name(author: &str, labels: &LabelTable) -> String {
    let normalized = normalize_bib_whitespace(author);
    let parts: Vec<_> = normalized.split(',').map(str::trim).collect();
    let name = match parts.as_slice() {
        [last, first] if !first.is_empty() => format!("{first} {last}"),
        [last, jr, first] if !first.is_empty() => format!("{first} {last}, {jr}"),
        _ => normalized,
    };
    bib_text_html(&name, labels)
}

fn bib_text_html(s: &str, labels: &LabelTable) -> String {
    let normalized = normalize_bib_latex(s);
    render_inline_latex(&normalized, labels)
}

fn normalize_bib_latex(s: &str) -> String {
    let collapsed = normalize_bib_whitespace(s);
    let mut out = strip_bib_protective_braces(&collapsed);
    out = out.replace(r#"\textbackslash""#, r#"\""#);
    out = out.replace(r"\&", "&");
    out = out.replace(r"\_", "_");
    out = out.replace("---", "—");
    out = out.replace("--", "–");
    out = normalize_bib_whitespace(&out);
    out = out.replace(" ,", ",");
    out = out.replace(" .", ".");
    out
}

fn normalize_bib_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_bib_protective_braces(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut keep_stack = Vec::<bool>::new();
    let mut keep_next_group = false;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            out.push('\\');
            i += 1;
            if i < bytes.len() {
                if bytes[i].is_ascii_alphabetic() {
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                        out.push(bytes[i] as char);
                        i += 1;
                    }
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
                keep_next_group = true;
            }
            continue;
        }

        match bytes[i] {
            b'{' => {
                let keep = keep_next_group;
                keep_stack.push(keep);
                if keep {
                    out.push('{');
                }
                keep_next_group = false;
                i += 1;
            }
            b'}' => {
                if keep_stack.pop().unwrap_or(false) {
                    out.push('}');
                }
                keep_next_group = false;
                i += 1;
            }
            b if b.is_ascii() => {
                out.push(b as char);
                keep_next_group = false;
                i += 1;
            }
            _ => {
                let ch = s[i..].chars().next().unwrap_or('\0');
                out.push(ch);
                keep_next_group = false;
                i += ch.len_utf8();
            }
        }
    }
    out
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

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Main => "main",
        Role::Supporting => "supporting",
        Role::Standard => "standard",
        Role::Omitted => "omitted",
    }
}

fn role_pill_html(role: Role) -> String {
    let label = role_label(role);
    let cls = match role {
        Role::Main => "role-pill role-main",
        Role::Supporting => "role-pill role-supporting",
        Role::Standard => "role-pill role-standard",
        Role::Omitted => "role-pill role-omitted",
    };
    format!(r#"<span class="{cls}">{label}</span>"#)
}

fn proof_head_html(title: &str, labels: &LabelTable) -> String {
    let trimmed = title.trim();
    let rendered = render_inline_latex(trimmed, labels);
    let lower = trimmed.to_ascii_lowercase();
    let text = if lower.starts_with("of ") {
        format!(r#"Proof <span class="proof-of">{rendered}</span>"#)
    } else {
        format!(r#"<span class="proof-of">{rendered}</span>"#)
    };
    format!(
        r#"<div class="proof-head" role="button" tabindex="0"><span class="fold-marker"></span>{text}.</div>"#
    )
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
  var currentProofMode = 'all';
  var currentSideTab = 'index';
  var currentPageMode = 'a4';
  var currentSideOpen = false;
  var navRefreshTimer = 0;
  var activePageTimer = 0;
  var pageGuideLayoutHeightPx = 0;
  var pageGuideVisualHeightPx = 0;
  var pageGuideCount = 1;
  var currentPageScale = 1;
  var NAV_IDLE_MS = 220;
  var NAV_RENDER_IDLE_MS = 900;
  var NAV_RESIZE_IDLE_MS = 120;
  var A4_CSS_WIDTH = 794;
  var A4_RATIO = 297 / 210;
  var navNeedsIndex = true;
  var navNeedsPages = true;
  var lastHeadingSignature = '';
  var lastPageGuideSignature = '';
  var selectedMath = null;

  function pageEl() {
    return document.getElementById('page');
  }

  function pageShellEl() {
    return document.getElementById('page-shell');
  }

  function cleanNavText(text) {
    return (text || '').replace(/\s+/g, ' ').trim();
  }

  function headingLevel(heading) {
    for (var i = 0; i < heading.classList.length; i++) {
      var m = /^sec-h(\d+)$/.exec(heading.classList[i]);
      if (m) return parseInt(m[1], 10);
    }
    return 2;
  }

  function headingSelector() {
    return '.sec-h0, .sec-h1, .sec-h2, .sec-h3, .sec-h4, .sec-h5, .sec-h6';
  }

  function pageTopY() {
    var page = pageEl();
    if (!page) return 0;
    return page.getBoundingClientRect().top + window.scrollY;
  }

  function scrollToPage(pageNo) {
    if (!pageGuideVisualHeightPx) refreshNavigation();
    var y = pageTopY() + (pageNo - 1) * pageGuideVisualHeightPx - 58;
    window.scrollTo({ top: Math.max(0, y), behavior: 'smooth' });
  }

  function scrollToTarget(target) {
    if (!target) return;
    var y = target.getBoundingClientRect().top + window.scrollY - 58;
    window.scrollTo({ top: Math.max(0, y), behavior: 'smooth' });
  }

  function setSideOpen(open, persist) {
    currentSideOpen = !!open;
    document.body.classList.toggle('side-panel-open', currentSideOpen);
    document.body.classList.toggle('side-panel-closed', !currentSideOpen);
    var btn = document.getElementById('side-toggle');
    if (btn) {
      btn.classList.toggle('active', currentSideOpen);
      btn.setAttribute('aria-expanded', currentSideOpen ? 'true' : 'false');
    }
    if (persist) {
      try { localStorage.setItem('mathpreview.sideOpen', currentSideOpen ? '1' : '0'); } catch (e) {}
    }
  }

  function setPageMode(mode) {
    currentPageMode = mode === 'dynamic' ? 'dynamic' : 'a4';
    document.body.classList.toggle('page-mode-a4', currentPageMode === 'a4');
    document.body.classList.toggle('page-mode-dynamic', currentPageMode === 'dynamic');
    document.querySelectorAll('.page-mode-toggle button').forEach(function(btn) {
      var active = btn.getAttribute('data-page-mode') === currentPageMode;
      btn.classList.toggle('active', active);
    });
    var toggle = document.querySelector('.page-mode-toggle');
    if (toggle) toggle.setAttribute('data-page-mode', currentPageMode);
    try { localStorage.setItem('mathpreview.pageMode', currentPageMode); } catch (e) {}
    lastPageGuideSignature = '';
    updatePageScale();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
  }

  function updatePageScale(contentHeight) {
    var page = pageEl();
    var shell = pageShellEl();
    if (!page || !shell) return;
    if (currentPageMode === 'a4') {
      var available = Math.max(320, document.documentElement.clientWidth - 32);
      currentPageScale = Math.min(1, available / A4_CSS_WIDTH);
      document.documentElement.style.setProperty('--page-scale', currentPageScale.toFixed(4));
      shell.style.width = Math.round(A4_CSS_WIDTH * currentPageScale) + 'px';
      if (typeof contentHeight !== 'number') contentHeight = page.scrollHeight;
      shell.style.height = Math.ceil(contentHeight * currentPageScale) + 'px';
    } else {
      currentPageScale = 1;
      document.documentElement.style.setProperty('--page-scale', '1');
      shell.style.width = '';
      shell.style.height = '';
    }
  }

  function pageGuideMetrics() {
    if (currentPageMode === 'a4') {
      var layoutHeight = A4_CSS_WIDTH * A4_RATIO;
      return {
        layoutHeight: layoutHeight,
        visualHeight: layoutHeight * currentPageScale
      };
    }
    var dynamicHeight = Math.max(560, Math.min(1100, window.innerHeight - 84));
    return {
      layoutHeight: dynamicHeight,
      visualHeight: dynamicHeight
    };
  }

  function setSideTab(tab) {
    currentSideTab = tab === 'pages' ? 'pages' : 'index';
    document.querySelectorAll('.side-tab').forEach(function(btn) {
      var active = btn.getAttribute('data-side-tab') === currentSideTab;
      btn.classList.toggle('active', active);
      btn.setAttribute('aria-selected', active ? 'true' : 'false');
    });
    var index = document.getElementById('side-index');
    var pages = document.getElementById('side-pages');
    if (index) index.hidden = currentSideTab !== 'index';
    if (pages) pages.hidden = currentSideTab !== 'pages';
    try { localStorage.setItem('mathpreview.sideTab', currentSideTab); } catch (e) {}
    updateActivePage();
  }

  function headingSignature(headings) {
    return headings.map(function(heading) {
      return heading.id + '|' + headingLevel(heading) + '|' + cleanNavText(heading.textContent);
    }).join('\n');
  }

  function rebuildIndex(force) {
    var page = pageEl();
    var index = document.getElementById('side-index');
    if (!page || !index) return;
    var headings = Array.from(page.querySelectorAll(headingSelector()));
    var signature = headingSignature(headings);
    if (!force && signature === lastHeadingSignature) return;
    lastHeadingSignature = signature;
    index.replaceChildren();
    if (!headings.length) {
      var empty = document.createElement('div');
      empty.className = 'side-empty';
      empty.textContent = 'No sections';
      index.appendChild(empty);
      return;
    }
    headings.forEach(function(heading) {
      if (!heading.id) return;
      var item = document.createElement('a');
      item.href = '#' + encodeURIComponent(heading.id);
      item.className = 'side-link side-level-' + headingLevel(heading);
      item.textContent = cleanNavText(heading.textContent);
      index.appendChild(item);
    });
  }

  function rebuildPageGuides() {
    var page = pageEl();
    var pages = document.getElementById('side-pages');
    if (!page || !pages) return;
    pages.setAttribute('aria-label', currentPageMode === 'a4' ? 'A4 pages' : 'dynamic pages');

    var totalHeight = page.scrollHeight;
    updatePageScale(totalHeight);
    var metrics = pageGuideMetrics();
    pageGuideLayoutHeightPx = metrics.layoutHeight;
    pageGuideVisualHeightPx = metrics.visualHeight;
    pageGuideCount = Math.max(1, Math.ceil(totalHeight / pageGuideLayoutHeightPx));
    var signature = currentPageMode + '|' + pageGuideCount + '|' + Math.round(pageGuideLayoutHeightPx);
    if (signature === lastPageGuideSignature) {
      updateActivePage();
      return;
    }
    lastPageGuideSignature = signature;

    var oldLayer = page.querySelector('.page-guide-layer');
    if (oldLayer) oldLayer.remove();

    var layer = document.createElement('div');
    layer.className = 'page-guide-layer';
    layer.setAttribute('aria-hidden', 'true');
    for (var i = 1; i < pageGuideCount; i++) {
      var guide = document.createElement('div');
      guide.className = 'page-guide';
      guide.style.top = Math.round(i * pageGuideLayoutHeightPx) + 'px';
      var label = document.createElement('span');
      label.textContent = (currentPageMode === 'a4' ? 'A4 page ' : 'Page ') + (i + 1);
      guide.appendChild(label);
      layer.appendChild(guide);
    }
    page.appendChild(layer);

    pages.replaceChildren();
    for (var p = 1; p <= pageGuideCount; p++) {
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'side-link page-link';
      btn.setAttribute('data-page-jump', String(p));
      btn.textContent = (currentPageMode === 'a4' ? 'A4 ' : 'Page ') + p;
      pages.appendChild(btn);
    }
    updateActivePage();
  }

  function refreshNavigation() {
    navRefreshTimer = 0;
    if (navNeedsIndex) rebuildIndex(false);
    if (navNeedsPages) rebuildPageGuides();
    navNeedsIndex = false;
    navNeedsPages = false;
  }

  function scheduleNavigationRefresh(delay, includeIndex) {
    navNeedsPages = true;
    if (includeIndex !== false) navNeedsIndex = true;
    if (navRefreshTimer) clearTimeout(navRefreshTimer);
    navRefreshTimer = setTimeout(refreshNavigation, typeof delay === 'number' ? delay : NAV_IDLE_MS);
  }

  function updateActivePage() {
    if (!pageGuideVisualHeightPx) return;
    var current = Math.floor((window.scrollY + 70 - pageTopY()) / pageGuideVisualHeightPx) + 1;
    current = Math.min(pageGuideCount, Math.max(1, current));
    document.querySelectorAll('.page-link').forEach(function(btn) {
      btn.classList.toggle('active', btn.getAttribute('data-page-jump') === String(current));
    });
  }

  function scheduleActivePageUpdate() {
    if (activePageTimer) return;
    activePageTimer = requestAnimationFrame(function() {
      activePageTimer = 0;
      updateActivePage();
    });
  }

  function refreshAfterInitialMathJax(tries) {
    if (window.MathJax && window.MathJax.startup && window.MathJax.startup.promise) {
      window.MathJax.startup.promise.then(scheduleNavigationRefresh);
      return;
    }
    if (tries > 0) {
      setTimeout(function() { refreshAfterInitialMathJax(tries - 1); }, 150);
    }
  }

  function theoremRole(thm) {
    if (!thm) return null;
    if (thm.classList.contains('role-main')) return 'main';
    if (thm.classList.contains('role-supporting')) return 'supporting';
    if (thm.classList.contains('role-standard')) return 'standard';
    if (thm.classList.contains('role-omitted')) return 'omitted';
    return null;
  }

  function roleFromRefs(root) {
    if (!root || !root.querySelectorAll) return null;
    var refs = root.querySelectorAll(".ref[href^='#'], .ref[data-target]");
    for (var i = 0; i < refs.length; i++) {
      var href = refs[i].getAttribute('href') || '';
      var id = href.charAt(0) === '#' ? href.slice(1) : '';
      if (!id && refs[i].dataset.target) {
        id = refs[i].dataset.target.replace(/[^A-Za-z0-9_-]/g, '-');
      }
      if (!id) continue;
      try { id = decodeURIComponent(id); } catch (e) {}
      var target = document.getElementById(id);
      if (!target) continue;
      if (target.classList.contains('thm')) return theoremRole(target);
      var thm = target.closest ? target.closest('.thm') : null;
      if (thm) return theoremRole(thm);
    }
    return null;
  }

  function theoremRoleInBlock(block) {
    if (!block) return null;
    if (block.classList && block.classList.contains('thm')) {
      return theoremRole(block);
    }
    var thm = block.querySelector ? block.querySelector('.thm') : null;
    return thm ? theoremRole(thm) : null;
  }

  function isEmptyBlock(block) {
    if (!block) return true;
    if (block.querySelector && block.querySelector('.thm, .proof, .math, .sec-h0, .sec-h1, .sec-h2, .sec-h3, .sec-h4, .sec-h5, .sec-h6')) {
      return false;
    }
    return !(block.textContent || '').trim();
  }

  function precedingTheoremRole(proof) {
    // Top-level render blocks are wrapped in <article class="blk"> for
    // patching, so the proof's logical predecessor is usually in a previous
    // block wrapper rather than as proof.previousElementSibling.
    var block = proof.closest('.blk') || proof;
    var el = block.previousElementSibling;
    while (el) {
      var role = theoremRoleInBlock(el);
      if (role) return role;
      if (!isEmptyBlock(el)) return null;
      el = el.previousElementSibling;
    }
    return null;
  }

  function referencedTheoremRole(proof) {
    return roleFromRefs(proof.querySelector('.proof-head'));
  }

  function sectionProofRole(proof) {
    var block = proof.closest('.blk') || proof;
    var el = block.previousElementSibling;
    while (el) {
      var section = el.querySelector ? el.querySelector('.sec-h0, .sec-h1, .sec-h2, .sec-h3, .sec-h4, .sec-h5, .sec-h6') : null;
      if (section) {
        if (/\bProof\s+of\b/i.test(section.textContent || '')) {
          return roleFromRefs(section);
        }
        return null;
      }
      el = el.previousElementSibling;
    }
    return null;
  }

  function applyMode(mode) {
    currentProofMode = mode;
    document.getElementById('page').setAttribute('data-proof-mode', mode);
    document.querySelectorAll('.proof').forEach(function(p) {
      var role = theoremRole(p) || referencedTheoremRole(p) || precedingTheoremRole(p) || sectionProofRole(p);
      var folded;
      if (mode === 'all')        folded = false;
      else if (mode === 'main')  folded = (role !== 'main');
      else                       folded = (role !== 'main' && role !== 'supporting');
      if (role === null) folded = false;
      p.classList.toggle('folded', folded);
    });
    scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
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

  var manualStopRequested = false;
  function setStopButtonMode(stopped) {
    var btn = document.getElementById('server-stop');
    if (!btn) return;
    btn.textContent = stopped ? 'start' : 'stop';
    btn.title = stopped ? 'reload when preview server is running' : 'stop preview server';
    btn.classList.toggle('is-start', stopped);
  }

  function startServer() {
    var stopBtn = document.getElementById('server-stop');
    if (stopBtn) stopBtn.disabled = true;
    setStatus('updating', '↻ waiting');
    var started = performance.now();
    function poll() {
      fetch('/?start=' + Date.now(), { cache: 'no-store' })
        .then(function(res) {
          if (!res.ok) throw new Error('not ready');
          location.reload();
        })
        .catch(function() {
          if (performance.now() - started > 20000) {
            if (stopBtn) stopBtn.disabled = false;
            setStatus('dead', '○ start unavailable');
            return;
          }
          setTimeout(poll, 300);
        });
    }
    poll();
  }

  async function stopServer() {
    var stopBtn = document.getElementById('server-stop');
    var restartBtn = document.getElementById('server-restart');
    if (stopBtn) stopBtn.disabled = true;
    if (restartBtn) restartBtn.disabled = true;
    manualStopRequested = true;
    setStatus('updating', '↻ stopping');
    try {
      var res = await fetch('/stop', { method: 'POST', cache: 'no-store' });
      if (!res.ok) throw new Error('stop failed');
      if (stopBtn) stopBtn.disabled = false;
      setStopButtonMode(true);
      setStatus('dead', '○ stopped');
    } catch (e) {
      manualStopRequested = false;
      if (stopBtn) stopBtn.disabled = false;
      if (restartBtn) restartBtn.disabled = false;
      setStopButtonMode(false);
      setStatus('dead', '○ stop failed');
    }
  }

  function closestMath(node) {
    if (!node) return null;
    var el = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
    return el && el.closest ? el.closest('.math[data-tex]') : null;
  }

  function rangeIntersectsNode(range, node) {
    try { return range.intersectsNode(node); }
    catch (e) { return false; }
  }

  function selectedMathNodes(selection) {
    var page = pageEl() || document;
    var result = [];
    var seen = new Set();
    var math = page.querySelectorAll('.math[data-tex]');
    for (var r = 0; r < selection.rangeCount; r++) {
      var range = selection.getRangeAt(r);
      math.forEach(function(node) {
        if (seen.has(node) || !rangeIntersectsNode(range, node)) return;
        seen.add(node);
        result.push(node);
      });
    }
    return result;
  }

  function mathCopyTex(node) {
    return node ? (node.getAttribute('data-tex') || '') : '';
  }

  function clearSelectedMath() {
    if (selectedMath) selectedMath.classList.remove('math-selected');
    selectedMath = null;
  }

  function fragmentLatexText(node) {
    if (!node) return '';
    if (node.nodeType === Node.TEXT_NODE) return node.nodeValue || '';
    if (node.nodeType !== Node.ELEMENT_NODE && node.nodeType !== Node.DOCUMENT_FRAGMENT_NODE) {
      return '';
    }
    if (node.nodeType === Node.ELEMENT_NODE) {
      if (node.matches && node.matches('.math[data-tex]')) {
        var tex = mathCopyTex(node);
        return node.classList.contains('display') ? '\n' + tex + '\n' : tex;
      }
      if (node.matches && node.matches('.para-indent-marker, .page-guide-layer, .fold-marker')) {
        return '';
      }
      if (node.hidden) return '';
      if (node.tagName === 'BR') return '\n';
    }

    var text = '';
    var child = node.firstChild;
    while (child) {
      text += fragmentLatexText(child);
      child = child.nextSibling;
    }

    if (node.nodeType === Node.ELEMENT_NODE) {
      var tag = node.tagName;
      if (/^(P|DIV|ARTICLE|SECTION|H[1-6]|LI|DT|DD|TR)$/.test(tag) && text && !/\n$/.test(text)) {
        text += '\n';
      }
    }
    return text;
  }

  function normalizeCopiedLatex(text) {
    return (text || '')
      .replace(/\u00a0/g, ' ')
      .replace(/[ \t]+\n/g, '\n')
      .replace(/\n{3,}/g, '\n\n')
      .trim();
  }

  function selectionIsExactNode(selection, node) {
    if (!selection || selection.rangeCount !== 1 || !node || !node.parentNode) return false;
    var range = selection.getRangeAt(0);
    return range.startContainer === node.parentNode &&
      range.endContainer === node.parentNode &&
      range.endOffset === range.startOffset + 1 &&
      node.parentNode.childNodes[range.startOffset] === node;
  }

  function copySelectionAsLatex(e) {
    var selection = window.getSelection ? window.getSelection() : null;
    if (!selection || !selection.rangeCount) return;

    var activeMath = closestMath(document.activeElement);
    if (selection.isCollapsed && activeMath) {
      e.clipboardData.setData('text/plain', mathCopyTex(activeMath));
      e.preventDefault();
      return;
    }
    if (selectedMath &&
        selectedMath.isConnected &&
        selectedMath.classList.contains('math-selected') &&
        selectionIsExactNode(selection, selectedMath)) {
      e.clipboardData.setData('text/plain', mathCopyTex(selectedMath));
      e.preventDefault();
      return;
    }
    if (selection.isCollapsed) return;

    var mathNodes = selectedMathNodes(selection);
    if (!mathNodes.length) return;

    var range = selection.getRangeAt(0);
    var fragment = range.cloneContents();
    var text = fragmentLatexText(fragment);
    if (!text || !fragment.querySelector || !fragment.querySelector('.math[data-tex]')) {
      var commonMath = closestMath(range.commonAncestorContainer);
      if (commonMath) {
        text = mathCopyTex(commonMath);
      }
    }
    if (!text) {
      text = mathNodes.map(mathCopyTex).filter(Boolean).join('\n\n');
    }

    e.clipboardData.setData('text/plain', normalizeCopiedLatex(text));
    e.preventDefault();
  }

  function selectMathNode(math) {
    if (!math) return;
    clearSelectedMath();
    selectedMath = math;
    math.classList.add('math-selected');
    if (math.focus) {
      try { math.focus({ preventScroll: true }); }
      catch (e) { math.focus(); }
    }
    var range = document.createRange();
    range.selectNode(math);
    var selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  }

  // Event delegation survives `#page` innerHTML replacement.
  document.addEventListener('copy', copySelectionAsLatex);
  document.addEventListener('mousedown', function(e) {
    var math = e.target.closest('.math[data-tex]');
    if (math) {
      e.preventDefault();
      selectMathNode(math);
      return;
    }
    clearSelectedMath();
  });
  document.addEventListener('dblclick', function(e) {
    var math = e.target.closest('.math[data-tex]');
    if (!math) return;
    e.preventDefault();
    selectMathNode(math);
  });
  document.addEventListener('click', function(e) {
    var restart = e.target.closest('#server-restart');
    if (restart) {
      restartServer();
      return;
    }
    var stop = e.target.closest('#server-stop');
    if (stop) {
      if (manualStopRequested) startServer();
      else stopServer();
      return;
    }
    var sideToggle = e.target.closest('#side-toggle');
    if (sideToggle) {
      setSideOpen(!currentSideOpen, true);
      return;
    }
    var pageMode = e.target.closest('.page-mode-toggle button');
    if (pageMode) {
      setPageMode(pageMode.getAttribute('data-page-mode'));
      return;
    }
    var sideTab = e.target.closest('.side-tab');
    if (sideTab) {
      setSideTab(sideTab.getAttribute('data-side-tab'));
      return;
    }
    var pageJump = e.target.closest('[data-page-jump]');
    if (pageJump) {
      scrollToPage(parseInt(pageJump.getAttribute('data-page-jump'), 10));
      return;
    }
    var indexLink = e.target.closest('#side-index a');
    if (indexLink && (indexLink.getAttribute('href') || '').charAt(0) === '#') {
      e.preventDefault();
      var id = indexLink.getAttribute('href').slice(1);
      try { id = decodeURIComponent(id); } catch (err) {}
      scrollToTarget(document.getElementById(id));
      return;
    }
    var btn = e.target.closest('.proof-toggle button');
    if (btn) {
      var mode = btn.getAttribute('data-mode');
      applyMode(mode);
      document.querySelectorAll('.proof-toggle button').forEach(function(x) {
        x.classList.toggle('active', x === btn);
      });
      document.querySelector('.proof-toggle').setAttribute('data-mode', mode);
      return;
    }
    var head = e.target.closest('.proof-head');
    if (head) {
      head.closest('.proof').classList.toggle('folded');
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
    }
  });
  document.addEventListener('keydown', function(e) {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    var head = e.target.closest('.proof-head');
    if (head) {
      e.preventDefault();
      head.closest('.proof').classList.toggle('folded');
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
    }
  });

  var pendingTypeset = new Set();
  var typesetTimer = 0;
  var typesetBusy = false;
  var TYPESET_IDLE_MS = 300;

  function clearRemovedMath(nodes) {
    if (!nodes.length || !window.MathJax || !window.MathJax.typesetClear) return;
    try { window.MathJax.typesetClear(nodes); }
    catch (e) { console.warn('mathpreview MathJax clear:', e); }
  }

  function leftoverMath(oldByHash) {
    var leftovers = [];
    oldByHash.forEach(function(pool) {
      for (var i = 0; i < pool.length; i++) leftovers.push(pool[i]);
    });
    return leftovers;
  }

  function copyAttr(dst, src, name) {
    var value = src.getAttribute(name);
    if (value === null) dst.removeAttribute(name);
    else dst.setAttribute(name, value);
  }

  function syncReusedMathNode(oldEl, newEl) {
    oldEl.id = newEl.id;
    copyAttr(oldEl, newEl, 'data-src');
    copyAttr(oldEl, newEl, 'data-tex');
    copyAttr(oldEl, newEl, 'title');
    copyAttr(oldEl, newEl, 'tabindex');
  }

  function pageBlocks(page) {
    return Array.prototype.filter.call(page.children, function(el) {
      return el.classList && el.classList.contains('blk');
    });
  }

  function syncReusedBlock(oldBlock, newBlock) {
    oldBlock.id = newBlock.id;
    oldBlock.className = newBlock.className;
    copyAttr(oldBlock, newBlock, 'data-blockhash');
  }

  function syncPatchBlockMetadata(page, blocks) {
    if (!blocks || !blocks.length) return;
    var els = pageBlocks(page);
    for (var i = 0; i < els.length && i < blocks.length; i++) {
      els[i].id = blocks[i].id;
      els[i].setAttribute('data-blockhash', blocks[i].hash);
    }
  }

  function indexMathByHash(root, oldByHash) {
    root.querySelectorAll('.math[data-hash]').forEach(function(oldEl) {
      var arr = oldByHash.get(oldEl.dataset.hash);
      if (!arr) { arr = []; oldByHash.set(oldEl.dataset.hash, arr); }
      arr.push(oldEl);
    });
  }

  function queueTypeset(nodes) {
    nodes.forEach(function(node) {
      pendingTypeset.add(node);
      node.classList.add('math-pending');
    });
    if (!pendingTypeset.size) {
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
      return;
    }
    if (typesetTimer) clearTimeout(typesetTimer);
    typesetTimer = setTimeout(flushTypeset, TYPESET_IDLE_MS);
  }

  async function flushTypeset() {
    typesetTimer = 0;
    if (typesetBusy) {
      typesetTimer = setTimeout(flushTypeset, 80);
      return;
    }
    if (!pendingTypeset.size) return;
    if (!window.MathJax || !window.MathJax.typesetPromise) {
      typesetTimer = setTimeout(flushTypeset, TYPESET_IDLE_MS);
      return;
    }

    var nodes = Array.from(pendingTypeset).filter(function(node) {
      return node && node.isConnected;
    });
    pendingTypeset.clear();
    if (!nodes.length) return;

    typesetBusy = true;
    setStatus('updating', '↻ typesetting ' + nodes.length + ' math');
    var tStart = performance.now();
    try {
      await window.MathJax.typesetPromise(nodes);
      var ms = Math.round(performance.now() - tStart);
      nodes.forEach(function(node) { node.classList.remove('math-pending'); });
      setStatus('live',
        '● live / idle typeset ' + ms + 'ms (' + nodes.length + ' math)' +
        memSuffix(window._lastRss));
    } catch (e) {
      console.error('mathpreview MathJax:', e);
      setStatus('dead', '○ MathJax error');
    } finally {
      typesetBusy = false;
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
      if (pendingTypeset.size && !typesetTimer) {
        typesetTimer = setTimeout(flushTypeset, TYPESET_IDLE_MS);
      }
    }
  }

  // Apply a server-computed block-level patch. Ops are positional ranges
  // against top-level .blk elements; after applying them we retag block ids
  // by position. This preserves shifted unchanged blocks without the id
  // collisions caused by insertion-before-existing-content edits.
  async function applyPatch(ops, blocksMeta) {
    var tStart = performance.now();
    setStatus('updating', '↻ patching');
    var page = document.getElementById('page');
    var tpl = document.createElement('template');
    var needTypeset = [];
    var reusedMath = 0, totalMath = 0;
    var replacedBlocks = 0, insertedBlocks = 0, removedBlocks = 0;
    var detachPage = ops.length > 8;
    var pageParent = detachPage ? page.parentNode : null;
    var pageNextSibling = detachPage ? page.nextSibling : null;
    if (pageParent) pageParent.removeChild(page);
    var oldGuideLayer = page.querySelector('.page-guide-layer');
    if (oldGuideLayer) oldGuideLayer.remove();

    try {
      for (var i = 0; i < ops.length; i++) {
        var op = ops[i];
        if (op.type === 'range') {
          var blocks = pageBlocks(page);
          var start = Math.max(0, Math.min(op.index || 0, blocks.length));
          var removeCount = Math.max(0, Math.min(op.remove || 0, blocks.length - start));
          var anchor = blocks[start + removeCount] || null;

          var oldByHash = new Map();
          for (var r = 0; r < removeCount; r++) indexMathByHash(blocks[start + r], oldByHash);

          tpl.innerHTML = op.html || '';
          var frag = tpl.content;
          var inserted = frag.querySelectorAll('.blk').length;
          frag.querySelectorAll('.math[data-hash]').forEach(function(newEl) {
            totalMath++;
            var pool = oldByHash.get(newEl.dataset.hash);
            if (pool && pool.length > 0) {
              var oldEl = pool.shift();
              syncReusedMathNode(oldEl, newEl);
              newEl.replaceWith(oldEl);
              reusedMath++;
            } else {
              needTypeset.push(newEl);
            }
          });
          clearRemovedMath(leftoverMath(oldByHash));

          for (var d = 0; d < removeCount; d++) {
            if (blocks[start + d] && blocks[start + d].parentNode === page) {
              blocks[start + d].remove();
              removedBlocks++;
            }
          }
          if (inserted) {
            page.insertBefore(frag, anchor);
            insertedBlocks += inserted;
          }
          replacedBlocks += Math.min(removeCount, inserted);
        }
      }
      syncPatchBlockMetadata(page, blocksMeta);
    } finally {
      if (pageParent) {
        if (pageNextSibling) pageParent.insertBefore(page, pageNextSibling);
        else pageParent.appendChild(page);
      }
    }

    queueTypeset(needTypeset);

    var total = Math.round(performance.now() - tStart);
    setStatus('live',
      '● ' + total + 'ms · ' + replacedBlocks + 'r' +
      (insertedBlocks ? '/+' + insertedBlocks : '') +
      (removedBlocks ? '/-' + removedBlocks : '') +
      ' / typeset ' + (needTypeset.length ? 'queued' : '0') +
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
  var WS_PROTOCOL_VERSION = '2';
  var status = document.getElementById('ws-status');
  function setStatus(cls, text) {
    if (!status) return;
    status.className = 'status ' + cls;
    status.textContent = text;
  }
  function connect() {
    if (!window.WebSocket) return;
    var url = (location.protocol === 'https:' ? 'wss://' : 'ws://') +
      location.host + '/ws?v=' + encodeURIComponent(WS_PROTOCOL_VERSION);
    var ws;
    try { ws = new WebSocket(url); } catch (e) { return; }
    ws.onopen  = function() { setStatus('live', '● live'); };
    ws.onclose = function() {
      if (manualStopRequested) {
        setStatus('dead', '○ stopped');
        return;
      }
      setStatus('dead', '○ disconnected');
      setTimeout(connect, 1000);
    };
    ws.onerror = function() { setStatus('dead', '○ error'); };
    ws.onmessage = async function(ev) {
      try {
        var msg = JSON.parse(ev.data);
        if (typeof msg.rss_mib === 'number') window._lastRss = msg.rss_mib;
        if (msg.event === 'patch') {
          await applyPatch(msg.ops, msg.blocks);
          applyMode(currentProofMode);
          scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, true);
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

          // Index existing blocks by rendered content hash. Whole-block reuse
          // is much cheaper than diffing every MathJax node when a full update
          // is unavoidable.
          var oldBlocksByHash = new Map();
          pageBlocks(page).forEach(function(block) {
            var hash = block.getAttribute('data-blockhash');
            if (!hash) return;
            var arr = oldBlocksByHash.get(hash);
            if (!arr) { arr = []; oldBlocksByHash.set(hash, arr); }
            arr.push(block);
          });
          var tIndex = performance.now();

          // Parse new HTML into a detached <template> (faster than <div>).
          var tpl = document.createElement('template');
          tpl.innerHTML = msg.html;
          var buf = tpl.content;
          var tParse = performance.now();

          var reusedBlocks = 0;
          buf.querySelectorAll('.blk[data-blockhash]').forEach(function(newBlock) {
            var pool = oldBlocksByHash.get(newBlock.getAttribute('data-blockhash'));
            if (pool && pool.length > 0) {
              var oldBlock = pool.shift();
              syncReusedBlock(oldBlock, newBlock);
              oldBlock.setAttribute('data-mp-reused-block', '1');
              newBlock.replaceWith(oldBlock);
              reusedBlocks++;
            }
          });

          // For remaining changed blocks, transplant matching old math nodes.
          var needTypeset = [];
          var oldByHash = new Map();
          oldBlocksByHash.forEach(function(pool) {
            for (var i = 0; i < pool.length; i++) indexMathByHash(pool[i], oldByHash);
          });
          var newMath = buf.querySelectorAll('.math[data-hash]');
          newMath.forEach(function(newEl) {
            var block = newEl.closest('.blk');
            if (block && block.getAttribute('data-mp-reused-block') === '1') return;
            var pool = oldByHash.get(newEl.dataset.hash);
            if (pool && pool.length > 0) {
              var oldEl = pool.shift();
              syncReusedMathNode(oldEl, newEl);
              newEl.replaceWith(oldEl);
            } else {
              needTypeset.push(newEl);
            }
          });
          var tDiff = performance.now();
          clearRemovedMath(leftoverMath(oldByHash));

          page.replaceChildren(buf);

          // Reattach #page in its original position. One layout pass for
          // the whole update, not 300+.
          if (pageNextSibling) pageParent.insertBefore(page, pageNextSibling);
          else pageParent.appendChild(page);
          page.querySelectorAll('[data-mp-reused-block]').forEach(function(block) {
            block.removeAttribute('data-mp-reused-block');
          });
          var tSwap = performance.now();

          queueTypeset(needTypeset);

          var tDone = performance.now();
          var total = Math.round(tDone - tStart);
          var reused = newMath.length - needTypeset.length;
          setStatus('live',
            '● ' + total + 'ms · idx ' + Math.round(tIndex - tStart) +
            ' / parse ' + Math.round(tParse - tIndex) +
            ' / diff ' + Math.round(tDiff - tParse) +
            ' / swap ' + Math.round(tSwap - tDiff) +
            ' / typeset ' + (needTypeset.length ? 'queued' : '0') +
            ' (reused ' + reused + '/' + newMath.length +
            (reusedBlocks ? ', blocks ' + reusedBlocks : '') + ')' +
            memSuffix(window._lastRss));
          applyMode(currentProofMode);
          scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, true);
        } else if (msg.event === 'full-reload') {
          location.reload();
        } else if (msg.event === 'error') {
          setStatus('dead', '○ ' + (msg.message || 'render error'));
        }
      } catch (e) { console.error('mathpreview WS:', e); }
    };
  }
  try {
    setPageMode(localStorage.getItem('mathpreview.pageMode') || 'a4');
    setSideTab(localStorage.getItem('mathpreview.sideTab') || 'index');
    var storedSideOpen = localStorage.getItem('mathpreview.sideOpen');
    setSideOpen(storedSideOpen === null ? window.innerWidth > 1340 : storedSideOpen === '1', false);
  } catch (e) {
    setPageMode('a4');
    setSideTab('index');
    setSideOpen(window.innerWidth > 1340, false);
  }
  scheduleNavigationRefresh();
  refreshAfterInitialMathJax(40);
  window.addEventListener('load', scheduleNavigationRefresh);
  window.addEventListener('resize', function() {
    updatePageScale();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
  });
  window.addEventListener('scroll', scheduleActivePageUpdate, { passive: true });
  connect();
})();
"#;

const DEFAULT_CSS: &str = r#"
:root {
  --fg: #1c1c1c;
  --muted: #666;
  --bg: #f2f1ec;
  --paper: #ffffff;
  --accent: #5b3ea2;
  --supporting: #2b6cb0;
  --standard: #888;
  --omitted: #b25800;
  --border: #e0e0e0;
  --a4-width: 794px;
  --dynamic-width: 720px;
  --page-scale: 1;
}
* { box-sizing: border-box; }
html, body {
  margin: 0;
  background: var(--bg);
  color: var(--fg);
  font: 16px/1.42 'Latin Modern Roman', 'CMU Serif', 'Computer Modern Serif', 'STIX Two Text', 'Iowan Old Style', 'Palatino Linotype', Palatino, Georgia, serif;
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
.side-toggle,
.server-restart,
.server-stop,
.page-mode-toggle button,
.proof-toggle button {
  border: 1px solid var(--border); background: #fff; padding: 4px 10px;
  font: inherit; cursor: pointer;
}
.server-restart:disabled,
.server-stop:disabled { opacity: 0.55; cursor: wait; }
.server-stop.is-start { border-color: var(--supporting); color: var(--supporting); }
.side-toggle.active,
.page-mode-toggle button.active,
.proof-toggle button.active { background: var(--accent); color: #fff; border-color: var(--accent); }
.page-mode-toggle,
.proof-toggle {
  display: inline-flex;
}
.page-mode-toggle button + button,
.proof-toggle button + button {
  margin-left: -1px;
}
.side-panel {
  position: fixed;
  left: 16px;
  top: 58px;
  bottom: 16px;
  width: 236px;
  display: flex;
  flex-direction: column;
  background: rgba(255, 255, 255, 0.92);
  border: 1px solid #d8d5cc;
  box-shadow: 0 8px 22px rgba(0, 0, 0, 0.05);
  z-index: 9;
  font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
  font-size: 12px;
  transform: translateX(0);
  transition: transform 0.16s ease;
}
body.side-panel-closed .side-panel { transform: translateX(calc(-100% - 24px)); }
.side-tabs {
  display: grid;
  grid-template-columns: 1fr 1fr;
  border-bottom: 1px solid var(--border);
}
.side-tab {
  border: 0;
  border-right: 1px solid var(--border);
  background: #fff;
  padding: 7px 8px;
  font: inherit;
  cursor: pointer;
}
.side-tab:last-child { border-right: 0; }
.side-tab.active {
  background: var(--accent);
  color: #fff;
}
.side-list {
  overflow: auto;
  padding: 8px 6px 10px;
}
.side-link {
  display: block;
  width: 100%;
  border: 0;
  border-radius: 3px;
  background: transparent;
  color: var(--fg);
  cursor: pointer;
  font: inherit;
  line-height: 1.24;
  padding: 5px 7px;
  text-align: left;
  text-decoration: none;
}
.side-link:hover,
.side-link.active {
  background: #f1efe8;
  color: var(--accent);
}
.side-level-0,
.side-level-1 { font-weight: 700; }
.side-level-3 { padding-left: 17px; }
.side-level-4 { padding-left: 29px; }
.side-level-5,
.side-level-6 { padding-left: 41px; color: var(--muted); }
.side-empty {
  color: var(--muted);
  padding: 8px 7px;
}
#page-shell {
  width: min(var(--dynamic-width), calc(100vw - 32px));
  margin: 28px auto 64px;
  overflow: visible;
}
body.page-mode-a4 #page-shell {
  width: var(--a4-width);
}
body.page-mode-dynamic #page-shell {
  width: min(var(--dynamic-width), calc(100vw - 32px));
}
main#page {
  width: 100%;
  max-width: none; margin: 0; padding: 46px 64px 68px;
  background: var(--paper);
  border: 1px solid #d8d5cc;
  box-shadow: 0 10px 28px rgba(0, 0, 0, 0.06);
  position: relative;
}
body.page-mode-a4 main#page {
  width: var(--a4-width);
  transform: scale(var(--page-scale));
  transform-origin: top left;
}
body.page-mode-dynamic main#page {
  width: 100%;
  transform: none;
}
.page-guide-layer {
  position: absolute;
  inset: 0 0 auto 0;
  height: 0;
  pointer-events: none;
  z-index: 5;
}
.page-guide {
  position: absolute;
  left: -1px;
  right: -1px;
  border-top: 1px dashed rgba(91, 62, 162, 0.42);
}
.page-guide span {
  position: absolute;
  right: 10px;
  top: -10px;
  padding: 1px 6px;
  background: var(--paper);
  color: var(--accent);
  font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
  font-size: 10px;
  line-height: 1.4;
}
@media (max-width: 1340px) {
  .side-panel {
    left: 8px;
    width: min(286px, calc(100vw - 16px));
    transform: translateX(calc(-100% - 24px));
  }
  body.side-panel-open .side-panel { transform: translateX(0); }
}
@media (max-width: 720px) {
  .topbar {
    gap: 6px;
    padding: 8px 10px;
    flex-wrap: wrap;
  }
  .topbar-spacer { flex-basis: 100%; height: 0; }
  main#page {
    padding: 34px 24px 46px;
  }
  #page-shell {
    width: calc(100vw - 16px);
    margin-top: 12px;
  }
}
.sec-h0,
.sec-h1,
.sec-h2,
.sec-h3,
.sec-h4,
.sec-h5,
.sec-h6 {
  font-family: inherit;
  font-weight: 700;
  line-height: 1.18;
  margin: 1.45em 0 0.55em;
}
.sec-h0 { font-size: 1.55em; text-align: center; }
.sec-h1 { font-size: 1.38em; text-align: center; }
.sec-h2 { font-size: 1.12em; }
.sec-h3 { font-size: 1.03em; }
.sec-h4,
.sec-h5,
.sec-h6 { font-size: 1em; }
.title-block + .blk .sec-h1,
.title-block + .blk .sec-h2 { margin-top: 1.1em; }
.thm {
  margin: 0.95em 0;
  padding: 0.72em 0.9em 0.76em;
  border: 1px solid var(--border);
  background: #fff;
  border-left: 3px solid var(--standard);
}
.thm.role-main       { border-left-color: var(--accent); }
.thm.role-supporting { border-left-color: var(--supporting); }
.thm.role-standard   { border-left-color: var(--standard); }
.thm.role-omitted    { border-left: 1px dashed var(--omitted); border-color: var(--omitted); }
.thm-head { display: block; font-family: inherit; font-weight: 700; margin-bottom: 0.28em; }
.thm-kind { color: var(--accent); }
.thm-num { font-variant-numeric: tabular-nums; color: var(--fg); }
.sec-num { color: var(--fg); font-variant-numeric: tabular-nums; margin-right: 0.45em; }

/* LaTeX-style display math spacing:
   \abovedisplayskip and \belowdisplayskip are ~12pt in standard classes,
   roughly 1em at body text size. Equation number sits on the right at the
   baseline of the math, not on its own line. */
.math.display {
  display: block;
  text-align: center;
  margin: 0.8em 0;
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
  margin-top: 0.3em;
}
.thm.role-supporting .thm-kind { color: var(--supporting); }
.thm.role-standard .thm-kind   { color: var(--standard); }
.thm.role-omitted .thm-kind    { color: var(--omitted); }
.thm-name { color: var(--muted); font-weight: 400; }
.thm-body { margin: 0; font-style: italic; }
.thm-omitref { margin-top: 0.3em; font-size: 0.92em; color: var(--omitted); }
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
.proof { margin: 0.85em 0; }
.proof-head {
  display: inline;
  font-family: inherit;
  font-weight: 700; font-size: 1em; color: var(--fg);
  cursor: pointer; user-select: none;
}
.proof-head::after { content: " "; }
.proof-head:focus { outline: 2px solid var(--accent); outline-offset: 2px; }
.proof-head:hover { color: var(--accent); }
.proof-of { color: var(--fg); font-weight: inherit; }
.proof-body { display: inline; margin: 0; }
.qed { float: right; font-style: normal; }
.fold-marker { display: inline-block; width: 0.7em; color: var(--muted); transition: transform 0.1s ease; }
.fold-marker::before { content: "▾"; }
.proof.folded .fold-marker { transform: rotate(-90deg); }
.proof.folded .proof-body { display: none; }
.proof.folded .proof-head { color: var(--muted); }

.math {
  cursor: text;
  user-select: text;
  -webkit-user-select: text;
}
.math.math-selected {
  outline: 2px solid rgba(91, 62, 162, 0.5);
  outline-offset: 3px;
  background: rgba(91, 62, 162, 0.08);
}
.math:focus {
  outline: 2px solid rgba(91, 62, 162, 0.5);
  outline-offset: 3px;
}
.math.inline { white-space: nowrap; }
.para {
  margin: 0;
  text-indent: 1.45em;
}
.blk:has(> .title-block) + .blk .para:not(.para-indent),
.blk:has(> .sec-h0) + .blk .para:not(.para-indent),
.blk:has(> .sec-h1) + .blk .para:not(.para-indent),
.blk:has(> .sec-h2) + .blk .para:not(.para-indent),
.blk:has(> .sec-h3) + .blk .para:not(.para-indent),
.blk:has(> .sec-h4) + .blk .para:not(.para-indent),
.blk:has(> .sec-h5) + .blk .para:not(.para-indent),
.blk:has(> .sec-h6) + .blk .para:not(.para-indent),
.blk:has(> .math.display) + .blk .para:not(.para-indent),
.blk:has(> .thm) + .blk .para:not(.para-indent),
.blk:has(> .proof) + .blk .para:not(.para-indent),
.blk:first-child .para:not(.para-indent) {
  text-indent: 0;
}
.para.para-indent { text-indent: 1.45em; }
.para.para-noindent { text-indent: 0; }
.para.para-flow { margin-top: 0.55em; }
.para-indent-marker {
  display: inline-block;
  width: 1.45em;
}
.para-break { display: block; height: 0.72em; }
.ref { color: var(--accent); text-decoration: none; border-bottom: 1px dotted var(--accent); }
.cite { color: var(--supporting); text-decoration: none; }
.cite:hover { text-decoration: underline; }
.cite.missing { color: #999; font-family: monospace; font-size: 0.9em; }
.title-block { text-align: center; margin: 0.4em 0 2.1em; }
.paper-title { font-size: 1.35em; font-weight: 700; line-height: 1.2; margin: 0 0 1.15em; }
.paper-authors { display: grid; gap: 0.32em; justify-items: center; margin-bottom: 0.65em; }
.paper-author { font-size: 1em; color: var(--fg); }
.paper-author-name { font-weight: 400; }
.paper-address,
.paper-email { font-size: 0.86em; color: var(--muted); line-height: 1.25; }
.paper-email a { color: var(--supporting); text-decoration: none; }
.paper-email a:hover { text-decoration: underline; }
.paper-date { font-size: 0.92em; color: var(--fg); }
.paper-date:empty { display: none; }
.paper-abstract {
  margin: -0.7em auto 1.8em;
  max-width: 92%;
  font-size: 0.94em;
  line-height: 1.38;
  color: var(--fg);
}
.paper-abstract h2 {
  margin: 0 0 0.35em;
  text-align: center;
  font-size: 1em;
  font-weight: 700;
}
.paper-abstract .para { margin: 0; text-indent: 0; }
.paper-abstract-body { text-align: left; }
.references { margin-top: 2.4em; padding-top: 0; border-top: 0; }
.references h2 { font-size: 1.12em; margin: 1.45em 0 0.55em; }
.bib-list { display: grid; grid-template-columns: max-content 1fr; column-gap: 0.8em; row-gap: 0.35em; font-size: 0.92em; line-height: 1.35; padding: 0; margin: 0; }
.bib-list.bib-style-numeric    { grid-template-columns: 2.4em 1fr; }
.bib-list.bib-style-alphabetic { grid-template-columns: max-content 1fr; }
.bib-list.bib-style-authoryear { grid-template-columns: max-content 1fr; }
.bib-label { font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif; color: var(--muted); white-space: nowrap; }
.bib-entry { margin: 0; }
.bib-entry em { font-style: italic; }
.bib-doi, .bib-url { color: var(--supporting); font-size: 0.85em; word-break: break-all; }
.bib-missing { color: var(--omitted); font-style: italic; }
.blk { display: contents; }
.opaque-env { padding: 0.2em 0 0.2em 0.75em; border-left: 1px solid var(--border); color: var(--muted); margin: 0.5em 0; }
.float-placeholder { margin: 1em 0; text-align: center; color: var(--muted); }
.float-placeholder figcaption { font-size: 0.95em; line-height: 1.35; }
.float-kind { font-weight: 700; color: var(--fg); }
.float-asset { font-size: 0.85em; margin-bottom: 0.35em; }
.float-image {
  display: block;
  max-width: 100%;
  height: auto;
  margin: 0 auto;
}
.float-pdf-preview { background: #fff; }
.proof-step,
.proof-case { color: var(--fg); }
.proof-step { font-weight: 700; }
.proof-case { font-style: italic; }
.flow-marker-break {
  display: block;
  margin-top: 0.55em;
}
.label-anchor { position: relative; top: -4.5rem; }
.status { font-size: 11px; padding: 2px 6px; border-radius: 3px; color: var(--muted); }
.status.live { color: #1e7e1e; }
.status.dead { color: #b22222; }
.status.updating { color: var(--accent); }
.latex-list { margin: 0.55em 0; padding-left: 1.8em; }
.latex-list.itemize { list-style: disc; }
.latex-list.enumerate { list-style: decimal; }
.latex-list.description { display: grid; grid-template-columns: max-content 1fr; column-gap: 0.6em; padding-left: 0; }
.latex-list.description .item-marker { font-weight: 600; }
.latex-list .item-body { margin-bottom: 0.12em; }
details.warnings {
  max-width: 720px;
  margin: 12px auto 0;
  padding: 6px 12px;
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

    #[test]
    fn blank_line_between_text_paragraphs_renders_indented_paragraphs() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nFirst paragraph.\n\nSecond paragraph.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert_eq!(out.body_html.matches(r#"<p class="para">"#).count(), 2);
        assert!(out.body_html.contains("First paragraph."));
        assert!(out.body_html.contains("Second paragraph."));
        assert!(!out.body_html.contains("<br><br>Second paragraph."));
    }

    #[test]
    fn blank_line_after_display_renders_indented_paragraph_without_extra_break() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\[a=b\\]\n\nNext line.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r"\[a=b\]"));
        assert!(out
            .body_html
            .contains(r#"<p class="para para-indent">Next line."#));
        assert!(!out.body_html.contains("<br><br>Next line."));
    }

    #[test]
    fn display_continuation_without_blank_line_stays_unindented() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\[a=b\\]\nNext line.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r"\[a=b\]"));
        assert!(out.body_html.contains(r#"<p class="para">Next line."#));
        assert!(!out
            .body_html
            .contains(r#"<p class="para para-indent">Next line."#));
    }

    #[test]
    fn single_newline_after_inline_math_keeps_interword_space() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nBefore $x$\nand after.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r"\(x\)</span> and after."));
        assert!(!out.body_html.contains(r"\(x\)</span>and after."));
    }

    #[test]
    fn math_nodes_store_latex_for_copying() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nInline $a&b$.\n\\begin{equation}\nx<y\n\\label{eq:test}\n\\end{equation}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"data-tex="\(a&amp;b\)""#));
        assert!(out.body_html.contains(r#"data-tex="\begin{equation}"#));
        assert!(out.body_html.contains("x&lt;y"));
        assert!(out.body_html.contains(r#"\label{eq:test}"#));
        assert!(out.body_html.contains(r#"title="Copy as LaTeX""#));
        assert!(out.body_html.contains(r#"tabindex="0""#));
    }

    #[test]
    fn nested_blank_line_after_display_renders_indent_marker_without_extra_break() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{proof}\n\\[a=b\\]\n\nNext line.\n\\end{proof}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r"\[a=b\]"));
        assert!(out.body_html.contains("para-indent-marker"));
        assert!(out.body_html.contains("Next line."));
        assert!(!out.body_html.contains("<br><br>Next line."));
    }

    #[test]
    fn nested_text_blank_line_uses_indent_marker_without_extra_break() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{proof}\nFirst paragraph.\n\nSecond paragraph.\n\\end{proof}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains("First paragraph."));
        assert!(out.body_html.contains("para-indent-marker"));
        assert!(out.body_html.contains("Second paragraph."));
        assert!(!out.body_html.contains("<br><br>Second paragraph."));
    }

    #[test]
    fn proof_role_is_rendered_as_metadata_not_title() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{proof}[role=main, of={Lemma 1}]\nQED.\n\\end{proof}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"<div class="proof role-main""#));
        assert!(out.body_html.contains(r#"data-role="main""#));
        assert!(out
            .body_html
            .contains(r#"Proof <span class="proof-of">of Lemma 1</span>."#));
        assert!(!out.body_html.contains("role=main"));
    }

    #[test]
    fn step_markers_are_numbered_and_start_noindent_paragraphs() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nBefore. \\step First step.\n\\step[With $x$] Second step.\n\\restartsteps[4]\n\\step Restarted.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"<p class="para">Before. "#));
        assert!(out.body_html.contains(
            r#"<p class="para para-noindent para-flow"><span class="proof-step flow-marker"><strong>Step 1:</strong></span> First step."#
        ));
        assert!(out.body_html.contains(
            r#"<span class="proof-step flow-marker"><strong>Step 2:</strong> With <span class="math inline"#
        ));
        assert!(out.body_html.contains(r#"data-tex="\(x\)""#));
        assert!(out.body_html.contains(
            r#"<span class="proof-step flow-marker"><strong>Step 5:</strong></span> Restarted."#
        ));
        assert!(!out.body_html.contains("<strong>Step.</strong>"));
    }

    #[test]
    fn proof_flow_markers_reset_and_break_nested_text_flow() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{proof}\nIntro.\\restartsteps\n\\step First.\n\\step[Second] More.\n\\begin{proofcases}\\case[Diagonal] Case text.\\end{proofcases}\n\\end{proof}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains("Intro."));
        assert!(out.body_html.contains("flow-marker-break"));
        assert!(out
            .body_html
            .contains(r#"<strong>Step 1:</strong></span> First."#));
        assert!(out
            .body_html
            .contains(r#"<strong>Step 2:</strong> Second.</span> More."#));
        assert!(out
            .body_html
            .contains(r#"<strong>Case I:</strong> Diagonal.</span> Case text."#));
        assert!(!out.body_html.contains(r#"\restartsteps"#));
        assert!(!out.body_html.contains(r#"\begin{proofcases}"#));
    }

    #[test]
    fn plain_bibliography_sorts_and_formats_like_bibtex_plain() {
        let dir =
            std::env::temp_dir().join(format!("mathpreview-plain-bib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("refs.bib"),
            r#"
@article{Zulu2020,
  author = {Zulu, Zoe},
  title = {{Later} Result},
  journal = {Journal of {{Tests}}},
  volume = {12},
  number = {3},
  pages = {45--67},
  year = {2020},
}
@book{Alpha2021,
  author = {Alpha, Ann and Beta, Bob},
  title = {Book of {{Things}}},
  publisher = {Press},
  address = {New York},
  year = {2021},
}
"#,
        )
        .unwrap();

        let root = dir.join("main.tex");
        let out = crate::render_project_from_source(
            &root,
            "\\begin{document}\nFirst \\cite{Zulu2020}. Then \\cite{Alpha2021}.\n\\bibliographystyle{plain}\n\\bibliography{refs}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"data-key="Zulu2020">2</a>"#));
        assert!(out.body_html.contains(r#"data-key="Alpha2021">1</a>"#));
        let refs = out
            .body_html
            .split(r#"<section class="references""#)
            .nth(1)
            .unwrap();
        let alpha_pos = refs.find(r#"data-key="Alpha2021""#).unwrap();
        let zulu_pos = refs.find(r#"data-key="Zulu2020""#).unwrap();
        assert!(alpha_pos < zulu_pos);
        assert!(refs
            .contains("Ann Alpha and Bob Beta. <em>Book of Things</em>. Press, New York, 2021."));
        assert!(
            refs.contains("Zoe Zulu. Later Result. <em>Journal of Tests</em>, 12(3):45–67, 2020.")
        );
    }

    #[test]
    fn floats_and_step_markers_do_not_dump_raw_latex() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\step[First moment]\nText.\n\\begin{figure}\n\\includegraphics[width=1in]{plot.pdf}\n\\caption{A useful plot at $T=1$.}\\label{fig:plot}\n\\end{figure}\nSee Figure~\\ref{fig:plot}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out
            .body_html
            .contains(r#"<strong>Step 1:</strong> First moment."#));
        assert!(out
            .body_html
            .contains(r#"class="float-placeholder float-figure""#));
        assert!(out.body_html.contains(r#"id="fig-plot""#));
        assert!(out
            .body_html
            .contains(r#"<span class="float-kind">Figure 1.</span>"#));
        assert!(out
            .body_html
            .contains(r##"data-target="fig:plot" data-kind="ref">1</a>"##));
        assert!(out.body_html.contains("A useful plot at "));
        assert!(out.body_html.contains(r#"data-tex="\(T=1\)""#));
        assert!(out
            .body_html
            .contains(r#"class="float-image float-pdf-preview""#));
        assert!(out.body_html.contains(r#"data-tex-options="width=1in""#));
        assert!(out
            .body_html
            .contains(r#"style="width: 1in; max-width: none; height: auto""#));
        assert!(out
            .body_html
            .contains(r#"src="/assets/plot.pdf?preview=png""#));
        assert!(out.body_html.contains(r#"href="/assets/plot.pdf""#));
        assert!(!out
            .body_html
            .contains(r#"<div class="opaque-env" data-env="figure""#));
        assert!(!out.body_html.contains(r#"\includegraphics"#));
    }

    #[test]
    fn includegraphics_textwidth_ratio_controls_image_width() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{figure}\n\\includegraphics[width=0.8\\textwidth]{plot.png}\n\\caption{Plot.}\\label{fig:plot}\n\\end{figure}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out
            .body_html
            .contains(r#"data-tex-options="width=0.8\textwidth""#));
        assert!(out
            .body_html
            .contains(r#"style="width: 80%; max-width: none; height: auto""#));
    }

    #[test]
    fn abstract_before_maketitle_renders_after_title() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\title{Paper Title}\n\\author{A. Author}\n\\begin{document}\n\\begin{abstract}\nSummary with $x$.\n\\end{abstract}\n\\maketitle\n\\section{Intro}\nText.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        let title_pos = out.body_html.find(r#"class="title-block""#).unwrap();
        let abstract_pos = out.body_html.find(r#"class="paper-abstract""#).unwrap();
        let section_pos = out.body_html.find(r#"class="sec-h2""#).unwrap();
        assert!(title_pos < abstract_pos);
        assert!(abstract_pos < section_pos);
        assert!(out.body_html.contains("<h2>Abstract</h2>"));
        assert!(out.body_html.contains(r#"data-tex="\(x\)""#));
    }

    #[test]
    fn subequation_group_labels_resolve_to_next_equation() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{subequations}\n\\label{eq:group}\n\\begin{equation}\n\\label{eq:first}\na=b\n\\end{equation}\n\\end{subequations}\nSee \\eqref{eq:group} and \\eqref{eq:first}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"id="eq-group""#));
        assert!(out.body_html.contains(r#"id="eq-first""#));
        assert!(out
            .body_html
            .contains(r##"href="#eq-group" data-target="eq:group" data-kind="eqref">(1)"##));
        assert!(out
            .body_html
            .contains(r##"href="#eq-first" data-target="eq:first" data-kind="eqref">(1)"##));
        assert!(!out.body_html.contains("(eq:group)"));
        assert!(!out.body_html.contains("(eq:first)"));
    }

    #[test]
    fn viewer_shell_contains_index_pages_and_a4_guides() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\section{Intro}\nText.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.html.contains(r#"id="viewer-side""#));
        assert!(out.html.contains(r#"id="side-toggle""#));
        assert!(out.html.contains(r#"data-side-tab="index""#));
        assert!(out.html.contains(r#"data-side-tab="pages""#));
        assert!(out.html.contains(r#"data-page-mode="a4""#));
        assert!(out.html.contains(r#"data-page-mode="dynamic""#));
        assert!(out.html.contains(r#"id="server-restart""#));
        assert!(out.html.contains(r#"id="server-stop""#));
        assert!(out.html.contains("setStopButtonMode"));
        assert!(out.html.contains("startServer"));
        assert!(out.html.contains("stopServer"));
        assert!(out.html.contains("manualStopRequested"));
        assert!(out.html.contains("fetch('/stop'"));
        assert!(out.html.contains("fetch('/?start='"));
        assert!(out.html.contains(r#"id="page-shell""#));
        assert!(out.html.contains("--page-scale"));
        assert!(out.html.contains("updatePageScale"));
        assert!(out.html.contains("A4_RATIO"));
        assert!(out.html.contains("page-guide-layer"));
        assert!(out.html.contains("copySelectionAsLatex"));
        assert!(out.html.contains("selectionIsExactNode"));
        assert!(out.html.contains("math-selected"));
        assert!(out.html.contains("document.addEventListener('mousedown'"));
        assert!(out.html.contains("syncReusedMathNode"));
        assert!(out.html.contains("copyAttr(oldEl, newEl, 'tabindex')"));
        assert!(!out.html.contains("user-select: all"));
    }
}
