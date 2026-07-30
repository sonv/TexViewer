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
//!   Row environments (`align`, `gather`, `alignat`, `flalign`, `xalignat`,
//!   `eqnarray`) get one number per top-level row unless that row has `\notag`
//!   / `\nonumber`.
//!   Starred forms (`equation*` etc.) are unnumbered.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use crate::ast::{Node, NodeKind, RefKind, Span};
use crate::bibtex::{alphabetic_label, alphabetic_sort_key, authoryear_label, BibEntry, BibStyle};
use crate::macros::ExtractedMacro;
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
    /// Bibliography style used to build the citation labels. Inline renderers
    /// without a full `RenderCtx` (notably native table cells) need this to
    /// choose the same citation delimiters as ordinary parsed prose.
    pub bib_style: BibStyle,
    /// Source start → float/table number, including unlabeled floats. Label
    /// lookup alone cannot supply the caption number when no `\label` exists.
    float_number: HashMap<(PathBuf, u32), String>,
}

impl LabelTable {
    pub fn float_number_for_span(&self, span: &Span) -> Option<&str> {
        self.float_number
            .get(&(span.file.clone(), span.start.byte))
            .map(String::as_str)
    }

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
    referenced: Option<HashSet<String>>,
) -> LabelTable {
    assign_numbers_with_macros(nodes, bib, style, thms, referenced, &[])
}

/// As [`assign_numbers`], while also expanding preamble text macros when
/// discovering citations retained inside lightweight native renderers such
/// as table cells. This keeps a wrapper like
/// `\newcommand{\source}[1]{\cite{#1}}` in the same citation sequence as the
/// inline renderer that later expands it.
pub fn assign_numbers_with_macros(
    nodes: &mut [Node],
    bib: &HashMap<String, BibEntry>,
    style: BibStyle,
    thms: &TheoremRegistry,
    referenced: Option<HashSet<String>>,
    macros: &[ExtractedMacro],
) -> LabelTable {
    let mut state = State::new(thms, macros);
    state.labels.bib_style = style;
    state.referenced = referenced;
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
                    // base-26 (a..z, aa, ab, …) so many colliding labels can't
                    // overflow a byte or produce non-letter glyphs.
                    let suffix = alphabetic_suffix(*c + 1);
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
    /// `Some(keys)` when mathtools' `showonlyrefs` is active: the set of every
    /// key referenced anywhere in the document. An equation row is numbered
    /// (and ticks the counter) only when one of its labels is in the set.
    /// `None` = normal numbering.
    referenced: Option<HashSet<String>>,
    citation_macros: HashMap<String, CitationMacroDef>,
    citation_budget: CitationExpansionBudget,
}

impl<'r> State<'r> {
    fn new(registry: &'r TheoremRegistry, macros: &[ExtractedMacro]) -> Self {
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
            referenced: None,
            citation_macros: build_citation_macro_defs(macros),
            citation_budget: CitationExpansionBudget::default(),
        }
    }
}

#[derive(Clone)]
struct CitationMacroDef {
    body: String,
    n_args: usize,
    default: Option<String>,
    intrinsic_citation: bool,
    argument_order: Vec<usize>,
    has_user_dependency: bool,
}

fn build_citation_macro_defs(macros: &[ExtractedMacro]) -> HashMap<String, CitationMacroDef> {
    let mut definitions = macros
        .iter()
        .filter_map(|definition| {
            let name = definition.name.trim_start_matches('\\');
            (!name.is_empty()).then(|| {
                (
                    name.to_string(),
                    CitationMacroDef {
                        body: definition.body.clone(),
                        n_args: definition.n_args as usize,
                        default: definition.default.clone(),
                        intrinsic_citation: false,
                        argument_order: Vec::new(),
                        has_user_dependency: false,
                    },
                )
            })
        })
        .collect::<HashMap<_, _>>();

    let names = definitions.keys().cloned().collect::<HashSet<_>>();
    let mut reverse_dependencies: HashMap<String, Vec<String>> = HashMap::new();
    let mut citation_emitters = HashSet::new();
    let mut analyses = Vec::with_capacity(definitions.len());
    for (name, definition) in &definitions {
        let (commands, argument_order) = live_macro_body_analysis(&definition.body);
        let direct = commands
            .iter()
            .any(|command| is_citation_command(command));
        if direct {
            citation_emitters.insert(name.clone());
        }
        let dependencies = commands
            .into_iter()
            .filter(|command| names.contains(command))
            .collect::<Vec<_>>();
        for dependency in &dependencies {
            reverse_dependencies
                .entry(dependency.clone())
                .or_default()
                .push(name.clone());
        }
        analyses.push((name.clone(), argument_order, !dependencies.is_empty()));
    }
    for (name, argument_order, has_user_dependency) in analyses {
        if let Some(definition) = definitions.get_mut(&name) {
            definition.argument_order = argument_order;
            definition.has_user_dependency = has_user_dependency;
        }
    }

    let mut queue = citation_emitters.iter().cloned().collect::<VecDeque<_>>();
    while let Some(emitter) = queue.pop_front() {
        if let Some(dependents) = reverse_dependencies.get(&emitter) {
            for dependent in dependents {
                if citation_emitters.insert(dependent.clone()) {
                    queue.push_back(dependent.clone());
                }
            }
        }
    }
    for name in citation_emitters {
        if let Some(definition) = definitions.get_mut(&name) {
            definition.intrinsic_citation = true;
        }
    }
    definitions
}

fn live_macro_body_analysis(src: &str) -> (Vec<String>, Vec<usize>) {
    let source = crate::parser::executable_latex_source(src);
    let bytes = source.as_bytes();
    let mut commands = Vec::new();
    let mut argument_order = Vec::new();
    let mut seen_arguments = HashSet::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'#' && bytes.get(i + 1) == Some(&b'#') {
            i += 2;
            continue;
        }
        if bytes[i] == b'#' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            let index = (bytes[i + 1] - b'0') as usize;
            if index > 0 && seen_arguments.insert(index) {
                argument_order.push(index);
            }
            i += 2;
            continue;
        }
        if bytes[i] != b'\\' {
            i += if bytes[i].is_ascii() {
                1
            } else {
                source[i..].chars().next().map_or(1, char::len_utf8)
            };
            continue;
        }

        let word_start = i + 1;
        let mut word_end = word_start;
        while word_end < bytes.len()
            && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
        {
            word_end += 1;
        }
        if word_end == word_start {
            i = tex_token_end_for_citations(&source, i);
            continue;
        }
        let name = &source[word_start..word_end];
        if name == "begin" {
            if let Some(span) = crate::parser::inert_environment_span_at(&source, i) {
                i = span.end;
                continue;
            }
        }
        if crate::parser::is_inline_literal_command(name) {
            i = crate::parser::inline_literal_payload(&source, name, word_end)
                .map(|(_, end)| end)
                .unwrap_or(bytes.len());
            continue;
        }
        if name == "string" {
            i = tex_token_end_for_citations(&source, skip_tex_argument_space(&source, word_end));
            continue;
        }
        if matches!(name, "detokenize" | "unexpanded") {
            let group_start = skip_tex_argument_space(&source, word_end);
            i = crate::parser::tex_group_end(&source, group_start, b'{', b'}')
                .unwrap_or(bytes.len());
            continue;
        }
        commands.push(name.to_string());
        i = word_end;
    }
    (commands, argument_order)
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
                // Reset only sectioning-derived theorem counters; continuous
                // (never-reset) counters keep counting across \appendix, as in
                // a real LaTeX build.
                reset_theorem_counters(state, 0);
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
                    let mut rows = split_math_rows(body);
                    // A trailing `\\` leaves an empty final row that MathJax does
                    // NOT render as a table row — drop it so it neither shows a
                    // gutter number nor advances the equation counter. (Empty
                    // rows in the middle, from `\\ \\`, are kept: MathJax does
                    // render those as blank rows.)
                    if rows.last().is_some_and(|r| r.is_empty()) {
                        rows.pop();
                    }
                    let mut first_numbered_row = true;
                    for row in rows {
                        if row_is_unnumbered(row) {
                            row_numbers.push(None);
                            // A manual \tag{X} row carries its own number X
                            // (MathJax renders it); map the row's labels to X so
                            // \ref / \eqref resolve. \notag / \nonumber rows are
                            // truly unnumbered and get nothing.
                            if let Some(tag) = tag_value(row) {
                                for l in labels_from_latex(row) {
                                    record_label(&mut state.labels, l, &tag, "Equation");
                                }
                            }
                            continue;
                        }

                        // mathtools `showonlyrefs`: a row shows a number (and
                        // ticks the counter) only when one of its labels is
                        // referenced somewhere in the document.
                        if let Some(referenced) = &state.referenced {
                            if !labels_from_latex(row)
                                .iter()
                                .any(|l| referenced.contains(l))
                            {
                                row_numbers.push(None);
                                continue;
                            }
                        }

                        let n = next_equation_number(state);
                        row_numbers.push(Some(n.clone()));
                        *number = Some(n.clone());

                        if first_numbered_row {
                            // Only pending labels (from before the env) bind to
                            // the first numbered row. The env's primary `label`
                            // is NOT recorded here: it is just the first \label
                            // in the body and may sit on a LATER row — the
                            // per-row pass below records every in-body label
                            // against its own row's number, and record_label is
                            // first-write-wins, so recording the primary here
                            // would pin it to the wrong row.
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
                } else if numbered && !row_is_unnumbered(body) && single_eq_is_shown(state, body) {
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
                } else if numbered {
                    // Unnumbered single display (\notag / \nonumber / \tag): it
                    // does NOT advance the auto-counter, and `*number` stays None
                    // so the renderer adds no eq-num (MathJax draws the tag). But
                    // a manual \tag{X} still supplies its own number X, so map any
                    // labels to it for \ref / \eqref. \notag / \nonumber leave it
                    // truly unnumbered → labels get no number (refs fall back).
                    if let Some(tag) = tag_value(body) {
                        if let Some(l) = label {
                            record_label(&mut state.labels, l.clone(), &tag, "Equation");
                        }
                        for l in labels_from_latex(body) {
                            record_label(&mut state.labels, l, &tag, "Equation");
                        }
                        let pending = std::mem::take(&mut state.pending_labels);
                        for l in pending {
                            record_label(&mut state.labels, l, &tag, "Equation");
                        }
                    }
                }
            }
            NodeKind::OpaqueEnv { env, body } if is_float_env(env) => {
                let (n, display_kind) = match env.trim_end_matches('*') {
                    "table" | "longtable" => {
                        state.table += 1;
                        (state.table.to_string(), "Table")
                    }
                    _ => {
                        state.figure += 1;
                        (state.figure.to_string(), "Figure")
                    }
                };
                state.labels.float_number.insert(
                    (node.span.file.clone(), node.span.start.byte),
                    n.clone(),
                );
                for l in labels_from_latex(body) {
                    record_label(&mut state.labels, l, &n, display_kind);
                }
                let pending = std::mem::take(&mut state.pending_labels);
                for l in pending {
                    record_label(&mut state.labels, l, &n, display_kind);
                }
                record_citations_from_latex(body, state);
            }
            NodeKind::OpaqueEnv { env, body } if is_native_table_env(env) => {
                record_citations_from_latex(body, state);
            }
            NodeKind::OpaqueCmd { name, raw } if name == "label" => {
                if let Some(label) = label_from_raw(raw) {
                    state.pending_labels.push(label);
                }
            }
            NodeKind::OpaqueCmd { name, .. } if name == "inline-literal" => {}
            NodeKind::OpaqueCmd { raw, .. } => {
                record_citations_from_latex(raw, state);
            }
            NodeKind::Cite { keys } => {
                record_citation_keys(keys.iter().cloned(), state);
            }
            _ => {
                walk(&mut node.children, state);
            }
        }
    }
}

const CITATION_COMMANDS: &[&str] = &[
    "cite",
    "citet",
    "citep",
    "citeauthor",
    "citeyear",
    "parencite",
    "textcite",
    "fullcite",
];

pub(crate) fn is_citation_command(command: &str) -> bool {
    CITATION_COMMANDS.contains(&command)
}

/// Parse the optional prenote/postnote arguments and required key group after
/// a citation control word. The returned end is the byte immediately after
/// the key group.
pub(crate) fn citation_call_after_command(
    src: &str,
    after_command: usize,
) -> Option<(Vec<String>, usize)> {
    let bytes = src.as_bytes();
    let mut i = skip_tex_argument_space(src, after_command);
    for _ in 0..2 {
        if bytes.get(i) != Some(&b'[') {
            break;
        }
        i = crate::parser::tex_group_end(src, i, b'[', b']')?;
        i = skip_tex_argument_space(src, i);
    }
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    let end = crate::parser::tex_group_end(src, i, b'{', b'}')?;
    let keys = src[i + 1..end - 1]
        .split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .collect();
    Some((keys, end))
}

fn record_citations_from_latex(src: &str, state: &mut State<'_>) {
    let mut keys = Vec::new();
    collect_expanded_citation_keys(
        src,
        &state.citation_macros,
        0,
        &mut state.citation_budget,
        &mut keys,
    );
    record_citation_keys(keys, state);
}

const MAX_CITATION_EXPANSION_DEPTH: usize = 32;
const MAX_CITATION_EXPANSIONS: usize = 1_024;
const MAX_CITATION_EXPANDED_BYTES: usize = 1 << 20;

struct CitationExpansionBudget {
    relevant_calls: usize,
    relevant_bytes: usize,
    structural_calls: usize,
    structural_bytes: usize,
}

impl Default for CitationExpansionBudget {
    fn default() -> Self {
        Self {
            relevant_calls: MAX_CITATION_EXPANSIONS,
            relevant_bytes: MAX_CITATION_EXPANDED_BYTES,
            structural_calls: MAX_CITATION_EXPANSIONS,
            structural_bytes: MAX_CITATION_EXPANDED_BYTES,
        }
    }
}

fn collect_expanded_citation_keys(
    src: &str,
    macros: &HashMap<String, CitationMacroDef>,
    depth: usize,
    budget: &mut CitationExpansionBudget,
    keys: &mut Vec<String>,
) {
    if depth >= MAX_CITATION_EXPANSION_DEPTH {
        return;
    }
    let source = crate::parser::executable_latex_source(src);
    let bytes = source.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += if bytes[i].is_ascii() {
                1
            } else {
                source[i..].chars().next().map_or(1, char::len_utf8)
            };
            continue;
        }

        let word_start = i + 1;
        let mut word_end = word_start;
        while word_end < bytes.len()
            && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
        {
            word_end += 1;
        }
        if word_end == word_start {
            i = word_start
                + source[word_start..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8);
            continue;
        }
        let name = &source[word_start..word_end];

        if name == "begin" {
            if let Some(span) = crate::parser::inert_environment_span_at(&source, i) {
                i = span.end;
                continue;
            }
        }
        if crate::parser::is_inline_literal_command(name) {
            i = crate::parser::inline_literal_payload(&source, name, word_end)
                .map(|(_, end)| end)
                .unwrap_or(bytes.len());
            continue;
        }
        if name == "string" {
            i = tex_token_end_for_citations(&source, skip_tex_argument_space(&source, word_end));
            continue;
        }
        if matches!(name, "detokenize" | "unexpanded") {
            let group_start = skip_tex_argument_space(&source, word_end);
            i = crate::parser::tex_group_end(&source, group_start, b'{', b'}')
                .unwrap_or(bytes.len());
            continue;
        }

        if is_citation_command(name) {
            if let Some((citation_keys, end)) = citation_call_after_command(&source, word_end) {
                keys.extend(citation_keys);
                i = end;
            } else {
                i = word_end;
            }
            continue;
        }

        if let Some(definition) = macros.get(name) {
            let (args, end) = read_citation_macro_args(
                &source,
                word_end,
                definition.n_args,
                definition.default.as_deref(),
            );
            if !definition.intrinsic_citation && !definition.has_user_dependency {
                for argument_index in &definition.argument_order {
                    if let Some(argument) = args.get(argument_index - 1) {
                        collect_expanded_citation_keys(
                            argument,
                            macros,
                            depth + 1,
                            budget,
                            keys,
                        );
                    }
                }
                i = end;
                continue;
            }
            let (calls, bytes_left) = if definition.intrinsic_citation {
                (&mut budget.relevant_calls, &mut budget.relevant_bytes)
            } else {
                (&mut budget.structural_calls, &mut budget.structural_bytes)
            };
            if *calls > 0 {
                let Some(expanded) =
                    fill_citation_placeholders(&definition.body, &args, *bytes_left)
                else {
                    i = end;
                    continue;
                };
                *calls -= 1;
                *bytes_left -= expanded.len();
                collect_expanded_citation_keys(&expanded, macros, depth + 1, budget, keys);
            }
            i = end;
            continue;
        }

        i = word_end;
    }
}

fn tex_token_end_for_citations(src: &str, start: usize) -> usize {
    let bytes = src.as_bytes();
    if start >= bytes.len() {
        return bytes.len();
    }
    if bytes[start] != b'\\' {
        return start + src[start..].chars().next().map_or(1, char::len_utf8);
    }
    let mut end = start + 1;
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
        end += src[end..].chars().next().map_or(1, char::len_utf8);
    }
    end
}

fn read_citation_macro_args(
    src: &str,
    from: usize,
    count: usize,
    default: Option<&str>,
) -> (Vec<String>, usize) {
    let bytes = src.as_bytes();
    let mut args = Vec::with_capacity(count);
    let mut i = from;
    let mut remaining = count;
    if let (Some(default), true) = (default, count > 0) {
        let start = skip_tex_argument_space(src, i);
        if bytes.get(start) == Some(&b'[') {
            if let Some(end) = crate::parser::tex_group_end(src, start, b'[', b']') {
                args.push(src[start + 1..end - 1].to_string());
                i = end;
            } else {
                args.push(default.to_string());
            }
        } else {
            args.push(default.to_string());
        }
        remaining -= 1;
    }
    for _ in 0..remaining {
        let start = skip_tex_argument_space(src, i);
        if bytes.get(start) != Some(&b'{') {
            args.push(String::new());
            continue;
        }
        let Some(end) = crate::parser::tex_group_end(src, start, b'{', b'}') else {
            args.push(String::new());
            continue;
        };
        args.push(src[start + 1..end - 1].to_string());
        i = end;
    }
    (args, i)
}

fn fill_citation_placeholders(
    template: &str,
    args: &[String],
    byte_limit: usize,
) -> Option<String> {
    let bytes = template.as_bytes();
    let mut output_len = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'#' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next == b'#' {
                output_len = output_len.checked_add(1)?;
                i += 2;
                continue;
            }
            if next.is_ascii_digit() && next != b'0' {
                if let Some(argument) = args.get((next - b'0') as usize - 1) {
                    output_len = output_len.checked_add(argument.len())?;
                }
                i += 2;
                continue;
            }
        }
        let ch = template[i..].chars().next().unwrap_or('\0');
        output_len = output_len.checked_add(ch.len_utf8())?;
        i += ch.len_utf8();
    }
    if output_len > byte_limit {
        return None;
    }

    let mut out = String::with_capacity(output_len);
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'#' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next == b'#' {
                out.push('#');
                i += 2;
                continue;
            }
            if next.is_ascii_digit() && next != b'0' {
                if let Some(argument) = args.get((next - b'0') as usize - 1) {
                    out.push_str(argument);
                }
                i += 2;
                continue;
            }
        }
        let ch = template[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    Some(out)
}

fn record_citation_keys(keys: impl IntoIterator<Item = String>, state: &mut State<'_>) {
    for key in keys {
        if state.labels.citation_number.contains_key(&key) {
            continue;
        }
        let n = state.labels.cite_order.len() as u32 + 1;
        state.labels.citation_number.insert(key.clone(), n);
        state.labels.cite_order.push(key);
    }
}

fn skip_tex_argument_space(src: &str, mut i: usize) -> usize {
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
    crate::parser::live_braced_command_calls(src, &["label"], 0)
        .into_iter()
        .map(|call| call.value.trim().to_string())
        .filter(|label| !label.is_empty())
        .collect()
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

/// mathtools `showonlyrefs` for single-display environments (`equation`,
/// `multline`): shown only when one of the body's labels is referenced.
/// Always shown when the option is off (`state.referenced` is `None`).
fn single_eq_is_shown(state: &State<'_>, body: &str) -> bool {
    match &state.referenced {
        None => true,
        Some(set) => labels_from_latex(body).iter().any(|l| set.contains(l)),
    }
}

/// Collect every key referenced by a `\ref`-family command in raw source
/// `src` into `out`. Line comments are skipped inline (a commented-out
/// `% \eqref{x}` does not count), matching the comment-stripped-scan
/// invariant of the preamble extractors.
/// Drives mathtools' `showonlyrefs`: an equation keeps its number only when
/// one of its labels lands in this set. mathtools itself only counts
/// `\eqref`/`\refeq`; the preview is deliberately generous — referencing a key
/// any way keeps its number visible (e.g. `\cref` + `showonlyrefs` produces
/// broken PDFs, so faithfulness to that combination isn't useful).
pub fn collect_referenced_keys(src: &str, out: &mut HashSet<String>) {
    const REF_COMMANDS: &[&str] = &[
        "ref", "eqref", "refeq", "pageref", "cref", "Cref", "autoref", "nameref",
    ];
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'\\' {
            if !bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
                // Escaped char (`\\`, `\%`, `\λ`…): skip both so an escaped
                // `%` doesn't read as a comment. The escaped char may be
                // multibyte.
                let next_w = src[i + 1..].chars().next().map_or(0, |c| c.len_utf8());
                i += 1 + next_w;
                continue;
            }
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
                end += 1;
            }
            if REF_COMMANDS.contains(&&src[start..end]) {
                let mut j = end;
                // Starred variants (`\cref*`, `\ref*`) reference all the same.
                if bytes.get(j) == Some(&b'*') {
                    j += 1;
                }
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'{') {
                    let arg_start = j + 1;
                    let mut depth = 1i32;
                    let mut k = arg_start;
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
                    if depth == 0 {
                        // `\cref{eq:a,eq:b}` and `\crefrange`-style comma lists.
                        for key in src[arg_start..k].split(',') {
                            let key = key.trim();
                            if !key.is_empty() {
                                out.insert(key.to_string());
                            }
                        }
                        i = k + 1;
                        continue;
                    }
                }
            }
            i = end;
            continue;
        }
        let ch = src[i..].chars().next().unwrap_or('\0');
        i += ch.len_utf8();
    }
}

fn row_is_unnumbered(row: &str) -> bool {
    has_latex_command(row, "notag")
        || has_latex_command(row, "nonumber")
        // A manual \tag / \tag* supplies its own number and does not advance the
        // automatic equation counter (amsmath semantics). `has_latex_command`
        // treats a trailing `*` as a word boundary, so match both spellings.
        || has_latex_command(row, "tag")
        || has_latex_command(row, "tag*")
}

/// Extract the argument of a manual `\tag{…}` / `\tag*{…}` — the author-supplied
/// equation number (e.g. `"a"`, `"$\star$"`). Both spellings store the same
/// reference value (amsmath: `\ref` shows the bare tag, `\eqref` wraps it in
/// parens); `\tag*` only changes how the equation itself prints, which MathJax
/// handles. Returns `None` when there is no braced `\tag` (e.g. `\tagsomething`,
/// or a `\tag` with no argument). Brace-matching mirrors `labels_from_latex`.
fn tag_value(src: &str) -> Option<String> {
    let needle = "\\tag";
    let bytes = src.as_bytes();
    let mut search_from = 0usize;
    while let Some(found) = src[search_from..].find(needle) {
        let start = search_from + found;
        let mut i = start + needle.len();
        // Accept an optional `*` (`\tag*{…}`).
        if bytes.get(i) == Some(&b'*') {
            i += 1;
        }
        // Anything else alphabetic means this was `\tagfoo`, not `\tag`.
        if bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
            search_from = start + needle.len();
            continue;
        }
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'{') {
            search_from = start + needle.len();
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
                        return Some(src[arg_start..i].trim().to_string());
                    }
                }
                _ => {}
            }
            i += 1;
        }
        return None;
    }
    None
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
            "equation"
                | "align"
                | "gather"
                | "multline"
                | "alignat"
                | "flalign"
                | "xalignat"
                | "eqnarray"
        )
}

fn is_multirow_numbered_math_env(env: &str) -> bool {
    !env.ends_with('*')
        && matches!(
            env,
            "align" | "gather" | "alignat" | "flalign" | "xalignat" | "eqnarray"
        )
}

fn is_float_env(env: &str) -> bool {
    matches!(env.trim_end_matches('*'), "figure" | "table" | "longtable")
}

fn is_native_table_env(env: &str) -> bool {
    matches!(env, "tabular" | "tabular*" | "tabularx")
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
            preamble_files: vec![],
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

    #[test]
    fn split_math_rows_handles_backslash_before_multibyte() {
        // Regression: `\` + a multibyte char (`\λ`, `\Δ`) advanced two bytes
        // into the codepoint and panicked on the next `src[i..]` slice.
        let rows = split_math_rows(r"x &= \λ \\ y &= 2");
        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains('λ'));
    }

    /// Parse + number a body whose theorem environments are declared by
    /// `preamble`, returning the resolved labels. Used to exercise
    /// `\newtheorem`-driven counters/titles.
    fn number_with(preamble: &str, src: &str) -> LabelTable {
        let project = project_with(preamble, src);
        let thms = TheoremRegistry::from_preamble(preamble);
        let mut ns = parse_body(&project, &thms).unwrap();
        assign_numbers(&mut ns, &HashMap::new(), BibStyle::Numeric, &thms, None)
    }

    /// Number an already-parsed body with built-in AMS defaults.
    fn assign(ns: &mut [Node]) -> LabelTable {
        assign_numbers(
            ns,
            &HashMap::new(),
            BibStyle::Numeric,
            &TheoremRegistry::with_builtin_defaults(),
            None,
        )
    }

    #[test]
    fn citations_inside_native_tables_keep_document_order() {
        let mut ns = nodes(concat!(
            "\\cite{before}\n",
            "\\begin{tabular}{l}\n",
            "\\cite[see][p.~2]{cell-a, cell-b}\\\\\n",
            "\\verb|\\cite{literal}| % \\cite{comment}\n",
            "\\iffalse\\cite{false}\\fi\n",
            "\\end{tabular}\n",
            "\\begin{table}\n",
            "\\caption{A source \\cite{caption}}\n",
            "\\begin{tabular}{l}\\cite{float-cell}\\\\\\end{tabular}\n",
            "\\end{table}\n",
            "\\begin{longtable}{l}\\cite{long}\\\\\\end{longtable}\n",
            "\\cite{after}\n",
        ));
        let labels = assign(&mut ns);

        assert_eq!(
            labels.cite_order,
            [
                "before",
                "cell-a",
                "cell-b",
                "caption",
                "float-cell",
                "long",
                "after",
            ]
        );
        assert_eq!(labels.citation_number.get("cell-a"), Some(&2));
        assert_eq!(labels.citation_number.get("float-cell"), Some(&5));
        assert!(!labels.citation_number.contains_key("literal"));
        assert!(!labels.citation_number.contains_key("comment"));
        assert!(!labels.citation_number.contains_key("false"));
    }

    #[test]
    fn every_longtable_advances_the_table_counter() {
        let mut ns = nodes(concat!(
            "\\begin{longtable}{l}Plain\\\\\\label{tab:plain}\\end{longtable}\n",
            "\\begin{longtable}{l}\\caption*{Starred}\\\\\\label{tab:star}\\end{longtable}\n",
            "\\begin{longtable}{l}\\caption{Numbered}\\\\\\label{tab:numbered}\\end{longtable}\n",
        ));
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("tab:plain").map(String::as_str), Some("1"));
        assert_eq!(labels.number.get("tab:star").map(String::as_str), Some("2"));
        assert_eq!(
            labels.number.get("tab:numbered").map(String::as_str),
            Some("3")
        );
    }

    #[test]
    fn tag_row_does_not_consume_equation_number() {
        let mut ns = nodes(
            "\\section{S}\n\
             \\begin{align}\n\
             a &= b \\label{eq:a}\\\\\n\
             c &= d \\tag{$\\star$}\\\\\n\
             e &= f \\label{eq:c}\n\
             \\end{align}\n",
        );
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("eq:a").unwrap(), "1.1");
        // The \tag row supplies its own number and is skipped, so eq:c stays
        // 1.2 rather than being offset to 1.3.
        assert_eq!(labels.number.get("eq:c").unwrap(), "1.2");
    }

    #[test]
    fn tag_equation_label_resolves_to_its_tag() {
        // Regression: \begin{equation}\label{eq:a} … \tag{a}\end{equation}
        // referenced via \ref/\eqref must show the tag "a", not the key "eq:a".
        let mut ns = nodes("\\begin{equation} \\label{eq:a} a^2 \\tag{a} \\end{equation}\n");
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("eq:a").map(String::as_str), Some("a"));
        assert_eq!(labels.resolve_ref(RefKind::Ref, "eq:a"), "a");
        assert_eq!(labels.resolve_ref(RefKind::Eqref, "eq:a"), "(a)");
    }

    #[test]
    fn tag_star_equation_label_resolves_to_its_tag() {
        let mut ns = nodes("\\begin{equation}\\label{eq:s} x \\tag*{$\\dagger$} \\end{equation}\n");
        let labels = assign(&mut ns);
        assert_eq!(
            labels.number.get("eq:s").map(String::as_str),
            Some("$\\dagger$")
        );
        assert_eq!(labels.resolve_ref(RefKind::Eqref, "eq:s"), "($\\dagger$)");
    }

    #[test]
    fn tag_row_label_resolves_to_its_tag() {
        // A \tag row inside align with its own \label maps to the tag, while the
        // auto-numbered rows keep their continuous numbers.
        let mut ns = nodes(
            "\\section{S}\n\
             \\begin{align}\n\
             a &= b \\label{eq:a}\\\\\n\
             c &= d \\tag{$\\star$}\\label{eq:t}\\\\\n\
             e &= f \\label{eq:c}\n\
             \\end{align}\n",
        );
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("eq:a").unwrap(), "1.1");
        assert_eq!(
            labels.number.get("eq:t").map(String::as_str),
            Some("$\\star$")
        );
        assert_eq!(labels.number.get("eq:c").unwrap(), "1.2");
    }

    #[test]
    fn notag_equation_label_gets_no_number() {
        // \notag without a manual \tag is truly unnumbered: a label on it has no
        // number, so refs fall back to the key (matches LaTeX — labeling an
        // unnumbered equation is a no-op).
        let mut ns = nodes("\\begin{equation}\\label{eq:n} y \\notag \\end{equation}\n");
        let labels = assign(&mut ns);
        assert!(!labels.number.contains_key("eq:n"));
        assert_eq!(labels.resolve_ref(RefKind::Ref, "eq:n"), "eq:n");
    }

    #[test]
    fn appendix_preserves_continuous_theorem_counter() {
        let labels = number_with(
            "\\newtheorem{thm}{Theorem}\n",
            "\\section{One}\n\
             \\begin{thm}\\label{t1}A\\end{thm}\n\
             \\appendix\n\
             \\section{App}\n\
             \\begin{thm}\\label{t2}B\\end{thm}\n",
        );
        assert_eq!(labels.number.get("t1").unwrap(), "1");
        // Continuous (never-reset) counter keeps counting across \appendix.
        assert_eq!(labels.number.get("t2").unwrap(), "2");
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
    fn extended_ams_alignment_environments_number_their_rows() {
        let mut ns = nodes(
            "\\section{S}\n\
             \\begin{flalign}\n\
             a &= b && \\label{eq:f1}\\\\\n\
             c &= d && \\label{eq:f2}\n\
             \\end{flalign}\n\
             \\begin{xalignat}{2}\n\
             e &= f &\\quad g &= h \\label{eq:x1}\\\\\n\
             i &= j & k &= l \\label{eq:x2}\n\
             \\end{xalignat}\n\
             \\begin{xxalignat}{2}m&=n&o&=p\\label{eq:xx}\\end{xxalignat}\n\
             \\begin{equation}\\label{eq:after}q=r\\end{equation}\n",
        );
        let labels = assign(&mut ns);
        assert_eq!(labels.number.get("eq:f1").unwrap(), "1.1");
        assert_eq!(labels.number.get("eq:f2").unwrap(), "1.2");
        assert_eq!(labels.number.get("eq:x1").unwrap(), "1.3");
        assert_eq!(labels.number.get("eq:x2").unwrap(), "1.4");
        assert!(!labels.number.contains_key("eq:xx"));
        assert_eq!(labels.number.get("eq:after").unwrap(), "1.5");
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
