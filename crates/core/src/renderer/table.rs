//! Lightweight native renderer for LaTeX text tables.
//!
//! The document parser intentionally retains tabular environments as
//! `OpaqueEnv`: ordinary prose parsing cannot assign semantics to `&`, `\\`,
//! column specifications, or horizontal rules.  This module interprets that
//! retained source without invoking TeX.  It is deliberately conservative:
//! malformed preambles return `None` so the caller can keep the safe opaque
//! fallback instead of dropping source.

use std::fmt::Write;

use crate::numbering::LabelTable;

use super::math::{label_alias_anchors, render_latex_text_with_math, strip_labels};
use super::util::{
    css_number, escape_attr, escape_html, parse_number_prefix, refkey_attr, sanitize_id,
    strip_wrapping_braces,
};

const TABULAR_ENVS: &[&str] = &["tabular", "tabular*", "tabularx", "longtable"];
const MAX_SPEC_DEPTH: usize = 12;
const MAX_REPEAT: usize = 128;
const MAX_COLUMNS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alignment {
    Left,
    Center,
    Right,
}

impl Alignment {
    fn class(self) -> &'static str {
        match self {
            Self::Left => "align-left",
            Self::Center => "align-center",
            Self::Right => "align-right",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerticalAlignment {
    Top,
    Baseline,
    Middle,
    Bottom,
}

impl VerticalAlignment {
    fn class(self) -> &'static str {
        match self {
            Self::Top => "valign-top",
            Self::Baseline => "valign-baseline",
            Self::Middle => "valign-middle",
            Self::Bottom => "valign-bottom",
        }
    }
}

#[derive(Debug, Clone)]
struct Column {
    alignment: Alignment,
    vertical: VerticalAlignment,
    width: Option<String>,
    wraps: bool,
    rule_left: bool,
    rule_right: bool,
}

impl Default for Column {
    fn default() -> Self {
        Self {
            alignment: Alignment::Center,
            vertical: VerticalAlignment::Baseline,
            width: None,
            wraps: false,
            rule_left: false,
            rule_right: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RuleWeight {
    Thin,
    Strong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rule {
    weight: RuleWeight,
    /// One-based inclusive column range. `None` means the whole row.
    range: Option<(usize, usize)>,
}

#[derive(Debug)]
struct Cell {
    tex: String,
    colspan: usize,
    local_column: Option<Column>,
}

#[derive(Debug)]
struct Row {
    cells: Vec<Cell>,
    rules_above: Vec<Rule>,
    rules_below: Vec<Rule>,
}

struct ParsedTabular {
    columns: Vec<Column>,
    rows: Vec<Row>,
    width: Option<String>,
}

pub(super) fn is_tabular_environment(env: &str) -> bool {
    TABULAR_ENVS.contains(&env)
}

pub(super) fn first_nested_tabular(source: &str) -> Option<(String, String)> {
    crate::parser::first_supported_environment(source, TABULAR_ENVS)
}

/// Render a retained tabular body as semantic HTML. Returns `None` if the
/// leading environment arguments or column specification are malformed.
pub(super) fn render_tabular(env: &str, body: &str, labels: &LabelTable) -> Option<String> {
    render_tabular_with_number(env, body, labels, None)
}

pub(super) fn render_tabular_with_number(
    env: &str,
    body: &str,
    labels: &LabelTable,
    table_number: Option<&str>,
) -> Option<String> {
    if !is_tabular_environment(env) {
        return None;
    }

    let cleaned = crate::parser::executable_latex_source(body);
    let (width, column_spec, content) = parse_environment_arguments(env, &cleaned)?;
    let columns = parse_column_spec(column_spec, 0);
    if columns.is_empty() {
        return None;
    }

    let longtable_caption = (env == "longtable")
        .then(|| {
            crate::parser::live_braced_command_calls(&cleaned, &["caption"], 1)
                .into_iter()
                .next()
        })
        .flatten();
    let content = if env == "longtable" {
        prepare_longtable_content(content)
    } else {
        content.to_string()
    };
    let rows = parse_rows(&content);
    if rows.is_empty() {
        return None;
    }

    let parsed = ParsedTabular {
        columns,
        rows,
        width: width.and_then(latex_dimension_to_css),
    };
    Some(render_parsed_tabular(
        env,
        &parsed,
        labels,
        longtable_caption
            .as_ref()
            .map(|caption| (caption.value.as_str(), caption.starred)),
        &cleaned,
        table_number,
    ))
}

fn parse_environment_arguments<'a>(
    env: &str,
    body: &'a str,
) -> Option<(Option<&'a str>, &'a str, &'a str)> {
    let mut i = skip_ws(body, 0);
    let mut width = None;

    if matches!(env, "tabular*" | "tabularx") {
        let (value, next) = read_group(body, i, b'{', b'}')?;
        width = Some(value.trim());
        i = skip_ws(body, next);
        if body.as_bytes().get(i) == Some(&b'[') {
            let (_, next) = read_group(body, i, b'[', b']')?;
            i = skip_ws(body, next);
        }
    } else {
        if body.as_bytes().get(i) == Some(&b'[') {
            let (_, next) = read_group(body, i, b'[', b']')?;
            i = skip_ws(body, next);
        }
    }

    let (column_spec, next) = read_group(body, i, b'{', b'}')?;
    Some((width, column_spec, &body[next..]))
}

fn parse_column_spec(spec: &str, depth: usize) -> Vec<Column> {
    if depth >= MAX_SPEC_DEPTH {
        return Vec::new();
    }

    let bytes = spec.as_bytes();
    let mut columns = Vec::<Column>::new();
    let mut pending_rule = false;
    let mut pending_alignment = None;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
            }
            b'|' | b':' => {
                pending_rule = true;
                i += 1;
            }
            b'>' => {
                i += 1;
                i = skip_ws(spec, i);
                if let Some((decorator, next)) = read_group(spec, i, b'{', b'}') {
                    pending_alignment = alignment_from_decorator(decorator);
                    i = next;
                }
            }
            b'<' | b'@' | b'!' => {
                i += 1;
                i = skip_ws(spec, i);
                if let Some((_, next)) = read_group(spec, i, b'{', b'}') {
                    i = next;
                }
            }
            b'*' => {
                i += 1;
                i = skip_ws(spec, i);
                let Some((count, after_count)) = read_group(spec, i, b'{', b'}') else {
                    continue;
                };
                i = skip_ws(spec, after_count);
                let Some((inner, next)) = read_group(spec, i, b'{', b'}') else {
                    continue;
                };
                i = next;
                let count = count.trim().parse::<usize>().unwrap_or(0).min(MAX_REPEAT);
                let repeated = parse_column_spec(inner, depth + 1);
                if repeated.is_empty() {
                    continue;
                }
                'repeats: for repeat_index in 0..count {
                    for (column_index, template) in repeated.iter().enumerate() {
                        if columns.len() >= MAX_COLUMNS {
                            break 'repeats;
                        }
                        let mut column = template.clone();
                        if repeat_index == 0 && column_index == 0 && pending_rule {
                            column.rule_left = true;
                            pending_rule = false;
                        }
                        if let Some(alignment) = pending_alignment.take() {
                            column.alignment = alignment;
                        }
                        columns.push(column);
                    }
                }
            }
            b'l' | b'c' | b'r' | b'p' | b'm' | b'b' | b'X' | b'S' | b'w' | b'W' => {
                let kind = bytes[i];
                i += 1;
                let mut column = match kind {
                    b'l' => Column {
                        alignment: Alignment::Left,
                        ..Column::default()
                    },
                    b'r' => Column {
                        alignment: Alignment::Right,
                        ..Column::default()
                    },
                    b'S' => {
                        i = skip_ws(spec, i);
                        if spec.as_bytes().get(i) == Some(&b'[') {
                            let Some((_, next)) = read_group(spec, i, b'[', b']') else {
                                return Vec::new();
                            };
                            i = next;
                        }
                        Column {
                            alignment: Alignment::Right,
                            ..Column::default()
                        }
                    }
                    b'p' | b'm' | b'b' => {
                        i = skip_ws(spec, i);
                        let width = read_group(spec, i, b'{', b'}');
                        if let Some((_, next)) = width {
                            i = next;
                        }
                        Column {
                            alignment: Alignment::Left,
                            vertical: match kind {
                                b'm' => VerticalAlignment::Middle,
                                b'b' => VerticalAlignment::Bottom,
                                _ => VerticalAlignment::Top,
                            },
                            width: width
                                .and_then(|(value, _)| latex_column_dimension_to_css(value.trim())),
                            wraps: true,
                            ..Column::default()
                        }
                    }
                    b'X' => Column {
                        alignment: Alignment::Left,
                        wraps: true,
                        ..Column::default()
                    },
                    b'w' | b'W' => {
                        i = skip_ws(spec, i);
                        let alignment = read_group(spec, i, b'{', b'}');
                        if let Some((_, next)) = alignment {
                            i = skip_ws(spec, next);
                        }
                        let width = read_group(spec, i, b'{', b'}');
                        if let Some((_, next)) = width {
                            i = next;
                        }
                        Column {
                            alignment: alignment
                                .and_then(|(value, _)| parse_alignment(value.trim()))
                                .unwrap_or(Alignment::Center),
                            width: width
                                .and_then(|(value, _)| latex_column_dimension_to_css(value.trim())),
                            wraps: kind == b'W',
                            ..Column::default()
                        }
                    }
                    _ => Column::default(),
                };
                if let Some(alignment) = pending_alignment.take() {
                    column.alignment = alignment;
                }
                column.rule_left = pending_rule;
                pending_rule = false;
                if columns.len() < MAX_COLUMNS {
                    columns.push(column);
                }
            }
            b'\\' => {
                let (_, next) = control_word_at(spec, i).unwrap_or(("", i + 1));
                i = next;
                let mut column = Column::default();
                if let Some(alignment) = pending_alignment.take() {
                    column.alignment = alignment;
                }
                column.rule_left = pending_rule;
                pending_rule = false;
                if columns.len() < MAX_COLUMNS {
                    columns.push(column);
                }
            }
            b'{' => {
                i = read_group(spec, i, b'{', b'}')
                    .map(|(_, next)| next)
                    .unwrap_or(bytes.len());
            }
            byte if byte.is_ascii_alphabetic() => {
                // A custom `\newcolumntype` token still represents one column.
                // Centering is the least surprising safe approximation.
                let mut column = Column::default();
                if let Some(alignment) = pending_alignment.take() {
                    column.alignment = alignment;
                }
                column.rule_left = pending_rule;
                pending_rule = false;
                if columns.len() < MAX_COLUMNS {
                    columns.push(column);
                }
                i += 1;
            }
            _ => {
                i += char_width_at(spec, i);
            }
        }
    }

    if pending_rule {
        if let Some(last) = columns.last_mut() {
            last.rule_right = true;
        }
    }
    columns
}

fn parse_rows(content: &str) -> Vec<Row> {
    let mut rows = Vec::<Row>::new();
    for raw_row in split_top_level_rows(content) {
        let (rules, rest) = take_leading_row_directives(raw_row);
        if rest.trim().is_empty() {
            if !rules.is_empty() {
                if let Some(previous) = rows.last_mut() {
                    previous.rules_below.extend(rules);
                }
            }
            continue;
        }

        let cells = split_top_level_cells(rest)
            .into_iter()
            .map(parse_cell)
            .collect::<Vec<_>>();
        if cells.iter().all(|cell| cell.tex.trim().is_empty()) && rules.is_empty() {
            continue;
        }
        rows.push(Row {
            cells,
            rules_above: rules,
            rules_below: Vec::new(),
        });
    }
    rows
}

fn parse_cell(raw: &str) -> Cell {
    let trimmed = raw.trim();
    if let Some((colspan, spec, content, next)) = parse_multicolumn(trimmed) {
        let mut tex = content.to_string();
        let trailing = trimmed[next..].trim();
        if !trailing.is_empty() {
            if !tex.ends_with(char::is_whitespace) {
                tex.push(' ');
            }
            tex.push_str(trailing);
        }
        return Cell {
            tex,
            colspan,
            local_column: parse_column_spec(spec, 0).into_iter().next(),
        };
    }
    Cell {
        tex: trimmed.to_string(),
        colspan: 1,
        local_column: None,
    }
}

fn parse_multicolumn(src: &str) -> Option<(usize, &str, &str, usize)> {
    let (name, mut i) = control_word_at(src, 0)?;
    if name != "multicolumn" {
        return None;
    }
    i = skip_ws(src, i);
    let (count, next) = read_group(src, i, b'{', b'}')?;
    i = skip_ws(src, next);
    let (spec, next) = read_group(src, i, b'{', b'}')?;
    i = skip_ws(src, next);
    let (content, next) = read_group(src, i, b'{', b'}')?;
    let colspan = count.trim().parse::<usize>().ok()?.max(1);
    Some((colspan, spec, content, next))
}

fn render_parsed_tabular(
    env: &str,
    table: &ParsedTabular,
    labels: &LabelTable,
    caption: Option<(&str, bool)>,
    original_body: &str,
    table_number: Option<&str>,
) -> String {
    let mut out = String::new();
    let table_style = table
        .width
        .as_deref()
        .map(|width| format!(r#" style="width:{width}""#))
        .unwrap_or_default();

    let longtable_labels = if env == "longtable" {
        crate::parser::live_braced_command_calls(original_body, &["label"], 0)
            .into_iter()
            .map(|call| call.value)
            .collect()
    } else {
        Vec::new()
    };
    let primary_label = longtable_labels.first().map(String::as_str);
    let alias_anchors = if env == "longtable" {
        label_alias_anchors(original_body, primary_label)
    } else {
        String::new()
    };
    let id_attr = primary_label
        .map(|label| format!(r#" id="{}""#, escape_attr(&sanitize_id(label))))
        .unwrap_or_default();
    let refkey = refkey_attr(primary_label);

    write!(
        out,
        r#"<div class="latex-tabular-scroll" data-env="{env}">{aliases}<table class="latex-tabular env-{class}"{id}{refkey}{style}>"#,
        env = escape_attr(env),
        aliases = alias_anchors,
        class = escape_attr(&sanitize_id(env)),
        id = id_attr,
        refkey = refkey,
        style = table_style,
    )
    .unwrap();

    if let Some((caption, starred)) = caption {
        let caption = render_latex_text_with_math(strip_labels(caption).trim(), labels);
        if starred {
            write!(out, r#"<caption>{caption}</caption>"#).unwrap();
        } else {
            let kind = table_number
                .map(str::to_string)
                .or_else(|| primary_label.and_then(|label| labels.number.get(label).cloned()))
                .map(|number| format!("Table {}.", escape_html(&number)))
                .unwrap_or_else(|| "Table.".to_string());
            write!(
                out,
                r#"<caption><span class="float-kind">{kind}</span> {caption}</caption>"#
            )
            .unwrap();
        }
    }

    out.push_str("<tbody>");
    for row in &table.rows {
        out.push_str("<tr>");
        let mut column_index = 0usize;
        for cell in &row.cells {
            let start_column = column_index + 1;
            let end_column = column_index.saturating_add(cell.colspan);
            let base = table.columns.get(column_index).cloned().unwrap_or_default();
            let ending = table
                .columns
                .get(end_column.saturating_sub(1))
                .unwrap_or(&base);
            let effective = cell.local_column.as_ref().unwrap_or(&base);

            let mut classes = vec![
                "latex-tabular-cell",
                effective.alignment.class(),
                effective.vertical.class(),
                if effective.wraps {
                    "cell-wrap"
                } else {
                    "cell-nowrap"
                },
            ];
            if effective.rule_left || base.rule_left {
                classes.push("rule-left");
            }
            if effective.rule_right || ending.rule_right {
                classes.push("rule-right");
            }
            if let Some(weight) = matching_rule(&row.rules_above, start_column, end_column) {
                classes.push(match weight {
                    RuleWeight::Thin => "rule-top",
                    RuleWeight::Strong => "rule-top-strong",
                });
            }
            if let Some(weight) = matching_rule(&row.rules_below, start_column, end_column) {
                classes.push(match weight {
                    RuleWeight::Thin => "rule-bottom",
                    RuleWeight::Strong => "rule-bottom-strong",
                });
            }

            let colspan = if cell.colspan > 1 {
                format!(r#" colspan="{}""#, cell.colspan)
            } else {
                String::new()
            };
            let style = effective
                .width
                .as_deref()
                .map(|width| format!(r#" style="width:{width}""#))
                .unwrap_or_default();
            write!(
                out,
                r#"<td class="{classes}"{colspan}{style}>{content}</td>"#,
                classes = classes.join(" "),
                content = render_cell_content(cell.tex.trim(), labels),
            )
            .unwrap();
            column_index = column_index.saturating_add(cell.colspan);
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    out
}

fn render_cell_content(tex: &str, labels: &LabelTable) -> String {
    let bytes = tex.as_bytes();
    let mut out = String::new();
    let mut segment_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += char_width_at(tex, i);
            continue;
        }
        if let Some((name, next)) = control_word_at(tex, i) {
            if crate::parser::is_inline_literal_command(name) {
                i = crate::parser::inline_literal_payload(tex, name, next)
                    .map_or(bytes.len(), |(_, end)| end);
                continue;
            }
            if name == "begin" {
                if let Some(span) = crate::parser::inert_environment_span_at(tex, i) {
                    out.push_str(&render_cell_inline_content(&tex[segment_start..i], labels));
                    if !span.discarded {
                        let body = tex[span.body_start..span.body_end]
                            .strip_prefix('\n')
                            .unwrap_or(&tex[span.body_start..span.body_end]);
                        let body = body.strip_suffix('\n').unwrap_or(body);
                        write!(
                            out,
                            r#"<pre class="table-literal-env" data-env="{env}"><code>{body}</code></pre>"#,
                            env = escape_attr(&span.name),
                            body = escape_html(body),
                        )
                        .unwrap();
                    }
                    i = span.end;
                    segment_start = i;
                    continue;
                }
            }
            i = next;
            continue;
        }
        i = escaped_token_end(tex, i);
    }
    out.push_str(&render_cell_inline_content(&tex[segment_start..], labels));
    out
}

fn render_cell_inline_content(tex: &str, labels: &LabelTable) -> String {
    // `&` is structural to the tabular scanner, but its escaped form is
    // ordinary text. The shared inline renderer also understands `\&`, but
    // retaining normalization here keeps the scanner/render boundary explicit:
    // only text-mode `\&` is changed after cells split, while math keeps the
    // author's original TeX for MathJax.
    let tex = normalize_text_ampersands(tex);
    render_latex_text_with_math(&tex, labels)
}

fn normalize_text_ampersands(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut in_math = false;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            if let Some((name, next)) = control_word_at(src, i) {
                if name == "string" {
                    let end = tex_token_end_after_command(src, next);
                    out.push_str(&src[i..end]);
                    i = end;
                    continue;
                }
                if matches!(name, "detokenize" | "unexpanded") {
                    let group_start = skip_tex_space_and_comments(src, next);
                    let end = crate::parser::tex_group_end(src, group_start, b'{', b'}')
                        .unwrap_or(bytes.len());
                    out.push_str(&src[i..end]);
                    i = end;
                    continue;
                }
                if name == "begin" {
                    if let Some(span) = crate::parser::inert_environment_span_at(src, i) {
                        out.push_str(&src[i..span.end]);
                        i = span.end;
                        continue;
                    }
                }
                if crate::parser::is_inline_literal_command(name) {
                    let end = crate::parser::inline_literal_payload(src, name, next)
                        .map_or(bytes.len(), |(_, end)| end);
                    out.push_str(&src[i..end]);
                    i = end;
                    continue;
                }
            }
            match bytes[i + 1] {
                b'&' if !in_math => {
                    out.push('&');
                    i += 2;
                }
                b'(' | b'[' if !in_math => {
                    out.push_str(&src[i..i + 2]);
                    in_math = true;
                    i += 2;
                }
                b')' | b']' if in_math => {
                    out.push_str(&src[i..i + 2]);
                    in_math = false;
                    i += 2;
                }
                _ => {
                    out.push('\\');
                    let escaped = src[i + 1..].chars().next().unwrap_or('\0');
                    out.push(escaped);
                    i += 1 + escaped.len_utf8();
                }
            }
            continue;
        }
        if bytes[i] == b'$' {
            let display = bytes.get(i + 1) == Some(&b'$');
            out.push('$');
            if display {
                out.push('$');
                i += 1;
            }
            in_math = !in_math;
            i += 1;
            continue;
        }
        let ch = src[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn matching_rule(rules: &[Rule], start: usize, end: usize) -> Option<RuleWeight> {
    rules
        .iter()
        .filter(|rule| {
            rule.range
                .is_none_or(|(from, to)| from <= end && to >= start)
        })
        .map(|rule| rule.weight)
        .max()
}

fn take_leading_row_directives(mut row: &str) -> (Vec<Rule>, &str) {
    let mut rules = Vec::<Rule>::new();
    loop {
        row = row.trim_start();
        let Some((name, mut i)) = control_word_at(row, 0) else {
            break;
        };
        let weight = match name {
            "hline" | "midrule" => Some(RuleWeight::Thin),
            "toprule" | "bottomrule" => Some(RuleWeight::Strong),
            _ => None,
        };
        if let Some(weight) = weight {
            i = skip_optional_dimension(row, i);
            rules.push(Rule {
                weight,
                range: None,
            });
            row = &row[i..];
            continue;
        }

        if matches!(name, "cline" | "cmidrule") {
            i = skip_ws(row, i);
            if name == "cmidrule" && row.as_bytes().get(i) == Some(&b'(') {
                if let Some((_, next)) = read_group(row, i, b'(', b')') {
                    i = skip_ws(row, next);
                }
            }
            i = skip_optional_dimension(row, i);
            i = skip_ws(row, i);
            let Some((range, next)) = read_group(row, i, b'{', b'}') else {
                break;
            };
            if let Some(range) = parse_rule_range(range) {
                rules.push(Rule {
                    weight: RuleWeight::Thin,
                    range: Some(range),
                });
            }
            row = &row[next..];
            continue;
        }

        if matches!(name, "addlinespace" | "noalign") {
            i = skip_ws(row, i);
            if row.as_bytes().get(i) == Some(&b'[') {
                if let Some((_, next)) = read_group(row, i, b'[', b']') {
                    i = next;
                }
            } else if row.as_bytes().get(i) == Some(&b'{') {
                if let Some((_, next)) = read_group(row, i, b'{', b'}') {
                    i = next;
                }
            }
            row = &row[i..];
            continue;
        }
        break;
    }
    (rules, row)
}

fn parse_rule_range(src: &str) -> Option<(usize, usize)> {
    let (from, to) = src.trim().split_once('-')?;
    let from = from.trim().parse::<usize>().ok()?.max(1);
    let to = to.trim().parse::<usize>().ok()?.max(from);
    Some((from, to))
}

fn split_top_level_rows(src: &str) -> Vec<&str> {
    let bytes = src.as_bytes();
    let mut rows = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut brace_depth = 0i32;
    let mut env_depth = 0i32;

    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if bytes.get(i + 1) == Some(&b'\\') && brace_depth == 0 && env_depth == 0 {
                rows.push(src[start..i].trim());
                i = skip_row_terminator_suffix(src, i + 2);
                start = i;
                continue;
            }
            if let Some((name, next)) = control_word_at(src, i) {
                if name == "string" {
                    i = tex_token_end_after_command(src, next);
                    continue;
                }
                if name == "begin" {
                    if let Some(span) = crate::parser::inert_environment_span_at(src, i) {
                        i = span.end;
                        continue;
                    }
                }
                if crate::parser::is_inline_literal_command(name) {
                    i = crate::parser::inline_literal_payload(src, name, next)
                        .map_or(bytes.len(), |(_, end)| end);
                    continue;
                }
                if brace_depth == 0 && env_depth == 0 && matches!(name, "tabularnewline" | "cr") {
                    rows.push(src[start..i].trim());
                    i = skip_row_terminator_suffix(src, next);
                    start = i;
                    continue;
                }
                if name == "begin" {
                    if let Some((_, end)) = command_group_after(src, next) {
                        env_depth += 1;
                        i = end;
                        continue;
                    }
                } else if name == "end" {
                    if let Some((_, end)) = command_group_after(src, next) {
                        env_depth = env_depth.saturating_sub(1);
                        i = end;
                        continue;
                    }
                }
                i = next;
                continue;
            }
            i = escaped_token_end(src, i);
            continue;
        }

        match bytes[i] {
            b'{' => brace_depth += 1,
            b'}' if brace_depth > 0 => brace_depth -= 1,
            _ => {}
        }
        i += char_width_at(src, i);
    }
    rows.push(src[start..].trim());
    rows
}

fn split_top_level_cells(src: &str) -> Vec<&str> {
    let bytes = src.as_bytes();
    let mut cells = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut brace_depth = 0i32;
    let mut env_depth = 0i32;

    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if let Some((name, next)) = control_word_at(src, i) {
                if name == "string" {
                    i = tex_token_end_after_command(src, next);
                    continue;
                }
                if name == "begin" {
                    if let Some(span) = crate::parser::inert_environment_span_at(src, i) {
                        i = span.end;
                        continue;
                    }
                }
                if crate::parser::is_inline_literal_command(name) {
                    i = crate::parser::inline_literal_payload(src, name, next)
                        .map_or(bytes.len(), |(_, end)| end);
                    continue;
                }
                if name == "begin" {
                    if let Some((_, end)) = command_group_after(src, next) {
                        env_depth += 1;
                        i = end;
                        continue;
                    }
                } else if name == "end" {
                    if let Some((_, end)) = command_group_after(src, next) {
                        env_depth = env_depth.saturating_sub(1);
                        i = end;
                        continue;
                    }
                }
                i = next;
                continue;
            }
            i = escaped_token_end(src, i);
            continue;
        }
        if bytes[i] == b'&' && brace_depth == 0 && env_depth == 0 {
            cells.push(src[start..i].trim());
            start = i + 1;
            i += 1;
            continue;
        }
        match bytes[i] {
            b'{' => brace_depth += 1,
            b'}' if brace_depth > 0 => brace_depth -= 1,
            _ => {}
        }
        i += char_width_at(src, i);
    }
    cells.push(src[start..].trim());
    cells
}

fn strip_longtable_metadata(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut copied = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += char_width_at(src, i);
            continue;
        }
        let Some((name, mut next)) = control_word_at(src, i) else {
            i = escaped_token_end(src, i);
            continue;
        };
        if name == "begin" {
            if let Some(span) = crate::parser::inert_environment_span_at(src, i) {
                i = span.end;
                continue;
            }
        }
        if crate::parser::is_inline_literal_command(name) {
            i = crate::parser::inline_literal_payload(src, name, next)
                .map_or(bytes.len(), |(_, end)| end);
            continue;
        }
        if name == "string" {
            i = tex_token_end_after_command(src, next);
            continue;
        }
        if matches!(name, "detokenize" | "unexpanded") {
            let group_start = skip_tex_space_and_comments(src, next);
            i = crate::parser::tex_group_end(src, group_start, b'{', b'}').unwrap_or(bytes.len());
            continue;
        }
        if !matches!(name, "caption" | "label") {
            i = next;
            continue;
        }
        if name == "caption" && src.as_bytes().get(next) == Some(&b'*') {
            next += 1;
        }
        next = skip_ws(src, next);
        if name == "caption" && src.as_bytes().get(next) == Some(&b'[') {
            let Some((_, after_optional)) = read_group(src, next, b'[', b']') else {
                i = next;
                continue;
            };
            next = skip_ws(src, after_optional);
        }
        let Some((_, after_arg)) = read_group(src, next, b'{', b'}') else {
            i = next;
            continue;
        };
        out.push_str(&src[copied..i]);
        copied = after_arg;
        i = after_arg;
    }
    out.push_str(&src[copied..]);
    out
}

fn prepare_longtable_content(src: &str) -> String {
    let stripped = strip_longtable_metadata(src);
    let markers = top_level_longtable_markers(&stripped);
    let header_start = markers
        .iter()
        .filter(|(name, _, _)| matches!(*name, "endfirsthead" | "endhead"))
        .min_by_key(|(_, start, _)| *start)
        .map(|(_, start, _)| *start);
    let footer_present = markers
        .iter()
        .any(|(name, _, _)| matches!(*name, "endfoot" | "endlastfoot"));
    if header_start.is_none() && !footer_present {
        return stripped;
    }
    let body_start = markers
        .iter()
        .map(|(_, _, end)| *end)
        .max()
        .unwrap_or_default();
    if let Some(header_start) = header_start {
        if body_start <= header_start {
            return stripped;
        }
        return format!(
            "{}\n{}",
            stripped[..header_start].trim_end(),
            stripped[body_start..].trim_start()
        );
    }
    stripped[body_start..].trim_start().to_string()
}

fn top_level_longtable_markers(src: &str) -> Vec<(&str, usize, usize)> {
    let bytes = src.as_bytes();
    let mut markers = Vec::new();
    let mut brace_depth = 0i32;
    let mut env_depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if let Some((name, next)) = control_word_at(src, i) {
                if name == "begin" {
                    if let Some(span) = crate::parser::inert_environment_span_at(src, i) {
                        i = span.end;
                        continue;
                    }
                }
                if crate::parser::is_inline_literal_command(name) {
                    i = crate::parser::inline_literal_payload(src, name, next)
                        .map_or(bytes.len(), |(_, end)| end);
                    continue;
                }
                if name == "string" {
                    i = tex_token_end_after_command(src, next);
                    continue;
                }
                if matches!(name, "detokenize" | "unexpanded") {
                    let group_start = skip_tex_space_and_comments(src, next);
                    i = crate::parser::tex_group_end(src, group_start, b'{', b'}')
                        .unwrap_or(bytes.len());
                    continue;
                }
                if name == "begin" {
                    if let Some((_, end)) = command_group_after(src, next) {
                        env_depth += 1;
                        i = end;
                        continue;
                    }
                } else if name == "end" {
                    if let Some((_, end)) = command_group_after(src, next) {
                        env_depth = env_depth.saturating_sub(1);
                        i = end;
                        continue;
                    }
                } else if brace_depth == 0
                    && env_depth == 0
                    && matches!(name, "endfirsthead" | "endhead" | "endfoot" | "endlastfoot")
                {
                    markers.push((name, i, next));
                }
                i = next;
                continue;
            }
            i = escaped_token_end(src, i);
            continue;
        }
        match bytes[i] {
            b'{' => brace_depth += 1,
            b'}' if brace_depth > 0 => brace_depth -= 1,
            _ => {}
        }
        i += char_width_at(src, i);
    }
    markers
}

fn alignment_from_decorator(src: &str) -> Option<Alignment> {
    if src.contains(r"\raggedleft") {
        Some(Alignment::Right)
    } else if src.contains(r"\centering") {
        Some(Alignment::Center)
    } else if src.contains(r"\raggedright") {
        Some(Alignment::Left)
    } else {
        None
    }
}

fn parse_alignment(src: &str) -> Option<Alignment> {
    match src.trim() {
        "l" => Some(Alignment::Left),
        "c" => Some(Alignment::Center),
        "r" => Some(Alignment::Right),
        _ => None,
    }
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
                prefix.trim_end_matches('*').parse::<f64>().ok()?
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

fn latex_column_dimension_to_css(raw: &str) -> Option<String> {
    let compact = strip_wrapping_braces(raw.trim())
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    for unit in [r"\textwidth", r"\linewidth", r"\columnwidth", r"\hsize"] {
        if let Some(prefix) = compact.strip_suffix(unit) {
            let factor = if prefix.is_empty() {
                1.0
            } else {
                prefix.trim_end_matches('*').parse::<f64>().ok()?
            };
            return Some(format!("{}cqi", css_number(factor * 100.0)));
        }
    }
    latex_dimension_to_css(&compact)
}

fn skip_row_terminator_suffix(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    i = skip_ws(src, i);
    if bytes.get(i) == Some(&b'*') {
        i += 1;
        i = skip_ws(src, i);
    }
    if bytes.get(i) == Some(&b'[') {
        if let Some((_, next)) = read_group(src, i, b'[', b']') {
            i = next;
        }
    }
    i
}

fn skip_optional_dimension(src: &str, mut i: usize) -> usize {
    i = skip_ws(src, i);
    if src.as_bytes().get(i) == Some(&b'[') {
        if let Some((_, next)) = read_group(src, i, b'[', b']') {
            return next;
        }
    }
    i
}

fn command_group_after(src: &str, from: usize) -> Option<(&str, usize)> {
    read_group(src, skip_ws(src, from), b'{', b'}')
}

fn control_word_at(src: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'\\') || !bytes.get(start + 1)?.is_ascii_alphabetic() {
        return None;
    }
    let mut end = start + 1;
    while end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'@') {
        end += 1;
    }
    Some((&src[start + 1..end], end))
}

fn read_group(src: &str, start: usize, open: u8, close: u8) -> Option<(&str, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&open) {
        return None;
    }
    let mut depth = 1i32;
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                i = escaped_token_end(src, i);
                continue;
            }
            byte if byte == open => depth += 1,
            byte if byte == close => {
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

fn skip_ws(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn skip_tex_space_and_comments(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
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

fn tex_token_end_after_command(src: &str, after_command: usize) -> usize {
    let start = skip_tex_space_and_comments(src, after_command);
    if start >= src.len() {
        return src.len();
    }
    if src.as_bytes()[start] != b'\\' {
        return start + char_width_at(src, start);
    }
    control_word_at(src, start)
        .map(|(_, end)| end)
        .unwrap_or_else(|| escaped_token_end(src, start))
}

fn char_width_at(src: &str, i: usize) -> usize {
    if src.as_bytes()[i].is_ascii() {
        1
    } else {
        src[i..].chars().next().map_or(1, char::len_utf8)
    }
}

fn escaped_token_end(src: &str, start: usize) -> usize {
    let after_slash = start.saturating_add(1);
    if after_slash >= src.len() {
        return src.len();
    }
    let width = src[after_slash..].chars().next().map_or(0, char::len_utf8);
    after_slash.saturating_add(width).min(src.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> LabelTable {
        LabelTable::default()
    }

    #[test]
    fn column_specs_cover_alignment_width_rules_and_repetition() {
        let columns = parse_column_spec(r"|l|>{\raggedleft}p{2cm}@{\quad}*{2}{c}|", 0);
        assert_eq!(columns.len(), 4);
        assert_eq!(columns[0].alignment, Alignment::Left);
        assert!(columns[0].rule_left);
        assert_eq!(columns[1].alignment, Alignment::Right);
        assert_eq!(columns[1].width.as_deref(), Some("2cm"));
        assert!(columns[1].wraps);
        assert_eq!(columns[2].alignment, Alignment::Center);
        assert_eq!(columns[3].alignment, Alignment::Center);
        assert!(columns[3].rule_right);
    }

    #[test]
    fn column_specs_consume_siunitx_options_and_preserve_paragraph_geometry() {
        let columns = parse_column_spec(r"S[table-format=2.2]rp{.4\textwidth}m{2cm}b{1in}", 0);
        assert_eq!(columns.len(), 5);
        assert_eq!(columns[0].alignment, Alignment::Right);
        assert_eq!(columns[1].alignment, Alignment::Right);
        assert_eq!(columns[2].vertical, VerticalAlignment::Top);
        assert_eq!(columns[2].width.as_deref(), Some("40cqi"));
        assert_eq!(columns[3].vertical, VerticalAlignment::Middle);
        assert_eq!(columns[4].vertical, VerticalAlignment::Bottom);
    }

    #[test]
    fn repeated_column_specs_have_a_global_output_cap() {
        let columns = parse_column_spec(r"*{128}{*{128}{*{128}{c}}}", 0);
        assert_eq!(columns.len(), MAX_COLUMNS);
    }

    #[test]
    fn row_and_cell_splitters_ignore_nested_separators() {
        let body =
            r"A \& B & \textbf{group {with & sign}} & \begin{matrix}a&b\\c&d\end{matrix}\\ C&D";
        let rows = split_top_level_rows(body);
        assert_eq!(rows.len(), 2);
        let first = split_top_level_cells(rows[0]);
        assert_eq!(first.len(), 3);
        assert!(first[0].contains(r"\&"));
        assert!(first[1].contains("with & sign"));
        assert!(first[2].contains(r"\begin{matrix}"));
    }

    #[test]
    fn row_and_cell_splitters_keep_literals_and_unicode_escapes_inert() {
        let body = r"\verbλa&bλ & \lstinline|x\\y| & \λ\\ C&D";
        let rows = split_top_level_rows(body);
        assert_eq!(rows.len(), 2);
        let first = split_top_level_cells(rows[0]);
        assert_eq!(first.len(), 3);
        assert_eq!(first[0], r"\verbλa&bλ");
        assert_eq!(first[1], r"\lstinline|x\\y|");
        assert_eq!(first[2], r"\λ");
    }

    #[test]
    fn row_and_cell_splitters_keep_stringified_separators_inert() {
        let rows = split_top_level_rows(r"A \string\\ B & right\\ C&D");
        assert_eq!(rows, [r"A \string\\ B & right", "C&D"]);
        let cells = split_top_level_cells(r"A \string& B & right");
        assert_eq!(cells, [r"A \string& B", "right"]);
    }

    #[test]
    fn table_comment_stripping_keeps_percent_inside_inline_literals() {
        let html = render_tabular(
            "tabular",
            concat!(
                "{ll}\n",
                "\\verb|50%| & ok\\\\\n",
                "\\lstinline|a%b| & yes\\\\\n",
                "\\lstinline{c%d} & braced\\\\\n",
                "A & B\\\\ % executable comment\n",
            ),
            &labels(),
        )
        .expect("valid table");
        assert_eq!(html.matches("<tr").count(), 4, "{html}");
        assert!(html.contains(">50%</code>"), "{html}");
        assert!(html.contains(">a%b</code>"), "{html}");
        assert!(html.contains(">c%d</code>"), "{html}");
        assert!(html.contains(">ok</td>"), "{html}");
        assert!(html.contains(">yes</td>"), "{html}");
        assert!(html.contains(">braced</td>"), "{html}");
        assert!(!html.contains("executable comment"), "{html}");
    }

    #[test]
    fn escaped_text_ampersand_is_normalized_without_touching_math() {
        assert_eq!(
            normalize_text_ampersands(r"A \& B, \(X \& Y\), \[P \& Q\], and $M \& N$"),
            r"A & B, \(X \& Y\), \[P \& Q\], and $M \& N$"
        );
        assert_eq!(
            normalize_text_ampersands(r"\verb|\&| and \lstinline{a\&b}"),
            r"\verb|\&| and \lstinline{a\&b}"
        );
        assert_eq!(
            normalize_text_ampersands(r"\string\& and \detokenize{\& $} and \unexpanded{\& \(}"),
            r"\string\& and \detokenize{\& $} and \unexpanded{\& \(}"
        );
    }

    #[test]
    fn multicolumn_and_rules_render_semantically() {
        let html = render_tabular(
            "tabular",
            r"{|l|cr|}\toprule Name & \multicolumn{2}{c|}{Result}\\\midrule Ada & $x^2$ & 10\\\bottomrule",
            &labels(),
        )
        .expect("valid table");
        assert!(html.contains(r#"<table class="latex-tabular env-tabular""#));
        assert!(html.contains(r#"colspan="2""#));
        assert!(html.contains("rule-top-strong"));
        assert!(html.contains("rule-top"));
        assert!(html.contains("rule-bottom-strong"));
        assert!(html.contains(r#"class="math inline""#));
        assert!(!html.contains(r"\multicolumn"));
    }

    #[test]
    fn malformed_or_empty_tabular_uses_caller_fallback() {
        assert!(render_tabular("tabular", "missing preamble", &labels()).is_none());
        assert!(render_tabular("tabular", "{} A&B", &labels()).is_none());
        assert!(render_tabular("tabular", "{lc}", &labels()).is_none());
    }

    #[test]
    fn tabularx_width_and_text_commands_are_preserved() {
        let html = render_tabular(
            "tabularx",
            r"{\linewidth}{lX} Label & \textbf{A long value}\\Next & \textcolor{red}{Alert}",
            &labels(),
        )
        .expect("valid tabularx");
        assert!(html.contains(r#"style="width:100%""#));
        assert!(html.contains("<strong>A long value</strong>"));
        assert!(html.contains("Alert"));
        assert!(!html.contains(r"\textcolor"));
        assert!(html.contains("cell-wrap"));
    }

    #[test]
    fn longtable_keeps_first_header_and_body_without_continuation_sections() {
        let html = render_tabular(
            "longtable",
            concat!(
                "{lr}\n",
                "\\caption{Values}\\label{tab:values}\\label{tab:alias}\\\\\n",
                "\\toprule Name & Value\\\\\\midrule\n",
                "\\endfirsthead\n",
                "Continued & Header\\\\\n",
                "\\endhead\n",
                "Page & Footer\\\\\n",
                "\\endfoot\n",
                "Last & Footer\\\\\n",
                "\\endlastfoot\n",
                "Ada & 1\\\\\n",
            ),
            &labels(),
        )
        .expect("valid longtable");
        assert_eq!(html.matches("Name").count(), 1);
        assert!(html.contains("Ada"));
        assert!(!html.contains("Continued"));
        assert!(!html.contains("Footer"));
        assert!(!html.contains("endfirsthead"));
        assert!(html.contains(r#"id="tab-values""#));
        assert!(html.contains(r#"id="tab-alias" data-refkey="tab:alias""#));
    }

    #[test]
    fn longtable_caption_hides_nested_label_source() {
        let mut labels = labels();
        labels
            .number
            .insert("tab:values".to_string(), "1".to_string());
        let html = render_tabular(
            "longtable",
            "{lr}\n\\caption{Values\\label{tab:values}}\\\\\nAda & 1\\\\\n",
            &labels,
        )
        .expect("valid longtable");

        assert!(html.contains(r#"<span class="float-kind">Table 1.</span> Values"#));
        assert!(html.contains(r#"id="tab-values""#));
        assert!(!html.contains(r#"\label"#));
        assert!(!html.contains(">tab:values<"));
    }

    #[test]
    fn longtable_metadata_scanner_keeps_stringified_and_detokenized_labels() {
        let html = render_tabular(
            "longtable",
            concat!(
                "{l}\n",
                "\\detokenize{\\label{literal}}\\\\\n",
                "\\string\\label{string}\\\\\n",
            ),
            &labels(),
        )
        .expect("valid longtable");
        assert!(html.contains(r#"\label{literal}"#), "{html}");
        assert!(html.contains(r#"\labelstring"#), "{html}");
    }

    #[test]
    fn longtable_with_only_footer_definitions_keeps_just_body_rows() {
        let html = render_tabular(
            "longtable",
            concat!(
                "{lr}\n",
                "Page & Footer\\\\\n",
                "\\endfoot\n",
                "Last & Footer\\\\\n",
                "\\endlastfoot\n",
                "Ada & 1\\\\\n",
            ),
            &labels(),
        )
        .expect("valid longtable");
        assert!(html.contains("Ada"));
        assert!(!html.contains("Page"));
        assert!(!html.contains("Last"));
        assert!(!html.contains("Footer"));
        assert!(!html.contains("endfoot"));
    }
}
