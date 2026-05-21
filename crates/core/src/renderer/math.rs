//! Math row splitting, equation numbering, ref resolution, label aliases, and
//! float placeholders (figure/table with `\includegraphics`). Used by the
//! `write_node` dispatcher in `renderer.rs`.

use std::collections::HashMap;
use std::fmt::Write;

use crate::ast::RefKind;
use crate::numbering::LabelTable;

use super::util::{
    asset_url, css_number, escape_attr, escape_html, escape_math, escape_tex_text, fnv_hash,
    is_relax_option, latex_command_arg, latex_command_args, latex_command_call, latex_optional_arg,
    parse_latex_number, parse_number_prefix, refkey_attr, sanitize_id, strip_wrapping_braces,
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

pub(super) fn math_row_labels(body: &str) -> Vec<Vec<String>> {
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

/// Remove `\label{anything}` from a math body. Brace-balanced so labels
/// containing nested braces (rare but legal) are handled.
pub(super) fn strip_labels(body: &str) -> String {
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

pub(super) fn render_latex_text_with_math(s: &str, labels: &LabelTable) -> String {
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
            out.push_str(&super::render_inline_latex(&s[text_start..i], labels));
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
            out.push_str(&super::render_inline_latex(&s[math_start..], labels));
            text_start = s.len();
            break;
        }
        let body = &s[math_start..i];
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
        i += 1;
        text_start = i;
    }

    if text_start < s.len() {
        out.push_str(&super::render_inline_latex(&s[text_start..], labels));
    }
    out
}

pub(super) fn write_float_placeholder(
    out: &mut String,
    env: &str,
    body: &str,
    labels: &LabelTable,
) {
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
