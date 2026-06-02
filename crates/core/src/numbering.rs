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
//! * Numbered display math is section-scoped, printed as `{section}.{n}`.
//!   Single-display environments (`equation`, `multline`) get one number.
//!   Row environments (`align`, `gather`, `alignat`, `eqnarray`) get one
//!   number per top-level row unless that row has `\notag` / `\nonumber`.
//!   Starred forms (`equation*` etc.) are unnumbered.

use std::collections::HashMap;

use crate::ast::{Node, NodeKind, RefKind};
use crate::bibtex::{alphabetic_label, alphabetic_sort_key, authoryear_label, BibEntry, BibStyle};
use crate::theorems::TheoremRegistry;

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
    thms: &TheoremRegistry,
) -> LabelTable {
    let mut state = State::new(thms);
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

struct State<'r> {
    section_prefix: Option<String>,
    section_counters: [u32; 7],
    appendix: bool,
    /// counter name → current value. Theorem-like environments may share a
    /// counter (`\newtheorem{lemma}[theorem]{Lemma}`) or have their own.
    thm_counters: HashMap<String, u32>,
    eq_in_section: u32,
    subequations: Vec<SubequationState>,
    figure: u32,
    table: u32,
    labels: LabelTable,
    pending_labels: Vec<String>,
    registry: &'r TheoremRegistry,
}

impl<'r> State<'r> {
    fn new(registry: &'r TheoremRegistry) -> Self {
        Self {
            section_prefix: None,
            section_counters: [0; 7],
            appendix: false,
            thm_counters: HashMap::new(),
            eq_in_section: 0,
            subequations: Vec::new(),
            figure: 0,
            table: 0,
            labels: LabelTable::default(),
            pending_labels: Vec::new(),
            registry,
        }
    }
}

/// Reset every theorem counter that resets under `level` (or a shallower
/// sectioning level). Counters with no reset level (continuous numbering) are
/// left untouched.
fn reset_theorem_counters(state: &mut State<'_>, level: u8) {
    let reg = state.registry;
    for (counter, value) in state.thm_counters.iter_mut() {
        if reg
            .counter_reset_level(counter)
            .is_some_and(|reset| level <= reset)
        {
            *value = 0;
        }
    }
}

struct SubequationState {
    parent: String,
    child_index: u32,
}

fn walk(nodes: &mut [Node], state: &mut State<'_>) {
    for node in nodes.iter_mut() {
        match &mut node.kind {
            NodeKind::Appendix => {
                state.appendix = true;
                state.section_prefix = None;
                state.section_counters = [0; 7];
                state.thm_counters.clear();
                state.eq_in_section = 0;
            }
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
                // `\section`/`\chapter` (level ≤ 2) drives the equation
                // prefix and resets the section-scoped equation counter.
                if *level <= 2 {
                    state.section_prefix = Some(section_number(
                        &state.section_counters,
                        *level,
                        state.appendix,
                    ));
                    state.eq_in_section = 0;
                }
                // Theorem counters reset per their own declared level (which may
                // be section, chapter, subsection, …) rather than a fixed one.
                reset_theorem_counters(state, *level);
                let n = section_number(&state.section_counters, *level, state.appendix);
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
                let reg = state.registry;
                if reg.numbered(env) {
                    let counter = reg.counter(env);
                    let value = state.thm_counters.entry(counter).or_insert(0);
                    *value += 1;
                    let val = *value;
                    let n = match reg.reset_level(env) {
                        Some(level) => {
                            let prefix =
                                section_number(&state.section_counters, level, state.appendix);
                            // Before any section of the reset level, `\the…` is
                            // "0"; LaTeX/AMS shows a flat number there.
                            if prefix.is_empty() || prefix == "0" {
                                val.to_string()
                            } else {
                                format!("{prefix}.{val}")
                            }
                        }
                        None => val.to_string(),
                    };
                    *number = Some(n.clone());
                    let display_kind = reg.title(env);
                    if let Some(l) = label {
                        record_label(&mut state.labels, l.clone(), &n, &display_kind);
                    }
                    let pending = std::mem::take(&mut state.pending_labels);
                    for l in pending {
                        record_label(&mut state.labels, l, &n, &display_kind);
                    }
                } else {
                    // `\newtheorem*` — unnumbered; carries no number or ref.
                    *number = None;
                    state.pending_labels.clear();
                }
                // Recurse into theorem body so equations inside still count.
                walk(&mut node.children, state);
            }
            NodeKind::Subequations { label, number } => {
                state.eq_in_section += 1;
                let n = format_with_section(state.section_prefix.as_deref(), state.eq_in_section);
                *number = Some(n.clone());
                if let Some(l) = label {
                    record_label(&mut state.labels, l.clone(), &n, "Equation");
                }
                let pending = std::mem::take(&mut state.pending_labels);
                for l in pending {
                    record_label(&mut state.labels, l, &n, "Equation");
                }
                state.subequations.push(SubequationState {
                    parent: n,
                    child_index: 0,
                });
                walk(&mut node.children, state);
                state.subequations.pop();
            }
            NodeKind::DisplayMath {
                body,
                env,
                label,
                number,
                row_numbers,
            } => {
                row_numbers.clear();
                let numbered = match env.as_deref() {
                    Some(e) => is_numbered_math_env(e),
                    None => false,
                };
                if numbered && env.as_deref().is_some_and(is_multirow_numbered_math_env) {
                    let rows = split_math_rows(body);
                    let mut first_numbered_row = true;
                    for row in rows {
                        if row_is_unnumbered(row) {
                            row_numbers.push(None);
                            continue;
                        }

                        let n = next_equation_number(state);
                        row_numbers.push(Some(n.clone()));
                        *number = Some(n.clone());

                        if first_numbered_row {
                            if let Some(l) = label {
                                record_label(&mut state.labels, l.clone(), &n, "Equation");
                            }
                            let pending = std::mem::take(&mut state.pending_labels);
                            for l in pending {
                                record_label(&mut state.labels, l, &n, "Equation");
                            }
                            first_numbered_row = false;
                        }

                        for l in labels_from_latex(row) {
                            record_label(&mut state.labels, l, &n, "Equation");
                        }
                    }
                } else if numbered {
                    let n = next_equation_number(state);
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

fn next_equation_number(state: &mut State<'_>) -> String {
    if let Some(subequations) = state.subequations.last_mut() {
        subequations.child_index += 1;
        return format!(
            "{}{}",
            subequations.parent,
            alphabetic_suffix(subequations.child_index)
        );
    }

    state.eq_in_section += 1;
    format_with_section(state.section_prefix.as_deref(), state.eq_in_section)
}

fn alphabetic_suffix(mut n: u32) -> String {
    if n == 0 {
        return String::new();
    }
    let mut chars = Vec::new();
    while n > 0 {
        n -= 1;
        chars.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    chars.iter().rev().collect()
}

fn alphabetic_section_prefix(n: u32) -> String {
    alphabetic_suffix(n).to_ascii_uppercase()
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

fn split_math_rows(src: &str) -> Vec<&str> {
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

fn row_is_unnumbered(row: &str) -> bool {
    has_latex_command(row, "notag") || has_latex_command(row, "nonumber")
}

fn has_latex_command(src: &str, command: &str) -> bool {
    let needle = format!("\\{command}");
    let bytes = src.as_bytes();
    let mut search_from = 0usize;
    while let Some(found) = src[search_from..].find(&needle) {
        let start = search_from + found;
        let after = start + needle.len();
        if bytes
            .get(after)
            .is_none_or(|b| !b.is_ascii_alphabetic() && *b != b'*')
        {
            return true;
        }
        search_from = after;
    }
    false
}

fn format_with_section(section: Option<&str>, n: u32) -> String {
    match section {
        Some(section) if !section.is_empty() => format!("{section}.{n}"),
        _ => n.to_string(),
    }
}

fn section_number(counters: &[u32; 7], level: u8, appendix: bool) -> String {
    let idx = (level as usize).min(counters.len() - 1);
    if appendix {
        if idx == 1 {
            return alphabetic_section_prefix(counters[1]);
        }
        if idx >= 2 {
            let chapter = counters[1];
            let top_idx = if chapter > 0 { 1 } else { 2 };
            let mut parts = vec![alphabetic_section_prefix(counters[top_idx])];
            parts.extend(
                counters[top_idx + 1..=idx]
                    .iter()
                    .copied()
                    .filter(|counter| *counter > 0)
                    .map(|counter| counter.to_string()),
            );
            return parts.join(".");
        }
    }
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

fn is_multirow_numbered_math_env(env: &str) -> bool {
    !env.ends_with('*') && matches!(env, "align" | "gather" | "alignat" | "eqnarray")
}

fn is_float_env(env: &str) -> bool {
    matches!(env.trim_end_matches('*'), "figure" | "table")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_body;
    use crate::project::{Preamble, Project, ProjectFile};
    use std::path::PathBuf;

    fn project_with(preamble: &str, src: &str) -> Project {
        Project {
            root: PathBuf::from("t.tex"),
            preamble: Preamble {
                source: preamble.to_string(),
                file: PathBuf::from("t.tex"),
            },
            files: vec![ProjectFile {
                path: PathBuf::from("t.tex"),
                source: src.to_string(),
                start: crate::ast::Pos::ZERO,
                is_root_body: true,
            }],
            warnings: vec![],
        }
    }

    fn nodes(src: &str) -> Vec<Node> {
        let project = project_with("", src);
        parse_body(&project, &TheoremRegistry::with_builtin_defaults()).unwrap()
    }

    /// Parse + number a body whose theorem environments are declared by
    /// `preamble`, returning the resolved labels. Used to exercise
    /// `\newtheorem`-driven counters/titles.
    fn number_with(preamble: &str, src: &str) -> LabelTable {
        let project = project_with(preamble, src);
        let thms = TheoremRegistry::from_preamble(preamble);
        let mut ns = parse_body(&project, &thms).unwrap();
        assign_numbers(&mut ns, &HashMap::new(), BibStyle::Numeric, &thms)
    }

    /// Number an already-parsed body with built-in AMS defaults.
    fn assign(ns: &mut [Node]) -> LabelTable {
        assign_numbers(
            ns,
            &HashMap::new(),
            BibStyle::Numeric,
            &TheoremRegistry::with_builtin_defaults(),
        )
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
        let labels = assign(&mut ns);
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
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("eq:a").unwrap(), "1.1");
        // equation* is unnumbered
        assert!(labels.display.get("eq:a").unwrap().contains("Equation 1.1"));
        assert_eq!(labels.number.get("eq:b").unwrap(), "1.2");
    }

    #[test]
    fn align_rows_get_separate_numbers() {
        let mut ns = nodes(
            "\\section{S}\n\
             \\begin{align}\n\
             a &= b \\label{eq:a}\\\\\n\
             c &= d \\label{eq:b}\\\\[3pt]\n\
             e &= f \\notag\\\\\n\
             g &= h \\label{eq:c}\n\
             \\end{align}\n\
             See \\eqref{eq:a}, \\eqref{eq:b}, \\eqref{eq:c}.\n",
        );
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("eq:a").unwrap(), "1.1");
        assert_eq!(labels.number.get("eq:b").unwrap(), "1.2");
        assert_eq!(labels.number.get("eq:c").unwrap(), "1.3");

        let align = ns
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::DisplayMath { row_numbers, .. } if !row_numbers.is_empty() => {
                    Some(row_numbers)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            align,
            &vec![
                Some("1.1".to_string()),
                Some("1.2".to_string()),
                None,
                Some("1.3".to_string())
            ]
        );
    }

    #[test]
    fn subequations_use_parent_number_and_alphabetic_children() {
        let mut ns = nodes(
            "\\section{S}\n\
             \\begin{subequations}\n\
             \\label{eq:group}\n\
             \\begin{equation}\\label{eq:a}a=b\\end{equation}\n\
             \\begin{align}\n\
             c &= d \\label{eq:b}\\\\\n\
             e &= f \\notag\\\\\n\
             g &= h \\label{eq:c}\n\
             \\end{align}\n\
             \\begin{equation*}\\label{eq:star}x=y\\end{equation*}\n\
             \\end{subequations}\n\
             \\begin{equation}\\label{eq:after}z=w\\end{equation}\n",
        );
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("eq:group").unwrap(), "1.1");
        assert_eq!(labels.number.get("eq:a").unwrap(), "1.1a");
        assert_eq!(labels.number.get("eq:b").unwrap(), "1.1b");
        assert_eq!(labels.number.get("eq:c").unwrap(), "1.1c");
        assert!(!labels.number.contains_key("eq:star"));
        assert_eq!(labels.number.get("eq:after").unwrap(), "1.2");
    }

    #[test]
    fn appendix_sections_use_alphabetic_prefixes() {
        let mut ns = nodes(
            "\\section{Main}\\label{sec:main}\n\
             \\begin{equation}\\label{eq:main}x\\end{equation}\n\
             \\appendix\n\
             \\section{Derivation}\\label{app:derivation}\n\
             \\subsection{Detail}\\label{app:detail}\n\
             \\begin{lemma}\\label{lem:app}y\\end{lemma}\n\
             \\begin{equation}\\label{eq:app}z\\end{equation}\n\
             \\begin{subequations}\\label{eq:app-group}\n\
             \\begin{equation}\\label{eq:app-a}a\\end{equation}\n\
             \\begin{equation}\\label{eq:app-b}b\\end{equation}\n\
             \\end{subequations}\n",
        );
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("sec:main").unwrap(), "1");
        assert_eq!(labels.number.get("eq:main").unwrap(), "1.1");
        assert_eq!(labels.number.get("app:derivation").unwrap(), "A");
        assert_eq!(labels.number.get("app:detail").unwrap(), "A.1");
        assert_eq!(labels.number.get("lem:app").unwrap(), "A.1");
        assert_eq!(labels.number.get("eq:app").unwrap(), "A.1");
        assert_eq!(labels.number.get("eq:app-group").unwrap(), "A.2");
        assert_eq!(labels.number.get("eq:app-a").unwrap(), "A.2a");
        assert_eq!(labels.number.get("eq:app-b").unwrap(), "A.2b");
    }

    #[test]
    fn appendix_chapters_use_alphabetic_prefixes() {
        let mut ns = nodes(
            "\\chapter{Main}\\label{chap:main}\n\
             \\appendix\n\
             \\chapter{Auxiliary}\\label{chap:aux}\n\
             \\section{Detail}\\label{sec:aux-detail}\n\
             \\begin{equation}\\label{eq:aux}z\\end{equation}\n",
        );
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("chap:main").unwrap(), "1");
        assert_eq!(labels.number.get("chap:aux").unwrap(), "A");
        assert_eq!(labels.number.get("sec:aux-detail").unwrap(), "A.1");
        assert_eq!(labels.number.get("eq:aux").unwrap(), "A.1.1");
    }

    #[test]
    fn no_section_falls_back_to_flat_numbering() {
        let mut ns = nodes(
            "\\begin{theorem}\\label{t1}\nx\n\\end{theorem}\n\
             \\begin{lemma}\\label{l1}\ny\n\\end{lemma}\n",
        );
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("t1").unwrap(), "1");
        assert_eq!(labels.number.get("l1").unwrap(), "2");
    }

    // --- \newtheorem-driven numbering (matches a real latexmk build) ---

    const BODY_THM_LEM: &str = "\\section{A}\\label{sec:a}\n\
         \\begin{theorem}\\label{t1}x\\end{theorem}\n\
         \\begin{lemma}\\label{l1}y\\end{lemma}\n\
         \\begin{theorem}\\label{t2}z\\end{theorem}\n";

    #[test]
    fn shared_counter_matches_ams_default() {
        // theorem + lemma share the `theorem` counter, reset per section.
        let labels = number_with(
            "\\newtheorem{theorem}{Theorem}[section]\n\
             \\newtheorem{lemma}[theorem]{Lemma}\n",
            BODY_THM_LEM,
        );
        assert_eq!(labels.number.get("t1").unwrap(), "1.1");
        assert_eq!(labels.number.get("l1").unwrap(), "1.2");
        assert_eq!(labels.number.get("t2").unwrap(), "1.3");
    }

    #[test]
    fn independent_counters_number_separately() {
        // lemma has its OWN section-reset counter, so it restarts at 1.
        let labels = number_with(
            "\\newtheorem{theorem}{Theorem}[section]\n\
             \\newtheorem{lemma}{Lemma}[section]\n",
            BODY_THM_LEM,
        );
        assert_eq!(labels.number.get("t1").unwrap(), "1.1");
        assert_eq!(labels.number.get("l1").unwrap(), "1.1");
        assert_eq!(labels.number.get("t2").unwrap(), "1.2");
    }

    #[test]
    fn no_reset_clause_numbers_continuously() {
        // `\newtheorem{theorem}{Theorem}` → no section prefix, never resets.
        let labels = number_with(
            "\\newtheorem{theorem}{Theorem}\n\\newtheorem{lemma}[theorem]{Lemma}\n",
            "\\section{A}\n\
             \\begin{theorem}\\label{t1}x\\end{theorem}\n\
             \\section{B}\n\
             \\begin{lemma}\\label{l1}y\\end{lemma}\n",
        );
        assert_eq!(labels.number.get("t1").unwrap(), "1");
        assert_eq!(labels.number.get("l1").unwrap(), "2");
    }

    #[test]
    fn custom_environment_is_numbered_and_titled() {
        let labels = number_with(
            "\\newtheorem{assumption}{Assumption}[section]\n",
            "\\section{A}\n\\begin{assumption}\\label{as1}x\\end{assumption}\n",
        );
        assert_eq!(labels.number.get("as1").unwrap(), "1.1");
        assert_eq!(labels.display.get("as1").unwrap(), "Assumption 1.1");
    }

    #[test]
    fn starred_theorem_is_unnumbered() {
        let labels = number_with(
            "\\newtheorem*{remark}{Remark}\n",
            "\\section{A}\n\\begin{remark}\\label{r1}x\\end{remark}\n",
        );
        assert!(!labels.number.contains_key("r1"));
    }

    #[test]
    fn numberwithin_resets_per_subsection() {
        let labels = number_with(
            "\\newtheorem{theorem}{Theorem}\n\\numberwithin{theorem}{subsection}\n",
            "\\section{A}\n\\subsection{A1}\n\
             \\begin{theorem}\\label{t1}x\\end{theorem}\n\
             \\subsection{A2}\n\
             \\begin{theorem}\\label{t2}y\\end{theorem}\n",
        );
        assert_eq!(labels.number.get("t1").unwrap(), "1.1.1");
        assert_eq!(labels.number.get("t2").unwrap(), "1.2.1");
    }

    #[test]
    fn custom_title_is_honored() {
        let labels = number_with(
            "\\newtheorem{thm}{Satz}[section]\n",
            "\\section{A}\n\\begin{thm}\\label{s1}x\\end{thm}\n",
        );
        assert_eq!(labels.display.get("s1").unwrap(), "Satz 1.1");
    }
}
