//! TeX root resolution (DESIGN §4).
//!
//! Step 1 implements:
//!   1. `% !TEX root = path/to/main.tex` magic-comment lookup.
//!   3. Parent-directory walk for a `.tex` file with `\begin{document}` that
//!      reaches the current file through `\input` / `\include` / `\subfile`.
//!   4. Fall back to treating the input as standalone.
//!
//! Step 2 in the design (vimtex RPC lookup) is reserved for the daemon and
//! deliberately skipped here.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use regex::Regex;

const MAGIC_RE: &str = r"(?m)^\s*%\s*!\s*TEX\s+root\s*=\s*(\S.*?)\s*$";
const INCLUDE_RE: &str = r"\\(?:input|include|subfile)\s*\{\s*([^}]+?)\s*\}";
const BEGIN_DOC_RE: &str = r"\\begin\s*\{\s*document\s*\}";

/// Search depth for the parent-directory walk.
pub const MAX_PARENT_DEPTH: usize = 4;

/// Resolve the project root for `path`.
///
/// `path` may itself be the root, or an included chapter file. The returned
/// path is always canonicalized.
pub fn resolve_root(path: &Path) -> Result<PathBuf> {
    let path = canonicalize(path)?;
    let src = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    // 1) Magic comment in the file itself.
    if let Some(root) = magic_root(&path, &src)? {
        return Ok(root);
    }

    // 3) Parent-directory walk.
    if let Some(root) = walk_for_root(&path)? {
        return Ok(root);
    }

    // 4) Standalone.
    Ok(path)
}

/// Resolve `% !TEX root = ...` relative to the file containing the directive.
fn magic_root(file: &Path, src: &str) -> Result<Option<PathBuf>> {
    let re = Regex::new(MAGIC_RE).unwrap();
    let Some(m) = re.captures(src).and_then(|c| c.get(1)) else {
        return Ok(None);
    };
    let rel = Path::new(m.as_str());
    let base = file.parent().unwrap_or_else(|| Path::new("."));
    let abs = if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        base.join(rel)
    };
    let abs = canonicalize(&abs)
        .with_context(|| format!("magic comment points to missing file {}", abs.display()))?;
    Ok(Some(abs))
}

fn walk_for_root(start: &Path) -> Result<Option<PathBuf>> {
    let mut dir_opt = start.parent().map(Path::to_path_buf);
    for _ in 0..=MAX_PARENT_DEPTH {
        let Some(dir) = dir_opt.clone() else { break };

        // Collect candidate .tex files (non-recursive: just this directory).
        let candidates = read_tex_files(&dir).unwrap_or_default();
        for candidate in candidates {
            if candidate == *start {
                continue;
            }
            if file_contains(&candidate, BEGIN_DOC_RE).unwrap_or(false)
                && reaches(&candidate, start)?
            {
                return Ok(Some(candidate));
            }
        }

        dir_opt = dir.parent().map(Path::to_path_buf);
    }
    Ok(None)
}

fn read_tex_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("tex") {
            out.push(canonicalize(&p)?);
        }
    }
    Ok(out)
}

fn file_contains(path: &Path, pattern: &str) -> Result<bool> {
    let re = Regex::new(pattern).unwrap();
    let src = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(re.is_match(&src))
}

/// True iff `candidate_root` transitively `\input`s / `\include`s / `\subfile`s
/// `target`.
fn reaches(candidate_root: &Path, target: &Path) -> Result<bool> {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let target = canonicalize(target)?;
    reaches_inner(candidate_root, &target, &mut visited)
}

fn reaches_inner(file: &Path, target: &Path, visited: &mut HashSet<PathBuf>) -> Result<bool> {
    let file = match canonicalize(file) {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    if file == *target {
        return Ok(true);
    }
    if !visited.insert(file.clone()) {
        return Ok(false);
    }

    let Ok(src) = fs::read_to_string(&file) else {
        return Ok(false);
    };
    let re = Regex::new(INCLUDE_RE).unwrap();
    let base = file.parent().unwrap_or_else(|| Path::new("."));
    for cap in re.captures_iter(&src) {
        let name = cap.get(1).unwrap().as_str();
        let p = resolve_include(base, name);
        if reaches_inner(&p, target, visited)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Resolve an `\input{NAME}` reference relative to `base`, appending `.tex`
/// if NAME has no extension.
pub fn resolve_include(base: &Path, name: &str) -> PathBuf {
    let p = Path::new(name);
    let has_ext = p.extension().is_some();
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    if has_ext {
        joined
    } else {
        joined.with_extension("tex")
    }
}

fn canonicalize(p: &Path) -> Result<PathBuf> {
    p.canonicalize()
        .with_context(|| format!("canonicalizing {}", p.display()))
        .or_else(|_| {
            // Fall back to the absolute form even if the file doesn't exist
            // — useful so error messages show what we tried to read.
            if p.is_absolute() {
                Ok(p.to_path_buf())
            } else {
                Err(anyhow!("cannot canonicalize {}", p.display()))
            }
        })
}
