//! Multi-file project loader.
//!
//! Given a root `.tex` file, recursively pull in everything reached through
//! `\input{...}`, `\include{...}`, or `\subfile{...}`, keeping each file's
//! source separate so AST nodes can be attributed to their true origin.
//!
//! Step 1 keeps this intentionally simple — every file is read once and
//! returned in include-order. Cycles are detected; missing files become
//! warnings (not crashes), per DESIGN §13.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;

use crate::ast::Pos;
use crate::root::resolve_include;

/// The preamble of the root file (everything before `\begin{document}`).
#[derive(Debug, Clone)]
pub struct Preamble {
    pub source: String,
    pub file: PathBuf,
}

/// One file participating in the project.
#[derive(Debug, Clone)]
pub struct ProjectFile {
    pub path: PathBuf,
    pub source: String,
    /// Source position of `source[0]` within `path`.
    pub start: Pos,
    /// Where this file's body content lives in the rendered order.
    /// `is_root_body` is true for the slice of the root file that comes
    /// after `\begin{document}` and before `\end{document}`.
    pub is_root_body: bool,
}

/// All inputs needed for parsing and macro extraction.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub preamble: Preamble,
    /// Files in body-include order. `files[0]` is the root file's body slice
    /// up to the first `\input/\include/\subfile`; subsequent entries are the
    /// included files, recursively. Order matches the order content would
    /// appear when LaTeX flattens the project.
    pub files: Vec<ProjectFile>,
    pub warnings: Vec<String>,
}

impl Project {
    pub fn included_files(&self) -> impl Iterator<Item = &Path> {
        let mut seen = HashSet::<PathBuf>::new();
        self.files.iter().filter_map(move |f| {
            if seen.insert(f.path.clone()) {
                Some(f.path.as_path())
            } else {
                None
            }
        })
    }
}

const INCLUDE_RE: &str = r"\\(?:input|include|subfile)\s*\{\s*([^}]+?)\s*\}";

/// Load a project from its resolved root file.
pub fn load_project(root: &Path) -> Result<Project> {
    let source =
        fs::read_to_string(root).with_context(|| format!("reading root {}", root.display()))?;
    load_project_with_source(root, source)
}

/// Load a project using `source` as the root file's content instead of
/// reading it from disk. Included files (`\input`/`\include`/`\subfile`) and
/// referenced `.sty` / `.bib` files are still read from disk. Used by
/// `serve` to render directly from an editor buffer without writing the
/// file out first.
pub fn load_project_from_source(root: &Path, source: String) -> Result<Project> {
    load_project_with_source(root, source)
}

fn load_project_with_source(root: &Path, source: String) -> Result<Project> {
    let (preamble_src, body_src, body_start) = split_preamble(&source);
    let preamble = Preamble {
        source: preamble_src.to_string(),
        file: root.to_path_buf(),
    };

    let mut files = Vec::new();
    let mut visited = HashSet::new();
    let mut warnings = Vec::new();

    // The root file's body is the first contributor.
    let root_key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    visited.insert(root_key);

    // Walk body content in source order, replacing include commands with the
    // included file content at the command site.
    let base = root.parent().unwrap_or_else(|| Path::new("."));
    append_with_includes(
        base,
        root.to_path_buf(),
        body_src,
        body_start,
        true,
        &mut files,
        &mut visited,
        &mut warnings,
        0,
    );

    Ok(Project {
        root: root.to_path_buf(),
        preamble,
        files,
        warnings,
    })
}

/// Split the root source into (preamble, body), where body is everything
/// strictly between `\begin{document}` and `\end{document}`. For a standalone
/// file with no `\begin{document}`, the whole source is the body and the
/// preamble is empty.
fn split_preamble(src: &str) -> (&str, &str, Pos) {
    static BEGIN_RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\\begin\s*\{\s*document\s*\}").unwrap());
    static END_RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\\end\s*\{\s*document\s*\}").unwrap());
    let Some(b) = BEGIN_RE.find(src) else {
        return ("", src, Pos::ZERO);
    };
    let preamble = &src[..b.start()];
    let after = &src[b.end()..];
    let body = match END_RE.find(after) {
        Some(e) => &after[..e.start()],
        None => after,
    };
    (preamble, body, pos_at(src, b.end()))
}

#[allow(clippy::too_many_arguments)]
fn append_with_includes(
    base: &Path,
    path: PathBuf,
    src: &str,
    start_pos: Pos,
    is_root_body: bool,
    out: &mut Vec<ProjectFile>,
    visited: &mut HashSet<PathBuf>,
    warnings: &mut Vec<String>,
    depth: usize,
) {
    if depth > 16 {
        warnings.push(format!("include depth exceeded at {}", base.display()));
        return;
    }
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(INCLUDE_RE).unwrap());
    let mut last = 0usize;
    for cap in RE.captures_iter(src) {
        let m = cap.get(0).unwrap();
        push_chunk(
            out,
            path.clone(),
            &src[last..m.start()],
            offset_pos(src, start_pos, last),
            is_root_body,
        );

        let name = cap.get(1).unwrap().as_str();
        let p = resolve_include(base, name);
        let canonical = p.canonicalize().unwrap_or(p.clone());
        if !visited.insert(canonical.clone()) {
            last = m.end();
            continue;
        }
        match fs::read_to_string(&canonical) {
            Ok(s) => {
                let child_base = canonical.parent().unwrap_or(base).to_path_buf();
                append_with_includes(
                    &child_base,
                    canonical.clone(),
                    &s,
                    Pos::ZERO,
                    false,
                    out,
                    visited,
                    warnings,
                    depth + 1,
                );
            }
            Err(e) => {
                warnings.push(format!("could not include {}: {}", canonical.display(), e));
                push_chunk(
                    out,
                    path.clone(),
                    &src[m.start()..m.end()],
                    offset_pos(src, start_pos, m.start()),
                    is_root_body,
                );
            }
        }
        last = m.end();
    }
    push_chunk(
        out,
        path,
        &src[last..],
        offset_pos(src, start_pos, last),
        is_root_body,
    );
}

fn push_chunk(
    out: &mut Vec<ProjectFile>,
    path: PathBuf,
    source: &str,
    start: Pos,
    is_root_body: bool,
) {
    if source.is_empty() {
        return;
    }
    out.push(ProjectFile {
        path,
        source: source.to_string(),
        start,
        is_root_body,
    });
}

fn pos_at(src: &str, byte: usize) -> Pos {
    offset_pos(src, Pos::ZERO, byte)
}

fn offset_pos(src: &str, start: Pos, byte: usize) -> Pos {
    let mut line = start.line;
    let mut col = start.col;
    for ch in src[..byte.min(src.len())].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    Pos {
        line,
        col,
        byte: start.byte + byte as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mathpreview-{name}-{nonce}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn includes_are_spliced_in_source_order() {
        let dir = temp_dir("include-order");
        let root = dir.join("main.tex");
        let child = dir.join("child.tex");
        fs::write(
            &root,
            "\\documentclass{article}\n\\begin{document}\nBefore.\n\\input{child}\nAfter.\n\\end{document}\n",
        )
        .unwrap();
        fs::write(&child, "Child.\n").unwrap();

        let project = load_project(&root).unwrap();
        let flattened = project
            .files
            .iter()
            .map(|f| f.source.as_str())
            .collect::<String>();

        assert!(flattened.contains("Before.\nChild.\n\nAfter."));
        assert!(!flattened.contains("\\input{child}"));
        let _ = fs::remove_dir_all(dir);
    }
}
