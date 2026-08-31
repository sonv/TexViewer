//! Versioned, source-language-neutral document conversion.
//!
//! A converter returns body-level HTML plus the metadata a viewer needs for
//! patching, source synchronization, dependencies, generated assets, and
//! runtime setup. It deliberately does not return the parser AST, a LaTeX
//! preamble object, or MathPreview's standalone page shell.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::bibtex::BibStyle;
use crate::engines::Engine;
use crate::{DocumentFormat, HtmlOptions};

/// Version of the in-process converter contract.
pub const CONVERTER_API_VERSION: u32 = 1;
/// Maximum number of editor buffers carried by one conversion request.
pub const MAX_CONVERSION_OVERRIDE_FILES: usize = 1024;
/// Maximum aggregate UTF-8 size of root and child editor buffers.
pub const MAX_CONVERSION_OVERRIDE_BYTES: usize = 256 * 1024 * 1024;
/// Maximum caller-supplied dependency records on one request.
pub const MAX_ADDITIONAL_DEPENDENCIES: usize = 2048;

/// Optional behavior advertised by a converter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(default)]
pub struct ConverterCapabilities {
    /// Accepts an unsaved root buffer in [`ConversionRequest::source`].
    pub buffer_source: bool,
    /// Accepts unsaved included, preamble, and bibliography files.
    pub multi_buffer_source: bool,
    /// Populates source anchors and the source-sync map.
    pub source_sync: bool,
    /// Returns position-stable top-level blocks suitable for live patching.
    pub block_patching: bool,
    /// Populates per-row locations for multi-row mathematics.
    pub math_row_sync: bool,
    /// Returns typed files that should trigger reconversion.
    pub dependency_tracking: bool,
    /// Can return generated asset source payloads.
    pub asset_payloads: bool,
}

/// Owned identity and capability declaration for a converter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConverterMetadata {
    pub api_version: u32,
    pub id: String,
    /// Open-ended source-format identifier such as `latex` or `typst`.
    pub format: String,
    /// Case-insensitive extensions without the leading dot.
    pub extensions: Vec<String>,
    pub capabilities: ConverterCapabilities,
}

impl ConverterMetadata {
    pub fn new(id: impl Into<String>, format: impl Into<String>) -> Self {
        Self {
            api_version: CONVERTER_API_VERSION,
            id: id.into(),
            format: format.into(),
            extensions: Vec::new(),
            capabilities: ConverterCapabilities::default(),
        }
    }

    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.extensions = extensions.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_capabilities(mut self, capabilities: ConverterCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Validate the stable converter identity before a host trusts its output.
    pub fn validate_contract(&self) -> Result<()> {
        if self.api_version != CONVERTER_API_VERSION {
            bail!(
                "converter {:?} uses API version {}; supported version is {}",
                self.id,
                self.api_version,
                CONVERTER_API_VERSION,
            );
        }
        if self.id.trim().is_empty() {
            bail!("converter id must not be empty");
        }
        if self.format.trim().is_empty() {
            bail!("converter format must not be empty");
        }
        Ok(())
    }

    pub fn supports_path(&self, path: &Path) -> bool {
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            return false;
        };
        self.extensions
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    }
}

/// Extensible dependency role serialized as a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DependencyKind(pub String);

impl DependencyKind {
    pub fn root() -> Self {
        Self("root".to_string())
    }

    pub fn include() -> Self {
        Self("include".to_string())
    }

    pub fn bibliography() -> Self {
        Self("bibliography".to_string())
    }

    pub fn config() -> Self {
        Self("config".to_string())
    }

    pub fn macro_file() -> Self {
        Self("macro".to_string())
    }

    pub fn custom(kind: impl Into<String>) -> Self {
        Self(kind.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One file that contributes to conversion and should normally be watched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConvertedDependency {
    pub path: PathBuf,
    pub kind: DependencyKind,
    pub exists: bool,
}

impl ConvertedDependency {
    pub fn new(path: impl Into<PathBuf>, kind: DependencyKind) -> Self {
        let path = path.into();
        Self {
            exists: path.exists(),
            path,
            kind,
        }
    }

    pub fn with_exists(mut self, exists: bool) -> Self {
        self.exists = exists;
        self
    }
}

/// Input to a document converter.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConversionRequest {
    /// Entry point or logical path for an unsaved root buffer.
    pub path: PathBuf,
    /// Unsaved root-buffer contents. This wins over a root entry in
    /// [`Self::file_overrides`].
    pub source: Option<String>,
    /// Unsaved project files keyed by path. Existing paths are canonicalized
    /// by the bundled converter before lookup.
    pub file_overrides: BTreeMap<PathBuf, String>,
    /// Caller-known inputs such as config files. Bundled converters merge
    /// these with dependencies discovered from the document.
    pub additional_dependencies: Vec<ConvertedDependency>,
}

impl ConversionRequest {
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            source: None,
            file_overrides: BTreeMap::new(),
            additional_dependencies: Vec::new(),
        }
    }

    pub fn from_source(root_hint: impl Into<PathBuf>, source: impl Into<String>) -> Self {
        Self {
            path: root_hint.into(),
            source: Some(source.into()),
            file_overrides: BTreeMap::new(),
            additional_dependencies: Vec::new(),
        }
    }

    pub fn with_file_override(
        mut self,
        path: impl Into<PathBuf>,
        source: impl Into<String>,
    ) -> Self {
        self.file_overrides.insert(path.into(), source.into());
        self
    }

    pub fn with_file_overrides(
        mut self,
        overrides: impl IntoIterator<Item = (PathBuf, String)>,
    ) -> Self {
        self.file_overrides.extend(overrides);
        self
    }

    pub fn with_dependency(mut self, dependency: ConvertedDependency) -> Self {
        self.additional_dependencies.push(dependency);
        self
    }

    pub fn is_buffer_backed(&self) -> bool {
        self.source.is_some() || !self.file_overrides.is_empty()
    }

    /// Enforce process-independent bounds before a bundled converter allocates
    /// project state. Custom implementations can apply stricter limits.
    pub fn validate(&self) -> Result<()> {
        self.validate_with_limits(
            MAX_CONVERSION_OVERRIDE_FILES,
            MAX_CONVERSION_OVERRIDE_BYTES,
            MAX_ADDITIONAL_DEPENDENCIES,
        )
    }

    fn validate_with_limits(
        &self,
        max_override_files: usize,
        max_override_bytes: usize,
        max_additional_dependencies: usize,
    ) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            bail!("conversion path must not be empty");
        }
        if self.file_overrides.len() > max_override_files {
            bail!(
                "conversion has {} file overrides; maximum is {}",
                self.file_overrides.len(),
                max_override_files
            );
        }
        if self.additional_dependencies.len() > max_additional_dependencies {
            bail!(
                "conversion has {} additional dependencies; maximum is {}",
                self.additional_dependencies.len(),
                max_additional_dependencies
            );
        }
        let total_bytes = self
            .file_overrides
            .values()
            .try_fold(
                self.source.as_ref().map_or(0usize, String::len),
                |total, source| total.checked_add(source.len()),
            )
            .context("conversion override byte count overflow")?;
        if total_bytes > max_override_bytes {
            bail!(
                "conversion has {total_bytes} bytes of buffer overrides; maximum is {max_override_bytes}"
            );
        }
        Ok(())
    }
}

/// One source anchor attached to rendered HTML.
pub type ConvertedSourceAnchor = crate::renderer::SourceAnchor;
/// One independently patchable child in a theorem/proof body.
pub type ConvertedSubChunk = crate::renderer::SubChunk;
/// Fine-grained patch metadata for a structured top-level block.
pub type ConvertedSubBlocks = crate::renderer::SubBody;

/// One ordered, position-stable viewer block. The bundled renderer produces
/// the neutral wire shape directly, so an in-process viewer can move the
/// complete block vector without an isomorphic remap or anchor allocation.
pub type ConvertedBlock = crate::renderer::RenderedBlock;

/// Stable source-position wire shape shared with the indexed sync runtime.
pub type ConvertedSourcePosition = crate::Pos;
/// Stable sync-role wire shape shared with the indexed sync runtime.
pub type ConvertedSyncKind = crate::sync::SyncKind;
/// Stable sync-entry wire shape shared with the indexed sync runtime.
pub type ConvertedSyncEntry = crate::sync::SyncEntry;
/// Stable math-row wire shape shared with the indexed sync runtime.
pub type ConvertedMathRow = crate::sync::MathRow;
/// Stable math-row-map wire shape shared with the indexed sync runtime.
pub type ConvertedMathRowsEntry = crate::sync::MathRowsEntry;

/// Public sync representation without `SyncIndex`'s lookup cache.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConvertedSyncMap {
    pub entries: Vec<ConvertedSyncEntry>,
    pub math_rows: Vec<ConvertedMathRowsEntry>,
    /// Preserved only for in-process handoff from the bundled renderer. The
    /// stable wire representation remains the two public vectors above;
    /// deserialized artifacts rebuild this cache once when indexed lookup is
    /// requested.
    #[serde(skip)]
    label_index: Option<HashMap<String, usize>>,
}

impl PartialEq for ConvertedSyncMap {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries && self.math_rows == other.math_rows
    }
}

impl Eq for ConvertedSyncMap {}

impl ConvertedSyncMap {
    pub fn from_parts(
        entries: Vec<ConvertedSyncEntry>,
        math_rows: Vec<ConvertedMathRowsEntry>,
    ) -> Self {
        Self {
            entries,
            math_rows,
            label_index: None,
        }
    }

    /// Move `SyncIndex` into the stable wire vectors while retaining its
    /// private lookup cache for a zero-rebuild in-process viewer handoff.
    pub fn from_sync_index(sync: crate::SyncIndex) -> Self {
        let (entries, math_rows, label_index) = sync.into_parts_with_label_index();
        Self {
            entries,
            math_rows,
            label_index: Some(label_index),
        }
    }

    /// Restore the indexed lookup representation while consuming the neutral
    /// vectors. In-process bundled conversions reuse the preserved cache;
    /// deserialized artifacts rebuild it once in O(n).
    pub fn into_sync_index(self) -> crate::SyncIndex {
        match self.label_index {
            Some(label_index) => crate::SyncIndex::from_parts_with_label_index(
                self.entries,
                self.math_rows,
                label_index,
            ),
            None => crate::SyncIndex::from_parts(self.entries, self.math_rows),
        }
    }
}

/// Extensible generated-asset kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetKind(pub String);

impl AssetKind {
    pub fn tikz_source() -> Self {
        Self("tikz-source".to_string())
    }

    pub fn custom(kind: impl Into<String>) -> Self {
        Self(kind.into())
    }
}

/// Extensible payload encoding identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetEncoding(pub String);

impl AssetEncoding {
    pub fn json() -> Self {
        Self("json".to_string())
    }

    pub fn utf8() -> Self {
        Self("utf-8".to_string())
    }

    pub fn base64() -> Self {
        Self("base64".to_string())
    }

    pub fn custom(encoding: impl Into<String>) -> Self {
        Self(encoding.into())
    }
}

/// Converter-owned payload from which a viewer or asset worker can produce a
/// runtime resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConvertedAsset {
    pub id: String,
    pub kind: AssetKind,
    /// Media type of `payload` itself.
    pub payload_media_type: String,
    /// Media type expected after processing, when different from the payload.
    pub intended_output_media_type: Option<String>,
    pub encoding: AssetEncoding,
    pub payload: serde_json::Value,
}

impl ConvertedAsset {
    pub fn new(
        id: impl Into<String>,
        kind: AssetKind,
        payload_media_type: impl Into<String>,
        encoding: AssetEncoding,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            payload_media_type: payload_media_type.into(),
            intended_output_media_type: None,
            encoding,
            payload,
        }
    }

    pub fn with_intended_output_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.intended_output_media_type = Some(media_type.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConversionDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
}

impl ConversionDiagnostic {
    pub fn new(
        severity: DiagnosticSeverity,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(DiagnosticSeverity::Warning, code, message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MathMacroRequirement {
    pub name: String,
    pub body: String,
    pub arguments: u8,
    pub default: Option<String>,
}

impl MathMacroRequirement {
    pub fn new(name: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            body: body.into(),
            arguments: 0,
            default: None,
        }
    }

    pub fn with_arguments(mut self, arguments: u8, default: Option<String>) -> Self {
        self.arguments = arguments;
        self.default = default;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MathRuntimeRequirements {
    pub engine: String,
    pub script_url: Option<String>,
    pub macros: Vec<MathMacroRequirement>,
    pub packages: Vec<String>,
    pub loader_packages: Vec<String>,
    /// Trusted user JavaScript; a host decides whether it may execute.
    pub config: String,
}

impl MathRuntimeRequirements {
    pub fn new(engine: impl Into<String>) -> Self {
        Self {
            engine: engine.into(),
            script_url: None,
            macros: Vec::new(),
            packages: Vec::new(),
            loader_packages: Vec::new(),
            config: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ViewerRuntimeRequirements {
    pub font_size: u32,
    pub ui_font_size: u32,
    pub hover_preview_scale: u32,
    pub default_page_mode: String,
    pub default_theme: String,
    pub source_jump_trigger: String,
    pub render_tikz: bool,
    pub theorem_numbering: String,
    pub fancy_theorems: bool,
    pub typeset_mode: String,
    pub page_margin_mm: Option<f64>,
    pub markdown_colon_fences: bool,
    pub keybindings: BTreeMap<String, Vec<String>>,
    pub keybinding_aliases: BTreeMap<String, String>,
    pub key_sequence_timeout_ms: u32,
}

impl Default for ViewerRuntimeRequirements {
    fn default() -> Self {
        let viewer = crate::ResolvedConfig::default().viewer;
        let page_margin_mm = crate::effective_page_margin_mm(&viewer, None);
        Self {
            font_size: viewer.font_size,
            ui_font_size: viewer.ui_font_size,
            hover_preview_scale: viewer.hover_preview_scale,
            default_page_mode: viewer.default_page_mode.as_str().to_string(),
            default_theme: viewer.default_theme.as_str().to_string(),
            source_jump_trigger: viewer.source_jump_trigger.as_str().to_string(),
            render_tikz: viewer.render_tikz,
            theorem_numbering: viewer.theorem_numbering.as_str().to_string(),
            fancy_theorems: viewer.fancy_theorems,
            typeset_mode: viewer.typeset_mode.as_str().to_string(),
            page_margin_mm,
            markdown_colon_fences: false,
            keybindings: viewer.keybindings,
            keybinding_aliases: viewer.keybinding_aliases,
            key_sequence_timeout_ms: viewer.key_sequence_timeout_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RuntimeRequirements {
    pub title: String,
    /// `None` means the converted HTML has no math-runtime dependency.
    pub math: Option<MathRuntimeRequirements>,
    pub viewer: ViewerRuntimeRequirements,
}

impl RuntimeRequirements {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            math: None,
            viewer: ViewerRuntimeRequirements::default(),
        }
    }

    pub fn with_math(mut self, math: MathRuntimeRequirements) -> Self {
        self.math = Some(math);
        self
    }

    pub fn with_viewer(mut self, viewer: ViewerRuntimeRequirements) -> Self {
        self.viewer = viewer;
        self
    }
}

/// Source-language-neutral document consumed by a viewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConvertedDocument {
    pub converter: ConverterMetadata,
    pub root_file: PathBuf,
    pub body_html: String,
    pub blocks: Vec<ConvertedBlock>,
    pub sync: ConvertedSyncMap,
    pub dependencies: Vec<ConvertedDependency>,
    pub assets: Vec<ConvertedAsset>,
    pub diagnostics: Vec<ConversionDiagnostic>,
    pub runtime: RuntimeRequirements,
}

impl ConvertedDocument {
    pub fn new(
        converter: ConverterMetadata,
        root_file: impl Into<PathBuf>,
        body_html: impl Into<String>,
        runtime: RuntimeRequirements,
    ) -> Self {
        Self {
            converter,
            root_file: root_file.into(),
            body_html: body_html.into(),
            blocks: Vec::new(),
            sync: ConvertedSyncMap::default(),
            dependencies: Vec::new(),
            assets: Vec::new(),
            diagnostics: Vec::new(),
            runtime,
        }
    }

    pub fn with_blocks(mut self, blocks: impl IntoIterator<Item = ConvertedBlock>) -> Self {
        self.blocks = blocks.into_iter().collect();
        self
    }

    pub fn with_sync(mut self, sync: ConvertedSyncMap) -> Self {
        self.sync = sync;
        self
    }

    pub fn with_dependencies(
        mut self,
        dependencies: impl IntoIterator<Item = ConvertedDependency>,
    ) -> Self {
        self.dependencies = dependencies.into_iter().collect();
        self
    }

    pub fn with_assets(mut self, assets: impl IntoIterator<Item = ConvertedAsset>) -> Self {
        self.assets = assets.into_iter().collect();
        self
    }

    pub fn with_diagnostics(
        mut self,
        diagnostics: impl IntoIterator<Item = ConversionDiagnostic>,
    ) -> Self {
        self.diagnostics = diagnostics.into_iter().collect();
        self
    }

    /// Validate the converter contract carried by this artifact.
    pub fn validate_contract(&self) -> Result<()> {
        self.converter.validate_contract()
    }

    /// Validate an artifact against the converter registration selected by a
    /// host. This rejects a converter that changes identity, format, or
    /// capabilities between discovery and conversion.
    pub fn validate_against(&self, expected: &ConverterMetadata) -> Result<()> {
        self.validate_contract()?;
        expected.validate_contract()?;
        if &self.converter != expected {
            bail!(
                "converter metadata changed between selection and conversion: expected {expected:?}, got {:?}",
                self.converter,
            );
        }
        Ok(())
    }
}

/// Bundled-converter result for hosts that also serve MathPreview's browser
/// shell. The public converter contract remains [`ConvertedDocument`]; these
/// built-in-only details let a cache-aware host construct the shell without a
/// second parse or render pass.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct BuiltinConversion {
    pub document: ConvertedDocument,
    pub preamble: crate::ExtractedPreamble,
    pub options: HtmlOptions,
}

/// Viewer-only state retained when an existing full-shell render is projected
/// into the neutral artifact. This is intentionally separate from
/// [`ConvertedDocument`] so cross-language consumers never depend on LaTeX or
/// MathPreview's embedded page shell.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct LegacyViewerSidecar {
    pub html: String,
    pub format: DocumentFormat,
    pub preamble: crate::ExtractedPreamble,
}

/// Source-language frontend consumed by the viewer.
pub trait DocumentConverter: fmt::Debug + Send + Sync {
    /// Owned metadata permits converters discovered from runtime config.
    fn metadata(&self) -> ConverterMetadata;

    fn supports_path(&self, path: &Path) -> bool {
        self.metadata().supports_path(path)
    }

    fn convert(&self, request: ConversionRequest, opts: &HtmlOptions) -> Result<ConvertedDocument>;
}

fn latex_metadata() -> ConverterMetadata {
    ConverterMetadata {
        api_version: CONVERTER_API_VERSION,
        id: "latex".to_string(),
        format: "latex".to_string(),
        extensions: vec!["tex".to_string(), "ltx".to_string()],
        capabilities: ConverterCapabilities {
            buffer_source: true,
            multi_buffer_source: true,
            source_sync: true,
            block_patching: true,
            math_row_sync: true,
            dependency_tracking: true,
            asset_payloads: true,
        },
    }
}

fn markdown_metadata() -> ConverterMetadata {
    ConverterMetadata {
        api_version: CONVERTER_API_VERSION,
        id: "markdown".to_string(),
        format: "markdown".to_string(),
        extensions: vec!["md".to_string(), "markdown".to_string()],
        capabilities: ConverterCapabilities {
            buffer_source: true,
            multi_buffer_source: false,
            source_sync: true,
            block_patching: true,
            math_row_sync: true,
            dependency_tracking: true,
            asset_payloads: false,
        },
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LatexConverter;

impl DocumentConverter for LatexConverter {
    fn metadata(&self) -> ConverterMetadata {
        latex_metadata()
    }

    fn convert(&self, request: ConversionRequest, opts: &HtmlOptions) -> Result<ConvertedDocument> {
        request.validate()?;
        let metadata = self.metadata();
        let document = convert_latex(request, opts, metadata.clone())?.document;
        document.validate_against(&metadata)?;
        Ok(document)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownConverter;

impl DocumentConverter for MarkdownConverter {
    fn metadata(&self) -> ConverterMetadata {
        markdown_metadata()
    }

    fn convert(&self, request: ConversionRequest, opts: &HtmlOptions) -> Result<ConvertedDocument> {
        request.validate()?;
        let metadata = self.metadata();
        let document = convert_markdown(request, opts, metadata.clone())?.document;
        document.validate_against(&metadata)?;
        Ok(document)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinConverter {
    Latex,
    Markdown,
}

impl BuiltinConverter {
    pub fn for_path(path: &Path) -> Self {
        match DocumentFormat::from_path(path) {
            Some(DocumentFormat::Markdown) => Self::Markdown,
            Some(DocumentFormat::Latex) | None => Self::Latex,
        }
    }

    pub fn metadata(self) -> ConverterMetadata {
        self.as_converter().metadata()
    }

    pub fn as_converter(self) -> &'static dyn DocumentConverter {
        static LATEX: LatexConverter = LatexConverter;
        static MARKDOWN: MarkdownConverter = MarkdownConverter;
        match self {
            Self::Latex => &LATEX,
            Self::Markdown => &MARKDOWN,
        }
    }

    pub fn convert(
        self,
        request: ConversionRequest,
        opts: &HtmlOptions,
    ) -> Result<ConvertedDocument> {
        self.as_converter().convert(request, opts)
    }

    /// Run a bundled converter while retaining only the private data needed
    /// to construct MathPreview's own viewer shell. External converters and
    /// cross-process consumers use [`DocumentConverter::convert`] instead.
    #[doc(hidden)]
    pub fn convert_for_viewer(
        self,
        request: ConversionRequest,
        opts: &HtmlOptions,
    ) -> Result<BuiltinConversion> {
        request.validate()?;
        let expected = self.metadata();
        let conversion = match self {
            Self::Latex => convert_latex(request, opts, self.metadata()),
            Self::Markdown => convert_markdown(request, opts, self.metadata()),
        }?;
        conversion.document.validate_against(&expected)?;
        Ok(conversion)
    }
}

pub fn converter_for_path(path: &Path) -> BuiltinConverter {
    BuiltinConverter::for_path(path)
}

pub fn convert(request: ConversionRequest, opts: &HtmlOptions) -> Result<ConvertedDocument> {
    converter_for_path(&request.path).convert(request, opts)
}

/// Project an already-rendered legacy result into the neutral viewer artifact
/// without parsing or rendering a second time. The returned sidecar retains
/// only state required by MathPreview's current full-shell server.
#[doc(hidden)]
pub fn split_render_output(
    output: crate::RenderOutput,
    opts: &HtmlOptions,
    metadata: ConverterMetadata,
    additional_dependencies: Vec<ConvertedDependency>,
) -> (ConvertedDocument, LegacyViewerSidecar) {
    let crate::RenderOutput {
        html,
        body_html,
        blocks,
        sync,
        root_file,
        preamble,
        included_files,
        tikz_assets,
        format,
    } = output;
    let mut project_files = Vec::new();
    let mut bib_paths = Vec::new();
    for path in included_files {
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bib"))
        {
            bib_paths.push(path);
        } else {
            project_files.push(path);
        }
    }
    let dependencies = collect_dependencies(
        &root_file,
        &project_files,
        &bib_paths,
        &opts.macro_overrides,
        additional_dependencies,
    );
    let mut projection_opts = opts.clone();
    projection_opts.document_format = format;
    let converted = finalize_builtin_document(
        metadata,
        root_file,
        crate::renderer::RenderedBody {
            body: body_html,
            blocks,
            tikz_assets,
        },
        sync,
        &preamble,
        dependencies,
        Vec::new(),
        &projection_opts,
    );
    let sidecar = LegacyViewerSidecar {
        html,
        format,
        preamble,
    };
    (converted, sidecar)
}

fn convert_latex(
    request: ConversionRequest,
    opts: &HtmlOptions,
    metadata: ConverterMetadata,
) -> Result<BuiltinConversion> {
    let ConversionRequest {
        path,
        source,
        file_overrides,
        additional_dependencies,
    } = request;
    let has_root_override = source.is_some() || override_matches_path(&file_overrides, &path);
    let root = if has_root_override {
        path
    } else {
        crate::root::resolve_root(&path)?
    };
    let mut source_overrides = normalized_overrides(file_overrides);
    if let Some(source) = source {
        source_overrides.insert(normalize_existing_path(&root), source);
    }
    let project = if source_overrides.is_empty() {
        crate::project::load_project(&root)?
    } else {
        crate::project::load_project_with_overrides(&root, &source_overrides)?
    };
    let project_warnings = project.warnings.clone();
    let project_files: Vec<PathBuf> = project.included_files().map(PathBuf::from).collect();
    let macro_overrides = crate::load_macro_overrides(opts);
    let preamble = crate::macros::extract_preamble_with_overrides(&project, &macro_overrides)?;
    let (bib_paths, bib) =
        crate::bibtex::discover_project_bib_with_overrides(&project, &source_overrides)?;
    let bib_style = crate::bibtex::detect_project_bib_style(&project);
    let mut theorems = crate::theorems::TheoremRegistry::from_project(&project);
    theorems.apply_numbering_scheme(opts.viewer_config.theorem_numbering);
    let mut body = crate::parser::parse_body_with_overrides(&project, &theorems, &macro_overrides)?;
    let referenced = preamble.show_only_refs.then(|| {
        let mut keys = HashSet::new();
        for file in &project.files {
            crate::numbering::collect_referenced_keys(&file.source, &mut keys);
        }
        keys
    });
    let labels = crate::numbering::assign_numbers_with_macros(
        &mut body,
        &bib,
        bib_style,
        &theorems,
        referenced,
        &preamble.macros,
    );
    let mut sync = crate::SyncIndex::new();
    let render_opts = {
        let mut owned = opts.clone();
        owned.document_format = DocumentFormat::Latex;
        if owned.source_path.is_none() {
            owned.source_path = Some(root.clone());
        }
        owned.latex_preamble = Some(project.preamble.source.clone());
        owned
    };
    let rendered = crate::renderer::render_body_only(
        &body,
        &preamble,
        &labels,
        &bib,
        bib_style,
        &mut sync,
        &render_opts,
    );
    let dependencies = collect_dependencies(
        &root,
        &project_files,
        &bib_paths,
        &render_opts.macro_overrides,
        additional_dependencies,
    );
    let diagnostics = project_warnings
        .into_iter()
        .map(|message| diagnostic("project-warning", message))
        .collect::<Vec<_>>();
    let document = finalize_builtin_document(
        metadata,
        root,
        rendered,
        sync,
        &preamble,
        dependencies,
        diagnostics,
        &render_opts,
    );
    Ok(BuiltinConversion {
        document,
        preamble,
        options: render_opts,
    })
}

fn convert_markdown(
    request: ConversionRequest,
    opts: &HtmlOptions,
    metadata: ConverterMetadata,
) -> Result<BuiltinConversion> {
    let ConversionRequest {
        path,
        source,
        mut file_overrides,
        additional_dependencies,
    } = request;
    let root_override = take_override_for_path(&mut file_overrides, &path);
    let source = source.or(root_override);
    if !file_overrides.is_empty() {
        bail!("the Markdown converter does not support non-root file overrides");
    }
    let (root, source) = match source {
        Some(source) => (path, source),
        None => {
            let root = std::fs::canonicalize(&path)
                .with_context(|| format!("resolving Markdown file {}", path.display()))?;
            let source = std::fs::read_to_string(&root)
                .with_context(|| format!("reading Markdown file {}", root.display()))?;
            (root, source)
        }
    };
    let macro_overrides = crate::load_macro_overrides(opts);
    let preamble = crate::macros::extract_preamble_from_overrides(&macro_overrides);
    let body = crate::markdown::parse_with_config(&source, &root, &opts.markdown_config)?;
    let labels = crate::numbering::LabelTable::default();
    let bib = HashMap::new();
    let mut sync = crate::SyncIndex::new();
    let render_opts = {
        let mut owned = opts.clone();
        owned.document_format = DocumentFormat::Markdown;
        if owned.source_path.is_none() {
            owned.source_path = Some(root.clone());
        }
        owned.latex_preamble = Some(String::new());
        owned
    };
    let rendered = crate::renderer::render_body_only(
        &body,
        &preamble,
        &labels,
        &bib,
        BibStyle::default(),
        &mut sync,
        &render_opts,
    );
    let dependencies = collect_dependencies(
        &root,
        &[],
        &[],
        &render_opts.macro_overrides,
        additional_dependencies,
    );
    let document = finalize_builtin_document(
        metadata,
        root,
        rendered,
        sync,
        &preamble,
        dependencies,
        Vec::new(),
        &render_opts,
    );
    Ok(BuiltinConversion {
        document,
        preamble,
        options: render_opts,
    })
}

/// Finalize body-level output from one of the bundled frontends into the same
/// neutral artifact returned by [`DocumentConverter::convert`]. This is the
/// cache-aware host boundary: parsing, preamble extraction, bibliography
/// loading, and body rendering may stay cached in the host, while the viewer
/// consumes only [`ConvertedDocument`].
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn finalize_builtin_document(
    metadata: ConverterMetadata,
    root_file: PathBuf,
    rendered: crate::renderer::RenderedBody,
    sync: crate::SyncIndex,
    preamble: &crate::ExtractedPreamble,
    dependencies: Vec<ConvertedDependency>,
    mut diagnostics: Vec<ConversionDiagnostic>,
    opts: &HtmlOptions,
) -> ConvertedDocument {
    diagnostics.extend(preamble_diagnostics(preamble));
    let runtime = runtime_requirements(preamble, opts);
    let blocks = rendered.blocks;
    let sync = ConvertedSyncMap::from_sync_index(sync);
    let mut assets = rendered
        .tikz_assets
        .into_iter()
        .map(|(id, asset)| ConvertedAsset {
            id,
            kind: AssetKind::tikz_source(),
            payload_media_type: "application/vnd.mathpreview.tikz-source+json".to_string(),
            intended_output_media_type: Some("image/svg+xml".to_string()),
            encoding: AssetEncoding::json(),
            payload: serde_json::json!({
                "environment": asset.environment,
                "body": asset.body,
                "preamble": asset.preamble,
            }),
        })
        .collect::<Vec<_>>();
    assets.sort_by(|left, right| left.id.cmp(&right.id));
    ConvertedDocument {
        converter: metadata,
        root_file,
        body_html: rendered.body,
        blocks,
        sync,
        dependencies,
        assets,
        diagnostics,
        runtime,
    }
}

fn runtime_requirements(
    preamble: &crate::ExtractedPreamble,
    opts: &HtmlOptions,
) -> RuntimeRequirements {
    let title = preamble
        .title_short
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(&opts.title)
        .to_string();
    let (engine, script_url) = match &opts.engine {
        Engine::MathJax(engine) => ("mathjax".to_string(), Some(engine.script_url.clone())),
    };
    let packages = effective_math_packages(&preamble.packages_short, "noerrors", "ams");
    let loader_packages =
        effective_math_packages(&preamble.packages_long, "[tex]/noerrors", "[tex]/ams");
    RuntimeRequirements {
        title,
        math: Some(MathRuntimeRequirements {
            engine,
            script_url,
            macros: preamble
                .macros
                .iter()
                .map(|definition| MathMacroRequirement {
                    name: definition.name.clone(),
                    body: definition.body.clone(),
                    arguments: definition.n_args,
                    default: definition.default.clone(),
                })
                .collect(),
            packages,
            loader_packages,
            config: opts.viewer_config.mathjax_config.clone(),
        }),
        viewer: ViewerRuntimeRequirements {
            font_size: opts.viewer_config.font_size,
            ui_font_size: opts.viewer_config.ui_font_size,
            hover_preview_scale: opts.viewer_config.hover_preview_scale,
            default_page_mode: opts.viewer_config.default_page_mode.as_str().to_string(),
            default_theme: opts.viewer_config.default_theme.as_str().to_string(),
            source_jump_trigger: opts.viewer_config.source_jump_trigger.as_str().to_string(),
            render_tikz: opts.viewer_config.render_tikz,
            theorem_numbering: opts.viewer_config.theorem_numbering.as_str().to_string(),
            fancy_theorems: opts.viewer_config.fancy_theorems,
            typeset_mode: opts.viewer_config.typeset_mode.as_str().to_string(),
            page_margin_mm: crate::effective_page_margin_mm(
                &opts.viewer_config,
                preamble.geometry_margin_mm,
            ),
            markdown_colon_fences: opts.document_format == DocumentFormat::Latex
                || opts.markdown_config.colon_fences,
            keybindings: opts.viewer_config.keybindings.clone(),
            keybinding_aliases: opts.viewer_config.keybinding_aliases.clone(),
            key_sequence_timeout_ms: opts.viewer_config.key_sequence_timeout_ms,
        },
    }
}

fn effective_math_packages(packages: &[String], first: &str, second: &str) -> Vec<String> {
    let mut effective = vec![first.to_string(), second.to_string()];
    for package in packages {
        if !effective.contains(package) {
            effective.push(package.clone());
        }
    }
    effective
}

fn preamble_diagnostics(preamble: &crate::ExtractedPreamble) -> Vec<ConversionDiagnostic> {
    let mut diagnostics = preamble
        .warnings
        .iter()
        .map(|message| diagnostic("preamble-warning", message.clone()))
        .collect::<Vec<_>>();
    diagnostics.extend(preamble.unmapped_packages.iter().map(|package| {
        diagnostic(
            "unmapped-latex-package",
            format!("LaTeX package {package:?} has no MathJax mapping"),
        )
    }));
    diagnostics
}

fn diagnostic(code: &str, message: String) -> ConversionDiagnostic {
    ConversionDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: code.to_string(),
        message,
    }
}

fn collect_dependencies(
    root: &Path,
    project_files: &[PathBuf],
    bib_paths: &[PathBuf],
    macro_files: &[PathBuf],
    additional: Vec<ConvertedDependency>,
) -> Vec<ConvertedDependency> {
    let mut dependencies = Vec::new();
    let mut seen = HashSet::new();
    push_dependency(
        &mut dependencies,
        &mut seen,
        ConvertedDependency::new(root, DependencyKind::root()),
    );
    for path in project_files {
        if same_path(path, root) {
            continue;
        }
        push_dependency(
            &mut dependencies,
            &mut seen,
            ConvertedDependency::new(path, DependencyKind::include()),
        );
    }
    for path in bib_paths {
        push_dependency(
            &mut dependencies,
            &mut seen,
            ConvertedDependency::new(path, DependencyKind::bibliography()),
        );
    }
    for path in macro_files {
        push_dependency(
            &mut dependencies,
            &mut seen,
            ConvertedDependency::new(path, DependencyKind::macro_file()),
        );
    }
    for dependency in additional {
        push_dependency(&mut dependencies, &mut seen, dependency);
    }
    dependencies
}

/// Collect and de-duplicate the dependency records emitted by the bundled
/// converters. Cache-aware hosts use this before
/// [`finalize_builtin_document`] so watched-file behavior stays identical to
/// an ordinary converter invocation.
#[doc(hidden)]
pub fn collect_builtin_dependencies(
    root: &Path,
    project_files: &[PathBuf],
    bib_paths: &[PathBuf],
    macro_files: &[PathBuf],
    additional: Vec<ConvertedDependency>,
) -> Vec<ConvertedDependency> {
    collect_dependencies(root, project_files, bib_paths, macro_files, additional)
}

fn push_dependency(
    dependencies: &mut Vec<ConvertedDependency>,
    seen: &mut HashSet<(PathBuf, DependencyKind)>,
    dependency: ConvertedDependency,
) {
    let key = (
        crate::project::override_key(&dependency.path),
        dependency.kind.clone(),
    );
    if seen.insert(key) {
        dependencies.push(dependency);
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    left == right || crate::project::override_key(left) == crate::project::override_key(right)
}

fn normalized_overrides(overrides: BTreeMap<PathBuf, String>) -> HashMap<PathBuf, String> {
    overrides
        .into_iter()
        .map(|(path, source)| (normalize_existing_path(&path), source))
        .collect()
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    crate::project::override_key(path)
}

fn override_matches_path(overrides: &BTreeMap<PathBuf, String>, path: &Path) -> bool {
    let target = crate::project::override_key(path);
    overrides
        .keys()
        .any(|candidate| crate::project::override_key(candidate) == target)
}

fn take_override_for_path(
    overrides: &mut BTreeMap<PathBuf, String>,
    path: &Path,
) -> Option<String> {
    if let Some(source) = overrides.remove(path) {
        return Some(source);
    }
    let target = crate::project::override_key(path);
    let key = overrides
        .keys()
        .find(|candidate| crate::project::override_key(candidate) == target)
        .cloned()?;
    overrides.remove(&key)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mathpreview-neutral-converter-{name}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn assert_neutral_matches_legacy(converted: &ConvertedDocument, legacy: &crate::RenderOutput) {
        assert_eq!(converted.converter.format, legacy.format.as_str());
        assert_eq!(converted.root_file, legacy.root_file);
        assert_eq!(converted.body_html, legacy.body_html);
        assert!(
            !legacy.html.is_empty(),
            "legacy output must retain its shell"
        );
        assert_eq!(converted.blocks.len(), legacy.blocks.len());
        for (converted, legacy) in converted.blocks.iter().zip(&legacy.blocks) {
            assert_eq!(converted.id, legacy.id);
            assert_eq!(converted.hash, legacy.hash);
            assert_eq!(converted.diff_hash, legacy.diff_hash);
            assert_eq!(converted.src, legacy.src);
            assert_eq!(converted.html, legacy.html);
            assert_eq!(converted.source_anchors.len(), legacy.source_anchors.len());
            for (converted, legacy) in converted.source_anchors.iter().zip(&legacy.source_anchors) {
                assert_eq!(converted.id, legacy.id);
                assert_eq!(converted.src, legacy.src);
            }
            match (&converted.sub_blocks, &legacy.sub_blocks) {
                (None, None) => {}
                (Some(converted), Some(legacy)) => {
                    assert_eq!(converted.prefix_diff, legacy.prefix_diff);
                    assert_eq!(converted.suffix_diff, legacy.suffix_diff);
                    assert_eq!(converted.children.len(), legacy.children.len());
                    for (converted, legacy) in converted.children.iter().zip(&legacy.children) {
                        assert_eq!(converted.diff_hash, legacy.diff_hash);
                        assert_eq!(converted.html, legacy.html);
                    }
                }
                pair => panic!("sub-block mismatch: {pair:?}"),
            }
        }
        assert_eq!(converted.sync.entries.len(), legacy.sync.entries.len());
        for (converted, legacy) in converted.sync.entries.iter().zip(&legacy.sync.entries) {
            assert_eq!(converted.element_id, legacy.element_id);
            assert_eq!(converted.file, legacy.file);
            assert_eq!(converted.start.line, legacy.start.line);
            assert_eq!(converted.start.col, legacy.start.col);
            assert_eq!(converted.start.byte, legacy.start.byte);
            assert_eq!(converted.end.line, legacy.end.line);
            assert_eq!(converted.end.col, legacy.end.col);
            assert_eq!(converted.end.byte, legacy.end.byte);
            assert_eq!(converted.label, legacy.label);
            let kind = match legacy.kind {
                crate::sync::SyncKind::Leaf => ConvertedSyncKind::Leaf,
                crate::sync::SyncKind::Container => ConvertedSyncKind::Container,
                crate::sync::SyncKind::Block => ConvertedSyncKind::Block,
            };
            assert_eq!(converted.kind, kind);
        }
        assert_eq!(converted.sync.math_rows.len(), legacy.sync.math_rows.len());
        for (converted, legacy) in converted.sync.math_rows.iter().zip(&legacy.sync.math_rows) {
            assert_eq!(converted.element_id, legacy.element_id);
            assert_eq!(converted.file, legacy.file);
            assert_eq!(converted.rows.len(), legacy.rows.len());
            for (converted, legacy) in converted.rows.iter().zip(&legacy.rows) {
                assert_eq!(converted.start_line, legacy.start_line);
                assert_eq!(converted.end_line, legacy.end_line);
                assert_eq!(converted.start_col, legacy.start_col);
            }
        }
        assert_eq!(converted.assets.len(), legacy.tikz_assets.len());
        for asset in &converted.assets {
            let legacy = legacy.tikz_assets.get(&asset.id).unwrap();
            assert_eq!(asset.kind, AssetKind::tikz_source());
            assert_eq!(asset.encoding, AssetEncoding::json());
            assert_eq!(
                asset.payload_media_type,
                "application/vnd.mathpreview.tikz-source+json"
            );
            assert_eq!(
                asset.intended_output_media_type.as_deref(),
                Some("image/svg+xml")
            );
            assert_eq!(asset.payload["environment"], legacy.environment);
            assert_eq!(asset.payload["body"], legacy.body);
            assert_eq!(asset.payload["preamble"], legacy.preamble);
        }
    }

    #[test]
    fn owned_metadata_and_object_safe_custom_converter_are_format_neutral() {
        #[derive(Debug)]
        struct CustomConverter {
            metadata: ConverterMetadata,
        }

        impl DocumentConverter for CustomConverter {
            fn metadata(&self) -> ConverterMetadata {
                self.metadata.clone()
            }

            fn convert(
                &self,
                request: ConversionRequest,
                opts: &HtmlOptions,
            ) -> Result<ConvertedDocument> {
                let metadata = self.metadata();
                Ok(ConvertedDocument {
                    converter: metadata,
                    root_file: request.path,
                    body_html: "<article>custom</article>".to_string(),
                    blocks: Vec::new(),
                    sync: ConvertedSyncMap::default(),
                    dependencies: Vec::new(),
                    assets: Vec::new(),
                    diagnostics: Vec::new(),
                    runtime: RuntimeRequirements {
                        title: opts.title.clone(),
                        math: None,
                        viewer: runtime_requirements(
                            &crate::macros::extract_preamble_from_overrides(&[]),
                            opts,
                        )
                        .viewer,
                    },
                })
            }
        }

        let runtime_format = format!("runtime-format-{}", std::process::id());
        let converter: Box<dyn DocumentConverter> = Box::new(CustomConverter {
            metadata: ConverterMetadata {
                api_version: CONVERTER_API_VERSION,
                id: format!("runtime-converter-{}", std::process::id()),
                format: runtime_format.clone(),
                extensions: vec!["runtime".to_string()],
                capabilities: ConverterCapabilities::default(),
            },
        });
        let output = converter
            .convert(
                ConversionRequest::from_path("notes.runtime"),
                &HtmlOptions::default(),
            )
            .unwrap();
        assert_eq!(output.converter.format, runtime_format);
        assert!(output.runtime.math.is_none());
        assert_eq!(output.body_html, "<article>custom</article>");
    }

    #[test]
    fn converter_contract_rejects_incompatible_or_changed_metadata() {
        let converter = MarkdownConverter;
        let expected = converter.metadata();
        let mut document = converter
            .convert(
                ConversionRequest::from_source("notes.md", "hello"),
                &HtmlOptions::default(),
            )
            .unwrap();
        document.validate_against(&expected).unwrap();

        document.converter.api_version += 1;
        assert!(document.validate_contract().is_err());
        document.converter.api_version = CONVERTER_API_VERSION;
        document.converter.id.clear();
        assert!(document.validate_contract().is_err());
        document.converter.id = expected.id.clone();
        document.converter.format.clear();
        assert!(document.validate_contract().is_err());
        document.converter.format = expected.format.clone();
        document.converter.capabilities.block_patching = false;
        assert!(document.validate_against(&expected).is_err());
    }

    #[test]
    fn markdown_converter_advertises_and_serializes_math_rows() {
        let source = concat!(
            "$$\n",
            "\\begin{aligned}\n",
            "a &= b \\\\\n",
            "  c &= d\n",
            "\\end{aligned}\n",
            "$$\n",
        );
        let converted = MarkdownConverter
            .convert(
                ConversionRequest::from_source("notes.md", source),
                &HtmlOptions::default(),
            )
            .unwrap();

        assert!(converted.converter.capabilities.math_row_sync);
        assert_eq!(converted.sync.math_rows.len(), 1);
        assert_eq!(converted.sync.math_rows[0].rows.len(), 2);
        assert_eq!(converted.sync.math_rows[0].rows[0].start_line, 3);
        assert_eq!(converted.sync.math_rows[0].rows[0].start_col, 1);
        assert_eq!(converted.sync.math_rows[0].rows[1].start_line, 4);
        assert_eq!(converted.sync.math_rows[0].rows[1].start_col, 3);

        let json = serde_json::to_value(&converted).unwrap();
        assert_eq!(
            json["converter"]["capabilities"]["math_row_sync"],
            serde_json::json!(true)
        );
        assert_eq!(json["sync"]["math_rows"][0]["rows"][1]["start_line"], 4);
    }

    #[test]
    fn source_and_disk_converters_match_legacy_body_sync_and_patch_state() {
        let mut opts = HtmlOptions::default();
        opts.viewer_config.render_tikz = true;
        opts.tikz_asset_base = Some("/tikz/".to_string());
        let latex = r#"\documentclass{article}
\newtheorem{theorem}{Theorem}
\begin{document}
\begin{theorem}For $x>0$, $x^2>0$.\end{theorem}
\begin{tikzpicture}\draw (0,0)--(1,1);\end{tikzpicture}
\end{document}"#;
        let converted = LatexConverter
            .convert(ConversionRequest::from_source("paper.tex", latex), &opts)
            .unwrap();
        let legacy =
            crate::render_project_from_source(Path::new("paper.tex"), latex.to_string(), &opts)
                .unwrap();
        assert_neutral_matches_legacy(&converted, &legacy);

        let markdown = "# Parity\n\nA formula $x^2$ and **strong text**.\n";
        let converted = MarkdownConverter
            .convert(ConversionRequest::from_source("notes.md", markdown), &opts)
            .unwrap();
        let legacy =
            crate::finish_markdown(PathBuf::from("notes.md"), markdown.to_string(), &opts).unwrap();
        assert_neutral_matches_legacy(&converted, &legacy);

        let dir = temp_dir("disk-parity");
        let tex = dir.join("paper.tex");
        let md = dir.join("notes.md");
        std::fs::write(
            &tex,
            "\\documentclass{article}\n\\begin{document}\nDisk $x$.\n\\end{document}\n",
        )
        .unwrap();
        std::fs::write(&md, "# Disk\n\nMarkdown $y$.\n").unwrap();
        let converted = LatexConverter
            .convert(ConversionRequest::from_path(&tex), &opts)
            .unwrap();
        let legacy = crate::render_project(&tex, &opts).unwrap();
        assert_neutral_matches_legacy(&converted, &legacy);
        let converted = MarkdownConverter
            .convert(ConversionRequest::from_path(&md), &opts)
            .unwrap();
        let legacy = crate::render_document(&md, &opts).unwrap();
        assert_neutral_matches_legacy(&converted, &legacy);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn split_legacy_output_projects_without_rerendering_or_losing_sidecar_state() {
        let source = concat!(
            "\\documentclass{article}\n",
            "\\newcommand{\\RR}{\\mathbb{R}}\n",
            "\\begin{document}A formula $\\RR$.\\end{document}\n",
        );
        let opts = HtmlOptions::default();
        let legacy =
            crate::render_project_from_source(Path::new("paper.tex"), source.to_string(), &opts)
                .unwrap();
        let expected = legacy.clone();
        let extra =
            ConvertedDependency::new("config.toml", DependencyKind::config()).with_exists(false);
        let (converted, sidecar) = split_render_output(
            legacy,
            &opts,
            LatexConverter.metadata(),
            vec![extra.clone()],
        );

        assert_neutral_matches_legacy(&converted, &expected);
        assert_eq!(sidecar.html, expected.html);
        assert_eq!(sidecar.format, expected.format);
        assert_eq!(
            sidecar.preamble.macros.len(),
            expected.preamble.macros.len()
        );
        assert!(converted.dependencies.contains(&extra));
    }

    #[test]
    fn latex_uses_unsaved_include_preamble_and_bibliography_buffers() {
        let dir = temp_dir("multi-buffer");
        let root = dir.join("paper.tex");
        let defs = dir.join("defs.tex");
        let child = dir.join("child.tex");
        let bib = dir.join("refs.bib");
        std::fs::write(&defs, "\\newcommand{\\place}{DiskMacro}\n").unwrap();
        std::fs::write(&child, "Disk child: \\place.\\cite{A}\n").unwrap();
        std::fs::write(
            &root,
            "\\documentclass{article}\n\\input{defs}\n\\begin{document}\n\\input{child}\n\\bibliography{refs}\n\\end{document}\n",
        )
        .unwrap();
        let request = ConversionRequest::from_path(&root)
            .with_file_override(&defs, "\\newcommand{\\place}{BufferMacro}\n")
            .with_file_override(&child, "Buffer child: \\place.\\cite{A}\n")
            .with_file_override(
                &bib,
                "@article{A, author={Ada Lovelace}, title={Buffer title}, year={1843}}\n",
            );
        let converted = LatexConverter
            .convert(request, &HtmlOptions::default())
            .unwrap();
        assert!(converted.body_html.contains(">Buffer</span>"));
        assert!(!converted.body_html.contains(">Disk</span>"));
        assert!(converted.body_html.contains("BufferMacro"));
        assert!(
            converted.body_html.contains("Buffer title"),
            "{}",
            converted.body_html
        );
        let math = converted.runtime.math.as_ref().unwrap();
        assert!(math
            .macros
            .iter()
            .any(|definition| definition.name == "place" && definition.body == "BufferMacro"));
        assert!(converted.dependencies.iter().any(|dependency| {
            dependency.kind == DependencyKind::include() && same_path(&dependency.path, &child)
        }));
        assert!(converted.dependencies.iter().any(|dependency| {
            dependency.kind == DependencyKind::bibliography()
                && same_path(&dependency.path, &bib)
                && !dependency.exists
        }));

        let legacy = crate::render_document(&root, &HtmlOptions::default()).unwrap();
        assert!(legacy
            .included_files
            .iter()
            .any(|path| same_path(path, &bib)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn markdown_root_source_wins_over_duplicate_override() {
        let root = Path::new("notes.md");
        let converted = MarkdownConverter
            .convert(
                ConversionRequest::from_source(root, "From source")
                    .with_file_override(crate::project::override_key(root), "From override"),
                &HtmlOptions::default(),
            )
            .unwrap();

        assert!(converted.body_html.contains(">source</span>"));
        assert!(!converted.body_html.contains(">override</span>"));
    }

    #[test]
    fn normalized_missing_root_override_is_accepted_without_disk_io() {
        let dir = temp_dir("missing-root-override");
        let root = dir.join("paper.tex");
        let normalized = crate::project::override_key(&root);
        let converted = LatexConverter
            .convert(
                ConversionRequest::from_path(&root).with_file_override(
                    normalized,
                    "\\begin{document}Unsaved root.\\end{document}",
                ),
                &HtmlOptions::default(),
            )
            .unwrap();

        assert!(converted.body_html.contains(">Unsaved</span>"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn runtime_colon_fences_match_full_shell_format_semantics() {
        let mut opts = HtmlOptions::default();
        opts.markdown_config.colon_fences = false;
        let latex = LatexConverter
            .convert(
                ConversionRequest::from_source(
                    "paper.tex",
                    "\\begin{document}Text.\\end{document}",
                ),
                &opts,
            )
            .unwrap();
        let markdown = MarkdownConverter
            .convert(ConversionRequest::from_source("notes.md", "Text."), &opts)
            .unwrap();

        assert!(latex.runtime.viewer.markdown_colon_fences);
        assert!(!markdown.runtime.viewer.markdown_colon_fences);
    }

    #[cfg(unix)]
    #[test]
    fn missing_bibliography_override_resolves_through_symlinked_project_path() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir("symlinked-missing-bib");
        let real = dir.join("real");
        let alias = dir.join("alias");
        std::fs::create_dir_all(&real).unwrap();
        symlink(&real, &alias).unwrap();

        let root = alias.join("paper.tex");
        let bibliography = alias.join("refs.bib");
        let source = concat!(
            "\\documentclass{article}\n",
            "\\begin{document}See \\cite{AliasEntry}.\\bibliography{refs}\\end{document}\n",
        );
        let converted = LatexConverter
            .convert(
                ConversionRequest::from_source(&root, source).with_file_override(
                    &bibliography,
                    "@book{AliasEntry, author={Ada Lovelace}, title={Alias title}, year={1843}}\n",
                ),
                &HtmlOptions::default(),
            )
            .unwrap();

        assert!(converted.body_html.contains("Alias title"));
        assert!(converted.dependencies.iter().any(|dependency| {
            dependency.kind == DependencyKind::bibliography()
                && same_path(&dependency.path, &bibliography)
                && !dependency.exists
        }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn runtime_and_additional_dependencies_are_complete() {
        let dir = temp_dir("runtime");
        let config = dir.join("future.toml");
        let mut opts = HtmlOptions {
            engine: Engine::MathJax(crate::MathJaxEngine::new("/vendor/mathjax.js")),
            ..HtmlOptions::default()
        };
        opts.viewer_config.mathjax_config =
            "window.MathJax.options.enableMenu = false;".to_string();
        let source = concat!(
            "\\documentclass{article}\n",
            "\\usepackage{amsmath}\n",
            "\\newcommand{\\RR}{\\mathbb{R}}\n",
            "\\begin{document}$\\RR$\\end{document}\n",
        );
        let converted = LatexConverter
            .convert(
                ConversionRequest::from_source(dir.join("paper.tex"), source)
                    .with_dependency(ConvertedDependency::new(&config, DependencyKind::config())),
                &opts,
            )
            .unwrap();
        let math = converted.runtime.math.as_ref().unwrap();
        assert_eq!(math.script_url.as_deref(), Some("/vendor/mathjax.js"));
        assert_eq!(math.config, "window.MathJax.options.enableMenu = false;");
        assert!(math.macros.iter().any(|macro_| macro_.name == "RR"));
        assert_eq!(math.packages, vec!["noerrors", "ams"]);
        assert_eq!(math.loader_packages, vec!["[tex]/noerrors", "[tex]/ams"]);
        assert!(converted.dependencies.iter().any(|dependency| {
            dependency.kind == DependencyKind::config()
                && dependency.path == config
                && !dependency.exists
        }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prospective_tex_inputs_are_emitted_as_missing_include_dependencies() {
        let dir = temp_dir("prospective-input-dependencies");
        let root = dir.join("paper.tex");
        let definitions = dir.join("definitions.tex");
        let local_style = dir.join("local-style.sty");
        let chapter = dir.join("chapter.tex");
        std::fs::write(
            &root,
            concat!(
                "\\documentclass{article}\n",
                "\\input{definitions}\n",
                "\\usepackage{local-style}\n",
                "\\begin{document}\\input{chapter}\\end{document}\n",
            ),
        )
        .unwrap();

        let converted = LatexConverter
            .convert(ConversionRequest::from_path(&root), &HtmlOptions::default())
            .unwrap();

        for path in [&definitions, &local_style, &chapter] {
            assert!(
                converted.dependencies.iter().any(|dependency| {
                    dependency.kind == DependencyKind::include()
                        && same_path(&dependency.path, path)
                        && !dependency.exists
                }),
                "missing prospective dependency {}",
                path.display()
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn conversion_request_bounds_override_count_and_total_bytes() {
        let mut request = ConversionRequest::from_path("paper.tex");
        for index in 0..=MAX_CONVERSION_OVERRIDE_FILES {
            request
                .file_overrides
                .insert(PathBuf::from(format!("chapter-{index}.tex")), String::new());
        }
        assert!(request.validate().is_err());

        let request = ConversionRequest::from_source("paper.tex", "root")
            .with_file_override("chapter.tex", "child");
        assert!(request.validate_with_limits(1, 8, 0).is_err());
    }

    #[test]
    fn converted_sync_map_reindexes_all_lookup_paths() {
        let file = PathBuf::from("paper.tex");
        let mut original = crate::SyncIndex::new();
        original.record_with_kind(
            "container",
            file.clone(),
            crate::Pos {
                line: 1,
                col: 1,
                byte: 0,
            },
            crate::Pos {
                line: 8,
                col: 1,
                byte: 80,
            },
            None,
            crate::sync::SyncKind::Container,
        );
        original.record(
            "old-label",
            file.clone(),
            crate::Pos {
                line: 2,
                col: 1,
                byte: 10,
            },
            crate::Pos {
                line: 2,
                col: 10,
                byte: 19,
            },
            Some("duplicate".to_string()),
        );
        original.record(
            "new-label",
            file.clone(),
            crate::Pos {
                line: 4,
                col: 3,
                byte: 40,
            },
            crate::Pos {
                line: 4,
                col: 12,
                byte: 49,
            },
            Some("duplicate".to_string()),
        );
        original.record(
            "math",
            file.clone(),
            crate::Pos {
                line: 10,
                col: 1,
                byte: 100,
            },
            crate::Pos {
                line: 13,
                col: 1,
                byte: 130,
            },
            None,
        );
        original.record_math_rows(
            "math",
            file.clone(),
            vec![
                crate::sync::MathRow {
                    start_line: 10,
                    end_line: 10,
                    start_col: 3,
                },
                crate::sync::MathRow {
                    start_line: 11,
                    end_line: 12,
                    start_col: 5,
                },
            ],
        );

        let neutral = ConvertedSyncMap::from_sync_index(original);
        assert!(neutral.label_index.is_some());
        let expected_entries = neutral.entries.clone();
        let expected_math_rows = neutral.math_rows.clone();
        let reindexed = neutral.into_sync_index();

        assert_eq!(
            ConvertedSyncMap::from_sync_index(reindexed.clone()),
            ConvertedSyncMap::from_parts(expected_entries, expected_math_rows)
        );
        assert_eq!(
            reindexed.lookup_by_label("duplicate").unwrap().element_id,
            "new-label"
        );
        assert_eq!(
            reindexed
                .lookup_by_source_position(&file, 4, 5)
                .unwrap()
                .element_id,
            "new-label"
        );
        assert_eq!(
            reindexed.leaves_in_range(&file, 2, 1, 4, 12),
            vec!["old-label".to_string(), "new-label".to_string()]
        );
        assert_eq!(
            reindexed.math_rows_in_range(&file, 11, 11),
            vec![("math".to_string(), 2, vec![1])]
        );
        assert_eq!(reindexed.math_row_pos("math", &file, 1, 2), Some((11, 5)));

        let encoded = serde_json::to_string(&ConvertedSyncMap::from_sync_index(reindexed)).unwrap();
        let decoded: ConvertedSyncMap = serde_json::from_str(&encoded).unwrap();
        assert!(decoded.label_index.is_none());
        assert_eq!(
            decoded
                .into_sync_index()
                .lookup_by_label("duplicate")
                .unwrap()
                .element_id,
            "new-label"
        );
    }

    #[test]
    fn large_in_process_sync_handoff_reuses_vectors_and_label_cache() {
        let file = PathBuf::from("large.tex");
        let mut original = crate::SyncIndex::new();
        for index in 0..25_000u32 {
            original.record(
                format!("node-{index}"),
                file.clone(),
                crate::Pos {
                    line: index + 1,
                    col: 1,
                    byte: index,
                },
                crate::Pos {
                    line: index + 1,
                    col: 2,
                    byte: index + 1,
                },
                (index % 10 == 0).then(|| format!("label-{index}")),
            );
        }
        let entries_ptr = original.entries.as_ptr();
        let started = std::time::Instant::now();
        let neutral = ConvertedSyncMap::from_sync_index(original);
        assert_eq!(neutral.entries.as_ptr(), entries_ptr);
        assert!(neutral.label_index.is_some());
        let indexed = neutral.into_sync_index();
        assert_eq!(indexed.entries.as_ptr(), entries_ptr);
        assert_eq!(
            indexed.lookup_by_label("label-24990").unwrap().element_id,
            "node-24990"
        );
        eprintln!(
            "25k-entry neutral sync handoff preserved allocations in {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn builtin_finalizer_moves_renderer_blocks_without_remapping() {
        let blocks = vec![crate::renderer::RenderedBlock {
            id: "blk-0".to_string(),
            hash: "public".to_string(),
            src: Some("paper.tex:1:1".to_string()),
            source_anchors: vec![crate::renderer::SourceAnchor {
                id: "word-g0-0".to_string(),
                src: "paper.tex:1:1".to_string(),
            }],
            diff_hash: "stable".to_string(),
            html: "<article>body</article>".to_string(),
            sub_blocks: Some(crate::renderer::SubBody {
                prefix_diff: "prefix".to_string(),
                suffix_diff: "suffix".to_string(),
                children: vec![crate::renderer::SubChunk {
                    diff_hash: "child".to_string(),
                    html: "<p>child</p>".to_string(),
                }],
            }),
        }];
        let blocks_ptr = blocks.as_ptr();
        let anchors_ptr = blocks[0].source_anchors.as_ptr();
        let children_ptr = blocks[0].sub_blocks.as_ref().unwrap().children.as_ptr();
        let preamble = crate::macros::extract_preamble_from_overrides(&[]);
        let opts = HtmlOptions::default();
        let document = finalize_builtin_document(
            latex_metadata(),
            PathBuf::from("paper.tex"),
            crate::renderer::RenderedBody {
                body: "<article>body</article>".to_string(),
                blocks,
                tikz_assets: HashMap::new(),
            },
            crate::SyncIndex::new(),
            &preamble,
            Vec::new(),
            Vec::new(),
            &opts,
        );

        assert_eq!(document.blocks.as_ptr(), blocks_ptr);
        assert_eq!(document.blocks[0].source_anchors.as_ptr(), anchors_ptr);
        assert_eq!(
            document.blocks[0]
                .sub_blocks
                .as_ref()
                .unwrap()
                .children
                .as_ptr(),
            children_ptr
        );
    }
}
