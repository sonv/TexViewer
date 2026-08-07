//! User preferences loaded from TOML.
//!
//! Discovery order (last layer wins per field, macro name, or keybinding
//! action):
//!   1. Built-in defaults (the `Default` impls below)
//!   2. `~/.config/mathpreview/config.toml` (or `$XDG_CONFIG_HOME/...`)
//!   3. `.mathpreview.toml` walking up from the document root
//!   4. `--config <file>` CLI flag
//!
//! Optional viewer fields fall through to the previous layer cleanly; map-like
//! tables merge by key. [`Config::resolve`] fills everything omitted with the
//! documented built-in defaults.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const PROJECT_CONFIG_FILENAME: &str = ".mathpreview.toml";

/// Complete built-in viewer configuration shown in the config dialog when
/// the selected global/project file does not exist yet. A regression test
/// resolves this template and compares it with `ResolvedConfig::default` so
/// the editable example cannot silently drift from the runtime defaults.
pub const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("assets/default-config.toml");

/// A `[text-macros]` entry: an HTML template plus an optional argument count
/// and an optional default for the first argument. Mirrors MathJax's macro
/// form, so it deserializes from either:
///
/// * a bare string — `name = "<b>#1</b>"` (arg count inferred from the highest
///   `#n`), or
/// * an array — `name = ["#1\\lvert#2#1\\rvert", 2, ""]` =
///   `[template, n_args, default]` (n_args and default optional).
///
/// The template is HTML (emitted as-is; `#1`..`#9` are filled with the
/// rendered, escaped arguments). TeX-valued macros go in a `macros.tex`
/// override via `\newcommand` instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMacro {
    pub html: String,
    /// Explicit argument count; `None` → inferred from the highest `#n`.
    pub n_args: Option<u32>,
    /// Default value for an optional first argument (LaTeX `[def]`).
    pub default: Option<String>,
}

impl<'de> Deserialize<'de> for TextMacro {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Seq(Vec<toml::Value>),
        }
        match Raw::deserialize(d)? {
            Raw::Str(html) => Ok(TextMacro {
                html,
                n_args: None,
                default: None,
            }),
            Raw::Seq(items) => {
                let html = items
                    .first()
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        serde::de::Error::custom(
                            "text-macros array: first element must be the HTML template string",
                        )
                    })?
                    .to_string();
                let n_args = items.get(1).and_then(|v| v.as_integer()).map(|n| n.clamp(0, 9) as u32);
                let default = items.get(2).and_then(|v| v.as_str()).map(|s| s.to_string());
                Ok(TextMacro {
                    html,
                    n_args,
                    default,
                })
            }
        }
    }
}

impl Serialize for TextMacro {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Round-trip to whichever accepted form is shortest.
        if self.n_args.is_none() && self.default.is_none() {
            return s.serialize_str(&self.html);
        }
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(None)?;
        seq.serialize_element(&self.html)?;
        seq.serialize_element(&self.n_args.unwrap_or(0))?;
        if let Some(d) = &self.default {
            seq.serialize_element(d)?;
        }
        seq.end()
    }
}

/// Top-level config object. Every field is optional at the TOML layer so
/// the cascade can do "later wins" merging without conflating "unset" and
/// "set to default". The `viewer` table is also optional so an empty
/// config file is valid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    #[serde(default)]
    pub viewer: ViewerConfig,
    /// Inline text-mode macro → HTML template map, for the preview only.
    /// Keys are command names (with or without a leading `\`); values are an
    /// HTML template with `#1`..`#9` placeholders filled by the rendered
    /// arguments. Lets you give a rendering to macros the previewer can't see
    /// (e.g. defined in a `\usepackage`'d `.sty`). Accepts the table name
    /// `[text-macros]` or `[text_macros]`.
    #[serde(default, alias = "text_macros")]
    pub text_macros: HashMap<String, TextMacro>,
    /// Viewer action → keyboard shortcut(s). A value can be one string or an
    /// array of strings; an empty array explicitly disables an action's
    /// built-in shortcuts. This table lives at top level (`[keybindings]`) so
    /// the global config can hold one keyboard layout for every paper.
    #[serde(default)]
    pub keybindings: BTreeMap<String, KeyBindingList>,
}

/// One or more shortcuts assigned to a viewer action. TOML accepts the common
/// one-shortcut form (`toggle-theme = "T"`) as well as an array
/// (`zoom-in = ["+", "Mod+="]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct KeyBindingList(pub Vec<String>);

impl<'de> Deserialize<'de> for KeyBindingList {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(String),
            Many(Vec<String>),
        }
        Ok(Self(match Raw::deserialize(d)? {
            Raw::One(value) => vec![value],
            Raw::Many(values) => values,
        }))
    }
}

/// Stable public action names accepted in `[keybindings]`. Fixed toolbar
/// controls are included even when they have no built-in shortcut, so every
/// button can be assigned one without changing the viewer code.
pub const KEYBINDING_ACTIONS: &[&str] = &[
    "scroll-left",
    "scroll-down",
    "scroll-up",
    "scroll-right",
    "half-page-down",
    "half-page-up",
    "full-page-down",
    "full-page-up",
    "previous-place",
    "go-top",
    "go-bottom",
    "open-search",
    "open-command",
    "search-next",
    "search-previous",
    "toggle-toc",
    "toggle-topbar",
    "toggle-crop",
    "close-viewer",
    "page-a4",
    "page-dynamic",
    "zoom-in",
    "zoom-out",
    "zoom-reset",
    "zoom-fit-width",
    "browser-print",
    "toggle-margin",
    "toggle-keys",
    "toggle-lines",
    "open-macros",
    "open-config",
    "toggle-log",
    "toggle-theme",
    "proof-main",
    "proof-supporting",
    "proof-all",
    "print-pdf",
    "restart-server",
    "stop-server",
];

fn default_keybindings() -> BTreeMap<String, Vec<String>> {
    let defaults: &[(&str, &[&str])] = &[
        ("scroll-left", &["h"]),
        ("scroll-down", &["j"]),
        ("scroll-up", &["k"]),
        ("scroll-right", &["l"]),
        ("half-page-down", &["Ctrl+d"]),
        ("half-page-up", &["Ctrl+u"]),
        ("full-page-down", &["Space"]),
        ("full-page-up", &["b"]),
        ("previous-place", &["Ctrl+o"]),
        ("go-top", &["g g"]),
        ("go-bottom", &["G"]),
        ("open-search", &["/"]),
        ("open-command", &[":"]),
        ("search-next", &["n"]),
        ("search-previous", &["N"]),
        ("toggle-toc", &["t"]),
        ("toggle-topbar", &["B"]),
        ("toggle-crop", &["c"]),
        ("close-viewer", &["q"]),
        ("page-a4", &["4"]),
        ("page-dynamic", &["d"]),
        ("zoom-in", &["+", "Mod+=", "Mod++"]),
        ("zoom-out", &["-", "_", "Mod+-", "Mod+_"]),
        ("zoom-reset", &["0", "Mod+0"]),
        ("zoom-fit-width", &["="]),
        ("browser-print", &["Mod+p"]),
        // Cmd+M is swallowed by macOS as Minimize, but retaining both forms
        // preserves the historical browser behavior where it is delivered.
        ("toggle-margin", &["Ctrl+m", "Meta+m"]),
    ];
    let mut out = BTreeMap::new();
    for action in KEYBINDING_ACTIONS {
        out.insert((*action).to_string(), Vec::new());
    }
    for (action, bindings) in defaults {
        out.insert(
            (*action).to_string(),
            bindings
                .iter()
                .map(|binding| (*binding).to_string())
                .collect(),
        );
    }
    out
}

fn valid_keybinding(binding: &str) -> bool {
    binding.split_whitespace().all(|raw_step| {
        let mut step = raw_step;
        loop {
            let lower = step.to_ascii_lowercase();
            let Some(prefix) = [
                "mod+", "ctrl+", "control+", "meta+", "cmd+", "command+", "alt+", "option+",
                "shift+",
            ]
            .into_iter()
            .find(|prefix| lower.starts_with(prefix)) else {
                break;
            };
            step = &step[prefix.len()..];
        }
        !step.is_empty() && (!step.contains('+') || step == "+")
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ViewerConfig {
    /// Body font size in CSS pixels. Default 18.
    pub font_size: Option<u32>,
    /// UI-chrome font size in CSS pixels — scales the toolbar (topbar) and
    /// the index/pages side panel (TOC) independently of the document font.
    /// Default 12.
    pub ui_font_size: Option<u32>,
    /// Initial page mode for new clients. localStorage still wins once
    /// the user toggles in-browser; this sets the *default* for a
    /// fresh tab. `"a4"` or `"dynamic"`.
    pub default_page_mode: Option<PageMode>,
    /// Initial theme for new clients. Same localStorage-wins semantics
    /// as `default-page-mode`. `"system"` follows the OS
    /// `prefers-color-scheme`.
    pub default_theme: Option<Theme>,
    /// Removed: display equations now stay unbroken and scroll when wider than
    /// the column, while inline math uses MathJax's browser-based line breaking.
    /// Keep accepting `wrap-equations` so older configs don't fail wholesale
    /// under `deny_unknown_fields`; its value is intentionally ignored.
    #[serde(default, rename = "wrap-equations", skip_serializing)]
    pub _removed_wrap_equations: Option<bool>,
    /// Compile supported TikZ-family diagram environments with a local TeX
    /// engine and show the resulting SVG in the live viewer. Off by default
    /// because TeX compilation executes document code; enable only for trusted
    /// projects. The daemon always disables shell escape for these jobs.
    pub render_tikz: Option<bool>,
    /// Raw JavaScript spliced into the page right after the generated
    /// `window.MathJax = {…}` config and before the MathJax library loads.
    /// Mutate `window.MathJax` here to override anything (output options,
    /// extra macros, packages…). Don't reassign `window.MathJax` wholesale —
    /// the client adapter relies on the generated config.
    pub mathjax_config: Option<String>,
    /// How theorem-like environments are numbered. `"auto"` (default) follows
    /// the document's `\newtheorem` declarations; `"continuous"` forces one
    /// document-wide sequence (Theorem 1, 2, 3…); `"section"` forces per-section
    /// numbering (Theorem 1.1, 1.2…). The override changes reset behavior for
    /// declarations the viewer recognized; it does not invent an undeclared
    /// theorem environment.
    pub theorem_numbering: Option<TheoremNumbering>,
    /// Render recognized theorem-like environments with MathPreview's
    /// enhanced card treatment. When disabled, the renderer keeps their
    /// semantic heading and numbering but uses a plain, PDF-like layout.
    /// Enabled by default for compatibility with the existing viewer style.
    pub fancy_theorems: Option<bool>,
    /// How much of the document to typeset (render math for) at once.
    /// `"local"` (default) typesets only the region around the viewport and
    /// leaves the rest until you scroll to it — lowest memory/CPU on a long
    /// paper. `"background"` typesets the visible region first, then quietly
    /// fills in the rest while the tab is idle, so scrolling to deep sections
    /// and printing never wait. Cmd+P always typesets the whole document on
    /// demand regardless of this setting.
    pub typeset_mode: Option<TypesetMode>,
    /// A4-mode horizontal page margin, in millimetres. When unset, the viewer
    /// uses the document's own `\usepackage[margin=…]{geometry}` if it declares
    /// one, else the built-in default (~17 mm, matching the print margin).
    /// Setting this pins the on-screen A4 margin AND the Cmd+P print margin
    /// together, so screen and print keep wrapping identically.
    pub page_margin: Option<u32>,
    #[serde(default)]
    pub source_jump: SourceJumpConfig,
    /// Removed: the on-screen page-break guides were dropped. Accepted but
    /// ignored so a config that still sets `page-guides` (written while the
    /// feature existed) doesn't fail to parse — `deny_unknown_fields` would
    /// otherwise reject the WHOLE file and silently drop every setting to its
    /// default. The dialog saves via `toml_edit`, which preserves the key in
    /// place, so it lingers harmlessly until hand-removed; `skip_serializing`
    /// only drops it from a wholesale serde re-serialize. Remove this shim once
    /// no live configs carry the key.
    #[serde(default, rename = "page-guides", skip_serializing)]
    pub _removed_page_guides: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PageMode {
    A4,
    Dynamic,
}

impl PageMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PageMode::A4 => "a4",
            PageMode::Dynamic => "dynamic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

/// How theorem-like environments are numbered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TheoremNumbering {
    /// Follow the document's `\newtheorem` declarations (default).
    Auto,
    /// One document-wide sequence (Theorem 1, 2, 3…), ignoring section resets.
    Continuous,
    /// Per-section numbering (Theorem 1.1, 1.2, 2.1…).
    Section,
}

impl TheoremNumbering {
    pub fn as_str(self) -> &'static str {
        match self {
            TheoremNumbering::Auto => "auto",
            TheoremNumbering::Continuous => "continuous",
            TheoremNumbering::Section => "section",
        }
    }
}

/// How much of the document is typeset at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypesetMode {
    /// Only the region around the viewport, plus a small buffer (default).
    Local,
    /// The viewport first, then the rest in the background while idle.
    Background,
}

impl TypesetMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TypesetMode::Local => "local",
            TypesetMode::Background => "background",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct SourceJumpConfig {
    /// Which click gesture in the viewer should ask the daemon to open
    /// the editor at the source line. Default `cmd-click` (which also
    /// matches Ctrl-click on Linux — the JS handler accepts either Meta
    /// or Ctrl when this is set).
    pub trigger: Option<SourceJumpTrigger>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceJumpTrigger {
    CmdClick,
    CtrlClick,
    AltClick,
    DoubleClick,
}

impl SourceJumpTrigger {
    /// String tag the client JS reads. Kept as a separate method (instead
    /// of leaning on `Display`) so the wire format is explicit.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceJumpTrigger::CmdClick => "cmd-click",
            SourceJumpTrigger::CtrlClick => "ctrl-click",
            SourceJumpTrigger::AltClick => "alt-click",
            SourceJumpTrigger::DoubleClick => "double-click",
        }
    }
}

/// Resolved config — every field has a value (either user-supplied or
/// the built-in default). This is the shape the renderer consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub viewer: ResolvedViewerConfig,
    /// Inline text-mode macro → HTML template map (see [`Config::text_macros`]).
    pub text_macros: HashMap<String, TextMacro>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedViewerConfig {
    pub font_size: u32,
    pub ui_font_size: u32,
    pub default_page_mode: PageMode,
    pub default_theme: Theme,
    pub source_jump_trigger: SourceJumpTrigger,
    pub render_tikz: bool,
    pub mathjax_config: String,
    pub theorem_numbering: TheoremNumbering,
    pub fancy_theorems: bool,
    pub typeset_mode: TypesetMode,
    /// Explicit A4 page margin (mm), or `None` to fall back to the document's
    /// geometry margin / the built-in default. Kept as an `Option` (unlike the
    /// other resolved fields) because "unset" is meaningful: only then does the
    /// preamble-derived margin apply.
    pub page_margin_mm: Option<u32>,
    /// Effective action bindings after built-ins + global + project + CLI
    /// config layers have merged action-by-action.
    pub keybindings: BTreeMap<String, Vec<String>>,
}

/// The A4 page margin actually in effect, in millimetres — the single value
/// the shell derives both the screen padding and the print `@page` margin
/// from, so they stay in lockstep. Precedence: explicit `page-margin` config >
/// the document's geometry margin > `None` (the built-in default; the caller
/// emits no override and default.css's 64px / 17mm stand). Each layer is
/// range-checked INDEPENDENTLY, so an absurd config value (a typo like 200)
/// falls through to a valid geometry margin instead of past it to the
/// default. Rounded to 0.1 mm so the value baked into the page and the value
/// pushed over the WebSocket compare equal on the client (float-exact), which
/// the reload-on-change check relies on.
pub fn effective_page_margin_mm(
    cfg: &ResolvedViewerConfig,
    geometry_margin_mm: Option<f64>,
) -> Option<f64> {
    let sane = |mm: &f64| (5.0..=60.0).contains(mm);
    cfg.page_margin_mm
        .map(f64::from)
        .filter(sane)
        .or_else(|| geometry_margin_mm.filter(sane))
        .map(|mm| (mm * 10.0).round() / 10.0)
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        Self {
            viewer: ResolvedViewerConfig {
                font_size: 18,
                ui_font_size: 12,
                default_page_mode: PageMode::A4,
                default_theme: Theme::System,
                source_jump_trigger: SourceJumpTrigger::CmdClick,
                render_tikz: false,
                mathjax_config: String::new(),
                theorem_numbering: TheoremNumbering::Auto,
                fancy_theorems: true,
                typeset_mode: TypesetMode::Local,
                page_margin_mm: None,
                keybindings: default_keybindings(),
            },
            text_macros: HashMap::new(),
        }
    }
}

impl Config {
    /// Merge `other` into `self`. Each `Option` field from `other`
    /// overwrites the corresponding field in `self` if it is `Some`,
    /// otherwise the existing value is kept. Nested `*Config` structs
    /// merge recursively. Standard config-cascade semantics: the call
    /// order is `lower.merge(higher)`, so later layers win per field.
    pub fn merge(&mut self, other: Config) {
        self.viewer.merge(other.viewer);
        // Later layers win per macro name.
        for (name, body) in other.text_macros {
            self.text_macros.insert(name, body);
        }
        // Later layers win per action, not for the whole table. A project can
        // override one shortcut while keeping the user's global keyboard map.
        for (action, bindings) in other.keybindings {
            self.keybindings.insert(action, bindings);
        }
    }

    /// Collapse this partial config into the resolved shape used by the
    /// renderer, filling any unset field with its built-in default.
    pub fn resolve(self) -> ResolvedConfig {
        let defaults = ResolvedConfig::default();
        let mut keybindings = defaults.viewer.keybindings.clone();
        for (action, bindings) in self.keybindings {
            keybindings.insert(action, bindings.0);
        }
        ResolvedConfig {
            viewer: ResolvedViewerConfig {
                font_size: self.viewer.font_size.unwrap_or(defaults.viewer.font_size),
                ui_font_size: self
                    .viewer
                    .ui_font_size
                    .unwrap_or(defaults.viewer.ui_font_size),
                default_page_mode: self
                    .viewer
                    .default_page_mode
                    .unwrap_or(defaults.viewer.default_page_mode),
                default_theme: self
                    .viewer
                    .default_theme
                    .unwrap_or(defaults.viewer.default_theme),
                source_jump_trigger: self
                    .viewer
                    .source_jump
                    .trigger
                    .unwrap_or(defaults.viewer.source_jump_trigger),
                render_tikz: self
                    .viewer
                    .render_tikz
                    .unwrap_or(defaults.viewer.render_tikz),
                mathjax_config: self.viewer.mathjax_config.unwrap_or_default(),
                theorem_numbering: self
                    .viewer
                    .theorem_numbering
                    .unwrap_or(defaults.viewer.theorem_numbering),
                fancy_theorems: self
                    .viewer
                    .fancy_theorems
                    .unwrap_or(defaults.viewer.fancy_theorems),
                typeset_mode: self
                    .viewer
                    .typeset_mode
                    .unwrap_or(defaults.viewer.typeset_mode),
                // Passthrough (no default fill): `None` means "fall back to the
                // document's geometry margin, resolved at render time".
                page_margin_mm: self.viewer.page_margin,
                keybindings,
            },
            text_macros: self.text_macros,
        }
    }

    /// Parse a TOML string into a `Config`. Returns an error with the
    /// source path attached for context if the file is malformed.
    pub fn parse(source: &str, label: &Path) -> Result<Self> {
        let config = toml::from_str::<Config>(source)
            .with_context(|| format!("parsing config file {}", label.display()))?;
        config.validate_keybindings(label)?;
        Ok(config)
    }

    /// Read and parse a single config file. Returns `Ok(None)` if the
    /// file doesn't exist (the cascade tolerates absent layers); returns
    /// an error only if the file *exists* but cannot be parsed.
    pub fn load_optional(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(src) => Self::parse(&src, path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::Error::new(e)
                .context(format!("reading config file {}", path.display()))),
        }
    }

    fn validate_keybindings(&self, label: &Path) -> Result<()> {
        for (action, bindings) in &self.keybindings {
            if !KEYBINDING_ACTIONS.contains(&action.as_str()) {
                bail!(
                    "unknown keybinding action {action:?} in {}; expected one of: {}",
                    label.display(),
                    KEYBINDING_ACTIONS.join(", ")
                );
            }
            if bindings.0.iter().any(|binding| binding.trim().is_empty()) {
                bail!(
                    "empty shortcut for keybinding action {action:?} in {}; use [] to disable it",
                    label.display()
                );
            }
            if let Some(binding) = bindings.0.iter().find(|binding| !valid_keybinding(binding)) {
                bail!(
                    "invalid shortcut {binding:?} for keybinding action {action:?} in {}",
                    label.display()
                );
            }
        }
        Ok(())
    }
}

impl ViewerConfig {
    fn merge(&mut self, other: ViewerConfig) {
        if other.font_size.is_some() {
            self.font_size = other.font_size;
        }
        if other.ui_font_size.is_some() {
            self.ui_font_size = other.ui_font_size;
        }
        if other.default_page_mode.is_some() {
            self.default_page_mode = other.default_page_mode;
        }
        if other.default_theme.is_some() {
            self.default_theme = other.default_theme;
        }
        if other.render_tikz.is_some() {
            self.render_tikz = other.render_tikz;
        }
        if other.mathjax_config.is_some() {
            self.mathjax_config = other.mathjax_config;
        }
        if other.theorem_numbering.is_some() {
            self.theorem_numbering = other.theorem_numbering;
        }
        if other.fancy_theorems.is_some() {
            self.fancy_theorems = other.fancy_theorems;
        }
        if other.typeset_mode.is_some() {
            self.typeset_mode = other.typeset_mode;
        }
        if other.page_margin.is_some() {
            self.page_margin = other.page_margin;
        }
        self.source_jump.merge(other.source_jump);
    }
}

impl SourceJumpConfig {
    fn merge(&mut self, other: SourceJumpConfig) {
        if other.trigger.is_some() {
            self.trigger = other.trigger;
        }
    }
}

/// Discover all config files in cascade order (lowest → highest priority).
/// Caller passes any extra explicit paths (e.g. from a `--config` CLI
/// flag); those land at the end so they win over both global and project.
///
/// Lookup order:
///   1. `$XDG_CONFIG_HOME/mathpreview/config.toml`
///      (or `~/.config/mathpreview/config.toml`)
///   2. The nearest `.mathpreview.toml` walking up from `root_dir`
///      — falling back to `root_dir/.mathpreview.toml` if no ancestor
///      file exists yet
///   3. Each path in `extra`, in order
///
/// Non-existent paths are still returned so the daemon picks them up
/// the moment they're created (by `POST /config/set` or by the user's
/// editor). `load_and_merge` silently skips missing files.
pub fn discover_config_files(root_dir: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = global_config_path() {
        out.push(p);
    }
    out.push(
        find_project_config(root_dir)
            .unwrap_or_else(|| root_dir.join(PROJECT_CONFIG_FILENAME)),
    );
    out.extend(extra.iter().cloned());
    out
}

/// Load + merge every config file in `paths` (cascade order, later
/// wins) and resolve the result with built-in defaults. Returns the
/// resolved config plus the list of files that were actually applied
/// (useful for surfacing to the user via `debug` output).
pub fn load_and_merge(paths: &[PathBuf]) -> Result<(ResolvedConfig, Vec<PathBuf>)> {
    let mut merged = Config::default();
    let mut applied = Vec::new();
    for p in paths {
        if let Some(layer) = Config::load_optional(p)? {
            merged.merge(layer);
            applied.push(p.clone());
        }
    }
    Ok((merged.resolve(), applied))
}

fn global_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("mathpreview").join("config.toml"))
}

fn find_project_config(start: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let candidate = dir.join(PROJECT_CONFIG_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_resolves_to_defaults() {
        let cfg = Config::default().resolve();
        assert_eq!(cfg, ResolvedConfig::default());
        assert_eq!(cfg.viewer.font_size, 18);
        assert_eq!(cfg.viewer.ui_font_size, 12);
        assert_eq!(cfg.viewer.source_jump_trigger, SourceJumpTrigger::CmdClick);
        assert!(!cfg.viewer.render_tikz);
        assert!(cfg.viewer.fancy_theorems);
    }

    #[test]
    fn editable_default_template_matches_runtime_defaults() {
        let cfg = Config::parse(DEFAULT_CONFIG_TEMPLATE, Path::new("<default-config>"))
            .expect("the config-dialog template must remain valid");
        assert_eq!(cfg.resolve(), ResolvedConfig::default());
    }

    #[test]
    fn parses_text_macros_string_and_array_forms() {
        let cfg = Config::parse(
            "[text-macros]\n\
             hello = \"world\"\n\
             abs = [\"#1\\\\lvert#2#1\\\\rvert\", 2, \"\"]\n\
             hl = [\"<mark>#1</mark>\", 1]\n",
            Path::new("t.toml"),
        )
        .unwrap();
        let hello = cfg.text_macros.get("hello").unwrap();
        assert_eq!(hello.html, "world");
        assert_eq!(hello.n_args, None);
        assert_eq!(hello.default, None);
        let abs = cfg.text_macros.get("abs").unwrap();
        assert_eq!(abs.n_args, Some(2));
        assert_eq!(abs.default.as_deref(), Some(""));
        let hl = cfg.text_macros.get("hl").unwrap();
        assert_eq!(hl.n_args, Some(1));
        assert_eq!(hl.default, None);
    }

    #[test]
    fn text_macros_underscore_table_alias() {
        let cfg = Config::parse(
            "[text_macros]\nfoo = \"bar\"\n",
            Path::new("t.toml"),
        )
        .unwrap();
        assert_eq!(cfg.text_macros.get("foo").unwrap().html, "bar");
    }

    #[test]
    fn parses_font_size_and_trigger() {
        let src = r#"
[viewer]
font-size = 22
ui-font-size = 15
render-tikz = true
fancy-theorems = false

[viewer.source-jump]
trigger = "double-click"
"#;
        let cfg = Config::parse(src, Path::new("test.toml")).unwrap();
        let resolved = cfg.resolve();
        assert_eq!(resolved.viewer.font_size, 22);
        assert_eq!(resolved.viewer.ui_font_size, 15);
        assert!(resolved.viewer.render_tikz);
        assert!(!resolved.viewer.fancy_theorems);
        assert_eq!(
            resolved.viewer.source_jump_trigger,
            SourceJumpTrigger::DoubleClick
        );
    }

    #[test]
    fn merge_later_wins_per_field() {
        let mut lower = Config::parse(
            r#"[viewer]
font-size = 16
fancy-theorems = false
[viewer.source-jump]
trigger = "alt-click"
"#,
            Path::new("global.toml"),
        )
        .unwrap();
        let higher = Config::parse(
            r#"[viewer]
font-size = 20
"#, // note: no source-jump section
            Path::new("project.toml"),
        )
        .unwrap();
        lower.merge(higher);
        let resolved = lower.resolve();
        // higher overrode font-size:
        assert_eq!(resolved.viewer.font_size, 20);
        // higher omitted fancy-theorems, so the lower layer survived:
        assert!(!resolved.viewer.fancy_theorems);
        // higher omitted source-jump, so lower's value survived:
        assert_eq!(
            resolved.viewer.source_jump_trigger,
            SourceJumpTrigger::AltClick,
        );
    }

    #[test]
    fn keybindings_accept_one_many_and_disabled_forms() {
        let cfg = Config::parse(
            r#"[keybindings]
zoom-in = "z"
toggle-theme = ["T", "Ctrl+t"]
toggle-lines = []
"#,
            Path::new("keys.toml"),
        )
        .unwrap()
        .resolve();
        assert_eq!(cfg.viewer.keybindings["zoom-in"], ["z"]);
        assert_eq!(cfg.viewer.keybindings["toggle-theme"], ["T", "Ctrl+t"]);
        assert!(cfg.viewer.keybindings["toggle-lines"].is_empty());
        // An omitted action keeps its built-in default.
        assert_eq!(cfg.viewer.keybindings["scroll-down"], ["j"]);
        assert_eq!(cfg.viewer.keybindings["full-page-down"], ["Space"]);
        assert_eq!(cfg.viewer.keybindings["full-page-up"], ["b"]);
    }

    #[test]
    fn keybindings_merge_action_by_action_across_config_layers() {
        let mut global = Config::parse(
            "[keybindings]\nscroll-down = \"ArrowDown\"\ntoggle-theme = \"T\"\n",
            Path::new("global.toml"),
        )
        .unwrap();
        let project = Config::parse(
            "[keybindings]\nscroll-down = \"j\"\n",
            Path::new("project.toml"),
        )
        .unwrap();
        global.merge(project);
        let resolved = global.resolve();
        assert_eq!(resolved.viewer.keybindings["scroll-down"], ["j"]);
        assert_eq!(resolved.viewer.keybindings["toggle-theme"], ["T"]);
    }

    #[test]
    fn keybindings_reject_unknown_actions_and_empty_strings() {
        let unknown = Config::parse(
            "[keybindings]\ntoogle-theme = \"T\"\n",
            Path::new("typo.toml"),
        )
        .unwrap_err();
        assert!(unknown.to_string().contains("unknown keybinding action"));

        let empty = Config::parse(
            "[keybindings]\ntoggle-theme = \"  \"\n",
            Path::new("empty.toml"),
        )
        .unwrap_err();
        assert!(empty.to_string().contains("use [] to disable"));

        let malformed = Config::parse(
            "[keybindings]\ntoggle-theme = \"Hyper+x\"\n",
            Path::new("malformed.toml"),
        )
        .unwrap_err();
        assert!(malformed.to_string().contains("invalid shortcut"));
    }

    #[test]
    fn every_configurable_action_has_a_resolved_binding_entry() {
        let resolved = Config::default().resolve();
        for action in KEYBINDING_ACTIONS {
            assert!(
                resolved.viewer.keybindings.contains_key(*action),
                "missing default entry for {action}"
            );
        }
    }

    #[test]
    fn unknown_field_errors() {
        let res = Config::parse(
            r#"[viewer]
font-size = 18
weird-extra-field = "oops"
"#,
            Path::new("test.toml"),
        );
        assert!(res.is_err(), "unknown field should be a parse error");
    }

    #[test]
    fn removed_page_guides_key_is_accepted_and_ignored() {
        // The page-guides feature was removed, but configs written while it
        // existed still carry the key. With `deny_unknown_fields` it would
        // reject the whole file and silently drop every other setting to
        // defaults; the shim field keeps such a config parsing.
        let cfg = Config::parse(
            "[viewer]\nfont-size = 19\npage-guides = true\n",
            Path::new("t.toml"),
        )
        .expect("a leftover page-guides key must not fail the whole config");
        // The real settings alongside it are honored, not lost to defaults.
        assert_eq!(cfg.resolve().viewer.font_size, 19);
    }

    #[test]
    fn removed_wrap_equations_key_is_accepted_and_ignored() {
        let cfg = Config::parse(
            "[viewer]\nfont-size = 19\nwrap-equations = false\n",
            Path::new("t.toml"),
        )
        .expect("a leftover wrap-equations key must not fail the whole config");
        assert_eq!(cfg.resolve().viewer.font_size, 19);
    }

    #[test]
    fn page_margin_config_and_geometry_precedence() {
        // Explicit config wins over the document's geometry margin.
        let cfg = Config::parse("[viewer]\npage-margin = 30\n", Path::new("t.toml"))
            .unwrap()
            .resolve();
        assert_eq!(cfg.viewer.page_margin_mm, Some(30));
        assert_eq!(
            effective_page_margin_mm(&cfg.viewer, Some(25.4)),
            Some(30.0)
        );
        // No config → geometry margin applies.
        let bare = Config::default().resolve();
        assert_eq!(bare.viewer.page_margin_mm, None);
        assert_eq!(
            effective_page_margin_mm(&bare.viewer, Some(25.4)),
            Some(25.4)
        );
        // No config, no geometry → None (caller keeps the built-in default).
        assert_eq!(effective_page_margin_mm(&bare.viewer, None), None);
        // Out-of-range config falls through to a VALID geometry margin —
        // a typo'd config must not poison the document's own setting.
        let big = Config::parse("[viewer]\npage-margin = 200\n", Path::new("t.toml"))
            .unwrap()
            .resolve();
        assert_eq!(
            effective_page_margin_mm(&big.viewer, Some(25.4)),
            Some(25.4)
        );
        // …and to None (built-in default) when there's no geometry either.
        assert_eq!(effective_page_margin_mm(&big.viewer, None), None);
        // Rounded to 0.1mm so baked and pushed values compare equal client-side.
        assert_eq!(
            effective_page_margin_mm(&bare.viewer, Some(25.4444)),
            Some(25.4)
        );
    }

    #[test]
    fn missing_file_yields_none() {
        let res = Config::load_optional(Path::new("/definitely/does/not/exist.toml")).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn discover_finds_project_config_in_ancestor() {
        let tmp = std::env::temp_dir().join(format!("mp-config-discover-{}", std::process::id()));
        let nested = tmp.join("chapters");
        std::fs::create_dir_all(&nested).unwrap();
        let project_file = tmp.join(PROJECT_CONFIG_FILENAME);
        std::fs::write(
            &project_file,
            "[viewer]\nfont-size = 19\n",
        )
        .unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_home = std::env::var_os("HOME");
        let isolated = tmp.join("fake-home");
        std::fs::create_dir_all(&isolated).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &isolated);
        std::env::set_var("HOME", &isolated);
        let found = discover_config_files(&nested, &[]);
        if let Some(v) = prev_xdg { std::env::set_var("XDG_CONFIG_HOME", v); } else { std::env::remove_var("XDG_CONFIG_HOME"); }
        if let Some(v) = prev_home { std::env::set_var("HOME", v); } else { std::env::remove_var("HOME"); }
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(
            found.iter().any(|p| p.ends_with(PROJECT_CONFIG_FILENAME)),
            "project config not discovered from nested dir: {found:?}",
        );
    }
}
