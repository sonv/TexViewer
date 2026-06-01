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
pub struct FrontMatterAuthor {
    pub name: String,
    pub addresses: Vec<String>,
    pub emails: Vec<String>,
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
    /// Optional short title from `\title[short]{long}`. Used in the topbar
    /// and other compact contexts where the full title would be too long.
    pub title_short: Option<String>,
    /// First `\author{…}` body, kept for callers that only need one author.
    pub author: Option<String>,
    /// Every `\author{…}` body in declaration order. A single command using
    /// top-level `\and` is split into multiple entries.
    pub authors: Vec<String>,
    /// Authors plus AMS-style metadata attached by following `\address{...}`
    /// and `\email{...}` declarations.
    pub author_details: Vec<FrontMatterAuthor>,
    /// `\date{…}` body.
    pub date: Option<String>,
    /// User macro names whose body wraps `\sidenote[...]{...}` — e.g.
    /// `\SV`, `\AB`, `\GI`, or any author-defined review/annotation
    /// command following the same `\newcommand{\X}[2][]{\sidenote[...]{...}}`
    /// pattern. The renderer picks these up and emits them as collapsible
    /// margin chips instead of falling back to opaque LaTeX text.
    pub sidenote_wrappers: Vec<String>,
}

/// Bundled built-in macro overrides. Loaded as the first (lowest-priority)
/// step of every extraction so that anything an author or user file later
/// re-defines automatically wins via the extractor's last-wins semantics.
const BUILTIN_MACROS_TEX: &str = include_str!("assets/builtin-macros.tex");

/// Per-project macro override filename. Looked up by walking upward from
/// the document root, the same way Git looks for `.git`.
pub const PROJECT_MACROS_FILENAME: &str = ".mathpreview-macros.tex";

/// Which override file the macros-dialog "Save" button should target.
/// `Global` writes to `~/.config/mathpreview/macros.tex`; `Project`
/// writes to the nearest `.mathpreview-macros.tex` walking up from the
/// document root, falling back to creating one in that root if none
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacrosScope {
    Global,
    Project,
}

impl MacrosScope {
    /// Parse the wire string sent by the dialog (`"global"` / `"project"`).
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "global" => Some(MacrosScope::Global),
            "project" => Some(MacrosScope::Project),
            _ => None,
        }
    }
}

/// Resolve the on-disk path that a `Save` action should write to for the
/// given scope and project root. Returns `None` for `Global` if neither
/// `XDG_CONFIG_HOME` nor `HOME` is set (vanishingly rare).
///
/// For `Project`, returns the existing `.mathpreview-macros.tex` walked
/// up from `root_dir` if one is found; otherwise the path where it
/// would be created (`root_dir/.mathpreview-macros.tex`).
pub fn resolve_override_path(root_dir: &Path, scope: MacrosScope) -> Option<PathBuf> {
    match scope {
        MacrosScope::Global => global_macros_path(),
        MacrosScope::Project => Some(
            find_project_macros(root_dir)
                .unwrap_or_else(|| root_dir.join(PROJECT_MACROS_FILENAME)),
        ),
    }
}

/// Validate a single override line as a parseable `\newcommand`-shaped
/// definition. Returns the extracted macro's name on success, or an
/// error describing why the line was rejected. Used by the macros
/// dialog to surface "this looks malformed" before writing to disk.
pub fn validate_override_line(line: &str) -> Result<String> {
    let mut e = Extractor::new();
    e.scan(line, Path::new("<dialog>"));
    let preamble = e.finish();
    if let Some(m) = preamble.macros.into_iter().next() {
        Ok(m.name)
    } else {
        let detail = preamble
            .warnings
            .into_iter()
            .next()
            .unwrap_or_else(|| "expected a \\newcommand-shaped definition".to_string());
        Err(anyhow::anyhow!("rejected macro line: {detail}"))
    }
}

/// Discover override file paths in cascade order (lowest → highest
/// priority). Caller passes any additional explicit paths (e.g. from a
/// `--macros` CLI flag) as `extra`; those land at the end of the
/// returned list so they win over both global and project files.
///
/// Lookup order:
///   1. `$XDG_CONFIG_HOME/mathpreview/macros.tex` (falls back to
///      `~/.config/mathpreview/macros.tex` if `XDG_CONFIG_HOME` unset).
///   2. The nearest `.mathpreview-macros.tex` walking up from `root_dir`.
///   3. Each path in `extra`, in order.
///
/// Non-existent paths are still returned — `extract_preamble_with_overrides`
/// (via `finish_render`) tolerates missing files so callers can blindly
/// pass the result without stat-checking each entry.
pub fn discover_macro_overrides(root_dir: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = global_macros_path() {
        if p.is_file() {
            out.push(p);
        }
    }
    if let Some(p) = find_project_macros(root_dir) {
        out.push(p);
    }
    out.extend(extra.iter().cloned());
    out
}

fn global_macros_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("mathpreview").join("macros.tex"))
}

fn find_project_macros(start: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let candidate = dir.join(PROJECT_MACROS_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    None
}

/// One user-supplied macro override file, in cascade order. Later entries
/// in the slice override earlier ones on name collision.
#[derive(Debug, Clone)]
pub struct MacroOverride {
    /// Display path for warnings / debug panel (typically the file path).
    pub label: PathBuf,
    /// Raw `.tex` contents — same syntax as a preamble (`\newcommand` etc.).
    pub source: String,
}

/// Extract macros and package mappings for a loaded project. Equivalent to
/// [`extract_preamble_with_overrides`] with no extra overrides — kept for
/// callers that don't need the cascade.
pub fn extract_preamble(project: &Project) -> Result<ExtractedPreamble> {
    extract_preamble_with_overrides(project, &[])
}

/// Extract macros + package mappings, then layer the override files on
/// top in the order given. Cascade: bundled built-ins → paper preamble
/// (including local `.sty` / `.tex` it references) → each override in
/// `overrides` (ordered lowest to highest priority).
pub fn extract_preamble_with_overrides(
    project: &Project,
    overrides: &[MacroOverride],
) -> Result<ExtractedPreamble> {
    let mut extractor = Extractor::new();

    // 1) Bundled built-ins (lowest priority).
    extractor.scan(BUILTIN_MACROS_TEX, Path::new("<builtin-macros.tex>"));

    // 2) Paper preamble.
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

    // 3) User overrides, in cascade order (later wins on name collision).
    for ov in overrides {
        extractor.scan(&ov.source, &ov.label);
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

        // Text-flow helpers such as KFP's \step and \case are parsed by the
        // document renderer, not expanded by MathJax. Do not surface them as
        // scary macro warnings just because their LaTeX body contains
        // counters or conditionals.
        if is_text_flow_macro(&name) {
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
        let metadata_src = strip_line_comments(&self.raw);
        let (title_short, title) = extract_optional_and_brace_arg(&metadata_src, r"\title");
        let author_details = extract_front_matter_authors(&metadata_src);
        let authors: Vec<String> = author_details.iter().map(|a| a.name.clone()).collect();
        let author = authors.first().cloned();
        let date = extract_brace_arg(&metadata_src, r"\date");
        // Any extracted user macro whose body wraps `\sidenote[...]{...}`
        // (svmacro.sty's `\SV` / `\AB` and any author-defined equivalent
        // like `\GI`) is a sidenote wrapper — the renderer treats them
        // the same way as the bare `\sidenote` command.
        let sidenote_wrappers: Vec<String> = self
            .macros
            .iter()
            .filter(|m| m.body.trim_start().starts_with(r"\sidenote"))
            .map(|m| m.name.clone())
            .collect();
        ExtractedPreamble {
            macros: self.macros,
            packages_short,
            packages_long,
            unmapped_packages: self.packages.unmapped.clone(),
            warnings: self.warnings,
            raw_preamble: self.raw,
            title,
            title_short,
            author,
            authors,
            author_details,
            date,
            sidenote_wrappers,
        }
    }
}

/// Like `extract_brace_arg`, but also captures the LaTeX optional `[...]`
/// argument when present. Returns `(optional, brace)`. For `\title[Short]{Long}`
/// this is `(Some("Short"), Some("Long"))`; for `\title{Long}` it's
/// `(None, Some("Long"))`.
fn extract_optional_and_brace_arg(src: &str, cmd: &str) -> (Option<String>, Option<String>) {
    let bytes = src.as_bytes();
    let needle = cmd.as_bytes();
    let mut i = 0;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let after = bytes.get(i + needle.len()).copied().unwrap_or(b' ');
            if !after.is_ascii_alphabetic() {
                let mut j = i + needle.len();
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let mut optional: Option<String> = None;
                if j < bytes.len() && bytes[j] == b'[' {
                    if let Some((opt, next)) = read_bracket_arg(src, j) {
                        optional = Some(opt);
                        j = next;
                        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                    }
                }
                if j < bytes.len() && bytes[j] == b'{' {
                    if let Some((arg, _)) = read_brace_arg(src, j) {
                        return (optional, Some(arg));
                    }
                }
                return (optional, None);
            }
        }
        i += 1;
    }
    (None, None)
}

/// Read a `[...]` LaTeX optional argument starting at `start` (the `[`).
/// Returns `(contents, end_index_after_])`. Brace-balanced inside, just
/// like `read_brace_arg`.
fn read_bracket_arg(src: &str, start: usize) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut depth = 1i32;
    let mut i = start + 1;
    let arg_start = i;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    let arg = src[arg_start..i - 1].to_string();
                    return Some((arg, i));
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Find the first `\<cmd>{...}` and return the brace-balanced contents.
fn extract_brace_arg(src: &str, cmd: &str) -> Option<String> {
    extract_brace_args(src, cmd).into_iter().next()
}

/// Find every `\<cmd>{...}` and return brace-balanced contents. Supports a
/// single LaTeX optional argument before the required brace arg, e.g.
/// `\author[short]{long}`.
fn extract_brace_args(src: &str, cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
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
                if j < bytes.len() && bytes[j] == b'[' {
                    if let Some(next) = skip_bracket_arg(src, j) {
                        j = next;
                        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                    }
                }
                if j < bytes.len() && bytes[j] == b'{' {
                    if let Some((arg, next)) = read_brace_arg(src, j) {
                        out.push(arg);
                        i = next;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

fn skip_bracket_arg(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut depth = 1i32;
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
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

fn read_brace_arg(src: &str, start: usize) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let arg_start = start + 1;
    let mut depth = 1i32;
    let mut i = arg_start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((src[arg_start..i].to_string(), i + 1));
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

fn split_author_arg(src: &str) -> Vec<String> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if depth == 0
            && bytes.get(i..i + 4) == Some(br"\and")
            && !bytes
                .get(i + 4)
                .copied()
                .is_some_and(|b| b.is_ascii_alphabetic())
        {
            out.push(src[start..i].to_string());
            i += 4;
            start = i;
            continue;
        }
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth = (depth - 1).max(0);
                i += 1;
            }
            _ => i += 1,
        }
    }
    out.push(src[start..].to_string());
    out
}

fn extract_front_matter_authors(src: &str) -> Vec<FrontMatterAuthor> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }

        let command = if command_at(src, i, r"\author") {
            Some("author")
        } else if command_at(src, i, r"\address") {
            Some("address")
        } else if command_at(src, i, r"\curraddr") {
            Some("curraddr")
        } else if command_at(src, i, r"\email") {
            Some("email")
        } else {
            None
        };

        let Some(command) = command else {
            i += 1;
            continue;
        };
        let cmd_len = command.len() + 1;
        let Some((arg, next)) = command_brace_arg(src, i + cmd_len, command == "author") else {
            i += cmd_len;
            continue;
        };
        match command {
            "author" => {
                for name in split_author_arg(&arg) {
                    let name = name.trim();
                    if !name.is_empty() {
                        out.push(FrontMatterAuthor {
                            name: name.to_string(),
                            addresses: Vec::new(),
                            emails: Vec::new(),
                        });
                    }
                }
            }
            "address" => {
                if let Some(author) = out.last_mut() {
                    let address = arg.trim();
                    if !address.is_empty() {
                        author.addresses.push(address.to_string());
                    }
                }
            }
            "curraddr" => {
                if let Some(author) = out.last_mut() {
                    let address = arg.trim();
                    if !address.is_empty() {
                        author.addresses.push(format!("Current address: {address}"));
                    }
                }
            }
            "email" => {
                if let Some(author) = out.last_mut() {
                    let email = arg.trim();
                    if !email.is_empty() {
                        author.emails.push(email.to_string());
                    }
                }
            }
            _ => {}
        }
        i = next;
    }
    out
}

fn command_at(src: &str, start: usize, command: &str) -> bool {
    let bytes = src.as_bytes();
    let needle = command.as_bytes();
    if bytes.get(start..start + needle.len()) != Some(needle) {
        return false;
    }
    !bytes
        .get(start + needle.len())
        .copied()
        .is_some_and(|b| b.is_ascii_alphabetic())
}

fn command_brace_arg(
    src: &str,
    mut pos: usize,
    allow_optional_arg: bool,
) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    if allow_optional_arg && pos < bytes.len() && bytes[pos] == b'[' {
        pos = skip_bracket_arg(src, pos)?;
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
    }
    read_brace_arg(src, pos)
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

fn is_text_flow_macro(name: &str) -> bool {
    matches!(name, "case" | "step" | "restartsteps")
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

    fn preamble(src: &str) -> ExtractedPreamble {
        let mut e = Extractor::new();
        e.scan(src, Path::new("test.tex"));
        e.finish()
    }

    /// Cascade the same way `extract_preamble_with_overrides` does, but
    /// driven by string inputs so tests don't have to construct a full
    /// `Project`. The order matches the production cascade:
    /// builtins → paper preamble → each override (later wins).
    fn cascade(paper_preamble: &str, override_sources: &[(&str, &str)]) -> Vec<ExtractedMacro> {
        let mut e = Extractor::new();
        e.scan(BUILTIN_MACROS_TEX, Path::new("<builtin-macros.tex>"));
        e.scan(paper_preamble, Path::new("paper.tex"));
        for (label, src) in override_sources {
            e.scan(src, Path::new(label));
        }
        e.finish().macros
    }

    fn find_by_name<'a>(ms: &'a [ExtractedMacro], name: &str) -> Option<&'a ExtractedMacro> {
        ms.iter().find(|m| m.name == name)
    }

    #[test]
    fn cascade_builtins_only_keeps_every_fallback() {
        let ms = cascade("", &[]);
        for name in [
            "given",
            "st",
            "bm",
            "underaccent",
            "xspace",
            "defeq",
            "sidenote",
            "add",
            "remove",
            "highlight",
            "replace",
        ] {
            assert!(
                find_by_name(&ms, name).is_some(),
                "missing built-in {name}",
            );
        }
    }

    #[test]
    fn cascade_paper_preamble_overrides_builtin() {
        let ms = cascade(r"\newcommand{\bm}{__paper__}", &[]);
        let bm = find_by_name(&ms, "bm").expect("\\bm present");
        assert_eq!(bm.body, "__paper__");
        assert_eq!(bm.source_file, Path::new("paper.tex"));
    }

    #[test]
    fn cascade_override_wins_over_paper_preamble() {
        let ms = cascade(
            r"\newcommand{\bm}[1]{__paper__{#1}}",
            &[(
                "global.tex",
                r"\newcommand{\bm}[1]{__override__{#1}}",
            )],
        );
        let bm = find_by_name(&ms, "bm").expect("\\bm present");
        assert_eq!(bm.body, "__override__{#1}");
        assert_eq!(bm.source_file, Path::new("global.tex"));
    }

    #[test]
    fn cascade_second_override_wins_over_first() {
        let ms = cascade(
            "",
            &[
                ("global.tex", r"\newcommand{\foo}{global}"),
                ("project.tex", r"\newcommand{\foo}{project}"),
            ],
        );
        let foo = find_by_name(&ms, "foo").expect("\\foo present");
        assert_eq!(foo.body, "project");
        assert_eq!(foo.source_file, Path::new("project.tex"));
    }

    #[test]
    fn cascade_missing_override_file_is_skipped_by_caller() {
        // `finish_render` filters missing files via `fs::read_to_string`
        // before reaching the extractor — we just confirm the contract
        // here: passing zero overrides leaves built-ins + preamble alone.
        let ms = cascade(r"\newcommand{\bar}{B}", &[]);
        assert_eq!(find_by_name(&ms, "bar").map(|m| m.body.as_str()), Some("B"));
    }

    #[test]
    fn validate_override_accepts_well_formed_newcommand() {
        let name = validate_override_line(r"\newcommand{\foo}{bar}").unwrap();
        assert_eq!(name, "foo");
    }

    #[test]
    fn validate_override_rejects_random_string() {
        let err = validate_override_line("not a macro at all").unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("rejected"),
            "got: {err}"
        );
    }

    #[test]
    fn resolve_path_global_returns_xdg_or_home_path() {
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/mp-test-xdg");
        std::env::remove_var("HOME");
        let p = resolve_override_path(Path::new("/tmp"), MacrosScope::Global).unwrap();
        if let Some(v) = prev_xdg { std::env::set_var("XDG_CONFIG_HOME", v); } else { std::env::remove_var("XDG_CONFIG_HOME"); }
        if let Some(v) = prev_home { std::env::set_var("HOME", v); }
        assert!(
            p.ends_with("mathpreview/macros.tex"),
            "expected ends with mathpreview/macros.tex; got {}",
            p.display()
        );
    }

    #[test]
    fn resolve_path_project_creates_path_when_missing() {
        let tmp = std::env::temp_dir().join(format!(
            "mp-resolve-{}-{}",
            std::process::id(),
            line!(),
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let p = resolve_override_path(&tmp, MacrosScope::Project).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(p, tmp.join(PROJECT_MACROS_FILENAME));
    }

    #[test]
    fn resolve_path_project_finds_existing_in_ancestor() {
        let tmp = std::env::temp_dir().join(format!(
            "mp-resolve-existing-{}-{}",
            std::process::id(),
            line!(),
        ));
        let nested = tmp.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let existing = tmp.join(PROJECT_MACROS_FILENAME);
        std::fs::write(&existing, "% placeholder\n").unwrap();
        let p = resolve_override_path(&nested, MacrosScope::Project).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(p, existing);
    }

    #[test]
    fn discover_finds_project_macros_in_ancestor() {
        let tmp = std::env::temp_dir().join(format!(
            "mp-discover-{}",
            std::process::id(),
        ));
        let nested = tmp.join("chapters");
        std::fs::create_dir_all(&nested).unwrap();
        let project_file = tmp.join(PROJECT_MACROS_FILENAME);
        std::fs::write(&project_file, r"\newcommand{\foo}{F}").unwrap();
        // Use an env-isolated discovery to keep the test from picking up
        // the developer's real ~/.config/mathpreview/macros.tex.
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_home = std::env::var_os("HOME");
        let isolated = tmp.join("fake-home");
        std::fs::create_dir_all(&isolated).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &isolated);
        std::env::set_var("HOME", &isolated);
        let found = discover_macro_overrides(&nested, &[]);
        // Restore env before any assertion that might panic.
        if let Some(v) = prev_xdg { std::env::set_var("XDG_CONFIG_HOME", v); } else { std::env::remove_var("XDG_CONFIG_HOME"); }
        if let Some(v) = prev_home { std::env::set_var("HOME", v); } else { std::env::remove_var("HOME"); }
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(
            found.iter().any(|p| p.ends_with(PROJECT_MACROS_FILENAME)),
            "project macros file not discovered from nested dir: {found:?}",
        );
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
    fn title_extracts_optional_short_argument() {
        let p = preamble(r#"\title[Short Title]{Long Title With $a^2$ in it}"#);
        assert_eq!(p.title.as_deref(), Some("Long Title With $a^2$ in it"));
        assert_eq!(p.title_short.as_deref(), Some("Short Title"));
    }

    #[test]
    fn title_without_optional_leaves_short_none() {
        let p = preamble(r#"\title{Just the Long Title}"#);
        assert_eq!(p.title.as_deref(), Some("Just the Long Title"));
        assert_eq!(p.title_short, None);
    }

    #[test]
    fn multiple_authors_are_preserved() {
        let p = preamble(
            r#"
\title{Paper}
\author{Alice A.}
\email{alice@example.test}
\address{Department A\\ University A}

\author{Bob B. \and Carol C.}
\email{carol@example.test}
%\author{Commented Out}
"#,
        );
        assert_eq!(p.author.as_deref(), Some("Alice A."));
        assert_eq!(
            p.authors,
            vec![
                "Alice A.".to_string(),
                "Bob B.".to_string(),
                "Carol C.".to_string()
            ]
        );
        assert_eq!(p.author_details[0].emails, vec!["alice@example.test"]);
        assert_eq!(
            p.author_details[0].addresses,
            vec![r"Department A\\ University A"]
        );
        assert!(p.author_details[1].emails.is_empty());
        assert_eq!(p.author_details[2].emails, vec!["carol@example.test"]);
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
