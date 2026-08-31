//! Math row splitting, equation numbering, ref resolution, label aliases, and
//! float placeholders (figure/table with `\includegraphics`). Used by the
//! `write_node` dispatcher in `renderer.rs`.

use std::collections::HashMap;
use std::fmt::Write;

use crate::ast::{Pos, RefKind};
use crate::numbering::LabelTable;
use crate::sync::MathRow;

use super::util::{
    asset_url, css_number, escape_attr, escape_html, escape_math, escape_tex_text, fnv_hash,
    is_relax_option, latex_command_args, latex_command_call, latex_optional_arg, parse_latex_number,
    parse_number_prefix, refkey_attr, sanitize_id, strip_wrapping_braces,
};

pub(super) fn equation_number_html(number: Option<&str>, row_numbers: &[Option<String>]) -> String {
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

pub(super) fn equation_row_refkey_html(
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
    // Note: this list is no longer aria-hidden; each chip is now clickable
    // (mirrors the .refkey-chip injected by the viewer for non-row labels)
    // so screen readers can reach the pin-by-refkey shortcut too.
    let mut out = String::from(r#"<span class="eq-refkey-list">"#);
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
                    r#"<span class="eq-refkey-chip"{id} data-target="{key}" tabindex="0" title="pin {key} to margin">{label}</span>"#,
                    id = id_attr,
                    key = escape_attr(label),
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

pub(super) fn math_row_labels(body: &str) -> Vec<Vec<String>> {
    let mut seen = Vec::<String>::new();
    let mut rows = split_math_rows(body);
    // A trailing `\\` leaves an empty final row that MathJax does NOT render as
    // a table row — drop it so the refkey row list lines up with the rendered
    // rows (and with `row_numbers`, which drops it the same way). An empty row
    // cannot carry a `\label`, so no labels are lost.
    if rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows.into_iter()
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

/// Remove `\label{anything}` from a math body. Brace-balanced so labels
/// containing nested braces (rare but legal) are handled.
pub(super) fn strip_labels(body: &str) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let word_start = i + 1;
            let mut word_end = word_start;
            while word_end < bytes.len()
                && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
            {
                word_end += 1;
            }
            if word_end == word_start {
                let width = body[word_start..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8);
                let end = (word_start + width).min(bytes.len());
                out.push_str(&body[i..end]);
                i = end;
                continue;
            }
            let word = &body[word_start..word_end];
            if crate::parser::is_inline_literal_command(word) {
                let end = crate::parser::inline_literal_payload(body, word, word_end)
                    .map(|(_, end)| end)
                    .unwrap_or(bytes.len());
                out.push_str(&body[i..end]);
                i = end;
                continue;
            }
            if matches!(word, "detokenize" | "unexpanded") {
                let mut group_start = word_end;
                while group_start < bytes.len() && bytes[group_start].is_ascii_whitespace() {
                    group_start += 1;
                }
                let end = crate::parser::tex_group_end(body, group_start, b'{', b'}')
                    .unwrap_or(bytes.len());
                out.push_str(&body[i..end]);
                i = end;
                continue;
            }
            if word == "string" {
                let mut end = word_end;
                while end < bytes.len() && bytes[end].is_ascii_whitespace() {
                    end += 1;
                }
                if end < bytes.len() && bytes[end] == b'\\' {
                    end += 1;
                    while end < bytes.len()
                        && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'@')
                    {
                        end += 1;
                    }
                } else if end < bytes.len() {
                    end += if bytes[end].is_ascii() {
                        1
                    } else {
                        body[end..].chars().next().map_or(1, char::len_utf8)
                    };
                }
                out.push_str(&body[i..end]);
                i = end;
                continue;
            }
            if word == "label" {
                let mut argument_start = word_end;
                while argument_start < bytes.len()
                    && bytes[argument_start].is_ascii_whitespace()
                {
                    argument_start += 1;
                }
                if let Some(end) =
                    crate::parser::tex_group_end(body, argument_start, b'{', b'}')
                {
                    i = end;
                    continue;
                }
            }
            out.push_str(&body[i..word_end]);
            i = word_end;
            continue;
        }
        let width = if bytes[i].is_ascii() {
            1
        } else {
            body[i..].chars().next().map_or(1, char::len_utf8)
        };
        out.push_str(&body[i..i + width]);
        i += width;
    }
    out
}

pub(super) fn resolve_math_refs(body: &str, labels: &LabelTable) -> String {
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

pub(super) fn label_alias_anchors(body: &str, primary: Option<&str>) -> String {
    let mut seen = Vec::<String>::new();
    let mut out = String::new();
    for label in crate::parser::live_braced_command_calls(body, &["label"], 0)
        .into_iter()
        .map(|call| call.value)
    {
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

/// Per-row source spans (1-based inclusive line range + start byte column) for
/// a multi-row display-math `body`, given the source line of the block's
/// `\begin{env}` (always the line the body starts on). Rows are the same
/// `\\`-split rows MathJax renders as table rows, so the i-th span corresponds
/// to the i-th rendered `mtr` — forward search highlights the rows an editor
/// selection covers; backward search jumps a click on a row to that row's own
/// source position. Uses each (trimmed) row slice's offset within `body` to
/// count the newlines before it; the trim means `start_col` lands on the row's
/// first non-whitespace char. A row that starts on the `\begin` line has no
/// knowable file column (`body` begins mid-line and we only know its line), so
/// its `start_col` is the 0 = unknown sentinel.
#[cfg(test)]
pub(super) fn math_row_spans(body: &str, start_line: u32) -> Vec<MathRow> {
    let rows = rendered_math_rows(body);
    math_row_spans_for_slices(body, &rows, start_line, &[], false)
}

fn math_row_spans_for_slices(
    body: &str,
    rows: &[&str],
    start_line: u32,
    source_lines: &[Pos],
    unknown_unmapped_columns: bool,
) -> Vec<MathRow> {
    let base = body.as_ptr() as usize;
    let bytes = body.as_bytes();
    let body_line_and_col = |off: usize| -> (usize, usize) {
        let off = off.min(bytes.len());
        let line = bytes[..off].iter().filter(|&&b| b == b'\n').count();
        let col = bytes[..off]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(off, |newline| off - newline - 1);
        (line, col)
    };
    let fallback_line = |off: usize| -> u32 {
        let off = off.min(bytes.len());
        start_line + bytes[..off].iter().filter(|&&b| b == b'\n').count() as u32
    };
    let fallback_col = |off: usize| -> u32 {
        match bytes[..off.min(bytes.len())]
            .iter()
            .rposition(|&b| b == b'\n')
        {
            Some(newline) => (off - newline) as u32,
            None => 0,
        }
    };

    rows.iter()
        .map(|row| {
            let off = (row.as_ptr() as usize)
                .saturating_sub(base)
                .min(bytes.len());
            let end = (off + row.len()).min(bytes.len());
            let content = row_content_offset(bytes, off, end);
            let (content_line, content_col) = body_line_and_col(content);
            let (end_line, _) = body_line_and_col(end);
            let mapped_start = source_lines.get(content_line);
            MathRow {
                start_line: mapped_start
                    .map(|position| position.line)
                    .unwrap_or_else(|| fallback_line(content)),
                end_line: source_lines
                    .get(end_line)
                    .map(|position| position.line)
                    .unwrap_or_else(|| fallback_line(end)),
                start_col: mapped_start.map_or_else(
                    || {
                        if unknown_unmapped_columns {
                            0
                        } else {
                            fallback_col(content)
                        }
                    },
                    |position| position.col + content_col as u32,
                ),
            }
        })
        .collect()
}

fn rendered_math_rows(src: &str) -> Vec<&str> {
    let mut rows = split_math_rows(src);
    // A trailing `\\` leaves an empty final row that MathJax does NOT render as
    // a table row — drop it so our row count/indices line up with the rendered
    // `mtr` rows. (Empty rows in the middle, from `\\ \\`, are kept: MathJax
    // does render those as blank rows.)
    if rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows
}

/// Where a row slice's REAL content starts. A slice can begin at a `%`
/// comment: split_math_rows skips comments only while SCANNING for `\\`, so a
/// trailing `% …` after the previous row's separator (or a full comment line
/// between rows) stays at the head of the next slice. The rendered row's
/// first token is the first non-whitespace char outside any comment — that's
/// where a backward jump must land and where a row copy must start, not the
/// comment (which can even be the PREVIOUS row's line). A leading literal
/// `\%` starts at the backslash, so it's safe. A row that is nothing but
/// comments renders as an empty table row; its own start is the best offset
/// it has.
fn row_content_offset(bytes: &[u8], off: usize, end: usize) -> usize {
    let mut i = off;
    loop {
        while i < end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < end && bytes[i] == b'%' {
            while i < end && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            break;
        }
    }
    if i >= end {
        off
    } else {
        i
    }
}

/// Byte spans — into the `\begin{env}…\end{env}` copy string, hence
/// `prefix_len` — of each rendered row's trimmed source, serialized as
/// `"s0:e0,s1:e1,…"` for the `data-row-tex-spans` attribute. Rows are the
/// same `\\`-split slices as `math_row_spans` (trailing empty row dropped),
/// so index i matches the i-th rendered `mtr` — letting a click on a row
/// copy exactly that row's LaTeX. Empty for single-row bodies. Offsets are
/// bytes into the RAW string (the client slices via TextEncoder, since the
/// attribute value unescapes back to the raw string).
#[cfg(test)]
pub(super) fn math_row_tex_spans(body: &str, prefix_len: usize) -> String {
    let rows = rendered_math_rows(body);
    math_row_tex_spans_for_slices(body, &rows, prefix_len)
}

fn math_row_tex_spans_for_slices(body: &str, rows: &[&str], prefix_len: usize) -> String {
    if rows.len() < 2 {
        return String::new();
    }
    let base = body.as_ptr() as usize;
    let bytes = body.as_bytes();
    rows.iter()
        .map(|r| {
            let off = (r.as_ptr() as usize).saturating_sub(base).min(body.len());
            let end = (off + r.len()).min(body.len());
            // Skip a leading comment left over from the previous row's tail —
            // copying "% done\n  c &= d" for a click on the "c &= d" row
            // would be junk on paste.
            let content = row_content_offset(bytes, off, end);
            format!("{}:{}", prefix_len + content, prefix_len + end)
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) struct DisplayMathRows {
    pub source_spans: Vec<MathRow>,
    pub tex_spans: String,
}

/// Normalize TeX and Markdown displays to the same logical row body, then
/// derive both source-sync rows and copy spans from that one slice set. TeX
/// math environments already store their interior in `body` (including any
/// environment arguments). Markdown keeps an inner
/// `aligned`/`gathered`-style wrapper in `body`. Project both representations
/// to the same argument-free row body before using the shared splitter.
pub(super) fn display_math_rows(
    body: &str,
    ast_environment: Option<&str>,
    markdown: bool,
    start_line: u32,
    source_lines: &[Pos],
    copy_prefix_len: usize,
) -> DisplayMathRows {
    let rows = if let Some(environment) = ast_environment {
        row_environment_content_start(body, environment, 0)
            .map(|content_start| rendered_math_rows(&body[content_start..]))
            .unwrap_or_default()
    } else if let Some(row_body) = outer_math_row_body(body) {
        rendered_math_rows(row_body)
    } else if markdown {
        // A bare `\\` inside `$$...$$` does not create MathJax table rows.
        // Do not publish a row map the browser cannot match to rendered mtrs.
        Vec::new()
    } else {
        // Preserve the established TeX behavior for raw `$$` / `\[...\]`.
        rendered_math_rows(body)
    };
    DisplayMathRows {
        source_spans: math_row_spans_for_slices(body, &rows, start_line, source_lines, markdown),
        tex_spans: math_row_tex_spans_for_slices(body, &rows, copy_prefix_len),
    }
}

/// Whether a Markdown display body is already a complete top-level display
/// environment. These environments must be handed to MathJax directly rather
/// than nested inside `\[...\]`; the inner `aligned`/`gathered` family still
/// needs the surrounding delimiter.
pub(super) fn markdown_standalone_display_environment(body: &str) -> bool {
    full_outer_environment(body).is_some_and(|outer| {
        matches!(
            outer.name,
            "equation"
                | "equation*"
                | "displaymath"
                | "align"
                | "align*"
                | "alignat"
                | "alignat*"
                | "flalign"
                | "flalign*"
                | "xalignat"
                | "xalignat*"
                | "xxalignat"
                | "gather"
                | "gather*"
                | "multline"
                | "multline*"
                | "eqnarray"
                | "eqnarray*"
        )
    })
}

const MAX_OUTER_MATH_WRAPPERS: usize = 8;
const MAX_MATH_ENVIRONMENT_NESTING: usize = 128;

fn outer_math_row_body(mut src: &str) -> Option<&str> {
    for _ in 0..MAX_OUTER_MATH_WRAPPERS {
        let outer = full_outer_environment(src)?;
        if is_row_environment(outer.name) {
            let content_start = row_environment_content_start(src, outer.name, outer.body_start)?;
            return Some(&src[content_start..outer.close_start]);
        }
        if !matches!(outer.name, "equation" | "equation*" | "displaymath") {
            return None;
        }
        src = &src[outer.body_start..outer.close_start];
    }
    None
}

fn is_row_environment(name: &str) -> bool {
    matches!(
        name,
        "align"
            | "align*"
            | "aligned"
            | "alignedat"
            | "alignat"
            | "alignat*"
            | "flalign"
            | "flalign*"
            | "xalignat"
            | "xalignat*"
            | "xxalignat"
            | "gather"
            | "gather*"
            | "gathered"
            | "multline"
            | "multline*"
            | "split"
            | "eqnarray"
            | "eqnarray*"
    )
}

struct OuterEnvironment<'a> {
    name: &'a str,
    body_start: usize,
    close_start: usize,
}

fn full_outer_environment(src: &str) -> Option<OuterEnvironment<'_>> {
    let begin_start = skip_math_whitespace_and_comments(src, 0);
    let (name, body_start) = latex_environment_command(src, begin_start, "begin")?;
    let mut stack = vec![name];
    let mut i = body_start;
    while i < src.len() {
        if src.as_bytes()[i] == b'%' {
            i += 1;
            while i < src.len() && src.as_bytes()[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if let Some((nested, end)) = latex_environment_command(src, i, "begin") {
            if stack.len() >= MAX_MATH_ENVIRONMENT_NESTING {
                return None;
            }
            stack.push(nested);
            i = end;
            continue;
        }
        if let Some((closing, end)) = latex_environment_command(src, i, "end") {
            if stack.pop() != Some(closing) {
                return None;
            }
            if stack.is_empty() {
                return (skip_math_whitespace_and_comments(src, end) == src.len()).then_some(
                    OuterEnvironment {
                        name,
                        body_start,
                        close_start: i,
                    },
                );
            }
            i = end;
            continue;
        }
        if src.as_bytes()[i] == b'\\' && i + 1 < src.len() {
            let escaped_width = src[i + 1..].chars().next()?.len_utf8();
            i += 1 + escaped_width;
        } else {
            let width = src[i..].chars().next()?.len_utf8();
            i += width;
        }
    }
    None
}

fn latex_environment_command<'a>(
    src: &'a str,
    at: usize,
    command: &str,
) -> Option<(&'a str, usize)> {
    let prefix = if command == "begin" {
        r"\begin"
    } else {
        r"\end"
    };
    if !src.get(at..)?.starts_with(prefix) {
        return None;
    }
    let i = skip_math_whitespace_and_comments(src, at + prefix.len());
    if src.as_bytes().get(i) != Some(&b'{') {
        return None;
    }
    let name_start = i + 1;
    let name_end = src[name_start..]
        .find('}')
        .map(|offset| name_start + offset)?;
    let name = src[name_start..name_end].trim();
    (!name.is_empty()).then_some((name, name_end + 1))
}

fn row_environment_content_start(src: &str, environment: &str, mut at: usize) -> Option<usize> {
    let base_name = environment.trim_end_matches('*');

    if matches!(base_name, "aligned" | "alignedat" | "gathered") {
        if let Some(end) = optional_math_group_end(src, at, b'[', b']') {
            at = end;
        }
    }
    if matches!(
        base_name,
        "alignedat" | "alignat" | "xalignat" | "xxalignat"
    ) {
        at = required_math_group_end(src, at, b'{', b'}')?;
    }
    Some(at)
}

fn optional_math_group_end(src: &str, at: usize, open: u8, close: u8) -> Option<usize> {
    let group_start = skip_math_whitespace_and_comments(src, at);
    (src.as_bytes().get(group_start) == Some(&open))
        .then(|| balanced_math_group_end(src, group_start, open, close))?
}

fn required_math_group_end(src: &str, at: usize, open: u8, close: u8) -> Option<usize> {
    let group_start = skip_math_whitespace_and_comments(src, at);
    (src.as_bytes().get(group_start) == Some(&open))
        .then(|| balanced_math_group_end(src, group_start, open, close))?
}

fn balanced_math_group_end(src: &str, start: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            byte if byte == open => {
                depth += 1;
                i += 1;
            }
            byte if byte == close => {
                depth = depth.checked_sub(1)?;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => i += 1,
        }
    }
    None
}

fn skip_math_whitespace_and_comments(src: &str, mut at: usize) -> usize {
    let bytes = src.as_bytes();
    loop {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if bytes.get(at) != Some(&b'%') {
            return at;
        }
        while at < bytes.len() && bytes[at] != b'\n' {
            at += 1;
        }
    }
}

pub(super) fn split_math_rows(src: &str) -> Vec<&str> {
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
                // Skip the backslash and the escaped char. The escaped char may
                // be multibyte (`\λ`, `\Δ`); a blind `i += 2` would split it and
                // panic on the next `src[i..]` slice.
                let next_w = src[i + 1..].chars().next().map_or(1, |c| c.len_utf8());
                i += 1 + next_w;
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
    if bytes.get(i) == Some(&b'*') {
        i += 1;
    }
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

/// Emit a lightweight inline math span (no source-sync id) for `body`, the
/// LaTeX between a `$…$` pair. Shared by `render_latex_text_with_math` and the
/// `$…$` branch of `render_inline_latex` so both produce identical markup.
pub(super) fn write_inline_math_span(out: &mut String, body: &str) {
    let copy_tex = format!(r"\({body}\)");
    write!(
        out,
        r#"<span class="math inline" data-hash="{hash}" data-tex="{copy_tex}" data-mathjax-tex="{mathjax_tex}" tabindex="0" title="Copy as LaTeX"><span class="math-source">\({math}\)</span></span>"#,
        hash = fnv_hash(&format!("i:{body}")),
        copy_tex = escape_attr(&copy_tex),
        mathjax_tex = escape_attr(&copy_tex),
        math = escape_math(body),
    )
    .unwrap();
}

pub(super) fn render_latex_text_with_math(s: &str, labels: &LabelTable) -> String {
    // `render_inline_latex` now owns both inline-math delimiters and TeX
    // grouping. Passing the full string through one call is important:
    // splitting at `$...$` here used to break a surrounding
    // `{\color{...} text $x$}` scope into three unrelated fragments.
    super::render_inline_latex(s, labels)
}

pub(super) fn write_float_placeholder(
    out: &mut String,
    env: &str,
    body: &str,
    labels: &LabelTable,
    float_number: Option<&str>,
    rendered_asset: Option<&str>,
) {
    let live_body = crate::parser::executable_latex_source(body);
    let kind = if env.trim_end_matches('*') == "table" {
        "Table"
    } else {
        "Figure"
    };
    let float_labels = crate::parser::live_braced_command_calls(&live_body, &["label"], 0)
        .into_iter()
        .map(|call| call.value)
        .collect::<Vec<_>>();
    let primary_label = float_labels.first().map(String::as_str);
    let id_attr = primary_label
        .map(|label| format!(r#" id="{}""#, escape_attr(&sanitize_id(label))))
        .unwrap_or_default();
    let refkey = refkey_attr(primary_label);
    let alias_html = label_alias_anchors(&live_body, primary_label);
    let kind_label = float_number
        .map(str::to_string)
        .or_else(|| primary_label.and_then(|label| labels.number.get(label).cloned()))
        .map(|number| format!("{kind} {}.", escape_html(&number)))
        .unwrap_or_else(|| format!("{kind}."));
    let caption = crate::parser::live_braced_command_calls(&live_body, &["caption"], 1)
        .into_iter()
        .next();
    let asset = latex_command_call(&live_body, "includegraphics");
    let caption_html = caption
        .as_ref()
        .map(|call| render_latex_text_with_math(strip_labels(&call.value).trim(), labels))
        .unwrap_or_else(|| "content omitted from preview".to_string());
    let caption_prefix = if caption.as_ref().is_some_and(|call| call.starred) {
        String::new()
    } else {
        format!(r#"<span class="float-kind">{kind_label}</span> "#)
    };
    let asset_html = rendered_asset
        .map(str::to_string)
        .or_else(|| {
            asset
                .as_ref()
                .map(|call| render_float_asset(&call.arg, call.optional.as_deref()))
        })
        .unwrap_or_default();
    writeln!(
        out,
        r#"<figure class="float-placeholder float-{env}"{id}{refkey} data-env="{env}">{aliases}{asset}<figcaption>{caption_prefix}{caption}</figcaption></figure>"#,
        env = escape_attr(env),
        id = id_attr,
        refkey = refkey,
        aliases = alias_html,
        asset = asset_html,
        caption_prefix = caption_prefix,
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

pub(super) fn write_flow_marker(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(start_line: u32, end_line: u32, start_col: u32) -> MathRow {
        MathRow {
            start_line,
            end_line,
            start_col,
        }
    }

    fn copied_rows<'a>(copy_tex: &'a str, spans: &str) -> Vec<&'a str> {
        spans
            .split(',')
            .map(|span| {
                let (start, end) = span.split_once(':').unwrap();
                &copy_tex[start.parse::<usize>().unwrap()..end.parse::<usize>().unwrap()]
            })
            .collect()
    }

    #[test]
    fn math_row_spans_maps_rows_to_source_lines() {
        // Body as captured after `\begin{align}` on source line 3: a leading
        // newline, then one row per line (4, 5, 6). `\\` is the LaTeX row sep.
        let body = "\na &= b \\\\\nc &= d \\\\\ne &= f\n";
        assert_eq!(
            math_row_spans(body, 3),
            vec![row(4, 4, 1), row(5, 5, 1), row(6, 6, 1)]
        );
    }

    #[test]
    fn math_row_spans_start_col_lands_on_first_token() {
        // Indented rows: start_col points at the first non-whitespace char
        // (byte col, 1-based), so a backward jump lands ON the row's content.
        let body = "\n  a &= b \\\\\n    c &= d\n";
        assert_eq!(math_row_spans(body, 3), vec![row(4, 4, 3), row(5, 5, 5)]);
    }

    #[test]
    fn math_row_spans_single_row_is_one_range() {
        // A one-liner align (all on the \begin line, source line 3). The file
        // column of the row is unknowable from the body slice → 0 sentinel.
        let body = " a &= b ";
        assert_eq!(math_row_spans(body, 3), vec![row(3, 3, 0)]);
    }

    #[test]
    fn math_row_spans_drops_trailing_backslash_row() {
        // A final `\\` must not add a phantom row — MathJax renders 2 rows here.
        let body = "\na &= b \\\\\nc &= d \\\\\n";
        assert_eq!(math_row_spans(body, 3), vec![row(4, 4, 1), row(5, 5, 1)]);
    }

    #[test]
    fn math_row_spans_multiline_row_spans_its_lines() {
        // One logical row wrapped across two source lines: the span covers
        // both, start_col still lands on the first token.
        let body = "\na &= b\n  + c \\\\\nd &= e\n";
        assert_eq!(math_row_spans(body, 3), vec![row(4, 5, 1), row(6, 6, 1)]);
    }

    #[test]
    fn math_row_tex_spans_are_trimmed_row_slices_with_prefix() {
        // \begin{align} is 13 bytes; "a &= b" sits at body bytes 1..7 and
        // "c &= d" at 11..17.
        let body = "\na &= b \\\\\nc &= d\n";
        assert_eq!(math_row_tex_spans(body, 13), "14:20,24:30");
        // Single-row bodies get no spans (no row selection to offer).
        assert_eq!(math_row_tex_spans(" a = b ", 13), "");
    }

    #[test]
    fn math_row_tex_spans_skip_leading_comment_from_previous_row() {
        // "% done" belongs to row 0's line; row 1's copy must start at "c".
        let body = "\na &= b \\\\ % done\nc &= d\n";
        // row 0: "a &= b" at body 1..7 → 14:20; row 1 slice starts at the
        // comment (body 11) but content "c &= d" is at 18..24 → 31:37.
        assert_eq!(math_row_tex_spans(body, 13), "14:20,31:37");
    }

    #[test]
    fn math_row_spans_skips_trailing_comment_after_separator() {
        // A `% …` after the previous row's `\\` heads the next row's slice —
        // the jump target must be the row's real first token on line 5, not
        // the comment back on line 4.
        let body = "\na &= b \\\\ % done\nc &= d\n";
        assert_eq!(math_row_spans(body, 3), vec![row(4, 4, 1), row(5, 5, 1)]);
    }

    #[test]
    fn math_row_spans_skips_full_comment_lines_between_rows() {
        // Whole comment lines between rows likewise belong to the next slice;
        // the row's position is its first non-comment token (line 6, col 1).
        let body = "\na &= b \\\\\n% explain\nc &= d\n";
        assert_eq!(math_row_spans(body, 3), vec![row(4, 4, 1), row(6, 6, 1)]);
    }

    #[test]
    fn markdown_outer_aligned_uses_shared_rows_with_exact_source_columns() {
        let body = "\n\\begin{aligned}\n  α &= β \\\\\n    γ &= δ\n\\end{aligned}\n";
        let source_lines = (20..=25)
            .map(|line| Pos {
                line,
                col: 3,
                byte: 0,
            })
            .collect::<Vec<_>>();
        let rows = display_math_rows(body, None, true, 20, &source_lines, 2);

        assert_eq!(rows.source_spans, vec![row(22, 22, 5), row(23, 23, 7)]);
        let copy_tex = format!(r"\[{body}\]");
        let copied = rows
            .tex_spans
            .split(',')
            .map(|span| {
                let (start, end) = span.split_once(':').unwrap();
                &copy_tex[start.parse::<usize>().unwrap()..end.parse::<usize>().unwrap()]
            })
            .collect::<Vec<_>>();
        assert_eq!(copied, ["α &= β", "γ &= δ"]);
    }

    #[test]
    fn markdown_outer_rows_ignore_nested_matrix_rows() {
        let body = concat!(
            "\\begin{aligned}\n",
            "a &= \\begin{matrix} 1 \\\\ 2 \\end{matrix} \\\\\n",
            "b &= 3\n",
            "\\end{aligned}",
        );
        let rows = display_math_rows(body, None, true, 1, &[], 2);
        assert_eq!(rows.source_spans.len(), 2);
        assert_eq!(rows.source_spans[0].start_line, 2);
        assert_eq!(rows.source_spans[1].start_line, 3);
    }

    #[test]
    fn markdown_only_standalone_equation_environments_skip_outer_delimiters() {
        for environment in ["align", "align*", "gather", "multline", "equation"] {
            let body = format!(r"\begin{{{environment}}}x = y\end{{{environment}}}");
            assert!(
                markdown_standalone_display_environment(&body),
                "{environment}"
            );
        }
        for environment in ["aligned", "alignedat", "gathered", "split"] {
            let body = format!(r"\begin{{{environment}}}x = y\end{{{environment}}}");
            assert!(
                !markdown_standalone_display_environment(&body),
                "{environment}"
            );
        }
    }

    #[test]
    fn markdown_environment_commands_allow_tex_comment_continuations() {
        let body = concat!(
            "\\begin% opening comment\n",
            "{align}\n",
            "a &= b \\\\\n",
            "c &= d\n",
            "\\end% closing comment\n",
            "{align}",
        );
        assert!(markdown_standalone_display_environment(body));

        let source_lines = (10..=15)
            .map(|line| Pos {
                line,
                col: 1,
                byte: 0,
            })
            .collect::<Vec<_>>();
        let rows = display_math_rows(body, None, true, 10, &source_lines, 0);
        assert_eq!(rows.source_spans, vec![row(12, 12, 1), row(13, 13, 1)]);
    }

    #[test]
    fn markdown_bare_row_separators_and_partial_wrappers_publish_no_rows() {
        let bare = display_math_rows("a &= b \\\\ c &= d", None, true, 1, &[], 2);
        assert!(bare.source_spans.is_empty());
        assert!(bare.tex_spans.is_empty());

        let partial = display_math_rows(
            "\\begin{aligned}a &= b \\\\ c &= d\\end{aligned} trailing",
            None,
            true,
            1,
            &[],
            2,
        );
        assert!(partial.source_spans.is_empty());
    }

    #[test]
    fn markdown_alignedat_skips_its_column_count_before_first_row() {
        let body =
            "\\begin{alignedat}[t]{2}\n a&=b &\\quad c&=d \\\\\n e&=f & g&=h\n\\end{alignedat}";
        let rows = display_math_rows(body, None, true, 7, &[], 2);
        assert_eq!(rows.source_spans, vec![row(8, 8, 0), row(9, 9, 0)]);

        let copy_tex = format!(r"\[{body}\]");
        let first = rows.tex_spans.split(',').next().unwrap();
        let (start, end) = first.split_once(':').unwrap();
        assert_eq!(
            &copy_tex[start.parse::<usize>().unwrap()..end.parse::<usize>().unwrap()],
            "a&=b &\\quad c&=d"
        );
    }

    #[test]
    fn tex_and_markdown_alignment_arguments_share_row_projection() {
        for environment in ["alignat", "alignat*", "xalignat", "xalignat*", "xxalignat"] {
            let tex_body = "{2}\n  a&=b & c&=d \\\\\n    e&=f & g&=h\n";
            let tex_prefix = format!(r"\begin{{{environment}}}");
            let tex_copy = format!(r"{tex_prefix}{tex_body}\end{{{environment}}}");
            let tex_rows =
                display_math_rows(tex_body, Some(environment), false, 7, &[], tex_prefix.len());

            let markdown_body = format!(r"\begin{{{environment}}}{tex_body}\end{{{environment}}}");
            let source_lines = (7..=10)
                .map(|line| Pos {
                    line,
                    col: 1,
                    byte: 0,
                })
                .collect::<Vec<_>>();
            let markdown_rows = display_math_rows(&markdown_body, None, true, 7, &source_lines, 0);

            assert_eq!(
                tex_rows.source_spans, markdown_rows.source_spans,
                "{environment}"
            );
            assert_eq!(
                copied_rows(&tex_copy, &tex_rows.tex_spans),
                copied_rows(&markdown_body, &markdown_rows.tex_spans),
                "{environment}"
            );
            assert_eq!(
                copied_rows(&tex_copy, &tex_rows.tex_spans),
                ["a&=b & c&=d", "e&=f & g&=h"],
                "{environment}"
            );
        }
    }

    #[test]
    fn starred_row_separator_does_not_leak_star_into_next_row() {
        let body = "\na &= b \\\\* [3pt]\nc &= d\n";
        assert_eq!(math_row_spans(body, 3), vec![row(4, 4, 1), row(5, 5, 1)]);
        assert_eq!(math_row_tex_spans(body, 2), "3:9,20:26");
    }
}
