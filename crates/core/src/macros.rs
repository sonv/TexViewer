//! Preamble macro extraction (DESIGN §6).
//!
//! Mirrors `latex-preview.nvim`'s strategy: scan the root preamble (and any
//! local `.sty` / `.tex` files referenced by `\usepackage` / `\input`) for
//! definition-shaped commands and lower them to MathJax's `tex.macros`
//! configuration shape.
//!
//! Normalization rules (opt-in, on by default):
//!   * `\providecommand` → `\newcommand`  (so MathJax doesn't silently keep
//!     its own built-in definition when the author overrode it).
//!   * `\edef` → `\def`                   (MathJax doesn't expand-at-definition).
//!
//! Never crash. When a definition uses a form we don't recognize, surface a
//! warning and keep going.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::packages::PackageMap;
use crate::project::Project;

/// One extracted macro, in the shape MathJax wants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMacro {
    pub name: String,
    pub body: String,
    pub n_args: u8,
    /// Default value for `#1` when the macro has an optional first argument.
    pub default: Option<String>,
    /// Original source form, for the debug panel.
    pub source: String,
    pub source_file: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedPreamble {
    pub macros: Vec<ExtractedMacro>,
    pub packages_short: Vec<String>,
    pub packages_long: Vec<String>,
    pub unmapped_packages: Vec<String>,
    pub warnings: Vec<String>,
    /// Verbatim concatenation of every preamble source scanned. Used by the
    /// debug panel referenced in DESIGN §6 ("see exactly what preamble is
    /// being sent to MathJax").
    pub raw_preamble: String,
    /// `\title{…}` body — rendered when the document calls `\maketitle`.
    pub title: Option<String>,
    /// `\author{…}` body.
    pub author: Option<String>,
    /// `\date{…}` body.
    pub date: Option<String>,
}

/// Extract macros and package mappings for a loaded project.
pub fn extract_preamble(project: &Project) -> Result<ExtractedPreamble> {
    let mut extractor = Extractor::new();
    extractor.scan(&project.preamble.source, &project.preamble.file);

    // Step 3 of DESIGN §6 — scan local .sty / .tex referenced from the preamble.
    let base = project
        .preamble
        .file
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let referenced = collect_referenced_files(&project.preamble.source, base);
    let mut visited: HashSet<PathBuf> = HashSet::new();
    visited.insert(project.preamble.file.clone());
    for f in referenced {
        if !visited.insert(f.clone()) {
            continue;
        }
        match fs::read_to_string(&f) {
            Ok(src) => extractor.scan(&src, &f),
            Err(_) => {
                // Missing local macro files are routine — system-installed
                // packages won't be on disk, that's fine.
                continue;
            }
        }
    }

    Ok(extractor.finish())
}

struct Extractor {
    macros: Vec<ExtractedMacro>,
    packages: PackageMap,
    warnings: Vec<String>,
    seen_names: HashSet<String>,
    raw: String,
}

impl Extractor {
    fn new() -> Self {
        Self {
            macros: Vec::new(),
            packages: PackageMap::new(),
            warnings: Vec::new(),
            seen_names: HashSet::new(),
            raw: String::new(),
        }
    }

    fn scan(&mut self, src: &str, file: &Path) {
        self.raw
            .push_str(&format!("%% ---- {} ----\n", file.display()));
        self.raw.push_str(src);
        self.raw.push('\n');

        // Strip line comments before scanning so a `%` doesn't accidentally
        // eat half a definition.
        let cleaned = strip_line_comments(src);

        // \usepackage[opts]{a,b,c} and \RequirePackage{...}
        static PKG_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r"\\(?:usepackage|RequirePackage)\s*(?:\[[^\]]*\])?\s*\{\s*([^}]+?)\s*\}")
                .unwrap()
        });
        for cap in PKG_RE.captures_iter(&cleaned) {
            let names = cap.get(1).unwrap().as_str();
            for n in names.split(',') {
                let n = n.trim();
                if !n.is_empty() {
                    self.packages.add(n);
                }
            }
        }

        // \newcommand / \renewcommand / \providecommand
        self.scan_newcommand_like(&cleaned, file);

        // \DeclareMathOperator[*]
        self.scan_declare_math_operator(&cleaned, file);

        // \NewDocumentCommand / \RenewDocumentCommand / \ProvideDocumentCommand
        self.scan_xparse(&cleaned, file);

        // \def and \edef (\edef normalized to \def)
        self.scan_def(&cleaned, file);

        // \let\new=\old   (or \let\new\old)
        self.scan_let(&cleaned, file);

        // \DeclarePairedDelimiter and common wrappers (e.g. svmacro.sty's
        // \newdelim) — lower to a 1-arg \newcommand using \left/\right.
        self.scan_paired_delimiter(&cleaned, file);
    }

    fn scan_newcommand_like(&mut self, src: &str, file: &Path) {
        static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r"\\(newcommand|renewcommand|providecommand)\b").unwrap()
        });
        let re = &*RE;
        for m in re.find_iter(src) {
            let head = m.as_str();
            let start = m.end();
            let mut sc = ArgScanner::new(src, start);

            let name = match sc.command_name_arg() {
                Some(n) => n,
                None => {
                    self.warnings.push(format!(
                        "{}: could not parse name in {}",
                        file.display(),
                        head,
                    ));
                    continue;
                }
            };
            let n_args = sc.optional_number_arg().unwrap_or(0);
            let default = sc.optional_arg();
            let body = match sc.balanced_brace_arg() {
                Some(b) => b,
                None => {
                    self.warnings.push(format!(
                        "{}: missing body for \\{} {}",
                        file.display(),
                        head,
                        name,
                    ));
                    continue;
                }
            };

            // providecommand → newcommand (always force the author's def).
            let _kind_override = head.contains("providecommand");

            let source = format!("{}{}", head, &src[start..sc.pos]);
            self.push(file, name, body, n_args, default, source);
        }
    }

    fn scan_declare_math_operator(&mut self, src: &str, file: &Path) {
        static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r"\\DeclareMathOperator(\*)?(?:\s|\{)").unwrap()
        });
        let re = &*RE;
        for cap in re.captures_iter(src) {
            let starred = cap.get(1).is_some();
            // Resume after the matched prefix excluding the lookahead char
            // (whitespace or `{`), which the ArgScanner will re-consume.
            let after_prefix = if starred {
                cap.get(1).unwrap().end()
            } else {
                cap.get(0).unwrap().start() + "\\DeclareMathOperator".len()
            };
            let start = after_prefix;
            let mut sc = ArgScanner::new(src, start);
            let Some(name) = sc.command_name_arg() else {
                continue;
            };
            let Some(op) = sc.balanced_brace_arg() else {
                continue;
            };
            let body = if starred {
                format!("\\operatorname*{{{}}}", op)
            } else {
                format!("\\operatorname{{{}}}", op)
            };
            let source = format!(
                "\\DeclareMathOperator{}{}",
                if starred { "*" } else { "" },
                &src[start..sc.pos]
            );
            self.push(file, name, body, 0, None, source);
        }
    }

    fn scan_xparse(&mut self, src: &str, file: &Path) {
        static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r"\\(?:New|Renew|Provide)DocumentCommand\b").unwrap()
        });
        let re = &*RE;
        for m in re.find_iter(src) {
            let head = m.as_str();
            let start = m.end();
            let mut sc = ArgScanner::new(src, start);
            let Some(name) = sc.command_name_arg() else {
                self.warnings.push(format!(
                    "{}: could not parse name in {}",
                    file.display(),
                    head
                ));
                continue;
            };
            let Some(spec) = sc.balanced_brace_arg() else {
                continue;
            };
            let Some(body) = sc.balanced_brace_arg() else {
                continue;
            };
            let n_args = count_mandatory_args(&spec);
            let default = leading_optional_default(&spec);
            let source = format!("{}{}", head, &src[start..sc.pos]);
            self.push(file, name, body, n_args, default, source);
        }
    }

    fn scan_def(&mut self, src: &str, file: &Path) {
        static RE: std::sync::LazyLock<Regex> =
            std::sync::LazyLock::new(|| Regex::new(r"\\(edef|def)\s*\\([A-Za-z@]+)").unwrap());
        let re = &*RE;
        for cap in re.captures_iter(src) {
            let kw = cap.get(1).unwrap().as_str();
            let name = cap.get(2).unwrap().as_str().to_string();
            let after_name_end = cap.get(0).unwrap().end();

            // Parse the parameter pattern up to the next `{` — that's the
            // (#1, #2, ...) signature. For preview purposes we only need
            // the arity.
            let bytes = src.as_bytes();
            let mut p = after_name_end;
            let mut n_args: u8 = 0;
            while p < bytes.len() {
                let b = bytes[p];
                if b == b'{' {
                    break;
                }
                if b == b'#' && p + 1 < bytes.len() && bytes[p + 1].is_ascii_digit() {
                    let d = bytes[p + 1] - b'0';
                    if d > n_args {
                        n_args = d;
                    }
                    p += 2;
                    continue;
                }
                p += 1;
            }
            let mut sc = ArgScanner::new(src, p);
            let Some(body) = sc.balanced_brace_arg() else {
                continue;
            };
            let source = format!("\\{} \\{}{}", kw, name, &src[after_name_end..sc.pos]);
            self.push(file, name, body, n_args, None, source);
        }
    }

    fn scan_paired_delimiter(&mut self, src: &str, file: &Path) {
        // `\DeclarePairedDelimiter{\name}{open}{close}` (from mathtools) and
        // the svmacro.sty wrapper `\newdelim{\name}{open}{close}` both produce
        // a 1-arg macro that wraps its argument in `open` … `close`. For
        // preview we always use \left/\right so the delimiters scale; the
        // unscaled (non-star) form is a minor cosmetic loss.
        static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r"\\(DeclarePairedDelimiter|newdelim)\b").unwrap()
        });
        let re = &*RE;
        for m in re.find_iter(src) {
            let kw = m.as_str();
            let start = m.end();
            let mut sc = ArgScanner::new(src, start);
            let Some(name) = sc.command_name_arg() else {
                continue;
            };
            let Some(open) = sc.balanced_brace_arg() else {
                continue;
            };
            let Some(close) = sc.balanced_brace_arg() else {
                continue;
            };
            let body = format!("\\left{} #1 \\right{}", open, close);
            let source = format!("{}{}", kw, &src[start..sc.pos]);
            self.push(file, name, body, 1, None, source);
        }
    }

    fn scan_let(&mut self, src: &str, file: &Path) {
        // \let\name=\target or \let\name\target — alias to target.
        static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r"\\let\s*\\([A-Za-z@]+)\s*=?\s*(\\[A-Za-z@]+|.)").unwrap()
        });
        let re = &*RE;
        for cap in re.captures_iter(src) {
            let name = cap.get(1).unwrap().as_str().to_string();
            let target = cap.get(2).unwrap().as_str();
            if !target.starts_with('\\') {
                // \let to a single character — rare in math, skip.
                continue;
            }
            let source = cap.get(0).unwrap().as_str().to_string();
            self.push(file, name, target.to_string(), 0, None, source);
        }
    }

    fn push(
        &mut self,
        file: &Path,
        name: String,
        body: String,
        n_args: u8,
        default: Option<String>,
        source: String,
    ) {
        // Names containing `@` are LaTeX-internal by convention — they live
        // between `\makeatletter` / `\makeatother` and are implementation
        // details of style files, never invoked in math mode. MathJax has no
        // use for them.
        if name.contains('@') {
            return;
        }

        // Bodies that rely on TeX primitives MathJax can't expand are
        // worse than useless — they'll likely error at typeset time. Warn
        // and skip; mirrors the §6 "Failure mode" guidance.
        if body_uses_unsupported_primitives(&body) {
            self.warnings.push(format!(
                "{}: skipping \\{} — body uses TeX primitives MathJax can't expand",
                file.display(),
                name,
            ));
            return;
        }

        // Last definition wins, matching LaTeX semantics. Replace any prior entry.
        if !self.seen_names.insert(name.clone()) {
            self.macros.retain(|m| m.name != name);
            self.seen_names.insert(name.clone());
        }
        self.macros.push(ExtractedMacro {
            name,
            body,
            n_args,
            default,
            source,
            source_file: file.to_path_buf(),
        });
    }

    fn finish(self) -> ExtractedPreamble {
        let packages_short = self
            .packages
            .short_names()
            .into_iter()
            .map(String::from)
            .collect();
        let packages_long = self
            .packages
            .extensions()
            .into_iter()
            .map(String::from)
            .collect();
        let title = extract_brace_arg(&self.raw, r"\title");
        let author = extract_brace_arg(&self.raw, r"\author");
        let date = extract_brace_arg(&self.raw, r"\date");
        ExtractedPreamble {
            macros: self.macros,
            packages_short,
            packages_long,
            unmapped_packages: self.packages.unmapped.clone(),
            warnings: self.warnings,
            raw_preamble: self.raw,
            title,
            author,
            date,
        }
    }
}

/// Find the first `\<cmd>{...}` and return the brace-balanced contents.
fn extract_brace_arg(src: &str, cmd: &str) -> Option<String> {
    let bytes = src.as_bytes();
    let needle = cmd.as_bytes();
    let mut i = 0;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Ensure next char isn't alphabetic (i.e. \title vs \titlepage).
            let after = bytes.get(i + needle.len()).copied().unwrap_or(b' ');
            if !after.is_ascii_alphabetic() {
                let mut j = i + needle.len();
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'{' {
                    let start = j + 1;
                    let mut depth = 1i32;
                    let mut k = start;
                    while k < bytes.len() {
                        match bytes[k] {
                            b'\\' if k + 1 < bytes.len() => k += 2,
                            b'{' => {
                                depth += 1;
                                k += 1;
                            }
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    return Some(src[start..k].to_string());
                                }
                                k += 1;
                            }
                            _ => k += 1,
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Find `\usepackage` and `\input` targets in `src` that look like they could
/// resolve to a local file (i.e. a sibling `.sty` or `.tex`).
fn collect_referenced_files(src: &str, base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let usepkg =
        Regex::new(r"\\(?:usepackage|RequirePackage)\s*(?:\[[^\]]*\])?\s*\{\s*([^}]+?)\s*\}")
            .unwrap();
    let inp = Regex::new(r"\\(?:input|include)\s*\{\s*([^}]+?)\s*\}").unwrap();
    for cap in usepkg.captures_iter(src) {
        for n in cap.get(1).unwrap().as_str().split(',') {
            let n = n.trim();
            if n.is_empty() {
                continue;
            }
            // Walk up to MAX_PARENT_DEPTH dirs looking for a local .sty match.
            let mut dir = Some(base.to_path_buf());
            for _ in 0..=crate::root::MAX_PARENT_DEPTH {
                let Some(d) = dir.clone() else { break };
                let cand = d.join(format!("{n}.sty"));
                if cand.is_file() {
                    out.push(cand);
                    break;
                }
                dir = d.parent().map(Path::to_path_buf);
            }
        }
    }
    for cap in inp.captures_iter(src) {
        let n = cap.get(1).unwrap().as_str().trim();
        let p = Path::new(n);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        };
        let withext = if p.extension().is_some() {
            joined.clone()
        } else {
            joined.with_extension("tex")
        };
        if withext.is_file() {
            out.push(withext);
        }
    }
    out
}

fn strip_line_comments(src: &str) -> String {
    // Remove `%` to end-of-line, but respect escaped `\%`.
    let mut out = String::with_capacity(src.len());
    for line in src.split_inclusive('\n') {
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut emit_end = line.len();
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if bytes[i] == b'%' {
                emit_end = i;
                break;
            }
            i += 1;
        }
        out.push_str(&line[..emit_end]);
        if emit_end < line.len() {
            // Preserve the trailing newline so line numbers stay aligned.
            if line.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

fn body_uses_unsupported_primitives(body: &str) -> bool {
    // Heuristic: any of these in a macro body almost certainly means MathJax
    // will fail at expansion. The list is conservative; new entries should
    // be driven by concrete papers that broke.
    const FORBIDDEN: &[&str] = &[
        "\\expandafter",
        "\\csname",
        "\\endcsname",
        "\\if",
        "\\else",
        "\\fi",
        "\\setkeys",
        "\\define@key",
        "\\renewenvironment",
        "\\newenvironment",
        "\\AtBeginDocument",
        "\\kern",
        "\\nonscript",
    ];
    if FORBIDDEN.iter().any(|p| body.contains(p)) {
        return true;
    }
    // `##` is TeX's escape for nested-def parameter substitution. It only
    // makes sense inside a `\def` whose body itself takes a parameter and
    // declares an inner `\def`. Top-level macro bodies with `##` confuse
    // MathJax's expansion (seen as "internal buffer exceeded" recursion).
    if body.contains("##") {
        return true;
    }
    // Bodies that invoke a TeX-private (`@`-containing) command will dangle
    // — we filter those private names out of the macro table, so calling
    // them produces an unknown-macro error or an infinite expansion through
    // some default. Easier to skip the wrapper than to chase the chain.
    static PRIVATE_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\\[A-Za-z]+@[A-Za-z@]*").unwrap());
    if PRIVATE_RE.is_match(body) {
        return true;
    }
    false
}

fn count_mandatory_args(spec: &str) -> u8 {
    // xparse spec: each `m` is a mandatory arg. `o`, `O{default}`, `s`, `t`, `e{...}`
    // are optional / other forms we don't faithfully reproduce here.
    spec.chars().filter(|c| *c == 'm').count().min(9) as u8
}

fn leading_optional_default(spec: &str) -> Option<String> {
    // `O{default}` as the first slot → default for #1.
    let re = Regex::new(r"^\s*O\{([^}]*)\}").unwrap();
    re.captures(spec)
        .map(|c| c.get(1).unwrap().as_str().to_string())
}

/// Brace-/bracket-aware micro-scanner that mirrors what a recursive descent
/// parser does when consuming `\command` arguments.
struct ArgScanner<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ArgScanner<'a> {
    fn new(src: &'a str, pos: usize) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    /// Parse `\name` or `{\name}` — the form `\newcommand` accepts for the
    /// macro being defined.
    fn command_name_arg(&mut self) -> Option<String> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return None;
        }
        if self.bytes[self.pos] == b'{' {
            let arg = self.balanced_brace_arg()?;
            let arg = arg.trim();
            arg.strip_prefix('\\').map(|s| s.to_string())
        } else if self.bytes[self.pos] == b'\\' {
            let start = self.pos + 1;
            let mut end = start;
            while end < self.bytes.len()
                && (self.bytes[end].is_ascii_alphabetic() || self.bytes[end] == b'@')
            {
                end += 1;
            }
            if end == start {
                return None;
            }
            let name = self.src[start..end].to_string();
            self.pos = end;
            Some(name)
        } else {
            None
        }
    }

    /// `[N]` where N is a digit; returns N, consuming the bracket pair.
    fn optional_number_arg(&mut self) -> Option<u8> {
        self.skip_ws();
        if self.pos >= self.bytes.len() || self.bytes[self.pos] != b'[' {
            return None;
        }
        let save = self.pos;
        let arg = self.optional_arg()?;
        match arg.trim().parse::<u8>() {
            Ok(n) => Some(n),
            Err(_) => {
                self.pos = save;
                None
            }
        }
    }

    /// `[...]` — content between brackets. No nesting of brackets, but we
    /// honor brace pairs so `[{\foo}]` works.
    fn optional_arg(&mut self) -> Option<String> {
        self.skip_ws();
        if self.pos >= self.bytes.len() || self.bytes[self.pos] != b'[' {
            return None;
        }
        let mut i = self.pos + 1;
        let mut depth = 0i32;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b']' if depth == 0 => {
                    let inside = &self.src[self.pos + 1..i];
                    self.pos = i + 1;
                    return Some(inside.to_string());
                }
                b'\\' if i + 1 < self.bytes.len() => {
                    i += 1;
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// `{...}` — balanced.
    fn balanced_brace_arg(&mut self) -> Option<String> {
        self.skip_ws();
        if self.pos >= self.bytes.len() || self.bytes[self.pos] != b'{' {
            return None;
        }
        let mut depth = 0i32;
        let mut i = self.pos;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b'\\' if i + 1 < self.bytes.len() => {
                    i += 2;
                    continue;
                }
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let inside = &self.src[self.pos + 1..i];
                        self.pos = i + 1;
                        return Some(inside.to_string());
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(src: &str) -> Vec<ExtractedMacro> {
        let mut e = Extractor::new();
        e.scan(src, Path::new("test.tex"));
        e.finish().macros
    }

    #[test]
    fn newcommand_no_args() {
        let m = extract(r"\newcommand{\R}{\mathbb{R}}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "R");
        assert_eq!(m[0].body, r"\mathbb{R}");
        assert_eq!(m[0].n_args, 0);
    }

    #[test]
    fn newcommand_with_args() {
        let m = extract(r"\newcommand{\norm}[1]{\left\|#1\right\|}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "norm");
        assert_eq!(m[0].n_args, 1);
    }

    #[test]
    fn newcommand_with_optional_default() {
        let m = extract(r"\newcommand{\foo}[2][x]{#1+#2}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].n_args, 2);
        assert_eq!(m[0].default.as_deref(), Some("x"));
    }

    #[test]
    fn declare_math_operator() {
        let m = extract(r"\DeclareMathOperator{\Tr}{Tr}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "Tr");
        assert_eq!(m[0].body, r"\operatorname{Tr}");
    }

    #[test]
    fn declare_math_operator_star() {
        let m = extract(r"\DeclareMathOperator*{\argmin}{arg\,min}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].body, r"\operatorname*{arg\,min}");
    }

    #[test]
    fn def() {
        let m = extract(r"\def\foo#1#2{#1 + #2}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "foo");
        assert_eq!(m[0].n_args, 2);
    }

    #[test]
    fn balanced_braces_in_body() {
        let m = extract(r"\newcommand{\vec}[1]{\mathbf{#1}}");
        assert_eq!(m[0].body, r"\mathbf{#1}");
    }

    #[test]
    fn comments_are_stripped() {
        let m = extract("\\newcommand{\\R}{\\mathbb{R}} % real numbers\n");
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn paired_delimiter() {
        let m = extract(r"\DeclarePairedDelimiter{\norm}{\lVert}{\rVert}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "norm");
        assert_eq!(m[0].n_args, 1);
        assert_eq!(m[0].body, r"\left\lVert #1 \right\rVert");
    }

    #[test]
    fn newdelim_wrapper() {
        let m = extract(r"\newdelim{\abs}{\lvert}{\rvert}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "abs");
        assert_eq!(m[0].body, r"\left\lvert #1 \right\rvert");
    }

    #[test]
    fn redefinition_replaces() {
        let m = extract("\\newcommand{\\R}{old}\n\\renewcommand{\\R}{new}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].body, "new");
    }
}
