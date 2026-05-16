//! Pre-render pass that assigns LaTeX-style numbers to sections, theorem-like
//! environments, and numbered display-math equations, and builds the
//! `LabelTable` consumed by the renderer for resolving `\ref` / `\cref` /
//! `\eqref` targets.
//!
//! Numbering convention (AMS-modern default — what the example paper would
//! get from `\newtheorem{theorem}{Theorem}[section]` with shared counters):
//!
//! * `\section`s number `1, 2, 3, …`.
//! * Theorem-like environments share one counter, reset at each `\section`,
//!   printed as `{section}.{n}`. So in §2 you get `Theorem 2.1`, `Lemma 2.2`,
//!   `Theorem 2.3`. With no section, you get `1, 2, 3, …`.
//! * Numbered display math (`equation`, `align`, `gather`, `multline`,
//!   `alignat`, `eqnarray`, `displaymath`) gets its own counter, also
//!   section-scoped, printed as `{section}.{n}`. Starred forms (`equation*`
//!   etc.) are unnumbered.

use std::collections::HashMap;

use crate::ast::{Node, NodeKind, RefKind};
use crate::bibtex::{alphabetic_label, alphabetic_sort_key, authoryear_label, BibEntry, BibStyle};

#[derive(Debug, Default, Clone)]
pub struct LabelTable {
    /// `label` → bare number, e.g. `"thm:main"` → `"2.1"`.
    pub number: HashMap<String, String>,
    /// `label` → friendly kind+number, e.g. `"thm:main"` → `"Theorem 2.1"`.
    pub display: HashMap<String, String>,
    /// `label` → kind word alone, e.g. `"thm:main"` → `"Theorem"`. Useful for
    /// `\autoref` / lowercase variants.
    pub kind: HashMap<String, String>,
    /// `cite key` → bibliography entry number (1-indexed, in citation order).
    pub citation_number: HashMap<String, u32>,
    /// `cite key` → in-text display token, e.g. `"1"` (numeric), `"SV06"`
    /// (alphabetic), or `"Stroock and Varadhan, 2006"` (author-year).
    pub citation_display: HashMap<String, String>,
    /// Cite keys in display order — for numeric style this is first-appearance,
    /// for alphabetic / author-year it's sorted alphabetically by author/year.
    pub cite_order: Vec<String>,
}

impl LabelTable {
    /// Resolve a `\ref` / `\cref` / etc. target to display text.
    pub fn resolve_ref(&self, kind: RefKind, key: &str) -> String {
        match kind {
            RefKind::Ref | RefKind::Pageref => self
                .number
                .get(key)
                .cloned()
                .unwrap_or_else(|| key.to_string()),
            RefKind::Eqref => self
                .number
                .get(key)
                .map(|n| format!("({n})"))
                .unwrap_or_else(|| format!("({key})")),
            RefKind::Cref | RefKind::Autoref => self
                .display
                .get(key)
                .cloned()
                .unwrap_or_else(|| key.to_string()),
            RefKind::Nameref => {
                // We don't currently store section titles; fall back to the
                // kind+number for now.
                self.display
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| key.to_string())
            }
        }
    }
}

/// Walk `nodes` in order, mutating each Section / Theorem / numbered
/// DisplayMath node's `number` field, and return the populated `LabelTable`.
/// Citation order and display tokens depend on `style`; missing-from-bib
/// citations still get an entry in `cite_order` and a display token that
/// matches the chosen style.
pub fn assign_numbers(
    nodes: &mut [Node],
    bib: &HashMap<String, BibEntry>,
    style: BibStyle,
) -> LabelTable {
    let mut state = State::default();
    walk(nodes, &mut state);

    // Finalize citation order + display per style.
    match style {
        BibStyle::Numeric => {
            // First-appearance order; display = the number we already assigned.
            for (i, k) in state.labels.cite_order.iter().enumerate() {
                state
                    .labels
                    .citation_display
                    .insert(k.clone(), (i + 1).to_string());
            }
        }
        BibStyle::NumericSorted => {
            // BibTeX `plain`-style numeric labels are sorted by bibliography
            // key material, not by first citation.
            state.labels.cite_order.sort_by(|a, b| {
                let ka = bib.get(a).map(alphabetic_sort_key);
                let kb = bib.get(b).map(alphabetic_sort_key);
                ka.cmp(&kb).then_with(|| a.cmp(b))
            });
            state.labels.citation_number.clear();
            for (i, k) in state.labels.cite_order.iter().enumerate() {
                state
                    .labels
                    .citation_display
                    .insert(k.clone(), (i + 1).to_string());
                state
                    .labels
                    .citation_number
                    .insert(k.clone(), (i + 1) as u32);
            }
        }
        BibStyle::Alphabetic => {
            // Sort by (family, year, title) of the bib entry.
            state.labels.cite_order.sort_by(|a, b| {
                let ka = bib.get(a).map(alphabetic_sort_key);
                let kb = bib.get(b).map(alphabetic_sort_key);
                ka.cmp(&kb).then_with(|| a.cmp(b))
            });
            // Compute labels and detect duplicates → suffix with a/b/c.
            let labels: Vec<String> = state
                .labels
                .cite_order
                .iter()
                .map(|k| {
                    bib.get(k)
                        .map(alphabetic_label)
                        .unwrap_or_else(|| k.clone())
                })
                .collect();
            let mut seen: HashMap<String, usize> = HashMap::new();
            for l in &labels {
                *seen.entry(l.clone()).or_insert(0) += 1;
            }
            let mut counters: HashMap<String, u32> = HashMap::new();
            for (k, base) in state.labels.cite_order.iter().zip(labels.iter()) {
                let final_label = if *seen.get(base).unwrap_or(&0) > 1 {
                    let c = counters.entry(base.clone()).or_insert(0);
                    let suffix = (b'a' + (*c as u8)) as char;
                    *c += 1;
                    format!("{base}{suffix}")
                } else {
                    base.clone()
                };
                state.labels.citation_display.insert(k.clone(), final_label);
            }
            // Re-issue citation_number to match new order.
            state.labels.citation_number.clear();
            for (i, k) in state.labels.cite_order.iter().enumerate() {
                state
                    .labels
                    .citation_number
                    .insert(k.clone(), (i + 1) as u32);
            }
        }
        BibStyle::AuthorYear => {
            state.labels.cite_order.sort_by(|a, b| {
                let ka = bib.get(a).map(alphabetic_sort_key);
                let kb = bib.get(b).map(alphabetic_sort_key);
                ka.cmp(&kb).then_with(|| a.cmp(b))
            });
            for k in &state.labels.cite_order {
                let disp = bib
                    .get(k)
                    .map(authoryear_label)
                    .unwrap_or_else(|| k.clone());
                state.labels.citation_display.insert(k.clone(), disp);
            }
            state.labels.citation_number.clear();
            for (i, k) in state.labels.cite_order.iter().enumerate() {
                state
                    .labels
                    .citation_number
                    .insert(k.clone(), (i + 1) as u32);
            }
        }
    }

    state.labels
}

#[derive(Default)]
struct State {
    section: u32,
    section_counters: [u32; 7],
    thm_in_section: u32,
    eq_in_section: u32,
    figure: u32,
    table: u32,
    labels: LabelTable,
    pending_labels: Vec<String>,
}

fn walk(nodes: &mut [Node], state: &mut State) {
    for node in nodes.iter_mut() {
        match &mut node.kind {
            NodeKind::Section {
                level,
                title: _,
                label,
                number,
            } => {
                let idx = (*level as usize).min(state.section_counters.len() - 1);
                state.section_counters[idx] += 1;
                for c in state.section_counters.iter_mut().skip(idx + 1) {
                    *c = 0;
                }
                // Only top-level `\section` (level 2) drives the prefix in
                // this default. Sub-sections don't reset the theorem counter
                // — that matches AMS practice.
                if *level <= 2 {
                    state.section = state.section_counters[idx];
                    state.thm_in_section = 0;
                    state.eq_in_section = 0;
                }
                let n = section_number(&state.section_counters, *level);
                *number = Some(n.clone());
                if let Some(l) = label {
                    record_label(&mut state.labels, l.clone(), &n, "Section");
                }
                let pending = std::mem::take(&mut state.pending_labels);
                for l in pending {
                    record_label(&mut state.labels, l, &n, "Section");
                }
            }
            NodeKind::Theorem {
                env, label, number, ..
            } => {
                state.thm_in_section += 1;
                let n = format_with_section(state.section, state.thm_in_section);
                *number = Some(n.clone());
                let display_kind = display_kind_for(env);
                if let Some(l) = label {
                    record_label(&mut state.labels, l.clone(), &n, display_kind);
                }
                let pending = std::mem::take(&mut state.pending_labels);
                for l in pending {
                    record_label(&mut state.labels, l, &n, display_kind);
                }
                // Recurse into theorem body so equations inside still count.
                walk(&mut node.children, state);
            }
            NodeKind::DisplayMath {
                body,
                env,
                label,
                number,
            } => {
                let numbered = match env.as_deref() {
                    Some(e) => is_numbered_math_env(e),
                    None => false,
                };
                if numbered {
                    state.eq_in_section += 1;
                    let n = format_with_section(state.section, state.eq_in_section);
                    *number = Some(n.clone());
                    if let Some(l) = label {
                        record_label(&mut state.labels, l.clone(), &n, "Equation");
                    }
                    for l in labels_from_latex(body) {
                        record_label(&mut state.labels, l, &n, "Equation");
                    }
                    let pending = std::mem::take(&mut state.pending_labels);
                    for l in pending {
                        record_label(&mut state.labels, l, &n, "Equation");
                    }
                }
            }
            NodeKind::OpaqueEnv { env, body } if is_float_env(env) => {
                let (n, display_kind) = match env.trim_end_matches('*') {
                    "table" => {
                        state.table += 1;
                        (state.table.to_string(), "Table")
                    }
                    _ => {
                        state.figure += 1;
                        (state.figure.to_string(), "Figure")
                    }
                };
                for l in labels_from_latex(body) {
                    record_label(&mut state.labels, l, &n, display_kind);
                }
                let pending = std::mem::take(&mut state.pending_labels);
                for l in pending {
                    record_label(&mut state.labels, l, &n, display_kind);
                }
            }
            NodeKind::OpaqueCmd { name, raw } if name == "label" => {
                if let Some(label) = label_from_raw(raw) {
                    state.pending_labels.push(label);
                }
            }
            NodeKind::Cite { keys } => {
                for k in keys {
                    if state.labels.citation_number.contains_key(k) {
                        continue;
                    }
                    let n = state.labels.cite_order.len() as u32 + 1;
                    state.labels.citation_number.insert(k.clone(), n);
                    state.labels.cite_order.push(k.clone());
                }
            }
            _ => {
                walk(&mut node.children, state);
            }
        }
    }
}

fn record_label(labels: &mut LabelTable, label: String, number: &str, kind: &str) {
    if labels.number.contains_key(&label) {
        return;
    }
    labels.number.insert(label.clone(), number.to_string());
    labels
        .display
        .insert(label.clone(), format!("{kind} {number}"));
    labels.kind.insert(label, kind.to_string());
}

fn label_from_raw(raw: &str) -> Option<String> {
    labels_from_latex(raw).into_iter().next()
}

fn labels_from_latex(src: &str) -> Vec<String> {
    let needle = "\\label";
    let bytes = src.as_bytes();
    let mut labels = Vec::new();
    let mut search_from = 0usize;
    while let Some(found) = src[search_from..].find(needle) {
        let start = search_from + found + needle.len();
        let mut i = start;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'{') {
            search_from = start;
            continue;
        }
        let arg_start = i + 1;
        let mut depth = 1i32;
        i = arg_start;
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
                        labels.push(src[arg_start..i].trim().to_string());
                        i += 1;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        search_from = i;
    }
    labels
}

fn format_with_section(section: u32, n: u32) -> String {
    if section == 0 {
        n.to_string()
    } else {
        format!("{section}.{n}")
    }
}

fn section_number(counters: &[u32; 7], level: u8) -> String {
    let idx = (level as usize).min(counters.len() - 1);
    if idx <= 1 {
        return counters[idx].to_string();
    }
    let start = if counters[1] > 0 { 1 } else { 2 };
    counters[start..=idx]
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

fn is_numbered_math_env(env: &str) -> bool {
    !env.ends_with('*')
        && matches!(
            env,
            "equation" | "align" | "gather" | "multline" | "alignat" | "eqnarray"
        )
}

fn is_float_env(env: &str) -> bool {
    matches!(env.trim_end_matches('*'), "figure" | "table")
}

/// Map raw environment name to its display word.
/// Trims trailing `*` and applies title-case + a small alias table.
fn display_kind_for(env: &str) -> &'static str {
    let bare = env.trim_end_matches('*');
    match bare {
        "theorem" | "thm" => "Theorem",
        "lemma" | "lem" => "Lemma",
        "proposition" | "prop" => "Proposition",
        "corollary" | "cor" => "Corollary",
        "definition" | "defn" | "defi" => "Definition",
        "remark" | "rem" => "Remark",
        "example" | "ex" => "Example",
        "claim" => "Claim",
        "fact" => "Fact",
        "conjecture" => "Conjecture",
        _ => "Statement",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_body;
    use crate::project::{Preamble, Project, ProjectFile};
    use std::path::PathBuf;

    fn nodes(src: &str) -> Vec<Node> {
        let project = Project {
            root: PathBuf::from("t.tex"),
            preamble: Preamble {
                source: String::new(),
                file: PathBuf::from("t.tex"),
            },
            files: vec![ProjectFile {
                path: PathBuf::from("t.tex"),
                source: src.to_string(),
                start: crate::ast::Pos::ZERO,
                is_root_body: true,
            }],
            warnings: vec![],
        };
        parse_body(&project).unwrap()
    }

    #[test]
    fn section_scoped_theorem_numbers() {
        let mut ns = nodes(
            "\\section{A}\\label{sec:a}\n\
             \\subsection{A1}\\label{sec:a1}\n\
             \\begin{theorem}\\label{thm:t1}\nx\n\\end{theorem}\n\
             \\begin{lemma}\\label{lem:l1}\ny\n\\end{lemma}\n\
             \\section{B}\\label{sec:b}\n\
             \\begin{theorem}\\label{thm:t2}\nz\n\\end{theorem}\n",
        );
        let labels = assign_numbers(&mut ns, &HashMap::new(), BibStyle::Numeric);
        assert_eq!(labels.number.get("sec:a").unwrap(), "1");
        assert_eq!(labels.number.get("sec:a1").unwrap(), "1.1");
        assert_eq!(labels.number.get("thm:t1").unwrap(), "1.1");
        assert_eq!(labels.number.get("lem:l1").unwrap(), "1.2");
        assert_eq!(labels.number.get("sec:b").unwrap(), "2");
        assert_eq!(labels.number.get("thm:t2").unwrap(), "2.1");
        assert_eq!(labels.display.get("thm:t1").unwrap(), "Theorem 1.1");
        assert_eq!(labels.display.get("lem:l1").unwrap(), "Lemma 1.2");
    }

    #[test]
    fn equation_numbers() {
        let mut ns = nodes(
            "\\section{S}\n\
             \\begin{equation}\\label{eq:a}x\\end{equation}\n\
             \\begin{equation*}y\\end{equation*}\n\
             \\begin{align}\\label{eq:b}z\\end{align}\n",
        );
        let labels = assign_numbers(&mut ns, &HashMap::new(), BibStyle::Numeric);
        assert_eq!(labels.number.get("eq:a").unwrap(), "1.1");
        // equation* is unnumbered
        assert!(labels.display.get("eq:a").unwrap().contains("Equation 1.1"));
        assert_eq!(labels.number.get("eq:b").unwrap(), "1.2");
    }

    #[test]
    fn no_section_falls_back_to_flat_numbering() {
        let mut ns = nodes(
            "\\begin{theorem}\\label{t1}\nx\n\\end{theorem}\n\
             \\begin{lemma}\\label{l1}\ny\n\\end{lemma}\n",
        );
        let labels = assign_numbers(&mut ns, &HashMap::new(), BibStyle::Numeric);
        assert_eq!(labels.number.get("t1").unwrap(), "1");
        assert_eq!(labels.number.get("l1").unwrap(), "2");
    }
}
