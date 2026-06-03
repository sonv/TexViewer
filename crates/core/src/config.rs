//! User preferences loaded from TOML.
//!
//! Discovery order (last layer wins per field):
//!   1. Built-in defaults (the `Default` impls below)
//!   2. `~/.config/mathpreview/config.toml` (or `$XDG_CONFIG_HOME/...`)
//!   3. `.mathpreview.toml` walking up from the document root
//!   4. `--config <file>` CLI flag
//!
//! All fields are `Option<T>` so that an unset value at a layer falls
//! through to the previous layer cleanly. Apply the merged config with
//! [`Config::with_defaults`] to fill missing fields with the documented
//! defaults; the result has `non-Option` accessors that always answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const PROJECT_CONFIG_FILENAME: &str = ".mathpreview.toml";

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ViewerConfig {
    /// Body font size in CSS pixels. Default 18.
    pub font_size: Option<u32>,
    /// Initial page mode for new clients. localStorage still wins once
    /// the user toggles in-browser; this sets the *default* for a
    /// fresh tab. `"a4"` or `"dynamic"`.
    pub default_page_mode: Option<PageMode>,
    /// Initial theme for new clients. Same localStorage-wins semantics
    /// as `default-page-mode`. `"system"` follows the OS
    /// `prefers-color-scheme`.
    pub default_theme: Option<Theme>,
    /// Wrap long display equations in the preview (MathJax automatic line
    /// breaking). Default `true`. Set `false` to let long math overflow and
    /// scroll horizontally instead, matching a non-`breqn` PDF more closely.
    pub wrap_equations: Option<bool>,
    #[serde(default)]
    pub source_jump: SourceJumpConfig,
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
    pub default_page_mode: PageMode,
    pub default_theme: Theme,
    pub source_jump_trigger: SourceJumpTrigger,
    pub wrap_equations: bool,
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        Self {
            viewer: ResolvedViewerConfig {
                font_size: 18,
                default_page_mode: PageMode::A4,
                default_theme: Theme::System,
                source_jump_trigger: SourceJumpTrigger::CmdClick,
                wrap_equations: true,
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
    }

    /// Collapse this partial config into the resolved shape used by the
    /// renderer, filling any unset field with its built-in default.
    pub fn resolve(self) -> ResolvedConfig {
        let defaults = ResolvedConfig::default();
        ResolvedConfig {
            viewer: ResolvedViewerConfig {
                font_size: self.viewer.font_size.unwrap_or(defaults.viewer.font_size),
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
                wrap_equations: self
                    .viewer
                    .wrap_equations
                    .unwrap_or(defaults.viewer.wrap_equations),
            },
            text_macros: self.text_macros,
        }
    }

    /// Parse a TOML string into a `Config`. Returns an error with the
    /// source path attached for context if the file is malformed.
    pub fn parse(source: &str, label: &Path) -> Result<Self> {
        toml::from_str::<Config>(source)
            .with_context(|| format!("parsing config file {}", label.display()))
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
}

impl ViewerConfig {
    fn merge(&mut self, other: ViewerConfig) {
        if other.font_size.is_some() {
            self.font_size = other.font_size;
        }
        if other.default_page_mode.is_some() {
            self.default_page_mode = other.default_page_mode;
        }
        if other.default_theme.is_some() {
            self.default_theme = other.default_theme;
        }
        if other.wrap_equations.is_some() {
            self.wrap_equations = other.wrap_equations;
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
        assert_eq!(cfg.viewer.source_jump_trigger, SourceJumpTrigger::CmdClick);
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

[viewer.source-jump]
trigger = "double-click"
"#;
        let cfg = Config::parse(src, Path::new("test.toml")).unwrap();
        let resolved = cfg.resolve();
        assert_eq!(resolved.viewer.font_size, 22);
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
        // higher omitted source-jump, so lower's value survived:
        assert_eq!(
            resolved.viewer.source_jump_trigger,
            SourceJumpTrigger::AltClick,
        );
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
