//! AST → HTML renderer.
//!
//! Output is a single self-contained HTML document. Math nodes are emitted as
//! engine-neutral `<span class="math" data-tex="\(...\)" data-hash="...">`
//! markers; the active [`crate::engines::MathEngine`] (default: MathJax v4
//! SVG) typesets them in the browser. Swapping engines is a frontend bundle
//! swap and does not require changing the AST walk.

use std::fmt::Write;
use std::path::PathBuf;

use serde::Serialize;

use std::collections::HashMap;

use crate::ast::{ListKind, Node, NodeKind, Pos, RefKind, Span};
use crate::bibtex::{BibEntry, BibStyle};
use crate::engines::Engine;
use crate::macros::ExtractedPreamble;
use crate::numbering::LabelTable;
use crate::sync::{SyncIndex, SyncKind};

mod bib;
mod math;
mod shell;
mod util;
use bib::format_bib_entry;
use math::{
    equation_number_html, equation_row_refkey_html, label_alias_anchors,
    render_latex_text_with_math, resolve_math_refs, strip_labels, write_float_placeholder,
    write_flow_marker,
};
use shell::wrap_in_shell;
use util::{
    balanced_group_end, capitalize, data_src, escape_attr, escape_html, escape_math, fnv_hash,
    is_blank_line_separator, latex_command_arg, latex_command_args, latex_command_call,
    latex_optional_usize, refkey_attr, role_label, role_pill_html, roman_upper, sanitize_id,
};

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
        NodeKind::Appendix => { /* numbering marker; no visible output */ }
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
                title = render_latex_text_with_math(title, ctx.labels),
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
                        render_latex_text_with_math(s, ctx.labels)
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
            write_chunked_children(out, &n.children, ctx);
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
            write_chunked_children(out, &n.children, ctx);
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
                r#"<span class="math inline" id="{id}" data-src="{src}" data-hash="{hash}" data-tex="{copy_tex}" data-mathjax-tex="{mathjax_tex}" tabindex="0" title="Copy as LaTeX"><span class="math-source">\({math}\)</span></span>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                hash = hash,
                copy_tex = escape_attr(&copy_tex),
                mathjax_tex = escape_attr(&format!(r"\({rendered_math}\)")),
                math = escape_math(&rendered_math),
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
                r#"<div class="math display" id="{id}" data-src="{src}"{refkey} data-hash="{hash}" data-tex="{copy_tex}" data-mathjax-tex="{mathjax_tex}" tabindex="0" title="Copy as LaTeX">{aliases}{row_refkeys}<span class="math-source">{math}</span>{num_html}</div>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                refkey = if row_numbers.is_empty() { refkey_attr(label.as_deref()) } else { String::new() },
                aliases = alias_html,
                row_refkeys = row_refkey_html,
                math = escape_math(&math),
                hash = hash,
                copy_tex = escape_attr(&copy_tex),
                mathjax_tex = escape_attr(&math),
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
                        let content = render_latex_text_with_math(&call.arg, ctx.labels);
                        record_container(ctx, &id, &n.span, None);
                        let src_attr = data_src(&n.span);
                        write!(
                            out,
                            r#"<span class="sidenote sidenote-note" id="{id}" data-src="{src_attr}" data-label="note"><span class="sidenote-marker">note</span><span class="sidenote-content">{content}</span></span>"#,
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
                        let content = render_latex_text_with_math(&call.arg, ctx.labels);
                        record_container(ctx, &id, &n.span, None);
                        let src_attr = data_src(&n.span);
                        write!(
                            out,
                            r#"<span class="sidenote sidenote-{kind}" id="{id}" data-src="{src_attr}" data-label="{label_attr}"><span class="sidenote-marker">{label_html}</span><span class="sidenote-content">{content}</span></span>"#,
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

fn is_chunked_block_child(node: &Node) -> bool {
    matches!(
        &node.kind,
        NodeKind::DisplayMath { .. } | NodeKind::Subequations { .. } | NodeKind::List { .. }
    )
}

/// Flush a paragraph-chunk buffer wrapped in a hashed `proof-para` span
/// so the client can sub-block-diff proof/theorem bodies — replacing
/// only the paragraphs whose hash changed instead of the entire block.
/// Empty / whitespace-only chunks are dropped to avoid polluting the
/// output with no-op spans.
///
/// The hash is computed over the chunk's STABLE diff source (the same
/// IDs / data-src normalization the top-level block diff uses) so a
/// single-paragraph edit doesn't ripple chunk hashes downstream just
/// because the `srcw-N` id counter shifted.
fn flush_chunk(buf: &mut String, out: &mut String) {
    let chunk = std::mem::take(buf);
    if chunk.trim().is_empty() {
        return;
    }
    let hash = fnv_hash(&stable_block_diff_source(&chunk));
    write!(
        out,
        r#"<span class="proof-para" data-subhash="{hash}">{chunk}</span>"#
    )
    .unwrap();
}

/// Like `write_children`, but groups runs of inline content into
/// hashed `<span class="proof-para" data-subhash="...">` chunks. Block
/// children (display math, subequations, lists) flush the current
/// chunk and are emitted as siblings, so the client can transplant
/// them by their existing `data-hash`. The state-machine logic mirrors
/// `write_children` closely; the only structural change is that
/// inline-bound writes go into a `chunk_buf` that is flushed on every
/// paragraph break / block-level child / end of children.
fn write_chunked_children(out: &mut String, children: &[Node], ctx: &mut RenderCtx) {
    let mut chunk_buf = String::new();
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
                    chunk_buf.push_str(r#"<span class="flow-marker-break"></span>"#);
                }
                write_node(&mut chunk_buf, child, ctx);
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
                            chunk_buf.push_str(
                                r#"<span class="para-indent-marker" aria-hidden="true"></span>"#,
                            );
                        }
                        if !trim_next_text
                            && !starts_blank_line
                            && text.starts_with(char::is_whitespace)
                            && !chunk_buf.ends_with(char::is_whitespace)
                        {
                            chunk_buf.push(' ');
                            let trimmed = trim_leading_paragraph_space(text);
                            text_start += text.len() - trimmed.len();
                            text = trimmed;
                        }
                        let text_span =
                            text_segment_span(&child.span, s, text_start, start + segment.len());
                        write_text_with_span(&mut chunk_buf, text, &text_span, ctx);
                        trim_next_text = false;
                        previous_was_display = false;
                        pending_paragraph_indent = false;
                        seen_content = true;
                    }
                    ParagraphTextPart::Break { start, end } => {
                        flush_chunk(&mut chunk_buf, out);
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

        if is_chunked_block_child(child) {
            flush_chunk(&mut chunk_buf, out);
            write_node(out, child, ctx);
            previous_was_display = matches!(&child.kind, NodeKind::DisplayMath { .. });
            trim_next_text = previous_was_display;
            pending_paragraph_indent = false;
            seen_content = true;
            continue;
        }

        if pending_paragraph_indent && is_inline_like_node(child) {
            chunk_buf.push_str(r#"<span class="para-indent-marker" aria-hidden="true"></span>"#);
        }
        write_node(&mut chunk_buf, child, ctx);
        previous_was_display = false;
        trim_next_text = false;
        pending_paragraph_indent = false;
        seen_content = true;
    }
    flush_chunk(&mut chunk_buf, out);
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

/// LaTeX-text → HTML for strings extracted into AST fields (section titles,
/// theorem names, proof "of" args, omitref payloads). Handles a curated set
/// of inline commands so embedded `\ref` / `\emph` / `\textbf` etc. don't
/// reach MathJax or land in the output as raw `\name{...}` source.
pub(super) fn render_inline_latex(s: &str, labels: &LabelTable) -> String {
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
            // LaTeX grouping braces have no visual effect — strip them so
            // text like `Hello {grouping} world` reads as `Hello grouping
            // world` rather than literally including the braces.
            if b == b'{' || b == b'}' {
                i += 1;
                continue;
            }
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
                let kind_str = if name == "pageref" { "pageref" } else { "ref" };
                let text = labels.resolve_ref(crate::ast::RefKind::Ref, key);
                let target = sanitize_id(key);
                write!(
                    out,
                    r##"<a class="ref" href="#{t}" data-target="{key}" data-kind="{kind_str}">{label}</a>"##,
                    t = escape_attr(&target),
                    key = escape_attr(key),
                    kind_str = kind_str,
                    label = escape_html(&text)
                )
                .unwrap();
                i = next;
            }
            ("cref", Some((key, next)))
            | ("Cref", Some((key, next)))
            | ("autoref", Some((key, next))) => {
                let kind_str = match name {
                    "autoref" => "autoref",
                    _ => "cref",
                };
                let text = labels.resolve_ref(crate::ast::RefKind::Cref, key);
                let target = sanitize_id(key);
                write!(
                    out,
                    r##"<a class="ref" href="#{t}" data-target="{key}" data-kind="{kind_str}">{label}</a>"##,
                    t = escape_attr(&target),
                    key = escape_attr(key),
                    kind_str = kind_str,
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
                    r##"<a class="ref" href="#{t}" data-target="{key}" data-kind="eqref">{label}</a>"##,
                    t = escape_attr(&target),
                    key = escape_attr(key),
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
        assert!(out
            .body_html
            .contains(r#"data-mathjax-tex="\begin{equation}"#));
        assert!(out.body_html.contains(r#"<span class="math-source">\("#));
        assert!(out
            .body_html
            .contains(r#"<span class="math-source">\begin{equation}"#));
        assert!(out.body_html.contains("x&lt;y"));
        assert!(out.body_html.contains(r#"\label{eq:test}"#));
        assert!(out.body_html.contains(r#"title="Copy as LaTeX""#));
        assert!(out.body_html.contains(r#"tabindex="0""#));
    }

    #[test]
    fn display_mathjax_source_is_isolated_from_viewer_chrome() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{equation*}\n  a^2\n\\end{equation*}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"class="math display""#));
        assert!(out
            .body_html
            .contains(r#"data-mathjax-tex="\begin{equation*}"#));
        assert!(out
            .body_html
            .contains(r#"<span class="math-source">\begin{equation*}"#));
        assert!(!out.body_html.contains(r#"<span class="math-source"><span"#));
    }

    #[test]
    fn theorem_optional_name_inline_math_is_typeset() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{lemma}[$Y$-energy]\nStatement.\n\\end{lemma}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        let name_start = out
            .body_html
            .find(r#"<span class="thm-name">"#)
            .expect("thm-name span");
        let name_end = name_start
            + out.body_html[name_start..]
                .find("</span>")
                .expect("thm-name close");
        let thm_name = &out.body_html[name_start..name_end];
        assert!(
            thm_name.contains(r#"<span class="math inline""#),
            "math in lemma name should be MathJax-typeset; got: {thm_name}",
        );
        assert!(
            thm_name.contains(r#"data-tex="\(Y\)""#),
            "math span should carry the $Y$ payload; got: {thm_name}",
        );
        assert!(
            !thm_name.contains("$Y$"),
            "literal $Y$ should not survive in the rendered name; got: {thm_name}",
        );
    }

    #[test]
    fn section_title_inline_math_is_typeset() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\section{The $L^p$ space}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(
            out.body_html.contains(r#"<span class="math inline""#),
            "math in section title should be MathJax-typeset; got: {}",
            out.body_html,
        );
        assert!(
            out.body_html.contains(r#"data-tex="\(L^p\)""#),
            "math span should carry the $L^p$ payload; got: {}",
            out.body_html,
        );
    }

    /// Margin cards build their refkey chip from `data-target` on the clicked
    /// `<a class="ref">`. Refs that appear inside title-like fields (section
    /// titles, theorem names, captions) go through `render_inline_latex`,
    /// which used to emit the link without `data-target`/`data-kind` — so
    /// clicking a `\ref` in a section title pinned a chip-less card. Assert
    /// every ref site carries the target key.
    #[test]
    fn refs_inside_inline_latex_fields_carry_data_target() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{proposition}\\label{prop:foo}\nStatement.\n\\end{proposition}\n\\section{See \\ref{prop:foo} and \\eqref{eq:x} and \\autoref{prop:foo}}\n\\begin{equation}\\label{eq:x}\na=b\n\\end{equation}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(
            out.body_html
                .contains(r##"data-target="prop:foo" data-kind="ref""##),
            "section-title \\ref should carry data-target/data-kind; got: {}",
            out.body_html,
        );
        assert!(
            out.body_html
                .contains(r##"data-target="eq:x" data-kind="eqref""##),
            "section-title \\eqref should carry data-target/data-kind; got: {}",
            out.body_html,
        );
        assert!(
            out.body_html
                .contains(r##"data-target="prop:foo" data-kind="autoref""##),
            "section-title \\autoref should carry data-target/data-kind; got: {}",
            out.body_html,
        );
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
    fn proof_paragraphs_are_wrapped_in_subhash_chunks() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{proof}\nFirst paragraph.\n\nSecond paragraph with $a+b$.\n\nThird paragraph.\n\\end{proof}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();
        // Each non-empty inline run inside the proof body becomes a
        // `<span class="proof-para" data-subhash="...">` chunk so the
        // client can sub-block diff at paragraph granularity rather
        // than swapping the whole proof's HTML on every edit.
        let chunk_count = out
            .body_html
            .matches(r#"<span class="proof-para" data-subhash=""#)
            .count();
        assert!(
            chunk_count >= 3,
            "expected at least one chunk per paragraph, got {chunk_count}: {body}",
            body = out.body_html
        );
        // Editing one paragraph should change exactly one chunk's hash;
        // the others should keep theirs across renders.
        let second = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{proof}\nFirst paragraph.\n\nSecond paragraph CHANGED with $a+b$.\n\nThird paragraph.\n\\end{proof}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();
        let extract = |html: &str| -> Vec<String> {
            let needle = r#"data-subhash=""#;
            let mut hashes = Vec::new();
            let mut i = 0;
            while let Some(pos) = html[i..].find(needle) {
                let start = i + pos + needle.len();
                if let Some(end) = html[start..].find('"') {
                    hashes.push(html[start..start + end].to_string());
                    i = start + end;
                } else {
                    break;
                }
            }
            hashes
        };
        let h1 = extract(&out.body_html);
        let h2 = extract(&second.body_html);
        assert_eq!(h1.len(), h2.len());
        let diffs: usize = h1.iter().zip(h2.iter()).filter(|(a, b)| a != b).count();
        assert_eq!(
            diffs, 1,
            "exactly one chunk's hash should change for a single-paragraph edit"
        );
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
    fn appendix_switches_section_and_equation_numbers() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\section{Main}\n\\appendix\n\\section{Derivation}\\label{app:derivation}\n\\begin{equation}\\label{eq:app}a=b\\end{equation}\nSee \\ref{app:derivation} and \\eqref{eq:app}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"<span class="sec-num">A</span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num">(A.1)</span>"#));
        assert!(out.body_html.contains(
            r##"href="#app-derivation" data-target="app:derivation" data-kind="ref">A</a>"##
        ));
        assert!(out
            .body_html
            .contains(r##"href="#eq-app" data-target="eq:app" data-kind="eqref">(A.1)</a>"##));
        assert!(!out.body_html.contains(r#"\appendix"#));
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
        assert!(out.body_html.contains(
            r#"<span class="eq-refkey-chip" data-target="eq:a" tabindex="0" title="pin eq:a to margin">eq:a</span>"#
        ));
        assert!(out.body_html.contains(
            r#"<span class="eq-refkey-chip" id="eq-b" data-target="eq:b" tabindex="0" title="pin eq:b to margin">eq:b</span>"#
        ));
        assert!(out.body_html.contains(
            r#"<span class="eq-refkey-chip" id="eq-c" data-target="eq:c" tabindex="0" title="pin eq:c to margin">eq:c</span>"#
        ));
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
        assert!(out.html.contains(r#"id="margin-cards""#));
        assert!(out.html.contains(r#"id="cmdline""#));
        assert!(out.html.contains(r#"id="cmdline-input""#));
        assert!(out.html.contains("pinByRefkey"));
        assert!(out.html.contains("openCmdline"));
        assert!(out.html.contains("setRefkeysVisible"));
        assert!(out.html.contains("mathpreview.refkeys"));
        assert!(out.html.contains("refkey-visible"));
        assert!(out.html.contains("setTopbarHidden"));
        assert!(out.html.contains("mathpreview.topbarHidden"));
        assert!(out.html.contains("topbar-hidden"));
        assert!(out.html.contains("topbarOffset"));
        assert!(out.html.contains("WS_PROTOCOL_VERSION = '50'"));
        assert!(out.html.contains("margin-card-grip"));
        assert!(out.html.contains("initMarginDnd"));
        assert!(out.html.contains("decorateRefkeyChips"));
        assert!(out.html.contains("startup: { typeset: false }"));
        assert!(out.html.contains("queueInitialTypeset"));
        assert!(out.html.contains("ensureInitialTypeset"));
        assert!(out.html.contains("queueUntypesetMath"));
        assert!(out.html.contains("MutationObserver"));
        assert!(out.html.contains("scheduleTypesetFlush"));
        assert!(out.html.contains("tex2svgPromise"));
        assert!(out.html.contains("mjx-container"));
        assert!(out.html.contains("oldEl.innerHTML = newEl.innerHTML"));
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
        assert!(out.html.contains(r#"[data-refkey]:not(.label-anchor)"#));
        assert!(out.html.contains(r#"body.refkey-visible .refkey-chip"#));
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

    #[test]
    fn local_mathjax_shell_points_to_vendored_newcm_svg_fonts() {
        let opts = HtmlOptions {
            engine: crate::engines::Engine::MathJax(crate::engines::MathJaxEngine::new(
                "/vendor/mathjax/tex-svg.js",
            )),
            ..HtmlOptions::default()
        };
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\documentclass{article}\n\\usepackage{bm}\n\\begin{document}\n$\\bm{E}$\n\\end{document}\n".to_string(),
            &opts,
        )
        .unwrap();

        assert!(out
            .html
            .contains(r#"'mathjax-newcm': "/vendor/mathjax/mathjax-newcm-font""#));
        assert!(out.html.contains("[tex]/boldsymbol"));
        assert!(out.html.contains(r#""bm": ["\\boldsymbol{#1}", 1]"#));
    }
}
