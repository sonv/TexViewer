//! Bibliography formatting. Produces the `<li>` body for each entry in the
//! references list, including normalized authors, italicized titles, venue
//! lines, DOI/URL links, and arXiv eprint detection.

use crate::bibtex::{BibEntry, BibStyle};
use crate::numbering::LabelTable;

use super::util::{escape_attr, safe_url};

pub(super) fn format_bib_entry(e: &BibEntry, style: BibStyle, labels: &LabelTable) -> String {
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
        // Only emit a clickable link for safe schemes; a `javascript:` (or
        // other unsafe-scheme) `url` field from an untrusted `.bib` would
        // otherwise become a one-click XSS in the preview origin. Unsafe
        // URLs are shown as inert escaped text instead of dropped.
        match safe_url(u) {
            Some(href) => parts.push(format!(
                r#"<a class="bib-url" href="{href}" target="_blank" rel="noopener">{text}</a>"#,
                text = escape_attr(u.trim()),
            )),
            None => parts.push(escape_attr(u.trim())),
        }
    }
    let mut s = parts.join(". ");
    if !s.ends_with('.') {
        s.push('.');
    }
    s
}

pub(super) fn format_authors(a: &str, labels: &LabelTable) -> String {
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
    super::render_inline_latex(&normalized, labels)
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
                    // The escaped char may be multibyte (`\é`, `\—`); push the
                    // whole char and advance by its UTF-8 width. A blind 1-byte
                    // step would land mid-codepoint and panic on the next
                    // `s[i..]` slice.
                    let ch = s[i..].chars().next().unwrap_or('\0');
                    out.push(ch);
                    i += ch.len_utf8();
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
                i += ch.len_utf8();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_braces_handles_backslash_before_multibyte() {
        // Regression: a backslash before a multibyte char (`\é`, `\—`) in a
        // `.bib` field used to advance one byte into the codepoint and panic on
        // the next slice.
        for s in [r"\é foo", r"author \— name", r"\ÿ", r"trailing \"] {
            let _ = strip_bib_protective_braces(s); // must not panic
        }
        assert!(strip_bib_protective_braces(r"\é").contains('é'));
    }
}
