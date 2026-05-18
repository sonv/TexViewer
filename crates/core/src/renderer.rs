//! AST → HTML renderer.
//!
//! Output is a single self-contained HTML document. Math nodes are emitted as
//! engine-neutral `<span class="math" data-tex="\(...\)" data-hash="...">`
//! markers; the active [`crate::engines::MathEngine`] (default: MathJax v3
//! SVG) typesets them in the browser. Swapping engines is a frontend bundle
//! swap and does not require changing the AST walk.

use std::fmt::Write;
use std::path::PathBuf;

use serde::Serialize;

use std::collections::HashMap;

use crate::ast::{ListKind, Node, NodeKind, Pos, RefKind, Role, Span};
use crate::bibtex::{BibEntry, BibStyle};
use crate::engines::Engine;
use crate::macros::ExtractedPreamble;
use crate::numbering::LabelTable;
use crate::sync::{SyncIndex, SyncKind};

#[derive(Debug, Clone)]
pub struct HtmlOptions {
    /// Math engine used to typeset math nodes in the browser. Default is
    /// [`crate::engines::MathJaxEngine`] pointed at the jsdelivr CDN.
    pub engine: Engine,
    /// Short document title — used both in `<title>` and as the bold label
    /// in the viewer topbar.
    pub title: String,
    /// Path to the root `.tex` file. Shown in the topbar next to the title
    /// (with `$HOME` shortened to `~`) so the reader can tell at a glance
    /// which file is being previewed. Populated automatically by
    /// `render_project` / `render_project_from_source`; the static
    /// `mathpreview-cli render` path leaves it unset.
    pub source_path: Option<PathBuf>,
    /// Whether to embed the default stylesheet inline. Off if you want to
    /// supply your own.
    pub inline_css: bool,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            engine: Engine::default(),
            title: "mathpreview".into(),
            source_path: None,
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
    pub src: Option<String>,
    pub source_anchors: Vec<SourceAnchor>,
    #[serde(skip)]
    pub diff_hash: String,
    pub html: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceAnchor {
    pub id: String,
    pub src: String,
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
        sidenote_counter: 0,
        source_anchors: Vec::new(),
    };

    // Top-level inline runs become paragraph blocks. Structural nodes
    // (sections, displays, theorem-likes, lists, etc.) stay as their own
    // blocks. The wrapper uses `display: contents` in CSS so it doesn't affect
    // visual layout — it exists purely so the diff/patch path can find and
    // replace blocks by id.
    let mut blocks: Vec<RenderedBlock> = Vec::with_capacity(nodes.len());
    let mut paragraph = ParagraphState::default();
    let mut previous_block_was_display = false;
    let mut blank_after_display = false;
    let ordered = front_matter_order(nodes);
    for (i, node) in ordered.iter().enumerate() {
        if is_blank_separator_node(node) {
            flush_paragraph(&mut blocks, &mut paragraph, &mut ctx);
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
            flush_paragraph(&mut blocks, &mut paragraph, &mut ctx);
            paragraph.start = Some(i);
            extend_paragraph_span(&mut paragraph.span, node.span.clone());
            paragraph.force_indent = false;
            paragraph.no_indent = true;
            paragraph.flow_marker = true;
            paragraph.trim_after_flow_marker = true;
            write_node(&mut paragraph.html, node, &mut ctx);
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
                        ParagraphTextPart::Text {
                            text: segment,
                            start,
                        } => {
                            let mut text = if paragraph.html.trim().is_empty()
                                || paragraph.trim_after_flow_marker
                            {
                                trim_leading_paragraph_space(segment)
                            } else {
                                segment
                            };
                            let mut text_start = start + (segment.len() - text.len());
                            if text.is_empty() {
                                continue;
                            }
                            if paragraph.start.is_none() {
                                paragraph.start = Some(i);
                                paragraph.span = Some(text_segment_span(
                                    &node.span,
                                    s,
                                    start,
                                    start + segment.len(),
                                ));
                                paragraph.force_indent =
                                    previous_block_was_display && blank_after_display;
                                paragraph.no_indent = false;
                                paragraph.flow_marker = false;
                            }
                            extend_paragraph_span(
                                &mut paragraph.span,
                                text_segment_span(&node.span, s, start, start + segment.len()),
                            );
                            if !paragraph.html.trim().is_empty()
                                && !starts_with_blank_line(text)
                                && text.starts_with(char::is_whitespace)
                                && !paragraph.html.ends_with(char::is_whitespace)
                            {
                                paragraph.html.push(' ');
                                let trimmed = trim_leading_paragraph_space(text);
                                text_start += text.len() - trimmed.len();
                                text = trimmed;
                            }
                            let text_span =
                                text_segment_span(&node.span, s, text_start, start + segment.len());
                            write_text_with_span(&mut paragraph.html, text, &text_span, &mut ctx);
                            paragraph.trim_after_flow_marker = false;
                            previous_block_was_display = false;
                            blank_after_display = false;
                        }
                        ParagraphTextPart::Break { .. } => {
                            flush_paragraph(&mut blocks, &mut paragraph, &mut ctx);
                            if previous_block_was_display {
                                blank_after_display = true;
                            }
                        }
                    }
                }
                continue;
            }
            if paragraph.start.is_none() {
                paragraph.start = Some(i);
                paragraph.span = Some(node.span.clone());
                paragraph.force_indent = previous_block_was_display && blank_after_display;
                paragraph.no_indent = false;
                paragraph.flow_marker = false;
            }
            extend_paragraph_span(&mut paragraph.span, node.span.clone());
            write_node(&mut paragraph.html, node, &mut ctx);
            paragraph.trim_after_flow_marker = false;
            previous_block_was_display = false;
            blank_after_display = false;
            continue;
        }

        flush_paragraph(&mut blocks, &mut paragraph, &mut ctx);
        let mut inner = String::new();
        write_node(&mut inner, node, &mut ctx);
        // Skip empty emissions (e.g. discarded comments, no-op opaque cmds)
        // so we don't pollute the block sequence with phantoms whose hash
        // would still match across renders but waste id space.
        if inner.trim().is_empty() {
            ctx.source_anchors.clear();
            continue;
        }
        push_block(&mut blocks, i, inner, Some(&node.span), &mut ctx);
        previous_block_was_display = matches!(&node.kind, NodeKind::DisplayMath { .. });
        blank_after_display = false;
    }
    flush_paragraph(&mut blocks, &mut paragraph, &mut ctx);

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

#[derive(Default)]
struct ParagraphState {
    html: String,
    start: Option<usize>,
    span: Option<Span>,
    force_indent: bool,
    no_indent: bool,
    flow_marker: bool,
    trim_after_flow_marker: bool,
}

fn flush_paragraph(
    blocks: &mut Vec<RenderedBlock>,
    paragraph: &mut ParagraphState,
    ctx: &mut RenderCtx<'_>,
) {
    let Some(start) = paragraph.start.take() else {
        return;
    };
    let span = paragraph.span.take();
    if paragraph.html.trim().is_empty() {
        paragraph.html.clear();
        paragraph.force_indent = false;
        paragraph.no_indent = false;
        paragraph.flow_marker = false;
        paragraph.trim_after_flow_marker = false;
        return;
    }
    let mut classes = vec!["para"];
    if paragraph.force_indent && !paragraph.no_indent {
        classes.push("para-indent");
    }
    if paragraph.no_indent {
        classes.push("para-noindent");
    }
    if paragraph.flow_marker {
        classes.push("para-flow");
    }
    let class = classes.join(" ");
    let inner = format!(
        r#"<p class="{class}">{}</p>"#,
        std::mem::take(&mut paragraph.html)
    );
    paragraph.force_indent = false;
    paragraph.no_indent = false;
    paragraph.flow_marker = false;
    paragraph.trim_after_flow_marker = false;
    push_block(blocks, start, inner, span.as_ref(), ctx);
}

fn push_block(
    blocks: &mut Vec<RenderedBlock>,
    _index: usize,
    inner: String,
    span: Option<&Span>,
    ctx: &mut RenderCtx<'_>,
) {
    let id = format!("blk-{}", blocks.len() + 1);
    let hash = fnv_hash(&inner);
    let diff_hash = fnv_hash(&stable_block_diff_source(&inner));
    let source_anchors = std::mem::take(&mut ctx.source_anchors);
    let src_value = span.map(data_src);
    let src_attr = src_value
        .as_deref()
        .map(|src| format!(r#" data-src="{}""#, escape_attr(src)))
        .unwrap_or_default();
    if let Some(span) = span {
        record_sync(ctx, &id, span, None, SyncKind::Container);
    }
    let html = format!(
        r#"<article class="blk" id="{id}" data-blockhash="{hash}"{src}>{inner}</article>"#,
        id = id,
        hash = hash,
        src = src_attr,
        inner = inner,
    );
    blocks.push(RenderedBlock {
        id,
        hash,
        src: src_value,
        source_anchors,
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
    const PREFIXES: [&str; 11] = [
        r#" id="im-"#,
        r#" id="dm-"#,
        r#" id="eq-"#,
        r#" id="sec-"#,
        r#" id="thm-"#,
        r#" id="proof-"#,
        r#" id="fn-"#,
        r#" id="ref-"#,
        r#" id="cite-"#,
        r#" id="srcs-"#,
        r#" id="srcw-"#,
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
    Text { text: &'a str, start: usize },
    Break { start: usize, end: usize },
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
                    parts.push(ParagraphTextPart::Text {
                        text: &s[start..i],
                        start,
                    });
                }
                parts.push(ParagraphTextPart::Break { start: i, end });
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
        parts.push(ParagraphTextPart::Text {
            text: &s[start..],
            start,
        });
    }
    parts
}

fn text_segment_span(node_span: &Span, text: &str, start: usize, end: usize) -> Span {
    Span {
        file: node_span.file.clone(),
        start: offset_pos(text, node_span.start, start),
        end: offset_pos(text, node_span.start, end),
    }
}

fn paragraph_break_span(node_span: &Span, text: &str, start: usize, end: usize) -> Span {
    let mut anchor_start = start;
    let bytes = text.as_bytes();
    if bytes.get(anchor_start) == Some(&b'\n') {
        anchor_start += 1;
    }
    let mut anchor_end = anchor_start;
    while anchor_end < end && anchor_end < bytes.len() {
        match bytes[anchor_end] {
            b'\n' | b'\r' => break,
            _ => anchor_end += 1,
        }
    }
    text_segment_span(node_span, text, anchor_start, anchor_end)
}

fn soft_line_break_span(node_span: &Span, text: &str, start: usize, end: usize) -> Option<Span> {
    let bytes = text.as_bytes();
    let newline = bytes
        .get(start..end)
        .and_then(|slice| slice.iter().rposition(|b| *b == b'\n'))
        .map(|offset| start + offset)?;
    let anchor_start = newline + 1;
    let mut anchor_end = anchor_start;
    while anchor_end < end && anchor_end < bytes.len() {
        match bytes[anchor_end] {
            b'\n' | b'\r' => break,
            _ => anchor_end += 1,
        }
    }
    Some(text_segment_span(node_span, text, anchor_start, anchor_end))
}

fn offset_pos(src: &str, start: Pos, byte: usize) -> Pos {
    let mut line = start.line;
    let mut col = start.col;
    for ch in src[..byte.min(src.len())].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    Pos {
        line,
        col,
        byte: start.byte + byte as u32,
    }
}

fn extend_paragraph_span(current: &mut Option<Span>, span: Span) {
    match current {
        Some(existing) if existing.file == span.file => {
            if span_ends_after(span.end, existing.end) {
                existing.end = span.end;
            }
        }
        Some(_) => {}
        None => *current = Some(span),
    }
}

fn span_ends_after(candidate: Pos, current: Pos) -> bool {
    candidate.line > current.line || (candidate.line == current.line && candidate.col > current.col)
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

fn write_text_with_span(out: &mut String, s: &str, span: &Span, ctx: &mut RenderCtx<'_>) {
    if is_blank_line_separator(s) {
        out.push_str(r#"<div class="para-break" aria-hidden="true"></div>"#);
    } else {
        out.push_str(&render_inline_latex_with_source_spans(s, span, ctx));
    }
}

fn write_source_space_anchor(out: &mut String, span: &Span, ctx: &mut RenderCtx<'_>) {
    let id = ctx.idgen.next("srcs");
    record(ctx, &id, span, None);
    write!(
        out,
        r#"<span class="source-space" id="{id}" data-src="{src}" aria-hidden="true"></span>"#,
        id = escape_attr(&id),
        src = escape_attr(&data_src(span)),
    )
    .unwrap();
}

fn render_inline_latex_with_source_spans(s: &str, span: &Span, ctx: &mut RenderCtx<'_>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let Some(ch) = s[i..].chars().next() else {
            break;
        };
        if ch.is_whitespace() {
            let start = i;
            i += ch.len_utf8();
            while i < s.len() {
                let Some(next) = s[i..].chars().next() else {
                    break;
                };
                if !next.is_whitespace() {
                    break;
                }
                i += next.len_utf8();
            }
            let had_output = !out.is_empty();
            let ended_with_whitespace = out.ends_with(char::is_whitespace);
            if let Some(space_span) = soft_line_break_span(span, s, start, i) {
                write_source_space_anchor(&mut out, &space_span, ctx);
            }
            let rendered = render_inline_latex(&s[start..i], ctx.labels);
            if rendered.is_empty() {
                if had_output && !ended_with_whitespace {
                    out.push(' ');
                }
            } else {
                out.push_str(&rendered);
            }
            continue;
        }

        let token_end = if ch == '\\' {
            latex_source_token_end(s, i).unwrap_or_else(|| i + ch.len_utf8())
        } else if is_source_word_char(ch) {
            source_word_end(s, i)
        } else {
            let start = i;
            i += ch.len_utf8();
            out.push_str(&render_inline_latex(&s[start..i], ctx.labels));
            continue;
        };

        let token = &s[i..token_end];
        let rendered = render_inline_latex(token, ctx.labels);
        if !rendered.is_empty() {
            let token_span = span_for_text_range(span, s, i, token_end);
            let id = ctx.idgen.next("srcw");
            record(ctx, &id, &token_span, None);
            write!(
                out,
                r#"<span class="src-word" id="{id}" data-src="{src}">{rendered}</span>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&token_span)),
                rendered = rendered,
            )
            .unwrap();
        }
        i = token_end;
    }
    out
}

fn source_word_end(s: &str, start: usize) -> usize {
    let mut i = start;
    while i < s.len() {
        let Some(ch) = s[i..].chars().next() else {
            break;
        };
        if !is_source_word_char(ch) {
            break;
        }
        i += ch.len_utf8();
    }
    i
}

fn is_source_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '\'' | '’' | '-')
}

fn latex_source_token_end(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'\\') {
        return None;
    }
    let mut i = start + 1;
    if i >= bytes.len() {
        return Some(i);
    }
    if bytes[i].is_ascii_alphabetic() {
        while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'*') {
            i += 1;
        }
        loop {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() && bytes[i] != b'\n' {
                i += 1;
            }
            let group = match bytes.get(i).copied() {
                Some(b'[') => balanced_group_end(s, i, b'[', b']'),
                Some(b'{') => balanced_group_end(s, i, b'{', b'}'),
                _ => None,
            };
            let Some(end) = group else {
                break;
            };
            i = end;
        }
        return Some(i);
    }
    let punct = s[i..].chars().next()?;
    i += punct.len_utf8();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() && bytes[i] != b'\n' {
        i += 1;
    }
    if bytes.get(i) == Some(&b'{') {
        return balanced_group_end(s, i, b'{', b'}');
    }
    if i < s.len() {
        let arg = s[i..].chars().next()?;
        i += arg.len_utf8();
    }
    Some(i)
}

fn span_for_text_range(span: &Span, text: &str, start: usize, end: usize) -> Span {
    Span {
        file: span.file.clone(),
        start: offset_pos(text, span.start, start),
        end: offset_pos(text, span.start, end),
    }
}

fn wrap_in_shell(body: &str, preamble: &ExtractedPreamble, opts: &HtmlOptions) -> String {
    let engine = opts.engine.as_dyn();
    let engine_head = engine.head_html(preamble);
    let engine_adapter_js = engine.client_adapter_js();
    let engine_css = engine.extra_css();
    let warnings_html = warnings_panel(preamble);
    let css = if opts.inline_css { DEFAULT_CSS } else { "" };

    let mut out = String::new();
    // <head><title> uses the file stem (opts.title) so the browser tab
    // is never blank.
    let head_title = escape_html(&opts.title);
    // The topbar's bold short-title slot is intentionally blank unless
    // the LaTeX source provides `\title[short]{long}`. Authors who want
    // a label in the topbar opt in via that optional argument.
    let topbar_short = preamble
        .title_short
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let topbar_title_html = match topbar_short {
        Some(s) => format!(
            r#"<strong class="topbar-doc-title">{s}</strong>"#,
            s = escape_html(s),
        ),
        None => String::new(),
    };
    let path_html = match opts.source_path.as_ref() {
        Some(p) => {
            let full = p.display().to_string();
            let short = shorten_home_path(p);
            format!(
                r#"<span class="topbar-doc-path" title="{full}">{short}</span>"#,
                full = escape_attr(&full),
                short = escape_html(&short),
            )
        }
        None => String::new(),
    };
    write!(
        out,
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{head_title}</title>
<style>{css}{engine_css}</style>
{engine_head}
</head>
<body class="page-mode-a4">
<header class="topbar">
  <div class="topbar-doc">
    {topbar_title_html}
    {path_html}
  </div>
  <span class="status" id="ws-status" title="live-reload status"></span>
  <span class="topbar-spacer"></span>
  <button class="side-toggle" id="side-toggle" type="button" aria-controls="viewer-side" aria-expanded="false" title="toggle index and pages pane">toc</button>
  <span class="page-mode-toggle" data-page-mode="a4">
    <button data-page-mode="a4" class="active" type="button">A4</button>
    <button data-page-mode="dynamic" type="button">dynamic</button>
  </span>
  <button class="refkey-toggle" id="refkey-toggle" type="button" aria-pressed="false" title="toggle LaTeX refkeys">keys</button>
  <button class="margin-toggle" id="margin-toggle" type="button" aria-pressed="false" title="toggle margin reference cards (click \\ref / \\cite to pin)">margin</button>
  <button class="server-restart" id="server-restart" type="button" title="restart preview server">restart</button>
  <button class="server-stop" id="server-stop" type="button" title="stop preview server">stop</button>
  <span class="proof-toggle" data-mode="all">
    <button data-mode="main">main only</button>
    <button data-mode="supporting">+ supporting</button>
    <button data-mode="all" class="active">all</button>
  </span>
  <!-- The topbar hide/show toggle lives as a thin stripe on the left edge
       of the viewport (see #topbar-stripe below) so it stays reachable
       when the margin column covers the right side of the screen. -->
</header>
<button class="topbar-stripe" id="topbar-stripe" type="button" aria-expanded="true" aria-controls="topbar-banner" title="toggle top banner"></button>
<div class="search-panel" id="search-panel" hidden>
  <label for="search-input">/</label>
  <input id="search-input" type="search" autocomplete="off" spellcheck="false" placeholder="search">
  <span class="search-help">Enter next · Shift+Enter previous · Esc close</span>
</div>
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
  <main id="page" data-proof-mode="all" data-refkeys="hidden">
{body}
  </main>
</div>
<aside id="margin"></aside>
<script>
{engine_adapter_js}
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
    sidenote_counter: usize,
    source_anchors: Vec<SourceAnchor>,
}

fn record(ctx: &mut RenderCtx, id: &str, span: &Span, label: Option<&str>) {
    record_with_kind(ctx, id, span, label, SyncKind::Leaf);
}

fn record_container(ctx: &mut RenderCtx, id: &str, span: &Span, label: Option<&str>) {
    record_with_kind(ctx, id, span, label, SyncKind::Container);
}

fn record_with_kind(
    ctx: &mut RenderCtx,
    id: &str,
    span: &Span,
    label: Option<&str>,
    kind: SyncKind,
) {
    record_sync(ctx, id, span, label, kind);
    ctx.source_anchors.push(SourceAnchor {
        id: id.to_string(),
        src: data_src(span),
    });
}

fn record_sync(ctx: &mut RenderCtx, id: &str, span: &Span, label: Option<&str>, kind: SyncKind) {
    ctx.sync.record_with_kind(
        id.to_string(),
        span.file.clone(),
        span.start,
        span.end,
        label.map(str::to_string),
        kind,
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

fn refkey_attr(label: Option<&str>) -> String {
    label
        .map(|label| format!(r#" data-refkey="{}""#, escape_attr(label)))
        .unwrap_or_default()
}

fn equation_number_html(number: Option<&str>, row_numbers: &[Option<String>]) -> String {
    if row_numbers.is_empty() {
        return number
            .map(|n| format!(r#"<span class="eq-num">({})</span>"#, escape_html(n)))
            .unwrap_or_default();
    }

    let mut out = String::from(r#"<span class="eq-num-list" aria-hidden="true">"#);
    for row in row_numbers {
        match row {
            Some(n) => write!(
                out,
                r#"<span class="eq-num-row">({})</span>"#,
                escape_html(n)
            )
            .unwrap(),
            None => out.push_str(r#"<span class="eq-num-row empty"></span>"#),
        }
    }
    out.push_str("</span>");
    out
}

fn equation_row_refkey_html(
    body: &str,
    primary: Option<&str>,
    row_numbers: &[Option<String>],
) -> String {
    if row_numbers.is_empty() {
        return String::new();
    }

    let labels_by_row = math_row_labels(body);
    if labels_by_row.iter().all(Vec::is_empty) {
        return String::new();
    }

    let row_count = row_numbers.len().max(labels_by_row.len());
    let mut out = String::from(r#"<span class="eq-refkey-list" aria-hidden="true">"#);
    for row_index in 0..row_count {
        out.push_str(r#"<span class="eq-refkey-row">"#);
        if let Some(labels) = labels_by_row.get(row_index) {
            for label in labels {
                let id_attr = if primary == Some(label.as_str()) {
                    String::new()
                } else {
                    format!(r#" id="{}""#, escape_attr(&sanitize_id(label)))
                };
                write!(
                    out,
                    r#"<span class="eq-refkey-chip"{id}>{label}</span>"#,
                    id = id_attr,
                    label = escape_html(label),
                )
                .unwrap();
            }
        }
        out.push_str("</span>");
    }
    out.push_str("</span>");
    out
}

fn math_row_labels(body: &str) -> Vec<Vec<String>> {
    let mut seen = Vec::<String>::new();
    split_math_rows(body)
        .into_iter()
        .map(|row| {
            let mut row_labels = Vec::new();
            for label in latex_command_args(row, "label") {
                if seen.iter().any(|existing| existing == &label) {
                    continue;
                }
                seen.push(label.clone());
                row_labels.push(label);
            }
            row_labels
        })
        .collect()
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
            write_text_with_span(out, s, &n.span, ctx);
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
                r#"<h{h} id="{id}" class="sec-h{level}" data-src="{src}"{refkey}>{num}{title}</h{h}>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                refkey = refkey_attr(label.as_deref()),
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
            record_container(ctx, &id, &n.span, label.as_deref());
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
                r#"<div class="thm {env_class} {role_class}" id="{id}" data-src="{src}"{refkey}>"#,
                env_class = format_args!("env-{env}"),
                role_class = role_class,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                refkey = refkey_attr(label.as_deref()),
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
            record_container(ctx, &id, &n.span, None);
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
            let rendered_math = resolve_math_refs(s, ctx.labels);
            let hash = fnv_hash(&format!("i:{rendered_math}"));
            let copy_tex = format!(r"\({s}\)");
            // Use \( \) so MathJax doesn't typeset the literal `$` text.
            write!(
                out,
                r#"<span class="math inline" id="{id}" data-src="{src}" data-hash="{hash}" data-tex="{copy_tex}" tabindex="0" title="Copy as LaTeX">\({}\)</span>"#,
                escape_math(&rendered_math),
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
            row_numbers,
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
            let body_rendered = resolve_math_refs(&body_clean, ctx.labels);
            let math = match env {
                Some(e) => format!(r"\begin{{{e}}}{}\end{{{e}}}", body_rendered),
                None => format!(r"\[{}\]", body_rendered),
            };
            let copy_tex = match env {
                Some(e) => format!(r"\begin{{{e}}}{}\end{{{e}}}", body),
                None => format!(r"\[{}\]", body),
            };
            let row_refkey_html = equation_row_refkey_html(body, label.as_deref(), row_numbers);
            let alias_html = if row_numbers.is_empty() {
                label_alias_anchors(body, label.as_deref())
            } else {
                String::new()
            };
            let num_html = equation_number_html(number.as_deref(), row_numbers);
            let label_fingerprint = latex_command_args(body, "label").join("\u{1f}");
            let hash = fnv_hash(&format!(
                "d:{}:{}:{:?}:{}:{}",
                env.as_deref().unwrap_or("[]"),
                number.as_deref().unwrap_or(""),
                row_numbers,
                label_fingerprint,
                math,
            ));
            writeln!(
                out,
                r#"<div class="math display" id="{id}" data-src="{src}"{refkey} data-hash="{hash}" data-tex="{copy_tex}" tabindex="0" title="Copy as LaTeX">{aliases}{row_refkeys}{math}{num_html}</div>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                refkey = if row_numbers.is_empty() { refkey_attr(label.as_deref()) } else { String::new() },
                aliases = alias_html,
                row_refkeys = row_refkey_html,
                math = escape_math(&math),
                hash = hash,
                copy_tex = escape_attr(&copy_tex),
            ).unwrap();
        }
        NodeKind::Subequations { label, number: _ } => {
            if let Some(label) = label {
                let id = sanitize_id(label);
                record_container(ctx, &id, &n.span, Some(label));
                write!(
                    out,
                    r#"<span class="label-anchor" id="{}" data-src="{}" data-refkey="{}"></span>"#,
                    escape_attr(&id),
                    escape_attr(&data_src(&n.span)),
                    escape_attr(label)
                )
                .unwrap();
            }
            write_children(out, &n.children, ctx);
        }
        NodeKind::Ref { kind, key } => {
            let id = ctx.idgen.next("ref");
            record(ctx, &id, &n.span, None);
            let target = sanitize_id(key);
            let label = ctx.labels.resolve_ref(*kind, key);
            write!(
                out,
                r##"<a class="ref" id="{id}" data-src="{src}" href="#{target}" data-target="{key}" data-kind="{kind_str}">{label}</a>"##,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
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
            let id = ctx.idgen.next("cite");
            record(ctx, &id, &n.span, None);
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
            write!(
                out,
                r#"<span class="cite-group" id="{id}" data-src="{src}">{l}{}{r}</span>"#,
                parts.join("; "),
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
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
                            r#"<span class="label-anchor" id="{}" data-refkey="{}"></span>"#,
                            escape_attr(&sanitize_id(&label)),
                            escape_attr(&label)
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
                // Sidenote / review-annotation commands.
                //
                // `\sidenote[opts]{text}` itself, plus any user-defined
                // wrapper detected at preamble time (svmacro.sty's `\SV`,
                // `\AB`, and any author-written `\GI`-shaped wrapper that
                // expands to `\sidenote[...]{...}`). Rendered as a small
                // collapsible chip — closed shows just the marker, open
                // reveals the content. Embedded math / refs / emphasis in
                // the content are re-parsed via `render_latex_text_with_math`.
                "sidenote" => {
                    if let Some(call) = latex_command_call(raw, "sidenote") {
                        ctx.sidenote_counter += 1;
                        let id = format!("sn-{}", ctx.sidenote_counter);
                        let content_id = format!("{id}-content");
                        let content = render_latex_text_with_math(&call.arg, ctx.labels);
                        // Register the chip in the sync index so editor cursor
                        // movement within the `\sidenote{...}` source range
                        // highlights / scrolls to the rendered chip.
                        record_container(ctx, &id, &n.span, None);
                        let src_attr = data_src(&n.span);
                        write!(
                            out,
                            r#"<span class="sidenote sidenote-note" id="{id}" data-src="{src_attr}" data-label="note"><button class="sidenote-marker" type="button" aria-expanded="false" aria-controls="{content_id}">note</button><span class="sidenote-content" id="{content_id}" hidden>{content}</span></span>"#,
                        )
                        .unwrap();
                    }
                }
                _ if ctx.preamble.sidenote_wrappers.iter().any(|w| w == name) => {
                    // User wrapper around `\sidenote`. Optional bracket arg
                    // becomes the chip's secondary label (typical author
                    // pattern: `\SV[2025-05-18]{...}` for a dated review
                    // note). The required brace arg is the body.
                    if let Some(call) = latex_command_call(raw, name) {
                        ctx.sidenote_counter += 1;
                        let id = format!("sn-{}", ctx.sidenote_counter);
                        let kind = name.to_lowercase();
                        let label = match call.optional.as_deref().map(str::trim) {
                            Some(opt) if !opt.is_empty() => format!("{name} {opt}"),
                            _ => name.to_string(),
                        };
                        let label_attr = escape_attr(&label);
                        let label_html = escape_html(&label);
                        let content_id = format!("{id}-content");
                        let content = render_latex_text_with_math(&call.arg, ctx.labels);
                        // Register the chip in the sync index so editor cursor
                        // movement within the `\SV{...}` / `\AB{...}` source
                        // range highlights / scrolls to the rendered chip.
                        record_container(ctx, &id, &n.span, None);
                        let src_attr = data_src(&n.span);
                        write!(
                            out,
                            r#"<span class="sidenote sidenote-{kind}" id="{id}" data-src="{src_attr}" data-label="{label_attr}"><button class="sidenote-marker" type="button" aria-expanded="false" aria-controls="{content_id}">{label_html}</button><span class="sidenote-content" id="{content_id}" hidden>{content}</span></span>"#,
                        )
                        .unwrap();
                    }
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
                    ParagraphTextPart::Text {
                        text: segment,
                        start,
                    } => {
                        let starts_blank_line = starts_with_blank_line(segment);
                        let mut text = if trim_next_text {
                            trim_leading_paragraph_space(segment)
                        } else {
                            segment
                        };
                        let mut text_start = start + (segment.len() - text.len());
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
                            let trimmed = trim_leading_paragraph_space(text);
                            text_start += text.len() - trimmed.len();
                            text = trimmed;
                        }
                        let text_span =
                            text_segment_span(&child.span, s, text_start, start + segment.len());
                        write_text_with_span(out, text, &text_span, ctx);
                        trim_next_text = false;
                        previous_was_display = false;
                        pending_paragraph_indent = false;
                        seen_content = true;
                    }
                    ParagraphTextPart::Break { start, end } => {
                        let break_span = paragraph_break_span(&child.span, s, start, end);
                        if seen_content || previous_was_display {
                            out.push_str(r#"<span class="para-break" aria-hidden="true"></span>"#);
                        }
                        write_source_space_anchor(out, &break_span, ctx);
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

fn resolve_math_refs(body: &str, labels: &LabelTable) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_alphabetic() {
            let cmd_start = i + 1;
            let mut cmd_end = cmd_start;
            while cmd_end < bytes.len() && bytes[cmd_end].is_ascii_alphabetic() {
                cmd_end += 1;
            }
            let name = &body[cmd_start..cmd_end];
            if let Some(kind) = math_ref_kind(name) {
                let mut arg_start = cmd_end;
                while arg_start < bytes.len() && bytes[arg_start].is_ascii_whitespace() {
                    arg_start += 1;
                }
                if let Some((key, next)) = balanced_arg_at(body, arg_start) {
                    let resolved = labels.resolve_ref(kind, key.trim());
                    if matches!(kind, RefKind::Cref | RefKind::Autoref | RefKind::Nameref) {
                        out.push_str(r"\text{");
                        out.push_str(&escape_tex_text(&resolved));
                        out.push('}');
                    } else {
                        out.push_str(&resolved);
                    }
                    i = next;
                    continue;
                }
            }
        }

        let ch = body[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn math_ref_kind(name: &str) -> Option<RefKind> {
    match name {
        "ref" => Some(RefKind::Ref),
        "eqref" => Some(RefKind::Eqref),
        "pageref" => Some(RefKind::Pageref),
        "cref" | "Cref" => Some(RefKind::Cref),
        "autoref" => Some(RefKind::Autoref),
        "nameref" => Some(RefKind::Nameref),
        _ => None,
    }
}

fn balanced_arg_at(src: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'{') {
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
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&src[start + 1..i], i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn escape_tex_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' | '{' | '}' | '$' | '%' | '#' | '&' | '_' | '^' => {
                out.push('\\');
                out.push(ch);
            }
            '~' => out.push_str(r"\textasciitilde{}"),
            _ => out.push(ch),
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
    let refkey = refkey_attr(primary_label);
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
        r#"<figure class="float-placeholder float-{env}"{id}{refkey} data-env="{env}">{aliases}{asset}<figcaption><span class="float-kind">{kind_label}</span> {caption}</figcaption></figure>"#,
        env = escape_attr(env),
        id = id_attr,
        refkey = refkey,
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
            r#"<span class="label-anchor" id="{}" data-refkey="{}"></span>"#,
            escape_attr(&sanitize_id(&label)),
            escape_attr(&label)
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

fn split_math_rows(src: &str) -> Vec<&str> {
    let bytes = src.as_bytes();
    let mut rows = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut brace_depth = 0i32;
    let mut env_depth = 0i32;

    while i < bytes.len() {
        if bytes[i] == b'%' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        if src[i..].starts_with("\\begin") {
            if let Some(end) = latex_env_command_end(src, i + "\\begin".len()) {
                env_depth += 1;
                i = end;
                continue;
            }
        }
        if src[i..].starts_with("\\end") {
            if let Some(end) = latex_env_command_end(src, i + "\\end".len()) {
                if env_depth > 0 {
                    env_depth -= 1;
                }
                i = end;
                continue;
            }
        }

        match bytes[i] {
            b'\\' if i + 1 < bytes.len() && bytes[i + 1] == b'\\' => {
                if brace_depth == 0 && env_depth == 0 {
                    rows.push(src[start..i].trim());
                    i += 2;
                    i = skip_row_separator_spacing(src, i);
                    start = i;
                    continue;
                }
                i += 2;
                continue;
            }
            b'\\' if i + 1 < bytes.len() => {
                i += 2;
                continue;
            }
            b'{' => brace_depth += 1,
            b'}' if brace_depth > 0 => brace_depth -= 1,
            _ => {}
        }

        let ch = src[i..].chars().next().unwrap_or('\0');
        i += ch.len_utf8();
    }

    rows.push(src[start..].trim());
    rows
}

fn latex_env_command_end(src: &str, mut i: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'}' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

fn skip_row_separator_spacing(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    let before_ws = i;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'[') {
        return before_ws;
    }

    let mut j = i + 1;
    let mut depth = 1i32;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' if j + 1 < bytes.len() => {
                j += 2;
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    before_ws
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

/// Replace the user's `$HOME` prefix with `~` for display in the topbar.
/// Falls back to the full path if `$HOME` is unset or the file lives
/// outside it.
fn shorten_home_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::Path::new(&home);
        if let Ok(rel) = path.strip_prefix(home) {
            let mut out = String::from("~/");
            out.push_str(&rel.display().to_string());
            return out;
        }
    }
    path.display().to_string()
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

/// Client-side script. Wires up:
///   * Event-delegated proof-toggle and proof-head click handlers (so they
///     keep working after `#page` content is swapped by the WebSocket update).
///   * A WebSocket connection to the same host that pushes `body-updated`
///     events with new `#page` HTML. After swapping, the active engine
///     re-typesets via `window.__mpEngine`.
///
/// Math-engine calls go through the `window.__mpEngine` shim injected by
/// [`crate::engines::MathEngine::client_adapter_js`] so this bundle stays
/// engine-neutral.
///
/// When the page is loaded statically (CLI `render` output, no server), the
/// WebSocket fails silently and the page works as a static document.
const CLIENT_JS: &str = include_str!("assets/client.js");

const DEFAULT_CSS: &str = include_str!("assets/default.css");

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::HtmlOptions;

    fn display_math_hash(html: &str) -> &str {
        let display = html.find(r#"class="math display""#).unwrap();
        let hash_start = html[display..].find(r#"data-hash=""#).unwrap() + display + 11;
        let hash_end = html[hash_start..].find('"').unwrap() + hash_start;
        &html[hash_start..hash_end]
    }

    fn text_content(html: &str) -> String {
        let mut out = String::with_capacity(html.len());
        let mut in_tag = false;
        for ch in html.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(ch),
                _ => {}
            }
        }
        out
    }

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
        let text = text_content(&out.body_html);
        assert!(text.contains("First paragraph."));
        assert!(text.contains("Second paragraph."));
        assert!(!out.body_html.contains("<br><br>Second paragraph."));
    }

    #[test]
    fn paragraph_blocks_are_source_sync_targets() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "First paragraph.\n\nSecond paragraph.\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out
            .body_html
            .contains(r#"<article class="blk" id="blk-1" data-blockhash=""#));
        assert!(out.body_html.contains(r#"data-src="t.tex:1:1""#));
        assert!(out.body_html.contains(r#"id="blk-2""#));
        assert!(out.body_html.contains(r#"data-src="t.tex:3:1""#));
        let block_entry = out
            .sync
            .entries
            .iter()
            .find(|entry| entry.element_id == "blk-2")
            .expect("source sync entry");
        assert_eq!(block_entry.start.line, 3);
        let word_entry = out
            .sync
            .lookup_by_source_position(Path::new("t.tex"), 3, 2)
            .expect("word sync entry");
        assert!(word_entry.element_id.starts_with("srcw-"));
        assert!(out.body_html.contains(r#"class="src-word""#));
        assert!(out.blocks[1]
            .source_anchors
            .iter()
            .any(|anchor| anchor.id == word_entry.element_id));
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
        assert!(out.body_html.contains(r#"<p class="para para-indent">"#));
        assert!(text_content(&out.body_html).contains("Next line."));
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
        assert!(out.body_html.contains(r#"<p class="para">"#));
        assert!(text_content(&out.body_html).contains("Next line."));
        assert!(!out.body_html.contains(r#"<p class="para para-indent">"#));
    }

    #[test]
    fn single_newline_after_inline_math_keeps_interword_space() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nBefore $x$\nand after.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        let text = text_content(&out.body_html);
        assert!(text.contains(r"\(x\) and after."));
        assert!(!text.contains(r"\(x\)and after."));
    }

    #[test]
    fn single_newline_before_inline_math_keeps_interword_space() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nSince $B$ is first order in $x$, the function\n$v\\cdot\\grad_x(B\\psi)$ contains at most two $x$-derivatives.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        let text = text_content(&out.body_html);
        assert!(text.contains(r"the function \(v\cdot\grad_x(B\psi)\) contains"));
        assert!(!text.contains(r"the function\(v\cdot\grad_x(B\psi)\)"));
    }

    #[test]
    fn soft_line_break_inside_paragraph_is_a_source_sync_target() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nSince $B$ is first order in $x$, the function\n  $v\\cdot\\grad_x(B\\psi)$ contains at most two.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        let entry = out
            .sync
            .lookup_leaf_by_source_position(Path::new("t.tex"), 3, 1)
            .expect("soft line break sync entry");
        assert!(entry.element_id.starts_with("srcs-"));
        assert!(out.body_html.contains(r#"class="source-space""#));
        assert!(out
            .blocks
            .iter()
            .flat_map(|block| block.source_anchors.iter())
            .any(|anchor| anchor.id == entry.element_id));
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
    fn labeled_items_store_refkeys_for_viewer_toggle() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\section{Intro}\\label{sec:intro}\n\\begin{theorem}[role=main]\\label{thm:main}\nStatement.\n\\end{theorem}\n\\begin{equation}\n\\label{eq:main}\na=b\n\\label{eq:alias}\n\\end{equation}\n\\begin{figure}\n\\caption{Plot.}\\label{fig:plot}\n\\end{figure}\nLoose\\label{misc:loose}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"data-refkey="sec:intro""#));
        assert!(out.body_html.contains(r#"data-refkey="thm:main""#));
        assert_eq!(
            out.body_html.matches(r#"data-refkey="thm:main""#).count(),
            1
        );
        assert!(!out
            .body_html
            .contains(r#"class="label-anchor" id="thm-main" data-refkey="thm:main""#));
        assert!(out.body_html.contains(r#"data-refkey="eq:main""#));
        assert!(out
            .body_html
            .contains(r#"id="eq-alias" data-refkey="eq:alias""#));
        assert!(out.body_html.contains(r#"data-refkey="fig:plot""#));
        assert!(out.body_html.contains(r#"data-refkey="misc:loose""#));
    }

    #[test]
    fn display_label_changes_affect_math_reuse_hash() {
        let first = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{equation}\\label{eq:first}a=b\\end{equation}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();
        let second = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{equation}\\label{eq:second}a=b\\end{equation}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(first.body_html.contains(r#"data-refkey="eq:first""#));
        assert!(second.body_html.contains(r#"data-refkey="eq:second""#));
        assert_ne!(
            display_math_hash(&first.body_html),
            display_math_hash(&second.body_html)
        );
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
        assert!(out.body_html.contains(r#"class="para-break""#));
        assert!(out.body_html.contains("para-indent-marker"));
        assert!(text_content(&out.body_html).contains("Next line."));
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

        let text = text_content(&out.body_html);
        assert!(text.contains("First paragraph."));
        assert!(out.body_html.contains(r#"class="para-break""#));
        assert!(out.body_html.contains("para-indent-marker"));
        assert!(text.contains("Second paragraph."));
        assert!(!out.body_html.contains("<br><br>Second paragraph."));
    }

    #[test]
    fn blank_line_inside_environment_is_a_source_sync_target() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{proof}\nFirst paragraph.\n\nSecond paragraph.\n\\end{proof}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"class="source-space""#));
        let entry = out
            .sync
            .lookup_leaf_by_source_position(Path::new("t.tex"), 4, 1)
            .expect("blank line sync entry");
        assert!(entry.element_id.starts_with("srcs-"));
        assert!(out
            .blocks
            .iter()
            .flat_map(|block| block.source_anchors.iter())
            .any(|anchor| anchor.id == entry.element_id));
    }

    #[test]
    fn forward_source_sync_ignores_environment_container_spans() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{proof}\nFirst paragraph.\n\\end{proof}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out
            .sync
            .lookup_leaf_by_source_position(Path::new("t.tex"), 2, 1)
            .is_none());
        let entry = out
            .sync
            .lookup_leaf_by_source_position(Path::new("t.tex"), 3, 3)
            .expect("word sync entry");
        assert!(entry.element_id.starts_with("srcw-"));
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

        let text = text_content(&out.body_html);
        assert!(out.body_html.contains(r#"<p class="para">"#));
        assert!(out
            .body_html
            .contains(r#"<p class="para para-noindent para-flow">"#));
        assert!(text.contains("Before."));
        assert!(text.contains("Step 1: First step."));
        assert!(text.contains(r"Step 2: With \(x\). Second step."));
        assert!(out.body_html.contains(r#"data-tex="\(x\)""#));
        assert!(text.contains("Step 5: Restarted."));
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

        let text = text_content(&out.body_html);
        assert!(text.contains("Intro."));
        assert!(out.body_html.contains("flow-marker-break"));
        assert!(text.contains("Step 1: First."));
        assert!(text.contains("Step 2: Second. More."));
        assert!(text.contains("Case I: Diagonal. Case text."));
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
    fn subequations_render_parent_and_alphabetic_child_refs() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{subequations}\n\\label{eq:group}\n\\begin{equation}\n\\label{eq:first}\na=b\n\\end{equation}\n\\begin{equation}\n\\label{eq:second}\nc=d\n\\end{equation}\n\\end{subequations}\nSee \\eqref{eq:group}, \\eqref{eq:first}, and \\eqref{eq:second}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"id="eq-group""#));
        assert!(out.body_html.contains(r#"id="eq-first""#));
        assert!(out.body_html.contains(r#"id="eq-second""#));
        assert!(out
            .body_html
            .contains(r##"href="#eq-group" data-target="eq:group" data-kind="eqref">(1)"##));
        assert!(out
            .body_html
            .contains(r##"href="#eq-first" data-target="eq:first" data-kind="eqref">(1a)"##));
        assert!(out
            .body_html
            .contains(r##"href="#eq-second" data-target="eq:second" data-kind="eqref">(1b)"##));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num">(1a)</span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num">(1b)</span>"#));
        assert!(!out.body_html.contains("(eq:group)"));
        assert!(!out.body_html.contains("(eq:first)"));
        assert!(!out.body_html.contains("(eq:second)"));
    }

    #[test]
    fn refs_inside_math_bodies_resolve_before_mathjax() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{subequations}\n\\label{assumptions}\n\\begin{equation}\\label{H1}a=b\\end{equation}\n\\begin{equation}\\label{H3}c=d\\end{equation}\n\\end{subequations}\n\\begin{equation}\nX_0 = x, \\qquad \\text{ $V_0$ satisfies~\\eqref{H3}} \\,.\n\\end{equation}\nInline $\\eqref{H3}$.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"\text{ $V_0$ satisfies~(1b)}"#));
        assert!(out.body_html.contains(r#"\((1b)\)"#));
        assert!(out.body_html.contains(r#"\eqref{H3}"#));
        assert!(!out.body_html.contains(r#">\eqref{H3}<"#));
    }

    #[test]
    fn align_rows_render_separate_numbers_and_refs() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\section{S}\n\\begin{align}\na &= b \\label{eq:a}\\\\\nc &= d \\label{eq:b}\\\\\ne &= f \\notag\\\\\ng &= h \\label{eq:c}\n\\end{align}\nSee \\eqref{eq:a}, \\eqref{eq:b}, \\eqref{eq:c}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"class="eq-num-list""#));
        assert!(out.body_html.contains(r#"class="eq-refkey-list""#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-refkey-chip">eq:a</span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-refkey-chip" id="eq-b">eq:b</span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-refkey-chip" id="eq-c">eq:c</span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row">(1.1)</span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row">(1.2)</span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row empty"></span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row">(1.3)</span>"#));
        assert!(out
            .body_html
            .contains(r##"data-target="eq:a" data-kind="eqref">(1.1)</a>"##));
        assert!(out
            .body_html
            .contains(r##"data-target="eq:b" data-kind="eqref">(1.2)</a>"##));
        assert!(out
            .body_html
            .contains(r##"data-target="eq:c" data-kind="eqref">(1.3)</a>"##));
        assert!(!out.body_html.contains(r#"data-refkey="eq:a""#));
        assert!(!out.body_html.contains(r#"id="eq-b" data-refkey="eq:b""#));
        assert!(!out.body_html.contains("(1.4)"));
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
        assert!(out.html.contains(r#"id="refkey-toggle""#));
        assert!(out.html.contains(r#"data-refkeys="hidden""#));
        assert!(out.html.contains(r#"id="server-restart""#));
        assert!(out.html.contains(r#"id="server-stop""#));
        assert!(out.html.contains(r#"id="topbar-stripe""#));
        assert!(out.html.contains("setRefkeysVisible"));
        assert!(out.html.contains("mathpreview.refkeys"));
        assert!(out.html.contains("refkey-visible"));
        assert!(out.html.contains("setTopbarHidden"));
        assert!(out.html.contains("mathpreview.topbarHidden"));
        assert!(out.html.contains("topbar-hidden"));
        assert!(out.html.contains("topbarOffset"));
        assert!(out.html.contains("WS_PROTOCOL_VERSION = '26'"));
        assert!(out.html.contains(r#"id="search-panel""#));
        assert!(out.html.contains(r#"id="search-input""#));
        assert!(out.html.contains("handleVimNavigation"));
        assert!(out.html.contains("recordViewerPlace"));
        assert!(out.html.contains("restorePreviousPlace"));
        assert!(out.html.contains("viewerJumpStack"));
        assert!(out.html.contains("window.find"));
        assert!(out.html.contains("TEX_SYMBOL_CODEPOINTS"));
        assert!(out.html.contains("theta: [0x03B8]"));
        assert!(out.html.contains("runMathSearch"));
        assert!(out.html.contains("clearSearchSession"));
        assert!(out.html.contains("searchPanelIsOpen"));
        assert!(out.html.contains("math-search-glyph-active"));
        assert!(out
            .html
            .contains(r#"body.refkey-visible [data-refkey]:not(.label-anchor)::after"#));
        assert!(out
            .html
            .contains(r#"right: calc(100% + var(--refkey-gap));"#));
        assert!(out.html.contains(r#".eq-refkey-list"#));
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
        assert!(out.html.contains("focusMathNode"));
        assert!(out.html.contains("if (e.shiftKey)"));
        assert!(out.html.contains("document.addEventListener('dblclick'"));
        assert!(!out
            .html
            .contains("if (math) {\n      return;\n    }\n    requestSourceJump(e);"));
        assert!(out.html.contains("revealSourceElement"));
        assert!(out.html.contains("scrollSourceIntoView"));
        assert!(out.html.contains("source-active"));
        assert!(out.html.contains("fetch('/jump'"));
        assert!(out.html.contains("source-cursor"));
        assert!(out.html.contains(r#"class="src-word""#));
        assert!(out.html.contains("syncBlockSourceAnchors"));
        assert!(out.html.contains("syncBlockSourceAnchorsFromBlock"));
        assert!(out.html.contains("document.addEventListener('mousedown'"));
        assert!(out.html.contains("syncReusedMathNode"));
        assert!(out.html.contains("copyAttr(oldEl, newEl, 'tabindex')"));
        assert!(out.html.contains("copyAttr(oldEl, newEl, 'data-refkey')"));
        assert!(out
            .html
            .contains("copyAttr(oldBlock, newBlock, 'data-src')"));
        assert!(!out.html.contains("user-select: all"));
    }
}
