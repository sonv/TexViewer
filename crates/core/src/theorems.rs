//! Parses `\newtheorem` / `\newtheorem*` / `\numberwithin` declarations from
//! the preamble into a registry that drives:
//!
//! * **recognition** — which `\begin{env}` names the parser treats as
//!   theorem-like (beyond the built-in list), so custom environments such as
//!   `assumption`, `setting`, or `question` get parsed as theorems;
//! * **the heading word** shown for each (`Theorem`, `Lemma`, `Satz`, …);
//! * **numbering** — whether the environment is numbered, which counter it
//!   advances (shared vs independent), and the sectioning level it resets
//!   under.
//!
//! This is what lets the preview's theorem/lemma numbers match a real
//! `latexmk` build instead of assuming one fixed AMS convention. When the
//! preamble declares nothing, the registry falls back to the AMS-modern
//! default the renderer used before: all built-in environments share one
//! `theorem` counter, reset per `\section`.

use std::collections::HashMap;

use crate::ast::THEOREM_LIKES;

/// Sectioning level a counter resets under (part=0 … subparagraph=6). Mirrors
/// `parser::section_level` so a reset level indexes the same counter array the
/// numbering pass walks.
fn section_level(name: &str) -> Option<u8> {
    Some(match name {
        "part" => 0,
        "chapter" => 1,
        "section" => 2,
        "subsection" => 3,
        "subsubsection" => 4,
        "paragraph" => 5,
        "subparagraph" => 6,
        _ => return None,
    })
}

/// Default heading word for the built-in recognized environments and their
/// common abbreviations. Used for built-ins and as a fallback when an
/// environment isn't in the registry.
fn default_title(env: &str) -> &'static str {
    match env {
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

#[derive(Debug, Clone)]
pub struct TheoremDef {
    /// Heading word, e.g. "Theorem", "Lemma", "Satz", "Assumption".
    pub title: String,
    /// `\newtheorem*` → false (unnumbered). Numbered otherwise.
    pub numbered: bool,
    /// Counter this environment advances. Environments declared with a shared
    /// counter (`\newtheorem{lemma}[theorem]{Lemma}`) point at one name.
    pub counter: String,
}

#[derive(Debug, Clone, Default)]
pub struct TheoremRegistry {
    /// env name → its definition.
    defs: HashMap<String, TheoremDef>,
    /// counter name → sectioning level it resets under. `None` value =
    /// continuous numbering (never resets, no section prefix).
    counter_reset: HashMap<String, Option<u8>>,
}

impl TheoremRegistry {
    /// AMS-modern defaults for the built-in recognized environments: all share
    /// one `theorem` counter, reset per `\section`, English titles. Reproduces
    /// the behaviour the renderer/numbering had before `\newtheorem` was
    /// honoured.
    pub fn with_builtin_defaults() -> Self {
        let mut reg = TheoremRegistry::default();
        reg.counter_reset.insert("theorem".to_string(), Some(2)); // section
        for env in THEOREM_LIKES {
            reg.defs.insert(
                (*env).to_string(),
                TheoremDef {
                    title: default_title(env).to_string(),
                    numbered: true,
                    counter: "theorem".to_string(),
                },
            );
        }
        reg
    }

    /// Build from a single preamble source.
    pub fn from_preamble(src: &str) -> Self {
        Self::from_sources(&[src.to_string()])
    }

    /// Build from a project: the root preamble plus every local `.sty` / `.tex`
    /// it `\usepackage`s / `\input`s (the same files the macro extractor reads).
    /// This is how `\newtheorem`s declared in a sibling package (e.g.
    /// `svmacro.sty`) are honored.
    pub fn from_project(project: &crate::project::Project) -> Self {
        let mut sources: Vec<String> = vec![project.preamble.source.clone()];
        sources.extend(
            project
                .preamble_files
                .iter()
                .map(|file| file.source.clone()),
        );
        Self::from_sources(&sources)
    }

    /// Build from the built-in defaults, then apply every `\newtheorem` /
    /// `\newtheorem*` / `\numberwithin` declaration found across `sources`, in
    /// order (so shared counters resolve against earlier definitions).
    ///
    /// An environment declared more than once is skipped: that only happens
    /// across mutually-exclusive conditional branches (`\if…\else…\fi`) — LaTeX
    /// forbids redeclaring a theorem otherwise — and we can't evaluate the
    /// conditional, so rather than let an arbitrary branch win we leave that
    /// environment at its built-in default.
    pub fn from_sources(sources: &[String]) -> Self {
        let mut reg = Self::with_builtin_defaults();
        let mut decls: Vec<Decl> = Vec::new();
        for src in sources {
            decls.extend(scan_declarations(src));
        }
        let mut counts: HashMap<String, u32> = HashMap::new();
        for d in &decls {
            if let Decl::NewTheorem { env, .. } = d {
                *counts.entry(env.clone()).or_insert(0) += 1;
            }
        }
        for decl in decls {
            if let Decl::NewTheorem { ref env, .. } = decl {
                if counts.get(env).copied().unwrap_or(0) > 1 {
                    continue; // ambiguous (conditional) — keep the built-in default
                }
            }
            reg.apply(decl);
        }
        reg
    }

    /// Override the detected per-counter reset levels with a global scheme from
    /// the viewer config. `Auto` keeps what was detected from `\newtheorem` (or
    /// the built-in default); `Continuous` makes every theorem counter number
    /// document-wide (no section prefix); `Section` forces per-section numbering.
    /// Applied after the registry is built, so it wins over both detection and
    /// the built-in default — the escape hatch for declarations the viewer can't
    /// see (a conditional `\if…\newtheorem` block, an unresolvable package).
    pub fn apply_numbering_scheme(&mut self, scheme: crate::config::TheoremNumbering) {
        use crate::config::TheoremNumbering;
        let level = match scheme {
            TheoremNumbering::Auto => return,
            TheoremNumbering::Continuous => None,
            TheoremNumbering::Section => Some(2), // within-section (N.M)
        };
        // Reset every counter that backs a theorem-like environment.
        let counters: std::collections::HashSet<String> =
            self.defs.values().map(|d| d.counter.clone()).collect();
        for c in counters {
            self.counter_reset.insert(c, level);
        }
    }

    fn apply(&mut self, decl: Decl) {
        match decl {
            Decl::NewTheorem {
                env,
                shared,
                title,
                reset,
                numbered,
            } => {
                let counter = shared.clone().unwrap_or_else(|| env.clone());
                // An explicit `[reset]` wins. A shared counter inherits whatever
                // that counter already resets under. A brand-new own counter
                // with no `[reset]` never resets (continuous numbering).
                let reset_level = if let Some(level) = reset.as_deref().and_then(section_level) {
                    Some(level)
                } else if shared.is_some() {
                    self.counter_reset.get(&counter).copied().flatten()
                } else {
                    None
                };
                self.counter_reset.insert(counter.clone(), reset_level);
                self.defs.insert(
                    env,
                    TheoremDef {
                        title,
                        numbered,
                        counter,
                    },
                );
            }
            Decl::NumberWithin { counter, within } => {
                if let Some(level) = section_level(&within) {
                    self.counter_reset.insert(counter, Some(level));
                }
            }
        }
    }

    /// Whether `env` (already `*`-stripped) is a theorem-like environment.
    pub fn is_theorem(&self, env: &str) -> bool {
        self.defs.contains_key(env)
    }

    /// Heading word for `env` (e.g. "Theorem", "Satz", "Assumption").
    pub fn title(&self, env: &str) -> String {
        self.defs
            .get(env)
            .map(|d| d.title.clone())
            .unwrap_or_else(|| default_title(env).to_string())
    }

    /// Whether `env` produces a number (`\newtheorem*` → false).
    pub fn numbered(&self, env: &str) -> bool {
        self.defs.get(env).map(|d| d.numbered).unwrap_or(true)
    }

    /// Counter name `env` advances.
    pub fn counter(&self, env: &str) -> String {
        self.defs
            .get(env)
            .map(|d| d.counter.clone())
            .unwrap_or_else(|| env.to_string())
    }

    /// Sectioning level `env`'s counter resets under, if any.
    pub fn reset_level(&self, env: &str) -> Option<u8> {
        self.counter_reset_level(&self.counter(env))
    }

    /// Sectioning level a given counter resets under, if any.
    pub fn counter_reset_level(&self, counter: &str) -> Option<u8> {
        self.counter_reset.get(counter).copied().flatten()
    }
}

enum Decl {
    NewTheorem {
        env: String,
        shared: Option<String>,
        title: String,
        reset: Option<String>,
        numbered: bool,
    },
    NumberWithin {
        counter: String,
        within: String,
    },
}

/// Read a `{...}` or `[...]` group starting at (or after whitespace from)
/// `start`. Returns the trimmed content and the index just past the closer.
/// Honors backslash escapes and nesting of the same delimiter.
fn read_delimited(src: &str, start: usize, open: u8, close: u8) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&open) {
        return None;
    }
    let content_start = i + 1;
    let mut depth = 1i32;
    i = content_start;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Some((src[content_start..i].trim().to_string(), i + 1));
            }
        }
        i += 1;
    }
    None
}

fn opt(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Scan the preamble for `\newtheorem`, `\newtheorem*`, and `\numberwithin`,
/// skipping `%` comments. Tolerant of unfamiliar shapes — anything that
/// doesn't parse cleanly is skipped rather than guessed at.
fn scan_declarations(src: &str) -> Vec<Decl> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < src.len() {
        match bytes[i] {
            b'%' => {
                // LaTeX comment — skip to end of line.
                i += 1;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'\\' => {
                if let Some(after) = src[i..].strip_prefix("\\newtheorem") {
                    let mut j = i + "\\newtheorem".len();
                    let numbered = if after.starts_with('*') {
                        j += 1;
                        false
                    } else {
                        true
                    };
                    if let Some((env, j1)) = read_delimited(src, j, b'{', b'}') {
                        // Either `[shared]{title}` or `{title}[reset]`.
                        let (shared, j2) = match read_delimited(src, j1, b'[', b']') {
                            Some((s, a)) => (opt(s), a),
                            None => (None, j1),
                        };
                        if let Some((title, j3)) = read_delimited(src, j2, b'{', b'}') {
                            let (reset, j4) = if shared.is_none() {
                                match read_delimited(src, j3, b'[', b']') {
                                    Some((r, a)) => (opt(r), a),
                                    None => (None, j3),
                                }
                            } else {
                                (None, j3)
                            };
                            if let Some(env) = opt(env) {
                                out.push(Decl::NewTheorem {
                                    env,
                                    shared,
                                    title,
                                    reset,
                                    numbered,
                                });
                            }
                            i = j4;
                            continue;
                        }
                    }
                    i += "\\newtheorem".len();
                    continue;
                }
                if src[i..].starts_with("\\numberwithin") {
                    let j = i + "\\numberwithin".len();
                    if let Some((counter, j1)) = read_delimited(src, j, b'{', b'}') {
                        if let Some((within, j2)) = read_delimited(src, j1, b'{', b'}') {
                            if let Some(counter) = opt(counter) {
                                out.push(Decl::NumberWithin { counter, within });
                            }
                            i = j2;
                            continue;
                        }
                    }
                    i += "\\numberwithin".len();
                    continue;
                }
                // Some other control sequence — skip the backslash and the
                // command name so we don't re-scan its letters.
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
            }
            _ => {
                let ch = src[i..].chars().next().unwrap_or('\0');
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
    fn numbering_scheme_override_forces_continuous_then_section() {
        let mut r =
            TheoremRegistry::from_sources(&["\\newtheorem{theorem}{Theorem}[section]".to_string()]);
        assert_eq!(r.reset_level("theorem"), Some(2));
        r.apply_numbering_scheme(crate::config::TheoremNumbering::Continuous);
        assert_eq!(
            r.reset_level("theorem"),
            None,
            "continuous should clear the reset"
        );
        r.apply_numbering_scheme(crate::config::TheoremNumbering::Section);
        assert_eq!(
            r.reset_level("theorem"),
            Some(2),
            "section should restore N.M"
        );
        r.apply_numbering_scheme(crate::config::TheoremNumbering::Auto);
        assert_eq!(r.reset_level("theorem"), Some(2), "auto is a no-op");
    }

    #[test]
    fn continuous_override_rescues_ambiguous_conditional_declaration() {
        // A conditional double-declaration is skipped → stuck at the built-in
        // section default; the config override recovers continuous numbering.
        let mut r = TheoremRegistry::from_sources(&[
            "\\newtheorem{theorem}{Theorem}[section]\n\\newtheorem{theorem}{Theorem}".to_string(),
        ]);
        assert_eq!(r.reset_level("theorem"), Some(2));
        r.apply_numbering_scheme(crate::config::TheoremNumbering::Continuous);
        assert_eq!(r.reset_level("theorem"), None);
    }

    #[test]
    fn builtin_defaults_share_theorem_counter_section_reset() {
        let r = TheoremRegistry::with_builtin_defaults();
        assert!(r.is_theorem("theorem"));
        assert!(r.is_theorem("lemma"));
        assert_eq!(r.counter("lemma"), "theorem");
        assert_eq!(r.reset_level("lemma"), Some(2));
        assert_eq!(r.title("thm"), "Theorem");
    }

    #[test]
    fn independent_counters() {
        let r = TheoremRegistry::from_preamble(
            "\\newtheorem{theorem}{Theorem}[section]\n\
             \\newtheorem{lemma}{Lemma}[section]\n",
        );
        assert_eq!(r.counter("theorem"), "theorem");
        assert_eq!(r.counter("lemma"), "lemma");
        assert_eq!(r.reset_level("theorem"), Some(2));
        assert_eq!(r.reset_level("lemma"), Some(2));
    }

    #[test]
    fn shared_counter_inherits_reset() {
        let r = TheoremRegistry::from_preamble(
            "\\newtheorem{theorem}{Theorem}[section]\n\
             \\newtheorem{lemma}[theorem]{Lemma}\n",
        );
        assert_eq!(r.counter("lemma"), "theorem");
        assert_eq!(r.reset_level("lemma"), Some(2));
    }

    #[test]
    fn continuous_when_no_reset() {
        let r = TheoremRegistry::from_preamble("\\newtheorem{theorem}{Theorem}\n");
        assert_eq!(r.reset_level("theorem"), None);
        // a builtin that shares the theorem counter now also goes continuous
        assert_eq!(r.reset_level("lemma"), None);
    }

    #[test]
    fn custom_env_and_title() {
        let r = TheoremRegistry::from_preamble("\\newtheorem{assumption}{Assumption}[section]\n");
        assert!(r.is_theorem("assumption"));
        assert_eq!(r.title("assumption"), "Assumption");
        assert_eq!(r.reset_level("assumption"), Some(2));
    }

    #[test]
    fn starred_is_unnumbered() {
        let r = TheoremRegistry::from_preamble("\\newtheorem*{remark}{Remark}\n");
        assert!(r.is_theorem("remark"));
        assert!(!r.numbered("remark"));
    }

    #[test]
    fn non_english_title() {
        let r = TheoremRegistry::from_preamble("\\newtheorem{thm}{Satz}[section]\n");
        assert_eq!(r.title("thm"), "Satz");
    }

    #[test]
    fn numberwithin_sets_reset() {
        let r = TheoremRegistry::from_preamble(
            "\\newtheorem{theorem}{Theorem}\n\\numberwithin{theorem}{subsection}\n",
        );
        assert_eq!(r.reset_level("theorem"), Some(3));
    }

    #[test]
    fn conditional_duplicate_env_falls_back_to_default() {
        // svmacro.sty shape: theorem declared twice across \if/\else with
        // different reset behavior. The duplicate is ambiguous, so `theorem`
        // stays at the built-in default (section reset), and `lemma` (declared
        // once, sharing it) inherits that — matching the common AMS result.
        let r = TheoremRegistry::from_sources(&[
            "\\ifSV@numwithin\n\
             \\newtheorem{theorem}{Theorem}[section]\n\
             \\else\n\
             \\newtheorem{theorem}{Theorem}\n\
             \\fi\n\
             \\newtheorem{lemma}[theorem]{Lemma}\n\
             \\newtheorem{problem}{Problem}[section]\n"
                .to_string(),
        ]);
        assert_eq!(r.reset_level("theorem"), Some(2)); // default kept, not None
        assert_eq!(r.counter("lemma"), "theorem");
        assert_eq!(r.reset_level("lemma"), Some(2));
        // A non-duplicated custom env from the same source is still honored.
        assert!(r.is_theorem("problem"));
        assert_eq!(r.counter("problem"), "problem");
    }

    #[test]
    fn commented_declarations_are_ignored() {
        let r = TheoremRegistry::from_preamble("% \\newtheorem{ghost}{Ghost}\n");
        assert!(!r.is_theorem("ghost"));
    }
}
