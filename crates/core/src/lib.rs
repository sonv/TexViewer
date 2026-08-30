//! mathpreview core: parser, macro extractor, renderer.
//!
//! Use [`converter`] for the versioned source-language contract,
//! [`render_document`] for automatic LaTeX/Markdown dispatch, or
//! [`render_project`] to force the historical LaTeX project pipeline.

pub mod ast;
pub mod bibtex;
pub mod config;
pub mod converter;
pub mod engines;
pub mod macros;
pub mod markdown;
pub mod numbering;
pub mod packages;
pub mod parser;
pub mod project;
pub mod renderer;
pub mod root;
pub mod sync;
pub mod theorems;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use ast::{Node, NodeKind, Pos, Role, Span};
pub use config::{
    discover_config_files, effective_page_margin_mm, load_and_merge as load_and_merge_config,
    Config, MarkdownBlockAppearance, MarkdownBlockReveal, PageMode, ResolvedConfig,
    ResolvedMarkdownBlock, ResolvedMarkdownBlockSyntax, ResolvedMarkdownConfig,
    ResolvedViewerConfig, SourceJumpTrigger, Theme,
};
pub use converter::{
    collect_builtin_dependencies, convert as convert_document, converter_for_path,
    finalize_builtin_document, split_render_output, AssetEncoding, AssetKind, BuiltinConversion,
    BuiltinConverter, ConversionDiagnostic, ConversionRequest, ConvertedAsset, ConvertedBlock,
    ConvertedDependency, ConvertedDocument, ConvertedMathRow, ConvertedMathRowsEntry,
    ConvertedSourceAnchor, ConvertedSourcePosition, ConvertedSubBlocks, ConvertedSubChunk,
    ConvertedSyncEntry, ConvertedSyncKind, ConvertedSyncMap, ConverterCapabilities,
    ConverterMetadata, DependencyKind, DiagnosticSeverity, DocumentConverter, LatexConverter,
    LegacyViewerSidecar, MarkdownConverter, MathMacroRequirement, MathRuntimeRequirements,
    RuntimeRequirements, ViewerRuntimeRequirements, CONVERTER_API_VERSION,
    MAX_ADDITIONAL_DEPENDENCIES, MAX_CONVERSION_OVERRIDE_BYTES, MAX_CONVERSION_OVERRIDE_FILES,
};
pub use engines::{Engine, MathEngine, MathJaxEngine};
pub use macros::{
    discover_macro_overrides, extract_preamble_from_overrides, resolve_override_path,
    validate_override_line, ExtractedMacro, ExtractedPreamble, MacroOverride, MacrosScope,
};
pub use packages::PackageMap;
pub use renderer::{HtmlOptions, RenderOutput, RenderedBlock};
pub use sync::SyncIndex;
pub use theorems::TheoremRegistry;

/// Source frontend selected for a preview document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentFormat {
    Latex,
    Markdown,
}

impl DocumentFormat {
    /// Infer a supported format from a path extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("tex" | "ltx") => Some(Self::Latex),
            Some("md" | "markdown") => Some(Self::Markdown),
            _ => None,
        }
    }

    /// Select the document frontend while preserving the historical LaTeX
    /// resolver contract: Markdown is opt-in by extension, and every other
    /// path (including explicitly named `\input` children such as `.inc`)
    /// remains a LaTeX project hint.
    pub fn detect(path: &Path) -> Result<Self> {
        Ok(match Self::from_path(path) {
            Some(Self::Markdown) => Self::Markdown,
            Some(Self::Latex) | None => Self::Latex,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Latex => "latex",
            Self::Markdown => "markdown",
        }
    }
}

/// Render either a LaTeX project or a standalone Markdown document, selected
/// from the input extension.
pub fn render_document(path: &Path, opts: &HtmlOptions) -> Result<RenderOutput> {
    match DocumentFormat::detect(path)? {
        DocumentFormat::Latex => render_project(path, opts),
        DocumentFormat::Markdown => {
            let root = std::fs::canonicalize(path)
                .with_context(|| format!("resolving Markdown file {}", path.display()))?;
            let source = std::fs::read_to_string(&root)
                .with_context(|| format!("reading Markdown file {}", root.display()))?;
            finish_markdown(root, source, opts)
        }
    }
}

/// Buffer-backed counterpart to [`render_document`]. Included LaTeX files are
/// still read from disk; Markdown is intentionally single-file.
pub fn render_document_from_source(
    root_hint: &Path,
    source: String,
    opts: &HtmlOptions,
) -> Result<RenderOutput> {
    match DocumentFormat::detect(root_hint)? {
        DocumentFormat::Latex => render_project_from_source(root_hint, source, opts),
        DocumentFormat::Markdown => finish_markdown(root_hint.to_path_buf(), source, opts),
    }
}

/// End-to-end Step-1 pipeline: given any `.tex` path, resolve the project
/// root, parse the project, extract preamble macros, and render to HTML.
pub fn render_project(path: &Path, opts: &HtmlOptions) -> Result<RenderOutput> {
    let root = root::resolve_root(path)?;
    let project = project::load_project(&root)?;
    finish_render(root, project, opts)
}

/// Same as [`render_project`], but the root file's content comes from
/// `source` (typically an editor buffer) instead of being read from disk.
/// Included / preamble / bib files are still read from disk.
pub fn render_project_from_source(
    root_hint: &Path,
    source: String,
    opts: &HtmlOptions,
) -> Result<RenderOutput> {
    // Skip the disk-based magic-comment / parent-walk root resolution. The
    // caller (typically the editor plugin) tells us which file owns this
    // buffer. If the buffer happens to be an included chapter, root
    // resolution is still useful — but only when the buffer matches disk;
    // in the live-edit case we trust the explicit hint.
    let project = project::load_project_from_source(root_hint, source)?;
    finish_render(root_hint.to_path_buf(), project, opts)
}

fn finish_render(
    root: PathBuf,
    project: project::Project,
    opts: &HtmlOptions,
) -> Result<RenderOutput> {
    // Load the explicit override files listed in `opts.macro_overrides`.
    // Missing files are skipped silently — the daemon's discovery helper
    // is allowed to point at "would-be" paths (e.g. `.mathpreview-macros.tex`
    // that doesn't exist yet) without failing the render.
    let overrides = load_macro_overrides(opts);
    let preamble = macros::extract_preamble_with_overrides(&project, &overrides)?;
    let (bib_paths, bib) =
        bibtex::discover_project_bib_with_overrides(&project, &std::collections::HashMap::new())?;
    let bib_style = bibtex::detect_project_bib_style(&project);
    // Theorem environments + their counters/titles are driven by the
    // preamble's `\newtheorem` declarations (including local `.sty` packages)
    // so numbering matches a real build.
    let mut thms = theorems::TheoremRegistry::from_project(&project);
    // Config override (`[viewer] theorem-numbering`): force continuous/section
    // numbering when the `\newtheorem` declarations aren't visible to detection.
    thms.apply_numbering_scheme(opts.viewer_config.theorem_numbering);
    let mut body = parser::parse_body_with_overrides(&project, &thms, &overrides)?;
    // mathtools `showonlyrefs`: collect every referenced key up front so the
    // numbering pass can suppress numbers on unreferenced equations. The scan
    // covers every body file (include order) and skips comments itself.
    let referenced = preamble.show_only_refs.then(|| {
        let mut keys = std::collections::HashSet::new();
        for f in &project.files {
            numbering::collect_referenced_keys(&f.source, &mut keys);
        }
        keys
    });
    let labels = numbering::assign_numbers_with_macros(
        &mut body,
        &bib,
        bib_style,
        &thms,
        referenced,
        &preamble.macros,
    );
    let mut sync = SyncIndex::new();
    // Inject the resolved root path so the topbar can show "title — path".
    // Done as a clone to avoid asking callers to mutate the &HtmlOptions
    // they passed in.
    let opts = {
        let mut owned = opts.clone();
        owned.document_format = DocumentFormat::Latex;
        if owned.source_path.is_none() {
            owned.source_path = Some(root.clone());
        }
        owned.latex_preamble = Some(project.preamble.source.clone());
        owned
    };
    let rendered = renderer::render(&body, &preamble, &labels, &bib, bib_style, &mut sync, &opts);
    let mut seen_files = std::collections::HashSet::new();
    let included_files = project
        .included_files()
        .map(PathBuf::from)
        .chain(bib_paths)
        .filter(|path| seen_files.insert(path.clone()))
        .collect();
    Ok(RenderOutput {
        html: rendered.full,
        body_html: rendered.body,
        blocks: rendered.blocks,
        sync,
        root_file: root,
        preamble,
        included_files,
        tikz_assets: rendered.tikz_assets,
        format: DocumentFormat::Latex,
    })
}

fn finish_markdown(root: PathBuf, source: String, opts: &HtmlOptions) -> Result<RenderOutput> {
    let overrides = load_macro_overrides(opts);
    let preamble = macros::extract_preamble_from_overrides(&overrides);
    let body = markdown::parse_with_config(&source, &root, &opts.markdown_config)?;
    let labels = numbering::LabelTable::default();
    let bib = std::collections::HashMap::new();
    let mut sync = SyncIndex::new();
    let opts = {
        let mut owned = opts.clone();
        owned.document_format = DocumentFormat::Markdown;
        if owned.source_path.is_none() {
            owned.source_path = Some(root.clone());
        }
        owned.latex_preamble = Some(String::new());
        owned
    };
    let rendered = renderer::render(
        &body,
        &preamble,
        &labels,
        &bib,
        bibtex::BibStyle::default(),
        &mut sync,
        &opts,
    );
    Ok(RenderOutput {
        html: rendered.full,
        body_html: rendered.body,
        blocks: rendered.blocks,
        sync,
        root_file: root,
        preamble,
        included_files: Vec::new(),
        tikz_assets: rendered.tikz_assets,
        format: DocumentFormat::Markdown,
    })
}

fn load_macro_overrides(opts: &HtmlOptions) -> Vec<MacroOverride> {
    opts.macro_overrides
        .iter()
        .filter_map(|path| {
            std::fs::read_to_string(path)
                .ok()
                .map(|source| MacroOverride {
                    label: path.clone(),
                    source,
                })
        })
        .collect()
}
