//! mathpreview core: parser, macro extractor, renderer.
//!
//! Public surface for Step 1 is intentionally small — orchestrate via
//! [`render_project`].

pub mod ast;
pub mod bibtex;
pub mod config;
pub mod engines;
pub mod macros;
pub mod numbering;
pub mod packages;
pub mod parser;
pub mod project;
pub mod renderer;
pub mod root;
pub mod sync;

use std::path::{Path, PathBuf};

use anyhow::Result;

pub use ast::{Node, NodeKind, Pos, Role, Span};
pub use config::{
    discover_config_files, load_and_merge as load_and_merge_config, Config, PageMode,
    ResolvedConfig, ResolvedViewerConfig, SourceJumpTrigger, Theme,
};
pub use engines::{Engine, MathEngine, MathJaxEngine};
pub use macros::{
    discover_macro_overrides, resolve_override_path, validate_override_line, ExtractedMacro,
    ExtractedPreamble, MacroOverride, MacrosScope,
};
pub use packages::PackageMap;
pub use renderer::{HtmlOptions, RenderOutput, RenderedBlock};
pub use sync::SyncIndex;

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
    let overrides: Vec<MacroOverride> = opts
        .macro_overrides
        .iter()
        .filter_map(|p| {
            std::fs::read_to_string(p).ok().map(|source| MacroOverride {
                label: p.clone(),
                source,
            })
        })
        .collect();
    let preamble = macros::extract_preamble_with_overrides(&project, &overrides)?;
    let bib = bibtex::load_project_bib(&project)?;
    let bib_style = bibtex::detect_project_bib_style(&project);
    let mut body = parser::parse_body(&project)?;
    let labels = numbering::assign_numbers(&mut body, &bib, bib_style);
    let mut sync = SyncIndex::new();
    // Inject the resolved root path so the topbar can show "title — path".
    // Done as a clone to avoid asking callers to mutate the &HtmlOptions
    // they passed in.
    let opts = if opts.source_path.is_none() {
        let mut owned = opts.clone();
        owned.source_path = Some(root.clone());
        owned
    } else {
        opts.clone()
    };
    let rendered = renderer::render(&body, &preamble, &labels, &bib, bib_style, &mut sync, &opts);
    Ok(RenderOutput {
        html: rendered.full,
        body_html: rendered.body,
        blocks: rendered.blocks,
        sync,
        root_file: root,
        preamble,
        included_files: project.included_files().map(PathBuf::from).collect(),
    })
}
