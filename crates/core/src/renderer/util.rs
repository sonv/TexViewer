//! Leaf string/escape/role helpers used across renderer submodules. These
//! depend only on the AST role enum and standard library — no `RenderCtx`,
//! no `LabelTable`. Moved out of `renderer.rs` to keep that file focused on
//! the AST → HTML dispatch.

use std::fmt::Write;

use crate::ast::{Role, Span};

pub(super) fn escape_html(s: &str) -> String {
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

pub(super) fn escape_attr(s: &str) -> String {
    escape_html(s)
}

/// Allow-list URL schemes for `href`s built from document content (e.g. a
/// `.bib` `url` field). Returns the attribute-escaped URL when the scheme is
/// safe (`http`/`https`/`mailto`/`ftp`) or the URL is relative/scheme-less;
/// returns `None` for anything else — notably `javascript:` and `data:` —
/// so the caller can drop or inert the link instead of emitting a clickable
/// XSS vector. Tabs/newlines/controls are stripped before sniffing the scheme
/// because browsers ignore them when resolving `javascript:` URLs.
pub(super) fn safe_url(u: &str) -> Option<String> {
    let stripped: String = u
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && !c.is_control())
        .collect();
    let lower = stripped.to_ascii_lowercase();
    let scheme = lower.split_once(':').and_then(|(scheme, _)| {
        let valid = !scheme.is_empty()
            && scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        valid.then_some(scheme)
    });
    let safe = match scheme {
        // Explicit scheme — only known-safe ones are allowed through.
        Some(s) => matches!(s, "http" | "https" | "mailto" | "ftp"),
        // No scheme: relative / protocol-relative / fragment URL.
        None => true,
    };
    safe.then(|| escape_attr(u.trim()))
}

pub(super) fn escape_math(s: &str) -> String {
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

/// Replace the user's `$HOME` prefix with `~` for display in the topbar.
/// Falls back to the full path if `$HOME` is unset or the file lives
/// outside it.
pub(super) fn shorten_home_path(path: &std::path::Path) -> String {
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

pub(super) fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

pub(super) fn sanitize_id(s: &str) -> String {
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

pub(super) fn fnv_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

pub(super) fn is_blank_line_separator(s: &str) -> bool {
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

pub(super) fn role_label(role: Role) -> &'static str {
    match role {
        Role::Main => "main",
        Role::Supporting => "supporting",
        Role::Standard => "standard",
        Role::Omitted => "omitted",
    }
}

pub(super) fn role_pill_html(role: Role) -> String {
    let label = role_label(role);
    let cls = match role {
        Role::Main => "role-pill role-main",
        Role::Supporting => "role-pill role-supporting",
        Role::Standard => "role-pill role-standard",
        Role::Omitted => "role-pill role-omitted",
    };
    format!(r#"<span class="{cls}">{label}</span>"#)
}

pub(super) fn data_src(span: &Span) -> String {
    format!(
        "{}:{}:{}",
        span.file.display(),
        span.start.line,
        span.start.col,
    )
}

pub(super) fn refkey_attr(label: Option<&str>) -> String {
    label
        .map(|label| format!(r#" data-refkey="{}""#, escape_attr(label)))
        .unwrap_or_default()
}

pub(super) fn escape_tex_text(s: &str) -> String {
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

pub(super) fn asset_url(path: &str) -> String {
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

pub(super) fn is_relax_option(s: &str) -> bool {
    let trimmed = s.trim();
    trimmed.is_empty() || trimmed == r"\relax"
}

pub(super) fn roman_upper(mut n: usize) -> String {
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

pub(super) fn strip_wrapping_braces(s: &str) -> &str {
    if s.starts_with('{') && s.ends_with('}') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

pub(super) fn parse_number_prefix(s: &str) -> Option<(f64, &str)> {
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

pub(super) fn parse_latex_number(s: &str) -> Option<f64> {
    let compact = strip_wrapping_braces(s.trim())
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    compact.parse().ok()
}

pub(super) fn css_number(value: f64) -> String {
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

pub(super) fn parse_balanced_bracket(
    src: &str,
    start: usize,
    open: u8,
    close: u8,
) -> Option<String> {
    let end = balanced_group_end(src, start, open, close)?;
    Some(src[start + 1..end - 1].to_string())
}

pub(super) fn balanced_group_end(src: &str, start: usize, open: u8, close: u8) -> Option<usize> {
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

pub(super) fn latex_optional_arg(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b'[' {
        i += 1;
    }
    parse_balanced_bracket(raw, i, b'[', b']')
}

pub(super) fn latex_optional_usize(raw: &str) -> Option<usize> {
    latex_optional_arg(raw)?.trim().parse().ok()
}

pub(super) fn latex_command_arg(src: &str, command: &str) -> Option<String> {
    latex_command_args(src, command).into_iter().next()
}

pub(super) struct LatexCommandCall {
    pub(super) optional: Option<String>,
    pub(super) arg: String,
}

pub(super) fn latex_command_call(src: &str, command: &str) -> Option<LatexCommandCall> {
    let needle = format!("\\{command}");
    let bytes = src.as_bytes();
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        // Byte-compare the (ASCII) needle so `i` need not be a char boundary;
        // the scan steps one byte at a time and may land inside a multibyte
        // char, which would make a `src[i..]` str slice panic.
        if bytes[i..].starts_with(needle.as_bytes()) {
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

pub(super) fn latex_command_args(src: &str, command: &str) -> Vec<String> {
    let needle = format!("\\{command}");
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        // Byte-compare the (ASCII) needle so `i` need not be a char boundary;
        // the scan steps one byte at a time and may land inside a multibyte
        // char, which would make a `src[i..]` str slice panic.
        if bytes[i..].starts_with(needle.as_bytes()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_url_blocks_dangerous_schemes() {
        assert!(safe_url("javascript:alert(1)").is_none());
        assert!(safe_url("  JavaScript:alert(1)").is_none());
        assert!(safe_url("java\tscript:alert(1)").is_none());
        assert!(safe_url("data:text/html,<script>1</script>").is_none());
        assert!(safe_url("vbscript:msgbox(1)").is_none());
    }

    #[test]
    fn safe_url_allows_safe_and_relative() {
        assert!(safe_url("https://example.com/x").is_some());
        assert!(safe_url("http://a").is_some());
        assert!(safe_url("mailto:a@b.com").is_some());
        assert!(safe_url("ftp://host/file").is_some());
        assert!(safe_url("/relative/path").is_some());
        assert!(safe_url("page.html#frag").is_some());
    }

    #[test]
    fn latex_command_scan_handles_multibyte_source() {
        // Regression: the byte-walking scan landed inside a multibyte char and
        // panicked on the next `src[i..]` slice.
        assert!(latex_command_arg(r"x = \λ \label{eq:a} y", "label").as_deref() == Some("eq:a"));
        assert!(latex_command_call(r"café \ref{thm:π}", "ref").is_some());
    }
}
