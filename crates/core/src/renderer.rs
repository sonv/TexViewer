//! AST → HTML renderer.
//!
//! Output is a single self-contained HTML document. Math nodes are emitted as
//! engine-neutral `<span class="math" data-tex="\(...\)" data-hash="...">`
//! markers; the active [`crate::engines::MathEngine`] (default: MathJax v4
//! SVG) typesets them in the browser. Swapping engines is a frontend bundle
//! swap and does not require changing the AST walk.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use std::collections::HashMap;

use crate::ast::{
    EnvironmentBoundary, ListKind, MarkdownAlignment, Node, NodeKind, Pos, RefKind, Span,
    TextAlignment,
};
use crate::bibtex::{BibEntry, BibStyle};
use crate::engines::Engine;
use crate::macros::ExtractedPreamble;
use crate::numbering::LabelTable;
use crate::sync::{SyncIndex, SyncKind};

mod bib;
mod color;
mod math;
mod shell;
mod table;
mod util;
use bib::format_bib_entry;
use color::resolve_css as resolve_color_css;
use math::{
    equation_number_html, equation_row_refkey_html, label_alias_anchors, math_row_spans,
    math_row_tex_spans, render_latex_text_with_math, resolve_math_refs, strip_labels,
    write_float_placeholder, write_flow_marker, write_inline_math_span,
};
use shell::wrap_in_shell;
use table::{
    first_nested_tabular, is_tabular_environment, render_tabular, render_tabular_with_number,
};
use util::{
    balanced_group_end, capitalize, data_src, escape_attr, escape_html, escape_math, fnv_hash,
    is_blank_line_separator, latex_command_arg, latex_command_args, latex_command_call,
    latex_optional_usize, refkey_attr, role_label, role_pill_html, roman_upper, sanitize_id,
};

#[derive(Debug, Clone)]
pub struct HtmlOptions {
    /// Source frontend used for format-aware viewer chrome and rendering.
    /// Generic entry points set this automatically from the input path.
    pub document_format: crate::DocumentFormat,
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
    /// Extra macro override files, in cascade order (lowest to highest
    /// priority). Layered on top of the paper preamble inside
    /// `macros::extract_preamble_with_overrides`, so later entries
    /// override earlier ones on name collision. Populated by the daemon's
    /// global / project / `--macros` discovery; the static `render`
    /// pipeline leaves it empty.
    pub macro_overrides: Vec<PathBuf>,
    /// User-resolved viewer preferences (font-size, source-jump trigger,
    /// keybindings, …). Loaded from the TOML config cascade by the daemon; static
    /// `render` callers get the built-in defaults.
    pub viewer_config: crate::config::ResolvedViewerConfig,
    /// Base URL for on-demand TikZ SVG assets. The live server sets this to
    /// `/tikz/`; standalone HTML leaves it unset so it never emits dead
    /// server-relative image URLs.
    pub tikz_asset_base: Option<String>,
    /// Base URL for local document assets in live-server mode. Markdown
    /// relative images are rewritten below this route; standalone HTML keeps
    /// their original relative URLs by leaving this unset.
    pub local_asset_base: Option<String>,
    /// Exact root-document preamble used to compile isolated TikZ diagrams.
    /// Populated internally after the project loader splits the document.
    pub latex_preamble: Option<String>,
    /// Inline text-mode macro → HTML template map from the TOML config's
    /// `[text-macros]` table. Applied to body text by `render_inline_latex`.
    /// Empty for static `render` callers.
    pub text_macros: std::collections::HashMap<String, crate::config::TextMacro>,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            document_format: crate::DocumentFormat::Latex,
            engine: Engine::default(),
            title: "mathpreview".into(),
            source_path: None,
            inline_css: true,
            macro_overrides: Vec::new(),
            viewer_config: crate::config::ResolvedConfig::default().viewer,
            tikz_asset_base: None,
            local_asset_base: None,
            latex_preamble: None,
            text_macros: std::collections::HashMap::new(),
        }
    }
}

/// One TikZ environment that the live server can compile into an SVG.
/// The URL hash covers all three fields, so a body, option, or preamble edit
/// produces a fresh immutable asset URL.
#[derive(Debug, Clone)]
pub struct TikzAsset {
    pub environment: String,
    pub body: String,
    pub preamble: String,
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
    /// On-demand TikZ sources keyed by the hash embedded in their image URLs.
    /// Internal server state, never part of the WebSocket/debug JSON payload.
    #[serde(skip)]
    pub tikz_assets: HashMap<String, TikzAsset>,
    /// Frontend that produced this output.
    pub format: crate::DocumentFormat,
}

/// Both forms of the rendered output, returned together so `render_project`
/// can populate `RenderOutput` without re-running the AST walk.
#[derive(Debug, Clone)]
pub struct RenderedHtml {
    pub full: String,
    pub body: String,
    pub blocks: Vec<RenderedBlock>,
    pub tikz_assets: HashMap<String, TikzAsset>,
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
    /// For theorem/proof blocks whose body is rendered as a sequence of
    /// independently-hashed `proof-para` chunks (plus single-element block
    /// children like display math and lists), this captures that structure
    /// so the diff can replace only the changed sub-range instead of
    /// re-sending the whole block's HTML on every keystroke. `None` for
    /// ordinary blocks and for any chunked block that contains a child
    /// expanding to multiple sibling elements (e.g. `subequations`), which
    /// would break the client's element-index addressing.
    #[serde(skip)]
    pub sub_blocks: Option<SubBody>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceAnchor {
    pub id: String,
    pub src: String,
}

/// One body child of a sub-diffable block. Each chunk renders to exactly
/// one top-level DOM element, so `children` indices line up 1:1 with the
/// body container's element children on the client.
#[derive(Debug, Clone)]
pub struct SubChunk {
    /// Position-stable content hash (generated ids / `data-src` stripped),
    /// used to find the unchanged common prefix/suffix between renders.
    pub diff_hash: String,
    /// The chunk's full HTML — one element.
    pub html: String,
}

/// Sub-block structure of a theorem/proof block, captured at render time.
#[derive(Debug, Clone)]
pub struct SubBody {
    /// Stable hash of the block HTML before the body interior (container
    /// open + head + body-open tag). If this differs between renders the
    /// scaffolding changed and the diff falls back to a full block replace.
    pub prefix_diff: String,
    /// Stable hash of the block HTML after the body interior (e.g. the
    /// proof `∎` + closing tags, or a theorem's omitted-ref note).
    pub suffix_diff: String,
    /// Body children in document order; each maps to one DOM element.
    pub children: Vec<SubChunk>,
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
    // Make the document's \newcommand definitions + the TOML [text-macros]
    // templates available to the inline text renderer for this render.
    install_text_macros(preamble, opts);
    color::install(&preamble.raw_preamble);
    reset_footnote_counter();
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
        source_anchors: Vec::new(),
        chunk_depth: 0,
        pending_sub: None,
        tikz_assets: HashMap::new(),
        render_tikz: opts.viewer_config.render_tikz,
        fancy_theorems: opts.viewer_config.fancy_theorems,
        tikz_asset_base: opts.tikz_asset_base.as_deref(),
        local_asset_base: opts.local_asset_base.as_deref(),
        latex_preamble: opts.latex_preamble.as_deref().unwrap_or(""),
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
            ctx.pending_sub = None;
            continue;
        }
        push_block(&mut blocks, i, inner, Some(&node.span), &mut ctx);
        previous_block_was_display = matches!(&node.kind, NodeKind::DisplayMath { .. });
        blank_after_display = false;
    }
    flush_paragraph(&mut blocks, &mut paragraph, &mut ctx);

    let body: String = blocks.iter().map(|b| b.html.as_str()).collect();
    let full = wrap_in_shell(&body, preamble, opts);
    RenderedHtml {
        full,
        body,
        blocks,
        tikz_assets: ctx.tikz_assets,
    }
}

fn front_matter_order(nodes: &[Node]) -> Vec<&Node> {
    let Some(title_index) = nodes
        .iter()
        .position(|node| matches!(node.kind, NodeKind::MakeTitle))
    else {
        return nodes.iter().collect();
    };
    let mut unsupported_depth = 0usize;
    let delayed_abstracts: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            match &node.kind {
                NodeKind::UnsupportedEnvBoundary {
                    boundary: EnvironmentBoundary::Begin,
                    ..
                } => unsupported_depth += 1,
                NodeKind::UnsupportedEnvBoundary {
                    boundary: EnvironmentBoundary::End | EnvironmentBoundary::MissingEnd,
                    ..
                } => unsupported_depth = unsupported_depth.saturating_sub(1),
                _ => {}
            }
            (index < title_index
                && unsupported_depth == 0
                && matches!(node.kind, NodeKind::Abstract))
            .then_some(index)
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
    // Consume any sub-block structure captured while rendering this block's
    // body. Offsets are into `inner`; slice out the scaffolding around the
    // body interior and hash it so the diff can detect head/number changes.
    let sub_blocks = ctx.pending_sub.take().and_then(|ps| {
        let prefix = inner.get(..ps.body_start)?;
        let suffix = inner.get(ps.body_end..)?;
        Some(SubBody {
            prefix_diff: fnv_hash(&stable_block_diff_source(prefix)),
            suffix_diff: fnv_hash(&stable_block_diff_source(suffix)),
            children: ps.children,
        })
    });
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
        sub_blocks,
    });
    // Ids for the NEXT block start a fresh per-block sequence (see IdGen).
    ctx.idgen.begin_block(blocks.len() as u32);
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
    // IdGen-produced ids are `<prefix>-g<block>-<n>` — the `g` marker (see
    // IdGen::next) also lets this stripper match them without ever matching a
    // label-derived id like `thm-2-1` (from `\label{thm:2.1}`), which must
    // NOT be stripped: label ids are stable, meaningful content.
    const IDGEN_PREFIXES: [&str; 16] = [
        r#" id="quote-"#,
        r#" id="callout-"#,
        r#" id="letter-"#,
        r#" id="letter-part-"#,
        r#" id="unsupported-env-"#,
        r#" id="sn-"#,
        r#" id="im-"#,
        r#" id="dm-"#,
        r#" id="sec-"#,
        r#" id="thm-"#,
        r#" id="proof-"#,
        r#" id="ref-"#,
        r#" id="cite-"#,
        r#" id="srcs-"#,
        r#" id="srcw-"#,
        r#" id="md-"#,
    ];
    // Counter-based ids that stay bare-numeric: footnotes keep their visible
    // document-order number (`fn-3` / `fnpop-3`), and `eq-` anchors predate
    // the marker scheme.
    const NUMERIC_PREFIXES: [&str; 3] = [r#" id="fn-"#, r#" id="fnpop-"#, r#" id="eq-"#];
    let generated = |p: &str, g_marker: bool| -> bool {
        if !rest.starts_with(p) {
            return false;
        }
        let b = rest.as_bytes();
        let mut i = p.len();
        if g_marker {
            if b.get(i) != Some(&b'g') {
                return false;
            }
            i += 1;
        }
        b.get(i).is_some_and(u8::is_ascii_digit)
    };
    IDGEN_PREFIXES.iter().any(|p| generated(p, true))
        || NUMERIC_PREFIXES.iter().any(|p| generated(p, false))
}

fn quoted_attr_end(s: &str, start: usize) -> Option<usize> {
    let first_quote = s[start..].find('"')?;
    let value_start = start + first_quote + 1;
    let second_quote = s[value_start..].find('"')?;
    Some(value_start + second_quote + 1)
}

fn is_top_level_inline_node(node: &Node) -> bool {
    match &node.kind {
        NodeKind::TextColor { .. } => node.children.iter().all(is_top_level_inline_node),
        NodeKind::Text(_)
        | NodeKind::InlineMath(_)
        | NodeKind::Ref { .. }
        | NodeKind::Cite { .. }
        | NodeKind::OpaqueCmd { .. }
        | NodeKind::Comment(_)
        | NodeKind::MarkdownText(_)
        | NodeKind::MarkdownEmphasis
        | NodeKind::MarkdownStrong
        | NodeKind::MarkdownStrikethrough
        | NodeKind::MarkdownLink { .. }
        | NodeKind::MarkdownImage { .. }
        | NodeKind::MarkdownInlineCode(_)
        | NodeKind::MarkdownSoftBreak
        | NodeKind::MarkdownHardBreak
        | NodeKind::MarkdownFootnoteReference { .. }
        | NodeKind::MarkdownRawHtml(_)
        | NodeKind::MarkdownSuperscript
        | NodeKind::MarkdownSubscript => true,
        _ => false,
    }
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
    if contains_stateful_color_switch(s) {
        let rendered = render_inline_latex(s, ctx.labels);
        if rendered.is_empty() {
            return rendered;
        }
        let id = ctx.idgen.next("srcw");
        record(ctx, &id, span, None);
        return format!(
            r#"<span class="src-word" id="{id}" data-src="{src}">{rendered}</span>"#,
            id = escape_attr(&id),
            src = escape_attr(&data_src(span)),
        );
    }

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
        } else if ch == '{' {
            // Keep `{` glued to its matching `}` when the group opens with a
            // stateful inline switch (for example `{\bf foo}` or
            // `{\color{red} foo}`), so its scope reaches the inline renderer
            // as one unit instead of being tokenized away.
            if let Some(end) = inline_switch_group_end(s, i) {
                end
            } else {
                let start = i;
                i += ch.len_utf8();
                out.push_str(&render_inline_latex(&s[start..i], ctx.labels));
                continue;
            }
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

fn contains_stateful_color_switch(s: &str) -> bool {
    [r"\color", r"\normalcolor"].iter().any(|needle| {
        let mut from = 0usize;
        while let Some(offset) = s[from..].find(needle) {
            let end = from + offset + needle.len();
            if s.as_bytes()
                .get(end)
                .is_none_or(|byte| !byte.is_ascii_alphabetic() && *byte != b'@')
            {
                return true;
            }
            from = end;
        }
        false
    })
}

/// If `start` points at `{` and the group opens with an old-style font switch
/// or a text-color declaration, return the index just past the matching `}`.
/// Used by the source-span tokenizer so the whole scoped chunk renders as one
/// unit.
fn inline_switch_group_end(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let mut k = start + 1;
    while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
        k += 1;
    }
    if k >= bytes.len() || bytes[k] != b'\\' {
        return None;
    }
    let n_start = k + 1;
    let mut n_end = n_start;
    while n_end < bytes.len() && bytes[n_end].is_ascii_alphabetic() {
        n_end += 1;
    }
    let cmd = &s[n_start..n_end];
    let is_inline_switch = matches!(
        cmd,
        "bf" | "bfseries"
            | "em"
            | "it"
            | "itshape"
            | "emshape"
            | "tt"
            | "ttfamily"
            | "sc"
            | "scshape"
            | "color"
            | "normalcolor"
    );
    if !is_inline_switch {
        return None;
    }
    let mut depth = 1i32;
    let mut q = n_end;
    while q < bytes.len() {
        match bytes[q] {
            b'\\' if q + 1 < bytes.len() => {
                q += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(q + 1);
                }
            }
            _ => {}
        }
        q += 1;
    }
    None
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
    // A control-space is complete at the space itself. Treating it like an
    // accent command would absorb the first letter of the following word into
    // the control-space's source span, even though the rendered text looked
    // correct after whitespace collapsing.
    if punct.is_whitespace() {
        return Some(i);
    }
    // Only accent control symbols consume a following character or group.
    // Literal escapes such as `\%R` end at `%`; treating every punctuation
    // command as an accent attached `R` to the escape's source span (and made
    // the inline renderer feed it into the broken escape token).
    if !matches!(punct, '\'' | '`' | '"' | '^' | '~' | '.' | '=') {
        return Some(i);
    }
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
/// Generated-element id source. Ids are scoped PER BLOCK
/// (`<prefix>-<block>-<n>`), not document-globally: with one global sequence,
/// inserting a single word early in the document renumbered every later
/// element, which invalidated every later block's anchor metadata — turning
/// each keystroke on a large document into a megabyte-scale patch and ~20k
/// client-side attribute writes. Block-scoped ids keep untouched blocks
/// byte-identical across renders, so the patch metadata delta ships only the
/// edited block. (Footnote ids are NOT from here — their visible numbering
/// must stay document-ordered.)
struct IdGen {
    counter: u32,
    block: u32,
}
impl IdGen {
    fn next(&mut self, prefix: &str) -> String {
        self.counter += 1;
        // The `g` marker keeps generated ids structurally disjoint from
        // label-derived ids: `sanitize_id("thm:2.1")` yields `thm-2-1`, which
        // without the marker would collide with a generated theorem id and
        // break getElementById targeting (refs, source-jump, highlights).
        format!("{prefix}-g{}-{}", self.block, self.counter)
    }
    /// Scope subsequent ids to block `ordinal` and restart the local counter.
    /// Called by `push_block` when a block is sealed; NOT called on the
    /// skipped-empty-block path, so discarded ids (already recorded in the
    /// sync index) can't collide with the next block's.
    fn begin_block(&mut self, ordinal: u32) {
        self.block = ordinal;
        self.counter = 0;
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
    source_anchors: Vec<SourceAnchor>,
    /// Nesting depth of `write_chunked_children`; only the outermost call
    /// (depth 1) captures sub-block structure, so a theorem nested inside a
    /// proof body doesn't clobber the outer block's chunks.
    chunk_depth: u32,
    /// Sub-block structure captured by the most recent outermost
    /// `write_chunked_children`, consumed by the next `push_block`.
    pending_sub: Option<PendingSub>,
    /// TikZ sources referenced by this render, keyed by their immutable URL
    /// hash. The live server consumes these lazily when the browser asks for an
    /// SVG, so ordinary document renders never spawn TeX.
    tikz_assets: HashMap<String, TikzAsset>,
    render_tikz: bool,
    fancy_theorems: bool,
    tikz_asset_base: Option<&'a str>,
    local_asset_base: Option<&'a str>,
    latex_preamble: &'a str,
}

/// Sub-block capture in progress, with body-interior byte offsets into the
/// block buffer so `push_block` can hash the surrounding scaffolding.
struct PendingSub {
    body_start: usize,
    body_end: usize,
    children: Vec<SubChunk>,
}

fn record(ctx: &mut RenderCtx, id: &str, span: &Span, label: Option<&str>) {
    record_with_kind(ctx, id, span, label, SyncKind::Leaf);
}

fn record_container(ctx: &mut RenderCtx, id: &str, span: &Span, label: Option<&str>) {
    record_with_kind(ctx, id, span, label, SyncKind::Container);
}

/// A block-level leaf (section heading): selectable as a range, but the cursor's
/// single-point flash skips it so it doesn't light up the whole line.
fn record_block(ctx: &mut RenderCtx, id: &str, span: &Span, label: Option<&str>) {
    record_with_kind(ctx, id, span, label, SyncKind::Block);
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

fn is_tikz_environment(env: &str) -> bool {
    matches!(env, "tikzpicture" | "tikzcd" | "circuitikz" | "forest")
}

/// Return the first TikZ environment nested inside an otherwise-opaque float.
/// The parser deliberately keeps `figure` bodies verbatim, so recover the
/// diagram here without trying to parse arbitrary LaTeX float contents.
fn first_nested_tikz(source: &str) -> Option<(String, String)> {
    crate::parser::first_supported_environment(
        source,
        &["tikzpicture", "tikzcd", "circuitikz", "forest"],
    )
}

fn write_opaque_environment(out: &mut String, env: &str, body: &str) {
    // Best-effort fallback: keep the source readable if a dedicated renderer
    // cannot safely understand it. Math remains inert in this path.
    writeln!(
        out,
        r#"<div class="opaque-env" data-env="{env}">{body}</div>"#,
        env = escape_attr(env),
        body = escape_html(body),
    )
    .unwrap();
}

fn tikz_html(env: &str, body: &str, span: &Span, ctx: &mut RenderCtx<'_>) -> String {
    let hash = fnv_hash(&format!(
        "tikz-svg-v1\0{env}\0{body}\0{}",
        ctx.latex_preamble
    ));
    ctx.tikz_assets.insert(
        hash.clone(),
        TikzAsset {
            environment: env.to_string(),
            body: body.to_string(),
            preamble: ctx.latex_preamble.to_string(),
        },
    );

    let id = ctx.idgen.next("tikz");
    record(ctx, &id, span, None);
    let (content, busy_attr) = if !ctx.render_tikz {
        (r#"<div class="tikz-placeholder"><strong>TikZ preview disabled.</strong> Enable <code>render-tikz = true</code> for this trusted project.</div>"#.to_string(), "")
    } else if let Some(base) = ctx.tikz_asset_base {
        (
            format!(
                r#"<div class="tikz-placeholder tikz-pending">Diagram queued for rendering…</div><img class="tikz-image" data-tikz-src="{src}" alt="TikZ diagram" decoding="async" hidden>"#,
                src = escape_attr(&format!("{base}{hash}.svg")),
            ),
            r#" aria-busy="true""#,
        )
    } else {
        (r#"<div class="tikz-placeholder"><strong>TikZ preview needs live server mode.</strong> Standalone HTML cannot compile local TeX assets.</div>"#.to_string(), "")
    };
    format!(
        r#"<div class="tikz-diagram" id="{id}" data-src="{src}" data-tikz-hash="{hash}" tabindex="0" title="TikZ diagram"{busy_attr}>{content}</div>"#,
        id = escape_attr(&id),
        src = escape_attr(&data_src(span)),
        hash = escape_attr(&hash),
    )
}

fn markdown_title_attr(title: Option<&str>) -> String {
    title
        .filter(|title| !title.is_empty())
        .map(|title| format!(r#" title="{}""#, escape_attr(title)))
        .unwrap_or_default()
}

/// Conservative URL policy for document-controlled links and images. Browser
/// navigation schemes are explicit; local absolute/relative paths, queries,
/// and fragments have no scheme and remain available.
pub(crate) fn safe_markdown_url(url: &str) -> bool {
    if url.is_empty()
        || url.chars().any(char::is_control)
        || url.starts_with("//")
        || url.starts_with("\\\\")
    {
        return false;
    }
    let boundary = url.find(['/', '?', '#']).unwrap_or(url.len());
    let Some(colon) = url[..boundary].find(':') else {
        return true;
    };
    let scheme = &url[..colon];
    !scheme.is_empty()
        && scheme.bytes().enumerate().all(|(i, byte)| match byte {
            b'a'..=b'z' | b'A'..=b'Z' => true,
            b'0'..=b'9' | b'+' | b'-' | b'.' => i > 0,
            _ => false,
        })
        && matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto" | "tel"
        )
}

fn markdown_image_url(destination: &str, asset_base: Option<&str>) -> String {
    let Some(base) = asset_base else {
        return destination.to_string();
    };
    if !is_local_relative_asset(destination) {
        return destination.to_string();
    }
    let relative = destination.trim_start_matches("./");
    let relative = if base.starts_with("file:") {
        let suffix_at = relative.find(['?', '#']).unwrap_or(relative.len());
        format!(
            "{}{}",
            percent_encode_url_path(&relative[..suffix_at], true),
            &relative[suffix_at..]
        )
    } else {
        relative.to_string()
    };
    format!("{}{relative}", base.trim_end_matches('/').to_string() + "/")
}

/// Build an absolute `file:` URL suitable as [`HtmlOptions::local_asset_base`].
/// The caller supplies a canonical absolute directory. URL path bytes are
/// encoded here (rather than relying on browser repair) so spaces, Unicode,
/// literal `%`, and Windows drive/UNC paths remain unambiguous.
pub fn file_url_base_for_directory(directory: &Path) -> Option<String> {
    if !directory.is_absolute() {
        return None;
    }
    let raw = directory.to_str()?;

    #[cfg(windows)]
    let normalized = {
        let slashed = raw.replace('\\', "/");
        if let Some(rest) = slashed.strip_prefix("//?/UNC/") {
            format!("//{rest}")
        } else if let Some(rest) = slashed.strip_prefix("//?/") {
            rest.to_string()
        } else {
            slashed
        }
    };
    #[cfg(not(windows))]
    let normalized = raw.to_string();

    // This is a filesystem path, not an already encoded URL: a literal `%20`
    // directory name must become `%2520`, and `?` / `#` must not turn into URL
    // delimiters.
    let encoded = percent_encode_url_path(&normalized, false);
    let mut url = if encoded.starts_with("//") {
        // UNC: //server/share -> file://server/share
        format!("file:{encoded}")
    } else if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        // Windows drive path: C:/dir -> file:///C:/dir
        format!("file:///{encoded}")
    };
    if !url.ends_with('/') {
        url.push('/');
    }
    Some(url)
}

fn percent_encode_url_path(path: &str, preserve_valid_escapes: bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let bytes = path.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let byte = bytes[i];
        let preserve_percent = preserve_valid_escapes
            && byte == b'%'
            && bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
            && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit);
        if preserve_percent {
            out.push('%');
            out.push(bytes[i + 1] as char);
            out.push(bytes[i + 2] as char);
            i += 3;
            continue;
        }
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        i += 1;
    }
    out
}

/// Reject percent escapes that a browser can normalize into containment-
/// relevant path syntax. In particular, URL parsers recognize encoded dot
/// segments (`%2e%2e`, `.%2e`, `%2e.`) and encoded separators before resolving
/// a `file:` or same-origin URL. Checking only `Path::components()` on the raw
/// Markdown destination would therefore accept a path the browser later turns
/// into `../...`.
fn safe_percent_encoded_local_path(path: &str) -> bool {
    for segment in path.split('/') {
        let bytes = segment.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut had_escape = false;
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit()
            {
                let high = (bytes[i + 1] as char).to_digit(16).unwrap() as u8;
                let low = (bytes[i + 2] as char).to_digit(16).unwrap() as u8;
                let byte = (high << 4) | low;
                // An encoded slash creates a new segment after the raw
                // component check; an encoded backslash can do the same on
                // Windows. Encoded controls are never useful in asset paths.
                if matches!(byte, b'/' | b'\\') || byte.is_ascii_control() {
                    return false;
                }
                decoded.push(byte);
                had_escape = true;
                i += 3;
            } else {
                decoded.push(bytes[i]);
                i += 1;
            }
        }
        if had_escape && matches!(decoded.as_slice(), b"." | b"..") {
            return false;
        }
    }
    true
}

fn safe_markdown_image_url(url: &str) -> bool {
    if !safe_markdown_url(url) {
        return false;
    }
    let boundary = url.find(['/', '?', '#']).unwrap_or(url.len());
    let has_scheme = url[..boundary].contains(':');
    if has_scheme || url.starts_with(['/', '#']) {
        return true;
    }
    let path = url.split(['?', '#']).next().unwrap_or("");
    !path.is_empty()
        && !url.contains('\\')
        && safe_percent_encoded_local_path(path)
        && PathBuf::from(path).components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

fn is_local_relative_asset(url: &str) -> bool {
    if url.starts_with(['/', '#', '?']) || url.contains('\\') {
        return false;
    }
    let path = url.split(['?', '#']).next().unwrap_or("");
    if path.is_empty() || !safe_markdown_image_url(url) {
        return false;
    }
    PathBuf::from(path).components().all(|component| {
        matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    })
}

fn markdown_plain_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match &node.kind {
            NodeKind::MarkdownText(text)
            | NodeKind::MarkdownInlineCode(text)
            | NodeKind::MarkdownRawHtml(text) => out.push_str(text),
            NodeKind::InlineMath(math) => {
                out.push('$');
                out.push_str(math);
                out.push('$');
            }
            NodeKind::MarkdownSoftBreak | NodeKind::MarkdownHardBreak => out.push(' '),
            _ => out.push_str(&markdown_plain_text(&node.children)),
        }
    }
    out
}

fn write_markdown_table(
    out: &mut String,
    table: &Node,
    alignments: &[MarkdownAlignment],
    ctx: &mut RenderCtx<'_>,
) {
    let id = ctx.idgen.next("md");
    record_container(ctx, &id, &table.span, None);
    write!(
        out,
        r#"<div class="latex-tabular-scroll md-table-scroll" id="{id}" data-src="{src}"><table class="latex-tabular md-table">"#,
        id = escape_attr(&id),
        src = escape_attr(&data_src(&table.span)),
    )
    .unwrap();

    let mut body_open = false;
    for child in &table.children {
        match &child.kind {
            NodeKind::MarkdownTableHead => {
                out.push_str("<thead><tr>");
                write_markdown_table_cells(out, &child.children, alignments, true, ctx);
                out.push_str("</tr></thead>");
            }
            NodeKind::MarkdownTableRow => {
                if !body_open {
                    out.push_str("<tbody>");
                    body_open = true;
                }
                let row_id = ctx.idgen.next("md");
                record_container(ctx, &row_id, &child.span, None);
                write!(
                    out,
                    r#"<tr id="{id}" data-src="{src}">"#,
                    id = escape_attr(&row_id),
                    src = escape_attr(&data_src(&child.span)),
                )
                .unwrap();
                write_markdown_table_cells(out, &child.children, alignments, false, ctx);
                out.push_str("</tr>");
            }
            _ => write_node(out, child, ctx),
        }
    }
    if body_open {
        out.push_str("</tbody>");
    }
    out.push_str("</table></div>");
}

fn write_markdown_table_cells(
    out: &mut String,
    cells: &[Node],
    alignments: &[MarkdownAlignment],
    header: bool,
    ctx: &mut RenderCtx<'_>,
) {
    let tag = if header { "th" } else { "td" };
    for (index, cell) in cells.iter().enumerate() {
        if !matches!(cell.kind, NodeKind::MarkdownTableCell) {
            write_node(out, cell, ctx);
            continue;
        }
        let alignment = match alignments
            .get(index)
            .copied()
            .unwrap_or(MarkdownAlignment::None)
        {
            MarkdownAlignment::None | MarkdownAlignment::Left => "align-left",
            MarkdownAlignment::Center => "align-center",
            MarkdownAlignment::Right => "align-right",
        };
        let id = ctx.idgen.next("md");
        record_container(ctx, &id, &cell.span, None);
        write!(
            out,
            r#"<{tag} class="latex-tabular-cell {alignment}" id="{id}" data-src="{src}">"#,
            id = escape_attr(&id),
            src = escape_attr(&data_src(&cell.span)),
        )
        .unwrap();
        write_children(out, &cell.children, ctx);
        write!(out, "</{tag}>").unwrap();
    }
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
        NodeKind::TextColor { model, color } => {
            let inline = n.children.iter().all(is_top_level_inline_node);
            let tag = if inline { "span" } else { "div" };
            let id = ctx.idgen.next("srcw");
            if inline {
                record(ctx, &id, &n.span, None);
            } else {
                record_container(ctx, &id, &n.span, None);
            }
            if let Some(css) = resolve_color_css(model.as_deref(), color) {
                write!(
                    out,
                    r#"<{tag} class="src-word text-color" id="{id}" data-src="{src}" style="color:{css}">"#,
                    src = escape_attr(&data_src(&n.span)),
                )
                .unwrap();
            } else {
                write!(
                    out,
                    r#"<{tag} class="src-word" id="{id}" data-src="{src}">"#,
                    src = escape_attr(&data_src(&n.span)),
                )
                .unwrap();
            }
            write_children_with_initial_trim(out, &n.children, ctx, false);
            write!(out, "</{tag}>").unwrap();
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
            // Block-level: selectable, but the cursor flash skips it (so moving
            // the cursor onto a heading doesn't flash the whole heading line).
            record_block(ctx, &id, &n.span, label.as_deref());
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
            kind_word,
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
            // Per-type class for color-coding (theorem/lemma/proposition/…). Key
            // off the resolved title word ("Lemma") rather than the env name so an
            // abbreviated `\newtheorem{lem}{Lemma}` still color-codes as a lemma;
            // fall back to the env name when no title word is known.
            let type_word = if kind_word.is_empty() { env } else { kind_word };
            let type_class = format!("thm-type-{}", sanitize_id(&type_word.to_lowercase()));
            // Heading word resolved from the preamble's `\newtheorem` title;
            // fall back to capitalizing the env name for legacy/empty nodes.
            let kind_label = if kind_word.is_empty() {
                // `env` is an attacker-controllable `\newtheorem{...}` name; it
                // is written into heading text, so it must be HTML-escaped.
                escape_html(&capitalize(env))
            } else {
                escape_html(kind_word)
            };
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
            let fancy_class = if ctx.fancy_theorems { " thm-fancy" } else { "" };
            let role_pill = if ctx.fancy_theorems {
                role_pill_html(*role)
            } else {
                String::new()
            };
            let heading_punctuation = if ctx.fancy_theorems { "" } else { "." };
            writeln!(
                out,
                r#"<div class="thm{fancy_class} {env_class} {type_class} {role_class}" id="{id}" data-src="{src}"{refkey}>"#,
                // `env` is an attacker-controllable `\newtheorem{...}` name;
                // `sanitize_id` keeps it a valid class token and prevents it
                // from breaking out of the attribute (stored-XSS otherwise).
                env_class = format_args!("env-{}", sanitize_id(env)),
                fancy_class = fancy_class,
                type_class = type_class,
                role_class = role_class,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                refkey = refkey_attr(label.as_deref()),
            )
            .unwrap();
            writeln!(
                out,
                r#"<div class="thm-head"><span class="thm-kind">{kind_label}</span>{num_html}{name_html}{role_pill}{heading_punctuation}</div>"#,
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
            // Per-row source spans: forward, an editor selection highlights
            // the individual align/gather rows it covers (the client maps
            // these indices to the SVG table rows); backward, a click on a
            // row jumps to that row's own source position. Only for genuinely
            // multi-row blocks; single equations keep the whole-block anchor.
            let row_spans = math_row_spans(body, n.span.start.line);
            if row_spans.len() > 1 {
                ctx.sync
                    .record_math_rows(id.clone(), n.span.file.clone(), row_spans);
            }
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
            // Byte spans of each row's source inside copy_tex, so a click on
            // a rendered row can copy exactly that row's LaTeX (empty attr
            // suppressed for single-row blocks).
            let row_tex_spans = {
                let prefix_len = match env {
                    Some(e) => format!(r"\begin{{{e}}}").len(),
                    None => r"\[".len(),
                };
                let spans = math_row_tex_spans(body, prefix_len);
                if spans.is_empty() {
                    String::new()
                } else {
                    format!(r#" data-row-tex-spans="{spans}""#)
                }
            };
            writeln!(
                out,
                r#"<div class="math display" id="{id}" data-src="{src}"{refkey} data-hash="{hash}" data-tex="{copy_tex}"{row_spans} data-mathjax-tex="{mathjax_tex}" tabindex="0" title="Copy as LaTeX">{aliases}{row_refkeys}<span class="math-source">{math}</span>{num_html}</div>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                refkey = if row_numbers.is_empty() { refkey_attr(label.as_deref()) } else { String::new() },
                aliases = alias_html,
                row_refkeys = row_refkey_html,
                math = escape_math(&math),
                hash = hash,
                copy_tex = escape_attr(&copy_tex),
                row_spans = row_tex_spans,
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
            let (l, r) = citation_delimiters(ctx.bib_style);
            write!(
                out,
                r#"<span class="cite-group" id="{id}" data-src="{src}">{l}{}{r}</span>"#,
                citation_links_html(keys, ctx.labels),
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
        NodeKind::MarkdownParagraph => {
            let id = ctx.idgen.next("md");
            record_container(ctx, &id, &n.span, None);
            let has_block_child = n.children.iter().any(|child| {
                matches!(
                    child.kind,
                    NodeKind::DisplayMath { .. }
                        | NodeKind::MarkdownCodeBlock { .. }
                        | NodeKind::MarkdownList { .. }
                        | NodeKind::MarkdownBlockQuote
                        | NodeKind::MarkdownTable { .. }
                )
            });
            let tag = if has_block_child { "div" } else { "p" };
            write!(
                out,
                r#"<{tag} class="para md-para" id="{id}" data-src="{src}">"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            writeln!(out, "</{tag}>").unwrap();
        }
        NodeKind::MarkdownHeading { level } => {
            let id = ctx.idgen.next("sec");
            record_block(ctx, &id, &n.span, None);
            let h = (*level).clamp(1, 6);
            write!(
                out,
                r#"<h{h} id="{id}" class="sec-h{h} md-heading" data-src="{src}">"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            writeln!(out, "</h{h}>").unwrap();
        }
        NodeKind::MarkdownText(text) => {
            let id = ctx.idgen.next("md");
            record(ctx, &id, &n.span, None);
            write!(
                out,
                r#"<span class="src-word md-text" id="{id}" data-src="{src}">{text}</span>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                text = escape_html(text),
            )
            .unwrap();
        }
        NodeKind::MarkdownEmphasis
        | NodeKind::MarkdownStrong
        | NodeKind::MarkdownStrikethrough
        | NodeKind::MarkdownSuperscript
        | NodeKind::MarkdownSubscript => {
            let (tag, class) = match n.kind {
                NodeKind::MarkdownEmphasis => ("em", "md-emphasis"),
                NodeKind::MarkdownStrong => ("strong", "md-strong"),
                NodeKind::MarkdownStrikethrough => ("del", "md-strikethrough"),
                NodeKind::MarkdownSuperscript => ("sup", "md-superscript"),
                NodeKind::MarkdownSubscript => ("sub", "md-subscript"),
                _ => unreachable!(),
            };
            let id = ctx.idgen.next("md");
            record(ctx, &id, &n.span, None);
            write!(
                out,
                r#"<{tag} class="{class}" id="{id}" data-src="{src}">"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            write!(out, "</{tag}>").unwrap();
        }
        NodeKind::MarkdownLink { destination, title } => {
            let id = ctx.idgen.next("md");
            record(ctx, &id, &n.span, None);
            if safe_markdown_url(destination) {
                let title_attr = markdown_title_attr(title.as_deref());
                write!(
                    out,
                    r#"<a class="md-link" id="{id}" data-src="{src}" href="{href}"{title}>"#,
                    id = escape_attr(&id),
                    src = escape_attr(&data_src(&n.span)),
                    href = escape_attr(destination),
                    title = title_attr,
                )
                .unwrap();
                write_children(out, &n.children, ctx);
                out.push_str("</a>");
            } else {
                write!(
                    out,
                    r#"<span class="md-link md-url-rejected" id="{id}" data-src="{src}" title="unsafe URL removed">"#,
                    id = escape_attr(&id),
                    src = escape_attr(&data_src(&n.span)),
                )
                .unwrap();
                write_children(out, &n.children, ctx);
                out.push_str("</span>");
            }
        }
        NodeKind::MarkdownImage { destination, title } => {
            let id = ctx.idgen.next("md");
            record(ctx, &id, &n.span, None);
            let alt = markdown_plain_text(&n.children);
            if safe_markdown_image_url(destination) {
                let src = markdown_image_url(destination, ctx.local_asset_base);
                write!(
                    out,
                    r#"<img class="md-image" id="{id}" data-src="{data_src}" src="{src}" alt="{alt}"{title} loading="lazy" decoding="async">"#,
                    id = escape_attr(&id),
                    data_src = escape_attr(&data_src(&n.span)),
                    src = escape_attr(&src),
                    alt = escape_attr(&alt),
                    title = markdown_title_attr(title.as_deref()),
                )
                .unwrap();
            } else {
                write!(
                    out,
                    r#"<span class="md-image-alt md-url-rejected" id="{id}" data-src="{src}">[{alt}]</span>"#,
                    id = escape_attr(&id),
                    src = escape_attr(&data_src(&n.span)),
                    alt = escape_html(&alt),
                )
                .unwrap();
            }
        }
        NodeKind::MarkdownBlockQuote => {
            let id = ctx.idgen.next("quote");
            record_container(ctx, &id, &n.span, None);
            write!(
                out,
                r#"<blockquote class="quote md-blockquote" id="{id}" data-src="{src}">"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            out.push_str("</blockquote>");
        }
        NodeKind::MarkdownInlineCode(code) => {
            let id = ctx.idgen.next("md");
            record(ctx, &id, &n.span, None);
            write!(
                out,
                r#"<code class="md-inline-code" id="{id}" data-src="{src}">{code}</code>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                code = escape_html(code),
            )
            .unwrap();
        }
        NodeKind::MarkdownCodeBlock { language, code } => {
            let id = ctx.idgen.next("md");
            record_container(ctx, &id, &n.span, None);
            let language_class = language
                .as_deref()
                .map(sanitize_id)
                .filter(|s| !s.is_empty())
                .map(|s| format!(" language-{s}"))
                .unwrap_or_default();
            writeln!(
                out,
                r#"<pre class="md-code-block" id="{id}" data-src="{src}"><code class="md-code{language_class}">{code}</code></pre>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                code = escape_html(code),
            )
            .unwrap();
        }
        NodeKind::MarkdownList { ordered, start } => {
            let id = ctx.idgen.next("md");
            record_container(ctx, &id, &n.span, None);
            let tag = if *ordered { "ol" } else { "ul" };
            let start_attr = if *ordered {
                start
                    .filter(|start| *start != 1)
                    .map(|start| format!(r#" start="{start}""#))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            write!(
                out,
                r#"<{tag} class="latex-list md-list" id="{id}" data-src="{src}"{start_attr}>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            writeln!(out, "</{tag}>").unwrap();
        }
        NodeKind::MarkdownListItem => {
            let id = ctx.idgen.next("md");
            record_container(ctx, &id, &n.span, None);
            write!(
                out,
                r#"<li class="item-body md-list-item" id="{id}" data-src="{src}">"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            out.push_str("</li>");
        }
        NodeKind::MarkdownTaskMarker { checked } => {
            let id = ctx.idgen.next("md");
            record(ctx, &id, &n.span, None);
            let checked_attr = if *checked { " checked" } else { "" };
            write!(
                out,
                r#"<input class="md-task-marker" id="{id}" data-src="{src}" type="checkbox" disabled{checked_attr}>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
        }
        NodeKind::MarkdownTable { alignments } => {
            write_markdown_table(out, n, alignments, ctx);
        }
        NodeKind::MarkdownTableHead => {
            out.push_str("<thead><tr>");
            write_children(out, &n.children, ctx);
            out.push_str("</tr></thead>");
        }
        NodeKind::MarkdownTableRow => {
            out.push_str("<tr>");
            write_children(out, &n.children, ctx);
            out.push_str("</tr>");
        }
        NodeKind::MarkdownTableCell => {
            out.push_str("<td class=\"latex-tabular-cell\">");
            write_children(out, &n.children, ctx);
            out.push_str("</td>");
        }
        NodeKind::MarkdownDefinitionList => {
            out.push_str("<dl class=\"md-definition-list\">");
            write_children(out, &n.children, ctx);
            out.push_str("</dl>");
        }
        NodeKind::MarkdownDefinitionTerm => {
            out.push_str("<dt>");
            write_children(out, &n.children, ctx);
            out.push_str("</dt>");
        }
        NodeKind::MarkdownDefinitionDescription => {
            out.push_str("<dd>");
            write_children(out, &n.children, ctx);
            out.push_str("</dd>");
        }
        NodeKind::MarkdownSoftBreak => out.push('\n'),
        NodeKind::MarkdownHardBreak => {
            let id = ctx.idgen.next("md");
            record(ctx, &id, &n.span, None);
            write!(
                out,
                r#"<br class="md-hard-break" id="{id}" data-src="{src}">"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
        }
        NodeKind::MarkdownRule => {
            let id = ctx.idgen.next("md");
            record_block(ctx, &id, &n.span, None);
            write!(
                out,
                r#"<hr class="md-rule" id="{id}" data-src="{src}">"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
        }
        NodeKind::MarkdownFootnoteDefinition { label } => {
            let id = format!("md-fn-{}", sanitize_id(label));
            record_container(ctx, &id, &n.span, Some(label));
            write!(
                out,
                r#"<section class="md-footnote" id="{id}" data-src="{src}"><sup>{label}</sup> "#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                label = escape_html(label),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            out.push_str("</section>");
        }
        NodeKind::MarkdownFootnoteReference { label } => {
            let id = ctx.idgen.next("md");
            record(ctx, &id, &n.span, None);
            write!(
                out,
                r##"<sup class="md-footnote-ref" id="{id}" data-src="{src}"><a href="#md-fn-{target}">{label}</a></sup>"##,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                target = escape_attr(&sanitize_id(label)),
                label = escape_html(label),
            )
            .unwrap();
        }
        NodeKind::MarkdownRawHtml(raw) => {
            let id = ctx.idgen.next("md");
            record(ctx, &id, &n.span, None);
            write!(
                out,
                r#"<code class="md-raw-html" id="{id}" data-src="{src}">{raw}</code>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                raw = escape_html(raw),
            )
            .unwrap();
        }
        NodeKind::MarkdownRawHtmlBlock => {
            let id = ctx.idgen.next("md");
            record_container(ctx, &id, &n.span, None);
            write!(
                out,
                r#"<pre class="md-raw-html-block" id="{id}" data-src="{src}">"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_children(out, &n.children, ctx);
            out.push_str("</pre>");
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
                tikz if is_tikz_environment(tikz) => {
                    out.push_str(&tikz_html(tikz, body, &n.span, ctx));
                }
                tabular if is_tabular_environment(tabular) => {
                    let number = (tabular == "longtable")
                        .then(|| ctx.labels.float_number_for_span(&n.span))
                        .flatten();
                    if let Some(table) =
                        render_tabular_with_number(tabular, body, ctx.labels, number)
                    {
                        out.push_str(&table);
                    } else {
                        write_opaque_environment(out, tabular, body);
                    }
                }
                "figure" | "figure*" | "table" | "table*" => {
                    // Tables are retained opaquely so their alignment syntax is
                    // not mistaken for prose. Recover the first live nested
                    // tabular here, using the same comment/definition-aware
                    // environment scanner as TikZ. If a float contains both,
                    // the table is the primary asset and avoids duplicating a
                    // diagram that belongs to one of its cells.
                    let tabular_asset =
                        first_nested_tabular(body).and_then(|(tabular_env, tabular_body)| {
                            render_tabular(&tabular_env, &tabular_body, ctx.labels)
                        });
                    let rendered_asset = tabular_asset.or_else(|| {
                        first_nested_tikz(body).map(|(tikz_env, tikz_body)| {
                            tikz_html(&tikz_env, &tikz_body, &n.span, ctx)
                        })
                    });
                    write_float_placeholder(
                        out,
                        env,
                        body,
                        ctx.labels,
                        ctx.labels.float_number_for_span(&n.span),
                        rendered_asset.as_deref(),
                    );
                }
                _ => {
                    write_opaque_environment(out, env, body);
                }
            }
        }
        NodeKind::UnsupportedEnvBoundary { env, boundary } => {
            let id = ctx.idgen.next("unsupported-env");
            record(ctx, &id, &n.span, None);
            let (class, latex, aria) = match boundary {
                EnvironmentBoundary::Begin => (
                    "begin",
                    format!(r"\begin{{{env}}}"),
                    format!(
                        "Unsupported LaTeX environment begins: {env}; contents are shown without environment formatting"
                    ),
                ),
                EnvironmentBoundary::End => (
                    "end",
                    format!(r"\end{{{env}}}"),
                    format!("Unsupported LaTeX environment ends: {env}"),
                ),
                EnvironmentBoundary::MissingEnd => (
                    "missing-end",
                    format!(r"\end{{{env}}}"),
                    format!("Missing end of unsupported LaTeX environment: {env}"),
                ),
            };
            let missing = if matches!(boundary, EnvironmentBoundary::MissingEnd) {
                r#"<span class="unsupported-env-missing" aria-hidden="true"> missing</span>"#
            } else {
                ""
            };
            writeln!(
                out,
                r#"<div class="unsupported-env-boundary unsupported-env-{class}" id="{id}" data-env="{env}" data-src="{src}" role="note" aria-label="{aria}" title="MathPreview does not handle this environment"><code aria-hidden="true">{latex}</code>{missing}<span class="unsupported-env-label" aria-hidden="true">unsupported</span></div>"#,
                class = class,
                id = escape_attr(&id),
                env = escape_attr(env),
                src = escape_attr(&data_src(&n.span)),
                aria = escape_attr(&aria),
                latex = escape_html(&latex),
                missing = missing,
            )
            .unwrap();
        }
        NodeKind::Quote { env } => {
            let id = ctx.idgen.next("quote");
            record_container(ctx, &id, &n.span, None);
            writeln!(
                out,
                r#"<blockquote class="quote env-{env}" id="{id}" data-src="{src}">"#,
                env = escape_attr(&sanitize_id(env)),
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_chunked_children(out, &n.children, ctx);
            out.push_str("</blockquote>\n");
        }
        NodeKind::Alignment { kind } => {
            let (class, env) = match kind {
                TextAlignment::Center => ("align-center", "center"),
                TextAlignment::FlushLeft => ("align-flush-left", "flushleft"),
                TextAlignment::FlushRight => ("align-flush-right", "flushright"),
            };
            let id = ctx.idgen.next("alignment");
            record_container(ctx, &id, &n.span, None);
            writeln!(
                out,
                r#"<div class="text-alignment {class}" id="{id}" data-env="{env}" data-src="{src}">"#,
                class = class,
                id = escape_attr(&id),
                env = env,
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            write_chunked_children(out, &n.children, ctx);
            out.push_str("</div>\n");
        }
        NodeKind::Letter { recipient } => {
            let address = ctx
                .preamble
                .letter_address
                .as_deref()
                .filter(|value| !value.trim().is_empty());
            let date = ctx
                .preamble
                .date
                .as_deref()
                .filter(|value| !value.trim().is_empty());
            let has_address = address.is_some();
            let id = ctx.idgen.next("letter");
            record_container(ctx, &id, &n.span, None);
            writeln!(
                out,
                r#"<section class="letter{}" id="{id}" data-src="{src}">"#,
                if has_address {
                    " letter-has-address"
                } else {
                    ""
                },
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();

            if address.is_some() || date.is_some() {
                out.push_str(r#"<header class="letter-head"><div class="letter-from">"#);
                if let Some(address) = address {
                    write!(
                        out,
                        r#"<div class="letter-address">{}</div>"#,
                        render_latex_text_with_math(address, ctx.labels),
                    )
                    .unwrap();
                }
                if let Some(date) = date {
                    write!(
                        out,
                        r#"<div class="letter-date">{}</div>"#,
                        render_latex_text_with_math(date, ctx.labels),
                    )
                    .unwrap();
                }
                out.push_str("</div></header>");
            }

            if !recipient.trim().is_empty() {
                write!(
                    out,
                    r#"<div class="letter-recipient">{}</div>"#,
                    render_latex_text_with_math(recipient, ctx.labels),
                )
                .unwrap();
            }

            out.push_str(r#"<div class="letter-body">"#);
            write_chunked_children(out, &n.children, ctx);
            out.push_str("</div>");

            if !has_address {
                let location = ctx
                    .preamble
                    .letter_location
                    .as_deref()
                    .filter(|value| !value.trim().is_empty());
                let telephone = ctx
                    .preamble
                    .letter_telephone
                    .as_deref()
                    .filter(|value| !value.trim().is_empty());
                if location.is_some() || telephone.is_some() {
                    out.push_str(r#"<footer class="letter-contact">"#);
                    if let Some(location) = location {
                        write!(
                            out,
                            r#"<div class="letter-location">{}</div>"#,
                            render_latex_text_with_math(location, ctx.labels),
                        )
                        .unwrap();
                    }
                    if let Some(telephone) = telephone {
                        write!(
                            out,
                            r#"<div class="letter-telephone">{}</div>"#,
                            render_latex_text_with_math(telephone, ctx.labels),
                        )
                        .unwrap();
                    }
                    out.push_str("</footer>");
                }
            }
            out.push_str("</section>\n");
        }
        NodeKind::LetterOpening { text } => {
            let id = ctx.idgen.next("letter-part");
            record(ctx, &id, &n.span, None);
            writeln!(
                out,
                r#"<div class="letter-opening" id="{id}" data-src="{src}">{text}</div>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                text = render_latex_text_with_math(text, ctx.labels),
            )
            .unwrap();
        }
        NodeKind::LetterClosing { text } => {
            let signature = ctx
                .preamble
                .letter_signature
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| {
                    ctx.preamble
                        .letter_name
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                });
            let id = ctx.idgen.next("letter-part");
            record(ctx, &id, &n.span, None);
            writeln!(
                out,
                r#"<div class="letter-closing" id="{id}" data-src="{src}"><div class="letter-closing-text">{text}</div>{signature}</div>"#,
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
                text = render_latex_text_with_math(text, ctx.labels),
                signature = signature
                    .map(|value| format!(
                        r#"<div class="letter-signature">{}</div>"#,
                        render_latex_text_with_math(value, ctx.labels),
                    ))
                    .unwrap_or_default(),
            )
            .unwrap();
        }
        NodeKind::Callout { env, class, title } => {
            let id = ctx.idgen.next("callout");
            record_container(ctx, &id, &n.span, None);
            writeln!(
                out,
                r#"<div class="callout callout-{cls} env-{env}" id="{id}" data-src="{src}">"#,
                cls = escape_attr(class),
                env = escape_attr(&sanitize_id(env)),
                id = escape_attr(&id),
                src = escape_attr(&data_src(&n.span)),
            )
            .unwrap();
            if let Some(t) = title {
                writeln!(
                    out,
                    r#"<div class="callout-head">{}</div>"#,
                    render_latex_text_with_math(t, ctx.labels),
                )
                .unwrap();
            }
            out.push_str(r#"<div class="callout-body">"#);
            write_chunked_children(out, &n.children, ctx);
            out.push_str("</div></div>\n");
        }
        NodeKind::OpaqueCmd { name, raw } => {
            match name.as_str() {
                "today" => out.push_str("(today)"),
                "LaTeX" => out.push_str("LaTeX"),
                "TeX" => out.push_str("TeX"),
                "inline-literal" => {
                    let id = ctx.idgen.next("srcw");
                    record(ctx, &id, &n.span, None);
                    write!(
                        out,
                        r#"<code class="inline-literal" id="{id}" data-src="{src}">{payload}</code>"#,
                        id = escape_attr(&id),
                        src = escape_attr(&data_src(&n.span)),
                        payload = escape_html(raw),
                    )
                    .unwrap();
                }
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
                // Spacing / layout no-ops: emit nothing — and do NOT touch
                // counters (sharing an arm with `restartsteps` used to reset
                // the proof step counter on every `\vspace` / `\noindent`).
                "vspace" | "hspace" | "smallskip" | "medskip" | "bigskip" | "newpage"
                | "clearpage" | "noindent" | "indent" | "linebreak" | "pagebreak" | "thanks" => {}
                "restartsteps" => {
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
                        let id = ctx.idgen.next("sn");
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
                // `\footnote[opt]{text}`. HTML has no footnote, and a continuous
                // preview has no "page foot", so render a numbered superscript
                // marker whose note pops up on hover / keyboard focus. The note
                // lives in the DOM right after the marker (so its math typesets
                // and screen readers reach it via aria-describedby); CSS reveals
                // it on `:hover` / `:focus-within`.
                "footnote" => {
                    if let Some(call) = latex_command_call(raw, "footnote") {
                        let content = render_latex_text_with_math(&call.arg, ctx.labels);
                        let num = next_footnote_number();
                        // Register the marker as a source-jump anchor (inline
                        // footnotes nested in a command arg can't — no ctx/span).
                        record(ctx, &format!("fn-{num}"), &n.span, None);
                        out.push_str(&footnote_html(num, &content, Some(&data_src(&n.span))));
                    }
                }
                // Inline review commands (marktext): \add / \remove / \highlight
                // take `[color]{content}`; render the content (text OR math via
                // render_latex_text_with_math) wrapped in a decoration span, with
                // the optional color sanitized into the relevant CSS property.
                "add" | "remove" | "highlight" => {
                    if let Some(call) = latex_command_call(raw, name) {
                        let content = render_latex_text_with_math(&call.arg, ctx.labels);
                        let (klass, prop) = match name.as_str() {
                            "add" => ("review-add", "text-decoration-color"),
                            "remove" => ("review-remove", "text-decoration-color"),
                            _ => ("review-highlight", "background-color"),
                        };
                        let style = call
                            .optional
                            .as_deref()
                            .and_then(|c| resolve_color_css(None, c))
                            .map(|css| format!(r#" style="{prop}:{css}""#))
                            .unwrap_or_default();
                        write!(out, r#"<span class="{klass}"{style}>{content}</span>"#).unwrap();
                    }
                }
                // \replace{old}{new}: struck-through old followed by underlined
                // new. Two required braces — the generic reader returns only the
                // first, so read both groups directly.
                "replace" => {
                    let after = raw
                        .find("replace")
                        .map(|i| i + "replace".len())
                        .unwrap_or(0);
                    let first = read_delim(raw, after, b'{', b'}');
                    let second = first
                        .as_ref()
                        .and_then(|(_, next)| read_delim(raw, *next, b'{', b'}'));
                    if let (Some((a, _)), Some((b, _))) = (first.as_ref(), second.as_ref()) {
                        let a_html = render_latex_text_with_math(a, ctx.labels);
                        let b_html = render_latex_text_with_math(b, ctx.labels);
                        write!(
                            out,
                            r#"<span class="review-remove">{a_html}</span> <span class="review-add">{b_html}</span>"#,
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
                        let id = ctx.idgen.next("sn");
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
                    // unknown ones fall back to their content argument. Wrap the
                    // result in a source-mapped span so a source-jump click on,
                    // say, an `\emph{…}`-wrapped theorem statement lands on THIS
                    // command's line — without a data-src of its own the click
                    // would walk up to the enclosing box and jump there instead.
                    let rendered = render_inline_latex(raw, ctx.labels);
                    if rendered.is_empty() {
                        // Nothing visible (e.g. a spacing/no-op command).
                    } else {
                        let id = ctx.idgen.next("srcw");
                        record(ctx, &id, &n.span, None);
                        write!(
                            out,
                            r#"<span class="src-word" id="{id}" data-src="{src}">{rendered}</span>"#,
                            id = escape_attr(&id),
                            src = escape_attr(&data_src(&n.span)),
                            rendered = rendered,
                        )
                        .unwrap();
                    }
                }
            }
        }
    }
}

fn write_children(out: &mut String, children: &[Node], ctx: &mut RenderCtx) {
    write_children_with_initial_trim(out, children, ctx, true);
}

fn write_children_with_initial_trim(
    out: &mut String,
    children: &[Node],
    ctx: &mut RenderCtx,
    trim_initial_text: bool,
) {
    let mut trim_next_text = trim_initial_text;
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
    match &node.kind {
        NodeKind::TextColor { .. } => node.children.iter().all(is_top_level_inline_node),
        NodeKind::InlineMath(_)
        | NodeKind::Ref { .. }
        | NodeKind::Cite { .. }
        | NodeKind::OpaqueCmd { .. } => true,
        _ => false,
    }
}

fn is_chunked_block_child(node: &Node) -> bool {
    matches!(
        &node.kind,
        NodeKind::DisplayMath { .. }
            | NodeKind::Subequations { .. }
            | NodeKind::List { .. }
            | NodeKind::OpaqueEnv { .. }
            | NodeKind::UnsupportedEnvBoundary { .. }
            | NodeKind::Letter { .. }
            | NodeKind::LetterOpening { .. }
            | NodeKind::LetterClosing { .. }
    ) || matches!(&node.kind, NodeKind::TextColor { .. })
        && !node.children.iter().all(is_top_level_inline_node)
}

/// Take a paragraph-chunk buffer and wrap it in a hashed `proof-para` span
/// so the client can sub-block-diff proof/theorem bodies — replacing only
/// the paragraphs whose hash changed instead of the entire block. Returns
/// `None` for empty / whitespace-only chunks so we don't pollute the output
/// with no-op spans.
///
/// The `data-subhash` is computed over the chunk's STABLE diff source (the
/// same IDs / data-src normalization the top-level block diff uses) so a
/// single-paragraph edit doesn't ripple chunk hashes downstream just
/// because the `srcw-N` id counter shifted.
fn build_chunk(buf: &mut String) -> Option<String> {
    let chunk = std::mem::take(buf);
    if chunk.trim().is_empty() {
        return None;
    }
    let hash = fnv_hash(&stable_block_diff_source(&chunk));
    Some(format!(
        r#"<span class="proof-para" data-subhash="{hash}">{chunk}</span>"#
    ))
}

/// Append one body-child segment to `out`, and — when capturing for the
/// outermost block — record it as a `SubChunk` keyed by its stable hash.
/// Each `seg` is exactly one top-level element, so the recorded chunk
/// indices line up 1:1 with the body container's element children.
fn record_seg(out: &mut String, chunks: &mut Vec<SubChunk>, record: bool, seg: String) {
    out.push_str(&seg);
    if record {
        chunks.push(SubChunk {
            diff_hash: fnv_hash(&stable_block_diff_source(&seg)),
            html: seg,
        });
    }
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
    // Only the outermost chunked body captures sub-block structure; a nested
    // theorem-like inside a proof body renders normally without clobbering
    // the outer block's chunks.
    let capture = ctx.chunk_depth == 0;
    ctx.chunk_depth += 1;
    let body_start = out.len();
    let mut chunks: Vec<SubChunk> = Vec::new();
    // A child that expands to multiple sibling elements (only `subequations`
    // today) breaks the client's element-index addressing, so we abandon
    // capture for the whole block if we hit one.
    let mut sub_diffable = true;

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
                        if let Some(seg) = build_chunk(&mut chunk_buf) {
                            record_seg(out, &mut chunks, capture, seg);
                        }
                        let break_span = paragraph_break_span(&child.span, s, start, end);
                        if seen_content || previous_was_display {
                            record_seg(
                                out,
                                &mut chunks,
                                capture,
                                r#"<span class="para-break" aria-hidden="true"></span>"#.to_string(),
                            );
                        }
                        let mut anchor = String::new();
                        write_source_space_anchor(&mut anchor, &break_span, ctx);
                        record_seg(out, &mut chunks, capture, anchor);
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
            if let Some(seg) = build_chunk(&mut chunk_buf) {
                record_seg(out, &mut chunks, capture, seg);
            }
            // `subequations` emits a label anchor plus N display rows as
            // siblings; that breaks 1-chunk-per-element addressing, so give
            // up on sub-diffing this block (it still renders normally).
            if matches!(&child.kind, NodeKind::Subequations { .. }) {
                sub_diffable = false;
            }
            let mut seg = String::new();
            write_node(&mut seg, child, ctx);
            record_seg(out, &mut chunks, capture, seg);
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
    if let Some(seg) = build_chunk(&mut chunk_buf) {
        record_seg(out, &mut chunks, capture, seg);
    }

    let body_end = out.len();
    ctx.chunk_depth -= 1;
    if capture && sub_diffable {
        ctx.pending_sub = Some(PendingSub {
            body_start,
            body_end,
            children: chunks,
        });
    }
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

fn citation_delimiters(style: BibStyle) -> (char, char) {
    match style {
        BibStyle::AuthorYear => ('(', ')'),
        _ => ('[', ']'),
    }
}

fn citation_links_html(keys: &[String], labels: &LabelTable) -> String {
    keys.iter()
        .map(|key| {
            let number = labels.citation_number.get(key).copied().unwrap_or(0);
            match labels.citation_display.get(key) {
                Some(display) => format!(
                    r##"<a class="cite" href="#bib-{number}" data-key="{key}">{label}</a>"##,
                    key = escape_attr(key),
                    label = escape_html(display),
                ),
                None => format!(
                    r#"<span class="cite missing" data-key="{key}">{label}</span>"#,
                    key = escape_attr(key),
                    label = escape_html(key),
                ),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// LaTeX-text → HTML for strings extracted into AST fields (section titles,
/// theorem names, proof "of" args, omitref payloads). Handles a curated set
/// of inline commands so embedded `\ref` / `\emph` / `\textbf` etc. don't
/// reach MathJax or land in the output as raw `\name{...}` source.
pub(super) fn render_inline_latex(s: &str, labels: &LabelTable) -> String {
    let Some(_depth_guard) = InlineRenderDepthGuard::enter() else {
        return escape_html(s);
    };
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut color_span_open = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'~' {
            out.push('\u{00a0}');
            i += 1;
            continue;
        }
        // Inline math `$…$`. Title-like fields and the recursive inner content
        // of `\emph{…}` / `\textbf{…}` reach this function directly, so without
        // this branch the `$` and its body leak through as literal source. A
        // `$` nested inside a command argument arrives here via that recursion,
        // after the command's braces have already been consumed, so matching
        // the next unescaped `$` at this level is sufficient.
        if b == b'$' {
            let mut k = i + 1;
            while k < bytes.len() {
                if bytes[k] == b'\\' && k + 1 < bytes.len() {
                    k += 2;
                    continue;
                }
                if bytes[k] == b'$' {
                    break;
                }
                k += 1;
            }
            if k < bytes.len() {
                write_inline_math_span(&mut out, &s[i + 1..k]);
                i = k + 1;
                continue;
            }
            // Unbalanced `$` — emit it literally and move on.
            out.push('$');
            i += 1;
            continue;
        }
        // TeX inline/display delimiters that occur inside title-like fields,
        // table cells, or a scoped color group. These contexts cannot host a
        // block-level display node, so both forms use the lightweight inline
        // MathJax wrapper while preserving the formula itself.
        if b == b'\\' && matches!(bytes.get(i + 1), Some(b'(') | Some(b'[')) {
            let close = if bytes[i + 1] == b'(' { b')' } else { b']' };
            let mut k = i + 2;
            while k + 1 < bytes.len() {
                if bytes[k] == b'\\' && bytes[k + 1] == close {
                    break;
                }
                k += if bytes[k].is_ascii() {
                    1
                } else {
                    s[k..].chars().next().map_or(1, char::len_utf8)
                };
            }
            if k + 1 < bytes.len() {
                write_inline_math_span(&mut out, &s[i + 2..k]);
                i = k + 2;
                continue;
            }
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
        if b == b'%' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            // A TeX comment consumes the line ending as well.
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        if b != b'\\' {
            // LaTeX grouping braces have no visual effect — strip them so
            // text like `Hello {grouping} world` reads as `Hello grouping
            // world` rather than literally including the braces.
            if b == b'{' {
                if let Some(group_end) = crate::parser::tex_group_end(s, i, b'{', b'}') {
                    let inner_end = group_end - 1;
                    let mut k = i + 1;
                    while k < inner_end && bytes[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    let mut rendered_switch = false;
                    if bytes.get(k) == Some(&b'\\') {
                        let n_start = k + 1;
                        let mut n_end = n_start;
                        while n_end < inner_end && bytes[n_end].is_ascii_alphabetic() {
                            n_end += 1;
                        }
                        let wrap = match &s[n_start..n_end] {
                            "bf" | "bfseries" => Some(("<strong>", "</strong>")),
                            "em" | "it" | "itshape" | "emshape" => Some(("<em>", "</em>")),
                            "tt" | "ttfamily" => Some(("<code>", "</code>")),
                            "sc" | "scshape" => Some((r#"<span class="sc">"#, "</span>")),
                            _ => None,
                        };
                        if let Some((open, close)) = wrap {
                            out.push_str(open);
                            out.push_str(&render_inline_latex(&s[n_end..inner_end], labels));
                            out.push_str(close);
                            rendered_switch = true;
                        }
                    }
                    if !rendered_switch {
                        // Recurse for every ordinary TeX group. Stateful
                        // switches opened inside this call close at its end,
                        // restoring the surrounding group's color/font state.
                        out.push_str(&render_inline_latex(&s[i + 1..inner_end], labels));
                    }
                    i = group_end;
                    continue;
                }
                // Incomplete group during an edit: drop only the brace and
                // keep rendering the useful suffix.
                i += 1;
                continue;
            }
            if b == b'}' {
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
                // TeX's seven one-character text escapes. Handle these before
                // `%` can be mistaken for a comment, `$` for inline math, or
                // braces for grouping. `~`, `^`, and `\` are intentionally not
                // listed: their literal text forms are the named commands
                // handled below, while `\~n`, `\^o`, and `\\` retain their
                // accent / line-break meanings.
                match p {
                    b'#' | b'$' | b'%' | b'_' | b'{' | b'}' => {
                        out.push(p as char);
                        i = cmd_start + 1;
                        continue;
                    }
                    b'&' => {
                        out.push_str("&amp;");
                        i = cmd_start + 1;
                        continue;
                    }
                    _ => {}
                }
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
                    b',' | b';' | b':' | b'!' => {
                        i = cmd_start + 1;
                        continue;
                    }
                    b' ' => {
                        // TeX's control-space (`\ `) inserts ordinary
                        // interword glue. Preserve it as a breakable HTML
                        // space instead of swallowing it like `\,`/`\!`.
                        out.push(' ');
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

        if crate::parser::is_inline_literal_command(name) {
            if let Some((payload, next)) = crate::parser::inline_literal_payload(s, name, cmd_end) {
                write!(
                    out,
                    r#"<code class="inline-literal">{}</code>"#,
                    escape_html(&payload)
                )
                .unwrap();
                i = next;
                continue;
            }
        }
        if name == "string" {
            let token_start = skip_tex_argument_space(s, cmd_end);
            if token_start < bytes.len() {
                let token_end = if bytes[token_start] == b'\\' {
                    let mut end = token_start + 1;
                    if bytes
                        .get(end)
                        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'@')
                    {
                        while end < bytes.len()
                            && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'@')
                        {
                            end += 1;
                        }
                    } else if end < bytes.len() {
                        end += s[end..].chars().next().map_or(1, char::len_utf8);
                    }
                    end
                } else {
                    token_start + s[token_start..].chars().next().map_or(1, char::len_utf8)
                };
                out.push_str(&escape_html(&s[token_start..token_end]));
                i = token_end;
                continue;
            }
        }
        if matches!(name, "detokenize" | "unexpanded") {
            if let Some((payload, next)) = read_delim(s, cmd_end, b'{', b'}') {
                out.push_str(&escape_html(&payload));
                i = next;
                continue;
            }
        }

        // User text macros: \newcommand bodies (re-rendered) and TOML
        // [text-macros] HTML templates. Defined names win over the built-in
        // fallbacks below, matching \renewcommand intent.
        if let Some(next) = render_text_macro(&mut out, name, s, cmd_end, labels) {
            i = next;
            continue;
        }

        // The remaining three TeX-special characters have named text-mode
        // forms because `\~`, `\^`, and `\\` already mean accent/accent/line
        // break. Leave any following `{...}` group for the normal grouping
        // path: these commands take no arguments, and `{}` is commonly used
        // only to delimit the control word. User macro overrides are checked
        // first, preserving `\renewcommand` semantics.
        if let Some(symbol) = match name {
            "textbackslash" => Some('\\'),
            "textasciitilde" => Some('~'),
            "textasciicircum" => Some('^'),
            _ => None,
        } {
            out.push(symbol);
            // A space/comment after a control word is a TeX token separator,
            // not printed content. Authors who want a visible space use the
            // usual empty-group delimiter: `\textasciitilde{} word`.
            i = skip_tex_argument_space(s, cmd_end);
            continue;
        }

        // Built-in `\textcolor[model]{color}{text}` → colored span.
        if name == "textcolor" {
            if let Some(next) = render_textcolor(&mut out, s, cmd_end, labels) {
                i = next;
                continue;
            }
        }
        if name == "color" {
            if let Some((css, next)) = read_color_declaration(s, cmd_end) {
                if let Some(css) = css {
                    if color_span_open {
                        out.push_str("</span>");
                    }
                    write!(out, r#"<span class="text-color" style="color:{css}">"#).unwrap();
                    color_span_open = true;
                }
                i = next;
                continue;
            }
        }
        if name == "normalcolor" {
            if color_span_open {
                out.push_str("</span>");
            }
            out.push_str(r#"<span class="text-color text-color-normal">"#);
            color_span_open = true;
            i = cmd_end;
            continue;
        }
        if name == "colorbox" {
            if let Some(next) = render_color_box(&mut out, s, cmd_end, labels, false) {
                i = next;
                continue;
            }
        }
        if name == "fcolorbox" {
            if let Some(next) = render_color_box(&mut out, s, cmd_end, labels, true) {
                i = next;
                continue;
            }
        }
        if crate::numbering::is_citation_command(name) {
            if let Some((keys, next)) = crate::numbering::citation_call_after_command(s, cmd_end) {
                let (left, right) = citation_delimiters(labels.bib_style);
                write!(
                    out,
                    r#"<span class="cite-group">{left}{}{right}</span>"#,
                    citation_links_html(&keys, labels),
                )
                .unwrap();
                i = next;
                continue;
            }
        }

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
            ("begin", Some((env, next))) | ("end", Some((env, next))) => {
                let boundary = name;
                let latex = format!(r"\{boundary}{{{env}}}");
                let aria = format!(
                    "Unsupported LaTeX environment {boundary}s: {}; contents are shown without environment formatting",
                    env.trim()
                );
                write!(
                    out,
                    r#"<span class="unsupported-env-inline unsupported-env-{boundary}" data-env="{env}" role="note" aria-label="{aria}" title="MathPreview does not handle this environment"><code aria-hidden="true">{latex}</code></span>"#,
                    boundary = boundary,
                    env = escape_attr(env.trim()),
                    aria = escape_attr(&aria),
                    latex = escape_html(&latex),
                )
                .unwrap();
                i = next;
            }
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
            ("footnote", Some((inner, next))) => {
                // A footnote nested inside a command argument (section title,
                // \emph{…}, theorem statement, caption, …) reaches the inline
                // renderer, which has no RenderCtx — so number it via the
                // thread-local counter shared with the block path, keeping the
                // sequence in document order. No source-jump anchor here.
                let content = render_latex_text_with_math(inner, labels);
                out.push_str(&footnote_html(next_footnote_number(), &content, None));
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
    if color_span_open {
        out.push_str("</span>");
    }
    out
}

// ---------------------------------------------------------------------------
// User text macros (\newcommand expansion + TOML [text-macros] templates) and
// the \textcolor / \color built-ins, used by `render_inline_latex`.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TextMacroDef {
    n_args: usize,
    body: String,
    /// true  → TOML HTML template: emit `body` verbatim with `#k` replaced by
    ///         the *rendered* argument (template is trusted local config).
    /// false → `\newcommand` body: substitute raw args into `#k`, then
    ///         re-render the result as LaTeX.
    html: bool,
    /// Default for an optional first argument (`\newcommand[1][def]`).
    default: Option<String>,
}

struct InlineRenderDepthGuard;

impl InlineRenderDepthGuard {
    fn enter() -> Option<Self> {
        INLINE_RENDER_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_INLINE_RENDER_DEPTH {
                None
            } else {
                if current == 0 {
                    TEXT_MACRO_EXPANSIONS_LEFT
                        .with(|budget| budget.set(MAX_TEXT_MACRO_EXPANSIONS));
                    TEXT_MACRO_BYTES_LEFT
                        .with(|budget| budget.set(MAX_TEXT_MACRO_EXPANDED_BYTES));
                }
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for InlineRenderDepthGuard {
    fn drop(&mut self) {
        INLINE_RENDER_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

thread_local! {
    static TEXT_MACROS: std::cell::RefCell<std::collections::HashMap<String, TextMacroDef>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    static EXPAND_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static INLINE_RENDER_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static TEXT_MACRO_EXPANSIONS_LEFT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TEXT_MACRO_BYTES_LEFT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TEXT_MACRO_DOCUMENT_EXPANSIONS_LEFT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static TEXT_MACRO_DOCUMENT_BYTES_LEFT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// Sequential footnote number for the current render. Thread-local (like the
    /// macro table) so the inline-text renderer — which has no `&mut RenderCtx`
    /// — can number footnotes nested inside section titles, `\emph{…}`, theorem
    /// statements, etc. in the same document-order sequence as block footnotes.
    static FOOTNOTE_COUNTER: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const MAX_EXPAND_DEPTH: u32 = 32;
const MAX_INLINE_RENDER_DEPTH: u32 = 128;
const MAX_TEXT_MACRO_EXPANSIONS: usize = 1_024;
const MAX_TEXT_MACRO_EXPANDED_BYTES: usize = 1 << 20;
const MAX_DOCUMENT_TEXT_MACRO_EXPANSIONS: usize = 16_384;
const MAX_DOCUMENT_TEXT_MACRO_EXPANDED_BYTES: usize = 8 << 20;

/// Reset per-render footnote numbering. Called at the top of the render walk.
fn reset_footnote_counter() {
    FOOTNOTE_COUNTER.with(|c| c.set(0));
}

/// Allocate the next footnote number (1-based) for the current render.
fn next_footnote_number() -> usize {
    FOOTNOTE_COUNTER.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    })
}

/// Build a footnote: a numbered superscript marker whose note pops up on hover
/// or keyboard focus (the note sits in the DOM right after the marker so its
/// math typesets and screen readers reach it via `aria-describedby`). `src` is
/// the marker's `data-src` for source-jump; footnotes nested inside a command
/// argument are rendered without a span and pass `None`.
fn footnote_html(num: usize, content: &str, src: Option<&str>) -> String {
    let src_attr = src
        .map(|s| format!(r#" data-src="{}""#, escape_attr(s)))
        .unwrap_or_default();
    format!(
        r#"<span class="footnote"><sup class="footnote-ref" id="fn-{num}"{src_attr} tabindex="0" role="doc-noteref" aria-describedby="fnpop-{num}">{num}</sup><span class="footnote-pop" id="fnpop-{num}" role="doc-footnote">{content}</span></span>"#,
    )
}

/// Install the per-render text-macro table: the document's `\newcommand`s
/// (LaTeX, re-rendered) plus the TOML `[text-macros]` templates (HTML), with
/// TOML winning on name collision.
fn install_text_macros(preamble: &ExtractedPreamble, opts: &HtmlOptions) {
    let mut map: std::collections::HashMap<String, TextMacroDef> = std::collections::HashMap::new();
    for m in &preamble.macros {
        let name = m.name.trim_start_matches('\\').to_string();
        if name.is_empty() {
            continue;
        }
        map.insert(
            name,
            TextMacroDef {
                n_args: m.n_args as usize,
                body: m.body.clone(),
                html: false,
                default: m.default.clone(),
            },
        );
    }
    for (name, spec) in &opts.text_macros {
        let name = name.trim_start_matches('\\').trim().to_string();
        if name.is_empty() {
            continue;
        }
        map.insert(
            name,
            TextMacroDef {
                // Explicit n_args from the array form wins; otherwise infer
                // from the highest `#n` in the template.
                n_args: spec
                    .n_args
                    .map(|n| n as usize)
                    .unwrap_or_else(|| max_placeholder(&spec.html)),
                body: spec.html.clone(),
                html: true,
                default: spec.default.clone(),
            },
        );
    }
    TEXT_MACROS.with(|t| *t.borrow_mut() = map);
    TEXT_MACRO_DOCUMENT_EXPANSIONS_LEFT
        .with(|budget| budget.set(MAX_DOCUMENT_TEXT_MACRO_EXPANSIONS));
    TEXT_MACRO_DOCUMENT_BYTES_LEFT
        .with(|budget| budget.set(MAX_DOCUMENT_TEXT_MACRO_EXPANDED_BYTES));
}

/// Highest `#1`..`#9` index referenced in a template (its arg count).
fn max_placeholder(s: &str) -> usize {
    let b = s.as_bytes();
    let mut max = 0usize;
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'#' && b[i + 1].is_ascii_digit() && b[i + 1] != b'0' {
            max = max.max((b[i + 1] - b'0') as usize);
            i += 2;
        } else {
            i += 1;
        }
    }
    max
}

/// Expand `name` if it's a defined text macro. Reads its argument groups from
/// `s` at `from`, substitutes, appends to `out`, and returns the index past
/// the consumed input. `None` if `name` isn't a text macro.
fn render_text_macro(
    out: &mut String,
    name: &str,
    s: &str,
    from: usize,
    labels: &LabelTable,
) -> Option<usize> {
    let def = TEXT_MACROS.with(|t| t.borrow().get(name).cloned())?;
    let (args, next) = read_braced_args(s, from, def.n_args, def.default.as_deref());
    let depth = EXPAND_DEPTH.with(|d| d.get());
    if depth >= MAX_EXPAND_DEPTH {
        // Runaway / recursive expansion — stop expanding to break the loop.
        return Some(next);
    }
    if !reserve_text_macro_call() {
        return Some(next);
    }
    if def.html {
        let rendered: Vec<String> = args.iter().map(|a| render_inline_latex(a, labels)).collect();
        let Some(expanded) = fill_placeholders(&def.body, &rendered, text_macro_bytes_left())
        else {
            return Some(next);
        };
        consume_text_macro_bytes(expanded.len());
        out.push_str(&expanded);
    } else {
        let Some(expanded) = fill_placeholders(&def.body, &args, text_macro_bytes_left()) else {
            return Some(next);
        };
        consume_text_macro_bytes(expanded.len());
        EXPAND_DEPTH.with(|d| d.set(depth + 1));
        out.push_str(&render_inline_latex(&expanded, labels));
        EXPAND_DEPTH.with(|d| d.set(depth));
    }
    Some(next)
}

fn reserve_text_macro_call() -> bool {
    TEXT_MACRO_EXPANSIONS_LEFT.with(|calls| {
        TEXT_MACRO_DOCUMENT_EXPANSIONS_LEFT.with(|document_calls| {
            let remaining = calls.get();
            let document_remaining = document_calls.get();
            if remaining == 0 || document_remaining == 0 {
                false
            } else {
                calls.set(remaining - 1);
                document_calls.set(document_remaining - 1);
                true
            }
        })
    })
}

fn text_macro_bytes_left() -> usize {
    TEXT_MACRO_BYTES_LEFT.with(|local| {
        TEXT_MACRO_DOCUMENT_BYTES_LEFT.with(|document| local.get().min(document.get()))
    })
}

fn consume_text_macro_bytes(bytes: usize) {
    TEXT_MACRO_BYTES_LEFT.with(|budget| budget.set(budget.get().saturating_sub(bytes)));
    TEXT_MACRO_DOCUMENT_BYTES_LEFT
        .with(|budget| budget.set(budget.get().saturating_sub(bytes)));
}

/// Replace `#1`..`#9` in `template` with `args` (`#k` → `args[k-1]`, missing →
/// empty). `##` is a literal `#`.
fn fill_placeholders(template: &str, args: &[String], byte_limit: usize) -> Option<String> {
    let b = template.as_bytes();
    let mut output_len = 0usize;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' && i + 1 < b.len() {
            let c = b[i + 1];
            if c == b'#' {
                output_len = output_len.checked_add(1)?;
                i += 2;
                continue;
            }
            if c.is_ascii_digit() && c != b'0' {
                if let Some(a) = args.get((c - b'0') as usize - 1) {
                    output_len = output_len.checked_add(a.len())?;
                }
                i += 2;
                continue;
            }
        }
        let ch = template[i..].chars().next().unwrap();
        output_len = output_len.checked_add(ch.len_utf8())?;
        i += ch.len_utf8();
    }
    if output_len > byte_limit {
        return None;
    }

    let mut out = String::with_capacity(output_len);
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' && i + 1 < b.len() {
            let c = b[i + 1];
            if c == b'#' {
                out.push('#');
                i += 2;
                continue;
            }
            if c.is_ascii_digit() && c != b'0' {
                if let Some(a) = args.get((c - b'0') as usize - 1) {
                    out.push_str(a);
                }
                i += 2;
                continue;
            }
        }
        let ch = template[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    Some(out)
}

/// Read up to `n` argument groups starting at `from`. If `default` is set the
/// first arg is optional (`[..]` or the default); the rest are braced `{..}`.
/// Returns the raw arg bodies and the index past the last consumed group.
fn read_braced_args(s: &str, from: usize, n: usize, default: Option<&str>) -> (Vec<String>, usize) {
    let bytes = s.as_bytes();
    let mut args: Vec<String> = Vec::with_capacity(n);
    let mut i = from;
    let mut remaining = n;
    if let (Some(def), true) = (default, n > 0) {
        let mut k = i;
        while k < bytes.len() && bytes[k] == b' ' {
            k += 1;
        }
        if bytes.get(k) == Some(&b'[') {
            if let Some((val, next)) = read_delim(s, k, b'[', b']') {
                args.push(val);
                i = next;
            } else {
                args.push(def.to_string());
            }
        } else {
            args.push(def.to_string());
        }
        remaining -= 1;
    }
    for _ in 0..remaining {
        if let Some((val, next)) = read_delim(s, i, b'{', b'}') {
            args.push(val);
            i = next;
        } else {
            // Missing mandatory arg (e.g. `\foo` used bare) — empty, stop.
            args.push(String::new());
        }
    }
    (args, i)
}

/// `\textcolor[model]{color}{text}` → colored span. `None` if the shape
/// doesn't parse (so the caller falls back to generic handling).
fn render_textcolor(out: &mut String, s: &str, from: usize, labels: &LabelTable) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = skip_tex_argument_space(s, from);
    let mut model = None;
    if bytes.get(i) == Some(&b'[') {
        let (m, next) = read_delim(s, i, b'[', b']')?;
        model = Some(m);
        i = skip_tex_argument_space(s, next);
    }
    let (color, after_color) = read_delim(s, i, b'{', b'}')?;
    let (text, after_text) = read_delim(s, after_color, b'{', b'}')?;
    let content = render_latex_text_with_math(&text, labels);
    if let Some(css) = resolve_color_css(model.as_deref(), &color) {
        let _ = write!(
            out,
            r#"<span class="text-color" style="color:{css}">{content}</span>"#
        );
    } else {
        // A complete but unsupported color should not make its declaration
        // visible or discard the useful text it wraps.
        out.push_str(&content);
    }
    Some(after_text)
}

/// Parse the model/specification portion of `\color[model]{spec}`. A
/// syntactically complete but unknown color still returns its end position so
/// the declaration never leaks as visible text.
fn read_color_declaration(s: &str, from: usize) -> Option<(Option<String>, usize)> {
    let bytes = s.as_bytes();
    let mut i = skip_tex_argument_space(s, from);
    let mut model = None;
    if bytes.get(i) == Some(&b'[') {
        let (value, next) = read_delim(s, i, b'[', b']')?;
        model = Some(value);
        i = skip_tex_argument_space(s, next);
    }
    let (spec, next) = read_delim(s, i, b'{', b'}')?;
    Some((resolve_color_css(model.as_deref(), &spec), next))
}

fn render_color_box(
    out: &mut String,
    s: &str,
    from: usize,
    labels: &LabelTable,
    framed: bool,
) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = skip_tex_argument_space(s, from);
    let mut first_model = None;
    if bytes.get(i) == Some(&b'[') {
        let (value, next) = read_delim(s, i, b'[', b']')?;
        first_model = Some(value);
        i = skip_tex_argument_space(s, next);
    }

    let (first, after_first) = read_delim(s, i, b'{', b'}')?;
    let (background, background_model, after_background, frame) = if framed {
        i = skip_tex_argument_space(s, after_first);
        let mut background_model = None;
        if bytes.get(i) == Some(&b'[') {
            let (value, next) = read_delim(s, i, b'[', b']')?;
            background_model = Some(value);
            i = skip_tex_argument_space(s, next);
        }
        let (background, after_background) = read_delim(s, i, b'{', b'}')?;
        (background, background_model, after_background, Some(first))
    } else {
        (first, first_model.clone(), after_first, None)
    };
    let (text, after_text) = read_delim(s, after_background, b'{', b'}')?;

    let background = resolve_color_css(background_model.as_deref(), &background);
    let frame = frame.and_then(|value| resolve_color_css(first_model.as_deref(), &value));
    let mut styles = Vec::new();
    if let Some(background) = background {
        styles.push(format!("background-color:{background}"));
    }
    if framed {
        styles.push(format!(
            "border:1px solid {}",
            frame.unwrap_or_else(|| "currentColor".to_string())
        ));
    }
    let style = if styles.is_empty() {
        String::new()
    } else {
        format!(r#" style="{}""#, styles.join(";"))
    };
    let class = if framed {
        "text-color-box text-color-frame"
    } else {
        "text-color-box"
    };
    write!(
        out,
        r#"<span class="{class}"{style}>{}</span>"#,
        render_latex_text_with_math(&text, labels)
    )
    .unwrap();
    Some(after_text)
}

fn skip_tex_argument_space(s: &str, mut i: usize) -> usize {
    let bytes = s.as_bytes();
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'%') {
            return i;
        }
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
    }
}

/// Read a `{..}` / `[..]` group at `start` (after optional spaces). Honors
/// backslash escapes and nesting of the same delimiter. Returns the inner
/// content and the index just past the closer.
fn read_delim(s: &str, start: usize, open: u8, close: u8) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    let i = skip_tex_argument_space(s, start);
    if bytes.get(i) != Some(&open) {
        return None;
    }
    let end = crate::parser::tex_group_end(s, i, open, close)?;
    Some((s[i + 1..end - 1].to_string(), end))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::HtmlOptions;

    #[cfg(unix)]
    #[test]
    fn file_url_base_encodes_literal_filesystem_delimiters_and_percent() {
        let base = super::file_url_base_for_directory(Path::new("/tmp/notes 100%20real/#draft?/λ"))
            .unwrap();
        assert_eq!(base, "file:///tmp/notes%20100%2520real/%23draft%3F/%CE%BB/");
    }

    #[test]
    fn file_asset_destinations_preserve_escapes_but_encode_raw_path_bytes() {
        let url = super::markdown_image_url(
            "./fig%20one λ.png?download=1#preview",
            Some("file:///tmp/source%20dir/"),
        );
        assert_eq!(
            url,
            "file:///tmp/source%20dir/fig%20one%20%CE%BB.png?download=1#preview"
        );
    }

    #[test]
    fn local_image_paths_reject_encoded_traversal_and_separators() {
        for destination in [
            "%2e%2e/secret.png",
            ".%2E/secret.png",
            "%2e./secret.png",
            "safe%2f..%2fsecret.png",
            "safe%5C..%5csecret.png",
            "%00secret.png",
        ] {
            assert!(
                !super::safe_markdown_image_url(destination),
                "accepted {destination}"
            );
        }
        assert!(super::safe_markdown_image_url("fig%20two.png"));
        assert!(super::safe_markdown_image_url("./figures/a.png"));
    }

    #[cfg(windows)]
    #[test]
    fn file_url_base_handles_windows_drive_and_unc_paths() {
        assert_eq!(
            super::file_url_base_for_directory(Path::new(r"C:\Notes 100%20real\λ")),
            Some("file:///C:/Notes%20100%2520real/%CE%BB/".to_string())
        );
        assert_eq!(
            super::file_url_base_for_directory(Path::new(r"\\server\share\Notes #1")),
            Some("file://server/share/Notes%20%231/".to_string())
        );
    }

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

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mathpreview-{name}-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn input_preamble_auto_loads_mathtools_and_applies_showonlyrefs() {
        let dir = temp_dir("mathtools-input");
        let root = dir.join("main.tex");
        let preamble = dir.join("preamble.tex");
        std::fs::write(
            &root,
            "\\documentclass{article}\n\\input{preamble}\n\\begin{document}\n\\begin{equation}\na \\coloneqq b\n\\end{equation}\n\\begin{equation}\nc=d \\label{eq:used}\n\\end{equation}\nSee \\eqref{eq:used}.\n\\end{document}\n",
        )
        .unwrap();
        std::fs::write(
            &preamble,
            "\\usepackage{mathtools}\n\\mathtoolsset{showonlyrefs=true}\n",
        )
        .unwrap();

        let out = crate::render_project(&root, &HtmlOptions::default()).unwrap();
        assert!(out.preamble.packages_short.iter().any(|p| p == "mathtools"));
        assert!(out
            .preamble
            .packages_long
            .iter()
            .any(|p| p == "[tex]/mathtools"));
        assert!(out.preamble.show_only_refs);
        assert!(out.html.contains(r#"mathjaxPackages: ["[tex]/mathtools"]"#));
        assert!(out
            .html
            .contains(r#"load: ['[tex]/noerrors', "[tex]/ams", "[tex]/mathtools"]"#));
        assert!(out.body_html.contains(r#"<span class="eq-num">(1)</span>"#));
        assert!(!out.body_html.contains("(2)"), "{}", out.body_html);
        assert!(out
            .included_files
            .contains(&preamble.canonicalize().unwrap()));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_dialog_has_common_viewer_controls_above_sparse_toml_editor() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nConfig\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.html.contains(r#"id="config-mode-viewer""#));
        assert!(out.html.contains(r#"id="config-mode-mathjax""#));
        assert!(out.html.contains(r#"id="config-font-size""#));
        assert!(out
            .html
            .contains(r#"id="config-hover-preview-scale" min="100" max="300""#));
        assert!(out.html.contains("Hover preview size (%)"));
        assert!(out.html.contains("hoverPreviewScale: 100"));
        assert!(out
            .html
            .contains("values['viewer.hover-preview-scale'] = hoverScale"));
        assert!(out
            .html
            .contains("window.__mpConfig.hoverPreviewScale = cfg.hover_preview_scale"));
        assert!(out
            .html
            .contains("setProperty('--hover-preview-scale', nextHoverScale)"));
        assert!(out
            .html
            .contains("positionHoverPreview(hoverPreviewEl, hoverPreviewSource)"));
        assert!(out
            .html
            .contains("e.target.id === 'config-hover-preview-scale'"));
        assert!(out.html.contains("hoverScaleEl.dataset.dirty === 'true'"));
        assert!(out.html.contains(r#"id="config-source-jump-trigger""#));
        assert!(out.html.contains(r#"id="config-typeset-mode""#));
        assert!(out.html.contains(r#"id="config-fancy-theorems""#));
        assert!(out.html.contains("Fancy theorem boxes"));
        assert!(out.html.contains(r#"id="config-render-tikz""#));
        assert!(out
            .html
            .contains("Render TikZ diagrams (trusted projects only)"));
        assert!(out.html.contains("renderTikz: false"));
        assert!(out.html.contains("fancyTheorems: true"));
        assert!(out.html.contains("fancyTheorems.dataset.dirty === 'true'"));
        assert!(out
            .html
            .contains("values['viewer.fancy-theorems'] = fancyTheorems.checked"));
        assert!(out
            .html
            .contains("window.__mpConfig.fancyTheorems = cfg.fancy_theorems"));
        assert!(out
            .html
            .contains("window.__mpConfig.theoremNumbering = cfg.theorem_numbering"));
        assert!(out.html.contains("renderTikz.dataset.dirty === 'true'"));
        assert!(out
            .html
            .contains("values['viewer.render-tikz'] = renderTikz.checked"));
        assert!(!out.html.contains(r#"id="config-wrap-equations""#));
        assert!(!out.html.contains("Wrap long equations"));
        assert!(!out.html.contains("wrap-equations ="));
        assert!(out.html.contains(r#"id="config-viewer-toml""#));
        assert!(out.html.contains(r#"id="config-keybindings-reference""#));
        assert!(out
            .html
            .contains("Omitted settings and keybindings remain inherited."));
        assert!(!out.html.contains("function withDefaultKeybindings"));
        assert!(out.html.contains("built-in keybindings remain inherited"));
        assert!(out.html.contains("editor.dataset.loadedScope = ''"));
        assert!(out.html.contains("save.disabled = !!loading"));
        assert!(out
            .html
            .contains("Wait for the selected config file to finish loading."));
        assert!(out.html.contains("withViewerKeybindingReference"));
        assert!(out.html.contains("editor._loadedContent"));
        assert!(out.html.contains("editor._displayContent"));
        assert!(out.html.contains("expected_content: editor._loadedContent"));
        assert!(out.html.contains("Macros and environments"));
        assert!(out.html.contains(r"\newenvironment"));
        assert!(out
            .html
            .contains("Unknown environment\n            bodies already render normally"));
        assert!(!out.html.contains(r"\renewenvironment{letter}"));
        assert!(out
            .html
            .contains("Enter a \\\\newcommand or \\\\newenvironment definition first."));
        assert!(out.html.contains("markMacroEditorDirty(e.target)"));
        assert!(out.html.contains("seq !== macroLoadSeq"));
        assert!(out
            .html
            .contains("(input.dataset.editRevision || '0') !== editRevision"));
        assert!(out
            .html
            .contains("expected_content: input.dataset.loadedScope === scopeKey"));
        assert!(out.html.contains("input.dataset.saving === 'true'"));
        assert!(out.html.contains("input.dataset.dirty = 'false'"));
        let controls = out.html.find(r#"id="config-font-size""#).unwrap();
        let editor = out.html.find(r#"id="config-viewer-toml""#).unwrap();
        assert!(
            controls < editor,
            "common controls must appear above the editor"
        );
        let editor_end = editor + out.html[editor..].find("</textarea>").unwrap();
        let editor_markup = &out.html[editor..editor_end];
        assert!(editor_markup.contains("# [keybindings]"));
        assert!(editor_markup.contains("# [keybindings.aliases]"));
        assert!(editor_markup.contains("# J = &quot;5j&quot;"));
        assert!(editor_markup.contains("# &quot;Shift+Space&quot; = &quot;b&quot;"));
        assert!(editor_markup
            .contains("# zoom-in = [&quot;+&quot;, &quot;Mod+=&quot;, &quot;Mod++&quot;]"));
        assert!(!editor_markup.contains("\n[keybindings]"));
        assert!(out.html.contains("Project (local)"));
        assert!(out
            .html
            .contains("Global <code>~/.config/mathpreview/config.toml</code>"));
    }

    #[test]
    fn hover_preview_scale_is_seeded_into_css_and_client_config() {
        let mut opts = HtmlOptions::default();
        opts.viewer_config.hover_preview_scale = 175;
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nConfig\n\\end{document}\n".to_string(),
            &opts,
        )
        .unwrap();

        assert!(out.html.contains("--hover-preview-scale: 1.75;"));
        assert!(out.html.contains("hoverPreviewScale: 175"));
    }

    #[test]
    fn config_dialog_keybinding_reference_tracks_every_documented_default() {
        let reference = super::shell::viewer_keybinding_reference();
        let start = crate::config::DEFAULT_CONFIG_TEMPLATE
            .find("# One shortcut string or an array is accepted.")
            .unwrap();

        for line in crate::config::DEFAULT_CONFIG_TEMPLATE[start..].lines() {
            let commented = if line.is_empty() {
                "#".to_string()
            } else if line.trim_start().starts_with('#') {
                line.to_string()
            } else {
                format!("# {line}")
            };
            assert!(
                reference.lines().any(|candidate| candidate == commented),
                "missing documented keybinding line: {line}"
            );
        }
        assert!(reference.contains(&format!(
            "# --- Complete Neovim-style keybinding reference for v{} (commented) ---",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(reference.contains("# [keybindings.aliases]"));
        assert!(reference.contains("# One shortcut string or an array is accepted."));
        assert!(reference.contains("# extension. For example, omit `j`/`k`"));
        assert!(reference.contains("# J = \"5j\""));
        assert!(reference.contains("# K = \"5k\""));
        assert!(reference.contains("# \"Shift+Space\" = \"b\""));
        assert!(!reference.lines().any(|line| line == "[keybindings]"));
        assert!(!reference
            .lines()
            .any(|line| line == "[keybindings.aliases]"));
    }

    #[test]
    fn config_shell_seeds_disabled_fancy_theorems() {
        let mut opts = HtmlOptions::default();
        opts.viewer_config.fancy_theorems = false;
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nConfig\n\\end{document}\n".to_string(),
            &opts,
        )
        .unwrap();

        assert!(out.html.contains("fancyTheorems: false"));
    }

    #[test]
    fn default_css_allows_mathjax_inline_breaks() {
        assert!(super::shell::DEFAULT_CSS.contains(".math.inline { white-space: normal; }"));
        assert!(!super::shell::DEFAULT_CSS.contains(".math.inline { white-space: nowrap; }"));
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

    fn render_body(source: &str) -> String {
        crate::render_project_from_source(Path::new("t.tex"), source.to_string(), &HtmlOptions::default())
            .unwrap()
            .body_html
    }

    #[test]
    fn callout_env_renders_box_with_typeset_math() {
        let body = render_body(
            "\\begin{document}\n\\begin{todo}[Fix this]\ntext $E=mc^2$ text\n\\end{todo}\n\\end{document}\n",
        );
        assert!(body.contains(r#"class="callout callout-todo"#), "{body}");
        assert!(body.contains("callout-head"), "{body}");
        assert!(text_content(&body).contains("Fix this"), "{body}");
        // The math inside renders as a typeset node, not raw `$...$`.
        assert!(
            body.contains(r#"class="math inline"#),
            "math not typeset: {body}"
        );
        assert!(
            !text_content(&body).contains("$E=mc^2$"),
            "raw math leaked: {body}"
        );
    }

    #[test]
    fn generated_ids_cannot_collide_with_label_ids() {
        // sanitize_id("thm:2.1") -> "thm-2-1"; generated theorem ids must be
        // structurally different (the `g` marker) or getElementById targets
        // the wrong element for refs/jumps/highlights.
        let body = render_body(concat!(
            "\\newtheorem{theorem}{Theorem}\n\\begin{document}\n",
            "\\begin{theorem} unlabeled \\end{theorem}\n",
            "\\begin{theorem}\\label{thm:0.2} labeled \\end{theorem}\n",
            "\\end{document}\n",
        ));
        // The labeled theorem's id is label-derived...
        assert!(body.contains(r#"id="thm-0-2""#), "label id missing: {body}");
        // ...and every GENERATED id carries the g marker, so no collision.
        let mut ids: Vec<&str> = Vec::new();
        for part in body.split(r#" id=""#).skip(1) {
            if let Some(end) = part.find('"') {
                ids.push(&part[..end]);
            }
        }
        let dup = ids.iter().find(|id| ids.iter().filter(|o| o == id).count() > 1);
        assert!(dup.is_none(), "duplicate DOM id {dup:?}: {body}");
        assert!(
            ids.iter().any(|id| id.starts_with("thm-g")),
            "generated theorem id should be g-marked: {ids:?}"
        );
    }

    #[test]
    fn theorem_card_class_tracks_fancy_theorems_option() {
        let source = concat!(
            "\\newtheorem{theorem}{Theorem}\n",
            "\\begin{document}\n",
            "\\begin{theorem}\\label{thm:one}Statement.\\end{theorem}\n",
            "\\end{document}\n",
        );
        let mut fancy = HtmlOptions::default();
        fancy.viewer_config.fancy_theorems = true;
        let fancy_body =
            crate::render_project_from_source(Path::new("t.tex"), source.to_string(), &fancy)
                .unwrap()
                .body_html;
        assert!(
            fancy_body.contains(r#"class="thm thm-fancy "#),
            "fancy theorem class missing: {fancy_body}"
        );

        let mut plain = HtmlOptions::default();
        plain.viewer_config.fancy_theorems = false;
        let plain_body =
            crate::render_project_from_source(Path::new("t.tex"), source.to_string(), &plain)
                .unwrap()
                .body_html;
        assert!(
            plain_body.contains(r#"class="thm env-theorem "#),
            "plain theorem should retain semantic markup: {plain_body}"
        );
        assert!(
            !plain_body.contains("thm-fancy"),
            "plain theorem retained card class: {plain_body}"
        );
        assert!(plain_body.contains(r#"class="thm-head"#));
        assert!(plain_body.contains(r#"class="thm-body"#));
        assert!(plain_body.contains(r#"data-refkey="thm:one""#));
        assert!(plain_body.contains(r#"class="thm-num">1</span>"#));
        assert!(
            plain_body.contains(r#"class="thm-num">1</span>.</div>"#),
            "plain theorem heading should end in a literal period: {plain_body}"
        );
        assert!(
            !plain_body.contains("role-pill"),
            "plain theorem should not render a role pill: {plain_body}"
        );
        assert!(
            fancy_body.contains(r#"class="role-pill role-standard">standard</span></div>"#),
            "fancy theorem should retain its role pill and no terminal period: {fancy_body}"
        );
    }

    #[test]
    fn undeclared_theorem_renders_as_transparent_unsupported_environment() {
        let body = render_body(
            "\\begin{document}\n\\begin{theorem}Text $x$.\\end{theorem}\n\\end{document}\n",
        );
        assert!(
            body.contains(r#"class="unsupported-env-boundary unsupported-env-begin"#),
            "missing begin diagnostic: {body}"
        );
        assert!(
            body.contains(r#"class="unsupported-env-boundary unsupported-env-end"#),
            "missing end diagnostic: {body}"
        );
        assert!(
            body.contains(r#"class="math inline"#),
            "body was not parsed: {body}"
        );
        assert!(
            !body.contains(r#"class="thm"#),
            "undeclared theorem was enhanced: {body}"
        );
    }

    #[test]
    fn plain_theorem_css_keeps_boxes_opt_in() {
        let css = super::shell::DEFAULT_CSS;
        assert!(css.contains(".thm-fancy {\n  padding:"));
        assert!(css.contains(".thm-head { display: inline;"));
        assert!(css.contains(".thm-fancy .thm-head { display: block;"));
        assert!(!css.contains(".thm-head::after"));
        assert!(css.contains(".thm-body { display: contents;"));
        assert!(css.contains("body.theme-dark .thm-fancy { background:"));
        assert!(!css.contains("body.theme-dark .thm { background:"));
    }

    #[test]
    fn quote_env_renders_blockquote_with_typeset_math() {
        let body = render_body(
            "\\begin{document}\n\\begin{quote}\nsay $E=mc^2$ now\n\\end{quote}\n\\end{document}\n",
        );
        assert!(
            body.contains(r#"<blockquote class="quote env-quote"#),
            "no blockquote wrapper: {body}"
        );
        // Math inside the quote is typeset, not dumped as raw text or an opaque env.
        assert!(
            body.contains(r#"class="math inline"#),
            "math not typeset in quote: {body}"
        );
        assert!(!body.contains("opaque-env\" data-env=\"quote"), "quote went opaque: {body}");
        assert!(!text_content(&body).contains("$E=mc^2$"), "raw math leaked: {body}");
    }

    #[test]
    fn unsupported_environment_marks_boundaries_and_renders_body_normally() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{mystery}\n",
            "Readable $E=mc^2$ with \\ref{eq:x}.\n",
            "\\end{mystery}\n",
            "\\end{document}\n",
        ));
        assert!(
            body.contains(r#"class="unsupported-env-boundary unsupported-env-begin""#),
            "begin diagnostic missing: {body}"
        );
        assert!(
            body.contains(r#"class="unsupported-env-boundary unsupported-env-end""#),
            "end diagnostic missing: {body}"
        );
        assert!(body.contains(r#"<code aria-hidden="true">\begin{mystery}</code>"#));
        assert!(body.contains(r#"<code aria-hidden="true">\end{mystery}</code>"#));
        assert!(body.contains("contents are shown without environment formatting"));
        assert!(
            body.contains(r#"class="math inline"#),
            "body math stayed raw: {body}"
        );
        assert!(
            body.contains(r#"class="ref""#),
            "body reference did not render: {body}"
        );
        assert!(
            !body.contains(r#"opaque-env" data-env="mystery"#),
            "supported fallback stayed opaque: {body}"
        );
    }

    #[test]
    fn unsupported_environment_marker_is_a_block_chunk_inside_proof() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{proof}\n",
            "Before.\n",
            "\\begin{mystery}Inside $x$.\\end{mystery}\n",
            "After.\n",
            "\\end{proof}\n",
            "\\end{document}\n",
        ));
        let begin = body
            .find(r#"class="unsupported-env-boundary unsupported-env-begin""#)
            .expect("begin diagnostic");
        let before = &body[..begin];
        let last_para_open = before.rfind(r#"<span class="proof-para""#);
        let last_para_close = before.rfind("</span>");
        assert!(
            last_para_open.is_none() || last_para_close > last_para_open,
            "diagnostic was nested inside proof-para: {body}"
        );
        assert!(body.contains(r#"class="math inline"#));
    }

    #[test]
    fn unclosed_unsupported_environment_keeps_opaque_remainder_outside_proof_paragraph() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{proof}\n",
            "Before.\n",
            "\\begin{mystery}Raw $x$ remains inert.\n",
            "\\end{proof}\n",
            "\\end{document}\n",
        ));
        let opaque = body
            .find(r#"class="opaque-env" data-env="mystery""#)
            .expect("opaque malformed remainder");
        let before = &body[..opaque];
        let last_para_open = before.rfind(r#"<span class="proof-para""#);
        let last_para_close = before.rfind("</span>");
        assert!(
            last_para_open.is_none() || last_para_close > last_para_open,
            "opaque block was nested inside proof-para: {body}"
        );
        assert!(body.contains("unsupported-env-missing-end"));
        assert!(!body.contains(r#"class="math inline"#));
    }

    #[test]
    fn unsupported_boundaries_keep_nested_abstract_in_source_order() {
        let body = render_body(concat!(
            "\\title{Title}\n",
            "\\begin{document}\n",
            "\\begin{mystery}\n",
            "\\begin{abstract}Nested abstract.\\end{abstract}\n",
            "\\end{mystery}\n",
            "\\maketitle\n",
            "\\end{document}\n",
        ));
        let begin = body.find("unsupported-env-begin").expect("begin marker");
        let abstract_pos = body.find("paper-abstract").expect("abstract");
        let end = body.find("unsupported-env-end").expect("end marker");
        assert!(
            begin < abstract_pos && abstract_pos < end,
            "front-matter reordering moved abstract outside diagnostics: {body}"
        );
    }

    #[test]
    fn unclosed_unsupported_environment_renders_missing_end_without_parsing_body() {
        let body = render_body(
            "\\begin{document}\n\\begin{mystery}Raw $x$ remains inert.\n\\end{document}\n",
        );
        assert!(
            body.contains(r#"unsupported-env-missing-end"#),
            "missing-end diagnostic absent: {body}"
        );
        assert!(body.contains(r#"<span class="unsupported-env-missing""#));
        assert!(
            !body.contains(r#"class="math inline"#),
            "malformed body was parsed: {body}"
        );
        assert!(body.contains("$x$"), "inert remainder disappeared: {body}");
    }

    #[test]
    fn unsupported_environment_name_is_escaped_in_text_and_attributes() {
        let env = r#"odd"><img src=x onerror=alert(1)>"#;
        let body = render_body(&format!("\\begin{{{env}}}Safe.\\end{{{env}}}"));
        assert!(
            !body.contains("<img"),
            "environment name injected HTML: {body}"
        );
        assert!(body.contains(r#"data-env="odd&quot;&gt;&lt;img"#));
        assert!(body.contains(r#"\begin{odd"#));
        assert!(body.contains("&lt;img"));
    }

    #[test]
    fn unsupported_environment_diagnostics_are_styled_and_skipped_by_line_numbers() {
        let css = super::shell::DEFAULT_CSS;
        assert!(css.contains(".unsupported-env-boundary {"));
        assert!(css.contains(".blk:has(> .unsupported-env-boundary)"));
        assert!(css.contains("border-left-width: 3px;"));
        assert!(!css.contains(".unsupported-env-label {\n  font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;\n  font-size: 0.88em;\n  font-weight: 600;\n  opacity:"));
        assert!(css.contains(".unsupported-env-boundary.source-range"));
        assert!(css.contains("--diagnostic-error: #b42318;"));
        assert!(css.contains("--diagnostic-error: #ff8a80;"));
        assert!(css.contains(
            "@media print {\n  /* Browsers print the paper on white even when the viewer is dark."
        ));
        assert!(super::shell::CLIENT_JS.contains(".unsupported-env-boundary';"));
    }

    #[test]
    fn unsupported_environment_marker_diff_ignores_shifted_generated_id() {
        let old = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{mystery}Body.\\end{mystery}\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();
        let new = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nInserted paragraph.\n\n\\begin{mystery}Body.\\end{mystery}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();
        let marker = |output: &crate::RenderOutput| {
            let block = output
                .blocks
                .iter()
                .find(|block| block.html.contains("unsupported-env-begin"))
                .expect("unsupported begin marker block");
            (block.html.clone(), block.diff_hash.clone())
        };
        let (old_html, old_diff_hash) = marker(&old);
        let (new_html, new_diff_hash) = marker(&new);
        assert_ne!(old_html, new_html);
        assert_eq!(
            old_diff_hash, new_diff_hash,
            "generated marker id or source metadata changed the semantic diff hash"
        );
    }

    #[test]
    fn tex_control_space_renders_as_html_interword_space() {
        let body = render_body("\\begin{document}\nLeft\\ right.\n\\end{document}\n");
        let text = text_content(&body);
        assert!(
            text.contains("Left right."),
            "control space vanished: {body}"
        );
        assert!(!text.contains("Leftright."), "words were joined: {body}");
    }

    #[test]
    fn text_mode_tex_special_characters_render_literally() {
        let labels = crate::numbering::LabelTable::default();
        let rendered = super::render_inline_latex(
            r"\# \$ \% \& \_ \{ \} \textbackslash{} \textasciitilde{} \textasciicircum{}",
            &labels,
        );
        assert_eq!(rendered, r"# $ % &amp; _ { } \ ~ ^");
        assert_eq!(
            super::render_inline_latex(r"\textasciitilde word", &labels),
            "~word",
        );
        assert_eq!(
            super::render_inline_latex(r"\textasciitilde{} word", &labels),
            "~ word",
        );
        assert_eq!(
            super::render_inline_latex("\\textasciitilde% separator\n word", &labels),
            "~word",
        );

        let body = render_body(concat!(
            "\\begin{document}\n",
            r"Escaped: \# \$ \% \& \_ \{ \} \textbackslash{};\textasciitilde{};\textasciicircum{}; tail.",
            "\n\\end{document}\n",
        ));
        assert!(
            text_content(&body).contains(r"Escaped: # $ % &amp; _ { } \;~;^; tail."),
            "special characters did not survive the document renderer: {body}",
        );
    }

    #[test]
    fn literal_control_symbols_do_not_absorb_the_following_source_character() {
        for source in [
            r"\#R", r"\$R", r"\%R", r"\&R", r"\_R", r"\{R", r"\}R", r"\%λ",
        ] {
            assert_eq!(
                super::latex_source_token_end(source, 0),
                Some(2),
                "literal control symbol swallowed its suffix in {source:?}",
            );
        }
        for source in [r"\,x", r"\!x", r"\\x", r"\ x"] {
            assert_eq!(
                super::latex_source_token_end(source, 0),
                Some(2),
                "non-accent control symbol swallowed its suffix in {source:?}",
            );
        }
        for source in [r"\'e", r"\`a", r#"\"o"#, r"\^u", r"\~n", r"\.z", r"\=a"] {
            assert_eq!(
                super::latex_source_token_end(source, 0),
                Some(source.len()),
                "accent command lost its argument in {source:?}",
            );
        }

        let body = render_body(
            "\\begin{document}\nA\\%B % real comment\nTail survives.\n\\end{document}\n",
        );
        let visible = text_content(&body);
        assert!(visible.contains("A%B Tail survives."), "{body}");
        assert!(!visible.contains("real comment"), "{body}");
        assert!(body.contains(r#"data-src="t.tex:2:2">%</span>"#), "{body}");
        assert!(body.contains(r#"data-src="t.tex:2:4">B</span>"#), "{body}");
    }

    #[test]
    fn escaped_percent_survives_all_shared_inline_rendering_contexts() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "Prose 100\\% ready.\\par\n",
            "\\textbf{Bold 100\\% ready.}\n",
            "Foot\\footnote{Note 100\\% ready.}.\n",
            "\\begin{figure}\\caption{Caption 100\\% ready.}\\end{figure}\n",
            "\\mystery{Fallback 100\\% ready.}\n",
            "\\begin{tabular}{l}Table 100\\% ready.\\\\\\end{tabular}\n",
            "Math $\\%$ stays on the MathJax path.\n",
            "\\end{document}\n",
        ));
        for expected in [
            "Prose 100% ready.",
            "Bold 100% ready.",
            "Note 100% ready.",
            "Caption 100% ready.",
            "Fallback 100% ready.",
            "Table 100% ready.",
        ] {
            assert!(
                text_content(&body).contains(expected),
                "missing {expected:?}: {body}",
            );
        }
        assert!(
            body.contains(r#"data-tex="\(\%\)""#),
            "math-mode percent should remain TeX for MathJax: {body}",
        );
    }

    #[test]
    fn alignment_envs_render_semantic_wrappers_with_typeset_math() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{center}Centered $c$.\\end{center}\n",
            "\\begin{flushleft}Left $l$.\\end{flushleft}\n",
            "\\begin{flushright}Right $r$.\\end{flushright}\n",
            "\\end{document}\n",
        ));
        for (class, env) in [
            ("align-center", "center"),
            ("align-flush-left", "flushleft"),
            ("align-flush-right", "flushright"),
        ] {
            assert!(
                body.contains(&format!(r#"class="text-alignment {class}""#)),
                "missing {env} wrapper: {body}"
            );
            assert!(body.contains(&format!(r#"data-env="{env}""#)));
            assert!(!body.contains(&format!(r#"opaque-env" data-env="{env}""#)));
        }
        assert_eq!(body.matches(r#"class="math inline"#).count(), 3);
    }

    #[test]
    fn user_newenvironment_expands_so_body_math_renders() {
        // A `\newenvironment` wrapper (referee = quote + italic) is expanded to
        // its begin/end code around the body and parsed — so the body's math
        // renders, instead of the whole env being dumped as an opaque block.
        let body = render_body(concat!(
            "\\newenvironment{referee}{\n\\begin{quote}\\itshape}{\n\\end{quote}}\n",
            "\\begin{document}\n\\begin{referee}\nClaim $x^2 = y$ holds.\n\\end{referee}\n\\end{document}\n",
        ));
        assert!(
            !body.contains(r#"opaque-env" data-env="referee"#),
            "referee went opaque: {body}"
        );
        assert!(
            body.contains(r#"<blockquote class="quote"#),
            "no blockquote wrapper from the expansion: {body}"
        );
        assert!(
            body.contains(r#"class="math inline"#),
            "body math not typeset: {body}"
        );
        assert!(
            !text_content(&body).contains("$x^2 = y$"),
            "raw math leaked: {body}"
        );
    }

    #[test]
    fn native_letter_renders_standard_address_and_closing_geometry() {
        let dir = temp_dir("native-letter");
        let root = dir.join("letter.tex");
        std::fs::write(
            &root,
            concat!(
                "\\documentclass{letter}\n",
                "\\newcommand{\\sendername}{Ada Lovelace}\n",
                "\\newcommand{\\senderaddress}{Ada Lovelace\\\\12 St James's Square\\\\London}\n",
                "\\newcommand{\\letterdate}{July 30, 2026}\n",
                "\\address{\\senderaddress}\n",
                "\\signature{\\sendername}\n",
                "\\date{Wrong date}\n",
                "\\date{\\letterdate}\n",
                "\\begin{document}\n",
                "\\begin{letter}{Charles Babbage\\\\London}\n",
                "\\opening{Dear Charles,}\n\n",
                "The engine satisfies $e^{i\\pi}+1=0$.\n\n",
                "\\closing{Yours sincerely,}\n",
                "\\end{letter}\n",
                "\\end{document}\n",
            ),
        )
        .unwrap();

        let out = crate::render_project(&root, &HtmlOptions::default()).unwrap();
        let body = out.body_html;
        assert!(
            body.contains(r#"class="letter letter-has-address""#),
            "{body}"
        );
        for class in [
            "letter-address",
            "letter-date",
            "letter-recipient",
            "letter-opening",
            "letter-body",
            "letter-closing",
            "letter-signature",
        ] {
            assert!(
                body.contains(&format!(r#"class="{class}""#)),
                "missing {class}: {body}"
            );
        }
        let order: Vec<usize> = [
            "letter-address",
            "letter-date",
            "letter-recipient",
            "letter-opening",
            r#"class="math inline""#,
            "letter-closing",
            "letter-signature",
        ]
        .iter()
        .map(|needle| {
            body.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}: {body}"))
        })
        .collect();
        assert!(
            order.windows(2).all(|pair| pair[0] < pair[1]),
            "letter parts out of order: {body}"
        );
        assert!(body.matches("<br>").count() >= 3, "{body}");
        assert_eq!(body.matches(r#"class="math inline"#).count(), 1);
        assert!(!body.contains("Wrong date"), "{body}");
        assert!(!body.contains(r"\senderaddress"), "{body}");
        assert!(!body.contains(r"\sendername"), "{body}");
        assert!(!body.contains("unsupported-env-boundary"), "{body}");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn native_letter_name_falls_back_for_signature_without_address() {
        let body = render_body(concat!(
            "\\name{Fallback Name}\n",
            "\\date{}\n",
            "\\begin{document}\n",
            "\\begin{letter}{Recipient}\n",
            "\\opening{Hello,}\n",
            "Body.\n",
            "\\closing{Regards,}\n",
            "\\end{letter}\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"class="letter""#), "{body}");
        assert!(!body.contains("letter-has-address"), "{body}");
        assert!(!body.contains("letter-address"), "{body}");
        assert!(!body.contains("letter-date"), "{body}");
        assert!(
            body.contains(r#"<div class="letter-signature">Fallback Name</div>"#),
            "{body}"
        );
    }

    #[test]
    fn native_letter_does_not_use_article_author_as_signature() {
        let body = render_body(concat!(
            "\\author{Paper Author}\n",
            "\\begin{document}\n",
            "\\begin{letter}{Recipient}\n",
            "\\opening{Hello,}\n",
            "Body.\n",
            "\\closing{Regards,}\n",
            "\\end{letter}\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"class="letter-closing""#), "{body}");
        assert!(!body.contains(r#"class="letter-signature""#), "{body}");
        assert!(!body.contains("Paper Author"), "{body}");
    }

    #[test]
    fn native_letter_css_and_live_patch_target_match_the_renderer() {
        let css = super::shell::DEFAULT_CSS;
        assert!(css.contains(".letter-from {"));
        assert!(css.contains("width: max-content;"));
        assert!(css.contains(".letter-body > .source-space {"));
        assert!(css.contains(".letter-closing {\n  width: 50%;"));
        assert!(css.contains(".letter-has-address .letter-closing"));
        assert!(css.contains(".letter-body .para-indent-marker { display: none; }"));
        assert!(super::shell::CLIENT_JS.contains("blockquote.quote, .letter-body"));
    }

    #[test]
    fn preview_macro_override_can_replace_letter_environment() {
        let dir = temp_dir("letter-environment-override");
        let root = dir.join("letter.tex");
        let overrides = dir.join(".mathpreview-macros.tex");
        std::fs::write(
            &root,
            concat!(
                "\\documentclass{letter}\n",
                "\\begin{document}\n",
                "\\begin{letter}{Charles Babbage\\\\London}\n",
                "\\opening{Dear Charles,}\n\n",
                "The engine satisfies $e^{i\\pi}+1=0$.\n\n",
                "\\closing{Yours sincerely,}\n",
                "\\end{letter}\n",
                "\\end{document}\n",
            ),
        )
        .unwrap();
        std::fs::write(
            &overrides,
            concat!(
                "\\renewenvironment{letter}[1]",
                "{\\begin{quote}\\textbf{To: #1}\\\\}",
                "{\\end{quote}}\n",
            ),
        )
        .unwrap();

        let opts = HtmlOptions {
            macro_overrides: vec![overrides],
            ..HtmlOptions::default()
        };
        let out = crate::render_project(&root, &opts).unwrap();
        let body = out.body_html;
        let text = text_content(&body);

        assert!(
            !body.contains(r#"opaque-env" data-env="letter"#),
            "letter stayed opaque: {body}"
        );
        assert!(
            body.contains(r#"<blockquote class="quote"#),
            "replacement wrapper missing: {body}"
        );
        assert!(
            !body.contains(r#"<section class="letter"#),
            "explicit preview override lost precedence: {body}"
        );
        for expected in [
            "To: Charles Babbage",
            "Dear Charles,",
            "The engine satisfies",
            "Yours sincerely,",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {body}");
        }
        assert!(
            body.contains(r#"class="math inline"#),
            "letter math did not render: {body}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn footnote_renders_inline_marker_with_hover_popover_and_math() {
        let body = render_body(
            "\\begin{document}\nFirst\\footnote{Note with $x^2$.} and second\\footnote{Two.} done.\n\\end{document}\n",
        );
        // Inline numbered superscript markers, keyboard-focusable and ARIA-linked
        // to their popover — NOT the note text dumped into the sentence (the bug).
        assert!(
            body.contains(r#"<sup class="footnote-ref" id="fn-1""#),
            "no marker 1: {body}"
        );
        assert!(
            body.contains(r#"aria-describedby="fnpop-1""#),
            "marker 1 aria: {body}"
        );
        assert!(body.contains(r#"id="fn-2""#), "no marker 2: {body}");
        // The note lives in a hover/focus popover (not an end-of-document
        // section), with its math typeset.
        assert!(
            body.contains(r#"<span class="footnote-pop" id="fnpop-1""#),
            "no popover: {body}"
        );
        assert!(
            body.contains(r#"class="math inline"#),
            "math not typeset in footnote: {body}"
        );
        // No bottom-of-document footnotes section.
        assert!(
            !body.contains(r#"<section class="footnotes">"#),
            "unexpected bottom section: {body}"
        );
        // The note content is wrapped in its popover, right after the marker.
        let marker = body.find(r#"id="fn-1""#).expect("marker 1");
        let note = body.find("Note with").expect("note text present");
        assert!(note > marker, "note not placed after its marker: {body}");
    }

    #[test]
    fn footnote_nested_in_command_arg_still_renders_marker_in_order() {
        // A footnote inside a section title reaches the inline renderer (which
        // has no RenderCtx); it must still produce a numbered marker + popover —
        // not dump its text — and share the document-order counter with prose
        // footnotes (heading note = 1, prose note = 2).
        let body = render_body(
            "\\begin{document}\n\\section{Title\\footnote{In a heading.}}\nBody\\footnote{In prose.} text.\n\\end{document}\n",
        );
        assert!(body.contains(r#"id="fn-1""#), "no fn-1 marker: {body}");
        assert!(body.contains(r#"id="fn-2""#), "no fn-2 marker: {body}");
        assert_eq!(
            body.matches(r#"class="footnote-pop"#).count(),
            2,
            "expected two popovers: {body}"
        );
        assert!(
            !body.contains("TitleIn a heading."),
            "footnote text leaked into the heading: {body}"
        );
    }

    #[test]
    fn open_footnote_popover_drops_block_paint_containment_only_on_screen() {
        let css = super::shell::DEFAULT_CSS;
        assert!(css.contains("@media screen {\n  main#page .blk:has(.footnote:hover),"));
        assert!(css.contains("main#page .blk:has(.footnote:focus-within) {"));
        assert!(css.contains("content-visibility: visible;\n    contain: layout style;"));
    }

    #[test]
    fn review_commands_render_decorations_with_math() {
        let body = render_body(
            "\\begin{document}\n\\add[red]{$x$} \\remove{old} \\replace{a}{b} \\highlight{h}\n\\end{document}\n",
        );
        assert!(body.contains("review-add"), "add missing: {body}");
        assert!(body.contains("review-remove"), "remove missing: {body}");
        assert!(
            body.contains("review-highlight"),
            "highlight missing: {body}"
        );
        // \add[red]{$x$} carries the color and typesets the math.
        assert!(
            body.contains("text-decoration-color:#FF0000"),
            "color missing: {body}"
        );
        assert!(
            body.contains(r#"class="math inline"#),
            "math in \\add not typeset: {body}"
        );
    }

    #[test]
    fn theorem_env_name_is_sanitized_not_injected() {
        // Regression: an attacker-controlled `\newtheorem` name was written
        // raw into the class attribute and the heading-word fallback, allowing
        // HTML/script injection into the served preview.
        let payload = r#"x"><img src=q onerror=alert(1)>"#;
        let src = format!(
            "\\newtheorem{{{payload}}}{{Lemma}}\n\\begin{{document}}\n\
             \\begin{{{payload}}}\nbody\n\\end{{{payload}}}\n\\end{{document}}\n"
        );
        let body = render_body(&src);
        assert!(
            !body.contains("<img"),
            "env name broke out of markup: {body}"
        );
        assert!(!body.contains("onerror="), "{body}");
        assert!(
            body.contains(r#"class="thm "#),
            "theorem still rendered: {body}"
        );
    }

    #[test]
    fn theorem_empty_title_env_name_is_escaped_in_heading() {
        // Regression: with an empty `\newtheorem` title the heading word fell
        // back to the raw env name (unescaped).
        let payload = r#"<b>boom</b>"#;
        let src = format!(
            "\\newtheorem{{{payload}}}{{}}\n\\begin{{document}}\n\
             \\begin{{{payload}}}\nbody\n\\end{{{payload}}}\n\\end{{document}}\n"
        );
        let body = render_body(&src);
        assert!(!body.contains("<b>boom</b>"), "raw env name leaked: {body}");
    }

    #[test]
    fn align_row_with_backslash_multibyte_does_not_panic() {
        // Regression: `\` + multibyte char in a multi-row math env panicked in
        // split_math_rows (both the numbering and renderer copies).
        let body = render_body(
            "\\begin{document}\n\\begin{align}\nx &= \\λ \\\\ y &= 2\n\\end{align}\n\\end{document}\n",
        );
        assert!(body.contains("math display"), "{body}");
    }

    #[test]
    fn user_newcommand_expands_in_text() {
        let body =
            render_body("\\newcommand{\\hello}{world}\n\\begin{document}\nsay \\hello now\n\\end{document}\n");
        assert!(text_content(&body).contains("say world now"), "{body}");
    }

    #[test]
    fn user_newcommand_with_arg_expands_in_text() {
        let body = render_body(
            "\\newcommand{\\GI}[1]{\\textbf{#1}}\n\\begin{document}\na \\GI{note} b\n\\end{document}\n",
        );
        assert!(body.contains("<strong>note</strong>"), "{body}");
    }

    #[test]
    fn textcolor_renders_colored_span() {
        let body = render_body("\\begin{document}\n\\textcolor{red}{warn} ok\n\\end{document}\n");
        assert!(
            body.contains(r#"<span class="text-color" style="color:#FF0000">warn</span>"#),
            "{body}"
        );
    }

    #[test]
    fn textcolor_html_model_is_hex() {
        let body =
            render_body("\\begin{document}\n\\textcolor[HTML]{FF8800}{x}\n\\end{document}\n");
        assert!(
            body.contains(r##"<span class="text-color" style="color:#FF8800">x</span>"##),
            "{body}"
        );
    }

    #[test]
    fn textcolor_argument_matching_ignores_comments_and_inline_literals() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\textcolor{red}{before % } ignored\n",
            "after} outside\n",
            "\\textcolor{blue}{left \\verb|}| right} done\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"style="color:#FF0000">before after</span>"#), "{body}");
        assert!(body.contains(r#"style="color:#0000FF">left "#), "{body}");
        assert!(body.contains(r#"<code class="inline-literal">}</code> right</span>"#), "{body}");
        assert!(!body.contains("ignored"), "comment contents leaked: {body}");
        assert!(
            body.contains(">outside</span>") && body.contains(">done</span>"),
            "{body}"
        );
    }

    #[test]
    fn stringified_and_detokenized_citations_stay_literal_inside_color() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\textcolor{red}{\\string\\cite{fake} ",
            "\\detokenize{\\cite{also-fake}} live}\n",
            "\\printbibliography\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"style="color:#FF0000""#), "{body}");
        assert!(!body.contains(r#"class="cite""#), "{body}");
        assert!(!body.contains(r#"data-key="fake""#), "{body}");
        assert!(!body.contains(r#"data-key="also-fake""#), "{body}");
    }

    #[test]
    fn deeply_nested_inline_groups_stop_without_overflowing_the_stack() {
        let nested = format!("{}deep{}", "{".repeat(2_000), "}".repeat(2_000));
        let body = render_body(&format!(
            "\\begin{{document}}\n{nested}\n\\end{{document}}\n"
        ));
        assert!(body.contains("deep"), "{body}");
    }

    #[test]
    fn scoped_color_keeps_math_and_stops_at_the_group_boundary() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "before {\\color{ForestGreen} green $x+1$} after\n",
            "\\end{document}\n",
        ));
        let color_start = body.find(r#"style="color:#009B55""#).expect("green span");
        let math = body.find(r#"class="math inline""#).expect("inline math");
        let after = body.find(">after</span>").expect("following text");
        let color_end = body[..after].rfind("</span>").expect("green span end");
        assert!(color_start < math && math < color_end, "{body}");
        assert!(color_end < after, "color escaped its TeX group: {body}");
        assert!(!body.contains("ForestGreen"), "color name leaked: {body}");
    }

    #[test]
    fn scoped_color_preserves_citations_numbered_displays_comments_and_literals() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "{\\color{red} see \\cite{smith} % hidden }\n",
            "\\verb|}| tail\n",
            "\\begin{equation}x=1\\label{eq:colored}\\end{equation}",
            "}\n",
            "See \\eqref{eq:colored}.\n",
            "\\end{document}\n",
        ));
        assert!(
            body.contains(r#"<div class="src-word text-color""#)
                && body.contains(r#"style="color:#FF0000""#),
            "{body}"
        );
        assert!(body.contains(r#"class="cite""#), "{body}");
        assert!(
            body.contains(r#"<code class="inline-literal""#) && body.contains(r#">}</code>"#),
            "{body}"
        );
        assert!(body.contains(r#"class="math display""#), "{body}");
        assert!(
            body.contains(r#"data-target="eq:colored" data-kind="eqref">(1)</a>"#),
            "{body}"
        );
        assert!(!body.contains("hidden }"), "comment leaked: {body}");
    }

    #[test]
    fn block_scoped_color_is_a_sync_container() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            concat!(
                "\\begin{document}\n",
                "{\\color{red} before\n",
                "\\begin{equation}x=1\\end{equation}\n",
                "after}\n",
                "\\end{document}\n",
            )
            .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();
        let wrapper = out
            .sync
            .entries
            .iter()
            .find(|entry| {
                entry.element_id.starts_with("srcw-")
                    && entry.kind == crate::sync::SyncKind::Container
            })
            .expect("block color sync container");
        let selected = out.sync.leaves_in_range(Path::new("t.tex"), 2, 1, 4, 6);
        assert!(
            !selected.contains(&wrapper.element_id),
            "selection included the whole colored subtree: {selected:?}"
        );
    }

    #[test]
    fn color_groups_preserve_space_and_restore_the_outer_switch() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "a{\\color{red} b}c\n",
            "\\section{\\color{blue}A {B \\color{red}C} D}\n",
            "\\color{red} body switch\n",
            "\\end{document}\n",
        ));
        assert!(text_content(&body).contains("a bc"), "{body}");
        assert!(
            body.contains(
                r#"<span class="text-color" style="color:#0000FF">A B <span class="text-color" style="color:#FF0000">C</span> D</span>"#
            ),
            "nested color did not restore blue: {body}"
        );
        assert!(
            body.contains(r#"<span class="text-color" style="color:#FF0000"> body switch"#,),
            "body-level color switch was split from its text: {body}"
        );
    }

    #[test]
    fn color_switch_inside_user_macro_expands_with_its_tex_scope() {
        let body = render_body(concat!(
            "\\newcommand{\\ar}[1]{{\\color{red}{#1}}}\n",
            "\\begin{document}\n",
            "plain \\ar{solution $x$} plain\n",
            "\\end{document}\n",
        ));
        assert!(
            body.contains(r#"<span class="text-color" style="color:#FF0000">solution "#),
            "{body}"
        );
        assert!(body.contains(r#"class="math inline""#), "{body}");
        assert!(!body.contains(">red"), "color argument leaked: {body}");
        assert_eq!(
            body.matches(r#"style="color:#FF0000""#).count(),
            1,
            "{body}"
        );
    }

    #[test]
    fn color_models_custom_definitions_aliases_and_mixes_render_safely() {
        let body = render_body(concat!(
            "\\usepackage{xcolor}\n",
            "\\definecolor{brand}{HTML}{336699}\n",
            "\\colorlet{softbrand}{brand!50!white}\n",
            "\\begin{document}\n",
            "\\textcolor[RGB]{255,128,0}{A}\n",
            "\\textcolor[rgb]{1,0.5,0}{B}\n",
            "\\textcolor[gray]{0.5}{C}\n",
            "\\textcolor[cmyk]{0,1,1,0}{D}\n",
            "\\textcolor{brand}{E}\n",
            "\\textcolor{softbrand}{F}\n",
            "\\textcolor{red!50!blue}{G}\n",
            "\\end{document}\n",
        ));
        assert_eq!(body.matches("color:#FF8000").count(), 2, "{body}");
        for expected in [
            "color:#808080",
            "color:#FF0000",
            "color:#336699",
            "color:#99B3CC",
            "color:#800080",
        ] {
            assert!(body.contains(expected), "missing {expected}: {body}");
        }
    }

    #[test]
    fn invalid_colors_are_consumed_without_style_injection_or_text_loss() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\textcolor{red;position:fixed}{kept} ",
            "{\\color[HTML]{bad-onload}also kept} end\n",
            "\\end{document}\n",
        ));
        assert!(body.contains("kept"), "{body}");
        assert!(body.contains("also kept"), "{body}");
        assert!(body.contains("end"), "{body}");
        assert!(!body.contains("position:fixed"), "{body}");
        assert!(!body.contains("bad-onload"), "{body}");
    }

    #[test]
    fn color_boxes_render_models_nested_formatting_and_math() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\colorbox[rgb]{1,0.5,0}{hot $x$} ",
            "\\fcolorbox[RGB]{0,0,255}[HTML]{FFFF00}{\\textbf{notice}}\n",
            "\\end{document}\n",
        ));
        assert!(
            body.contains(r#"class="text-color-box" style="background-color:#FF8000""#),
            "{body}"
        );
        assert!(body.contains(r#"class="math inline""#), "{body}");
        assert!(
            body.contains(
                r#"class="text-color-box text-color-frame" style="background-color:#FFFF00;border:1px solid #0000FF""#
            ),
            "{body}"
        );
        assert!(body.contains("<strong>notice</strong>"), "{body}");
    }

    #[test]
    fn color_boxes_preserve_the_surrounding_foreground() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "{\\color{red}\\colorbox{yellow}{alert $x$}}\n",
            "\\end{document}\n",
        ));
        assert!(
            body.contains(r#"style="color:#FF0000""#)
                && body.contains(
                    r#"<span class="text-color-box" style="background-color:#FFFF00">alert "#
                ),
            "{body}"
        );
        assert!(!body.contains("background-color:#FFFF00;color:"), "{body}");
    }

    #[test]
    fn custom_text_color_wraps_inline_math_without_changing_math_tex() {
        let body = render_body(concat!(
            "\\usepackage{xcolor}\n",
            "\\definecolor{brand}{HTML}{336699}\n",
            "\\begin{document}\n",
            "\\textcolor{brand}{word $x$}\n",
            "\\end{document}\n",
        ));
        assert!(
            body.contains(r#"class="text-color" style="color:#336699">word "#),
            "{body}"
        );
        assert!(
            body.contains(r#"data-mathjax-tex="\(x\)""#) && body.contains(r#"data-tex="\(x\)""#),
            "copy/source TeX was changed: {body}"
        );
    }

    fn tm(html: &str) -> crate::config::TextMacro {
        crate::config::TextMacro { html: html.to_string(), n_args: None, default: None }
    }

    #[test]
    fn toml_text_macro_html_template_for_unseen_macro() {
        let mut opts = HtmlOptions::default();
        opts.text_macros
            .insert("GI".to_string(), tm(r#"<span class="gi">#1</span>"#));
        let body = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nnote \\GI{check this} end\n\\end{document}\n".to_string(),
            &opts,
        )
        .unwrap()
        .body_html;
        assert!(body.contains(r#"<span class="gi">check this</span>"#), "{body}");
    }

    #[test]
    fn toml_text_macro_default_and_explicit_nargs() {
        // MathJax-style [template, n_args, default]: \hl{x} uses the default
        // first arg; \hl[pink]{x} overrides it.
        let mut opts = HtmlOptions::default();
        opts.text_macros.insert(
            "hl".to_string(),
            crate::config::TextMacro {
                html: r#"<mark style="background:#1">#2</mark>"#.to_string(),
                n_args: Some(2),
                default: Some("yellow".to_string()),
            },
        );
        let body = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\hl{a} \\hl[pink]{b}\n\\end{document}\n".to_string(),
            &opts,
        )
        .unwrap()
        .body_html;
        assert!(body.contains(r#"<mark style="background:yellow">a</mark>"#), "{body}");
        assert!(body.contains(r#"<mark style="background:pink">b</mark>"#), "{body}");
    }

    #[test]
    fn toml_text_macro_overrides_newcommand() {
        let mut opts = HtmlOptions::default();
        opts.text_macros
            .insert("hi".to_string(), tm("<b>TOML</b>"));
        let body = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\newcommand{\\hi}{tex}\n\\begin{document}\n\\hi\n\\end{document}\n".to_string(),
            &opts,
        )
        .unwrap()
        .body_html;
        assert!(body.contains("<b>TOML</b>"), "{body}");
        assert!(!text_content(&body).contains("tex"), "{body}");
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
    fn latex_list_is_one_top_level_structural_block() {
        let out = crate::render_project_from_source(
            Path::new("list.tex"),
            concat!(
                "\\begin{document}\n",
                "\\begin{enumerate}\n",
                "\\item First item.\n",
                "\\item Second item.\n",
                "\\end{enumerate}\n",
                "\\end{document}\n",
            )
            .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert_eq!(out.blocks.len(), 1, "{}", out.body_html);
        let block = &out.blocks[0].html;
        assert!(block.starts_with(r#"<article class="blk""#), "{block}");
        assert!(
            block.contains(r#"<ol class="latex-list enumerate">"#),
            "{block}"
        );
        assert!(block.ends_with("</ol>\n</article>"), "{block}");
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
            "\\newtheorem{lemma}{Lemma}\n\\begin{document}\n\\begin{lemma}[$Y$-energy]\nStatement.\n\\end{lemma}\n\\end{document}\n".to_string(),
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
    fn inline_command_is_source_mapped_so_clicks_dont_hit_the_box() {
        // `\emph{…}` / `\textbf{…}` render as OpaqueCmd nodes; without a data-src
        // of their own, a source-jump click inside (e.g. an \emph-wrapped theorem
        // statement) walks up to the enclosing box and jumps there. They're now
        // wrapped in a source-mapped span pointing at the command's own line.
        let body = render_body(
            "\\begin{document}\nText \\emph{emphasized phrase} here.\n\\end{document}\n",
        );
        assert!(
            body.contains(r#"data-src="t.tex:2:6"><em>emphasized phrase</em></span>"#),
            "emph not wrapped in a source-mapped span: {body}"
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

    #[test]
    fn paper_title_inline_math_is_typeset() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\title{The $L^p$ space}\n\\begin{document}\n\\maketitle\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        let start = out
            .body_html
            .find(r#"<h1 class="paper-title">"#)
            .expect("paper-title h1");
        let end = start + out.body_html[start..].find("</h1>").expect("h1 close");
        let title = &out.body_html[start..end];
        assert!(
            title.contains(r#"<span class="math inline""#) && title.contains(r#"data-tex="\(L^p\)""#),
            "math in \\title should be MathJax-typeset; got: {title}",
        );
        assert!(
            !title.contains("$L^p$"),
            "literal $L^p$ should not survive in the title; got: {title}",
        );
    }

    #[test]
    fn inline_math_inside_emph_is_typeset() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nText \\emph{with $x^2$ inside}.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(
            out.body_html.contains(r#"<em>"#)
                && out.body_html.contains(r#"<span class="math inline""#)
                && out.body_html.contains(r#"data-tex="\(x^2\)""#),
            "math inside \\emph should be MathJax-typeset; got: {}",
            out.body_html,
        );
        assert!(
            !out.body_html.contains("$x^2$"),
            "literal $x^2$ should not survive inside \\emph; got: {}",
            out.body_html,
        );
    }

    #[test]
    fn old_style_font_switches_render_as_strong_em() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\nA {\\bf bold word}, {\\em emph word}, {\\it italic word}, and {\\tt code word}.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(
            out.body_html.contains("<strong>") && out.body_html.contains("bold word</strong>"),
            "{{\\bf …}} should wrap in <strong>; got: {}",
            out.body_html,
        );
        assert!(
            out.body_html.contains("<em>") && out.body_html.contains("emph word</em>"),
            "{{\\em …}} should wrap in <em>; got: {}",
            out.body_html,
        );
        assert!(
            out.body_html.contains("italic word</em>"),
            "{{\\it …}} should wrap in <em>; got: {}",
            out.body_html,
        );
        assert!(
            out.body_html.contains("<code>") && out.body_html.contains("code word</code>"),
            "{{\\tt …}} should wrap in <code>; got: {}",
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
            "\\newtheorem{proposition}{Proposition}\n\\begin{document}\n\\begin{proposition}\\label{prop:foo}\nStatement.\n\\end{proposition}\n\\section{See \\ref{prop:foo} and \\eqref{eq:x} and \\autoref{prop:foo}}\n\\begin{equation}\\label{eq:x}\na=b\n\\end{equation}\n\\end{document}\n".to_string(),
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
            "\\newtheorem{theorem}{Theorem}\n\\begin{document}\n\\section{Intro}\\label{sec:intro}\n\\begin{theorem}[role=main]\\label{thm:main}\nStatement.\n\\end{theorem}\n\\begin{equation}\n\\label{eq:main}\na=b\n\\label{eq:alias}\n\\end{equation}\n\\begin{figure}\n\\caption{Plot.}\\label{fig:plot}\n\\end{figure}\nLoose\\label{misc:loose}.\n\\end{document}\n"
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
    fn standalone_tabular_renders_columns_rules_multicolumn_and_inline_content() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            concat!(
                "\\begin{document}\n",
                "\\begin{tabular}{|l|cr|}\n",
                "\\toprule\n",
                "Name & \\multicolumn{2}{c|}{Results}\\\\\n",
                "\\midrule\n",
                "Ada \\& Charles & \\textbf{Score} $x^2$ & ",
                "\\textcolor{red}{10}\\\\[2pt]\n",
                "\\bottomrule\n",
                "\\end{tabular}\n",
                "\\end{document}\n",
            )
            .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(
            out.body_html
                .contains(r#"<table class="latex-tabular env-tabular""#),
            "{}",
            out.body_html
        );
        assert!(out.body_html.contains(r#"colspan="2""#));
        assert!(out.body_html.contains("align-left"));
        assert!(out.body_html.contains("align-center"));
        assert!(out.body_html.contains("align-right"));
        assert!(out.body_html.contains("rule-left"));
        assert!(out.body_html.contains("rule-right"));
        assert!(out.body_html.contains("rule-top-strong"));
        assert!(out.body_html.contains("rule-top"));
        assert!(out.body_html.contains("rule-bottom-strong"));
        assert!(out.body_html.contains("Ada &amp; Charles"));
        assert!(out.body_html.contains("<strong>Score</strong>"));
        assert!(out.body_html.contains(r#"class="math inline""#));
        assert!(out.body_html.contains(">10</span>"));
        assert!(!out.body_html.contains(r#"\multicolumn"#));
        assert!(!out.body_html.contains(r#"\textcolor"#));
        assert!(!out
            .body_html
            .contains(r#"class="opaque-env" data-env="tabular""#));
        assert!(
            out.blocks
                .iter()
                .any(|block| block.html.contains("latex-tabular")
                    && block
                        .src
                        .as_deref()
                        .is_some_and(|src| src.contains("t.tex:2:"))),
            "tabular block lost its outer source mapping: {:?}",
            out.blocks
                .iter()
                .map(|block| &block.src)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn table_citations_render_and_populate_the_bibliography_in_source_order() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            concat!(
                "\\begin{document}\n",
                "Before \\cite{before}.\n",
                "\\begin{tabular}{l}\n",
                "\\textbf{Evidence \\cite[see][p.~2]{cell-a,cell-b}}\\\\\n",
                "\\end{tabular}\n",
                "\\begin{table}\n",
                "\\caption{Sources \\cite{caption}}\n",
                "\\begin{tabular}{l}\\cite{float-cell}\\\\\\end{tabular}\n",
                "\\end{table}\n",
                "\\printbibliography\n",
                "\\end{document}\n",
            )
            .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(
            out.body_html
                .contains(r#"data-key="cell-a">2</a>; <a class="cite""#),
            "{}",
            out.body_html
        );
        assert!(out.body_html.contains(r#"data-key="cell-b">3</a>"#));
        assert!(out.body_html.contains(r#"data-key="caption">4</a>"#));
        assert!(out.body_html.contains(r#"data-key="float-cell">5</a>"#));
        assert!(out
            .body_html
            .contains(r#"<strong>Evidence <span class="cite-group">["#));

        let references = out
            .body_html
            .split(r#"<section class="references""#)
            .nth(1)
            .expect("bibliography rendered");
        let positions = ["before", "cell-a", "cell-b", "caption", "float-cell"].map(|key| {
            references
                .find(&format!(r#"data-key="{key}""#))
                .unwrap_or_else(|| panic!("missing bibliography entry for {key}: {references}"))
        });
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn table_live_source_joins_comments_and_ignores_dormant_citations() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{tabular}{l}\n",
            "A% this newline is consumed\n",
            "B\\\\\n",
            "\\iftrue Live \\cite{live}\\else Hidden \\cite{hidden}\\fi\\\\\n",
            "\\newcommand{\\stored}{Stored \\cite{stored}}\n",
            "\\DeclareDocumentCommand{\\storedx}{}{StoredX \\cite{stored-x}}\n",
            "\\NewDocumentEnvironment{stored-env}{}",
            "{StoredEnv \\cite{stored-env}}{}\n",
            "After\\\\\n",
            "\\verb|\\cite{literal}|\\\\\n",
            "\\end{tabular}\n",
            "\\printbibliography\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(">AB</td>"), "{body}");
        assert!(body.contains(r#"data-key="live">1</a>"#), "{body}");
        assert!(!body.contains("Hidden"), "{body}");
        assert!(!body.contains("Stored"), "{body}");
        for key in ["hidden", "stored", "stored-x", "stored-env", "literal"] {
            assert!(
                !body.contains(&format!(r#"data-key="{key}""#)),
                "dormant citation {key} was registered: {body}"
            );
        }
    }

    #[test]
    fn textcolor_and_user_macro_citations_inside_tables_are_registered() {
        let body = render_body(concat!(
            "\\newcommand{\\tcite}[1]{\\cite{#1}}\n",
            "\\newcommand{\\discard}[1]{}\n",
            "\\newcommand{\\wrapper}[1]{\\discard{#1}}\n",
            "\\begin{document}\n",
            "\\begin{tabular}{llll}\n",
            "\\textcolor{red}{\\cite{colored}} & \\tcite{wrapped} & ",
            "\\wrapper{\\cite{discarded}} & \\cite{real}\\\\\n",
            "\\end{tabular}\n",
            "\\printbibliography\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"data-key="colored">1</a>"#), "{body}");
        assert!(body.contains(r#"data-key="wrapped">2</a>"#), "{body}");
        assert!(body.contains(r#"data-key="real">3</a>"#), "{body}");
        assert!(!body.contains(r#"data-key="discarded""#), "{body}");
        let references = body
            .split(r#"<section class="references""#)
            .nth(1)
            .expect("bibliography rendered");
        assert!(references.contains(r#"data-key="colored""#), "{references}");
        assert!(references.contains(r#"data-key="wrapped""#), "{references}");
        assert!(references.contains(r#"data-key="real""#), "{references}");
    }

    #[test]
    fn unrelated_text_macros_cannot_exhaust_table_citation_discovery() {
        let mut source = concat!(
            "\\newcommand{\\fmt}[1]{\\textbf{#1}}\n",
            "\\newcommand{\\tcite}[1]{\\cite{#1}}\n",
            "\\begin{document}\n",
        )
        .to_string();
        for _ in 0..1_024 {
            source.push_str("\\fmt{x}\n");
        }
        source.push_str(concat!(
            "\\begin{tabular}{l}\\tcite{late}\\\\\\end{tabular}\n",
            "\\printbibliography\n",
            "\\end{document}\n",
        ));
        let body = render_body(&source);
        assert!(body.contains(r#"data-key="late">1</a>"#), "{body}");
    }

    #[test]
    fn branching_recursive_text_macro_has_a_bounded_work_budget() {
        let body = render_body(concat!(
            "\\newcommand{\\dup}{\\dup\\dup}\n",
            "\\begin{document}\n",
            "\\begin{tabular}{l}\\dup\\\\\\end{tabular}\n",
            "\\cite{after}\n",
            "\\printbibliography\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"data-key="after">1</a>"#), "{body}");
    }

    #[test]
    fn text_macro_work_budget_remains_global_across_inline_roots() {
        super::TEXT_MACRO_DOCUMENT_EXPANSIONS_LEFT.with(|budget| budget.set(1));
        super::TEXT_MACRO_DOCUMENT_BYTES_LEFT
            .with(|budget| budget.set(super::MAX_DOCUMENT_TEXT_MACRO_EXPANDED_BYTES));
        super::TEXT_MACRO_EXPANSIONS_LEFT
            .with(|budget| budget.set(super::MAX_TEXT_MACRO_EXPANSIONS));
        assert!(super::reserve_text_macro_call());

        // Simulate the next independently rendered field/cell. Its local
        // recursion budget resets, but the document-wide allowance does not.
        super::TEXT_MACRO_EXPANSIONS_LEFT
            .with(|budget| budget.set(super::MAX_TEXT_MACRO_EXPANSIONS));
        assert!(!super::reserve_text_macro_call());
    }

    #[test]
    fn unsupported_environment_inside_a_cell_keeps_content_and_marks_boundaries() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{tabular}{l}\n",
            "\\begin{mystery}Hello $x$\\end{mystery}\\\\\n",
            "\\end{tabular}\n",
            "\\end{document}\n",
        ));
        assert_eq!(body.matches("unsupported-env-inline").count(), 2, "{body}");
        assert!(body.contains(r#"\begin{mystery}"#), "{body}");
        assert!(body.contains(r#"\end{mystery}"#), "{body}");
        assert!(body.contains("Hello"), "{body}");
        assert!(body.contains(r#"class="math inline""#), "{body}");
    }

    #[test]
    fn literal_environment_inside_a_cell_is_inert_and_cannot_shift_citations() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{tabular}{p{5cm}}\n",
            "\\begin{lstlisting}\n",
            "first \\end{other}\n",
            "inside & literal \\\\ break\n",
            "\\cite{fake} 50%\n",
            "\\end{lstlisting}\\\\\n",
            "After\\\\\n",
            "\\end{tabular}\n",
            "\\cite{real}\n",
            "\\printbibliography\n",
            "\\end{document}\n",
        ));
        assert_eq!(body.matches("<tr").count(), 2, "{body}");
        assert!(body.contains(r#"class="table-literal-env" data-env="lstlisting""#), "{body}");
        assert!(body.contains("inside &amp; literal \\\\ break"), "{body}");
        assert!(body.contains(r#"\cite{fake} 50%"#), "{body}");
        assert!(!body.contains(r#"data-key="fake""#), "{body}");
        assert!(body.contains(r#"data-key="real">1</a>"#), "{body}");
    }

    #[test]
    fn table_float_renders_nested_tabular_caption_number_and_reference() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            concat!(
                "\\begin{document}\n",
                "\\begin{table}[ht]\n",
                "\\centering\n",
                "\\caption{Scores at $t=1$.}\\label{tab:scores}\n",
                "\\begin{tabular}{lc}\n",
                "Name & Score\\\\\n",
                "\\hline\n",
                "Ada & 10\\\\\n",
                "\\end{tabular}\n",
                "\\end{table}\n",
                "See Table~\\ref{tab:scores}.\n",
                "\\end{document}\n",
            )
            .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out
            .body_html
            .contains(r#"class="float-placeholder float-table""#));
        assert!(out.body_html.contains(r#"id="tab-scores""#));
        assert!(out
            .body_html
            .contains(r#"<span class="float-kind">Table 1.</span>"#));
        assert!(out.body_html.contains("Scores at "));
        assert_eq!(out.body_html.matches(r#"class="math inline""#).count(), 1);
        assert!(out
            .body_html
            .contains(r#"<table class="latex-tabular env-tabular""#));
        assert!(out
            .body_html
            .contains(r#"data-target="tab:scores" data-kind="ref">1</a>"#));
        assert!(!out.body_html.contains("content omitted from preview"));
        assert!(!out.body_html.contains(r#"\begin{tabular}"#));
    }

    #[test]
    fn resizebox_wrapped_tabular_renders_natively() {
        // Regression: `\resizebox{\textwidth}{!}{\begin{tabular}…}` outside a
        // float left the tabular inside the command's brace argument, where
        // prose parsing degraded it into unsupported-env chips with raw `&`
        // cells (a float would have recovered it via first_nested_tabular;
        // `\begin{center}` did not). The box transforms now unwrap: sizing
        // args are dropped and the content parses as block content.
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{center}\n",
            "\\resizebox{\\textwidth}{!}{\\begin{tabular}{@{}ll@{}}\n",
            "a & b \\\\\n",
            "c & d\n",
            "\\end{tabular}}\n",
            "\\end{center}\n",
            "\\end{document}\n",
        ));
        assert!(
            body.contains(r#"<table class="latex-tabular env-tabular""#),
            "{body}"
        );
        assert!(!body.contains("unsupported-env"), "{body}");
        assert!(!body.contains("textwidth"), "sizing args leaked: {body}");

        // \scalebox{s}[v]{content} and \rotatebox[opts]{angle}{content} take
        // the same unwrap; starred \resizebox* too, plus TeX-legal spellings
        // the brace-only scan missed: a %-comment between sizing args and an
        // undelimited control-sequence arg.
        for wrapper in [
            "\\scalebox{0.8}[0.9]",
            "\\rotatebox[origin=c]{90}",
            "\\resizebox*{\\textwidth}{!}",
            "\\resizebox{\\textwidth}%\n{!}",
            "\\resizebox\\columnwidth{!}",
        ] {
            let body = render_body(&format!(
                "\\begin{{document}}\n{wrapper}{{\\begin{{tabular}}{{ll}}\nx & y\n\\end{{tabular}}}}\n\\end{{document}}\n"
            ));
            assert!(
                body.contains(r#"<table class="latex-tabular env-tabular""#),
                "{wrapper} did not unwrap: {body}"
            );
        }

        // Malformed (no content group): must not panic, sizing args are
        // consumed, following prose still renders.
        let body = render_body(
            "\\begin{document}\n\\resizebox{\\textwidth}{!} and prose continues.\n\\end{document}\n",
        );
        assert!(
            text_content(&body).contains("and prose continues."),
            "{body}"
        );
    }

    #[test]
    fn tabular_variants_and_longtable_caption_render_natively() {
        for (env, args, width) in [
            ("tabular*", r"{0.8\linewidth}{lr}", "width:80%"),
            ("tabularx", r"{\linewidth}{lX}", "width:100%"),
        ] {
            let body = render_body(&format!(
                "\\begin{{document}}\n\\begin{{{env}}}{args}\nA & B\\\\\n\\end{{{env}}}\n\\end{{document}}\n"
            ));
            assert!(
                body.contains(&format!(
                    r#"<table class="latex-tabular env-{}""#,
                    super::util::sanitize_id(env)
                )),
                "{env} did not render: {body}"
            );
            assert!(body.contains(width), "{env} width was lost: {body}");
            assert!(
                !body.contains(&format!(r#"opaque-env" data-env="{env}""#)),
                "{env} used opaque fallback: {body}"
            );
        }

        let longtable = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{longtable}{lr}\n",
            "\\caption{Long values}\\label{tab:long}\\\\\n",
            "Name & Value\\\\\n",
            "\\hline\n",
            "Ada & 10\\\\\n",
            "\\end{longtable}\n",
            "See \\ref{tab:long}.\n",
            "\\end{document}\n",
        ));
        assert!(longtable.contains(r#"class="latex-tabular env-longtable""#));
        assert!(longtable.contains(r#"id="tab-long""#));
        assert!(longtable.contains("<caption>"));
        assert!(longtable.contains(r#"<span class="float-kind">Table 1.</span>"#));
        assert!(longtable.contains(r#"data-target="tab:long" data-kind="ref">1</a>"#));
        assert!(!longtable.contains(r#"\caption"#));
        assert!(!longtable.contains(r#"\label"#));
    }

    #[test]
    fn longtable_numbers_unlabeled_captions_and_keeps_starred_caption_text() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{longtable}{l}\\caption{First}\\\\A\\\\\\end{longtable}\n",
            "\\begin{longtable}{l}\\caption*{Second}\\\\B\\\\\\end{longtable}\n",
            "\\begin{longtable}{l}\\caption{Third}\\\\C\\\\\\end{longtable}\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"<span class="float-kind">Table 1.</span> First"#), "{body}");
        assert!(body.contains("<caption>Second</caption>"), "{body}");
        assert!(body.contains(r#"<span class="float-kind">Table 3.</span> Third"#), "{body}");
        assert!(!body.contains("Table 2."), "{body}");
    }

    #[test]
    fn ordinary_float_caption_hides_its_nested_label_source() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{table}\n",
            "\\caption{Scores\\label{tab:scores}}\n",
            "\\begin{tabular}{l}Ada\\\\\\end{tabular}\n",
            "\\end{table}\n",
            "See \\ref{tab:scores}.\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"<span class="float-kind">Table 1.</span> Scores"#), "{body}");
        assert!(body.contains(r#"data-target="tab:scores" data-kind="ref">1</a>"#), "{body}");
        assert!(!body.contains(r#"\label"#), "{body}");
        assert!(!body.contains(">tab:scores<"), "{body}");
    }

    #[test]
    fn scoped_text_color_is_inherited_by_a_native_table() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "{\\color{red}\n",
            "\\begin{tabular}{l}Red\\\\\\end{tabular}\n",
            "}\n",
            "\\end{document}\n",
        ));
        let color = body.find(r#"style="color:#FF0000""#).expect("red wrapper");
        let table = body.find(r#"class="latex-tabular env-tabular""#).expect("table");
        assert!(color < table, "{body}");
        assert!(body[table..].contains(">Red</td>"), "{body}");
    }

    #[test]
    fn malformed_tabular_keeps_safe_opaque_source() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\begin{tabular}\n",
            "Still visible $x$ & source.\\\\\n",
            "\\end{tabular}\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"class="opaque-env" data-env="tabular""#));
        assert!(body.contains("Still visible $x$ &amp; source."));
        assert!(!body.contains(r#"<table class="latex-tabular"#));
        assert!(!body.contains(r#"class="math inline"#));
    }

    #[test]
    fn mathematical_array_stays_on_the_mathjax_path() {
        let body = render_body(concat!(
            "\\begin{document}\n",
            "\\[\\begin{array}{cc}a&b\\\\c&d\\end{array}\\]\n",
            "\\end{document}\n",
        ));
        assert!(body.contains(r#"class="math display""#));
        assert!(body.contains(r#"\begin{array}{cc}"#));
        assert!(!body.contains(r#"class="latex-tabular"#));
        assert!(!body.contains(r#"class="opaque-env" data-env="array""#));
    }

    #[test]
    fn tabular_css_has_alignment_rules_and_local_overflow() {
        let css = super::shell::DEFAULT_CSS;
        assert!(css.contains(".latex-tabular-scroll {"));
        assert!(css.contains("container-type: inline-size;"));
        assert!(css.contains("overflow-x: auto;"));
        assert!(css.contains("color: inherit;"));
        assert!(css.contains(".latex-tabular-cell.align-left"));
        assert!(css.contains(".latex-tabular-cell.align-center"));
        assert!(css.contains(".latex-tabular-cell.align-right"));
        assert!(css.contains(".latex-tabular-cell.valign-top"));
        assert!(css.contains(".latex-tabular-cell.rule-top-strong"));
        assert!(css.contains(".latex-tabular-cell.rule-bottom-strong"));
        assert!(css.contains(".latex-tabular-cell.cell-wrap"));
        assert!(css.contains(".unsupported-env-inline {"));
    }

    #[test]
    fn tikz_environments_become_lazy_svg_assets_when_enabled() {
        let mut opts = HtmlOptions::default();
        opts.viewer_config.render_tikz = true;
        opts.tikz_asset_base = Some("/tikz/".to_string());
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\documentclass{article}\n\\usepackage{tikz}\n\\begin{document}\n\\begin{tikzpicture}[scale=2]\n\\draw (0,0) -- (1,1);\n\\end{tikzpicture}\n\\end{document}\n"
                .to_string(),
            &opts,
        )
        .unwrap();

        assert_eq!(out.tikz_assets.len(), 1);
        let (hash, asset) = out.tikz_assets.iter().next().unwrap();
        assert_eq!(asset.environment, "tikzpicture");
        assert!(asset.body.starts_with("[scale=2]"));
        assert!(asset.preamble.contains(r"\usepackage{tikz}"));
        assert!(out.body_html.contains(r#"class="tikz-diagram""#));
        assert!(out
            .body_html
            .contains(&format!(r#"data-tikz-src="/tikz/{hash}.svg""#)));
        assert!(!out
            .body_html
            .contains(&format!(r#" src="/tikz/{hash}.svg""#)));
        assert!(!out.body_html.contains(r#"class="opaque-env""#));
        assert!(!out.body_html.contains(r#"class="math "#));
    }

    #[test]
    fn tikz_inside_figure_keeps_caption_and_has_safe_disabled_placeholder() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\documentclass{article}\n\\usepackage{tikz}\n\\begin{document}\n\\begin{figure}\n\\centering\n\\begin{tikzpicture}\n\\draw (0,0) circle (1cm);\n\\end{tikzpicture}\n\\caption{A circle.}\\label{fig:circle}\n\\end{figure}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert_eq!(out.tikz_assets.len(), 1);
        assert!(out
            .body_html
            .contains(r#"class="float-placeholder float-figure""#));
        assert!(out.body_html.contains("A circle."));
        assert!(out.body_html.contains("TikZ preview disabled."));
        assert!(out.body_html.contains("render-tikz = true"));
        assert!(!out.body_html.contains(r#"\draw"#));
    }

    #[test]
    fn float_nested_diagram_discovery_ignores_commented_fake_begin() {
        let mut opts = HtmlOptions::default();
        opts.viewer_config.render_tikz = true;
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            concat!(
                "\\documentclass{article}\n",
                "\\usepackage{circuitikz}\n",
                "\\begin{document}\n",
                "\\begin{figure}\n",
                "% \\begin{tikzpicture}\\draw (9,9)--(8,8);\\end{tikzpicture}\n",
                "\\begin{circuitikz}\n",
                "\\draw (0,0)--(1,1);\n",
                "\\end{circuitikz}\n",
                "\\end{figure}\n",
                "\\end{document}\n",
            )
            .to_string(),
            &opts,
        )
        .unwrap();

        assert_eq!(out.tikz_assets.len(), 1);
        let asset = out.tikz_assets.values().next().unwrap();
        assert_eq!(asset.environment, "circuitikz");
        assert!(asset.body.contains(r"\draw (0,0)--(1,1);"));
        assert!(!asset.body.contains("(9,9)"));
    }

    #[test]
    fn tikzcd_is_lazy_and_static_render_never_emits_a_dead_asset_url() {
        let mut opts = HtmlOptions::default();
        opts.viewer_config.render_tikz = true;
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\documentclass{article}\n\\usepackage{tikz-cd}\n\\begin{document}\n\\begin{tikzcd}\nA \\arrow[r] & B\n\\end{tikzcd}\n\\end{document}\n"
                .to_string(),
            &opts,
        )
        .unwrap();

        assert_eq!(out.tikz_assets.len(), 1);
        assert_eq!(
            out.tikz_assets.values().next().unwrap().environment,
            "tikzcd"
        );
        assert!(out
            .body_html
            .contains("TikZ preview needs live server mode."));
        assert!(!out.body_html.contains(r#"src="/tikz/"#));
        assert!(!out.body_html.contains(r#"\arrow"#));
    }

    #[test]
    fn forest_is_sent_to_the_lazy_native_diagram_renderer() {
        let mut opts = HtmlOptions::default();
        opts.viewer_config.render_tikz = true;
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\documentclass{article}\n\\usepackage{forest}\n\\begin{document}\n\\begin{forest}\n[A [B] [C]]\n\\end{forest}\n\\end{document}\n"
                .to_string(),
            &opts,
        )
        .unwrap();

        assert_eq!(out.tikz_assets.len(), 1);
        assert_eq!(
            out.tikz_assets.values().next().unwrap().environment,
            "forest"
        );
        assert!(!out
            .body_html
            .contains(r#"class="unsupported-env-boundary""#));
        assert!(!out.body_html.contains("[A [B] [C]]"));
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
    fn align_label_on_second_row_refs_that_rows_number() {
        // Regression: the env's primary label (first \label anywhere in the
        // body) was recorded against the FIRST numbered row, and record_label
        // is first-write-wins — so a label sitting on row 2 resolved to row
        // 1's number.
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{align}\na &= b \\\\\nc &= d \\label{eq:second}\n\\end{align}\nSee \\eqref{eq:second}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out
            .body_html
            .contains(r##"data-target="eq:second" data-kind="eqref">(2)</a>"##));
        assert!(!out.body_html.contains(r#"data-kind="eqref">(1)</a>"#));
    }

    #[test]
    fn showonlyrefs_numbers_only_referenced_align_rows() {
        // mathtools [showonlyrefs]: only referenced equations are numbered,
        // consecutively among the shown ones. Label+ref on row 2 only → row 1
        // gets no number, row 2 is (1), and \eqref resolves to (1).
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\usepackage[showonlyrefs]{mathtools}\n\\begin{document}\n\\begin{align}\na &= b \\\\\nc &= d \\label{eq:second}\n\\end{align}\nSee \\eqref{eq:second}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row empty"></span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row">(1)</span>"#));
        assert!(out
            .body_html
            .contains(r##"data-target="eq:second" data-kind="eqref">(1)</a>"##));
        assert!(!out.body_html.contains("(2)"), "{}", out.body_html);
    }

    #[test]
    fn showonlyrefs_via_mathtoolsset_skips_unreferenced_equation() {
        // The \mathtoolsset{showonlyrefs} spelling. An unreferenced equation
        // shows no number and does not tick the counter; the referenced one
        // after it is (1).
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\usepackage{mathtools}\n\\mathtoolsset{showonlyrefs}\n\\begin{document}\n\\begin{equation}\na = b\n\\end{equation}\n\\begin{equation}\nc = d \\label{eq:used}\n\\end{equation}\nSee \\eqref{eq:used}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r#"<span class="eq-num">(1)</span>"#));
        assert!(out
            .body_html
            .contains(r##"data-target="eq:used" data-kind="eqref">(1)</a>"##));
        assert!(!out.body_html.contains("(2)"), "{}", out.body_html);
    }

    #[test]
    fn showonlyrefs_preserves_equation_labels_inside_theorems() {
        // Regression for #5: the theorem parser used to claim the first label
        // anywhere in its body. That moved `eq2` from the nested equation onto
        // the theorem, suppressed the equation under showonlyrefs, and made
        // both references resolve to the theorem number.
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            concat!(
                "\\documentclass{minimal}\n",
                "\\newtheorem{theorem}{Theorem}\n",
                "\\usepackage{mathtools}\n",
                "\\mathtoolsset{showonlyrefs=true,showmanualtags=true}\n",
                "\\begin{document}\n",
                "\\begin{equation}\\label{eq1}E=mc^2\\end{equation}\n",
                "Reference: \\eqref{eq1}.\n",
                "\\begin{theorem}\n",
                "\\begin{equation}\\label{eq2}E=mc^2\\end{equation}\n",
                "Inside: \\eqref{eq2}.\n",
                "\\end{theorem}\n",
                "Outside: \\eqref{eq2}.\n",
                "\\end{document}\n",
            )
            .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(
            out.body_html.contains(r#"class="math display" id="eq2""#),
            "nested equation did not retain eq2: {}",
            out.body_html,
        );
        assert!(
            out.body_html.contains(r#"<span class="eq-num">(2)</span>"#),
            "nested equation was not numbered second: {}",
            out.body_html,
        );
        assert_eq!(
            out.body_html
                .matches(r##"data-target="eq2" data-kind="eqref">(2)</a>"##)
                .count(),
            2,
            "inside and outside refs should both resolve to equation 2: {}",
            out.body_html,
        );
        assert_eq!(out.body_html.matches(r#"data-refkey="eq2""#).count(), 1);
    }

    #[test]
    fn showonlyrefs_false_keeps_normal_numbering() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\usepackage[showonlyrefs=false]{mathtools}\n\\begin{document}\n\\begin{align}\na &= b \\\\\nc &= d \\label{eq:second}\n\\end{align}\nSee \\eqref{eq:second}.\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row">(1)</span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row">(2)</span>"#));
        assert!(out
            .body_html
            .contains(r##"data-target="eq:second" data-kind="eqref">(2)</a>"##));
    }

    #[test]
    fn align_trailing_row_separator_gets_no_phantom_number() {
        // Regression: a trailing `\\` leaves an empty final row that MathJax
        // does not render, but numbering still gave it a gutter number and
        // advanced the equation counter — a 2-row align showed (1)(2)(3) and
        // the next equation became (4).
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\begin{align}\na &= b \\\\\nc &= d \\\\\n\\end{align}\n\\begin{equation}\ne = f\n\\end{equation}\n\\end{document}\n"
                .to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row">(1)</span>"#));
        assert!(out
            .body_html
            .contains(r#"<span class="eq-num-row">(2)</span>"#));
        assert_eq!(
            out.body_html.matches("eq-num-row").count(),
            2,
            "phantom gutter row for the empty trailing align row: {}",
            out.body_html
        );
        assert!(out.body_html.contains(r#"<span class="eq-num">(3)</span>"#));
        assert!(!out.body_html.contains("(4)"));
    }

    #[test]
    fn hover_preview_uses_readable_document_scale() {
        let css = super::shell::DEFAULT_CSS;
        let start = css.find(".hover-preview {").expect("hover preview rule");
        let tail = &css[start..];
        let end = tail.find("\n}").expect("hover preview rule end");
        let rule = &tail[..end];

        assert!(
            rule.contains(concat!(
                "font-size: max(\n",
                "    calc(var(--body-font-size) * var(--hover-preview-scale, 1)),\n",
                "    calc(var(--body-font-size) * var(--page-scale, 1) * var(--hover-preview-scale, 1))\n",
                "  );"
            )),
            "hover preview must follow document sizing: {rule}"
        );
        assert!(
            !rule.contains("font-size: 13px"),
            "hover preview regressed to a fixed small font: {rule}"
        );
    }

    #[test]
    fn viewer_shell_contains_index_and_page_modes() {
        let out = crate::render_project_from_source(
            Path::new("t.tex"),
            "\\begin{document}\n\\section{Intro}\nText.\n\\end{document}\n".to_string(),
            &HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.html.contains(r#"id="viewer-side""#));
        assert!(out.html.contains(r#"id="side-toggle""#));
        assert!(out.html.contains(r#"id="side-index""#));
        assert!(out.html.contains(r#"data-page-mode="a4""#));
        assert!(out.html.contains(r#"data-page-mode="dynamic""#));
        assert!(out.html.contains(r#"data-viewer-action="page-a4""#));
        assert!(out.html.contains(r#"data-viewer-action="page-dynamic""#));
        assert!(out.html.contains(r#"id="refkey-toggle""#));
        assert!(out.html.contains(r#"id="lineno-toggle""#));
        assert!(out.html.contains("setLineNumbers"));
        assert!(out.html.contains("mathpreview.lineNumbers"));
        assert!(out.html.contains(r#"id="theme-toggle""#));
        assert!(out.html.contains("setTheme"));
        assert!(out.html.contains("mathpreview.theme"));
        assert!(out.html.contains(r#"data-refkeys="hidden""#));
        assert!(out.html.contains(r#"id="server-restart""#));
        assert!(out.html.contains(r#"id="server-stop""#));
        assert!(out.html.contains(r#"id="topbar-stripe""#));
        assert!(out.html.contains(r#"id="margin-cards""#));
        assert!(out.html.contains(r#"id="cmdline""#));
        assert!(out.html.contains(r#"id="cmdline-input""#));
        assert!(out.html.contains("pinByRefkey"));
        assert!(out.html.contains("togglePinByRefkey"));
        assert!(out
            .html
            .contains("togglePinByRefkey(refkeyChip.dataset.target"));
        assert!(out.html.contains("margin-card-expand"));
        assert!(out.html.contains("expand.textContent = '↔'"));
        assert!(!out.html.contains("margin-card-pin"));
        assert!(out
            .html
            .contains("calc(var(--body-font-size) * var(--page-scale, 1) * 1.15)"));
        assert!(out.html.contains("openCmdline"));
        assert!(out.html.contains("setRefkeysVisible"));
        assert!(out.html.contains("mathpreview.refkeys"));
        assert!(out.html.contains("refkey-visible"));
        assert!(out.html.contains("setTopbarHidden"));
        assert!(out.html.contains("mathpreview.topbarHidden"));
        assert!(out.html.contains("topbar-hidden"));
        assert!(out.html.contains("topbarOffset"));
        // Presence only — the exact value is pinned against the server const by
        // serve.rs's `client_ws_protocol_matches_server`, so this needn't churn
        // on every protocol bump.
        assert!(out.html.contains("WS_PROTOCOL_VERSION = '"));
        assert!(out.html.contains("margin-card-grip"));
        assert!(out.html.contains("initMarginDnd"));
        assert!(out.html.contains("decorateRefkeyChips"));
        assert!(out.html.contains("startup: { typeset: false }"));
        assert!(out.html.contains("queueInitialTypeset"));
        assert!(out.html.contains("ensureInitialTypeset"));
        assert!(out.html.contains("queueUntypesetMath"));
        assert!(out.html.contains("MutationObserver"));
        assert!(out.html.contains("scheduleTypesetFlush"));
        assert!(out.html.contains("startTikzScheduler"));
        assert!(out.html.contains("var tikzStates = new Map()"));
        assert!(out.html.contains("window.URL.createObjectURL(svg)"));
        assert!(out
            .html
            .contains("window.URL.revokeObjectURL(state.objectUrl)"));
        assert!(out.html.contains("tikzMathStartupDeadline"));
        assert!(out.html.contains("visibleMathHasTikzPriority"));
        assert!(out.html.contains("tex2svgPromise"));
        assert!(out.html.contains("mjx-container"));
        assert!(out.html.contains("oldEl.innerHTML = newEl.innerHTML"));
        assert!(out.html.contains(r#"id="search-panel""#));
        assert!(out.html.contains(r#"id="search-input""#));
        assert!(out.html.contains("handleViewerKeybindings"));
        assert!(out
            .html
            .contains("function viewerSequenceWaitsForCharacter()"));
        assert!(out
            .html
            .contains("armKeySequenceTimeout(viewerSequenceWaitsForCharacter())"));
        assert!(out.html.contains("if (waitForCharacter)"));
        assert!(out.html.contains("'● mark ' + name + ' restored'"));
        assert!(out
            .html
            .contains("if (handleViewerKeybindings(e)) {\n      e.preventDefault();\n    }"));
        assert!(out.html.contains("runViewerAction"));
        assert!(out.html.contains(r#"data-viewer-action="toggle-theme""#));
        assert!(out.html.contains(r#"data-viewer-action="print-pdf""#));
        assert!(out.html.contains(r#""go-top":["g g","Home"]"#));
        assert!(out
            .html
            .contains(r#""full-page-down":["Space","Ctrl+f","PageDown"]"#));
        assert!(out
            .html
            .contains(r#""full-page-up":["b","Ctrl+b","PageUp"]"#));
        assert!(out.html.contains(r#""five-lines-down":[]"#));
        assert!(out.html.contains(r#""five-lines-up":[]"#));
        assert!(out
            .html
            .contains(r#"keybindingAliases: {"J":"5j","K":"5k","Shift+Space":"b"}"#));
        assert!(out.html.contains(r#""toggle-lines":[]"#));
        for action in [
            "page-a4",
            "page-dynamic",
            "toggle-crop",
            "toggle-keys",
            "toggle-lines",
            "open-macros",
            "open-config",
            "toggle-log",
            "toggle-margin",
            "toggle-theme",
            "proof-main",
            "proof-supporting",
            "proof-all",
            "print-pdf",
            "restart-server",
            "stop-server",
            "toggle-topbar",
            "toggle-toc",
        ] {
            assert!(
                out.html
                    .contains(&format!(r#"data-viewer-action="{action}""#)),
                "fixed viewer control is missing action {action}"
            );
        }
        for action in crate::config::KEYBINDING_ACTIONS {
            assert!(
                out.html.contains(&format!("'{action}': function")),
                "configured action has no client implementation: {action}"
            );
        }
        assert!(out.html.contains("recordViewerPlace"));
        assert!(out.html.contains("currentViewerPlace"));
        assert!(out.html.contains("restoreViewerPlace"));
        assert!(out.html.contains("restorePreviousPlace"));
        assert!(out.html.contains("restoreNextPlace"));
        assert!(out.html.contains("viewerJumpList"));
        assert!(out.html.contains("viewerJumpIndex"));
        assert!(out.html.contains("checkpointViewerJumps"));
        assert!(out.html.contains("rollbackViewerJumps"));
        assert!(!out.html.contains("viewerJumpStack"));
        assert!(out.html.contains("if (!typing) recordViewerPlace()"));
        assert!(out
            .html
            .contains("viewerCountedDistance(viewerTextLineStep(), ctx)"));
        assert!(out
            .html
            .contains("viewerCountedDistance(viewerFiveLineStep(), ctx)"));
        assert!(out.html.contains("viewerCountedDistance(vh, ctx)"));
        assert!(out.html.contains("parseAliasExpansion"));
        assert!(out.html.contains("viewerCountDigits"));
        assert!(out.html.contains("fixedCount: resolved.fixedCount"));
        let line_step_start = out.html.find("function viewerTextLineStep()").unwrap();
        let five_step_start = out.html.find("function viewerFiveLineStep()").unwrap();
        let line_step = &out.html[line_step_start..five_step_start];
        assert!(line_step.contains("pageScalePlan(currentUserZoom).pageScale"));
        assert!(!line_step.contains("getBoundingClientRect"));
        assert!(!line_step.contains("offsetHeight"));
        assert!(out
            .html
            .contains("function primeStructuralBlockIntrinsicSizes(roots)"));
        // The structural pass primes EVERY top-level block (not just theorem
        // and list blocks): plain blocks' 180px estimates and WebKit's
        // re-skip forgetting made page motions drift around long equations.
        assert!(out
            .html
            .contains("block.parentElement === page"));
        // Typeset batches keep the reading position steady: the correction is
        // measured displacement, a no-op where native anchoring already ran.
        assert!(out
            .html
            .contains("function captureTypesetViewportAnchor()"));
        assert!(out
            .html
            .contains("settleTypesetViewportAnchor(anchor)"));
        assert!(out
            .html
            .contains("block.style.contain = 'layout style paint'"));
        assert!(out.html.contains("void page.offsetHeight"));
        assert!(out
            .html
            .contains("primeStructuralBlockIntrinsicSizes(touchedRoots)"));
        assert!(out
            .html
            .contains("primeStructuralBlockIntrinsicSizes(newReplacementBlocks)"));
        assert!(!out.html.contains("vimScrollHistory"));
        assert!(out.html.contains("seedTypesetBlockIntrinsicSizes(nodes)"));
        assert!(out
            .html
            .contains("var snapshots = snapshotBlockIntrinsicSizes(blocks);"));
        assert!(out
            .html
            .contains("seedBlockIntrinsicSizeSnapshots(snapshots);"));
        assert!(out.html.contains("seedCurrentBlockIntrinsicSizes(blocks);"));
        // Both priming branches route through the bounded viewport-ordered
        // queue — a mass-change patch must not synchronously prime hundreds
        // of blocks any more than the initial full pass may.
        assert!(out.html.contains("queueStructuralPrime(blocks, !roots);"));
        assert!(out
            .html
            .contains("function drainStructuralPrimeSlice()"));
        assert!(out
            .html
            .contains("function orderBlocksByViewportProximity(blocks)"));
        // Priming passes and typeset batches pin the reading position; the
        // settle aborts when the user scrolled during the async window.
        assert!(out
            .html
            .contains("settleTypesetViewportAnchor(viewportAnchor);"));
        assert!(out
            .html
            .contains("Math.abs(window.scrollY - anchor.scrollY) >= 1"));
        assert!(out
            .html
            .contains("scheduleBlockIntrinsicSizePriming([block]);"));
        assert!(out.html.contains("touchedRoots.push(c);"));
        assert!(!out
            .html
            .contains("seedBlockIntrinsicSize(block, snapshotBlockIntrinsicSize(block))"));
        let batch_start = out
            .html
            .find("function seedCurrentBlockIntrinsicSizes(blocks)")
            .unwrap();
        let batch_end = out.html[batch_start..]
            .find("// Force a targeted set of lazy blocks visible")
            .map(|offset| batch_start + offset)
            .unwrap();
        let batch_fn = &out.html[batch_start..batch_end];
        assert!(
            batch_fn
                .find("snapshotBlockIntrinsicSizes(blocks)")
                .unwrap()
                < batch_fn
                    .find("seedBlockIntrinsicSizeSnapshots(snapshots)")
                    .unwrap(),
            "intrinsic-size geometry reads must finish before seed writes"
        );
        let prime_start = out
            .html
            .find("function primeTopLevelBlockIntrinsicSizes(blocks)")
            .unwrap();
        let prime_end = out.html[prime_start..]
            .find("// SVG image loads")
            .map(|offset| prime_start + offset)
            .unwrap();
        let prime_fn = &out.html[prime_start..prime_end];
        let prime_snapshot = prime_fn
            .find("seedCurrentBlockIntrinsicSizes(targets)")
            .unwrap();
        assert!(prime_fn.find("void page.offsetHeight").unwrap() < prime_snapshot);
        assert!(
            prime_snapshot
                < prime_fn
                    .find("entry.block.style.contentVisibility = entry.contentVisibility")
                    .unwrap(),
            "theorem/TikZ geometry snapshots must precede containment restoration"
        );
        assert!(out.html.contains("text-align: justify;"));
        assert!(out.html.contains("hyphens: auto;"));
        assert!(out
            .html
            .contains(".text-alignment.align-center { text-align: center; hyphens: none; }"));
        assert!(out
            .html
            .contains(".text-alignment.align-flush-left { text-align: left; hyphens: none; }"));
        assert!(out
            .html
            .contains(".text-alignment.align-flush-right { text-align: right; hyphens: none; }"));
        assert!(out.html.contains("window.find"));
        assert!(out.html.contains("TEX_SYMBOL_CODEPOINTS"));
        assert!(out.html.contains("theta: [0x03B8]"));
        assert!(out.html.contains("runMathSearch"));
        assert!(out.html.contains("clearSearchSession"));
        assert!(out.html.contains("searchPanelIsOpen"));
        assert!(out.html.contains("math-search-glyph-active"));
        // Editor search must retain Vim's match semantics instead of reducing
        // `\<f\>` to a browser substring search for every `f`.
        assert!(out.html.contains("function editorBoundaryMatches"));
        assert!(out.html.contains("spec.wholeStart"));
        assert!(out.html.contains("spec.wholeEnd"));
        assert!(out.html.contains("spec.caseSensitive ? 'gu' : 'giu'"));
        assert!(out.html.contains("msg.whole_start === true"));
        assert!(out.html.contains("msg.whole_end === true"));
        assert!(out.html.contains("msg.case_sensitive === true"));
        // Chips live in a PAGE-LEVEL layer (built by layoutRefkeys) — chips
        // rendered inside the blocks get clipped by paint containment.
        // Guard the layer plumbing and the absence of in-block placement.
        assert!(out.html.contains(r#".refkey-layer"#));
        assert!(out.html.contains("layoutRefkeys"));
        // Native WebKit may not activate content-visibility blocks until the
        // scroll itself. Both lightweight overlays cache block-local geometry
        // up front without enrolling raw equations in eager MathJax work.
        assert!(out.html.contains("function ensureOverlayMetrics(page)"));
        assert!(out.html.contains("refkeyBlockMetrics = new WeakMap()"));
        assert!(out.html.contains("lineNumberBlockMetrics = new WeakMap()"));
        assert!(out.html.contains("blk.__mpOverlayPrelayoutToken"));
        // A forced `content-visibility:visible` pre-layout must retain the
        // implicit containment supplied by `auto`; otherwise heading and
        // display-math margins collapse through their blocks and both overlay
        // caches record positions above the visible content.
        assert!(out
            .html
            .contains("blk.style.contain = 'layout style paint'"));
        assert!(out.html.contains("entry.blk.style.contain = entry.contain"));
        assert!(out.html.contains("'contain-intrinsic-size',"));
        assert!(!out.html.contains("refkeyEstimateObserver"));
        assert_eq!(
            out.html
                .matches("font-size: calc(var(--body-font-size) * 0.611111)")
                .count(),
            2
        );
        assert!(out.html.contains("var chipHalfHeight = chipHeight / 2"));
        assert!(out.html.contains("y + c * chipStackStep"));
        assert!(out
            .html
            .contains(r#"body.refkey-visible .refkey-layer .refkey-chip"#));
        assert!(!out
            .html
            .contains(r#"right: calc(100% + var(--refkey-gap));"#));
        assert!(out.html.contains(r#".eq-refkey-list"#));
        // Interactive geometry toggles rebuild the layer pre-paint; the
        // render path keeps the trailing coalescing timer (DEVELOPMENT.md,
        // "The margin overlays").
        assert!(out.html.contains("scheduleRefkeys(0)"));
        // Pure zoom preserves the page's local geometry. It must not trigger
        // the expensive line-number text walk or keys-layer rebuild at all.
        let zoom_start = out
            .html
            .find("function setUserZoom(z, persist)")
            .expect("zoom setter present");
        let zoom_tail = &out.html[zoom_start..];
        let zoom_end = zoom_tail
            .find("function bumpUserZoom")
            .expect("zoom setter closes before bump helper");
        let zoom_fn = &zoom_tail[..zoom_end];
        assert!(!zoom_fn.contains("scheduleNavigationRefresh"));
        assert!(zoom_fn.contains("page.style.transform"));
        assert!(zoom_fn.contains("captureZoomAnchor"));
        assert!(zoom_fn.contains("zoomPreviewAnchor.localX"));
        assert!(zoom_fn.contains("zoomPreviewAnchor.localY"));
        assert!(zoom_fn.contains("setTimeout(commitUserZoom, NAV_RESIZE_IDLE_MS)"));
        assert!(!zoom_fn.contains("scheduleLineNumbers"));
        assert!(!zoom_fn.contains("scheduleRefkeys"));
        // Dynamic mode must scale one stable natural column too. Dividing its
        // width by userZoom reflows text and invalidates the line layer.
        let plan_start = out
            .html
            .find("function pageScalePlan(userZoom)")
            .expect("page scale planner present");
        let plan_tail = &out.html[plan_start..];
        let plan_end = plan_tail
            .find("function textRectAtPoint")
            .expect("page scale planner closes before text anchor helper");
        let plan_fn = &plan_tail[..plan_end];
        assert!(plan_fn.contains("Math.min(DYNAMIC_BASE_WIDTH, available)"));
        assert!(!plan_fn.contains("available / Math.max(userZoom"));
        assert!(out.html.contains("function restoreZoomAnchor"));
        assert!(out.html.contains("function scheduleZoomAnchorRestore"));
        assert!(out
            .html
            .contains("zoomAnchorRestoreRaf = requestAnimationFrame"));
        assert!(out
            .html
            .contains("zoomAnchorVerifyRaf = requestAnimationFrame"));
        assert!(out
            .html
            .contains("var viewportY = Math.min(vh, readingTop + 1)"));
        assert!(out.html.contains("function firstVisibleTextAnchor"));
        assert!(out.html.contains("document.caretPositionFromPoint"));
        assert!(out.html.contains("document.caretRangeFromPoint"));
        assert!(out.html.contains("textAnchor = firstVisibleTextAnchor"));
        assert!(out
            .html
            .contains("if (anchor.element && anchor.element.isConnected"));
        assert!(out.html.contains("scheduleZoomAnchorRestore(page, anchor)"));
        assert!(out
            .html
            .contains("window.scrollBy({ left: dx, top: dy, behavior: 'auto' })"));
        // Near EOF a live replacement must retain enough page height to keep
        // the top line fixed when the natural document loses one line.
        assert!(out.html.contains("function beginLivePatchViewportAnchor"));
        assert!(out.html.contains("page.style.overflowAnchor = 'none'"));
        assert!(out
            .html
            .contains("page.style.minHeight = livePatchFloorHeight + 'px'"));
        // Replacement blocks inherit the outgoing outer-box size before
        // content-visibility can collapse them to the generic 180px estimate.
        // Otherwise the preserved EOF scroll position can land in a blank
        // min-height floor until the next browser resize.
        assert!(out
            .html
            .contains("function snapshotBlockIntrinsicSize(block)"));
        assert!(out
            .html
            .contains("function seedBlockIntrinsicSize(block, size)"));
        assert!(out
            .html
            .contains("oldBlockIntrinsicSizes.get(oldReplacementBlocks[bi])"));
        assert!(out
            .html
            .contains("seedBlockIntrinsicSize(newFragBlocks[pp], pairedSize)"));
        assert!(out.html.contains("scrollY: window.scrollY"));
        assert_eq!(
            out.html
                .matches("settleLivePatchViewportAnchor(page, viewportAnchor)")
                .count(),
            2,
            "both range patches and full-body updates must settle the EOF anchor"
        );
        assert!(out.html.contains("previewUserZoom(next, true)"));
        assert!(out
            .html
            .contains("previewUserZoom(available / (base - cropDxNow()), true)"));
        // Browser-only zoom keeps the fast transform preview, then commits the
        // common CSS-zoom path without native-webview markers or sizing hooks.
        assert!(out
            .html
            .contains("var previewScale = targetScale / Math.max(committedPageScale"));
        assert!(out
            .html
            .contains("page.style.transform = 'scale(' + previewScale.toFixed(6) + ')'"));
        assert!(!out.html.contains("usesCompositePageZoom"));
        assert!(!out.html.contains("syncCompositePageHeight"));
        assert!(!out.html.contains("compositePageResizeObserver"));
        assert!(!out.html.contains("locus-composite-zoom"));
        assert!(!out.html.contains("locus-macos"));
        // Dynamic mode pins a 10mm margin by re-deriving --page-pad-x AT THE
        // ELEMENT. Overriding only the base is dead CSS: :root substitutes
        // var() at declaration time and descendants inherit the resolved
        // value (this shipped broken once — DEVELOPMENT.md has the story).
        let dyn_rule_start = out
            .html
            .find("body.page-mode-dynamic main#page {")
            .expect("dynamic page-mode rule present");
        let dyn_rule = &out.html[dyn_rule_start..];
        let dyn_rule = &dyn_rule[..dyn_rule.find('}').expect("rule closes")];
        assert!(dyn_rule.contains("--page-pad-x-base: 37.8px;"));
        assert!(dyn_rule.contains("--page-pad-x: var(--page-pad-x-base);"));
        // Crop wins over mode-level margin overrides by SOURCE ORDER (equal
        // specificity): its --page-pad-x rule must stay after the dynamic
        // rule, or cropping stops trimming the margin in dynamic mode.
        let crop_pad_override = out
            .html
            .find("--page-pad-x: var(--crop-pad)")
            .expect("crop pad override present");
        assert!(crop_pad_override > dyn_rule_start);
        // The crop toggle button (syncs with the `c` key via setPageCrop).
        assert!(out.html.contains(r#"id="crop-toggle""#));
        // The cursor flash box is drawn in a page-level layer — an outline on
        // the flashed element is clipped by block paint containment (a
        // paragraph's box rendered with no edges). Guard the layer plumbing
        // and the absence of the outline-based rule.
        assert!(out.html.contains(r#".flash-layer"#));
        assert!(out.html.contains("drawFlashBox"));
        // (leading \n: .source-space.source-active's `outline: 0` reset must
        // not trip this — only a revived bare .source-active box rule should)
        assert!(!out.html.contains("\n.source-active {"));
        assert!(out.html.contains("setStopButtonMode"));
        assert!(out.html.contains("startServer"));
        assert!(out.html.contains("stopServer"));
        assert!(out.html.contains("manualStopRequested"));
        assert!(out.html.contains("fetch('/stop'"));
        assert!(out.html.contains("fetch('/?start='"));
        assert!(out.html.contains(r#"id="page-shell""#));
        assert!(out.html.contains("--page-scale"));
        assert!(out.html.contains("updatePageScale"));
        // Tab favicon: the MathPreview icon inlined as a data URI in the head.
        assert!(out
            .html
            .contains(r#"<link rel="icon" type="image/png" href="data:image/png;base64,"#));
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
        // Inter-word whitespace is deliberately left as a bare text node for
        // DOM/patch efficiency. Inverse search must refine its coarse block
        // target from the click coordinates instead of jumping to the start
        // of the enclosing proof, theorem, or paragraph.
        assert!(out.html.contains("function textCharacterAtPoint"));
        assert!(out.html.contains("function sourceFlowScope"));
        assert!(out.html.contains(".item-body, .paper-abstract-body"));
        assert!(out.html.contains(".src-word.text-color"));
        assert!(out.html.contains("function sourceLeavesAroundNode"));
        assert!(out.html.contains("function nearestSourceLeafOnLine"));
        assert_eq!(
            out.html
                .matches("sourceElementFromTarget(e.target, e.clientX, e.clientY)")
                .count(),
            2,
            "both inverse-search paths must use coordinate-aware hit-testing"
        );
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
