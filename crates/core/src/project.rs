//! Multi-file project loader.
//!
//! Given a root `.tex` file, recursively pull in everything reached through
//! `\input{...}`, `\include{...}`, or `\subfile{...}`, keeping each file's
//! source separate so AST nodes can be attributed to their true origin.
//!
//! Step 1 keeps this intentionally simple — every file is read once and
//! returned in include-order. Cycles are detected; missing files become
//! warnings (not crashes), per DESIGN §13.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;

use crate::ast::Pos;
use crate::root::{resolve_include, MAX_PARENT_DEPTH};

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
    load_project_with_source_and_overrides(root, source, &HashMap::new())
}

/// Load a project using `source` as the root file's content instead of
/// reading it from disk. Included files (`\input`/`\include`/`\subfile`) and
/// referenced `.sty` / `.bib` files are still read from disk. Used by
/// `serve` to render directly from an editor buffer without writing the
/// file out first.
pub fn load_project_from_source(root: &Path, source: String) -> Result<Project> {
    load_project_with_source_and_overrides(root, source, &HashMap::new())
}

/// Load a project while replacing selected files with in-memory buffer
/// contents. Keys should be canonical file paths when possible.
pub fn load_project_with_overrides(
    root: &Path,
    overrides: &HashMap<PathBuf, String>,
) -> Result<Project> {
    let source = read_source_with_overrides(root, overrides)
        .with_context(|| format!("reading root {}", root.display()))?;
    load_project_with_source_and_overrides(root, source, overrides)
}

fn load_project_with_source_and_overrides(
    root: &Path,
    source: String,
    overrides: &HashMap<PathBuf, String>,
) -> Result<Project> {
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
        overrides,
        0,
    );

    Ok(Project {
        root: root.to_path_buf(),
        preamble,
        files,
        warnings,
    })
}

fn override_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn read_source_with_overrides(
    path: &Path,
    overrides: &HashMap<PathBuf, String>,
) -> std::io::Result<String> {
    if let Some(source) = overrides.get(&override_key(path)) {
        return Ok(source.clone());
    }
    if let Some(source) = overrides.get(path) {
        return Ok(source.clone());
    }
    fs::read_to_string(path)
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
    overrides: &HashMap<PathBuf, String>,
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
        // Containment: an untrusted `.tex` must not be able to splice files
        // from outside the project into the served HTML. Reject absolute
        // targets and relative paths that climb more than MAX_PARENT_DEPTH
        // levels above their referrer; leave the include command as literal
        // text so the source still round-trips.
        if !include_within_bounds(name) {
            warnings.push(format!("skipped out-of-project include {name}"));
            push_chunk(
                out,
                path.clone(),
                &src[m.start()..m.end()],
                offset_pos(src, start_pos, m.start()),
                is_root_body,
            );
            last = m.end();
            continue;
        }
        let p = resolve_include(base, name);
        let canonical = p.canonicalize().unwrap_or(p.clone());
        if !visited.insert(canonical.clone()) {
            last = m.end();
            continue;
        }
        match read_source_with_overrides(&canonical, overrides) {
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
                    overrides,
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

/// Bounded-containment check for an `\input`/`\include`/`\subfile` target.
/// Returns `false` for absolute paths and for relative paths that climb more
/// than `MAX_PARENT_DEPTH` levels above the referring file (e.g.
/// `../../../../../../etc/passwd`). This "bounded" policy still permits the
/// `../shared/...`-style parent includes real multi-file projects rely on while
/// blocking the arbitrary-file-read vector. Measured on the path string, so it
/// is independent of where the project lives on disk.
///
/// Scope: this covers the body-include splice path only — `% !TEX root` and
/// `.sty`/`.bib` resolution are separate and not bounded here.
fn include_within_bounds(name: &str) -> bool {
    use std::path::Component;
    let p = Path::new(name);
    if p.is_absolute() {
        return false;
    }
    let mut depth: i32 = 0;
    let mut max_up: i32 = 0;
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                depth -= 1;
                max_up = max_up.max(-depth);
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    max_up <= MAX_PARENT_DEPTH as i32
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

    #[test]
    fn included_file_overrides_are_spliced_without_disk_write() {
        let dir = temp_dir("include-override");
        let root = dir.join("main.tex");
        let child = dir.join("child.tex");
        fs::write(
            &root,
            "\\documentclass{article}\n\\begin{document}\nBefore.\n\\input{child}\nAfter.\n\\end{document}\n",
        )
        .unwrap();
        fs::write(&child, "Disk child.\n").unwrap();

        let mut overrides = HashMap::new();
        overrides.insert(child.canonicalize().unwrap(), "Live child.\n".to_string());
        let project = load_project_with_overrides(&root, &overrides).unwrap();
        let flattened = project
            .files
            .iter()
            .map(|f| f.source.as_str())
            .collect::<String>();

        assert!(flattened.contains("Before.\nLive child.\n\nAfter."));
        assert!(!flattened.contains("Disk child."));
        assert_eq!(fs::read_to_string(&child).unwrap(), "Disk child.\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn absolute_include_outside_project_is_skipped() {
        // Security: an untrusted document must not exfiltrate arbitrary local
        // files by splicing them into the rendered output via an absolute path.
        let dir = temp_dir("traversal-abs");
        let secret_dir = temp_dir("traversal-secret");
        let secret = secret_dir.join("secret.tex");
        fs::write(&secret, "TOPSECRET-LEAK\n").unwrap();
        let root = dir.join("main.tex");
        fs::write(
            &root,
            format!(
                "\\documentclass{{article}}\n\\begin{{document}}\nBefore.\n\\input{{{}}}\nAfter.\n\\end{{document}}\n",
                secret.display(),
            ),
        )
        .unwrap();

        let project = load_project(&root).unwrap();
        let flattened = project
            .files
            .iter()
            .map(|f| f.source.as_str())
            .collect::<String>();

        assert!(
            !flattened.contains("TOPSECRET-LEAK"),
            "absolute include leaked file contents: {flattened}"
        );
        assert!(project
            .warnings
            .iter()
            .any(|w| w.contains("out-of-project include")));
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(secret_dir);
    }

    #[test]
    fn deep_parent_traversal_include_is_skipped() {
        // `../../../../../../../../etc/passwd`-style traversal escapes far above
        // the project root and must be refused.
        let dir = temp_dir("traversal-deep");
        let root = dir.join("main.tex");
        let ups = "../".repeat(MAX_PARENT_DEPTH + 4);
        fs::write(
            &root,
            format!(
                "\\documentclass{{article}}\n\\begin{{document}}\n\\input{{{ups}etc/hostname}}\n\\end{{document}}\n"
            ),
        )
        .unwrap();

        let project = load_project(&root).unwrap();
        assert!(project
            .warnings
            .iter()
            .any(|w| w.contains("out-of-project include")));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn parent_dir_include_within_bounds_is_spliced() {
        // The bounded policy still allows a normal `../shared` sibling include.
        let parent = temp_dir("bounds-parent");
        let proj = parent.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(parent.join("shared.tex"), "SHARED-OK\n").unwrap();
        let root = proj.join("main.tex");
        fs::write(
            &root,
            "\\documentclass{article}\n\\begin{document}\n\\input{../shared}\n\\end{document}\n",
        )
        .unwrap();

        let project = load_project(&root).unwrap();
        let flattened = project
            .files
            .iter()
            .map(|f| f.source.as_str())
            .collect::<String>();

        assert!(
            flattened.contains("SHARED-OK"),
            "bounded parent include should splice: {flattened}"
        );
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn include_bounds_metric() {
        // Within the project, or a few parents up: allowed.
        assert!(include_within_bounds("child.tex"));
        assert!(include_within_bounds("sub/dir/child.tex"));
        assert!(include_within_bounds("./child.tex"));
        assert!(include_within_bounds("../shared/preamble.tex"));
        assert!(include_within_bounds("../../common/defs.tex"));
        assert!(include_within_bounds("a/../b/child.tex"));
        // Absolute, or climbing more than MAX_PARENT_DEPTH (4) levels: rejected.
        assert!(!include_within_bounds("/etc/passwd"));
        assert!(!include_within_bounds("../../../../../etc/passwd"));
    }
}
