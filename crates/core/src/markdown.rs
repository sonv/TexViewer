//! CommonMark/GFM frontend with source spans and native math events.
//!
//! The parser never emits raw HTML into the viewer. HTML events are retained
//! as explicit inert AST nodes and escaped by the renderer.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use anyhow::{anyhow, Result};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};

use crate::ast::{MarkdownAlignment, Node, NodeKind, Pos, Span};

/// Parse one Markdown source file into the shared source-spanned AST.
pub fn parse(source: &str, file: &Path) -> Result<Vec<Node>> {
    let options = markdown_options();
    let (parser_source, delimiter_overrides) = protect_tex_math_delimiters(source, options);
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
                if let Some(Node {
                    kind: NodeKind::MarkdownCodeBlock { code, .. },
                    ..
                }) = stack.last_mut()
                {
                    code.push_str(&text);
                } else {
                    append_leaf(
                        &mut roots,
                        &mut stack,
                        NodeKind::MarkdownText(text.into_string()),
                        positions.span(file, range.start, range.end),
                    );
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
    Ok(roots)
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
