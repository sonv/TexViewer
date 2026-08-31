//! CommonMark/GFM frontend with source spans and native math events.
//!
//! The parser never emits raw HTML into the viewer. HTML events are retained
//! as explicit inert AST nodes and escaped by the renderer.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

use anyhow::{anyhow, Result};
use github_slugger::slug as github_heading_slug;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};
use unicase::UniCase;

use crate::ast::{
    MarkdownAlignment, MarkdownReferenceStyle, MarkdownTheoremDialect, MarkdownTheoremMeta, Node,
    NodeKind, Pos, Span,
};
use crate::config::{
    ResolvedMarkdownConfig, MAX_MARKDOWN_BLOCK_SYNTAXES, MAX_MARKDOWN_BLOCK_SYNTAX_STARTS,
};

const MAX_MARKDOWN_CUSTOM_BLOCK_NESTING: usize = 32;
const MAX_MARKDOWN_CUSTOM_BLOCK_NAME_BYTES: usize = 32;
const MAX_MARKDOWN_CUSTOM_BLOCK_MARKER_BYTES: usize = 4_096;
const MAX_MARKDOWN_INLINE_HTML_NESTING: usize = 128;
const MAX_MARKDOWN_PANDOC_ATTRIBUTES: usize = 64;

/// Parse one Markdown source file into the shared source-spanned AST.
pub fn parse(source: &str, file: &Path) -> Result<Vec<Node>> {
    parse_with_config(source, file, &ResolvedMarkdownConfig::default())
}

/// Parse one Markdown source file using its resolved custom-block registry.
pub fn parse_with_config(
    source: &str,
    file: &Path,
    config: &ResolvedMarkdownConfig,
) -> Result<Vec<Node>> {
    let options = markdown_options();
    let custom_blocks = find_markdown_custom_blocks(source, options, config);
    let references = find_markdown_cross_references(source, options, &custom_blocks.marker_lines);
    let masked_source = mask_markdown_custom_block_markers(source, &custom_blocks.blocks);
    let masked_source = mask_markdown_cross_references(&masked_source, &references);
    let (parser_source, delimiter_overrides) = protect_tex_math_delimiters(&masked_source, options);
    let positions = LineIndex::new(source);
    let mut roots = Vec::new();
    let mut stack: Vec<Node> = Vec::new();

    for (event, range) in Parser::new_ext(&parser_source, options).into_offset_iter() {
        match event {
            Event::Start(tag) => stack.push(Node {
                kind: node_kind_for_tag(tag),
                span: positions.span(file, range.start, range.end),
                children: Vec::new(),
            }),
            Event::End(_) => {
                let mut node = stack
                    .pop()
                    .ok_or_else(|| anyhow!("unbalanced Markdown parser event stream"))?;
                node.span.end = positions.pos(range.end);
                append_node(&mut roots, &mut stack, node);
            }
            Event::Text(text) => {
                let text = text.into_string();
                let in_code_block = matches!(
                    stack.last().map(|node| &node.kind),
                    Some(NodeKind::MarkdownCodeBlock { .. })
                );
                let text_nodes = markdown_text_nodes(
                    &parser_source,
                    &text,
                    range.clone(),
                    &positions,
                    file,
                    in_code_block,
                );
                if in_code_block {
                    let node = stack
                        .last_mut()
                        .expect("code-block text must have a parent node");
                    if let NodeKind::MarkdownCodeBlock { code, .. } = &mut node.kind {
                        code.push_str(&text);
                    }
                    node.children.extend(text_nodes);
                } else {
                    for node in text_nodes {
                        append_node(&mut roots, &mut stack, node);
                    }
                }
            }
            Event::Code(code) => append_leaf(
                &mut roots,
                &mut stack,
                NodeKind::MarkdownInlineCode(code.into_string()),
                positions.span(file, range.start, range.end),
            ),
            Event::InlineMath(math) => {
                let body = delimiter_overrides
                    .get(&(range.start, range.end))
                    .filter(|m| !m.display)
                    .map(|m| m.body.clone())
                    .unwrap_or_else(|| math.into_string());
                append_leaf(
                    &mut roots,
                    &mut stack,
                    NodeKind::InlineMath(body),
                    positions.span(file, range.start, range.end),
                );
            }
            Event::DisplayMath(math) => {
                let body = delimiter_overrides
                    .get(&(range.start, range.end))
                    .filter(|m| m.display)
                    .map(|m| m.body.clone())
                    .unwrap_or_else(|| math.into_string());
                append_leaf(
                    &mut roots,
                    &mut stack,
                    NodeKind::DisplayMath {
                        body,
                        env: None,
                        label: None,
                        number: None,
                        row_numbers: Vec::new(),
                    },
                    positions.span(file, range.start, range.end),
                );
            }
            Event::Html(html) | Event::InlineHtml(html) => append_leaf(
                &mut roots,
                &mut stack,
                NodeKind::MarkdownRawHtml(html.into_string()),
                positions.span(file, range.start, range.end),
            ),
            Event::FootnoteReference(label) => append_leaf(
                &mut roots,
                &mut stack,
                NodeKind::MarkdownFootnoteReference {
                    label: label.into_string(),
                    target: String::new(),
                },
                positions.span(file, range.start, range.end),
            ),
            Event::SoftBreak => append_leaf(
                &mut roots,
                &mut stack,
                NodeKind::MarkdownSoftBreak,
                positions.span(file, range.start, range.end),
            ),
            Event::HardBreak => append_leaf(
                &mut roots,
                &mut stack,
                NodeKind::MarkdownHardBreak,
                positions.span(file, range.start, range.end),
            ),
            Event::Rule => append_leaf(
                &mut roots,
                &mut stack,
                NodeKind::MarkdownRule,
                positions.span(file, range.start, range.end),
            ),
            Event::TaskListMarker(checked) => append_leaf(
                &mut roots,
                &mut stack,
                NodeKind::MarkdownTaskMarker { checked },
                positions.span(file, range.start, range.end),
            ),
        }
    }

    if !stack.is_empty() {
        return Err(anyhow!("unbalanced Markdown parser containers"));
    }
    wrap_markdown_custom_blocks(&mut roots, &custom_blocks.blocks, &positions, file);
    integrate_markdown_cross_references(&mut roots, &references, source, &positions, file);
    promote_markdown_theorem_headings(&mut roots);
    assign_markdown_theorems_and_references(&mut roots, config);
    assign_markdown_heading_anchors(&mut roots);
    resolve_markdown_heading_links(&mut roots);
    assign_markdown_footnote_targets(&mut roots);
    Ok(roots)
}

#[derive(Debug)]
struct MarkdownCustomBlockMatch {
    name: String,
    title: Option<String>,
    label: Option<String>,
    card: bool,
    content_key: String,
    theorem: Option<MarkdownTheoremMeta>,
    opening: Range<usize>,
    closing: Range<usize>,
}

#[derive(Debug)]
struct MarkdownCustomBlockOpen {
    name: String,
    title: Option<String>,
    label: Option<String>,
    card: bool,
    theorem: Option<MarkdownTheoremMeta>,
    range: Range<usize>,
}

#[derive(Debug, Default)]
struct MarkdownCustomBlockScan {
    blocks: Vec<MarkdownCustomBlockMatch>,
    /// Every unprotected line that has the shape of an enabled block marker,
    /// including unknown and unclosed markers. Cross-reference syntax on
    /// these structural lines must remain metadata rather than visible prose.
    marker_lines: Vec<Range<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkdownCustomBlockFamily {
    ColonFence,
    Configured(usize),
}

#[derive(Debug)]
struct MarkdownCustomBlockFrame {
    /// More than one family is retained only for an ambiguous opening line.
    /// Such a frame is always opaque but still shields an enclosing block
    /// from either possible closer.
    families: Vec<MarkdownCustomBlockFamily>,
    opening: Option<MarkdownCustomBlockOpen>,
}

#[derive(Debug)]
struct MarkdownCustomBlockOverflow {
    /// The first family set seen beyond the renderable nesting limit.
    families: Vec<MarkdownCustomBlockFamily>,
    depth: usize,
    /// Once overflow nesting becomes ambiguous, extension parsing remains
    /// disabled for the rest of the document so tracked outer frames stay
    /// literal without retaining an unbounded overflow stack.
    poisoned: bool,
}

enum MarkdownCustomBlockMarker {
    Open {
        opening: Option<Box<MarkdownCustomBlockOpening>>,
    },
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkdownCustomBlockOpening {
    name: String,
    title: Option<String>,
    label: Option<String>,
    card: bool,
    theorem: Option<MarkdownTheoremMeta>,
}

#[derive(Clone, Copy)]
struct MarkdownTheoremSpec {
    name: &'static str,
    prefix: &'static str,
    bookdown: bool,
    bookdown_numbered: bool,
    quarto: bool,
}

const MARKDOWN_THEOREM_SPECS: &[MarkdownTheoremSpec] = &[
    MarkdownTheoremSpec {
        name: "theorem",
        prefix: "thm",
        bookdown: true,
        bookdown_numbered: true,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "lemma",
        prefix: "lem",
        bookdown: true,
        bookdown_numbered: true,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "corollary",
        prefix: "cor",
        bookdown: true,
        bookdown_numbered: true,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "proposition",
        prefix: "prp",
        bookdown: true,
        bookdown_numbered: true,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "conjecture",
        prefix: "cnj",
        bookdown: true,
        bookdown_numbered: true,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "definition",
        prefix: "def",
        bookdown: true,
        bookdown_numbered: true,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "example",
        prefix: "exm",
        bookdown: true,
        bookdown_numbered: true,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "exercise",
        prefix: "exr",
        bookdown: true,
        bookdown_numbered: true,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "hypothesis",
        prefix: "hyp",
        bookdown: true,
        bookdown_numbered: true,
        quarto: false,
    },
    MarkdownTheoremSpec {
        name: "solution",
        prefix: "sol",
        bookdown: true,
        bookdown_numbered: false,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "remark",
        prefix: "rem",
        bookdown: true,
        bookdown_numbered: false,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "algorithm",
        prefix: "alg",
        bookdown: false,
        bookdown_numbered: false,
        quarto: true,
    },
    MarkdownTheoremSpec {
        name: "proof",
        prefix: "",
        bookdown: true,
        bookdown_numbered: false,
        quarto: false,
    },
];

fn markdown_theorem_by_name(name: &str) -> Option<MarkdownTheoremSpec> {
    MARKDOWN_THEOREM_SPECS
        .iter()
        .copied()
        .find(|spec| spec.name == name)
}

fn markdown_quarto_theorem_id(identifier: &str) -> Option<(MarkdownTheoremSpec, &str)> {
    MARKDOWN_THEOREM_SPECS.iter().copied().find_map(|spec| {
        if !spec.quarto {
            return None;
        }
        let suffix = identifier.strip_prefix(spec.prefix)?.strip_prefix('-')?;
        valid_quarto_theorem_identifier(suffix).then_some((spec, suffix))
    })
}

fn markdown_theorem_meta(
    spec: MarkdownTheoremSpec,
    dialect: MarkdownTheoremDialect,
    identifier: Option<String>,
) -> MarkdownTheoremMeta {
    let numbered = match dialect {
        MarkdownTheoremDialect::Bookdown => spec.bookdown_numbered,
        MarkdownTheoremDialect::Quarto => true,
    };
    MarkdownTheoremMeta {
        dialect,
        prefix: spec.prefix.to_string(),
        identifier: numbered.then_some(identifier).flatten(),
        anchor: None,
        title_span: None,
        numbered,
        number: None,
    }
}

/// Mark source regions where block fences and semantic references are data,
/// not Markdown prose. The leading YAML range is detected explicitly because
/// the current CommonMark/GFM rendering profile intentionally does not enable
/// pulldown-cmark's metadata extension.
fn protect_markdown_literal_ranges(source: &str, options: Options, protected: &mut [bool]) {
    let (math_aware_source, _) = protect_tex_math_delimiters(source, options);
    let mut inline_html = Vec::<(String, usize)>::new();
    let mut inline_html_overflow_from = None;
    for (event, range) in Parser::new_ext(&math_aware_source, options).into_offset_iter() {
        if let Event::InlineHtml(html) = &event {
            match markdown_inline_html_tag(html) {
                Some((name, false, false)) => {
                    if inline_html.len() < MAX_MARKDOWN_INLINE_HTML_NESTING {
                        inline_html.push((name, range.start));
                    } else {
                        inline_html_overflow_from.get_or_insert(range.start);
                    }
                }
                Some((name, true, _)) => {
                    if let Some(index) = inline_html
                        .iter()
                        .rposition(|(open, _)| open.eq_ignore_ascii_case(&name))
                    {
                        let (_, start) = &inline_html[index];
                        protect_range(protected, *start..range.end);
                        inline_html.truncate(index);
                    }
                }
                _ => {}
            }
        }
        match event {
            Event::Start(
                Tag::CodeBlock(_)
                | Tag::HtmlBlock
                | Tag::MetadataBlock(_)
                | Tag::Link { .. }
                | Tag::Image { .. },
            )
            | Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::Html(_)
            | Event::InlineHtml(_) => protect_range(protected, range),
            _ => {}
        }
    }
    if let Some(range) = leading_markdown_yaml_range(source) {
        protect_range(protected, range);
    }
    if let Some(start) = inline_html_overflow_from {
        // Extremely deep or adversarial raw HTML degrades conservatively: its
        // remainder stays literal without growing or repeatedly scanning an
        // unbounded tag stack.
        protect_range(protected, start..source.len());
    }
}

/// Return `(name, closing, self_closing)` for ordinary inline HTML tags. Paired
/// tags delimit raw HTML islands whose contents must stay outside viewer
/// extensions even though CommonMark reports the text between them normally.
fn markdown_inline_html_tag(html: &str) -> Option<(String, bool, bool)> {
    let html = html.trim();
    let body = html.strip_prefix('<')?.strip_suffix('>')?.trim();
    if body.starts_with(['!', '?']) {
        return None;
    }
    let (closing, body) = match body.strip_prefix('/') {
        Some(body) => (true, body.trim_start()),
        None => (false, body),
    };
    let name_end = body
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | ':')))
        .unwrap_or(body.len());
    if name_end == 0 {
        return None;
    }
    let name = body[..name_end].to_ascii_lowercase();
    let void = matches!(
        name.as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    );
    Some((
        name,
        closing,
        !closing && (body.trim_end().ends_with('/') || void),
    ))
}

fn leading_markdown_yaml_range(source: &str) -> Option<Range<usize>> {
    let mut offset = 0;
    let mut lines = source.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }
    offset += first.len();
    for line in lines {
        offset += line.len();
        if matches!(line.trim_end_matches(['\r', '\n']), "---" | "...") {
            return Some(0..offset);
        }
    }
    None
}

#[derive(Debug)]
struct MarkdownLiteralBlockSyntax<'a> {
    family: MarkdownCustomBlockFamily,
    starts: Vec<MarkdownLiteralBlockTemplate<'a>>,
    end: &'a str,
}

#[derive(Debug, Clone)]
struct MarkdownLiteralBlockTemplate<'a> {
    source: &'a str,
    captures: Vec<MarkdownLiteralBlockCapture>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkdownLiteralBlockCapture {
    kind: MarkdownLiteralBlockCaptureKind,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkdownLiteralBlockCaptureKind {
    Name,
    Title,
    Label,
    Card,
}

impl MarkdownLiteralBlockCaptureKind {
    const ALL: [(Self, &'static str); 4] = [
        (Self::Name, "{name}"),
        (Self::Title, "{title}"),
        (Self::Label, "{label}"),
        (Self::Card, "{card}"),
    ];
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MarkdownLiteralBlockValues {
    title: Option<String>,
    label: Option<String>,
    card: bool,
}

const MAX_MARKDOWN_LITERAL_BLOCK_MATCH_WORK: usize = MAX_MARKDOWN_CUSTOM_BLOCK_MARKER_BYTES
    * MAX_MARKDOWN_BLOCK_SYNTAXES
    * MAX_MARKDOWN_BLOCK_SYNTAX_STARTS
    * 2;

#[derive(Debug)]
struct MarkdownLiteralBlockMatchBudget {
    remaining: usize,
}

impl MarkdownLiteralBlockMatchBudget {
    fn new(remaining: usize) -> Self {
        Self { remaining }
    }

    fn charge(&mut self, work: usize) -> Result<(), MarkdownLiteralBlockMatchExhausted> {
        if work > self.remaining {
            self.remaining = 0;
            Err(MarkdownLiteralBlockMatchExhausted)
        } else {
            self.remaining -= work;
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkdownLiteralBlockMatchExhausted;

/// Find configured block pairs before masking their boundary lines. The first
/// CommonMark pass supplies immunity ranges: marker-looking lines in code,
/// math, metadata, and raw HTML remain literal source for the real parse.
/// Only known pairs that actually close are returned for masking; unknown,
/// stray, malformed, and unclosed markers remain authored text.
fn find_markdown_custom_blocks(
    source: &str,
    options: Options,
    config: &ResolvedMarkdownConfig,
) -> MarkdownCustomBlockScan {
    find_markdown_custom_blocks_with_match_work_limit(
        source,
        options,
        config,
        MAX_MARKDOWN_LITERAL_BLOCK_MATCH_WORK,
    )
}

fn find_markdown_custom_blocks_with_match_work_limit(
    source: &str,
    options: Options,
    config: &ResolvedMarkdownConfig,
    match_work_limit: usize,
) -> MarkdownCustomBlockScan {
    let mut protected = vec![false; source.len()];
    protect_markdown_literal_ranges(source, options, &mut protected);
    let literal_syntaxes = markdown_literal_block_syntaxes(config);

    let mut open = Vec::<MarkdownCustomBlockFrame>::new();
    let mut overflow = None::<MarkdownCustomBlockOverflow>;
    let mut scan = MarkdownCustomBlockScan::default();
    let mut line_start = 0;
    for line in source.split_inclusive('\n') {
        let line_end = line_start + line.len();
        let range = line_start..line_end;
        let Some((indent, body)) = markdown_custom_block_line(line) else {
            line_start = line_end;
            continue;
        };
        let marker_byte = line_start + indent;
        if protected.get(marker_byte).copied().unwrap_or(false) {
            line_start = line_end;
            continue;
        }

        let colon_marker = config
            .colon_fences
            .then(|| markdown_custom_block_marker(line, config))
            .flatten();
        let colon_shaped =
            config.colon_fences && body.bytes().take_while(|byte| *byte == b':').count() >= 3;
        let mut closing_families = Vec::new();
        let mut opening_candidates = Vec::new();
        match colon_marker {
            Some(MarkdownCustomBlockMarker::Close) => {
                closing_families.push(MarkdownCustomBlockFamily::ColonFence);
            }
            Some(MarkdownCustomBlockMarker::Open { opening }) => {
                opening_candidates.push((MarkdownCustomBlockFamily::ColonFence, opening));
            }
            None => {}
        }

        if body.len() <= MAX_MARKDOWN_CUSTOM_BLOCK_MARKER_BYTES {
            for syntax in &literal_syntaxes {
                if body == syntax.end {
                    closing_families.push(syntax.family);
                }
            }

            let mut literal_opening_candidates = Vec::new();
            let mut match_budget = MarkdownLiteralBlockMatchBudget::new(match_work_limit);
            let mut exhausted = false;
            for syntax in &literal_syntaxes {
                match markdown_literal_block_opening(body, syntax, config, &mut match_budget) {
                    Ok(Some(opening)) => {
                        literal_opening_candidates.push((syntax.family, opening.map(Box::new)));
                    }
                    Ok(None) => {}
                    Err(MarkdownLiteralBlockMatchExhausted) => {
                        exhausted = true;
                        break;
                    }
                }
            }
            if exhausted {
                // A line whose bounded match cannot be decided stays opaque.
                // Treat it as a possible opener for every configured family
                // so a following literal closer cannot steal an outer block.
                literal_opening_candidates = literal_syntaxes
                    .iter()
                    .map(|syntax| (syntax.family, None))
                    .collect();
            }
            opening_candidates.extend(literal_opening_candidates);
        }
        if colon_shaped || !closing_families.is_empty() || !opening_candidates.is_empty() {
            scan.marker_lines.push(range.clone());
        }

        if let Some(state) = overflow.as_mut() {
            if state.poisoned {
                line_start = line_end;
                continue;
            }
            if !closing_families.is_empty() {
                if state
                    .families
                    .iter()
                    .any(|family| closing_families.contains(family))
                {
                    state.depth -= 1;
                    if state.depth == 0 {
                        overflow = None;
                    }
                } else {
                    state.poisoned = true;
                }
                line_start = line_end;
                continue;
            }
            if !opening_candidates.is_empty() {
                let families: Vec<_> = opening_candidates
                    .into_iter()
                    .map(|(family, _)| family)
                    .collect();
                if state.families == families {
                    state.depth = state.depth.saturating_add(1);
                } else {
                    state.poisoned = true;
                }
                line_start = line_end;
                continue;
            }
            line_start = line_end;
            continue;
        }

        // Close only the current top family. If this line is also a valid
        // opening for another family, an unmatched closer must not suppress
        // that opening; searching down the stack would create overlapping
        // source spans and let a stray closer steal an enclosing block.
        let mut closed = false;
        if !closing_families.is_empty()
            && open.last().is_some_and(|frame| {
                frame
                    .families
                    .iter()
                    .any(|family| closing_families.contains(family))
            })
        {
            let frame = open.pop().expect("matching custom-block frame exists");
            if let Some(opening) = frame.opening {
                let content = source
                    .get(opening.range.end..range.start)
                    .unwrap_or_default();
                scan.blocks.push(MarkdownCustomBlockMatch {
                    name: opening.name,
                    title: opening.title,
                    label: opening.label,
                    card: opening.card,
                    content_key: markdown_custom_block_content_key(content),
                    theorem: opening.theorem,
                    opening: opening.range,
                    closing: range.clone(),
                });
            }
            closed = true;
        }
        if closed {
            line_start = line_end;
            continue;
        }

        if !opening_candidates.is_empty() {
            if open.len() >= MAX_MARKDOWN_CUSTOM_BLOCK_NESTING {
                let families: Vec<_> = opening_candidates
                    .into_iter()
                    .map(|(family, _)| family)
                    .collect();
                overflow = Some(MarkdownCustomBlockOverflow {
                    families,
                    depth: 1,
                    poisoned: false,
                });
            } else {
                let opening = (opening_candidates.len() == 1)
                    .then(|| opening_candidates[0].1.clone())
                    .flatten()
                    .map(|opening| {
                        let opening = *opening;
                        MarkdownCustomBlockOpen {
                            name: opening.name,
                            title: opening.title,
                            label: opening.label,
                            card: opening.card,
                            theorem: opening.theorem,
                            range,
                        }
                    });
                open.push(MarkdownCustomBlockFrame {
                    families: opening_candidates
                        .into_iter()
                        .map(|(family, _)| family)
                        .collect(),
                    opening,
                });
            }
        }
        line_start = line_end;
    }
    scan
}

fn markdown_custom_block_line(line: &str) -> Option<(usize, &str)> {
    let indent = line
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == b' ')
        .count();
    if indent > 3 {
        return None;
    }
    let body = line
        .get(indent..)?
        .trim_end_matches(['\r', '\n'])
        .trim_end_matches([' ', '\t']);
    Some((indent, body))
}

fn markdown_literal_block_syntaxes(
    config: &ResolvedMarkdownConfig,
) -> Vec<MarkdownLiteralBlockSyntax<'_>> {
    config
        .block_syntaxes
        .values()
        .enumerate()
        .filter_map(|(index, syntax)| {
            let starts: Vec<_> = syntax
                .start
                .iter()
                .filter_map(|template| markdown_literal_block_template(template))
                .collect();
            (!starts.is_empty() && !syntax.end.is_empty() && !syntax.end.contains(['\r', '\n']))
                .then_some(MarkdownLiteralBlockSyntax {
                    family: MarkdownCustomBlockFamily::Configured(index),
                    starts,
                    end: &syntax.end,
                })
        })
        .collect()
}

fn markdown_literal_block_template(template: &str) -> Option<MarkdownLiteralBlockTemplate<'_>> {
    if template.is_empty()
        || template.len() > MAX_MARKDOWN_CUSTOM_BLOCK_MARKER_BYTES
        || template.contains(['\r', '\n'])
    {
        return None;
    }
    let mut captures = Vec::new();
    for (kind, placeholder) in MarkdownLiteralBlockCaptureKind::ALL {
        let mut positions = template.match_indices(placeholder);
        let Some((start, _)) = positions.next() else {
            if kind == MarkdownLiteralBlockCaptureKind::Name {
                return None;
            }
            continue;
        };
        if positions.next().is_some() {
            return None;
        }
        captures.push(MarkdownLiteralBlockCapture {
            kind,
            start,
            end: start + placeholder.len(),
        });
    }
    captures.sort_unstable_by_key(|capture| capture.start);
    if captures.first().map(|capture| capture.kind) != Some(MarkdownLiteralBlockCaptureKind::Name)
        || captures.windows(2).any(|pair| pair[0].end >= pair[1].start)
    {
        return None;
    }
    Some(MarkdownLiteralBlockTemplate {
        source: template,
        captures,
    })
}

fn markdown_literal_block_opening(
    line: &str,
    syntax: &MarkdownLiteralBlockSyntax<'_>,
    config: &ResolvedMarkdownConfig,
    budget: &mut MarkdownLiteralBlockMatchBudget,
) -> Result<Option<Option<MarkdownCustomBlockOpening>>, MarkdownLiteralBlockMatchExhausted> {
    let mut found = false;
    let mut selected = None::<MarkdownCustomBlockOpening>;
    let mut ambiguous = false;
    for template in &syntax.starts {
        let Some(opening) =
            markdown_literal_block_template_opening(line, template, config, budget)?
        else {
            continue;
        };
        found = true;
        let Some(opening) = opening else {
            // A broad title-less variant can have the same fixed prefix and
            // suffix as a more specific titled form. An invalid capture from
            // that broad variant must not make the valid titled match
            // ambiguous; if no variant is valid, the frame remains opaque.
            continue;
        };
        match &selected {
            Some(existing) if *existing != opening => ambiguous = true,
            Some(_) => {}
            None => selected = Some(opening),
        }
    }
    Ok(found.then_some(if ambiguous { None } else { selected }))
}

fn markdown_literal_block_template_opening(
    line: &str,
    template: &MarkdownLiteralBlockTemplate<'_>,
    config: &ResolvedMarkdownConfig,
    budget: &mut MarkdownLiteralBlockMatchBudget,
) -> Result<Option<Option<MarkdownCustomBlockOpening>>, MarkdownLiteralBlockMatchExhausted> {
    let Some(first) = template.captures.first() else {
        return Ok(None);
    };
    let prefix = &template.source[..first.start];
    budget.charge(prefix.len().min(line.len()).saturating_add(1))?;
    let Some(rest) = line.strip_prefix(prefix) else {
        return Ok(None);
    };
    if !markdown_literal_block_skeleton_matches(rest, template, 0, budget)? {
        return Ok(None);
    }

    let after_name = markdown_literal_block_literal_after(template, 0);
    let mut selected = None::<(usize, MarkdownCustomBlockOpening)>;
    let mut ambiguous = false;
    for name in config.blocks.keys() {
        budget.charge(name.len().min(rest.len()).saturating_add(1))?;
        if name.len() > MAX_MARKDOWN_CUSTOM_BLOCK_NAME_BYTES || !rest.starts_with(name) {
            continue;
        }
        let name_tail = &rest[name.len()..];
        budget.charge(after_name.len().min(name_tail.len()).saturating_add(1))?;
        let Some(tail) = name_tail.strip_prefix(after_name) else {
            continue;
        };
        let mut values = Vec::new();
        markdown_literal_block_values(
            tail,
            template,
            1,
            MarkdownLiteralBlockValues::default(),
            &mut values,
            budget,
        )?;
        for value in values {
            let opening = MarkdownCustomBlockOpening {
                name: name.clone(),
                title: value.title,
                label: value.label,
                card: value.card,
                // Literal delimiter templates only choose presentation. They
                // do not imply Bookdown/Quarto numbering semantics.
                theorem: None,
            };
            match &selected {
                Some((length, _)) if *length > name.len() => {}
                Some((length, current)) if *length == name.len() && *current != opening => {
                    ambiguous = true;
                }
                Some((length, _)) if *length < name.len() => {
                    selected = Some((name.len(), opening));
                    ambiguous = false;
                }
                Some(_) => {}
                None => selected = Some((name.len(), opening)),
            }
        }
    }
    Ok(Some(if ambiguous {
        None
    } else {
        selected.map(|(_, opening)| opening)
    }))
}

fn markdown_literal_block_skeleton_matches(
    input: &str,
    template: &MarkdownLiteralBlockTemplate<'_>,
    capture_index: usize,
    budget: &mut MarkdownLiteralBlockMatchBudget,
) -> Result<bool, MarkdownLiteralBlockMatchExhausted> {
    let literal = markdown_literal_block_literal_after(template, capture_index);
    if capture_index + 1 == template.captures.len() {
        budget.charge(literal.len().min(input.len()).saturating_add(1))?;
        return Ok(input.ends_with(literal));
    }
    budget.charge(input.len().saturating_add(literal.len()).saturating_add(1))?;
    for (at, _) in input.match_indices(literal) {
        if markdown_literal_block_skeleton_matches(
            &input[at + literal.len()..],
            template,
            capture_index + 1,
            budget,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn markdown_literal_block_literal_after<'a>(
    template: &'a MarkdownLiteralBlockTemplate<'a>,
    capture_index: usize,
) -> &'a str {
    let capture = template.captures[capture_index];
    let end = template
        .captures
        .get(capture_index + 1)
        .map_or(template.source.len(), |next| next.start);
    &template.source[capture.end..end]
}

fn markdown_literal_block_values(
    input: &str,
    template: &MarkdownLiteralBlockTemplate<'_>,
    capture_index: usize,
    values: MarkdownLiteralBlockValues,
    matches: &mut Vec<MarkdownLiteralBlockValues>,
    budget: &mut MarkdownLiteralBlockMatchBudget,
) -> Result<(), MarkdownLiteralBlockMatchExhausted> {
    if matches.len() > 1 {
        return Ok(());
    }
    if capture_index == template.captures.len() {
        budget.charge(1)?;
        if input.is_empty() && !matches.contains(&values) {
            matches.push(values);
        }
        return Ok(());
    }

    let literal = markdown_literal_block_literal_after(template, capture_index);
    if capture_index + 1 == template.captures.len() {
        budget.charge(literal.len().min(input.len()).saturating_add(1))?;
        let Some(raw) = input.strip_suffix(literal) else {
            return Ok(());
        };
        budget.charge(raw.len().saturating_add(1))?;
        let Some(values) =
            markdown_literal_block_capture_value(raw, template, capture_index, values)
        else {
            return Ok(());
        };
        return markdown_literal_block_values(
            "",
            template,
            capture_index + 1,
            values,
            matches,
            budget,
        );
    }

    budget.charge(input.len().saturating_add(literal.len()).saturating_add(1))?;
    for (at, _) in input.match_indices(literal) {
        budget.charge(at.saturating_add(1))?;
        let Some(values) = markdown_literal_block_capture_value(
            &input[..at],
            template,
            capture_index,
            values.clone(),
        ) else {
            continue;
        };
        markdown_literal_block_values(
            &input[at + literal.len()..],
            template,
            capture_index + 1,
            values,
            matches,
            budget,
        )?;
    }
    Ok(())
}

fn markdown_literal_block_capture_value(
    raw: &str,
    template: &MarkdownLiteralBlockTemplate<'_>,
    capture_index: usize,
    mut values: MarkdownLiteralBlockValues,
) -> Option<MarkdownLiteralBlockValues> {
    let capture = template.captures[capture_index];
    match capture.kind {
        MarkdownLiteralBlockCaptureKind::Name => return None,
        MarkdownLiteralBlockCaptureKind::Title => {
            let title = markdown_literal_block_string(raw, template, capture_index)?;
            values.title = if title.is_empty() {
                None
            } else {
                clean_markdown_block_title(&title)
            };
            if !title.is_empty() && values.title.is_none() {
                return None;
            }
        }
        MarkdownLiteralBlockCaptureKind::Label => {
            let label = markdown_literal_block_string(raw, template, capture_index)?;
            if !valid_markdown_literal_label(&label) {
                return None;
            }
            values.label = Some(label);
        }
        MarkdownLiteralBlockCaptureKind::Card => {
            values.card = match raw {
                "true" => true,
                "false" => false,
                _ => return None,
            };
        }
    }
    Some(values)
}

fn markdown_literal_block_string(
    raw: &str,
    template: &MarkdownLiteralBlockTemplate<'_>,
    capture_index: usize,
) -> Option<String> {
    let capture = template.captures[capture_index];
    let before_start = capture_index
        .checked_sub(1)
        .map_or(0, |previous| template.captures[previous].end);
    let before = &template.source[before_start..capture.start];
    let after = markdown_literal_block_literal_after(template, capture_index);
    let quote = before.chars().next_back().and_then(|before| {
        let after = after.chars().next()?;
        (before == after && matches!(before, '\'' | '"')).then_some(before)
    });
    if let Some(quote) = quote {
        let mut decoded = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(ch) = chars.next() {
            if ch == quote {
                return None;
            }
            if ch == '\\' {
                let escaped = chars.next()?;
                if escaped == quote || escaped == '\\' {
                    decoded.push(escaped);
                } else {
                    decoded.push(ch);
                    decoded.push(escaped);
                }
            } else {
                decoded.push(ch);
            }
        }
        Some(decoded)
    } else {
        Some(raw.to_string())
    }
}

fn markdown_custom_block_content_key(content: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in content.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn markdown_custom_block_marker(
    line: &str,
    config: &ResolvedMarkdownConfig,
) -> Option<MarkdownCustomBlockMarker> {
    let indent = line
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == b' ')
        .count();
    if indent > 3 {
        return None;
    }
    let body = line.get(indent..)?.trim_end_matches(['\r', '\n']);
    let fence_len = body.bytes().take_while(|byte| *byte == b':').count();
    if fence_len < 3 {
        return None;
    }
    let body = body.get(fence_len..)?;
    if body.trim_matches([' ', '\t']).is_empty() {
        return Some(MarkdownCustomBlockMarker::Close);
    }

    let opening = if body.starts_with(char::is_whitespace) {
        let body = body.trim_matches([' ', '\t']);
        if body.starts_with('{') {
            let (attributes, consumed) = parse_markdown_pandoc_attributes(body)?;
            if !body[consumed..]
                .chars()
                .all(|ch| ch == ':' || ch == ' ' || ch == '\t')
            {
                return None;
            }
            markdown_pandoc_block_opening(attributes, config)
        } else {
            let mut words = body.split_ascii_whitespace();
            let name = words.next()?;
            if words.any(|word| !word.chars().all(|ch| ch == ':')) {
                return None;
            }
            markdown_named_block_opening(name, None, None, MarkdownTheoremDialect::Bookdown, config)
        }
    } else {
        let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
        let name = &body[..name_end];
        let title = clean_markdown_block_title(body[name_end..].trim_matches([' ', '\t']));
        config
            .blocks
            .contains_key(name)
            .then(|| MarkdownCustomBlockOpening {
                name: name.to_string(),
                title,
                label: None,
                card: false,
                // The compact MathPreview syntax predates semantic theorem
                // blocks. Keep it purely presentational for compatibility.
                theorem: None,
            })
    };

    Some(MarkdownCustomBlockMarker::Open {
        opening: opening.map(Box::new),
    })
}

#[derive(Default)]
struct MarkdownPandocAttributes {
    classes: Vec<String>,
    identifier: Option<String>,
    name: Option<String>,
    title: Option<String>,
}

fn parse_markdown_pandoc_attributes(input: &str) -> Option<(MarkdownPandocAttributes, usize)> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut attributes = MarkdownPandocAttributes::default();
    let mut attribute_count = 0usize;
    let mut i = 1;
    loop {
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        match bytes.get(i).copied()? {
            b'}' => return Some((attributes, i + 1)),
            prefix @ (b'.' | b'#') => {
                attribute_count += 1;
                if attribute_count > MAX_MARKDOWN_PANDOC_ATTRIBUTES {
                    return None;
                }
                let start = i + 1;
                i = start;
                while bytes
                    .get(i)
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'}')
                {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                let value = input.get(start..i)?.to_string();
                if value.len() > 128 {
                    return None;
                }
                if prefix == b'.' {
                    attributes.classes.push(value);
                } else if attributes.identifier.replace(value).is_some() {
                    return None;
                }
            }
            _ => {
                attribute_count += 1;
                if attribute_count > MAX_MARKDOWN_PANDOC_ATTRIBUTES {
                    return None;
                }
                let key_start = i;
                while bytes.get(i).is_some_and(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-')
                }) {
                    i += 1;
                }
                if i == key_start {
                    return None;
                }
                let key = input.get(key_start..i)?;
                while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                    i += 1;
                }
                if bytes.get(i) != Some(&b'=') {
                    return None;
                }
                i += 1;
                while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                    i += 1;
                }
                let (value, end) = parse_markdown_attribute_value(input, i)?;
                if value.len() > 2048 {
                    return None;
                }
                i = end;
                match key {
                    "name" if attributes.name.is_none() => attributes.name = Some(value),
                    "title" if attributes.title.is_none() => attributes.title = Some(value),
                    "name" | "title" => return None,
                    _ => {}
                }
            }
        }
    }
}

fn parse_markdown_attribute_value(input: &str, start: usize) -> Option<(String, usize)> {
    let bytes = input.as_bytes();
    let quote = bytes
        .get(start)
        .copied()
        .filter(|byte| matches!(byte, b'\'' | b'"'));
    if let Some(quote) = quote {
        let mut value = String::new();
        let mut i = start + 1;
        while i < bytes.len() {
            if bytes[i] == quote {
                return Some((value, i + 1));
            }
            let ch = input.get(i..)?.chars().next()?;
            if ch == '\\' {
                i += ch.len_utf8();
                let escaped = input.get(i..)?.chars().next()?;
                value.push(escaped);
                i += escaped.len_utf8();
            } else {
                value.push(ch);
                i += ch.len_utf8();
            }
        }
        None
    } else {
        let mut end = start;
        while bytes
            .get(end)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'}')
        {
            end += 1;
        }
        (end > start).then(|| (input[start..end].to_string(), end))
    }
}

fn markdown_pandoc_block_opening(
    attributes: MarkdownPandocAttributes,
    config: &ResolvedMarkdownConfig,
) -> Option<MarkdownCustomBlockOpening> {
    let title = attributes
        .name
        .as_deref()
        .and_then(clean_markdown_block_title)
        .or_else(|| {
            attributes
                .title
                .as_deref()
                .and_then(clean_markdown_block_title)
        });
    let identifier = match attributes.identifier.as_deref() {
        Some(identifier) if valid_markdown_theorem_identifier(identifier) => Some(identifier),
        Some(_) => return None,
        None => None,
    };

    if let Some((spec, suffix)) = identifier.and_then(markdown_quarto_theorem_id) {
        return config
            .blocks
            .contains_key(spec.name)
            .then(|| MarkdownCustomBlockOpening {
                name: spec.name.to_string(),
                title,
                label: None,
                card: false,
                theorem: Some(markdown_theorem_meta(
                    spec,
                    MarkdownTheoremDialect::Quarto,
                    Some(suffix.to_string()),
                )),
            });
    }

    let mut semantic_names = attributes.classes.iter().filter(|class| {
        config.blocks.contains_key(class.as_str())
            && markdown_theorem_by_name(class).is_some_and(|spec| spec.bookdown)
    });
    let semantic_name = semantic_names.next();
    if semantic_names.next().is_some() {
        return None;
    }
    let name = semantic_name.or_else(|| {
        attributes
            .classes
            .iter()
            .find(|class| config.blocks.contains_key(class.as_str()))
    })?;
    markdown_named_block_opening(
        name,
        title,
        identifier.map(str::to_string),
        MarkdownTheoremDialect::Bookdown,
        config,
    )
}

fn markdown_named_block_opening(
    name: &str,
    title: Option<String>,
    identifier: Option<String>,
    dialect: MarkdownTheoremDialect,
    config: &ResolvedMarkdownConfig,
) -> Option<MarkdownCustomBlockOpening> {
    config.blocks.contains_key(name).then(|| {
        let theorem = markdown_theorem_by_name(name)
            .filter(|spec| dialect != MarkdownTheoremDialect::Bookdown || spec.bookdown)
            .map(|spec| markdown_theorem_meta(spec, dialect, identifier));
        MarkdownCustomBlockOpening {
            name: name.to_string(),
            title,
            label: None,
            card: false,
            theorem,
        }
    })
}

fn clean_markdown_block_title(title: &str) -> Option<String> {
    (!title.is_empty() && title.chars().count() <= 512 && !title.chars().any(char::is_control))
        .then(|| title.to_string())
}

fn valid_markdown_literal_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-'))
}

fn valid_markdown_theorem_identifier(identifier: &str) -> bool {
    let bytes = identifier.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..].iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b':' | b'_' | b'.' | b'/' | b'-')
        })
}

fn valid_quarto_theorem_identifier(identifier: &str) -> bool {
    let bytes = identifier.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-'))
}

fn mask_markdown_custom_block_markers(source: &str, blocks: &[MarkdownCustomBlockMatch]) -> String {
    let mut masked = source.as_bytes().to_vec();
    for block in blocks {
        for range in [&block.opening, &block.closing] {
            for byte in &mut masked[range.clone()] {
                if !matches!(*byte, b'\r' | b'\n') {
                    *byte = b' ';
                }
            }
        }
    }
    String::from_utf8(masked).expect("ASCII masking preserves UTF-8")
}

fn wrap_markdown_custom_blocks(
    nodes: &mut Vec<Node>,
    blocks: &[MarkdownCustomBlockMatch],
    positions: &LineIndex<'_>,
    file: &Path,
) {
    let trees = markdown_custom_block_trees(blocks);
    *nodes =
        integrate_markdown_custom_blocks(std::mem::take(nodes), &trees, blocks, positions, file);
}

#[derive(Debug)]
struct MarkdownCustomBlockTree {
    block: usize,
    children: Vec<MarkdownCustomBlockTree>,
}

fn markdown_custom_block_trees(
    blocks: &[MarkdownCustomBlockMatch],
) -> Vec<MarkdownCustomBlockTree> {
    fn level(
        order: &[usize],
        cursor: &mut usize,
        before: usize,
        blocks: &[MarkdownCustomBlockMatch],
    ) -> Vec<MarkdownCustomBlockTree> {
        let mut trees = Vec::new();
        while let Some(&index) = order.get(*cursor) {
            let block = &blocks[index];
            if block.opening.start >= before {
                break;
            }
            *cursor += 1;
            trees.push(MarkdownCustomBlockTree {
                block: index,
                children: level(order, cursor, block.closing.start, blocks),
            });
        }
        trees
    }

    let mut order: Vec<_> = (0..blocks.len()).collect();
    order.sort_unstable_by_key(|index| {
        (
            blocks[*index].opening.start,
            std::cmp::Reverse(blocks[*index].closing.end),
        )
    });
    level(&order, &mut 0, usize::MAX, blocks)
}

fn integrate_markdown_custom_blocks(
    nodes: Vec<Node>,
    trees: &[MarkdownCustomBlockTree],
    blocks: &[MarkdownCustomBlockMatch],
    positions: &LineIndex<'_>,
    file: &Path,
) -> Vec<Node> {
    let mut nodes: std::collections::VecDeque<_> = nodes.into();
    let mut output = Vec::with_capacity(nodes.len() + trees.len());
    let mut tree_index = 0;
    while let Some(tree) = trees.get(tree_index) {
        let block = &blocks[tree.block];
        let whole_start = block.opening.start;
        let whole_end = block.closing.end;

        if let Some(node) = nodes.front() {
            let node_start = node.span.start.byte as usize;
            let node_end = node.span.end.byte as usize;
            if node_end <= whole_start {
                output.push(nodes.pop_front().expect("front node exists"));
                continue;
            }
            if node_start <= whole_start
                && whole_end <= node_end
                && (node_start < whole_start || whole_end < node_end)
            {
                let mut end = tree_index + 1;
                while let Some(next) = trees.get(end) {
                    let next = &blocks[next.block];
                    if node_start <= next.opening.start && next.closing.end <= node_end {
                        end += 1;
                    } else {
                        break;
                    }
                }
                let mut node = nodes.pop_front().expect("front node exists");
                node.children = integrate_markdown_custom_blocks(
                    std::mem::take(&mut node.children),
                    &trees[tree_index..end],
                    blocks,
                    positions,
                    file,
                );
                output.push(node);
                tree_index = end;
                continue;
            }
            if node_start < whole_start {
                // A normal Markdown container can partially overlap a custom
                // fence only when the source crosses container boundaries in
                // an unusual way. Preserve that node rather than dropping or
                // moving any of its content; the custom wrapper remains at the
                // closest source-ordered level possible.
                output.push(nodes.pop_front().expect("front node exists"));
                continue;
            }
        }

        let content_start = block.opening.end;
        let content_end = block.closing.start;
        let mut children = Vec::new();
        while let Some(node) = nodes.front() {
            let start = node.span.start.byte as usize;
            let end = node.span.end.byte as usize;
            if content_start <= start && end <= content_end {
                children.push(nodes.pop_front().expect("front node exists"));
            } else {
                break;
            }
        }
        let children =
            integrate_markdown_custom_blocks(children, &tree.children, blocks, positions, file);
        output.push(Node {
            kind: NodeKind::MarkdownCustomBlock {
                name: block.name.clone(),
                title: block.title.clone(),
                label: block.label.clone(),
                anchor: None,
                card: block.card,
                content_key: block.content_key.clone(),
                theorem: block.theorem.clone(),
            },
            span: positions.span(file, whole_start, whole_end),
            children,
        });
        tree_index += 1;
    }
    output.extend(nodes);
    output
}

#[derive(Debug)]
struct MarkdownCrossReferenceMatch {
    prefix: String,
    identifier: String,
    style: MarkdownReferenceStyle,
    range: Range<usize>,
}

fn find_markdown_cross_references(
    source: &str,
    options: Options,
    marker_lines: &[Range<usize>],
) -> Vec<MarkdownCrossReferenceMatch> {
    let mut protected = vec![false; source.len()];
    protect_markdown_literal_ranges(source, options, &mut protected);
    for range in marker_lines {
        protect_range(&mut protected, range.clone());
    }
    let eligible = markdown_text_event_ranges(source, options);

    let bytes = source.as_bytes();
    let mut matches = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let starts_visible_escape = bytes[i] == b'\\'
            && eligible.get(i + 1).copied().unwrap_or(false)
            && !protected.get(i + 1).copied().unwrap_or(true);
        if protected[i] || (!eligible[i] && !starts_visible_escape) {
            i += 1;
            continue;
        }

        if bytes[i] == b'\\'
            && is_unescaped_backslash(bytes, i)
            && source[i..].starts_with("\\@ref(")
        {
            let body_start = i + "\\@ref(".len();
            let search_end = body_start.saturating_add(260).min(source.len());
            if let Some(offset) = bytes[body_start..search_end]
                .iter()
                .position(|byte| *byte == b')')
            {
                let end = body_start + offset;
                let body = &source[body_start..end];
                if let Some((prefix, identifier)) = body.split_once(':') {
                    let supported = MARKDOWN_THEOREM_SPECS
                        .iter()
                        .any(|spec| spec.bookdown_numbered && spec.prefix == prefix);
                    let range = i..end + 1;
                    if supported
                        && valid_markdown_theorem_identifier(identifier)
                        // CommonMark treats the leading backslash as escape
                        // syntax, so its byte is outside the emitted Text
                        // range even though the following `@ref(...)` is
                        // visible prose.
                        && markdown_reference_starts_in_prose(i + 1, &eligible, &protected)
                    {
                        matches.push(MarkdownCrossReferenceMatch {
                            prefix: prefix.to_string(),
                            identifier: identifier.to_string(),
                            style: MarkdownReferenceStyle::Bookdown,
                            range,
                        });
                        i = end + 1;
                        continue;
                    }
                }
            }
        }

        if bytes[i] == b'@'
            && (i == 0
                || (!bytes[i - 1].is_ascii_alphanumeric() && !matches!(bytes[i - 1], b'_' | b'\\')))
        {
            let rest = &source[i + 1..];
            if let Some(spec) = MARKDOWN_THEOREM_SPECS.iter().copied().find(|spec| {
                spec.quarto
                    && rest.starts_with(spec.prefix)
                    && rest[spec.prefix.len()..].starts_with('-')
            }) {
                let identifier_start = i + 1 + spec.prefix.len() + 1;
                let mut end = identifier_start;
                while bytes.get(end).is_some_and(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-')
                }) {
                    end += 1;
                }
                let range = i..end;
                if end > identifier_start
                    && valid_quarto_theorem_identifier(&source[identifier_start..end])
                    && markdown_reference_starts_in_prose(i, &eligible, &protected)
                {
                    matches.push(MarkdownCrossReferenceMatch {
                        prefix: spec.prefix.to_string(),
                        identifier: source[identifier_start..end].to_string(),
                        style: MarkdownReferenceStyle::Quarto,
                        range,
                    });
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    matches
}

/// Limit reference recognition to source ranges that pulldown-cmark exposes as
/// visible prose. This excludes metadata that never becomes a text node—such
/// as footnote labels and reference-link destinations—so masking cannot alter
/// unrelated Markdown structure before the real parse.
fn markdown_text_event_ranges(source: &str, options: Options) -> Vec<bool> {
    let (math_aware_source, _) = protect_tex_math_delimiters(source, options);
    let mut eligible = vec![false; source.len()];
    for (event, range) in Parser::new_ext(&math_aware_source, options).into_offset_iter() {
        if matches!(event, Event::Text(_)) {
            let end = range.end.min(eligible.len());
            for item in &mut eligible[range.start.min(end)..end] {
                *item = true;
            }
        }
    }
    eligible
}

fn markdown_reference_starts_in_prose(at: usize, eligible: &[bool], protected: &[bool]) -> bool {
    eligible.get(at).copied().unwrap_or(false) && !protected.get(at).copied().unwrap_or(true)
}

fn mask_markdown_cross_references(
    source: &str,
    references: &[MarkdownCrossReferenceMatch],
) -> String {
    let mut masked = source.as_bytes().to_vec();
    for reference in references {
        for byte in &mut masked[reference.range.clone()] {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b'r';
            }
        }
    }
    String::from_utf8(masked).expect("ASCII masking preserves UTF-8")
}

fn integrate_markdown_cross_references(
    nodes: &mut Vec<Node>,
    references: &[MarkdownCrossReferenceMatch],
    source: &str,
    positions: &LineIndex<'_>,
    file: &Path,
) {
    for node in nodes.iter_mut() {
        integrate_markdown_cross_references(
            &mut node.children,
            references,
            source,
            positions,
            file,
        );
    }

    let mut output = Vec::with_capacity(nodes.len());
    for node in std::mem::take(nodes) {
        let NodeKind::MarkdownText(text) = &node.kind else {
            output.push(node);
            continue;
        };
        let start = node.span.start.byte as usize;
        let end = node.span.end.byte as usize;
        if text.len() != end.saturating_sub(start) {
            output.push(node);
            continue;
        }
        let first = references.partition_point(|reference| reference.range.end <= start);
        let contained: Vec<_> = references[first..]
            .iter()
            .take_while(|reference| reference.range.start < end)
            .filter(|reference| start <= reference.range.start && reference.range.end <= end)
            .collect();
        if contained.is_empty() {
            output.push(node);
            continue;
        }

        let mut cursor = start;
        for reference in contained {
            if cursor < reference.range.start {
                output.push(Node {
                    kind: NodeKind::MarkdownText(
                        text[cursor - start..reference.range.start - start].to_string(),
                    ),
                    span: positions.span(file, cursor, reference.range.start),
                    children: Vec::new(),
                });
            }
            output.push(Node {
                kind: NodeKind::MarkdownCrossReference {
                    prefix: reference.prefix.clone(),
                    identifier: reference.identifier.clone(),
                    raw: source[reference.range.clone()].to_string(),
                    style: reference.style,
                    anchor: None,
                    display: None,
                },
                span: positions.span(file, reference.range.start, reference.range.end),
                children: Vec::new(),
            });
            cursor = reference.range.end;
        }
        if cursor < end {
            output.push(Node {
                kind: NodeKind::MarkdownText(text[cursor - start..].to_string()),
                span: positions.span(file, cursor, end),
                children: Vec::new(),
            });
        }
    }
    *nodes = output;
}

fn promote_markdown_theorem_headings(nodes: &mut [Node]) {
    for node in nodes {
        let consume_heading = matches!(
            &node.kind,
            NodeKind::MarkdownCustomBlock {
                title: None,
                theorem: Some(MarkdownTheoremMeta {
                    dialect: MarkdownTheoremDialect::Quarto,
                    ..
                }),
                ..
            }
        ) && matches!(
            node.children.first().map(|child| &child.kind),
            Some(NodeKind::MarkdownHeading { .. })
        );
        if consume_heading {
            let heading_span = node.children[0].span.clone();
            let mut title = String::new();
            markdown_heading_text(&node.children[0].children, &mut title);
            let title = title.trim();
            if !title.is_empty() && title.chars().count() <= 512 {
                node.children.remove(0);
                if let NodeKind::MarkdownCustomBlock {
                    title: current,
                    theorem,
                    ..
                } = &mut node.kind
                {
                    *current = Some(title.to_string());
                    if let Some(theorem) = theorem {
                        theorem.title_span = Some(heading_span);
                    }
                }
            }
        }
        promote_markdown_theorem_headings(&mut node.children);
    }
}

#[derive(Clone)]
struct MarkdownTheoremTarget {
    anchor: String,
    label: String,
    number: String,
}

fn markdown_theorem_target_key(
    dialect: MarkdownTheoremDialect,
    prefix: &str,
    identifier: &str,
) -> String {
    let dialect = match dialect {
        MarkdownTheoremDialect::Bookdown => 'b',
        MarkdownTheoremDialect::Quarto => 'q',
    };
    format!("{dialect}\0{prefix}\0{identifier}")
}

fn markdown_theorem_anchor_base(prefix: &str, identifier: &str) -> String {
    let identifier: String = identifier
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    format!("{prefix}-{identifier}")
}

fn assign_markdown_theorems_and_references(nodes: &mut [Node], config: &ResolvedMarkdownConfig) {
    fn assign_blocks(
        nodes: &mut [Node],
        config: &ResolvedMarkdownConfig,
        counters: &mut HashMap<String, u32>,
        targets: &mut HashMap<String, MarkdownTheoremTarget>,
        custom_targets: &mut HashMap<String, String>,
        used_anchors: &mut HashSet<String>,
        next_suffix: &mut HashMap<String, u32>,
    ) {
        for node in nodes {
            if let NodeKind::MarkdownCustomBlock {
                name,
                label,
                anchor,
                theorem,
                ..
            } = &mut node.kind
            {
                if let Some(theorem) = theorem {
                    if theorem.numbered {
                        let counter = counters.entry(theorem.prefix.clone()).or_default();
                        *counter += 1;
                        theorem.number = Some(counter.to_string());
                    }
                    if let (Some(identifier), Some(number)) =
                        (theorem.identifier.as_deref(), theorem.number.as_deref())
                    {
                        let target = unique_markdown_target(
                            markdown_theorem_anchor_base(&theorem.prefix, identifier),
                            used_anchors,
                            next_suffix,
                        );
                        theorem.anchor = Some(target.clone());
                        if let Some(format) = config.blocks.get(name) {
                            targets
                                .entry(markdown_theorem_target_key(
                                    theorem.dialect,
                                    &theorem.prefix,
                                    identifier,
                                ))
                                .or_insert_with(|| MarkdownTheoremTarget {
                                    anchor: target,
                                    label: format.label.clone(),
                                    number: number.to_string(),
                                });
                        }
                    }
                }
                if let Some(label) = label.as_ref() {
                    let target = unique_markdown_target(label.clone(), used_anchors, next_suffix);
                    *anchor = Some(target.clone());
                    custom_targets.entry(label.clone()).or_insert(target);
                }
            }
            assign_blocks(
                &mut node.children,
                config,
                counters,
                targets,
                custom_targets,
                used_anchors,
                next_suffix,
            );
        }
    }

    fn resolve_references(
        nodes: &mut [Node],
        targets: &HashMap<String, MarkdownTheoremTarget>,
        custom_targets: &HashMap<String, String>,
    ) {
        for node in nodes {
            if let NodeKind::MarkdownCrossReference {
                prefix,
                identifier,
                style,
                anchor,
                display,
                ..
            } = &mut node.kind
            {
                let dialect = match *style {
                    MarkdownReferenceStyle::Bookdown => MarkdownTheoremDialect::Bookdown,
                    MarkdownReferenceStyle::Quarto => MarkdownTheoremDialect::Quarto,
                };
                if let Some(target) =
                    targets.get(&markdown_theorem_target_key(dialect, prefix, identifier))
                {
                    *anchor = Some(target.anchor.clone());
                    *display = Some(match *style {
                        MarkdownReferenceStyle::Bookdown => target.number.clone(),
                        MarkdownReferenceStyle::Quarto => {
                            format!("{} {}", target.label, target.number)
                        }
                    });
                }
            }
            if let NodeKind::MarkdownLink { destination, .. } = &mut node.kind {
                if let Some(fragment) = destination
                    .strip_prefix('#')
                    .filter(|fragment| !fragment.starts_with("mdr:"))
                    .and_then(decode_markdown_fragment)
                {
                    if let Some(anchor) = custom_targets.get(&fragment) {
                        *destination = format!("#mdr:{anchor}");
                    }
                }
            }
            resolve_references(&mut node.children, targets, custom_targets);
        }
    }

    let mut counters = HashMap::new();
    let mut targets = HashMap::new();
    let mut custom_targets = HashMap::new();
    let mut used_anchors = HashSet::new();
    let mut next_suffix = HashMap::new();
    assign_blocks(
        nodes,
        config,
        &mut counters,
        &mut targets,
        &mut custom_targets,
        &mut used_anchors,
        &mut next_suffix,
    );
    resolve_references(nodes, &targets, &custom_targets);
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
}

fn node_kind_for_tag(tag: Tag<'_>) -> NodeKind {
    match tag {
        Tag::Paragraph => NodeKind::MarkdownParagraph,
        Tag::Heading { level, .. } => NodeKind::MarkdownHeading {
            level: heading_level(level),
            anchor: String::new(),
        },
        Tag::BlockQuote(_) => NodeKind::MarkdownBlockQuote,
        Tag::CodeBlock(kind) => NodeKind::MarkdownCodeBlock {
            language: match kind {
                CodeBlockKind::Indented => None,
                CodeBlockKind::Fenced(info) => info
                    .split_ascii_whitespace()
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            },
            code: String::new(),
        },
        Tag::HtmlBlock | Tag::MetadataBlock(_) => NodeKind::MarkdownRawHtmlBlock,
        Tag::List(start) => NodeKind::MarkdownList {
            ordered: start.is_some(),
            start,
        },
        Tag::Item => NodeKind::MarkdownListItem,
        Tag::FootnoteDefinition(label) => NodeKind::MarkdownFootnoteDefinition {
            label: label.into_string(),
            target: String::new(),
        },
        Tag::DefinitionList => NodeKind::MarkdownDefinitionList,
        Tag::DefinitionListTitle => NodeKind::MarkdownDefinitionTerm,
        Tag::DefinitionListDefinition => NodeKind::MarkdownDefinitionDescription,
        Tag::Table(alignments) => NodeKind::MarkdownTable {
            alignments: alignments.into_iter().map(markdown_alignment).collect(),
        },
        Tag::TableHead => NodeKind::MarkdownTableHead,
        Tag::TableRow => NodeKind::MarkdownTableRow,
        Tag::TableCell => NodeKind::MarkdownTableCell,
        Tag::Emphasis => NodeKind::MarkdownEmphasis,
        Tag::Strong => NodeKind::MarkdownStrong,
        Tag::Strikethrough => NodeKind::MarkdownStrikethrough,
        Tag::Superscript => NodeKind::MarkdownSuperscript,
        Tag::Subscript => NodeKind::MarkdownSubscript,
        Tag::Link {
            dest_url, title, ..
        } => NodeKind::MarkdownLink {
            destination: dest_url.into_string(),
            title: nonempty(title.into_string()),
        },
        Tag::Image {
            dest_url, title, ..
        } => NodeKind::MarkdownImage {
            destination: dest_url.into_string(),
            title: nonempty(title.into_string()),
        },
    }
}

fn append_leaf(roots: &mut Vec<Node>, stack: &mut [Node], kind: NodeKind, span: Span) {
    append_node(
        roots,
        stack,
        Node {
            kind,
            span,
            children: Vec::new(),
        },
    );
}

fn append_node(roots: &mut Vec<Node>, stack: &mut [Node], node: Node) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        roots.push(node);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkdownTextClass {
    Whitespace,
    Visible,
}

fn markdown_text_class(ch: char) -> MarkdownTextClass {
    if ch.is_whitespace() {
        MarkdownTextClass::Whitespace
    } else {
        MarkdownTextClass::Visible
    }
}

fn markdown_text_runs(text: &str) -> Vec<Range<usize>> {
    let mut runs = Vec::new();
    let mut chars = text.char_indices();
    let Some((mut start, first)) = chars.next() else {
        return runs;
    };
    let mut class = markdown_text_class(first);
    for (byte, ch) in chars {
        let next_class = markdown_text_class(ch);
        if next_class != class {
            runs.push(start..byte);
            start = byte;
            class = next_class;
        }
    }
    runs.push(start..text.len());
    runs
}

/// Split source-identical Markdown text into small sync units while preserving
/// pulldown-cmark's exact byte offsets. Entity and backslash-escape events are
/// kept atomic because their rendered text is not byte-identical to the source.
/// Code events get a second, ordered-search mapping path so indented blocks
/// (whose source ranges retain indentation stripped from rendered text) still
/// receive precise token anchors.
fn markdown_text_nodes(
    source: &str,
    text: &str,
    range: Range<usize>,
    positions: &LineIndex<'_>,
    file: &Path,
    code_block: bool,
) -> Vec<Node> {
    let runs = markdown_text_runs(text);
    if runs.is_empty() {
        return Vec::new();
    }

    let exact = source.get(range.clone()) == Some(text);
    let mut search_from = range.start;
    let mut mapped = Vec::with_capacity(runs.len());
    for run in runs {
        let value = &text[run.clone()];
        let source_range = if exact {
            range.start + run.start..range.start + run.end
        } else if code_block && !value.chars().all(char::is_whitespace) {
            let Some(offset) = source
                .get(search_from..range.end)
                .and_then(|remaining| remaining.find(value))
            else {
                return vec![Node {
                    kind: NodeKind::MarkdownText(text.to_string()),
                    span: positions.span(file, range.start, range.end),
                    children: Vec::new(),
                }];
            };
            let start = search_from + offset;
            let end = start + value.len();
            search_from = end;
            start..end
        } else if code_block {
            // Whitespace is emitted verbatim but intentionally has no sync
            // leaf. Its approximate span is therefore used only to retain the
            // source ordering of the AST; the next visible token is searched
            // from the last exact match above.
            search_from..search_from
        } else {
            return vec![Node {
                kind: NodeKind::MarkdownText(text.to_string()),
                span: positions.span(file, range.start, range.end),
                children: Vec::new(),
            }];
        };
        mapped.push(Node {
            kind: NodeKind::MarkdownText(value.to_string()),
            span: positions.span(file, source_range.start, source_range.end),
            children: Vec::new(),
        });
    }
    mapped
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Assign GitHub-compatible fragment names in document order. The slugger's
/// occupied set is global across the Markdown AST, including headings nested
/// in blockquotes and other containers.
fn assign_markdown_heading_anchors(nodes: &mut [Node]) {
    fn visit(
        nodes: &mut [Node],
        used: &mut HashSet<String>,
        next_suffix: &mut HashMap<String, u32>,
    ) {
        for node in nodes {
            if matches!(node.kind, NodeKind::MarkdownHeading { .. }) {
                let mut text = String::new();
                markdown_heading_text(&node.children, &mut text);
                let anchor = unique_markdown_target(github_heading_slug(&text), used, next_suffix);
                if let NodeKind::MarkdownHeading {
                    anchor: current, ..
                } = &mut node.kind
                {
                    *current = anchor;
                }
            }
            visit(&mut node.children, used, next_suffix);
        }
    }

    visit(nodes, &mut HashSet::new(), &mut HashMap::new());
}

fn markdown_heading_text(nodes: &[Node], out: &mut String) {
    for node in nodes {
        match &node.kind {
            NodeKind::MarkdownText(text)
            | NodeKind::MarkdownInlineCode(text)
            | NodeKind::MarkdownRawHtml(text)
            | NodeKind::InlineMath(text) => out.push_str(text),
            NodeKind::MarkdownFootnoteReference { label, .. } => out.push_str(label),
            NodeKind::MarkdownCrossReference { raw, display, .. } => {
                out.push_str(display.as_deref().unwrap_or(raw));
            }
            NodeKind::MarkdownSoftBreak | NodeKind::MarkdownHardBreak => out.push(' '),
            _ => markdown_heading_text(&node.children, out),
        }
    }
}

fn collect_markdown_heading_anchors(nodes: &[Node], anchors: &mut HashSet<String>) {
    for node in nodes {
        if let NodeKind::MarkdownHeading { anchor, .. } = &node.kind {
            anchors.insert(anchor.clone());
        }
        collect_markdown_heading_anchors(&node.children, anchors);
    }
}

fn percent_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_markdown_fragment(fragment: &str) -> Option<String> {
    let bytes = fragment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let high = percent_hex(*bytes.get(i + 1)?)?;
            let low = percent_hex(*bytes.get(i + 2)?)?;
            decoded.push((high << 4) | low);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded)
        .ok()
        .filter(|value| !value.chars().any(char::is_control))
}

fn resolve_markdown_heading_links(nodes: &mut [Node]) {
    fn visit(nodes: &mut [Node], anchors: &HashSet<String>) {
        for node in nodes {
            if let NodeKind::MarkdownLink { destination, .. } = &mut node.kind {
                if let Some(fragment) = destination
                    .strip_prefix('#')
                    .filter(|value| !value.is_empty())
                {
                    let already_canonical = fragment
                        .strip_prefix("mdh:")
                        .and_then(decode_markdown_fragment)
                        .is_some_and(|decoded| anchors.contains(&decoded));
                    if !already_canonical
                        && decode_markdown_fragment(fragment)
                            .is_some_and(|decoded| anchors.contains(&decoded))
                    {
                        *destination = format!("#mdh:{fragment}");
                    }
                }
            }
            visit(&mut node.children, anchors);
        }
    }

    let mut anchors = HashSet::new();
    collect_markdown_heading_anchors(nodes, &mut anchors);
    visit(nodes, &anchors);
}

fn markdown_footnote_base(label: &str) -> String {
    let target: String = label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if target.is_empty() {
        "note".to_string()
    } else {
        target
    }
}

fn unique_markdown_target(
    base: String,
    used: &mut HashSet<String>,
    next_suffix: &mut HashMap<String, u32>,
) -> String {
    if used.insert(base.clone()) {
        next_suffix.entry(base.clone()).or_insert(1);
        return base;
    }
    let suffix = next_suffix.entry(base.clone()).or_insert(1);
    loop {
        let candidate = format!("{base}-{suffix}");
        *suffix += 1;
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
}

/// Pulldown-cmark resolves footnote labels with full Unicode case folding.
/// Preserve those semantics while assigning a distinct id to every rendered
/// definition. Repeated definitions resolve references to the last occurrence,
/// matching pulldown-cmark's definition table overwrite behavior.
fn assign_markdown_footnote_targets(nodes: &mut [Node]) {
    fn assign_definitions(
        nodes: &mut [Node],
        references: &mut HashMap<UniCase<String>, String>,
        used: &mut HashSet<String>,
        next_suffix: &mut HashMap<String, u32>,
    ) {
        for node in nodes {
            if let NodeKind::MarkdownFootnoteDefinition { label, target } = &mut node.kind {
                let assigned =
                    unique_markdown_target(markdown_footnote_base(label), used, next_suffix);
                *target = assigned.clone();
                references.insert(UniCase::new(label.clone()), assigned);
            }
            assign_definitions(&mut node.children, references, used, next_suffix);
        }
    }

    fn assign_references(nodes: &mut [Node], references: &HashMap<UniCase<String>, String>) {
        for node in nodes {
            if let NodeKind::MarkdownFootnoteReference { label, target } = &mut node.kind {
                *target = references
                    .get(&UniCase::new(label.clone()))
                    .cloned()
                    .unwrap_or_else(|| markdown_footnote_base(label));
            }
            assign_references(&mut node.children, references);
        }
    }

    let mut references = HashMap::new();
    let mut used = HashSet::new();
    let mut next_suffix = HashMap::new();
    assign_definitions(nodes, &mut references, &mut used, &mut next_suffix);
    assign_references(nodes, &references);
}

fn markdown_alignment(alignment: Alignment) -> MarkdownAlignment {
    match alignment {
        Alignment::None => MarkdownAlignment::None,
        Alignment::Left => MarkdownAlignment::Left,
        Alignment::Center => MarkdownAlignment::Center,
        Alignment::Right => MarkdownAlignment::Right,
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[derive(Debug)]
struct MathOverride {
    display: bool,
    body: String,
}

/// Pulldown-cmark natively recognizes `$…$` and `$$…$$`. Convert the two
/// MathJax-style backslash delimiter pairs to byte-for-byte-equivalent dollar
/// forms before parsing, while preserving exact source offsets. Conversion is
/// restricted to one prose block at a time: Markdown destinations, titles,
/// autolinks, images, code, HTML, and block boundaries remain inert.
fn protect_tex_math_delimiters(
    source: &str,
    options: Options,
) -> (String, HashMap<(usize, usize), MathOverride>) {
    let mut prose_ranges = Vec::new();
    let mut protected = vec![false; source.len()];
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        match event {
            Event::Start(
                Tag::Paragraph | Tag::Heading { .. } | Tag::TableCell | Tag::DefinitionListTitle,
            ) => prose_ranges.push(range),
            Event::Start(Tag::Link { link_type, .. }) => {
                if matches!(
                    link_type,
                    pulldown_cmark::LinkType::Autolink | pulldown_cmark::LinkType::Email
                ) {
                    protect_range(&mut protected, range);
                } else {
                    protect_link_metadata(source, &mut protected, range);
                }
            }
            Event::Start(Tag::Image { .. } | Tag::CodeBlock(_) | Tag::HtmlBlock) => {
                protect_range(&mut protected, range);
            }
            Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::Html(_)
            | Event::InlineHtml(_) => protect_range(&mut protected, range),
            _ => {}
        }
    }

    let bytes = source.as_bytes();
    let mut transformed = bytes.to_vec();
    let mut overrides = HashMap::new();
    for range in prose_ranges {
        transform_tex_math_in_prose_range(
            source,
            range,
            &protected,
            &mut transformed,
            &mut overrides,
        );
    }
    (
        String::from_utf8(transformed).expect("ASCII substitutions preserve UTF-8"),
        overrides,
    )
}

fn transform_tex_math_in_prose_range(
    source: &str,
    range: Range<usize>,
    protected: &[bool],
    transformed: &mut [u8],
    overrides: &mut HashMap<(usize, usize), MathOverride>,
) {
    let bytes = source.as_bytes();
    let mut i = range.start;
    while i + 1 < range.end {
        if protected[i]
            || bytes[i] != b'\\'
            || !is_unescaped_backslash(bytes, i)
            || !matches!(bytes[i + 1], b'(' | b'[')
        {
            i += 1;
            continue;
        }
        let display = bytes[i + 1] == b'[';
        let close_delimiter = if display { b']' } else { b')' };
        let unprotected_end = protected[i..range.end]
            .iter()
            .position(|item| *item)
            .map(|offset| i + offset)
            .unwrap_or(range.end);
        let Some(close) = find_tex_math_close(bytes, i + 2, unprotected_end, close_delimiter)
        else {
            i += 2;
            continue;
        };
        let range_end = close + 2;
        let body = source[i + 2..close].to_string();
        if display {
            transformed[i..i + 2].copy_from_slice(b"$$");
            transformed[close..range_end].copy_from_slice(b"$$");
        } else {
            // `${…}$` has the same byte length as `\(…\)` and gives the math
            // parser balanced braces. The event body is replaced with the
            // original unwrapped source below.
            transformed[i..i + 2].copy_from_slice(b"${");
            transformed[close..range_end].copy_from_slice(b"}$");
        }
        overrides.insert((i, range_end), MathOverride { display, body });
        i = range_end;
    }
}

fn find_tex_math_close(bytes: &[u8], start: usize, end: usize, delimiter: u8) -> Option<usize> {
    (start..end.saturating_sub(1)).find(|index| {
        bytes[*index] == b'\\'
            && bytes[*index + 1] == delimiter
            && is_unescaped_backslash(bytes, *index)
    })
}

fn protect_link_metadata(source: &str, protected: &mut [bool], range: Range<usize>) {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut i = range.start;
    while i < range.end {
        match bytes[i] {
            b'\\' if is_unescaped_backslash(bytes, i) => {
                i = (i + 2).min(range.end);
                continue;
            }
            b'[' => depth += 1,
            b']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    protect_range(protected, i + 1..range.end);
                    return;
                }
            }
            _ => {}
        }
        i += 1;
    }
    // A parser-recognized link with an unexpected source shape is safer left
    // entirely inert than partially rewritten.
    protect_range(protected, range);
}

fn protect_range(mask: &mut [bool], range: Range<usize>) {
    let end = range.end.min(mask.len());
    for item in &mut mask[range.start.min(end)..end] {
        *item = true;
    }
}

fn is_unescaped_backslash(bytes: &[u8], at: usize) -> bool {
    let mut count = 0usize;
    let mut i = at;
    while i > 0 && bytes[i - 1] == b'\\' {
        count += 1;
        i -= 1;
    }
    count % 2 == 0
}

struct LineIndex<'a> {
    source: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(i, byte)| (byte == b'\n').then_some(i + 1)),
        );
        Self { source, starts }
    }

    fn pos(&self, byte: usize) -> Pos {
        let byte = byte.min(self.source.len());
        let line_index = self.starts.partition_point(|start| *start <= byte) - 1;
        let line_start = self.starts[line_index];
        Pos {
            line: (line_index + 1) as u32,
            // Neovim's cursor API and the preview sync protocol both use
            // 1-based UTF-8 byte columns. `pulldown-cmark` offsets are bytes,
            // so preserve that unit instead of counting Unicode scalars.
            col: (byte - line_start + 1) as u32,
            byte: byte as u32,
        }
    }

    fn span(&self, file: &Path, start: usize, end: usize) -> Span {
        Span {
            file: file.to_path_buf(),
            start: self.pos(start),
            end: self.pos(end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_nodes(nodes: &[Node]) -> Vec<&Node> {
        fn visit<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
            for node in nodes {
                out.push(node);
                visit(&node.children, out);
            }
        }
        let mut out = Vec::new();
        visit(nodes, &mut out);
        out
    }

    fn pos(line: u32, col: u32, byte: u32) -> Pos {
        Pos { line, col, byte }
    }

    fn custom_block<'a>(nodes: &'a [Node], name: &str) -> &'a Node {
        all_nodes(nodes)
            .into_iter()
            .find(|node| {
                matches!(
                    &node.kind,
                    NodeKind::MarkdownCustomBlock { name: found, .. } if found == name
                )
            })
            .unwrap_or_else(|| panic!("missing custom Markdown block {name:?}"))
    }

    fn config_with_block(name: &str) -> ResolvedMarkdownConfig {
        let mut config = ResolvedMarkdownConfig::default();
        let format = config
            .blocks
            .get("proof")
            .expect("default proof format")
            .clone();
        config.blocks.insert(name.to_string(), format);
        config
    }

    fn config_with_jinja_result_syntax() -> ResolvedMarkdownConfig {
        let mut config = ResolvedMarkdownConfig::default();
        config.block_syntaxes.insert(
            "jinja-result".to_string(),
            crate::config::ResolvedMarkdownBlockSyntax {
                start: vec![
                    r#"{% call result("{name}", "{title}") %}"#.to_string(),
                    r#"{% call result("{name}") %}"#.to_string(),
                ],
                end: "{% endcall %}".to_string(),
            },
        );
        config
    }

    fn config_with_jinja_result_arguments() -> ResolvedMarkdownConfig {
        let mut config = config_with_block("problem");
        config.block_syntaxes.insert(
            "jinja-result".to_string(),
            crate::config::ResolvedMarkdownBlockSyntax {
                start: vec![
                    r#"{% call result("{name}", "{title}", "{label}", card={card}) %}"#.to_string(),
                    r#"{% call result("{name}", "{title}", "{label}") %}"#.to_string(),
                    r#"{% call result("{name}", "{title}", label="{label}", card={card}) %}"#
                        .to_string(),
                    r#"{% call result("{name}", "{title}", label="{label}") %}"#.to_string(),
                    r#"{% call result("{name}", label="{label}", card={card}) %}"#.to_string(),
                    r#"{% call result("{name}", label="{label}", card="{card}") %}"#.to_string(),
                    r#"{% call result("{name}", label="{label}") %}"#.to_string(),
                    r#"{% call result("{name}", "{title}", card={card}) %}"#.to_string(),
                    r#"{% call result("{name}", card={card}) %}"#.to_string(),
                    r#"{% call result("{name}", "{title}") %}"#.to_string(),
                    r#"{% call result("{name}") %}"#.to_string(),
                ],
                end: "{% endcall %}".to_string(),
            },
        );
        config
    }

    #[test]
    fn default_parser_recognizes_proof_blocks_with_plain_titles_and_exact_spans() {
        let path = Path::new("proof.md");
        let source = concat!(
            "before\n\n",
            "  :::proof A **plain** title\n",
            "Body with **bold** and $x$.\n",
            "  :::\n\n",
            "after\n",
        );
        let nodes = parse(source, path).unwrap();
        let proof = custom_block(&nodes, "proof");
        assert!(matches!(
            &proof.kind,
            NodeKind::MarkdownCustomBlock { title, .. }
                if title.as_deref() == Some("A **plain** title")
        ));

        let start = source.find("  :::proof").unwrap();
        let closing = source.find("  :::\n\n").unwrap();
        let end = closing + "  :::\n".len();
        assert_eq!(
            proof.span,
            LineIndex::new(source).span(path, start, end),
            "the wrapper must cover both complete marker lines"
        );
        assert!(all_nodes(&proof.children)
            .iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownStrong)));
        assert!(all_nodes(&proof.children)
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "x")));
    }

    #[test]
    fn literal_templates_render_the_users_jinja_result_blocks() {
        let config = config_with_jinja_result_syntax();
        let source = concat!(
            r#"{% call result("definition", "Poisson distribution") %}"#,
            "\n",
            "We say $X \\sim \\Pois(\\lambda)$ when\n",
            "$$\\P(X=k)=e^{-\\lambda}\\lambda^k/k!.$$\n",
            "{% endcall %}\n\n",
            r#"{% call result("proposition") %}"#,
            "\n",
            "For every $N \\in \\N$, the binomial probabilities converge.\n",
            "{% endcall %}\n",
        );
        let nodes = parse_with_config(source, Path::new("jinja.md"), &config).unwrap();
        let blocks: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock {
                    name,
                    title,
                    theorem,
                    ..
                } => Some((name.as_str(), title.as_deref(), theorem.as_ref())),
                _ => None,
            })
            .collect();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "definition");
        assert_eq!(blocks[0].1, Some("Poisson distribution"));
        assert!(blocks[0].2.is_none(), "custom syntax stays presentational");
        assert_eq!(blocks[1].0, "proposition");
        assert_eq!(blocks[1].1, None);
        assert!(blocks[1].2.is_none());
        assert!(all_nodes(&nodes)
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body.contains("Pois"))));
        assert!(all_nodes(&nodes)
            .iter()
            .any(|node| matches!(node.kind, NodeKind::DisplayMath { .. })));
    }

    #[test]
    fn literal_templates_capture_jinja_labels_and_card_arguments() {
        let config = config_with_jinja_result_arguments();
        let source = concat!(
            "[Jump forward](#t-greens).\n\n",
            r#"{% call result("theorem", "Green's theorem", "t-greens", card=true) %}"#,
            "\nFirst body.\n{% endcall %}\n\n",
            r#"{% call result("problem", label="p-area") %}"#,
            "\nSecond body.\n{% endcall %}\n\n",
            r#"{% call result("definition", label="d-sint", card="true") %}"#,
            "\nThird body.\n{% endcall %}\n\n",
            r#"{% call result("remark", "Stable", label="r-stable", card=false) %}"#,
            "\nFourth body.\n{% endcall %}\n\n",
            r#"{% call result("example", card=true) %}"#,
            "\nFifth body.\n{% endcall %}\n\n",
            r#"{% call result("proof", "Standalone card", card=true) %}"#,
            "\nSixth body.\n{% endcall %}\n",
        );
        let nodes = parse_with_config(source, Path::new("arguments.md"), &config).unwrap();
        let blocks: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock {
                    name,
                    title,
                    label,
                    anchor,
                    card,
                    theorem,
                    ..
                } => Some((
                    name.as_str(),
                    title.as_deref(),
                    label.as_deref(),
                    anchor.as_deref(),
                    *card,
                    theorem.as_ref(),
                )),
                _ => None,
            })
            .collect();

        assert_eq!(blocks.len(), 6);
        assert_eq!(
            &blocks[..3],
            &[
                (
                    "theorem",
                    Some("Green's theorem"),
                    Some("t-greens"),
                    Some("t-greens"),
                    true,
                    None,
                ),
                ("problem", None, Some("p-area"), Some("p-area"), false, None,),
                (
                    "definition",
                    None,
                    Some("d-sint"),
                    Some("d-sint"),
                    true,
                    None,
                ),
            ]
        );
        assert_eq!(blocks[3].0, "remark");
        assert_eq!(blocks[3].1, Some("Stable"));
        assert_eq!(blocks[3].2, Some("r-stable"));
        assert!(!blocks[3].4);
        assert_eq!(blocks[4], ("example", None, None, None, true, None));
        assert_eq!(
            blocks[5],
            ("proof", Some("Standalone card"), None, None, true, None,)
        );
        assert!(blocks.iter().all(|block| block.5.is_none()));
        assert!(all_nodes(&nodes).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownLink { destination, .. } if destination == "#mdr:t-greens"
        )));
    }

    #[test]
    fn literal_template_labels_are_duplicate_safe_and_invalid_captures_fail_closed() {
        let config = config_with_jinja_result_arguments();
        let source = concat!(
            "[First target](#same).\n\n",
            r#"{% call result("definition", label="same") %}"#,
            "\nFirst.\n{% endcall %}\n\n",
            r#"{% call result("proposition", label="same") %}"#,
            "\nSecond.\n{% endcall %}\n\n",
            r#"{% call result("remark", label="bad label") %}"#,
            "\nInvalid label.\n{% endcall %}\n\n",
            r#"{% call result("example", label="ok", card=yes) %}"#,
            "\nInvalid card.\n{% endcall %}\n",
        );
        let nodes = parse_with_config(source, Path::new("fail-closed.md"), &config).unwrap();
        let anchors: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock { anchor, .. } => anchor.as_deref(),
                _ => None,
            })
            .collect();

        assert_eq!(anchors, ["same", "same-1"]);
        assert!(all_nodes(&nodes).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownLink { destination, .. } if destination == "#mdr:same"
        )));
        let visible = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(visible.contains("bad label"));
        assert!(visible.contains("card=yes"));
    }

    #[test]
    fn literal_labels_share_theorem_allocation_and_win_plain_fragment_links() {
        let config = config_with_jinja_result_arguments();
        let source = concat!(
            "[Custom](#thm-key). Bookdown: \\@ref(thm:key).\n\n",
            r#"{% call result("definition", label="thm-key") %}"#,
            "\nCustom body.\n{% endcall %}\n\n",
            "# thm-key\n\n",
            "::: {.theorem #key}\nSemantic body.\n:::\n",
        );
        let nodes = parse_with_config(source, Path::new("target-collisions.md"), &config).unwrap();

        assert!(matches!(
            &custom_block(&nodes, "definition").kind,
            NodeKind::MarkdownCustomBlock { anchor, .. }
                if anchor.as_deref() == Some("thm-key")
        ));
        assert!(matches!(
            &custom_block(&nodes, "theorem").kind,
            NodeKind::MarkdownCustomBlock {
                theorem: Some(MarkdownTheoremMeta { anchor, .. }),
                ..
            } if anchor.as_deref() == Some("thm-key-1")
        ));
        assert!(all_nodes(&nodes).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownHeading { anchor, .. } if anchor == "thm-key"
        )));
        assert!(all_nodes(&nodes).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownLink { destination, .. } if destination == "#mdr:thm-key"
        )));
        assert!(all_nodes(&nodes).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownCrossReference { anchor, .. }
                if anchor.as_deref() == Some("thm-key-1")
        )));
    }

    #[test]
    fn titled_literal_templates_choose_the_longest_enabled_name_prefix() {
        let mut config = config_with_block("guided");
        let format = config
            .blocks
            .get("proof")
            .expect("default proof format")
            .clone();
        config.blocks.insert("guided-exercise".to_string(), format);
        config.block_syntaxes.insert(
            "dash-title".to_string(),
            crate::config::ResolvedMarkdownBlockSyntax {
                start: vec!["BEGIN {name}-{title}".to_string()],
                end: "END".to_string(),
            },
        );
        let source = "BEGIN guided-exercise-Poisson distribution\nBody.\nEND\n";

        let nodes = parse_with_config(source, Path::new("longest-name.md"), &config).unwrap();
        assert!(matches!(
            &custom_block(&nodes, "guided-exercise").kind,
            NodeKind::MarkdownCustomBlock { title, .. }
                if title.as_deref() == Some("Poisson distribution")
        ));
        assert!(!all_nodes(&nodes).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownCustomBlock { name, .. } if name == "guided"
        )));
    }

    #[test]
    fn literal_template_match_budget_exhaustion_stays_opaque() {
        let mut config = config_with_block("guided");
        config.block_syntaxes.insert(
            "budgeted".to_string(),
            crate::config::ResolvedMarkdownBlockSyntax {
                start: vec!["BEGIN {name}-{title}-{label}-{card} END".to_string()],
                end: "END".to_string(),
            },
        );
        let opener = format!("BEGIN proof-{}ok-true END", "part-".repeat(80));
        let syntax = markdown_literal_block_syntaxes(&config)
            .into_iter()
            .find(|syntax| syntax.family == MarkdownCustomBlockFamily::Configured(0))
            .expect("budgeted syntax");
        let mut budget = MarkdownLiteralBlockMatchBudget::new(64);
        assert!(matches!(
            markdown_literal_block_opening(&opener, &syntax, &config, &mut budget),
            Err(MarkdownLiteralBlockMatchExhausted)
        ));
        assert_eq!(budget.remaining, 0);

        let source = format!("{opener}\nOpaque body.\nEND\n");
        let scan = find_markdown_custom_blocks_with_match_work_limit(
            &source,
            markdown_options(),
            &config,
            64,
        );
        assert!(scan.blocks.is_empty());
        assert_eq!(
            mask_markdown_custom_block_markers(&source, &scan.blocks),
            source
        );
        assert_eq!(scan.marker_lines.len(), 2);
    }

    #[test]
    fn repeated_literal_capture_separators_are_ambiguous_and_stay_opaque() {
        let mut config = config_with_block("guided");
        config.block_syntaxes.insert(
            "ambiguous".to_string(),
            crate::config::ResolvedMarkdownBlockSyntax {
                start: vec!["BEGIN {name}-{title}-{label}".to_string()],
                end: "END".to_string(),
            },
        );
        let source = "BEGIN proof-A-B-C\nOpaque body.\nEND\n";

        let nodes = parse_with_config(source, Path::new("ambiguous.md"), &config).unwrap();
        assert!(!all_nodes(&nodes)
            .iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownCustomBlock { .. })));
    }

    #[test]
    fn literal_match_budget_supports_the_maximum_configured_start_count() {
        let mut config = ResolvedMarkdownConfig::default();
        let mut global = 0;
        for syntax_index in 0..MAX_MARKDOWN_BLOCK_SYNTAXES {
            let mut start = Vec::new();
            for _ in 0..MAX_MARKDOWN_BLOCK_SYNTAX_STARTS {
                start.push(format!("BEGIN {{name}}-{{title}} Z{global:03}"));
                global += 1;
            }
            config.block_syntaxes.insert(
                format!("stress-{syntax_index:02}"),
                crate::config::ResolvedMarkdownBlockSyntax {
                    start,
                    end: "END".to_string(),
                },
            );
        }
        let last = global - 1;
        let source = format!(
            "BEGIN proof-{}done Z{last:03}\nBody.\nEND\n",
            "part-".repeat(80)
        );

        let nodes =
            parse_with_config(source.as_str(), Path::new("max-starts.md"), &config).unwrap();
        assert!(matches!(
            &custom_block(&nodes, "proof").kind,
            NodeKind::MarkdownCustomBlock { title, .. }
                if title.as_deref().is_some_and(|title| title.ends_with("done"))
        ));
    }

    #[test]
    fn literal_start_end_overlap_uses_the_current_top_family() {
        let mut config = ResolvedMarkdownConfig::default();
        config.block_syntaxes.insert(
            "end-owner".to_string(),
            crate::config::ResolvedMarkdownBlockSyntax {
                start: vec!["OUTER {name}".to_string()],
                end: "BEGIN definition".to_string(),
            },
        );
        config.block_syntaxes.insert(
            "start-owner".to_string(),
            crate::config::ResolvedMarkdownBlockSyntax {
                start: vec!["BEGIN {name}".to_string()],
                end: "DONE".to_string(),
            },
        );

        let top_level = parse_with_config(
            "BEGIN definition\nBody.\nDONE\n",
            Path::new("start-end-top.md"),
            &config,
        )
        .unwrap();
        assert!(matches!(
            &custom_block(&top_level, "definition").kind,
            NodeKind::MarkdownCustomBlock { title: None, .. }
        ));

        let nested = parse_with_config(
            "OUTER proof\nBody.\nBEGIN definition\nTail.\nDONE\n",
            Path::new("start-end-nested.md"),
            &config,
        )
        .unwrap();
        let names: Vec<_> = all_nodes(&nested)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["proof"]);
    }

    #[test]
    fn colon_and_literal_overlap_uses_the_current_top_family() {
        let mut config = ResolvedMarkdownConfig::default();
        config.block_syntaxes.insert(
            "literal".to_string(),
            crate::config::ResolvedMarkdownBlockSyntax {
                start: vec!["BEGIN {name}".to_string()],
                end: ":::proof".to_string(),
            },
        );

        let top_level = parse_with_config(
            ":::proof\nBody.\n:::\n",
            Path::new("colon-overlap-top.md"),
            &config,
        )
        .unwrap();
        assert!(matches!(
            &custom_block(&top_level, "proof").kind,
            NodeKind::MarkdownCustomBlock { title: None, .. }
        ));

        let nested = parse_with_config(
            "BEGIN definition\nBody.\n:::proof\nTail.\n:::\n",
            Path::new("colon-overlap-nested.md"),
            &config,
        )
        .unwrap();
        let names: Vec<_> = all_nodes(&nested)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["definition"]);
    }

    #[test]
    fn colon_fences_can_be_disabled_without_disabling_literal_templates() {
        let mut config = config_with_jinja_result_syntax();
        config.colon_fences = false;
        let source = concat!(
            ":::proof Colon proof\ncolon body\n:::\n\n",
            "::: {#thm-colon}\nPandoc body.\n:::\n\n",
            r#"{% call result("definition") %}"#,
            "\ncustom body\n{% endcall %}\n",
        );
        let nodes = parse_with_config(source, Path::new("no-colons.md"), &config).unwrap();
        let blocks: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(blocks, ["definition"]);
        let literal: String = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(literal.contains(":::proof Colon proof"));
        assert!(literal.contains("::: {#thm-colon}"));
    }

    #[test]
    fn literal_and_colon_families_nest_without_cross_closing() {
        let config = config_with_jinja_result_syntax();
        let source = concat!(
            ":::proof Outer\n",
            r#"{% call result("definition", "Inner") %}"#,
            "\n",
            "::: \n",
            "inner tail\n",
            "{% endcall %}\n",
            "outer tail\n",
            ":::\n",
        );
        let nodes = parse_with_config(source, Path::new("mixed-delimiters.md"), &config).unwrap();
        assert_eq!(nodes.len(), 1);
        let proof = custom_block(&nodes, "proof");
        let definition = custom_block(&proof.children, "definition");
        let inner_text: String = all_nodes(&definition.children)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(inner_text.contains(":::"));
        assert!(inner_text.contains("inner tail"));
        let outer_text: String = all_nodes(&proof.children)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(outer_text.contains("outer tail"));
    }

    #[test]
    fn unknown_and_protected_literal_markers_cannot_steal_a_closer() {
        let config = config_with_jinja_result_syntax();
        let source = concat!(
            "---\n",
            "template: '{% call result(\"definition\") %}'\n",
            "ending: '{% endcall %}'\n",
            "---\n\n",
            r#"{% call result("definition") %}"#,
            "\n",
            r#"{% call result("not-configured") %}"#,
            "\nunknown body\n{% endcall %}\n",
            "```jinja\n{% endcall %}\n```\n\n",
            "$$\n{% endcall %}\n$$\n\n",
            "<div>\n{% endcall %}\n</div>\n\n",
            "outer tail\n{% endcall %}\n",
        );
        let nodes = parse_with_config(source, Path::new("protected-literal.md"), &config).unwrap();
        let definition = custom_block(&nodes, "definition");
        assert_eq!(
            all_nodes(&nodes)
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::MarkdownCustomBlock { .. }))
                .count(),
            1
        );
        let literal: String = all_nodes(&definition.children)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                NodeKind::MarkdownRawHtml(html) => Some(html.as_str()),
                _ => None,
            })
            .collect();
        assert!(literal.contains("not-configured"));
        assert!(literal.contains("outer tail"));
        assert!(all_nodes(&definition.children).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownCodeBlock { code, .. } if code.contains("{% endcall %}")
        )));
    }

    #[test]
    fn references_on_literal_template_markers_stay_structural() {
        let config = config_with_jinja_result_syntax();
        let source = concat!(
            "::: {#thm-key}\nTarget.\n:::\n\n",
            r#"{% call result("definition", "See @thm-key") %}"#,
            "\nBody reference @thm-key.\n{% endcall %}\n",
        );
        let nodes = parse_with_config(source, Path::new("marker-reference.md"), &config).unwrap();
        let references: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCrossReference { raw, .. } => Some(raw.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(references, ["@thm-key"]);
        assert!(matches!(
            &custom_block(&nodes, "definition").kind,
            NodeKind::MarkdownCustomBlock { title, .. }
                if title.as_deref() == Some("See @thm-key")
        ));
    }

    #[test]
    fn literal_templates_keep_crlf_unicode_spans_and_decode_quoted_escapes() {
        let config = config_with_jinja_result_arguments();
        let path = Path::new("jinja-unicode.md");
        let source = concat!(
            "Before λ.\r\n\r\n",
            "  {% call result(\"definition\", \"Café \\\"quoted\\\" 😀\\\\path\", \"d-cafe\", card=true) %}  \r\n",
            "Body é.\r\n",
            "  {% endcall %}\t\r\n",
        );
        let nodes = parse_with_config(source, path, &config).unwrap();
        let definition = custom_block(&nodes, "definition");
        assert!(matches!(
            &definition.kind,
            NodeKind::MarkdownCustomBlock {
                title,
                label,
                anchor,
                card,
                ..
            } if title.as_deref() == Some("Café \"quoted\" 😀\\path")
                && label.as_deref() == Some("d-cafe")
                && anchor.as_deref() == Some("d-cafe")
                && *card
        ));
        let opening = source.find("  {% call").unwrap();
        assert_eq!(
            definition.span,
            LineIndex::new(source).span(path, opening, source.len())
        );
        let body_start = source.find("Body").unwrap();
        let body = all_nodes(&definition.children)
            .into_iter()
            .find(|node| matches!(&node.kind, NodeKind::MarkdownText(text) if text == "Body"))
            .expect("body source-sync leaf");
        assert_eq!(body.span.start, LineIndex::new(source).pos(body_start));
    }

    #[test]
    fn bookdown_and_quarto_theorems_share_semantics_but_keep_reference_styles() {
        let source = concat!(
            "Bookdown says Theorem \\@ref(thm:pyth); Quarto says @lem-unique.\n\n",
            "::: {.theorem #pyth name=\"Pythagorean theorem\" data-latex=\"\"}\n",
            "For a right triangle, **boldly** use $a^2+b^2=c^2$.\n",
            ":::\n\n",
            ":::: {#lem-unique}\n",
            "## Unique factorization\n\n",
            "Every integer has the expected factorization.\n",
            "::::\n",
        );
        let nodes = parse(source, Path::new("theorems.md")).unwrap();
        let theorem = custom_block(&nodes, "theorem");
        let lemma = custom_block(&nodes, "lemma");

        assert!(matches!(
            &theorem.kind,
            NodeKind::MarkdownCustomBlock {
                title,
                theorem: Some(MarkdownTheoremMeta {
                    dialect: MarkdownTheoremDialect::Bookdown,
                    prefix,
                    identifier: Some(identifier),
                    number: Some(number),
                    anchor: Some(anchor),
                    title_span: None,
                    numbered: true,
                }),
                ..
            } if title.as_deref() == Some("Pythagorean theorem")
                && prefix == "thm"
                && identifier == "pyth"
                && number == "1"
                && anchor == "thm-pyth"
        ));
        assert!(all_nodes(&theorem.children)
            .iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownStrong)));
        assert!(matches!(
            &lemma.kind,
            NodeKind::MarkdownCustomBlock {
                title,
                theorem: Some(MarkdownTheoremMeta {
                    dialect: MarkdownTheoremDialect::Quarto,
                    prefix,
                    identifier: Some(identifier),
                    number: Some(number),
                    anchor: Some(anchor),
                    title_span: Some(_),
                    numbered: true,
                }),
                ..
            } if title.as_deref() == Some("Unique factorization")
                && prefix == "lem"
                && identifier == "unique"
                && number == "1"
                && anchor == "lem-unique"
        ));
        assert!(!lemma
            .children
            .iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownHeading { .. })));

        let references: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCrossReference {
                    raw,
                    anchor,
                    display,
                    ..
                } => Some((raw.as_str(), anchor.as_deref(), display.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(
            references,
            [
                ("\\@ref(thm:pyth)", Some("thm-pyth"), Some("1")),
                ("@lem-unique", Some("lem-unique"), Some("Lemma 1")),
            ]
        );
    }

    #[test]
    fn theorem_dialects_keep_numbering_and_resolution_rules_distinct() {
        let source = concat!(
            "::: {.remark #book-note name=\"Aside\"}\nBookdown.\n:::\n\n",
            "::: {#rem-quarto-note}\n## Aside\nQuarto.\n:::\n\n",
            "::: {.solution #book-solution}\nBookdown.\n:::\n\n",
            "::: {#sol-quarto-solution name=\"Answer\"}\nQuarto.\n:::\n\n",
            "@rem-quarto-note @sol-quarto-solution ",
            "\\@ref(rem:book-note) \\@ref(sol:book-solution)\n",
        );
        let nodes = parse(source, Path::new("dialects.md")).unwrap();
        let remarks: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock {
                    name,
                    theorem: Some(theorem),
                    ..
                } if name == "remark" || name == "solution" => Some((name.as_str(), theorem)),
                _ => None,
            })
            .collect();
        assert_eq!(remarks.len(), 4);
        assert!(!remarks[0].1.numbered);
        assert_eq!(remarks[1].1.number.as_deref(), Some("1"));
        assert!(!remarks[2].1.numbered);
        assert_eq!(remarks[3].1.number.as_deref(), Some("1"));

        let references: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCrossReference { raw, display, .. } => {
                    Some((raw.as_str(), display.as_deref()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            references,
            [
                ("@rem-quarto-note", Some("Remark 1")),
                ("@sol-quarto-solution", Some("Solution 1")),
            ],
            "Bookdown never recognizes remark/solution references"
        );
    }

    #[test]
    fn theorem_counters_are_per_kind_and_shared_across_surface_syntaxes() {
        let source = concat!(
            "::: {.theorem #book-a}\nBook A.\n:::\n\n",
            "::: {#thm-quarto-a}\nQuarto A.\n:::\n\n",
            "::: {.lemma #book-lemma}\nLemma.\n:::\n\n",
            "::: {.theorem #book-b}\nBook B.\n:::\n\n",
            "\\@ref(thm:book-a) @thm-quarto-a ",
            "\\@ref(thm:quarto-a) @thm-book-a\n",
        );
        let nodes = parse(source, Path::new("mixed-counters.md")).unwrap();
        let numbered: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock {
                    theorem: Some(theorem),
                    ..
                } if theorem.numbered => Some((theorem.prefix.as_str(), theorem.number.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(
            numbered,
            [
                ("thm", Some("1")),
                ("thm", Some("2")),
                ("lem", Some("1")),
                ("thm", Some("3")),
            ]
        );

        let references: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCrossReference { raw, display, .. } => {
                    Some((raw.as_str(), display.as_deref()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            references,
            [
                ("\\@ref(thm:book-a)", Some("1")),
                ("@thm-quarto-a", Some("Theorem 2")),
                ("\\@ref(thm:quarto-a)", None),
                ("@thm-book-a", None),
            ],
            "targets never acquire aliases in the other dialect"
        );
    }

    #[test]
    fn duplicate_theorem_identifiers_get_unique_anchors_and_first_target_wins() {
        let source = concat!(
            "::: {.theorem #duplicate}\nFirst.\n:::\n\n",
            "::: {.theorem #duplicate}\nSecond.\n:::\n\n",
            "See \\@ref(thm:duplicate).\n",
        );
        let nodes = parse(source, Path::new("duplicate-targets.md")).unwrap();
        let targets: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock {
                    theorem: Some(theorem),
                    ..
                } => Some((theorem.number.as_deref(), theorem.anchor.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(
            targets,
            [
                (Some("1"), Some("thm-duplicate")),
                (Some("2"), Some("thm-duplicate-1")),
            ]
        );
        let reference = all_nodes(&nodes)
            .into_iter()
            .find_map(|node| match &node.kind {
                NodeKind::MarkdownCrossReference {
                    anchor, display, ..
                } => Some((anchor.as_deref(), display.as_deref())),
                _ => None,
            })
            .expect("reference node");
        assert_eq!(reference, (Some("thm-duplicate"), Some("1")));
    }

    #[test]
    fn algorithm_is_quarto_only_and_ambiguous_or_invalid_bookdown_divs_stay_literal() {
        let source = concat!(
            "::: {.algorithm #plain-alg}\nGeneric algorithm.\n:::\n\n",
            "::: {#alg-quarto}\nSemantic algorithm.\n:::\n\n",
            "::: {.theorem .lemma #ambiguous}\nAmbiguous.\n:::\n\n",
            "::: {.theorem #invalid!}\nInvalid identifier.\n:::\n\n",
            "\\@ref(alg:plain-alg) @alg-quarto\n",
        );
        let nodes = parse(source, Path::new("algorithm.md")).unwrap();
        let algorithms: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock { name, theorem, .. } if name == "algorithm" => {
                    Some(theorem.as_ref())
                }
                _ => None,
            })
            .collect();
        assert_eq!(algorithms.len(), 2);
        assert!(algorithms[0].is_none());
        assert!(matches!(
            algorithms[1],
            Some(MarkdownTheoremMeta {
                dialect: MarkdownTheoremDialect::Quarto,
                number: Some(number),
                ..
            }) if number == "1"
        ));
        assert_eq!(
            all_nodes(&nodes)
                .into_iter()
                .filter(|node| matches!(node.kind, NodeKind::MarkdownCrossReference { .. }))
                .count(),
            1,
            "Bookdown has no alg: reference namespace"
        );
        let literal: String = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(literal.contains(".theorem .lemma #ambiguous"));
        assert!(literal.contains(".theorem #invalid!"));
        assert!(literal.contains("@ref(alg:plain-alg)"));
    }

    #[test]
    fn every_theorem_kind_has_the_expected_bookdown_and_quarto_contract() {
        let expected = [
            ("theorem", "thm", true, true, true),
            ("lemma", "lem", true, true, true),
            ("corollary", "cor", true, true, true),
            ("proposition", "prp", true, true, true),
            ("conjecture", "cnj", true, true, true),
            ("definition", "def", true, true, true),
            ("example", "exm", true, true, true),
            ("exercise", "exr", true, true, true),
            ("hypothesis", "hyp", true, true, false),
            ("solution", "sol", true, false, true),
            ("remark", "rem", true, false, true),
            ("algorithm", "alg", false, false, true),
            ("proof", "", true, false, false),
        ];
        let actual: Vec<_> = MARKDOWN_THEOREM_SPECS
            .iter()
            .map(|spec| {
                (
                    spec.name,
                    spec.prefix,
                    spec.bookdown,
                    spec.bookdown_numbered,
                    spec.quarto,
                )
            })
            .collect();
        assert_eq!(actual, expected);

        for (name, prefix, bookdown, bookdown_numbered, quarto) in expected {
            let bookdown_source = format!("::: {{.{name} #book-key}}\nBody.\n:::\n");
            let bookdown_nodes = parse(&bookdown_source, Path::new("bookdown-kinds.md")).unwrap();
            let bookdown_block = custom_block(&bookdown_nodes, name);
            match &bookdown_block.kind {
                NodeKind::MarkdownCustomBlock {
                    theorem: Some(theorem),
                    ..
                } if bookdown => {
                    assert_eq!(theorem.dialect, MarkdownTheoremDialect::Bookdown);
                    assert_eq!(theorem.prefix, prefix);
                    assert_eq!(theorem.numbered, bookdown_numbered);
                }
                NodeKind::MarkdownCustomBlock { theorem: None, .. } if !bookdown => {}
                other => panic!("unexpected Bookdown contract for {name}: {other:#?}"),
            }

            if quarto {
                let quarto_source = format!("::: {{#{prefix}-quarto-key}}\nBody.\n:::\n");
                let quarto_nodes = parse(&quarto_source, Path::new("quarto-kinds.md")).unwrap();
                let quarto_block = custom_block(&quarto_nodes, name);
                assert!(matches!(
                    &quarto_block.kind,
                    NodeKind::MarkdownCustomBlock {
                        theorem: Some(MarkdownTheoremMeta {
                            dialect: MarkdownTheoremDialect::Quarto,
                            prefix: found_prefix,
                            numbered: true,
                            ..
                        }),
                        ..
                    } if found_prefix == prefix
                ));
            }
        }
    }

    #[test]
    fn adjacent_theorem_references_keep_exact_unicode_byte_spans() {
        let source = concat!(
            "::: {.theorem #book}\nBook.\n:::\n\n",
            "::: {#thm-quarto}\nQuarto.\n:::\n\n",
            "λ: \\@ref(thm:book),—@thm-quarto.\n",
        );
        let path = Path::new("λ-theorems.md");
        let nodes = parse(source, path).unwrap();
        let references: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter(|node| matches!(node.kind, NodeKind::MarkdownCrossReference { .. }))
            .collect();
        assert_eq!(references.len(), 2);
        for node in references {
            let NodeKind::MarkdownCrossReference { raw, .. } = &node.kind else {
                unreachable!()
            };
            let start = node.span.start.byte as usize;
            let end = node.span.end.byte as usize;
            assert_eq!(&source[start..end], raw);
            assert_eq!(node.span, LineIndex::new(source).span(path, start, end));
        }
    }

    #[test]
    fn overlong_unclosed_bookdown_reference_with_unicode_stays_literal() {
        let source = format!("Before \\@ref(thm:{} after.\n", "€".repeat(100));
        let nodes = parse(&source, Path::new("unicode-reference.md")).unwrap();
        assert!(!all_nodes(&nodes)
            .into_iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownCrossReference { .. })));
        let visible: String = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(visible.contains("@ref(thm:"));
        assert!(visible.contains('€'));
    }

    #[test]
    fn excessive_inline_html_nesting_disables_extensions_conservatively() {
        let source = format!(
            "{}@thm-hidden\n",
            "<span>".repeat(MAX_MARKDOWN_INLINE_HTML_NESTING + 1)
        );
        let mut protected = vec![false; source.len()];
        protect_markdown_literal_ranges(&source, markdown_options(), &mut protected);
        let reference_start = source.find("@thm-hidden").unwrap();
        assert!(protected[reference_start]);

        let nodes = parse(&source, Path::new("deep-inline-html.md")).unwrap();
        assert!(!all_nodes(&nodes)
            .into_iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownCrossReference { .. })));
    }

    #[test]
    fn pandoc_fences_nest_with_variable_lengths_and_unknown_divs_stay_literal() {
        let source = concat!(
            ":::: {#thm-outer} ::::::\n",
            "Outer body.\n\n",
            "::: {.unknown style=\"color:red\"}\n",
            "Unknown body.\n",
            "::::::\n\n",
            "::: {.proof}\n",
            "Known proof.\n",
            ":::::\n",
            ":::\n",
        );
        let nodes = parse(source, Path::new("nested-pandoc.md")).unwrap();
        let theorem = custom_block(&nodes, "theorem");
        let proof = custom_block(&theorem.children, "proof");
        assert!(matches!(
            &proof.kind,
            NodeKind::MarkdownCustomBlock {
                theorem: Some(MarkdownTheoremMeta {
                    numbered: false,
                    ..
                }),
                ..
            }
        ));
        let text: String = all_nodes(&theorem.children)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains("::: {.unknown style=\"color:red\"}"));
        assert!(text.contains("Unknown body."));
    }

    #[test]
    fn theorem_references_are_inert_in_literals_links_and_leading_yaml() {
        let source = concat!(
            "---\n",
            "title: '@thm-yaml'\n",
            "note: '::: {#thm-yaml}'\n",
            "---\n\n",
            "`@thm-code` [@thm-link](#thm-real) <b>@thm-html</b>\n\n",
            "$$@thm-math$$\n\n",
            "::: {#thm-real}\nReal.\n:::\n\n",
            "@thm-real and mail@thm-real.test\n",
        );
        let nodes = parse(source, Path::new("inert-references.md")).unwrap();
        let references: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCrossReference { raw, .. } => Some(raw.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(references, ["@thm-real"]);
    }

    #[test]
    fn theorem_reference_scanning_preserves_footnote_and_link_metadata() {
        let source = concat!(
            "::: {#thm-key}\nTarget.\n:::\n\n",
            "Use[^@thm-key] and [external][doc].\n\n",
            "[^@thm-key]: The note may still cite @thm-key in its body.\n\n",
            "[doc]: https://example.test/@thm-key\n",
        );
        let nodes = parse(source, Path::new("reference-metadata.md")).unwrap();

        let footnote_reference = all_nodes(&nodes)
            .into_iter()
            .find_map(|node| match &node.kind {
                NodeKind::MarkdownFootnoteReference { label, target } => {
                    Some((label.as_str(), target.as_str()))
                }
                _ => None,
            })
            .expect("footnote reference");
        let footnote_definition = all_nodes(&nodes)
            .into_iter()
            .find_map(|node| match &node.kind {
                NodeKind::MarkdownFootnoteDefinition { label, target } => {
                    Some((label.as_str(), target.as_str()))
                }
                _ => None,
            })
            .expect("footnote definition");
        assert_eq!(footnote_reference.0, "@thm-key");
        assert_eq!(footnote_definition.0, "@thm-key");
        assert!(!footnote_reference.1.is_empty());
        assert_eq!(footnote_reference.1, footnote_definition.1);

        let destination = all_nodes(&nodes)
            .into_iter()
            .find_map(|node| match &node.kind {
                NodeKind::MarkdownLink { destination, .. } => Some(destination.as_str()),
                _ => None,
            })
            .expect("reference link");
        assert_eq!(destination, "https://example.test/@thm-key");

        let references: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCrossReference { raw, display, .. } => {
                    Some((raw.as_str(), display.as_deref()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(references, [("@thm-key", Some("Theorem 1"))]);
    }

    #[test]
    fn theorem_references_in_unknown_or_malformed_fence_metadata_stay_literal() {
        let source = concat!(
            "::: {#thm-key}\nTarget.\n:::\n\n",
            "::: {.unknown title=\"@thm-key\"}\nUnknown body.\n:::\n\n",
            "::: {.theorem #bad! title=\"@thm-key\"}\nMalformed body.\n:::\n",
        );
        let nodes = parse(source, Path::new("literal-fence-metadata.md")).unwrap();
        assert!(!all_nodes(&nodes)
            .into_iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownCrossReference { .. })));
        let literal: String = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(literal.matches("@thm-key").count(), 2);
    }

    #[test]
    fn parse_with_config_recognizes_only_effective_block_names() {
        let mut config = config_with_block("warning");
        config.blocks.remove("proof");
        let source = concat!(
            ":::proof Disabled\nproof body\n:::\n\n",
            ":::warning Take care\nwarning body\n:::\n",
        );
        let nodes = parse_with_config(source, Path::new("configured.md"), &config).unwrap();
        let blocks: Vec<_> = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock { name, title, .. } => {
                    Some((name.as_str(), title.as_deref()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(blocks, [("warning", Some("Take care"))]);
        let text: String = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains(":::proof Disabled"));
    }

    #[test]
    fn custom_blocks_nest_without_losing_markdown_structure() {
        let config = config_with_block("note");
        let source = concat!(
            ":::proof Outer\n",
            "Outer paragraph.\n\n",
            ":::note Inner\n",
            "# Nested heading\n",
            ":::\n\n",
            "Tail paragraph.\n",
            ":::\n",
        );
        let nodes = parse_with_config(source, Path::new("nested.md"), &config).unwrap();
        assert_eq!(nodes.len(), 1);
        let outer = &nodes[0];
        assert!(matches!(
            &outer.kind,
            NodeKind::MarkdownCustomBlock { name, title, .. }
                if name == "proof" && title.as_deref() == Some("Outer")
        ));
        let inner = custom_block(&outer.children, "note");
        assert!(matches!(
            &inner.kind,
            NodeKind::MarkdownCustomBlock { title, .. }
                if title.as_deref() == Some("Inner")
        ));
        assert!(all_nodes(&inner.children).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownHeading { level: 1, anchor } if anchor == "nested-heading"
        )));
        assert_eq!(outer.span.start, pos(1, 1, 0));
        assert_eq!(outer.span.end, LineIndex::new(source).pos(source.len()));
        assert_eq!(inner.span.start.line, 4);
        assert_eq!(inner.span.end.line, 7);
    }

    #[test]
    fn empty_custom_blocks_keep_their_source_order_and_can_nest() {
        let config = config_with_block("note");
        let source = concat!(
            "before\n\n",
            ":::proof Empty\n:::\n\n",
            ":::proof Outer\n",
            ":::note Empty inner\n:::\n",
            ":::\n\n",
            "after\n",
        );
        let nodes = parse_with_config(source, Path::new("empty.md"), &config).unwrap();
        let top_level: Vec<_> = nodes
            .iter()
            .map(|node| match &node.kind {
                NodeKind::MarkdownCustomBlock { title, .. } => title.as_deref().unwrap_or("custom"),
                NodeKind::MarkdownParagraph => "paragraph",
                other => panic!("unexpected top-level node {other:?}"),
            })
            .collect();
        assert_eq!(top_level, ["paragraph", "Empty", "Outer", "paragraph"]);

        let empty = nodes
            .iter()
            .find(|node| {
                matches!(
                    &node.kind,
                    NodeKind::MarkdownCustomBlock { title, .. }
                        if title.as_deref() == Some("Empty")
                )
            })
            .expect("empty top-level block");
        assert!(empty.children.is_empty());

        let outer = nodes
            .iter()
            .find(|node| {
                matches!(
                    &node.kind,
                    NodeKind::MarkdownCustomBlock { title, .. }
                        if title.as_deref() == Some("Outer")
                )
            })
            .expect("outer block");
        assert_eq!(outer.children.len(), 1);
        assert!(matches!(
            &outer.children[0].kind,
            NodeKind::MarkdownCustomBlock { name, title, .. }
                if name == "note" && title.as_deref() == Some("Empty inner")
        ));
        assert!(outer.children[0].children.is_empty());
    }

    #[test]
    fn only_matched_custom_markers_are_masked() {
        let source = concat!(
            ":::unknown Literal\nunknown body\n:::\n\n",
            ":::proof Unclosed\nouter body\n\n",
            ":::proof Matched\ninner body\n:::\n",
        );
        let config = ResolvedMarkdownConfig::default();
        let matched = find_markdown_custom_blocks(source, markdown_options(), &config);
        assert_eq!(matched.blocks.len(), 1);
        assert_eq!(matched.blocks[0].title.as_deref(), Some("Matched"));

        let masked = mask_markdown_custom_block_markers(source, &matched.blocks);
        assert!(masked.contains(":::unknown Literal"));
        assert!(masked.contains(":::proof Unclosed"));
        assert!(!masked.contains(":::proof Matched"));

        let nodes = parse_with_config(source, Path::new("literal.md"), &config).unwrap();
        assert_eq!(
            all_nodes(&nodes)
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::MarkdownCustomBlock { .. }))
                .count(),
            1
        );
        let text: String = all_nodes(&nodes)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains(":::unknown Literal"));
        assert!(text.contains(":::proof Unclosed"));
    }

    #[test]
    fn custom_markers_are_inert_in_code_math_and_raw_html() {
        let source = concat!(
            "```markdown\n:::proof Fenced\nbody\n:::\n```\n\n",
            "    :::proof Indented\n    body\n    :::\n\n",
            "`code across\n:::proof Inline\nbody\n:::\nlines`\n\n",
            "$$\n:::proof Math\nbody\n:::\n$$\n\n",
            "\\[\n:::proof Slash display\nbody\n:::\n\\]\n\n",
            "\\(\n:::proof Slash inline\nbody\n:::\n\\)\n\n",
            "<div>\n:::proof HTML\nbody\n:::\n</div>\n",
        );
        let nodes = parse(source, Path::new("inert.md")).unwrap();
        assert!(!all_nodes(&nodes)
            .iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownCustomBlock { .. })));
        assert!(all_nodes(&nodes).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownCodeBlock { code, .. } if code.contains(":::proof Fenced")
        )));
        assert!(all_nodes(&nodes).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownRawHtml(html) if html.contains(":::proof HTML")
        )));
    }

    #[test]
    fn protected_closing_markers_do_not_end_an_enclosing_custom_block() {
        let source = concat!(
            ":::proof Outer\n",
            "```markdown\n:::\n```\n\n",
            "<div>\n:::\n</div>\n\n",
            "tail\n",
            ":::\n",
        );
        let nodes = parse(source, Path::new("protected-close.md")).unwrap();
        assert_eq!(nodes.len(), 1);
        let proof = custom_block(&nodes, "proof");
        assert_eq!(proof.span.end, LineIndex::new(source).pos(source.len()));
        assert!(all_nodes(&proof.children).iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownCodeBlock { code, .. } if code.contains(":::")
        )));
        assert!(all_nodes(&proof.children)
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::MarkdownText(text) if text == "tail")));
    }

    #[test]
    fn custom_markers_allow_at_most_three_leading_spaces() {
        for indent in 0..=3 {
            let spaces = " ".repeat(indent);
            let source = format!("{spaces}:::proof\nbody\n{spaces}:::\n");
            let nodes = parse(&source, Path::new("indent.md")).unwrap();
            assert!(
                all_nodes(&nodes)
                    .iter()
                    .any(|node| matches!(node.kind, NodeKind::MarkdownCustomBlock { .. })),
                "{indent} leading spaces should be recognized"
            );
        }

        let source = "    :::proof\n    body\n    :::\n";
        let nodes = parse(source, Path::new("indent.md")).unwrap();
        assert!(!all_nodes(&nodes)
            .iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownCustomBlock { .. })));
    }

    #[test]
    fn line_level_custom_blocks_can_live_inside_a_continuing_list_item() {
        let source = concat!(
            "- before\n\n",
            "  :::proof Listed\n",
            "  body with **bold**\n",
            "  :::\n\n",
            "  after\n",
        );
        let nodes = parse(source, Path::new("list-block.md")).unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(matches!(nodes[0].kind, NodeKind::MarkdownList { .. }));
        let item = all_nodes(&nodes)
            .into_iter()
            .find(|node| matches!(node.kind, NodeKind::MarkdownListItem))
            .expect("list item");
        let proof = custom_block(&item.children, "proof");
        assert!(matches!(
            &proof.kind,
            NodeKind::MarkdownCustomBlock { title, .. }
                if title.as_deref() == Some("Listed")
        ));
        assert!(all_nodes(&proof.children)
            .iter()
            .any(|node| matches!(node.kind, NodeKind::MarkdownStrong)));
        let item_text: String = all_nodes(&item.children)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(item_text.contains("before"));
        assert!(item_text.contains("after"));
    }

    #[test]
    fn custom_blocks_keep_crlf_unicode_offsets_exact() {
        let path = Path::new("unicode-block.md");
        let source = " :::proof Café 😀\r\nBody é.\r\n :::\r\n";
        let nodes = parse(source, path).unwrap();
        let proof = custom_block(&nodes, "proof");
        assert!(matches!(
            &proof.kind,
            NodeKind::MarkdownCustomBlock { title, .. }
                if title.as_deref() == Some("Café 😀")
        ));
        assert_eq!(
            proof.span,
            LineIndex::new(source).span(path, 0, source.len())
        );

        let body_start = source.find("Body").unwrap();
        let body = all_nodes(&proof.children)
            .into_iter()
            .find(|node| matches!(&node.kind, NodeKind::MarkdownText(text) if text == "Body"))
            .expect("body sync leaf");
        assert_eq!(body.span.start, LineIndex::new(source).pos(body_start));
        assert_eq!(body.span.end, LineIndex::new(source).pos(body_start + 4));
    }

    #[test]
    fn document_wide_heading_and_footnote_passes_cross_custom_blocks() {
        let source = concat!(
            "[jump](#inside) and outside[^note].\n\n",
            ":::proof\n",
            "# Inside\n\n",
            "Inside reference[^note].\n",
            ":::\n\n",
            "[^note]: Shared definition.\n",
        );
        let nodes = parse(source, Path::new("global.md")).unwrap();
        let flat = all_nodes(&nodes);
        assert!(flat.iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownLink { destination, .. } if destination == "#mdh:inside"
        )));
        assert!(flat.iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownHeading { anchor, .. } if anchor == "inside"
        )));
        let targets: Vec<_> = flat
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownFootnoteReference { target, .. } => Some(target.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(targets, ["note", "note"]);
        assert!(flat.iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownFootnoteDefinition { target, .. } if target == "note"
        )));
    }

    #[test]
    fn many_sequential_custom_blocks_do_not_regress_to_quadratic_wrapping() {
        let source = ":::proof\nbody\n:::\n\n".repeat(8_000);
        let started = std::time::Instant::now();
        let nodes = parse(&source, Path::new("many-blocks.md")).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(nodes.len(), 8_000);
        assert!(nodes.iter().all(|node| matches!(
            node.kind,
            NodeKind::MarkdownCustomBlock { ref name, .. } if name == "proof"
        )));
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "custom-block integration regressed from near-linear behavior: {elapsed:?}"
        );
    }

    #[test]
    fn excessive_custom_block_nesting_stays_bounded_and_literal() {
        let depth = 4_096;
        let mut source = String::with_capacity(depth * 20);
        for _ in 0..depth {
            source.push_str(":::proof\n");
        }
        source.push_str("Deep body.\n");
        for _ in 0..depth {
            source.push_str(":::\n");
        }

        let nodes = parse(&source, Path::new("deep-blocks.md")).unwrap();
        let flat = all_nodes(&nodes);
        assert_eq!(
            flat.iter()
                .filter(|node| matches!(node.kind, NodeKind::MarkdownCustomBlock { .. }))
                .count(),
            MAX_MARKDOWN_CUSTOM_BLOCK_NESTING,
        );
        assert!(flat.iter().any(
            |node| matches!(&node.kind, NodeKind::MarkdownText(text) if text.contains(":::proof"))
        ));

        let rendered = crate::render_document_from_source(
            Path::new("deep-blocks.md"),
            source,
            &crate::HtmlOptions::default(),
        )
        .unwrap();
        assert_eq!(
            rendered
                .body_html
                .matches(r#"data-md-custom-name="proof""#)
                .count(),
            MAX_MARKDOWN_CUSTOM_BLOCK_NESTING,
        );
    }

    #[test]
    fn literal_overflow_cannot_be_unwound_by_colon_closers() {
        let config = config_with_jinja_result_syntax();
        let mut source = ":::proof\n".repeat(MAX_MARKDOWN_CUSTOM_BLOCK_NESTING);
        source.push_str(r#"{% call result("definition") %}"#);
        source.push_str("\nDeep body.\n");
        for _ in 0..=MAX_MARKDOWN_CUSTOM_BLOCK_NESTING {
            source.push_str(":::\n");
        }

        let nodes = parse_with_config(&source, Path::new("literal-overflow.md"), &config).unwrap();
        assert_eq!(
            all_nodes(&nodes)
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::MarkdownCustomBlock { .. }))
                .count(),
            0,
            "colon closers must not release an unclosed literal overflow frame",
        );
    }

    #[test]
    fn colon_overflow_cannot_be_unwound_by_literal_closers() {
        let config = config_with_jinja_result_syntax();
        let mut source = String::new();
        for _ in 0..MAX_MARKDOWN_CUSTOM_BLOCK_NESTING {
            source.push_str(r#"{% call result("definition") %}"#);
            source.push('\n');
        }
        source.push_str(":::proof\nDeep body.\n");
        for _ in 0..=MAX_MARKDOWN_CUSTOM_BLOCK_NESTING {
            source.push_str("{% endcall %}\n");
        }

        let nodes = parse_with_config(&source, Path::new("colon-overflow.md"), &config).unwrap();
        assert_eq!(
            all_nodes(&nodes)
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::MarkdownCustomBlock { .. }))
                .count(),
            0,
            "literal closers must not release an unclosed colon overflow frame",
        );
    }

    #[test]
    fn alternating_overflow_families_poison_extension_parsing_without_growing_state() {
        let config = config_with_jinja_result_syntax();
        let depth = 4_096;
        let mut source = ":::proof\n".repeat(MAX_MARKDOWN_CUSTOM_BLOCK_NESTING);
        for index in 0..depth {
            if index % 2 == 0 {
                source.push_str(r#"{% call result("definition") %}"#);
                source.push('\n');
            } else {
                source.push_str(":::proof\n");
            }
        }
        source.push_str("Deep body.\n");
        for index in 0..depth {
            if index % 2 == 0 {
                source.push_str("{% endcall %}\n");
            } else {
                source.push_str(":::\n");
            }
        }
        for _ in 0..MAX_MARKDOWN_CUSTOM_BLOCK_NESTING {
            source.push_str(":::\n");
        }

        let scan = find_markdown_custom_blocks(&source, markdown_options(), &config);
        assert!(
            scan.blocks.is_empty(),
            "ambiguous overflow must keep all tracked outer frames literal",
        );
    }

    #[test]
    fn parses_all_math_delimiters_but_not_code() {
        let source = concat!(
            "Dollar $a+b$ and slash \\(c+d\\).\n\n",
            "$$e+f$$\n\n\\[g+h\\]\n\n",
            "`$not$ \\(math\\)`\n\n",
            "```tex\n$$still_code$$\n\\[also_code\\]\n```\n",
        );
        let nodes = parse(source, Path::new("paper.md")).unwrap();
        let flat = all_nodes(&nodes);
        let inline: Vec<_> = flat
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::InlineMath(body) => Some(body.as_str()),
                _ => None,
            })
            .collect();
        let display: Vec<_> = flat
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::DisplayMath { body, .. } => Some(body.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(inline, ["a+b", "c+d"]);
        assert_eq!(display, ["e+f", "g+h"]);
        assert!(flat.iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownInlineCode(code) if code.contains("$not$")
        )));
        assert!(flat.iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownCodeBlock { code, .. }
                if code.contains("$$still_code$$") && code.contains("\\[also_code\\]")
        )));
    }

    #[test]
    fn slash_math_prepass_leaves_metadata_autolinks_and_html_unchanged() {
        let source = concat!(
            "[link](https://example.com/\\(part\\) \"title \\(literal\\)\")\n\n",
            "![image](figures/\\(plot\\).png \"image \\[title\\]\")\n\n",
            "<https://example.com/\\(auto\\)>\n\n",
            "<span data-math=\"\\(attribute\\)\">raw</span>\n\n",
            "<div>\n\\[html block literal\\]\n</div>\n",
        );
        let (transformed, overrides) = protect_tex_math_delimiters(source, markdown_options());
        assert_eq!(transformed, source);
        assert!(overrides.is_empty());
    }

    #[test]
    fn slash_math_in_link_label_does_not_rewrite_destination_or_title() {
        let source = "[value \\(x+1\\)](notes/\\(raw\\).md \"title \\[literal\\]\")\n";
        let (transformed, overrides) = protect_tex_math_delimiters(source, markdown_options());
        assert_eq!(
            transformed,
            "[value ${x+1}$](notes/\\(raw\\).md \"title \\[literal\\]\")\n"
        );
        assert_eq!(overrides.len(), 1);

        let nodes = parse(source, Path::new("link.md")).unwrap();
        let flat = all_nodes(&nodes);
        assert!(flat.iter().any(|node| matches!(
            &node.kind,
            NodeKind::MarkdownLink { destination, title }
                if destination == "notes/(raw).md"
                    && title.as_deref() == Some("title [literal]")
        )));
        assert_eq!(
            flat.iter()
                .filter_map(|node| match &node.kind {
                    NodeKind::InlineMath(body) => Some(body.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            ["x+1"]
        );
    }

    #[test]
    fn slash_math_prepass_never_pairs_across_paragraphs() {
        let source = "before \\(alpha\n\nafter omega\\)\n";
        let (transformed, overrides) = protect_tex_math_delimiters(source, markdown_options());
        assert_eq!(transformed, source);
        assert!(overrides.is_empty());

        let nodes = parse(source, Path::new("paragraphs.md")).unwrap();
        assert!(!all_nodes(&nodes)
            .iter()
            .any(|node| matches!(node.kind, NodeKind::InlineMath(_))));
    }

    #[test]
    fn slash_math_keeps_tex_punctuation_and_exact_source_offsets() {
        let source = concat!(
            "Prose \\(f(x)=x^2,\\; \\text{if } x\\le 1!\\) done.\n\n",
            "\\[A_{ij}:=\\{x_i,x_j\\}.\\]\n",
        );
        let inline_start = source.find("\\(").unwrap();
        let inline_end = source.find("\\) done").unwrap() + 2;
        let display_start = source.find("\\[").unwrap();
        let display_end = source.rfind("\\]").unwrap() + 2;
        let nodes = parse(source, Path::new("punctuation.md")).unwrap();
        let flat = all_nodes(&nodes);
        let inline = flat
            .iter()
            .find(|node| matches!(node.kind, NodeKind::InlineMath(_)))
            .unwrap();
        let display = flat
            .iter()
            .find(|node| matches!(node.kind, NodeKind::DisplayMath { .. }))
            .unwrap();

        assert!(matches!(
            &inline.kind,
            NodeKind::InlineMath(body) if body == r"f(x)=x^2,\; \text{if } x\le 1!"
        ));
        assert!(matches!(
            &display.kind,
            NodeKind::DisplayMath { body, .. } if body == r"A_{ij}:=\{x_i,x_j\}."
        ));
        assert_eq!(inline.span.start.byte, inline_start as u32);
        assert_eq!(inline.span.end.byte, inline_end as u32);
        assert_eq!(display.span.start.byte, display_start as u32);
        assert_eq!(display.span.end.byte, display_end as u32);
    }

    #[test]
    fn unicode_source_positions_use_neovim_byte_columns() {
        let nodes = parse("é **bold**\n", Path::new("unicode.md")).unwrap();
        let flat = all_nodes(&nodes);
        let bold = flat
            .iter()
            .find(|node| matches!(node.kind, NodeKind::MarkdownStrong))
            .unwrap();
        let text = flat
            .iter()
            .find(|node| matches!(&node.kind, NodeKind::MarkdownText(text) if text == "bold"))
            .unwrap();

        assert_eq!(bold.span.start, pos(1, 4, 3));
        assert_eq!(bold.span.end, pos(1, 12, 11));
        assert_eq!(text.span.start, pos(1, 6, 5));
        assert_eq!(text.span.end, pos(1, 10, 9));
        assert_eq!(bold.span.file, Path::new("unicode.md"));
    }

    #[test]
    fn rendered_sync_lookup_uses_neovim_byte_columns_after_unicode() {
        let out = crate::render_document_from_source(
            Path::new("unicode.md"),
            "😀 **bold**\n".to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        let text = out
            .sync
            .entries
            .iter()
            .find(|entry| entry.start == pos(1, 8, 7) && entry.end == pos(1, 12, 11))
            .expect("rendered bold text should retain its byte-column span");
        assert_eq!(
            out.sync
                // Column 11 is the `d` in `bold`: the four-byte emoji makes
                // this distinguish byte columns from Unicode-scalar columns.
                .lookup_leaf_by_source_position(Path::new("unicode.md"), 1, 11)
                .map(|entry| entry.element_id.as_str()),
            Some(text.element_id.as_str())
        );
    }

    #[test]
    fn markdown_words_are_distinct_sync_leaves() {
        let path = Path::new("words.md");
        let out = crate::render_document_from_source(
            path,
            "alpha beta gamma\n\n😀 alpha beta\n".to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        let alpha = out
            .sync
            .lookup_leaf_by_source_position(path, 1, 2)
            .expect("alpha leaf");
        let beta = out
            .sync
            .lookup_leaf_by_source_position(path, 1, 8)
            .expect("beta leaf");
        let gamma = out
            .sync
            .lookup_leaf_by_source_position(path, 1, 13)
            .expect("gamma leaf");
        assert_ne!(alpha.element_id, beta.element_id);
        assert_ne!(beta.element_id, gamma.element_id);
        assert_eq!(
            (alpha.start.col, beta.start.col, gamma.start.col),
            (1, 7, 12)
        );

        let unicode_alpha = out
            .sync
            .lookup_leaf_by_source_position(path, 3, 7)
            .expect("alpha after emoji");
        let unicode_beta = out
            .sync
            .lookup_leaf_by_source_position(path, 3, 13)
            .expect("beta after emoji");
        assert_eq!((unicode_alpha.start.col, unicode_beta.start.col), (6, 12));
    }

    #[test]
    fn markdown_entity_and_escape_mapping_keeps_original_byte_columns() {
        let path = Path::new("entities.md");
        let source = "a &amp; beta and \\* gamma\n";
        let out = crate::render_document_from_source(
            path,
            source.to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        for word in ["beta", "gamma"] {
            let byte = source.find(word).unwrap();
            let col = byte as u32 + 1;
            let entry = out
                .sync
                .lookup_leaf_by_source_position(path, 1, col)
                .unwrap_or_else(|| panic!("missing {word} sync leaf"));
            assert_eq!(entry.start.col, col, "wrong source start for {word}");
        }
    }

    #[test]
    fn markdown_code_content_has_line_and_token_sync_leaves() {
        let path = Path::new("code.md");
        let source = concat!(
            "```rust\n",
            "let alpha = 1;\n",
            "let beta = 2;\n",
            "```\n\n",
            "    indented alpha\n",
            "    indented beta\n",
        );
        let out = crate::render_document_from_source(
            path,
            source.to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        let fenced_alpha = out
            .sync
            .lookup_leaf_by_source_position(path, 2, 6)
            .expect("fenced alpha leaf");
        let fenced_beta = out
            .sync
            .lookup_leaf_by_source_position(path, 3, 6)
            .expect("fenced beta leaf");
        assert_ne!(fenced_alpha.element_id, fenced_beta.element_id);
        assert_eq!((fenced_alpha.start.line, fenced_alpha.start.col), (2, 5));
        assert_eq!((fenced_beta.start.line, fenced_beta.start.col), (3, 5));

        let indented_alpha = out
            .sync
            .lookup_leaf_by_source_position(path, 6, 14)
            .expect("indented alpha leaf");
        let indented_beta = out
            .sync
            .lookup_leaf_by_source_position(path, 7, 14)
            .expect("indented beta leaf");
        assert_eq!(
            (indented_alpha.start.line, indented_alpha.start.col),
            (6, 14)
        );
        assert_eq!((indented_beta.start.line, indented_beta.start.col), (7, 14));
        assert_ne!(indented_alpha.element_id, indented_beta.element_id);
        assert!(out.body_html.contains("md-code-block"));
        assert!(out.body_html.contains("src-word md-text"));
    }

    #[test]
    fn markdown_structural_wrappers_do_not_duplicate_selection_leaves() {
        let path = Path::new("selection.md");
        let source = "# Alpha Beta\n\n**bold words** and [link words](#target)\n";
        let out = crate::render_document_from_source(
            path,
            source.to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        let heading = out.sync.leaves_in_range(path, 1, 9, 1, 12);
        assert_eq!(heading.len(), 1, "heading ancestors leaked: {heading:?}");
        let bold = out.sync.leaves_in_range(path, 3, 3, 3, 6);
        assert_eq!(bold.len(), 1, "strong ancestors leaked: {bold:?}");
        let link = out.sync.leaves_in_range(path, 3, 21, 3, 24);
        assert_eq!(link.len(), 1, "link ancestors leaked: {link:?}");
    }

    #[test]
    fn markdown_heading_fragments_are_unique_resolvable_and_sync_safe() {
        let path = Path::new("fragments.md");
        let source = concat!(
            "[first](#hello-world) [second](#hello-world-1) ",
            "[collision](#hello-world-1-1) ",
            "[unicode](#caf%C3%A9-d%C3%A9j%C3%A0) ",
            "[missing](#missing) [malformed](#bad%)\n\n",
            "# Hello, *World!*\n\n",
            "# Hello World\n\n",
            "# Hello-World-1\n\n",
            "# Café déjà\n\n",
            "# !!!\n",
        );
        let out = crate::render_document_from_source(
            path,
            source.to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        for anchor in [
            "hello-world",
            "hello-world-1",
            "hello-world-1-1",
            "café-déjà",
            "",
        ] {
            assert!(
                out.body_html.contains(&format!(r#"id="mdh:{anchor}""#)),
                "missing fragment id {anchor}: {}",
                out.body_html
            );
        }
        assert!(!out.body_html.contains("md-heading-anchor\" name="));
        for href in [
            "#mdh:hello-world",
            "#mdh:hello-world-1",
            "#mdh:hello-world-1-1",
            "#mdh:caf%C3%A9-d%C3%A9j%C3%A0",
        ] {
            assert!(
                out.body_html.contains(&format!(r#"href="{href}""#)),
                "missing resolved fragment link {href}: {}",
                out.body_html
            );
        }
        assert!(out.body_html.contains(r##"href="#missing""##));
        assert!(out.body_html.contains(r##"href="#bad%""##));

        let heading = out
            .sync
            .lookup_containing_container_by_source_position(path, 3, 1)
            .expect("heading sync container");
        assert_eq!(heading.element_id, "sec-g1-1");
        let first_word = out
            .sync
            .lookup_leaf_by_source_position(path, 3, 3)
            .expect("heading word sync leaf");
        assert_eq!(first_word.element_id, "md-g1-2");
        assert!(out
            .blocks
            .iter()
            .flat_map(|block| block.source_anchors.iter())
            .all(|anchor| !anchor.id.starts_with("mdh:")));
        assert!(out
            .sync
            .entries
            .iter()
            .all(|entry| !entry.element_id.starts_with("mdh:")));
    }

    #[test]
    fn markdown_heading_slugs_match_github_edge_cases() {
        let out = crate::render_document_from_source(
            Path::new("slugs.md"),
            concat!(
                "# 😄 emoji\n\n# two  spaces\n\n# -literal-\n\n# !!!\n\n",
                "# Echo\n\n# Echo\n\n# Echo 1\n\n# Echo-1\n\n# Echo\n",
            )
            .to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        for anchor in [
            "-emoji",
            "two--spaces",
            "-literal-",
            "",
            "echo",
            "echo-1",
            "echo-1-1",
            "echo-1-2",
            "echo-2",
        ] {
            assert!(
                out.body_html.contains(&format!(r#"id="mdh:{anchor}""#)),
                "missing GitHub-compatible fragment {anchor:?}: {}",
                out.body_html
            );
        }
    }

    #[test]
    fn markdown_duplicate_heading_slugging_scales_for_generated_documents() {
        let source = "# Same\n\n".repeat(16_000);
        let started = std::time::Instant::now();
        let nodes = parse(&source, Path::new("generated.md")).unwrap();
        let elapsed = started.elapsed();
        let last_anchor = all_nodes(&nodes)
            .into_iter()
            .rev()
            .find_map(|node| match &node.kind {
                NodeKind::MarkdownHeading { anchor, .. } => Some(anchor.as_str()),
                _ => None,
            })
            .expect("last heading");

        assert_eq!(last_anchor, "same-15999");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "duplicate heading allocation regressed from linear behavior: {elapsed:?}"
        );
    }

    #[test]
    fn markdown_heading_links_use_a_reserved_collision_free_namespace() {
        let source = concat!(
            "[chrome](#page) [canonical](#mdh:page) ",
            "[generated](#sec-g1-1) [footnote](#md-fn-a) ",
            "[reserved](#mdh-page)\n\n",
            "# Page\n\n",
            "# Sec G1 1\n\n",
            "# Md Fn A\n\n",
            "# Mdh Page\n\n",
            "Use a note[^a].\n\n",
            "[^a]: Note body.\n",
        );
        let parsed = parse(source, Path::new("namespaces.md")).unwrap();
        let destinations: Vec<_> = all_nodes(&parsed)
            .into_iter()
            .filter_map(|node| match &node.kind {
                NodeKind::MarkdownLink { destination, .. } => Some(destination.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            destinations,
            [
                "#mdh:page",
                "#mdh:page",
                "#mdh:sec-g1-1",
                "#mdh:md-fn-a",
                "#mdh:mdh-page",
            ]
        );
        let out = crate::render_document_from_source(
            Path::new("namespaces.md"),
            source.to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        for href in [
            "#mdh:page",
            "#mdh:sec-g1-1",
            "#mdh:md-fn-a",
            "#mdh:mdh-page",
        ] {
            assert!(out.body_html.contains(&format!(r#"href="{href}""#)));
        }
        for id in [
            "mdh:page",
            "mdh:sec-g1-1",
            "mdh:md-fn-a",
            "mdh:mdh-page",
            "md-fn-a",
        ] {
            assert!(out.body_html.contains(&format!(r#"id="{id}""#)));
        }
        assert!(out.body_html.contains(r#"id="sec-g1-1""#));

        let mut seen = HashSet::new();
        for rest in out.html.split(r#" id=""#).skip(1) {
            let id = rest.split('"').next().expect("id value");
            assert!(seen.insert(id), "duplicate DOM id {id:?}");
        }
    }

    #[test]
    fn markdown_fragment_aliases_are_stable_but_collision_changes_invalidate_blocks() {
        let render = |source: &str| {
            crate::render_document_from_source(
                Path::new("fragments.md"),
                source.to_string(),
                &crate::HtmlOptions::default(),
            )
            .unwrap()
        };
        let baseline = render("# Repeat\n\n# Repeat-1\n");
        let shifted = render("Unrelated paragraph.\n\n# Repeat\n\n# Repeat-1\n");
        let collision = render("# Repeat\n\n# Repeat\n\n# Repeat-1\n");

        let baseline_tail = baseline.blocks.last().unwrap();
        let shifted_tail = shifted.blocks.last().unwrap();
        let collision_tail = collision.blocks.last().unwrap();
        assert!(baseline_tail.html.contains(r#"id="mdh:repeat-1""#));
        assert!(shifted_tail.html.contains(r#"id="mdh:repeat-1""#));
        assert!(collision_tail.html.contains(r#"id="mdh:repeat-1-1""#));
        assert_eq!(
            baseline_tail.diff_hash, shifted_tail.diff_hash,
            "an unrelated preceding block changed the semantic heading hash"
        );
        assert_ne!(
            baseline_tail.diff_hash, collision_tail.diff_hash,
            "a changed fragment target was stripped from the semantic hash"
        );
    }

    #[test]
    fn markdown_footnote_targets_distinguish_sanitized_label_collisions() {
        let path = Path::new("footnotes.md");
        let source = concat!(
            "First[^a:b] and second[^a?b].\n\n",
            "[^a:b]: Colon label.\n",
            "[^a?b]: Question label.\n",
        );
        let out = crate::render_document_from_source(
            path,
            source.to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        assert_eq!(out.body_html.matches(r#"id="md-fn-a-b""#).count(), 1);
        assert_eq!(out.body_html.matches(r#"id="md-fn-a-b-1""#).count(), 1);
        assert_eq!(out.body_html.matches(r##"href="#md-fn-a-b""##).count(), 1);
        assert_eq!(out.body_html.matches(r##"href="#md-fn-a-b-1""##).count(), 1);
        assert!(out
            .sync
            .entries
            .iter()
            .any(|entry| entry.element_id == "md-fn-a-b"));
        assert!(out
            .sync
            .entries
            .iter()
            .any(|entry| entry.element_id == "md-fn-a-b-1"));
    }

    #[test]
    fn markdown_footnotes_follow_casefolding_and_duplicate_definition_semantics() {
        let source = concat!(
            "ASCII[^a], Unicode[^MASSE], and duplicate[^dup].\n\n",
            "[^A]: Uppercase target.\n\n",
            "[^Maße]: Unicode target.\n\n",
            "[^dup]: First duplicate.\n\n",
            "[^dup]: Last duplicate.\n",
        );
        let out = crate::render_document_from_source(
            Path::new("footnotes.md"),
            source.to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        assert!(out.body_html.contains(r##"href="#md-fn-A""##));
        assert!(out.body_html.contains(r##"href="#md-fn-Ma-e""##));
        assert!(out.body_html.contains(r##"href="#md-fn-dup-1""##));
        for id in ["md-fn-A", "md-fn-Ma-e", "md-fn-dup", "md-fn-dup-1"] {
            assert_eq!(
                out.body_html.matches(&format!(r#"id="{id}""#)).count(),
                1,
                "expected one {id:?}: {}",
                out.body_html
            );
        }
    }

    #[test]
    fn markdown_fragment_support_never_changes_latex_section_ids_or_refs() {
        let path = Path::new("stable.tex");
        let source = concat!(
            "\\begin{document}\n",
            "\\section{Stable Section}\\label{sec:stable}\n",
            "See Section~\\ref{sec:stable}.\n",
            "\\begin{equation}\\label{eq:stable}a=b\\end{equation}\n",
            "See Equation~\\eqref{eq:stable}.\n",
            "\\end{document}\n",
        );
        let out = crate::render_document_from_source(
            path,
            source.to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();

        assert_eq!(out.format, crate::DocumentFormat::Latex);
        assert!(!out.body_html.contains("md-heading-anchor"));
        assert!(!out.body_html.contains("mdh:"));
        assert!(out.body_html.contains(
            r#"<h2 id="sec-stable" class="sec-h2" data-src="stable.tex:2:1" data-refkey="sec:stable">"#
        ));
        assert!(out
            .body_html
            .contains(r##"href="#sec-stable" data-target="sec:stable" data-kind="ref""##));
        assert!(out
            .body_html
            .contains(r##"href="#eq-stable" data-target="eq:stable" data-kind="eqref""##));
        assert!(out
            .sync
            .entries
            .iter()
            .any(|entry| entry.element_id == "sec-stable"));
    }

    #[test]
    fn renders_gfm_structure_math_and_inert_html() {
        let source = concat!(
            "# Rich *Markdown*\n\n",
            "Text with **strength**, ~~strike~~, [safe](https://example.com), ",
            "[bad](javascript:evil), and ![plot](images/plot.png \"Plot\").\n\n",
            "Slash math \\(r+s\\).\n\n$$u+v$$\n\n",
            "> Quoted $q$.\n\n",
            "- [x] complete\n- [ ] pending\n\n",
            "3. third\n4. fourth\n\n",
            "| Left | Right |\n| :--- | ---: |\n| $x$ | `\\(literal\\)` |\n\n",
            "```rust\nlet raw = \"$$not_math$$\";\n```\n\n",
            "\\[a^2+b^2=c^2\\]\n\n---\n\n",
            "<script>alert('never execute')</script>\n",
        );
        let opts = crate::HtmlOptions {
            local_asset_base: Some("/assets/".to_string()),
            ..crate::HtmlOptions::default()
        };
        let out =
            crate::render_document_from_source(Path::new("paper.md"), source.to_string(), &opts)
                .unwrap();

        assert_eq!(out.format, crate::DocumentFormat::Markdown);
        for marker in [
            "md-heading",
            "md-emphasis",
            "md-strong",
            "md-strikethrough",
            "md-blockquote",
            "md-task-marker",
            "md-table",
            "md-code-block",
            "md-rule",
            r#"class="math inline"#,
            r#"class="math display"#,
        ] {
            assert!(
                out.body_html.contains(marker),
                "missing {marker}: {}",
                out.body_html
            );
        }
        assert!(out.body_html.contains(r#"src="/assets/images/plot.png""#));
        assert!(
            out.body_html.contains("md-url-rejected"),
            "{}",
            out.body_html
        );
        assert!(!out.body_html.contains(r#"href="javascript:""#));
        assert!(!out
            .body_html
            .contains("<script>alert('never execute')</script>"));
        assert!(out
            .body_html
            .contains("&lt;script&gt;alert('never execute')&lt;/script&gt;"));
        assert!(out.body_html.contains("$$not_math$$"));
        assert!(out.body_html.contains("\\(literal\\)"));
        assert!(out.html.contains(r#"data-document-format="markdown""#));
        assert!(out.html.contains(r#"data-viewer-action="browser-print""#));
        assert!(!out.html.contains(r#"data-viewer-action="print-pdf""#));
        assert!(out
            .html
            .contains(r#"body[data-document-format="markdown"] .refkey-toggle"#));
        assert!(out
            .html
            .contains(r#"body[data-document-format="markdown"] .proof-toggle"#));
        assert!(!out.blocks.is_empty());
        assert!(!out.sync.entries.is_empty());
        assert!(out
            .sync
            .entries
            .iter()
            .all(|entry| entry.file == Path::new("paper.md")));
        assert_eq!(out.body_html.matches(r#"class="math inline""#).count(), 3);
        assert_eq!(out.body_html.matches(r#"class="math display""#).count(), 2);
    }

    #[test]
    fn static_images_stay_relative_and_unsafe_schemes_are_removed() {
        let out = crate::render_document_from_source(
            Path::new("images.md"),
            "![local](./figures/a.png) ![bad](data:text/html,boom) ![up](../secret.png)\n"
                .to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();
        assert!(out.body_html.contains(r#"src="./figures/a.png""#));
        assert!(!out.body_html.contains(r#"src="data:""#));
        assert!(!out.body_html.contains(r#"src="../secret.png""#));
        assert_eq!(out.body_html.matches("md-image-alt").count(), 2);
    }

    #[test]
    fn static_file_base_applies_only_to_safe_images_and_not_fragment_links() {
        let opts = crate::HtmlOptions {
            local_asset_base: Some("file:///tmp/Notes%20%CE%BB/".to_string()),
            ..crate::HtmlOptions::default()
        };
        let out = crate::render_document_from_source(
            Path::new("images.md"),
            "![local](<fig one-λ.png>) ![encoded](fig%20two.png) ![up](../secret.png) [jump](#part)\n"
                .to_string(),
            &opts,
        )
        .unwrap();
        assert!(out
            .body_html
            .contains(r#"src="file:///tmp/Notes%20%CE%BB/fig%20one-%CE%BB.png""#));
        assert!(out
            .body_html
            .contains(r#"src="file:///tmp/Notes%20%CE%BB/fig%20two.png""#));
        assert!(!out.body_html.contains("file:///tmp/Notes%20%CE%BB/../"));
        assert!(out.body_html.contains(r##"href="#part""##));
    }

    #[test]
    fn encoded_image_traversal_is_inert_for_live_and_static_asset_bases() {
        let source = concat!(
            "![dot](%2e%2e/secret.png) ",
            "![mixed](.%2E/secret.png) ",
            "![slash](safe%2f..%2fsecret.png) ",
            "![backslash](safe%5c..%5csecret.png)\n",
        );
        for local_asset_base in [
            None,
            Some("/assets/".to_string()),
            Some("file:///tmp/markdown-root/".to_string()),
        ] {
            let opts = crate::HtmlOptions {
                local_asset_base,
                ..crate::HtmlOptions::default()
            };
            let out = crate::render_document_from_source(
                Path::new("images.md"),
                source.to_string(),
                &opts,
            )
            .unwrap();
            assert!(!out.body_html.contains("<img"), "{}", out.body_html);
            assert_eq!(out.body_html.matches("md-image-alt").count(), 4);
            assert!(!out.body_html.contains("secret.png"), "{}", out.body_html);
        }
    }

    #[test]
    fn url_policy_rejects_active_schemes_unc_and_parent_images() {
        for url in [
            "javascript:alert(1)",
            "JaVaScRiPt:evil",
            "data:text/html,boom",
            "vbscript:evil",
            r"\\server\share",
        ] {
            assert!(!crate::renderer::safe_markdown_url(url), "accepted {url}");
        }
        assert!(crate::renderer::safe_markdown_url("https://example.com/a"));
        assert!(crate::renderer::safe_markdown_url("notes/next.md#part"));
    }

    #[test]
    fn unchanged_markdown_blocks_keep_hashes_and_source_anchors() {
        let first = crate::render_document_from_source(
            Path::new("stable.md"),
            "# Heading\n\nFirst paragraph.\n\nSecond paragraph.\n".to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();
        let second = crate::render_document_from_source(
            Path::new("stable.md"),
            "# Heading\n\nFirst paragraph.\n\nSecond paragraph changed.\n".to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();
        assert_eq!(first.blocks.len(), 3);
        assert_eq!(second.blocks.len(), 3);
        assert_eq!(first.blocks[0].diff_hash, second.blocks[0].diff_hash);
        assert_eq!(first.blocks[1].diff_hash, second.blocks[1].diff_hash);
        assert_ne!(first.blocks[2].diff_hash, second.blocks[2].diff_hash);
        assert!(!first.blocks[1].source_anchors.is_empty());
    }

    #[test]
    fn format_detection_opts_markdown_in_and_defaults_other_paths_to_latex() {
        assert_eq!(
            crate::DocumentFormat::from_path(Path::new("paper.MARKDOWN")),
            Some(crate::DocumentFormat::Markdown)
        );
        assert_eq!(
            crate::DocumentFormat::from_path(Path::new("paper.TeX")),
            Some(crate::DocumentFormat::Latex)
        );
        assert_eq!(
            crate::DocumentFormat::from_path(Path::new("chapter.inc")),
            None
        );
        assert_eq!(
            crate::DocumentFormat::detect(Path::new("chapter.inc")).unwrap(),
            crate::DocumentFormat::Latex
        );
        assert_eq!(
            crate::DocumentFormat::detect(Path::new("untitled")).unwrap(),
            crate::DocumentFormat::Latex
        );
    }

    #[test]
    fn generic_render_resolves_explicit_extension_latex_child() {
        let unique = format!(
            "mathpreview-explicit-inc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("main.tex");
        let child = dir.join("chapter.inc");
        std::fs::write(
            &root,
            concat!(
                "\\documentclass{article}\n",
                "\\begin{document}\n",
                "Root before.\n",
                "\\input{chapter.inc}\n",
                "Root after.\n",
                "\\end{document}\n",
            ),
        )
        .unwrap();
        std::fs::write(&child, "Included from inc.\n").unwrap();

        let rendered = crate::render_document(&child, &crate::HtmlOptions::default()).unwrap();
        let root = root.canonicalize().unwrap();
        let child = child.canonicalize().unwrap();
        assert_eq!(rendered.format, crate::DocumentFormat::Latex);
        assert_eq!(rendered.root_file, root);
        assert!(rendered.included_files.contains(&child));
        assert!(rendered.body_html.contains("Included"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn markdown_preamble_keeps_builtins_and_user_overrides() {
        let overrides = [crate::MacroOverride {
            label: Path::new("custom.tex").to_path_buf(),
            source: r"\newcommand{\markdownmacro}[1]{\mathbf{#1}}".to_string(),
        }];
        let preamble = crate::extract_preamble_from_overrides(&overrides);
        assert!(preamble
            .macros
            .iter()
            .any(|macro_def| macro_def.name == "markdownmacro"));
        assert!(preamble.raw_preamble.contains("<builtin-macros.tex>"));
    }

    #[test]
    fn generic_render_keeps_all_math_source_lines() {
        let out = crate::render_document_from_source(
            Path::new("sync.md"),
            "$a$ and \\(b\\)\n\n$$c$$\n\n\\[d\\]\n".to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();
        let inline_lines: Vec<_> = out
            .sync
            .entries
            .iter()
            .filter(|entry| entry.element_id.starts_with("im-"))
            .map(|entry| (entry.start.line, entry.end.line))
            .collect();
        let display_lines: Vec<_> = out
            .sync
            .entries
            .iter()
            .filter(|entry| entry.element_id.starts_with("dm-"))
            .map(|entry| (entry.start.line, entry.end.line))
            .collect();
        assert_eq!(inline_lines, [(1, 1), (1, 1)]);
        assert_eq!(display_lines, [(3, 3), (5, 5)]);
        assert_eq!(out.body_html.matches(r#"class="math inline""#).count(), 2);
        assert_eq!(out.body_html.matches(r#"class="math display""#).count(), 2);
    }

    #[test]
    fn table_header_is_retained_as_cells_and_renders_th() {
        let source = "| A | B |\n| :- | -: |\n| x | y |\n";
        let nodes = parse(source, Path::new("table.md")).unwrap();
        let table = nodes
            .iter()
            .find(|node| matches!(node.kind, NodeKind::MarkdownTable { .. }))
            .unwrap();
        let head = table
            .children
            .iter()
            .find(|node| matches!(node.kind, NodeKind::MarkdownTableHead))
            .unwrap();
        assert_eq!(head.children.len(), 2);
        assert!(head
            .children
            .iter()
            .all(|node| matches!(node.kind, NodeKind::MarkdownTableCell)));

        let rendered = crate::render_document_from_source(
            Path::new("table.md"),
            source.to_string(),
            &crate::HtmlOptions::default(),
        )
        .unwrap();
        assert_eq!(rendered.body_html.matches("<th ").count(), 2);
        assert!(rendered.body_html.contains("align-left"));
        assert!(rendered.body_html.contains("align-right"));
    }

    #[test]
    fn generic_render_loads_markdown_macro_override_file() {
        let unique = format!(
            "mathpreview-markdown-macros-{}-{}.tex",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, r"\newcommand{\markdownmacro}[1]{\mathbf{#1}}").unwrap();
        let opts = crate::HtmlOptions {
            macro_overrides: vec![path.clone()],
            ..crate::HtmlOptions::default()
        };
        let rendered = crate::render_document_from_source(
            Path::new("macros.md"),
            r"$\markdownmacro{x}$".to_string(),
            &opts,
        )
        .unwrap();
        let _ = std::fs::remove_file(path);
        assert!(rendered
            .preamble
            .macros
            .iter()
            .any(|macro_def| macro_def.name == "markdownmacro"));
        assert!(rendered.html.contains("markdownmacro"));
    }
}
