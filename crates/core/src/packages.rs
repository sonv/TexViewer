//! `\usepackage{...}` → MathJax extension mapping.
//!
//! Packages not in the table are silently ignored — MathJax doesn't need
//! to know about `geometry` or `fancyhdr`.

use std::collections::BTreeSet;

/// Pairs of (LaTeX package name, MathJax extension id).
pub const PACKAGE_TO_MATHJAX: &[(&str, &str)] = &[
    ("amsmath", "[tex]/ams"),
    ("amssymb", "[tex]/ams"),
    ("amsthm", "[tex]/ams"),
    ("mathtools", "[tex]/mathtools"),
    ("physics", "[tex]/physics"),
    ("mhchem", "[tex]/mhchem"),
    ("cancel", "[tex]/cancel"),
    ("color", "[tex]/color"),
    ("xcolor", "[tex]/color"),
    ("braket", "[tex]/braket"),
    ("upgreek", "[tex]/upgreek"),
    ("siunitx", "[tex]/textmacros"),
    ("bm", "[tex]/boldsymbol"),
    ("boldsymbol", "[tex]/boldsymbol"),
    ("enclose", "[tex]/enclose"),
    ("verb", "[tex]/verb"),
    ("centernot", "[tex]/centernot"),
    ("textcomp", "[tex]/textcomp"),
    ("gensymb", "[tex]/gensymb"),
    ("bbox", "[tex]/bbox"),
    ("html", "[tex]/html"),
];

/// Common LaTeX packages that affect layout, fonts, images, captions, or PDF
/// metadata rather than MathJax's TeX input. They are expected in real papers
/// and should not be reported as MathJax mapping problems.
pub const PACKAGE_NOOP: &[&str] = &[
    "inputenc",
    "fontenc",
    "subcaption",
    "caption",
    "array",
    "latexsym",
    "graphics",
    "graphicx",
    "epsfig",
    "epsf",
    "setspace",
    "amsfonts",
    "multicol",
    "colordvi",
    "hyperref",
    "geometry",
    "url",
];

/// Map a sequence of `\usepackage` names to MathJax extension ids,
/// deduplicated and sorted for deterministic output.
#[derive(Debug, Default, Clone)]
pub struct PackageMap {
    extensions: BTreeSet<&'static str>,
    /// Package names the author requested but we don't have a MathJax mapping for.
    /// Surfaced for diagnostics only.
    pub unmapped: Vec<String>,
}

impl PackageMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, package_name: &str) {
        let name = package_name.trim();
        if PACKAGE_NOOP.contains(&name) {
            return;
        }
        let mut hit = false;
        for (pkg, ext) in PACKAGE_TO_MATHJAX {
            if *pkg == name {
                self.extensions.insert(*ext);
                hit = true;
            }
        }
        if !hit {
            self.unmapped.push(name.to_string());
        }
    }

    pub fn extensions(&self) -> Vec<&'static str> {
        self.extensions.iter().copied().collect()
    }

    /// Short names suitable for MathJax's `tex.packages[+]` array
    /// (e.g. `[tex]/ams` → `ams`).
    pub fn short_names(&self) -> Vec<&'static str> {
        self.extensions
            .iter()
            .map(|e| e.rsplit('/').next().unwrap_or(e))
            .collect()
    }
}
