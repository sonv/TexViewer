//! Hand-rolled LaTeX body parser.
//!
//! Scope (DESIGN §7): document structure, theorem-like environments with
//! `[role=...]`, sectioning, math delimiters and named math environments,
//! `\ref` / `\eqref` / `\cref` / `\Cref` / `\autoref` / `\pageref` / `\nameref`,
//! `\cite` / `\citet` / `\citep` / `\parencite` / `\textcite`, `\label`, and
//! `\omitref`. Everything else passes through as opaque tokens. Source
//! positions survive to every leaf.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::ast::{
    EnvironmentBoundary, ListKind, Node, NodeKind, Pos, RefKind, Role, Span, TextAlignment,
};
use crate::macros::MacroOverride;
use crate::project::Project;
use crate::theorems::TheoremRegistry;

/// A user `\newenvironment{name}[nargs][default]{begin}{end}` definition.
#[derive(Clone)]
struct EnvMacro {
    /// Code emitted at `\begin{name}` (leading/trailing whitespace trimmed so
    /// the wrapped body keeps its real line numbers — see `parse_user_env`).
    begin: String,
    /// Code emitted at `\end{name}`.
    end: String,
    /// Number of arguments (`#1`…`#n`).
    nargs: u8,
    /// Default for the first argument, when it's declared optional.
    default: Option<String>,
}

thread_local! {
    /// User `\newenvironment` definitions for the current `parse_body`, keyed by
    /// env name. Thread-local (like the renderer's macro table) so every
    /// sub-parser sees them without threading a reference through `new_at`.
    static ENV_MACROS: RefCell<HashMap<String, EnvMacro>> = RefCell::new(HashMap::new());
    /// Package/user declarations that make otherwise-unknown environments
    /// literal. Their bodies must never be recursively interpreted as TeX.
    static DYNAMIC_LITERAL_ENVS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Minted declarations that create user-named inline literal commands.
    static DYNAMIC_INLINE_LITERAL_COMMANDS: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
    /// Shared expansion budget for one outermost user environment. The depth
    /// cap alone
    /// does not bound a recursive definition that emits two copies of itself
    /// at each level (exponential fan-out), so every custom-environment
    /// expansion also consumes from this global budget.
    static ENV_EXPANSIONS_LEFT: Cell<usize> = const { Cell::new(MAX_USER_ENV_EXPANSIONS) };
    /// Number of active user-environment expansions. The work budget resets
    /// for each outermost use, so a long document with many ordinary uses does
    /// not exhaust one document-global allowance.
    static ENV_EXPANSION_ACTIVE: Cell<u32> = const { Cell::new(0) };
}

const MAX_USER_ENV_EXPANSIONS: usize = 1024;

/// Gather `\newenvironment` definitions from the whole project and the
/// preview-only macro override cascade. Referenced files come first, then the
/// root preamble (so a document-local `\renewenvironment` wins), then viewer
/// overrides such as `.mathpreview-macros.tex` (so users can approximate a
/// class-provided environment without changing the real PDF).
fn env_macros_for_project(
    project: &Project,
    overrides: &[MacroOverride],
) -> HashMap<String, EnvMacro> {
    let mut out = HashMap::new();
    for file in &project.preamble_files {
        out.extend(extract_env_macros(&file.source));
    }
    out.extend(extract_env_macros(&project.preamble.source));
    for override_layer in overrides {
        out.extend(extract_env_macros(&override_layer.source));
    }
    out
}

fn literal_envs_for_project(project: &Project, overrides: &[MacroOverride]) -> HashSet<String> {
    let mut out = HashSet::new();
    for file in &project.preamble_files {
        out.extend(declared_literal_environments(&file.source));
    }
    out.extend(declared_literal_environments(&project.preamble.source));
    for override_layer in overrides {
        out.extend(declared_literal_environments(&override_layer.source));
    }
    out
}

fn literal_commands_for_project(project: &Project, overrides: &[MacroOverride]) -> HashSet<String> {
    let mut out = HashSet::new();
    for file in &project.preamble_files {
        out.extend(declared_inline_literal_commands(&file.source));
    }
    out.extend(declared_inline_literal_commands(&project.preamble.source));
    for override_layer in overrides {
        out.extend(declared_inline_literal_commands(&override_layer.source));
    }
    out
}

/// Discover package declarations that create verbatim/code environments.
/// Exposed for the live edit guard so it skips exactly the same literal input
/// before deciding whether a transient buffer is safe to render.
pub fn declared_literal_environments(raw: &str) -> HashSet<String> {
    scan_literal_declarations(raw).environments
}

/// Discover minted declarations that create user-named inline literal macros.
pub fn declared_inline_literal_commands(raw: &str) -> HashSet<String> {
    scan_literal_declarations(raw).commands
}

#[derive(Default)]
struct LiteralDeclarations {
    environments: HashSet<String>,
    commands: HashSet<String>,
}

/// Read a declaration that creates a line-oriented literal environment,
/// returning its public name and the byte after the stored declaration
/// arguments. Consuming the complete declaration matters: commands appearing
/// in option or begin/end-code groups are stored tokens, not live preamble
/// input.
fn literal_environment_declaration_at(
    src: &str,
    command: &str,
    after_word: usize,
) -> Option<(String, usize)> {
    let mut p = skip_ascii_ws(src, after_word);
    if matches!(
        command,
        "newtcblisting"
            | "renewtcblisting"
            | "DeclareTCBListing"
            | "NewTCBListing"
            | "RenewTCBListing"
            | "ProvideTCBListing"
    ) {
        if let Some((_, end)) = read_bracketed(src, p) {
            p = skip_ascii_ws(src, end);
        }
    }

    if command == "newminted" {
        let custom = read_bracketed(src, p).map(|(name, end)| {
            p = skip_ascii_ws(src, end);
            name
        });
        let (language, end) = read_braced(src, p)?;
        p = skip_ascii_ws(src, end);
        if let Some((_, end)) = read_braced(src, p) {
            p = end;
        }
        let name = custom
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("{}code", language.trim()));
        return (!name.trim().is_empty()).then(|| (name.trim().to_string(), p));
    }

    let (name, end) = read_braced(src, p)?;
    p = skip_ascii_ws(src, end);
    match command {
        "DefineVerbatimEnvironment"
        | "CustomVerbatimEnvironment"
        | "RecustomVerbatimEnvironment" => {
            for _ in 0..2 {
                let (_, end) = read_braced(src, p)?;
                p = skip_ascii_ws(src, end);
            }
        }
        "lstnewenvironment" => {
            for _ in 0..2 {
                if let Some((_, end)) = read_bracketed(src, p) {
                    p = skip_ascii_ws(src, end);
                }
            }
            for _ in 0..2 {
                let (_, end) = read_braced(src, p)?;
                p = skip_ascii_ws(src, end);
            }
        }
        "newtcblisting" | "renewtcblisting" => {
            for _ in 0..2 {
                if let Some((_, end)) = read_bracketed(src, p) {
                    p = skip_ascii_ws(src, end);
                }
            }
            let (_, end) = read_braced(src, p)?;
            p = end;
        }
        "DeclareTCBListing" | "NewTCBListing" | "RenewTCBListing" | "ProvideTCBListing" => {
            for _ in 0..2 {
                let (_, end) = read_braced(src, p)?;
                p = skip_ascii_ws(src, end);
            }
        }
        _ => return None,
    }
    (!name.trim().is_empty()).then(|| (name.trim().to_string(), p))
}

fn inline_literal_declaration_at(
    src: &str,
    command: &str,
    after_word: usize,
) -> Option<(String, usize)> {
    if !matches!(command, "newmint" | "newmintinline") {
        return None;
    }
    let mut p = skip_ascii_ws(src, after_word);
    let custom = read_bracketed(src, p).map(|(name, end)| {
        p = skip_ascii_ws(src, end);
        name
    });
    let (language, end) = read_braced(src, p)?;
    p = skip_ascii_ws(src, end);
    if let Some((_, end)) = read_braced(src, p) {
        p = end;
    }
    let default = if command == "newmintinline" {
        format!("{}inline", language.trim())
    } else {
        language.trim().to_string()
    };
    let name = custom
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(default);
    let name = name.trim().trim_start_matches('\\');
    (!name.is_empty()).then(|| (name.to_string(), p))
}

/// Scan only executable preamble input. False branches, stored command
/// replacements, and literal/code bodies cannot create live declarations.
fn scan_literal_declarations(raw: &str) -> LiteralDeclarations {
    let src = crate::macros::strip_line_comments(raw);
    let bytes = src.as_bytes();
    let mut out = LiteralDeclarations::default();
    let mut hidden_ranges: Vec<(usize, usize)> = Vec::new();
    let mut conditionals = ConditionalLookup::default();
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(resume) = scheduled_hidden_resume(i, &mut hidden_ranges) {
            i = resume;
            continue;
        }
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'@') {
            i += 1;
        }
        let command = &src[start + 1..i];
        if command == "iffalse" {
            if let Some(bounds) = conditionals.get(&src, start, i) {
                i = bounds.else_end.unwrap_or(bounds.fi_end);
                continue;
            }
        } else if command == "iftrue" {
            if let Some(bounds) = conditionals.get(&src, start, i) {
                if let Some(else_start) = bounds.else_start {
                    hidden_ranges.push((else_start, bounds.fi_end));
                }
            }
            continue;
        }
        if let Some(end) = skip_command_macro_declaration(&src, start) {
            i = end;
            continue;
        }
        if is_static_inline_literal_command(command) || out.commands.contains(command) {
            let dynamic = out.commands.contains(command);
            i = inline_literal_payload_with_dynamic(&src, command, i, dynamic)
                .map(|(_, end)| end)
                .unwrap_or(bytes.len());
            continue;
        }
        if command == "begin" {
            if let Some(token) = environment_token_at(&src, start) {
                if token.kind == EnvironmentTokenKind::Begin
                    && (environment_is_literal_with(&token.name, &out.environments)
                        || SKIP_ENVS.contains(&token.name.as_str()))
                {
                    i = if (token.name != "alltt"
                        && environment_is_literal_with(&token.name, &out.environments))
                        || SKIP_ENVS.contains(&token.name.as_str())
                    {
                        literal_environment_bounds(&src, token.end, &token.name)
                            .map(|(_, end)| end)
                            .unwrap_or(bytes.len())
                    } else {
                        find_matching_end_lexical_in(&src, token.end, &token.name)
                            .map(|(_, end)| end)
                            .unwrap_or(bytes.len())
                    };
                    continue;
                }
            }
        }
        if let Some((name, end)) = literal_environment_declaration_at(&src, command, i) {
            out.environments.insert(name);
            i = end;
            continue;
        }
        if let Some((name, end)) = inline_literal_declaration_at(&src, command, i) {
            out.commands.insert(name);
            i = end;
            continue;
        }
    }
    out
}

struct EnvDeclaration {
    name: String,
    def: EnvMacro,
    start_line: usize,
    end_line: usize,
}

/// Scan `\newenvironment` / `\renewenvironment` declarations out of one source
/// into a name → definition map. `%` comments are stripped first (like the
/// `\newcommand` extractor does): a trailing-`%` continued definition — the
/// standard multi-line style — must parse, and a commented-out definition must
/// not be honored (nor shadow a live one).
fn extract_env_macros(raw: &str) -> HashMap<String, EnvMacro> {
    let mut out = HashMap::new();
    for declaration in scan_env_declarations(raw, false).unwrap_or_default() {
        out.insert(declaration.name, declaration.def);
    }
    out
}

/// Byte immediately after an environment-definition command at `start`,
/// including its optional star. Parsing the whole control word (rather than
/// prefix-matching text) keeps `\newenvironmenthelper` from being mistaken for
/// a declaration.
fn environment_keyword_end(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'\\') {
        return None;
    }
    let mut end = start + 1;
    while end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'@') {
        end += 1;
    }
    if !matches!(&src[start + 1..end], "newenvironment" | "renewenvironment") {
        return None;
    }
    if bytes.get(end) == Some(&b'*') {
        end += 1;
    }
    Some(end)
}

fn skip_command_name_arg(src: &str, mut p: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    p = skip_tex_space_and_comments(src, p);
    if bytes.get(p) == Some(&b'{') {
        p = tex_inert_group_end(src, p, b'{', b'}')?;
    } else if bytes.get(p) == Some(&b'\\') {
        p += 1;
        if bytes
            .get(p)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@')
        {
            while p < bytes.len() && (bytes[p].is_ascii_alphabetic() || bytes[p] == b'@') {
                p += 1;
            }
        } else {
            p += usize::from(p < bytes.len());
        }
    } else {
        return None;
    }
    Some(p)
}

fn skip_braced_groups(src: &str, mut p: usize, count: usize) -> Option<usize> {
    for _ in 0..count {
        p = skip_tex_space_and_comments(src, p);
        p = tex_inert_group_end(src, p, b'{', b'}')?;
    }
    Some(p)
}

/// Skip declarations whose braced bodies are stored as command definitions,
/// not executed while the preamble is scanned. Otherwise a literal
/// `\newenvironment` inside (say) `\def\factory{...}` would be activated.
fn skip_command_macro_declaration(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'\\') {
        return None;
    }
    let mut p = start + 1;
    while p < bytes.len() && (bytes[p].is_ascii_alphabetic() || bytes[p] == b'@') {
        p += 1;
    }
    let keyword = &src[start + 1..p];
    if bytes.get(p) == Some(&b'*') {
        p += 1;
    }

    if matches!(
        keyword,
        "newcommand" | "renewcommand" | "providecommand" | "DeclareRobustCommand"
    ) {
        p = skip_command_name_arg(src, p)?;
        p = skip_tex_space_and_comments(src, p);
        if let Some((_, next)) = read_bracketed(src, p) {
            p = skip_tex_space_and_comments(src, next);
            if let Some((_, next)) = read_bracketed(src, p) {
                p = next;
            }
        }
        return skip_braced_groups(src, p, 1);
    }

    if matches!(
        keyword,
        "NewDocumentCommand"
            | "RenewDocumentCommand"
            | "ProvideDocumentCommand"
            | "DeclareDocumentCommand"
    ) {
        p = skip_command_name_arg(src, p)?;
        return skip_braced_groups(src, p, 2);
    }

    if matches!(
        keyword,
        "NewDocumentEnvironment"
            | "RenewDocumentEnvironment"
            | "ProvideDocumentEnvironment"
            | "DeclareDocumentEnvironment"
    ) {
        p = skip_command_name_arg(src, p)?;
        return skip_braced_groups(src, p, 3);
    }

    if matches!(keyword, "def" | "edef" | "gdef" | "xdef") {
        p = skip_tex_space_and_comments(src, p);
        if bytes.get(p) != Some(&b'\\') {
            return None;
        }
        p += 1;
        if bytes
            .get(p)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@')
        {
            while p < bytes.len() && (bytes[p].is_ascii_alphabetic() || bytes[p] == b'@') {
                p += 1;
            }
        } else {
            p += usize::from(p < bytes.len());
        }
        while p < bytes.len() {
            if bytes[p] == b'\\' && p + 1 < bytes.len() {
                p += 2;
            } else if bytes[p] == b'%' {
                while p < bytes.len() && bytes[p] != b'\n' {
                    p += 1;
                }
            } else if bytes[p] == b'{' {
                return skip_braced_groups(src, p, 1);
            } else {
                p += 1;
            }
        }
        return None;
    }

    if keyword == "DeclareMathOperator" {
        p = skip_command_name_arg(src, p)?;
        return skip_braced_groups(src, p, 1);
    }

    if matches!(keyword, "DeclarePairedDelimiter" | "newdelim") {
        p = skip_command_name_arg(src, p)?;
        return skip_braced_groups(src, p, 2);
    }

    None
}

fn advance_env_scan(bytes: &[u8], i: &mut usize, line: &mut usize, to: usize) {
    let to = to.min(bytes.len());
    *line += bytes[*i..to].iter().filter(|b| **b == b'\n').count();
    *i = to;
}

/// Lex environment definitions while skipping command-macro declarations as
/// units. This preserves definitions inside immediately executed wrappers such
/// as `\AtBeginDocument{...}`, while text inside a `\newcommand` replacement
/// cannot become an accidental live environment. In strict mode a malformed
/// declaration is an error (used by the macro editor); normal preamble
/// extraction remains best-effort and simply ignores it.
fn scan_env_declarations(raw: &str, strict: bool) -> Result<Vec<EnvDeclaration>> {
    let src = crate::macros::strip_line_comments(raw);
    let bytes = src.as_bytes();
    let literals = scan_literal_declarations(raw);
    let mut declarations = Vec::new();
    let mut hidden_ranges: Vec<(usize, usize)> = Vec::new();
    let mut conditionals = ConditionalLookup::default();
    let mut i = 0usize;
    let mut line = 1usize;
    while i < bytes.len() {
        if let Some(resume) = scheduled_hidden_resume(i, &mut hidden_ranges) {
            advance_env_scan(bytes, &mut i, &mut line, resume);
            continue;
        }
        match bytes[i] {
            b'\\' => {
                let mut word_end = i + 1;
                while word_end < bytes.len()
                    && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
                {
                    word_end += 1;
                }
                let word = &src[i + 1..word_end];
                if word == "iffalse" {
                    let resume = conditionals
                        .get(&src, i, word_end)
                        .map(|bounds| bounds.else_end.unwrap_or(bounds.fi_end))
                        .unwrap_or(bytes.len());
                    advance_env_scan(bytes, &mut i, &mut line, resume);
                    continue;
                }
                if word == "iftrue" {
                    if let Some(bounds) = conditionals.get(&src, i, word_end) {
                        if let Some(else_start) = bounds.else_start {
                            hidden_ranges.push((else_start, bounds.fi_end));
                        }
                    }
                    advance_env_scan(bytes, &mut i, &mut line, word_end);
                    continue;
                }
                if let Some(end) = skip_command_macro_declaration(&src, i) {
                    advance_env_scan(bytes, &mut i, &mut line, end);
                    continue;
                }
                if is_static_inline_literal_command(word) || literals.commands.contains(word) {
                    let end = inline_literal_payload_with_dynamic(
                        &src,
                        word,
                        word_end,
                        literals.commands.contains(word),
                    )
                    .map(|(_, end)| end)
                    .unwrap_or(bytes.len());
                    advance_env_scan(bytes, &mut i, &mut line, end);
                    continue;
                }
                if word == "begin" {
                    if let Some(token) = environment_token_at(&src, i) {
                        if token.kind == EnvironmentTokenKind::Begin
                            && (environment_is_literal_with(&token.name, &literals.environments)
                                || SKIP_ENVS.contains(&token.name.as_str()))
                        {
                            let end = if (token.name != "alltt"
                                && environment_is_literal_with(&token.name, &literals.environments))
                                || SKIP_ENVS.contains(&token.name.as_str())
                            {
                                literal_environment_bounds(&src, token.end, &token.name)
                                    .map(|(_, end)| end)
                                    .unwrap_or(bytes.len())
                            } else {
                                find_matching_end_lexical_in(&src, token.end, &token.name)
                                    .map(|(_, end)| end)
                                    .unwrap_or(bytes.len())
                            };
                            advance_env_scan(bytes, &mut i, &mut line, end);
                            continue;
                        }
                    }
                }
                if let Some(keyword_end) = environment_keyword_end(&src, i) {
                    let start_line = line;
                    match parse_env_macro(&src, keyword_end) {
                        Some((name, def, declaration_end)) => {
                            let end_line = line
                                + bytes[i..declaration_end]
                                    .iter()
                                    .filter(|b| **b == b'\n')
                                    .count();
                            declarations.push(EnvDeclaration {
                                name,
                                def,
                                start_line,
                                end_line,
                            });
                            advance_env_scan(bytes, &mut i, &mut line, declaration_end);
                            continue;
                        }
                        None if strict => {
                            anyhow::bail!("line {start_line}: malformed environment definition");
                        }
                        None => {}
                    }
                }
                // Skip an escaped control symbol / first control-word byte so
                // it is not reconsidered as another command start.
                let next = (i + 2).min(bytes.len());
                advance_env_scan(bytes, &mut i, &mut line, next);
            }
            _ => {
                let next = i + 1;
                advance_env_scan(bytes, &mut i, &mut line, next);
            }
        }
    }
    Ok(declarations)
}

/// Parse one declaration after its `\newenvironment` / `\renewenvironment`
/// keyword. Returns the name, definition, and byte immediately after the end
/// replacement. LaTeX environment definitions have at most nine arguments.
fn parse_env_macro(src: &str, keyword_end: usize) -> Option<(String, EnvMacro, usize)> {
    let mut p = skip_tex_space_and_comments(src, keyword_end);
    let (name, np) = read_braced_inert(src, p)?;
    p = skip_tex_space_and_comments(src, np);
    let mut nargs = 0u8;
    let mut default = None;
    if let Some((n, np)) = read_bracketed(src, p) {
        nargs = n.trim().parse().ok()?;
        if nargs > 9 {
            return None;
        }
        p = skip_tex_space_and_comments(src, np);
        if let Some((d, np2)) = read_bracketed(src, p) {
            if nargs == 0 {
                return None;
            }
            default = Some(d);
            p = skip_tex_space_and_comments(src, np2);
        }
    }
    let (begin, np) = read_braced_inert(src, p)?;
    p = skip_tex_space_and_comments(src, np);
    let (end, declaration_end) = read_braced_inert(src, p)?;
    if !env_parameters_valid(&begin, nargs) || !env_parameters_valid(&end, nargs) {
        return None;
    }
    if default
        .as_deref()
        .is_some_and(|value| !env_default_valid(value))
    {
        return None;
    }
    let name = name.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some((
        name,
        EnvMacro {
            begin: begin.trim().to_string(),
            end: end.trim().to_string(),
            nargs,
            default,
        },
        declaration_end,
    ))
}

fn env_parameters_valid(template: &str, nargs: u8) -> bool {
    let bytes = template.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'#' if i + 1 < bytes.len() && bytes[i + 1] == b'#' => i += 2,
            b'#' if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() => {
                let parameter = bytes[i + 1] - b'0';
                if parameter == 0 || parameter > nargs {
                    return false;
                }
                i += 2;
            }
            b'#' => return false,
            _ => i += 1,
        }
    }
    true
}

fn env_default_valid(default: &str) -> bool {
    let bytes = default.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'#' => return false,
            _ => i += 1,
        }
    }
    true
}

/// Whether a trimmed command-shaped line starts a real environment
/// declaration (not merely a longer control word with the same prefix).
pub fn is_environment_definition_start(line: &str) -> bool {
    let stripped = crate::macros::strip_line_comments(line);
    let src = stripped.trim_start();
    environment_keyword_end(src, 0).is_some()
}

/// Validate one complete environment replacement, as used by the macro
/// editor's single-definition append path.
pub fn validate_environment_override_line(line: &str) -> Result<String> {
    let stripped = crate::macros::strip_line_comments(line);
    let src = stripped.trim();
    let Some(keyword_end) = environment_keyword_end(src, 0) else {
        anyhow::bail!("expected a \\newenvironment or \\renewenvironment definition");
    };
    let Some((name, _, end)) = parse_env_macro(src, keyword_end) else {
        anyhow::bail!("malformed environment definition");
    };
    if !src[end..].trim().is_empty() {
        anyhow::bail!("unexpected text after environment definition");
    }
    Ok(name)
}

/// Validate every environment declaration in an override file, including
/// balanced definitions split across lines. Returns inclusive, one-based line
/// ranges occupied by declarations, allowing the dialog validator to avoid
/// treating replacement-body commands as standalone macro definitions.
pub fn validate_environment_override_source(source: &str) -> Result<Vec<(usize, usize)>> {
    Ok(scan_env_declarations(source, true)?
        .into_iter()
        .map(|declaration| (declaration.start_line, declaration.end_line))
        .collect())
}

fn skip_ascii_ws(src: &str, mut i: usize) -> usize {
    let b = src.as_bytes();
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Match a stored/tokenized group without executing commands found inside it.
/// This is deliberately iterative: deeply nested `\detokenize` or macro
/// definitions in a live buffer must not recurse on the Rust stack.
fn tex_inert_group_end(src: &str, from: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(from) != Some(&open) {
        return None;
    }
    let mut depth = 1usize;
    let mut i = from + 1;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'\\' {
            let token_start = i + 1;
            if token_start >= bytes.len() {
                return None;
            }
            if bytes[token_start].is_ascii_alphabetic() || bytes[token_start] == b'@' {
                i = token_start + 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'@')
                {
                    i += 1;
                }
            } else {
                i = token_start
                    + src[token_start..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8);
            }
            continue;
        }
        match bytes[i] {
            byte if byte == open => depth += 1,
            byte if byte == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += if bytes[i].is_ascii() {
            1
        } else {
            src[i..].chars().next().map_or(1, char::len_utf8)
        };
    }
    None
}

fn read_braced_inert(src: &str, from: usize) -> Option<(String, usize)> {
    let end = tex_inert_group_end(src, from, b'{', b'}')?;
    Some((src[from + 1..end - 1].to_string(), end))
}

/// Return the byte immediately after the matching delimiter. TeX comments,
/// escaped control symbols, `\string`, and inline verbatim payloads are inert:
/// braces inside them cannot close the surrounding group.
pub(crate) fn tex_group_end(src: &str, from: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(from) != Some(&open) {
        return None;
    }
    let mut depth = 1usize;
    let mut i = from + 1;
    let mut hidden_true_branches = Vec::new();
    let mut conditionals = ConditionalLookup::default();
    while i < bytes.len() {
        if hidden_true_branches
            .last()
            .is_some_and(|(else_start, _)| *else_start == i)
        {
            let (_, fi_end) = hidden_true_branches.pop().expect("checked nonempty");
            i = fi_end;
            continue;
        }
        if bytes[i] == b'%' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'\\' {
            let word_start = i + 1;
            let mut word_end = word_start;
            while word_end < bytes.len()
                && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
            {
                word_end += 1;
            }
            if word_end == word_start {
                if word_start >= bytes.len() {
                    return None;
                }
                let step = src[word_start..].chars().next().map_or(1, char::len_utf8);
                i = word_start + step;
                continue;
            }
            let word = &src[word_start..word_end];
            if is_inline_literal_command(word) {
                i = inline_literal_payload(src, word, word_end)
                    .map(|(_, end)| end)
                    .unwrap_or(bytes.len());
                continue;
            }
            if word == "string" {
                i = tex_token_end(src, skip_tex_space_and_comments(src, word_end));
                continue;
            }
            if matches!(word, "detokenize" | "unexpanded") {
                let group_start = skip_tex_space_and_comments(src, word_end);
                i = tex_inert_group_end(src, group_start, b'{', b'}').unwrap_or(bytes.len());
                continue;
            }
            if let Some(end) = skip_command_macro_declaration(src, i) {
                i = end;
                continue;
            }
            if let Some(keyword_end) = environment_keyword_end(src, i) {
                i = parse_env_macro(src, keyword_end)
                    .map(|(_, _, end)| end)
                    .unwrap_or(keyword_end);
                continue;
            }
            if word == "iffalse" {
                if let Some(bounds) = conditionals.get(src, i, word_end) {
                    i = bounds.else_end.unwrap_or(bounds.fi_end);
                    continue;
                }
            }
            if word == "iftrue" {
                if let Some(bounds) = conditionals.get(src, i, word_end) {
                    if let Some(else_start) = bounds.else_start {
                        hidden_true_branches.push((else_start, bounds.fi_end));
                    }
                }
            }
            i = word_end;
            continue;
        }
        match bytes[i] {
            byte if byte == open => depth += 1,
            byte if byte == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += if bytes[i].is_ascii() {
            1
        } else {
            src[i..].chars().next().map_or(1, char::len_utf8)
        };
    }
    None
}

/// If `src[from]` is `{`, return the group's inner text and the index past `}`.
fn read_braced(src: &str, from: usize) -> Option<(String, usize)> {
    let end = tex_group_end(src, from, b'{', b'}')?;
    Some((src[from + 1..end - 1].to_string(), end))
}

/// Braced inline-literal payloads use braces only as delimiters; TeX comment
/// and escape rules are disabled inside them. Balance nested braces while
/// keeping `%`, backslashes, and Unicode completely inert.
fn read_literal_braced(src: &str, from: usize) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(from) != Some(&b'{') {
        return None;
    }
    let mut depth = 1usize;
    let mut i = from + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((src[from + 1..i].to_string(), i + 1));
                }
            }
            _ => {}
        }
        i += if bytes[i].is_ascii() {
            1
        } else {
            src[i..].chars().next().map_or(1, char::len_utf8)
        };
    }
    Some((src[from + 1..].to_string(), bytes.len()))
}

/// Recognize a group whose first executable token is a text-mode color
/// declaration: `{\color[model]{spec} ...}`. The cheap prefix check happens
/// before matching the outer group, avoiding a full-group rescan for every
/// ordinary `{...}` in the document. Returns the declaration, body byte
/// range, and the byte immediately after the outer `}`.
fn scoped_text_color_group(
    src: &str,
    start: usize,
) -> Option<(Option<String>, String, usize, usize, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let mut i = skip_tex_space_and_comments(src, start + 1);
    if bytes.get(i) != Some(&b'\\') {
        return None;
    }
    let word_start = i + 1;
    let mut word_end = word_start;
    while word_end < bytes.len()
        && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
    {
        word_end += 1;
    }
    if &src[word_start..word_end] != "color" {
        return None;
    }
    i = skip_tex_space_and_comments(src, word_end);
    let model = if bytes.get(i) == Some(&b'[') {
        let (model, after_model) = read_bracketed(src, i)?;
        i = skip_tex_space_and_comments(src, after_model);
        Some(model)
    } else {
        None
    };
    let (color, body_start) = read_braced(src, i)?;
    let end = tex_group_end(src, start, b'{', b'}')?;
    let body_end = end - 1;
    (body_start <= body_end).then_some((model, color, body_start, body_end, end))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvironmentTokenKind {
    Begin,
    End,
}

struct EnvironmentToken {
    kind: EnvironmentTokenKind,
    name: String,
    end: usize,
}

const MAX_ENVIRONMENT_NAME_BYTES: usize = 256;

fn skip_tex_space_and_comments(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'%') {
            return i;
        }
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
    }
}

fn read_environment_name(src: &str, from: usize) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(from) != Some(&b'{') {
        return None;
    }
    let limit = (from + 1 + MAX_ENVIRONMENT_NAME_BYTES).min(bytes.len());
    let mut i = from + 1;
    while i < limit {
        match bytes[i] {
            b'}' => return Some((src[from + 1..i].trim().to_string(), i + 1)),
            // Environment names are not nested groups. Reject immediately so
            // repeated malformed `\begin{` tokens cannot each rescan to EOF.
            b'{' => return None,
            _ => i += 1,
        }
    }
    None
}

/// Recognize `\begin{env}` / `\end{env}` at `start`, including TeX-legal
/// whitespace and `%` comments between the control word and its argument.
fn environment_token_at(src: &str, start: usize) -> Option<EnvironmentToken> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'\\') {
        return None;
    }
    let mut word_end = start + 1;
    while word_end < bytes.len()
        && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
    {
        word_end += 1;
    }
    let kind = match &src[start + 1..word_end] {
        "begin" => EnvironmentTokenKind::Begin,
        "end" => EnvironmentTokenKind::End,
        _ => return None,
    };
    let arg_start = skip_tex_space_and_comments(src, word_end);
    let (name, end) = read_environment_name(src, arg_start)?;
    Some(EnvironmentToken { kind, name, end })
}

/// Read one inline verbatim command. The payload must stay inert both while
/// matching an outer environment and while recursively parsing its body.
pub(crate) fn inline_literal_payload(
    src: &str,
    command: &str,
    after_word: usize,
) -> Option<(String, usize)> {
    let base = command.trim_end_matches('*');
    let dynamic = DYNAMIC_INLINE_LITERAL_COMMANDS.with(|commands| commands.borrow().contains(base));
    inline_literal_payload_with_dynamic(src, command, after_word, dynamic)
}

fn inline_literal_payload_with_dynamic(
    src: &str,
    command: &str,
    mut after_word: usize,
    dynamic: bool,
) -> Option<(String, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(after_word) == Some(&b'*') {
        after_word += 1;
    }
    let base = command.trim_end_matches('*');
    if dynamic || matches!(base, "Verb" | "lstinline" | "mintinline" | "mint") {
        after_word = skip_ascii_ws(src, after_word);
        if let Some((_, end)) = read_bracketed(src, after_word) {
            after_word = skip_ascii_ws(src, end);
        }
    }
    if matches!(base, "mintinline" | "mint") {
        let (_, end) = read_braced(src, after_word)?;
        after_word = skip_ascii_ws(src, end);
    }
    if (dynamic || matches!(base, "lstinline" | "mintinline" | "mint"))
        && bytes.get(after_word) == Some(&b'{')
    {
        return read_literal_braced(src, after_word);
    }
    let delimiter = src[after_word..].chars().next()?;
    if delimiter.is_whitespace() {
        return None;
    }
    let payload_start = after_word + delimiter.len_utf8();
    let mut i = payload_start;
    while i < bytes.len() {
        let ch = src[i..].chars().next()?;
        if ch == delimiter {
            return Some((
                src[payload_start..i].to_string(),
                i.saturating_add(ch.len_utf8()),
            ));
        }
        i += ch.len_utf8();
    }
    Some((src[payload_start..].to_string(), bytes.len()))
}

fn is_static_inline_literal_command(command: &str) -> bool {
    matches!(
        command.trim_end_matches('*'),
        "verb" | "Verb" | "lstinline" | "mintinline" | "mint"
    )
}

pub(crate) fn is_inline_literal_command(command: &str) -> bool {
    let base = command.trim_end_matches('*');
    is_static_inline_literal_command(base)
        || DYNAMIC_INLINE_LITERAL_COMMANDS.with(|commands| commands.borrow().contains(base))
}

fn tex_token_end(src: &str, start: usize) -> usize {
    let bytes = src.as_bytes();
    if start >= bytes.len() {
        return bytes.len();
    }
    if bytes[start] != b'\\' {
        let width = src[start..].chars().next().map(char::len_utf8).unwrap_or(0);
        return (start + width).min(bytes.len());
    }
    let mut end = start + 1;
    if bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'@')
    {
        while end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'@') {
            end += 1;
        }
        end
    } else {
        (end + 1).min(bytes.len())
    }
}

/// Literal environments terminate only at a boundary command beginning a
/// source line (apart from indentation). This avoids treating example text
/// such as `print("\\end{verbatim}")` as the real closer. Returns the start of
/// the closing command and the byte immediately after it.
fn literal_environment_bounds(src: &str, from: usize, env: &str) -> Option<(usize, usize)> {
    let close = format!("\\end{{{env}}}");
    let bytes = src.as_bytes();
    let mut line_start = from;
    while line_start < bytes.len() {
        let line_end = src[line_start..]
            .find('\n')
            .map(|offset| line_start + offset)
            .unwrap_or(bytes.len());
        let mut token_start = line_start;
        while token_start < line_end && matches!(bytes[token_start], b' ' | b'\t' | b'\r') {
            token_start += 1;
        }
        if src[token_start..line_end].starts_with(&close) {
            return Some((token_start, token_start + close.len()));
        }
        line_start = line_end.saturating_add(1);
    }
    None
}

/// If `src[from]` is `[`, return the optional-arg text and the index past `]`.
fn read_bracketed(src: &str, from: usize) -> Option<(String, usize)> {
    let b = src.as_bytes();
    if b.get(from) != Some(&b'[') {
        return None;
    }
    let mut depth = 0usize;
    let start = from + 1;
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b']' if depth == 0 => return Some((src[start..i].to_string(), i + 1)),
            _ => {}
        }
        i += 1;
    }
    None
}

/// True when `s` ends in a control word — a `\` (itself unescaped) followed
/// only by ASCII letters through the end, e.g. `…\itshape`. Used to keep a
/// token boundary at the expansion seams in `parse_user_env`: TeX tokenizes a
/// macro body separately, but our textual splice would glue `\itshape` onto a
/// following `Hello` as `\itshapeHello`.
fn ends_with_control_word(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = b.len();
    while i > 0 && b[i - 1].is_ascii_alphabetic() {
        i -= 1;
    }
    if i == b.len() || i == 0 || b[i - 1] != b'\\' {
        return false;
    }
    // The backslash must itself be a command start, not the tail of an escaped
    // `\\` pair: count the run of backslashes ending here — odd means command.
    let mut run = 0;
    let mut j = i;
    while j > 0 && b[j - 1] == b'\\' {
        run += 1;
        j -= 1;
    }
    run % 2 == 1
}

/// Substitute `#1`…`#9` in a `\newenvironment` begin/end template with `args`
/// (`##` → a literal `#`); out-of-range or absent references become empty.
fn substitute_env_args(template: &str, args: &[String]) -> String {
    if !template.contains('#') {
        return template.to_string();
    }
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            // `\#` is a literal hash command, not a parameter marker. Preserve
            // the escaped character as one unit; a doubled backslash naturally
            // leaves a following `#1` available for substitution.
            out.push('\\');
            let ch = template[i + 1..].chars().next().unwrap();
            out.push(ch);
            i += 1 + ch.len_utf8();
            continue;
        }
        if bytes[i] == b'#' && i + 1 < bytes.len() {
            let c = bytes[i + 1];
            if c == b'#' {
                out.push('#');
                i += 2;
                continue;
            }
            if c.is_ascii_digit() {
                let idx = (c - b'0') as usize;
                if idx >= 1 && idx <= args.len() {
                    let arg = &args[idx - 1];
                    if ends_with_control_word(&out)
                        && arg.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                    {
                        out.push(' ');
                    }
                    out.push_str(arg);
                    if ends_with_control_word(arg)
                        && template.as_bytes()[i + 2..]
                            .first()
                            .is_some_and(u8::is_ascii_alphabetic)
                    {
                        out.push(' ');
                    }
                }
                i += 2;
                continue;
            }
        }
        let ch = template[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

const MATH_ENVS: &[&str] = &[
    "equation",
    "equation*",
    "align",
    "align*",
    "gather",
    "gather*",
    "multline",
    "multline*",
    "displaymath",
    "eqnarray",
    "eqnarray*",
    "alignat",
    "alignat*",
    "flalign",
    "flalign*",
    "xalignat",
    "xalignat*",
    "xxalignat",
    "split",
];

/// Environments whose entire body is discarded, like a `%` line comment — the
/// `comment` package's `comment` env (and its common aliases). Matches how
/// LaTeX drops them, rather than showing the body as a muted opaque block.
const SKIP_ENVS: &[&str] = &["comment"];

/// Environments that must keep their body verbatim or feed a dedicated
/// renderer. User `\newenvironment` replacements are checked first, so an
/// explicit preview override can still opt one of these into normal parsing.
///
/// Everything else takes the transparent unsupported-environment path: visible
/// `\begin`/`\end` diagnostics with an ordinarily parsed body.
const SPECIAL_OPAQUE_ENVS: &[&str] = &[
    // Viewer-specialized floats and diagrams.
    "figure",
    "figure*",
    "table",
    "table*",
    "tikzpicture",
    "tikzcd",
    "circuitikz",
    "forest",
    // Structured/package math that the ordinary prose parser cannot preserve
    // and the bundled MathJax configuration does not necessarily implement.
    "dmath",
    "dmath*",
    "dgroup",
    "dgroup*",
    "dseries",
    "dseries*",
    "IEEEeqnarray",
    "IEEEeqnarray*",
    "array",
    "tabular",
    "tabular*",
    "tabularx",
    "longtable",
];

const LITERAL_ENVS: &[&str] = &[
    // Literal/code environments whose contents may intentionally resemble TeX.
    "verbatim",
    "verbatim*",
    "Verbatim",
    "Verbatim*",
    "BVerbatim",
    "LVerbatim",
    "SaveVerbatim",
    "VerbatimOut",
    "VerbatimOut*",
    "alltt",
    "semiverbatim",
    "lstlisting",
    "minted",
    "filecontents",
    "filecontents*",
    "luacode",
    "luacode*",
    "pycode",
    "pyblock",
    "python",
    "python*",
    "sageblock",
    "sagesilent",
    "asy",
    "asydef",
    "tcblisting",
];

fn environment_is_literal(env: &str) -> bool {
    LITERAL_ENVS.contains(&env) || DYNAMIC_LITERAL_ENVS.with(|envs| envs.borrow().contains(env))
}

fn environment_is_literal_with(env: &str, dynamic: &HashSet<String>) -> bool {
    LITERAL_ENVS.contains(&env) || dynamic.contains(env)
}

/// True verbatim/listing environments require their closing command to begin
/// a source line. `alltt` is intentionally excluded: it keeps content opaque
/// in the preview but TeX permits an inline `\end{alltt}`.
fn environment_is_line_delimited_literal(env: &str) -> bool {
    environment_is_literal(env) && env != "alltt"
}

pub(crate) struct InertEnvironmentSpan {
    pub name: String,
    pub body_start: usize,
    pub body_end: usize,
    pub end: usize,
    pub discarded: bool,
}

/// Locate a literal/code or discarded environment beginning at `start`.
/// Callers that retain raw table source use this to keep the entire region
/// inert rather than interpreting comments, citations, conditionals, or row
/// separators inside it.
pub(crate) fn inert_environment_span_at(
    src: &str,
    start: usize,
) -> Option<InertEnvironmentSpan> {
    let token = environment_token_at(src, start)?;
    if token.kind != EnvironmentTokenKind::Begin {
        return None;
    }
    let discarded = SKIP_ENVS.contains(&token.name.as_str());
    if !discarded && !environment_is_literal(&token.name) {
        return None;
    }
    let (body_end, end) = if discarded || environment_is_line_delimited_literal(&token.name) {
        literal_environment_bounds(src, token.end, &token.name)
            .unwrap_or((src.len(), src.len()))
    } else {
        find_matching_end_lexical_in(src, token.end, &token.name)
            .unwrap_or((src.len(), src.len()))
    };
    Some(InertEnvironmentSpan {
        name: token.name,
        body_start: token.end,
        body_end,
        end,
        discarded,
    })
}

fn environment_is_special_opaque(env: &str) -> bool {
    SPECIAL_OPAQUE_ENVS.contains(&env)
}

/// TeX conditional primitives that pair with `\fi`. Used to balance nested
/// conditionals when skipping an `\iffalse … \fi` block. Deliberately excludes
/// package macros that merely look like `\if…` but take no `\fi` (e.g.
/// `\ifthenelse`), so they don't throw off the `\fi` matching.
const IF_OPENERS: &[&str] = &[
    "if",
    "iffalse",
    "iftrue",
    "ifnum",
    "ifdim",
    "ifodd",
    "ifx",
    "ifcat",
    "ifcase",
    "ifmmode",
    "ifvmode",
    "ifhmode",
    "ifinner",
    "ifvoid",
    "ifhbox",
    "ifvbox",
    "ifeof",
    "ifdefined",
    "ifcsname",
    "iffontchar",
];

#[derive(Clone, Copy)]
struct ConditionalBounds {
    else_start: Option<usize>,
    else_end: Option<usize>,
    fi_start: usize,
    fi_end: usize,
}

#[derive(Clone, Copy)]
struct OpenConditional {
    after_word: usize,
    else_start: Option<usize>,
    else_end: Option<usize>,
}

/// Lazily index matching conditional boundaries for one forward scanner.
///
/// Looking up every nested `\iftrue` with `conditional_bounds` rescans the
/// remaining suffix once per nesting level. A single iterative pass records
/// every matched opener instead, keeping deeply nested live branches linear.
/// The index starts at the first conditional the caller encounters, so normal
/// source without conditionals pays no setup cost.
#[derive(Default)]
pub(crate) struct ConditionalLookup {
    bounds: Option<HashMap<usize, ConditionalBounds>>,
}

impl ConditionalLookup {
    fn get(
        &mut self,
        src: &str,
        command_start: usize,
        after_word: usize,
    ) -> Option<ConditionalBounds> {
        let bounds = self
            .bounds
            .get_or_insert_with(|| conditional_bounds_index(src, command_start));
        bounds.get(&after_word).copied()
    }

    pub(crate) fn false_branch_resume(
        &mut self,
        src: &str,
        command_start: usize,
        after_word: usize,
    ) -> usize {
        self.get(src, command_start, after_word)
            .map(|bounds| bounds.else_end.unwrap_or(bounds.fi_end))
            .unwrap_or(src.len())
    }

    pub(crate) fn true_branch_else_range(
        &mut self,
        src: &str,
        command_start: usize,
        after_word: usize,
    ) -> Option<(usize, usize)> {
        let bounds = self.get(src, command_start, after_word)?;
        bounds
            .else_start
            .map(|else_start| (else_start, bounds.fi_end))
    }
}

#[cfg(test)]
thread_local! {
    static CONDITIONAL_SCAN_STEPS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn count_conditional_scan_step() {
    CONDITIONAL_SCAN_STEPS.with(|steps| steps.set(steps.get().saturating_add(1)));
}

#[cfg(not(test))]
#[inline]
fn count_conditional_scan_step() {}

/// Pair every lexically active primitive conditional at or after `start`.
/// Stored definitions, comments, literal/code payloads, and stringified input
/// are skipped exactly as they are by `conditional_bounds`.
fn conditional_bounds_index(src: &str, start: usize) -> HashMap<usize, ConditionalBounds> {
    let bytes = src.as_bytes();
    let mut bounds = HashMap::new();
    let mut stack: Vec<OpenConditional> = Vec::new();
    let mut i = start;
    while i < bytes.len() {
        count_conditional_scan_step();
        if bytes[i] == b'%' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] != b'\\' {
            i += if bytes[i].is_ascii() {
                1
            } else {
                src[i..].chars().next().map_or(1, char::len_utf8)
            };
            continue;
        }

        let name_start = i + 1;
        let mut end = name_start;
        while end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'@') {
            end += 1;
        }
        if end == name_start {
            i = name_start + src[name_start..].chars().next().map_or(0, char::len_utf8);
            continue;
        }
        let name = &src[name_start..end];
        if let Some(declaration_end) = skip_command_macro_declaration(src, i) {
            i = declaration_end;
            continue;
        }
        if is_inline_literal_command(name) {
            i = inline_literal_payload(src, name, end)
                .map(|(_, payload_end)| payload_end)
                .unwrap_or(bytes.len());
            continue;
        }
        if name == "string" {
            i = tex_token_end(src, skip_tex_space_and_comments(src, end));
            continue;
        }
        if matches!(name, "detokenize" | "unexpanded") {
            let group_start = skip_tex_space_and_comments(src, end);
            i = tex_inert_group_end(src, group_start, b'{', b'}').unwrap_or(bytes.len());
            continue;
        }
        if let Some(token) = environment_token_at(src, i) {
            if token.kind == EnvironmentTokenKind::Begin
                && (environment_is_line_delimited_literal(&token.name)
                    || SKIP_ENVS.contains(&token.name.as_str()))
            {
                i = literal_environment_bounds(src, token.end, &token.name)
                    .map(|(_, close_end)| close_end)
                    .unwrap_or(bytes.len());
                continue;
            }
        }

        if IF_OPENERS.contains(&name) {
            stack.push(OpenConditional {
                after_word: end,
                else_start: None,
                else_end: None,
            });
        } else if name == "else" {
            if let Some(open) = stack.last_mut() {
                if open.else_start.is_none() {
                    open.else_start = Some(i);
                    open.else_end = Some(end);
                }
            }
        } else if name == "fi" {
            if let Some(open) = stack.pop() {
                bounds.insert(
                    open.after_word,
                    ConditionalBounds {
                        else_start: open.else_start,
                        else_end: open.else_end,
                        fi_start: i,
                        fi_end: end,
                    },
                );
            }
        }
        i = end;
    }
    bounds
}

/// Locate the top-level `\else` and matching `\fi` for a known primitive
/// conditional. Stored macro bodies, comments, inline literals, and true
/// verbatim/listing environments stay inert while balancing.
fn conditional_bounds(src: &str, mut i: usize) -> Option<ConditionalBounds> {
    let bytes = src.as_bytes();
    let mut depth: i32 = 1;
    let mut else_start = None;
    let mut else_end = None;
    while i < bytes.len() {
        count_conditional_scan_step();
        let b = bytes[i];
        if b == b'%' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'\\' {
            let name_start = i + 1;
            let mut end = name_start;
            while end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'@') {
                end += 1;
            }
            if end == name_start {
                i = (i + 2).min(bytes.len());
                continue;
            }
            let name = &src[name_start..end];
            if let Some(declaration_end) = skip_command_macro_declaration(src, i) {
                i = declaration_end;
                continue;
            }
            if is_inline_literal_command(name) {
                i = inline_literal_payload(src, name, end)
                    .map(|(_, payload_end)| payload_end)
                    .unwrap_or(bytes.len());
                continue;
            }
            if name == "string" {
                i = tex_token_end(src, skip_tex_space_and_comments(src, end));
                continue;
            }
            if matches!(name, "detokenize" | "unexpanded") {
                let group_start = skip_tex_space_and_comments(src, end);
                i = tex_inert_group_end(src, group_start, b'{', b'}').unwrap_or(bytes.len());
                continue;
            }
            if let Some(token) = environment_token_at(src, i) {
                if token.kind == EnvironmentTokenKind::Begin
                    && (environment_is_line_delimited_literal(&token.name)
                        || SKIP_ENVS.contains(&token.name.as_str()))
                {
                    i = literal_environment_bounds(src, token.end, &token.name)
                        .map(|(_, close_end)| close_end)
                        .unwrap_or(bytes.len());
                    continue;
                }
            }
            if IF_OPENERS.contains(&name) {
                depth += 1;
            } else if name == "fi" {
                depth -= 1;
                if depth == 0 {
                    return Some(ConditionalBounds {
                        else_start,
                        else_end,
                        fi_start: i,
                        fi_end: end,
                    });
                }
            } else if name == "else" && depth == 1 && else_start.is_none() {
                else_start = Some(i);
                else_end = Some(end);
            }
            i = end;
            continue;
        }
        i += 1;
    }
    None
}

/// Return where parsing should resume after the hidden branch of an
/// `\iffalse`. A top-level `\else` starts visible input; without one, resume
/// after the matching `\fi`. Nested primitive conditionals stay balanced.
pub(crate) fn false_branch_resume(src: &str, after_word: usize) -> usize {
    conditional_bounds(src, after_word)
        .map(|bounds| bounds.else_end.unwrap_or(bounds.fi_end))
        .unwrap_or(src.len())
}

/// Remove lexically dormant source while retaining executable TeX in order.
/// This is used by lightweight renderers (notably native tables) that operate
/// on retained source rather than a recursively parsed AST.
pub(crate) fn executable_latex_source(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut hidden_ranges = Vec::new();
    let mut conditionals = ConditionalLookup::default();
    let mut out = String::with_capacity(source.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(resume) = scheduled_hidden_resume(i, &mut hidden_ranges) {
            i = resume;
            continue;
        }
        if bytes[i] == b'%' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        if bytes[i] != b'\\' {
            let width = if bytes[i].is_ascii() {
                1
            } else {
                source[i..].chars().next().map_or(1, char::len_utf8)
            };
            out.push_str(&source[i..i + width]);
            i += width;
            continue;
        }

        let command_start = i;
        let word_start = i + 1;
        let mut word_end = word_start;
        while word_end < bytes.len()
            && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
        {
            word_end += 1;
        }
        if word_end == word_start {
            if word_start >= bytes.len() {
                out.push('\\');
                break;
            }
            let width = source[word_start..]
                .chars()
                .next()
                .map_or(1, char::len_utf8);
            out.push_str(&source[command_start..word_start + width]);
            i = word_start + width;
            continue;
        }
        let word = &source[word_start..word_end];

        if word == "begin" {
            if let Some(span) = inert_environment_span_at(source, command_start) {
                if !span.discarded {
                    out.push_str(&source[command_start..span.end]);
                }
                i = span.end;
                continue;
            }
        }
        if word == "iffalse" {
            i = conditionals
                .get(source, command_start, word_end)
                .map(|bounds| bounds.else_end.unwrap_or(bounds.fi_end))
                .unwrap_or(bytes.len());
            continue;
        }
        if word == "iftrue" {
            if let Some(bounds) = conditionals.get(source, command_start, word_end) {
                if let Some(else_start) = bounds.else_start {
                    hidden_ranges.push((else_start, bounds.fi_end));
                }
            }
            i = word_end;
            continue;
        }
        if matches!(word, "else" | "fi") {
            i = word_end;
            continue;
        }
        if let Some(end) = skip_command_macro_declaration(source, command_start) {
            i = end;
            continue;
        }
        if let Some(keyword_end) = environment_keyword_end(source, command_start) {
            i = parse_env_macro(source, keyword_end)
                .map(|(_, _, end)| end)
                .unwrap_or(keyword_end);
            continue;
        }
        if let Some((_, end)) = literal_environment_declaration_at(source, word, word_end) {
            i = end;
            continue;
        }
        if let Some((_, end)) = inline_literal_declaration_at(source, word, word_end) {
            i = end;
            continue;
        }
        if is_inline_literal_command(word) {
            let end = inline_literal_payload(source, word, word_end)
                .map(|(_, end)| end)
                .unwrap_or(bytes.len());
            out.push_str(&source[command_start..end]);
            i = end;
            continue;
        }
        if matches!(word, "detokenize" | "unexpanded") {
            let group_start = skip_tex_space_and_comments(source, word_end);
            let end =
                tex_inert_group_end(source, group_start, b'{', b'}').unwrap_or(bytes.len());
            out.push_str(&source[command_start..end]);
            i = end;
            continue;
        }
        if word == "string" {
            let end = tex_token_end(source, skip_tex_space_and_comments(source, word_end));
            out.push_str(&source[command_start..end]);
            i = end;
            continue;
        }

        out.push_str(&source[command_start..word_end]);
        i = word_end;
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveBracedCommand {
    pub name: String,
    pub value: String,
    pub starred: bool,
}

/// Collect live control-word calls with a final required braced argument,
/// preserving source order while ignoring definitions, comments, literals,
/// stringified input, and inactive conditional branches.
pub(crate) fn live_braced_command_calls(
    source: &str,
    commands: &[&str],
    max_optional_args: usize,
) -> Vec<LiveBracedCommand> {
    let source = executable_latex_source(source);
    let bytes = source.as_bytes();
    let mut calls = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += if bytes[i].is_ascii() {
                1
            } else {
                source[i..].chars().next().map_or(1, char::len_utf8)
            };
            continue;
        }
        let word_start = i + 1;
        let mut word_end = word_start;
        while word_end < bytes.len()
            && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
        {
            word_end += 1;
        }
        if word_end == word_start {
            i = word_start
                + source[word_start..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8);
            continue;
        }
        let word = &source[word_start..word_end];
        if word == "begin" {
            if let Some(span) = inert_environment_span_at(&source, i) {
                i = span.end;
                continue;
            }
        }
        if is_inline_literal_command(word) {
            i = inline_literal_payload(&source, word, word_end)
                .map(|(_, end)| end)
                .unwrap_or(bytes.len());
            continue;
        }
        if word == "string" {
            i = tex_token_end(&source, skip_tex_space_and_comments(&source, word_end));
            continue;
        }
        if matches!(word, "detokenize" | "unexpanded") {
            let group_start = skip_tex_space_and_comments(&source, word_end);
            i = tex_inert_group_end(&source, group_start, b'{', b'}').unwrap_or(bytes.len());
            continue;
        }
        if !commands.contains(&word) {
            i = word_end;
            continue;
        }

        let starred = bytes.get(word_end) == Some(&b'*');
        let mut argument_start = word_end + usize::from(starred);
        argument_start = skip_tex_space_and_comments(&source, argument_start);
        let mut valid = true;
        for _ in 0..max_optional_args {
            if bytes.get(argument_start) != Some(&b'[') {
                break;
            }
            let Some(end) = tex_group_end(&source, argument_start, b'[', b']') else {
                valid = false;
                break;
            };
            argument_start = skip_tex_space_and_comments(&source, end);
        }
        if !valid {
            i = word_end;
            continue;
        }
        if bytes.get(argument_start) != Some(&b'{') {
            i = word_end;
            continue;
        }
        let Some(end) = tex_group_end(&source, argument_start, b'{', b'}') else {
            i = word_end;
            continue;
        };
        calls.push(LiveBracedCommand {
            name: word.to_string(),
            value: source[argument_start + 1..end - 1].to_string(),
            starred,
        });
        i = end;
    }
    calls
}

/// Maximum recognized-environment nesting before we stop recursing and capture
/// the rest as an opaque block. Each recognized container (`center`, theorems,
/// lists, proofs, …) descends with a fresh sub-`Parser` — one stack frame per
/// level — so without a cap, pathologically nested input (`\begin{center}` ×N)
/// would overflow the stack and abort the whole daemon. 64 is far deeper than
/// any real document (LaTeX itself caps list nesting far lower) yet leaves
/// comfortable headroom below the ~128–256-frame overflow point on a 2 MB
/// stack — the daemon parses on a default tokio worker thread.
const MAX_NESTING_DEPTH: u32 = 64;

/// Annotation / callout environments (e.g. review packages like marktext's
/// `todo`, `note`, `added`, …). Rendered as a titled box whose body is parsed
/// *recursively* — so math and nested content inside them work, unlike the
/// opaque fallback. Tuple: (env name, CSS class, default title, whether the
/// optional `[arg]` is a title). For some of these the optional arg is a color
/// or tcolorbox options rather than a title, so we consume but ignore it.
const CALLOUT_ENVS: &[(&str, &str, &str, bool)] = &[
    ("todo", "todo", "TODO", true),
    ("note", "note", "Note", true),
    ("note*", "note", "Note", true),
    ("added", "added", "Added", true),
    ("removed", "removed", "Removed", true),
    ("marked", "marked", "Marked", true),
    ("markedleft", "marked", "Marked", false),
    ("markedright", "marked", "Marked", false),
    ("highlighted", "highlighted", "Highlighted", false),
    ("quoted", "quote", "Quote", false),
];

fn callout_for(env: &str) -> Option<(&'static str, &'static str, bool)> {
    CALLOUT_ENVS
        .iter()
        .find(|(name, ..)| *name == env)
        .map(|(_, class, title, arg_is_title)| (*class, *title, *arg_is_title))
}

/// Parse every project file's body into a flat node list, preserving include
/// order.
pub fn parse_body(project: &Project, thms: &TheoremRegistry) -> Result<Vec<Node>> {
    parse_body_with_overrides(project, thms, &[])
}

/// Parse a project body while also applying preview-only macro override layers.
/// This is the render-path entry point; [`parse_body`] remains the no-overrides
/// convenience used by parser/numbering callers.
pub fn parse_body_with_overrides(
    project: &Project,
    thms: &TheoremRegistry,
    overrides: &[MacroOverride],
) -> Result<Vec<Node>> {
    // User `\newenvironment` definitions for this parse (thread-local; read by
    // parse_environment when it meets an otherwise-unknown environment). Scans
    // the root preamble, its `\input`/`\usepackage`d files, and viewer-only
    // macro override files.
    ENV_MACROS.with(|m| *m.borrow_mut() = env_macros_for_project(project, overrides));
    DYNAMIC_LITERAL_ENVS
        .with(|envs| *envs.borrow_mut() = literal_envs_for_project(project, overrides));
    DYNAMIC_INLINE_LITERAL_COMMANDS
        .with(|commands| *commands.borrow_mut() = literal_commands_for_project(project, overrides));
    ENV_EXPANSIONS_LEFT.with(|budget| budget.set(MAX_USER_ENV_EXPANSIONS));
    ENV_EXPANSION_ACTIVE.with(|active| active.set(0));
    let mut nodes = Vec::new();
    for f in &project.files {
        let mut p = Parser::new_at(&f.source, f.path.clone(), f.start, thms, 0);
        p.parse_block_into(&mut nodes, None);
    }
    Ok(nodes)
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    file: PathBuf,
    byte: usize,
    byte_base: u32,
    start_line: u32,
    start_col: u32,
    line: u32,
    col: u32,
    thms: &'a TheoremRegistry,
    /// Recognized-environment nesting depth, propagated to sub-parsers so the
    /// recursive descent can bail before overflowing the stack.
    depth: u32,
}

impl<'a> Parser<'a> {
    fn new_at(
        src: &'a str,
        file: PathBuf,
        start: Pos,
        thms: &'a TheoremRegistry,
        depth: u32,
    ) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            file,
            byte: 0,
            byte_base: start.byte,
            start_line: start.line,
            start_col: start.col,
            line: start.line,
            col: start.col,
            thms,
            depth,
        }
    }

    fn pos(&self) -> Pos {
        Pos {
            line: self.line,
            col: self.col,
            byte: self.byte_base + self.byte as u32,
        }
    }

    fn span_from(&self, start: Pos) -> Span {
        Span {
            file: self.file.clone(),
            start,
            end: self.pos(),
        }
    }

    fn at_end(&self) -> bool {
        self.byte >= self.bytes.len()
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.byte).copied()
    }

    fn advance(&mut self, n: usize) {
        let target = (self.byte + n).min(self.bytes.len());
        while self.byte < target {
            let b = self.bytes[self.byte];
            self.byte += 1;
            if b == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
    }

    /// Advance to absolute byte offset, updating line/col.
    fn advance_to(&mut self, target: usize) {
        if target <= self.byte {
            return;
        }
        self.advance(target - self.byte);
    }

    fn starts_with(&self, lit: &str) -> bool {
        self.bytes
            .get(self.byte..self.byte + lit.len())
            .map(|s| s == lit.as_bytes())
            .unwrap_or(false)
    }

    fn parse_block_into(&mut self, out: &mut Vec<Node>, stop_env: Option<&str>) {
        let mut text_start: Option<Pos> = None;
        let mut text_buf = String::new();

        // Format the stop-sentinel once before the loop, not on every
        // byte. The previous `format!("\\end{{{env}}}")` inside the
        // hot path allocated a fresh String per iteration — on a 100 KB
        // body with nested environments that's tens of thousands of
        // allocations and was the dominant cost of `parse_body`.
        let stop_sentinel = stop_env.map(|env| format!("\\end{{{env}}}"));

        let flush_text = |text_buf: &mut String,
                          text_start: &mut Option<Pos>,
                          out: &mut Vec<Node>,
                          end: Pos,
                          file: &Path| {
            if let Some(start) = text_start.take() {
                if !text_buf.is_empty()
                    && (text_buf.chars().any(|c| !c.is_whitespace())
                        || contains_blank_line(text_buf))
                {
                    out.push(Node {
                        kind: NodeKind::Text(std::mem::take(text_buf)),
                        span: Span {
                            file: file.to_path_buf(),
                            start,
                            end,
                        },
                        children: vec![],
                    });
                } else {
                    text_buf.clear();
                }
            }
        };

        while !self.at_end() {
            // \end{stop_env}? — caller wants us to stop.
            if let Some(sentinel) = stop_sentinel.as_deref() {
                if self.starts_with(sentinel) {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    return;
                }
            }

            let b = self.bytes[self.byte];

            // Comment to end of line.
            if b == b'%' {
                flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                let start = self.pos();
                let line_end = self.src[self.byte..]
                    .find('\n')
                    .map(|i| self.byte + i)
                    .unwrap_or(self.bytes.len());
                let comment = self.src[self.byte + 1..line_end].to_string();
                self.advance_to(line_end);
                out.push(Node {
                    kind: NodeKind::Comment(comment),
                    span: self.span_from(start),
                    children: vec![],
                });
                continue;
            }

            // A scoped text-color declaration must stay atomic through this
            // parser. Otherwise an inline equation inside the group would
            // split the opening `{\color...` and closing `}` across separate
            // nodes, losing TeX's color scope in the HTML renderer.
            if b == b'{' && self.depth < MAX_NESTING_DEPTH {
                if let Some((model, color, body_start, body_end, end)) =
                    scoped_text_color_group(self.src, self.byte)
                {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    let start = self.pos();
                    self.advance_to(body_start);
                    let child_start = self.pos();
                    let mut children = Vec::new();
                    {
                        let mut sub = Parser::new_at(
                            &self.src[body_start..body_end],
                            self.file.clone(),
                            child_start,
                            self.thms,
                            self.depth + 1,
                        );
                        sub.parse_block_into(&mut children, None);
                    }
                    self.advance_to(end);
                    out.push(Node {
                        kind: NodeKind::TextColor { model, color },
                        span: self.span_from(start),
                        children,
                    });
                    continue;
                }
            }

            // Display math $$...$$ — check before $...$ to avoid swallowing.
            if b == b'$' && self.bytes.get(self.byte + 1) == Some(&b'$') {
                flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                let start = self.pos();
                self.advance(2);
                let body_start = self.byte;
                while !self.at_end() {
                    if self.bytes[self.byte] == b'$' && self.bytes.get(self.byte + 1) == Some(&b'$')
                    {
                        break;
                    }
                    self.advance(1);
                }
                let body = self.src[body_start..self.byte].to_string();
                if !self.at_end() {
                    self.advance(2);
                }
                out.push(Node {
                    kind: NodeKind::DisplayMath {
                        body,
                        env: None,
                        label: None,
                        number: None,
                        row_numbers: Vec::new(),
                    },
                    span: self.span_from(start),
                    children: vec![],
                });
                continue;
            }

            // Inline math $...$
            if b == b'$' {
                flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                let start = self.pos();
                self.advance(1);
                let body_start = self.byte;
                while !self.at_end() {
                    let c = self.bytes[self.byte];
                    if c == b'\\' && self.byte + 1 < self.bytes.len() {
                        self.advance(2);
                        continue;
                    }
                    if c == b'$' {
                        break;
                    }
                    self.advance(1);
                }
                let body = self.src[body_start..self.byte].to_string();
                if !self.at_end() {
                    self.advance(1);
                }
                out.push(Node {
                    kind: NodeKind::InlineMath(body),
                    span: self.span_from(start),
                    children: vec![],
                });
                continue;
            }

            // \( ... \) and \[ ... \]
            if b == b'\\' && self.byte + 1 < self.bytes.len() {
                let next = self.bytes[self.byte + 1];
                if next == b'(' || next == b'[' {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    let display = next == b'[';
                    let closer = if display {
                        (b'\\', b']')
                    } else {
                        (b'\\', b')')
                    };
                    let start = self.pos();
                    self.advance(2);
                    let body_start = self.byte;
                    while self.byte + 1 < self.bytes.len() {
                        if self.bytes[self.byte] == closer.0
                            && self.bytes[self.byte + 1] == closer.1
                        {
                            break;
                        }
                        self.advance(1);
                    }
                    let body = self.src[body_start..self.byte].to_string();
                    if self.byte + 1 < self.bytes.len() {
                        self.advance(2);
                    }
                    out.push(Node {
                        kind: if display {
                            NodeKind::DisplayMath {
                                body,
                                env: None,
                                label: None,
                                number: None,
                                row_numbers: Vec::new(),
                            }
                        } else {
                            NodeKind::InlineMath(body)
                        },
                        span: self.span_from(start),
                        children: vec![],
                    });
                    continue;
                }
            }

            // Backslash: command, environment, or escaped char.
            if b == b'\\' {
                let cmd_name_end = self.command_word_end();
                if cmd_name_end == self.byte + 1 {
                    // \<symbol> — keep as text (treated as opaque punctuation).
                    if text_start.is_none() {
                        text_start = Some(self.pos());
                    }
                    // Slice `\` plus one full following char — the escaped char
                    // may be multibyte (`\é`, `\λ`), and a fixed `+2` byte slice
                    // would split the codepoint and panic. `next_len` is 0 only
                    // for a trailing `\` at end-of-input.
                    let next_len = self.src[self.byte + 1..]
                        .chars()
                        .next()
                        .map_or(0, |c| c.len_utf8());
                    let end = (self.byte + 1 + next_len).min(self.bytes.len());
                    let slice = &self.src[self.byte..end];
                    text_buf.push_str(slice);
                    self.advance(slice.len());
                    continue;
                }
                let cmd = self.src[self.byte + 1..cmd_name_end].to_string();
                let cmd_start = self.pos();

                if is_inline_literal_command(&cmd) {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    if let Some((payload, end)) =
                        inline_literal_payload(self.src, &cmd, cmd_name_end)
                    {
                        self.advance_to(end);
                        out.push(Node {
                            kind: NodeKind::OpaqueCmd {
                                name: "inline-literal".to_string(),
                                raw: payload,
                            },
                            span: self.span_from(cmd_start),
                            children: vec![],
                        });
                    } else {
                        self.advance_to(cmd_name_end);
                    }
                    continue;
                }

                if cmd == "begin" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.parse_environment(out, cmd_start);
                    continue;
                }

                // `\iffalse … \fi` — the "comment out a block" idiom. Skip the
                // whole conditional (its body never renders), like a `%` comment.
                if cmd == "iffalse" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    self.skip_false_conditional();
                    continue;
                }

                // `\iftrue` keeps its first branch and discards the optional
                // `\else` branch. Parse the live slice recursively so hidden
                // boundary-looking tokens cannot leak into the preview.
                if cmd == "iftrue" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    if let Some(bounds) = conditional_bounds(self.src, cmd_name_end) {
                        self.advance_to(cmd_name_end);
                        if self.depth >= MAX_NESTING_DEPTH {
                            self.advance_to(bounds.fi_end);
                            continue;
                        }
                        let visible_end = bounds.else_start.unwrap_or(bounds.fi_start);
                        let mut children = Vec::new();
                        let mut sub = Parser::new_at(
                            &self.src[self.byte..visible_end],
                            self.file.clone(),
                            self.pos(),
                            self.thms,
                            self.depth + 1,
                        );
                        sub.parse_block_into(&mut children, None);
                        self.advance_to(bounds.fi_end);
                        out.extend(children);
                    } else {
                        self.advance_to(cmd_name_end);
                    }
                    continue;
                }

                // Preview-only unwrap of the graphicx box transforms:
                // \resizebox{w}{h}{content} (+ starred), \scalebox{s}[v]{content},
                // \rotatebox[opts]{angle}{content}. Their geometry cannot apply
                // in a reflowing HTML preview, but the CONTENT argument
                // routinely holds a whole tabular
                // (`\resizebox{\textwidth}{!}{\begin{tabular}…}`) which must
                // parse as block content — left in prose it degrades into
                // unsupported-env chips with raw `&`-separated cells. The
                // content renders at natural size and orientation instead.
                if matches!(
                    cmd.trim_end_matches('*'),
                    "resizebox" | "scalebox" | "rotatebox"
                ) {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    // command_word_end already swallowed a trailing `*`
                    // (starred variants size by total height; same unwrap).
                    self.advance_to(cmd_name_end);
                    // TeX argument scanning skips %-comments and accepts
                    // undelimited token args (`\resizebox\columnwidth{!}{…}`),
                    // so use required_macro_arg / skip_tex_argument_space, not
                    // the brace-only, comment-blind readers.
                    match cmd.trim_end_matches('*') {
                        "resizebox" => {
                            let _ = self.required_macro_arg();
                            let _ = self.required_macro_arg();
                        }
                        "scalebox" => {
                            let _ = self.required_macro_arg();
                            self.skip_tex_argument_space();
                            let _ = self.skip_optional_arg();
                        }
                        _ => {
                            self.skip_tex_argument_space();
                            let _ = self.skip_optional_arg();
                            let _ = self.required_macro_arg();
                        }
                    }
                    self.skip_tex_argument_space();
                    let group_start = self.byte;
                    if self.peek_byte() == Some(b'{') {
                        if let Some(group_end) = tex_group_end(self.src, group_start, b'{', b'}') {
                            self.advance(1);
                            if self.depth >= MAX_NESTING_DEPTH {
                                self.advance_to(group_end);
                                continue;
                            }
                            let mut children = Vec::new();
                            let mut sub = Parser::new_at(
                                &self.src[self.byte..group_end - 1],
                                self.file.clone(),
                                self.pos(),
                                self.thms,
                                self.depth + 1,
                            );
                            sub.parse_block_into(&mut children, None);
                            self.advance_to(group_end);
                            out.extend(children);
                        }
                    }
                    continue;
                }

                if cmd == "appendix" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    out.push(Node {
                        kind: NodeKind::Appendix,
                        span: self.span_from(cmd_start),
                        children: vec![],
                    });
                    continue;
                }

                // Sectioning. `command_word_end` includes a trailing star, so
                // normalize the command name while retaining whether this is
                // an unnumbered LaTeX heading (`\section*`, etc.).
                let (section_cmd, adjacent_star) = match cmd.strip_suffix('*') {
                    Some(base) => (base, true),
                    None => (cmd.as_str(), false),
                };
                if let Some(level) = section_level(section_cmd) {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    // TeX discards whitespace after a control word before the
                    // section macro tests for `*`, so `\section *{Title}` and
                    // a comment-continued spelling behave like `\section*`.
                    self.skip_tex_argument_space();
                    let section_starred = adjacent_star || self.peek_byte() == Some(b'*');
                    if !adjacent_star && section_starred {
                        self.advance(1);
                        self.skip_tex_argument_space();
                    }
                    self.skip_optional_arg();
                    let title = self.balanced_brace_arg().unwrap_or_default();
                    let mut node = Node {
                        kind: NodeKind::Section {
                            level,
                            title,
                            label: None,
                            starred: section_starred,
                            number: None,
                        },
                        span: self.span_from(cmd_start),
                        children: vec![],
                    };
                    // If a \label{...} follows immediately, attach it.
                    self.skip_ws_inline();
                    if self.starts_with("\\label") {
                        let lstart = self.pos();
                        self.advance("\\label".len());
                        if let Some(lab) = self.balanced_brace_arg() {
                            if let NodeKind::Section { label, .. } = &mut node.kind {
                                *label = Some(lab);
                            }
                            node.span = self.span_from(node.span.start);
                            let _ = lstart;
                        }
                    }
                    out.push(node);
                    continue;
                }

                // Reference commands.
                if let Some(kind) = RefKind::from_command(&cmd) {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    let key = self.balanced_brace_arg().unwrap_or_default();
                    out.push(Node {
                        kind: NodeKind::Ref { kind, key },
                        span: self.span_from(cmd_start),
                        children: vec![],
                    });
                    continue;
                }

                // Citation commands.
                if matches!(
                    cmd.as_str(),
                    "cite"
                        | "citet"
                        | "citep"
                        | "citeauthor"
                        | "citeyear"
                        | "parencite"
                        | "textcite"
                        | "fullcite"
                ) {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    self.skip_optional_arg();
                    self.skip_optional_arg();
                    let keys_raw = self.balanced_brace_arg().unwrap_or_default();
                    let keys = keys_raw
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    out.push(Node {
                        kind: NodeKind::Cite { keys },
                        span: self.span_from(cmd_start),
                        children: vec![],
                    });
                    continue;
                }

                // \printbibliography (biblatex) or \bibliography{name} (bibtex).
                if cmd == "printbibliography" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    self.skip_optional_arg();
                    out.push(Node {
                        kind: NodeKind::Bibliography,
                        span: self.span_from(cmd_start),
                        children: vec![],
                    });
                    continue;
                }
                if cmd == "bibliography" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    let _ = self.balanced_brace_arg();
                    out.push(Node {
                        kind: NodeKind::Bibliography,
                        span: self.span_from(cmd_start),
                        children: vec![],
                    });
                    continue;
                }
                if cmd == "maketitle" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    out.push(Node {
                        kind: NodeKind::MakeTitle,
                        span: self.span_from(cmd_start),
                        children: vec![],
                    });
                    continue;
                }
                if cmd == "bibliographystyle" || cmd == "nocite" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    let _ = self.balanced_brace_arg();
                    continue;
                }

                // \label outside a known parent — record as opaque so the
                // renderer can index it. (Theorems consume their own labels.)
                if cmd == "label" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    let key = self.balanced_brace_arg().unwrap_or_default();
                    out.push(Node {
                        kind: NodeKind::OpaqueCmd {
                            name: "label".into(),
                            raw: format!("\\label{{{}}}", key),
                        },
                        span: self.span_from(cmd_start),
                        children: vec![],
                    });
                    continue;
                }

                // `letter.cls` permits comments/newlines between these control
                // words and their braced arguments. The generic unknown-command
                // path deliberately stops at `%`, so preserve the full logical
                // call here; `parse_letter` promotes it after parsing the body.
                if matches!(cmd.as_str(), "opening" | "closing") {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    let saved = (self.byte, self.line, self.col);
                    self.skip_tex_argument_space();
                    let raw = if self.peek_byte() == Some(b'{') {
                        self.balanced_brace_arg()
                            .map(|arg| format!(r"\{cmd}{{{arg}}}"))
                    } else {
                        None
                    };
                    if raw.is_none() {
                        self.byte = saved.0;
                        self.line = saved.1;
                        self.col = saved.2;
                    }
                    out.push(Node {
                        kind: NodeKind::OpaqueCmd {
                            name: cmd.clone(),
                            raw: raw.unwrap_or_else(|| format!(r"\{cmd}")),
                        },
                        span: self.span_from(cmd_start),
                        children: vec![],
                    });
                    continue;
                }

                // Stateful font/color switches change the remainder of their
                // enclosing TeX group. Keep them inline in the text buffer so
                // the wrapping `{…}` reaches the renderer as one chunk.
                if matches!(
                    cmd.as_str(),
                    "bf" | "bfseries"
                        | "em"
                        | "it"
                        | "itshape"
                        | "emshape"
                        | "tt"
                        | "ttfamily"
                        | "sc"
                        | "scshape"
                        | "rm"
                        | "rmfamily"
                        | "sf"
                        | "sffamily"
                        | "color"
                        | "normalcolor"
                ) {
                    if text_start.is_none() {
                        text_start = Some(cmd_start);
                    }
                    text_buf.push('\\');
                    text_buf.push_str(&cmd);
                    self.advance_to(cmd_name_end);
                    continue;
                }

                // Unknown command — opaque, but keep the args so the renderer
                // could surface them. For now, take the raw token slice.
                flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                self.advance_to(cmd_name_end);
                let mut raw = format!("\\{}", cmd);
                // Drag in trailing args until we hit non-arg content.
                loop {
                    let saved = self.byte;
                    self.skip_ws_inline();
                    let after_ws = self.byte;
                    if let Some(o) = self.optional_arg_raw() {
                        raw.push_str(&o);
                        continue;
                    }
                    if let Some(g) = self.brace_group_raw() {
                        raw.push_str(&g);
                        continue;
                    }
                    // Restore positional whitespace so it doesn't get eaten.
                    if after_ws != saved {
                        // Rewind not supported simply; just stop. Whitespace
                        // gets resumed below.
                        self.byte = saved;
                        self.line = self.pos_line_at(saved);
                        self.col = self.pos_col_at(saved);
                    }
                    break;
                }
                out.push(Node {
                    kind: NodeKind::OpaqueCmd { name: cmd, raw },
                    span: self.span_from(cmd_start),
                    children: vec![],
                });
                continue;
            }

            // Default: append to text buffer.
            if text_start.is_none() {
                text_start = Some(self.pos());
            }
            if b.is_ascii() {
                text_buf.push(b as char);
                self.advance(1);
            } else {
                let ch = self.src[self.byte..].chars().next().unwrap_or('\0');
                text_buf.push(ch);
                self.advance(ch.len_utf8());
            }
        }

        flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
    }

    fn parse_environment(&mut self, out: &mut Vec<Node>, start: Pos) {
        // self.byte is on '\'; consume "\begin"
        self.advance("\\begin".len());
        // TeX permits whitespace and `%` comments between a control word and
        // its argument. Keep the main parser in step with the lexical matcher
        // so `\begin% comment\n{env}` takes the same environment path.
        self.advance_to(skip_tex_space_and_comments(self.src, self.byte));
        let env = match self.balanced_brace_arg() {
            Some(e) => e.trim().to_string(),
            None => return,
        };

        if env == "math" {
            let inner_start = self.byte;
            let body_end = self.find_matching_end(&env);
            let body = self.src[inner_start..body_end].to_string();
            self.advance_to(body_end);
            self.advance("\\end{math}".len());
            out.push(Node {
                kind: NodeKind::InlineMath(body),
                span: self.span_from(start),
                children: vec![],
            });
            return;
        }

        // `empheq` decorates another display environment. Ignore its visual
        // box/options but preserve and number the underlying math environment.
        if env == "empheq" {
            self.skip_ws_inline();
            self.skip_optional_arg();
            self.skip_ws_inline();
            let target = self
                .balanced_brace_arg()
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "align".to_string());
            let inner_start = self.byte;
            let body_end = self.find_matching_end(&env);
            let body = self.src[inner_start..body_end].to_string();
            let label = extract_label(&body);
            self.advance_to(body_end);
            self.advance("\\end{empheq}".len());
            out.push(Node {
                kind: NodeKind::DisplayMath {
                    body,
                    env: Some(target),
                    label,
                    number: None,
                    row_numbers: Vec::new(),
                },
                span: self.span_from(start),
                children: vec![],
            });
            return;
        }

        // Math environments — capture body verbatim.
        if MATH_ENVS.contains(&env.as_str()) {
            let inner_start = self.byte;
            let close = format!("\\end{{{env}}}");
            let body_end = self.find_matching_end(&env);
            let body = self.src[inner_start..body_end].to_string();
            let label = extract_label(&body);
            self.advance_to(body_end);
            self.advance(close.len());
            out.push(Node {
                kind: NodeKind::DisplayMath {
                    body,
                    env: Some(env),
                    label,
                    number: None,
                    row_numbers: Vec::new(),
                },
                span: self.span_from(start),
                children: vec![],
            });
            return;
        }

        // Bound recursion: past the nesting cap, capture the environment as an
        // opaque block rather than descending with another sub-parser, which
        // would add a stack frame per level and eventually abort the process.
        if self.depth >= MAX_NESTING_DEPTH {
            self.capture_opaque_env(out, start, env);
            return;
        }

        // Discarded environments (the `comment` package): consume the body and
        // emit nothing, like a `%` comment — don't render it as an opaque block.
        if SKIP_ENVS.contains(&env.as_str()) {
            let end_after = literal_environment_bounds(self.src, self.byte, &env)
                .map(|(_, end_after)| end_after)
                .unwrap_or(self.bytes.len());
            self.advance_to(end_after);
            return;
        }

        // Theorem-like with optional [role=...]{name}. Match the exact
        // environment name declared by `\newtheorem`: declaring `lemma` does
        // not implicitly define a separate `lemma*` environment.
        if self.thms.is_theorem(&env) {
            self.parse_theorem(out, start, env);
            return;
        }

        if env == "proof" {
            self.parse_proof(out, start);
            return;
        }

        if let Some((class, default_title, arg_is_title)) = callout_for(&env) {
            self.parse_callout(out, start, env, class, default_title, arg_is_title);
            return;
        }

        if let Some(kind) = list_kind_for(&env) {
            self.parse_list(out, start, env, kind);
            return;
        }

        if env == "abstract" {
            self.parse_abstract(out, start, env);
            return;
        }

        if env == "proofsteps" {
            self.parse_counter_env(out, start, env, "restartsteps");
            return;
        }

        if env == "proofcases" {
            self.parse_counter_env(out, start, env, "restartcases");
            return;
        }

        if env == "subequations" {
            self.parse_subequations(out, start, env);
            return;
        }

        if env == "quote" || env == "quotation" {
            self.parse_quote(out, start, env);
            return;
        }

        let alignment = match env.as_str() {
            "center" => Some(TextAlignment::Center),
            "flushleft" => Some(TextAlignment::FlushLeft),
            "flushright" => Some(TextAlignment::FlushRight),
            _ => None,
        };
        if let Some(kind) = alignment {
            self.parse_alignment(out, start, env, kind);
            return;
        }

        if env == "document" {
            // Defensive: someone slipped a nested document env. Treat as a
            // transparent block.
            self.parse_transparent_env(out, env);
            return;
        }

        // Literal/code environments are safety boundaries even if the source
        // also declares an environment replacement with the same name.
        if environment_is_literal(&env) {
            self.capture_opaque_env(out, start, env);
            return;
        }

        // User-defined environment (`\newenvironment`): expand to its begin/end
        // code around the body and parse THAT, so the body's math/refs render and
        // the wrapper (e.g. `\begin{quote}\itshape`) is honored — instead of the
        // body being dumped verbatim as an opaque block.
        let user_def = ENV_MACROS.with(|m| m.borrow().get(&env).cloned());
        if let Some(def) = user_def {
            let outermost = ENV_EXPANSION_ACTIVE.with(|active| active.get() == 0);
            if outermost {
                ENV_EXPANSIONS_LEFT.with(|budget| budget.set(MAX_USER_ENV_EXPANSIONS));
            }
            let has_budget = ENV_EXPANSIONS_LEFT.with(|budget| {
                let remaining = budget.get();
                if remaining == 0 {
                    false
                } else {
                    budget.set(remaining - 1);
                    true
                }
            });
            if !has_budget {
                self.capture_opaque_env(out, start, env);
                return;
            }
            ENV_EXPANSION_ACTIVE.with(|active| active.set(active.get() + 1));
            self.parse_user_env(out, start, &env, &def);
            ENV_EXPANSION_ACTIVE.with(|active| active.set(active.get().saturating_sub(1)));
            return;
        }

        // `letter.cls` has meaningful page geometry that a flattened
        // `\newenvironment` approximation cannot reproduce. Keep explicit
        // preview overrides authoritative; otherwise retain a native letter
        // container and parse its message body normally.
        if env == "letter" {
            self.parse_letter(out, start, env);
            return;
        }

        if environment_is_special_opaque(&env) {
            self.capture_opaque_env(out, start, env);
            return;
        }

        // Unknown but non-literal environments are transparent diagnostics:
        // show their unsupported boundaries while parsing their contents like
        // ordinary body TeX. Flattening the parsed children between two marker
        // nodes keeps large wrappers (for example `questions`) incrementally
        // patchable instead of turning the whole document into one block.
        self.parse_unsupported_env(out, start, env);
    }

    /// Expand a `\newenvironment` at `\begin{env}`: read its arguments, splice
    /// the body into `begin + body + end`, and parse that transparently (its
    /// nodes flatten into `out`). Because the begin/end code is trimmed of
    /// surrounding whitespace, single-line wrappers (the common case) leave the
    /// body's line/col — hence source-jump — intact; a wrapper with interior
    /// newlines only shifts positions, it never drops content.
    fn parse_user_env(&mut self, out: &mut Vec<Node>, start: Pos, env: &str, def: &EnvMacro) {
        let mut args: Vec<String> = Vec::new();
        if def.nargs > 0 {
            let mut remaining = def.nargs;
            if def.default.is_some() {
                self.skip_tex_argument_space();
                let a = self
                    .optional_arg()
                    .or_else(|| def.default.clone())
                    .unwrap_or_default();
                args.push(a);
                remaining = remaining.saturating_sub(1);
            }
            for _ in 0..remaining {
                args.push(self.required_macro_arg().unwrap_or_default());
            }
        }
        let (body_end, end_after) = self
            .find_matching_end_lexical(env)
            .unwrap_or((self.bytes.len(), self.bytes.len()));
        let body_src = &self.src[self.byte..body_end];
        let begin = substitute_env_args(&def.begin, &args);
        let end = substitute_env_args(&def.end, &args);
        // Keep a token boundary at each seam: if the earlier piece ends in a
        // control word and the next starts with a letter, splice a space (TeX
        // consumes a space after a control word, so semantics are unchanged) —
        // otherwise `\itshape` + `Hello` would re-parse as `\itshapeHello` and
        // the body text would vanish. Only same-line columns shift by 1.
        let starts_alpha = |s: &str| s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
        let sep1 = if ends_with_control_word(&begin) && starts_alpha(body_src) {
            " "
        } else {
            ""
        };
        let sep2 = if ends_with_control_word(body_src) && starts_alpha(&end) {
            " "
        } else {
            ""
        };
        let expanded = format!("{begin}{sep1}{body_src}{sep2}{end}");
        let mut children = Vec::new();
        {
            let mut sub = Parser::new_at(
                &expanded,
                self.file.clone(),
                start,
                self.thms,
                self.depth + 1,
            );
            sub.parse_block_into(&mut children, None);
        }
        self.advance_to(end_after);
        out.extend(children);
    }

    /// Capture an environment's body verbatim as an `OpaqueEnv` without
    /// descending into it. Used both for genuinely opaque environments and as
    /// the depth-cap fallback for recognized-but-too-deeply-nested ones.
    fn capture_opaque_env(&mut self, out: &mut Vec<Node>, start: Pos, env: String) {
        let (body_end, end_after) = if environment_is_line_delimited_literal(&env) {
            literal_environment_bounds(self.src, self.byte, &env)
                .unwrap_or((self.bytes.len(), self.bytes.len()))
        } else {
            self.find_matching_end_lexical(&env)
                .unwrap_or((self.bytes.len(), self.bytes.len()))
        };
        let body = self.src[self.byte..body_end].to_string();
        self.advance_to(end_after);
        out.push(Node {
            kind: NodeKind::OpaqueEnv { env, body },
            span: self.span_from(start),
            children: vec![],
        });
    }

    /// Parse an otherwise-unsupported environment transparently. Its body
    /// nodes are flattened between visible boundary diagnostics, preserving
    /// normal math/reference rendering and fine-grained live patches.
    fn parse_unsupported_env(&mut self, out: &mut Vec<Node>, start: Pos, env: String) {
        let begin_end = self.pos();
        let matching_end = self.find_matching_end_lexical(&env);
        out.push(Node {
            kind: NodeKind::UnsupportedEnvBoundary {
                env: env.clone(),
                boundary: EnvironmentBoundary::Begin,
            },
            span: Span {
                file: self.file.clone(),
                start,
                end: begin_end,
            },
            children: vec![],
        });

        let Some((body_end, end_after)) = matching_end else {
            // During a transient/malformed edit, do not recursively interpret
            // the rest of the file (especially literal TikZ/code) as the
            // environment body. Keep it visible but inert, then flag the
            // missing closer at EOF.
            let body_start = self.pos();
            let body = self.src[self.byte..].to_string();
            self.advance_to(self.bytes.len());
            out.push(Node {
                kind: NodeKind::OpaqueEnv {
                    env: env.clone(),
                    body,
                },
                span: Span {
                    file: self.file.clone(),
                    start: body_start,
                    end: self.pos(),
                },
                children: vec![],
            });
            out.push(Node {
                kind: NodeKind::UnsupportedEnvBoundary {
                    env,
                    boundary: EnvironmentBoundary::MissingEnd,
                },
                span: Span {
                    file: self.file.clone(),
                    start: self.pos(),
                    end: self.pos(),
                },
                children: vec![],
            });
            return;
        };

        let inner_src = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            inner_src,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);
        out.extend(children);

        self.advance_to(body_end);
        let end_start = self.pos();
        self.advance_to(end_after);
        out.push(Node {
            kind: NodeKind::UnsupportedEnvBoundary {
                env,
                boundary: EnvironmentBoundary::End,
            },
            span: Span {
                file: self.file.clone(),
                start: end_start,
                end: self.pos(),
            },
            children: vec![],
        });
    }

    /// `quote` / `quotation`: parse the body into children (so math, refs, and
    /// nested blocks render) and keep a `Quote` wrapper node for the renderer to
    /// emit as a `<blockquote>`. Mirrors `parse_transparent_env` but, unlike it,
    /// retains a containing node rather than flattening the children.
    fn parse_quote(&mut self, out: &mut Vec<Node>, start: Pos, env: String) {
        let body_end = self.find_matching_end(&env);
        let inner_src = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            inner_src,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);
        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());
        out.push(Node {
            kind: NodeKind::Quote { env },
            span: self.span_from(start),
            children,
        });
    }

    /// Parse TeX alignment environments recursively while retaining a wrapper
    /// that lets the renderer reproduce their centered or ragged alignment.
    fn parse_alignment(
        &mut self,
        out: &mut Vec<Node>,
        start: Pos,
        env: String,
        kind: TextAlignment,
    ) {
        let body_end = self.find_matching_end(&env);
        let inner_src = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            inner_src,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);
        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());
        out.push(Node {
            kind: NodeKind::Alignment { kind },
            span: self.span_from(start),
            children,
        });
    }

    fn parse_transparent_env(&mut self, out: &mut Vec<Node>, env: String) {
        let body_end = self.find_matching_end(&env);
        let inner_src = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            inner_src,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);
        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());
        out.extend(children);
    }

    fn parse_subequations(&mut self, out: &mut Vec<Node>, start: Pos, env: String) {
        let body_end = self.find_matching_end(&env);
        let inner_src = &self.src[self.byte..body_end];
        let (label, child_src) = match initial_label_span(inner_src) {
            Some((label, label_start, label_end)) => {
                let mut child_src = inner_src.to_string();
                child_src
                    .replace_range(label_start..label_end, &" ".repeat(label_end - label_start));
                (Some(label), child_src)
            }
            None => (None, inner_src.to_string()),
        };

        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            &child_src,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);
        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());
        out.push(Node {
            kind: NodeKind::Subequations {
                label,
                number: None,
            },
            span: self.span_from(start),
            children,
        });
    }

    fn parse_abstract(&mut self, out: &mut Vec<Node>, start: Pos, env: String) {
        let body_end = self.find_matching_end(&env);
        let inner_src = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            inner_src,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);
        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());
        out.push(Node {
            kind: NodeKind::Abstract,
            span: self.span_from(start),
            children,
        });
    }

    fn parse_letter(&mut self, out: &mut Vec<Node>, start: Pos, env: String) {
        // A half-typed `\begin{letter}` must not consume `\opening` as an
        // undelimited recipient. The class requires a braced address, so only
        // take that exact shape and leave any other token in the body.
        self.skip_tex_argument_space();
        let recipient = if self.peek_byte() == Some(b'{') {
            self.balanced_brace_arg().unwrap_or_default()
        } else {
            String::new()
        };

        let (body_end, end_after) = self
            .find_matching_end_lexical(&env)
            .unwrap_or((self.bytes.len(), self.bytes.len()));
        let inner_src = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            inner_src,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);
        promote_letter_commands(&mut children);

        self.advance_to(end_after);
        out.push(Node {
            kind: NodeKind::Letter { recipient },
            span: self.span_from(start),
            children,
        });
    }

    fn parse_counter_env(
        &mut self,
        out: &mut Vec<Node>,
        start: Pos,
        env: String,
        reset_name: &str,
    ) {
        let body_end = self.find_matching_end(&env);
        let inner_src = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            inner_src,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);
        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());
        out.push(Node {
            kind: NodeKind::OpaqueCmd {
                name: reset_name.to_string(),
                raw: format!("\\{reset_name}"),
            },
            span: self.span_from(start),
            children: vec![],
        });
        out.extend(children);
    }

    fn parse_theorem(&mut self, out: &mut Vec<Node>, start: Pos, env: String) {
        // Look for [role=...] and {name} in either order; spec is `[role=…]{name}`.
        let mut role = Role::Standard;
        let mut name: Option<String> = None;
        loop {
            self.skip_ws_inline();
            if let Some(opt) = self.optional_arg() {
                // Parse `role=foo` or `name=bar`.
                for kv in opt.split(',') {
                    let kv = kv.trim();
                    if let Some(rest) = kv.strip_prefix("role=") {
                        role = Role::parse(rest);
                    } else if let Some(rest) = kv.strip_prefix("name=") {
                        name = Some(rest.to_string());
                    } else if !kv.is_empty() && name.is_none() && !kv.contains('=') {
                        // Plain `[Title]` form some users write.
                        name = Some(kv.to_string());
                    }
                }
                continue;
            }
            if let Some(arg) = self.balanced_brace_arg() {
                if name.is_none() {
                    name = Some(arg);
                } else {
                    break;
                }
                continue;
            }
            break;
        }

        let body_end = self.find_matching_end(&env);
        let inner_src = &self.src[self.byte..body_end];

        // Parse the body before choosing the theorem's label. A raw-text scan
        // cannot distinguish a theorem label from one owned by a nested
        // equation (or another numbered environment).
        let omit_ref = if matches!(role, Role::Omitted) {
            find_first_omitref(inner_src)
        } else {
            None
        };

        let mut children = Vec::new();
        // Parse inner content with a sub-parser so nested commands work.
        let mut sub = Parser::new_at(
            inner_src,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);
        // Only a label parsed directly in the theorem body belongs to its
        // outer box. Nested labels remain on their child nodes. Remove the
        // owned command so it does not also render as a loose label chip.
        let label = take_first_direct_label(&mut children);

        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());

        let kind_word = self.thms.title(&env);
        out.push(Node {
            kind: NodeKind::Theorem {
                env,
                kind_word,
                role,
                name,
                label,
                number: None,
                omit_ref,
            },
            span: self.span_from(start),
            children,
        });
    }

    fn parse_list(&mut self, out: &mut Vec<Node>, start: Pos, env: String, kind: ListKind) {
        let body_end = self.find_matching_end(&env);
        let inner = &self.src[self.byte..body_end];
        let item_chunks = split_list_items(inner);

        // Build ListItem children. We track the parser's position as we
        // descend into the inner slice so child nodes get realistic spans.
        let mut list_children = Vec::new();
        for (marker, slice_start, slice_end) in item_chunks {
            let item_slice = &inner[slice_start..slice_end];
            let mut item_children = Vec::new();
            let mut sub = Parser::new_at(
                item_slice,
                self.file.clone(),
                self.pos_at_byte(self.byte + slice_start),
                self.thms,
                self.depth + 1,
            );
            sub.parse_block_into(&mut item_children, None);
            list_children.push(Node {
                kind: NodeKind::ListItem { marker },
                span: self.span_from(start),
                children: item_children,
            });
        }

        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());

        out.push(Node {
            kind: NodeKind::List { kind },
            span: self.span_from(start),
            children: list_children,
        });
    }

    fn parse_proof(&mut self, out: &mut Vec<Node>, start: Pos) {
        self.skip_ws_inline();
        let (of, role) = parse_proof_option(self.optional_arg());

        let body_end = self.find_matching_end("proof");
        let inner = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            inner,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);

        self.advance_to(body_end);
        self.advance("\\end{proof}".len());

        out.push(Node {
            kind: NodeKind::Proof { of, role },
            span: self.span_from(start),
            children,
        });
    }

    /// Parse a callout/annotation environment (`\begin{todo}[title] … \end`).
    /// Captures the optional `[arg]` (used as the title when it's a title for
    /// this env), then sub-parses the body so math and nested content render.
    fn parse_callout(
        &mut self,
        out: &mut Vec<Node>,
        start: Pos,
        env: String,
        class: &str,
        default_title: &str,
        arg_is_title: bool,
    ) {
        self.skip_ws_inline();
        // Always consume any leading `[...]` so it isn't left in the body, but
        // only use it as the title when this env's optional arg is a title (for
        // others it's a color / tcolorbox options).
        let arg = self.optional_arg();
        let title = if arg_is_title {
            arg.map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| default_title.to_string())
        } else {
            default_title.to_string()
        };

        let body_end = self.find_matching_end(&env);
        let inner = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(
            inner,
            self.file.clone(),
            self.pos(),
            self.thms,
            self.depth + 1,
        );
        sub.parse_block_into(&mut children, None);

        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());

        out.push(Node {
            kind: NodeKind::Callout {
                env,
                class: class.to_string(),
                title: Some(title),
            },
            span: self.span_from(start),
            children,
        });
    }

    /// Find the end byte of the matching `\end{env}` from `self.byte`,
    /// respecting nested `\begin{env}` / `\end{env}` of the same env name.
    /// Returns the byte index of `\end`. If unmatched, returns end-of-source.
    /// Skip a `\iffalse … \fi` block (self.byte must be just past `\iffalse`).
    /// Nested TeX conditionals are balanced so an inner `\iffalse`/`\iftrue`/…
    /// doesn't let the wrong `\fi` close the block. A top-level `\else` switches
    /// to the false branch, which DOES render, so we resume parsing there (the
    /// trailing `\fi` is then an inert unknown control word). Only known
    /// conditional primitives count as nested openers — an unknown `\newif`
    /// conditional at worst stops the skip early (showing a little) rather than
    /// over-skipping real content.
    fn skip_false_conditional(&mut self) {
        self.advance_to(false_branch_resume(self.src, self.byte));
    }

    /// Find a trustworthy matching end while ignoring inert TeX source.
    fn find_matching_end_lexical(&self, env: &str) -> Option<(usize, usize)> {
        find_matching_end_lexical_in(self.src, self.byte, env)
    }

    fn find_matching_end(&self, env: &str) -> usize {
        self.find_matching_end_lexical(env)
            .map(|(start, _)| start)
            .unwrap_or(self.bytes.len())
    }

    fn command_word_end(&self) -> usize {
        let mut i = self.byte + 1;
        while i < self.bytes.len() {
            let c = self.bytes[i];
            if c.is_ascii_alphabetic() || c == b'*' || c == b'@' {
                i += 1;
            } else {
                break;
            }
        }
        if i == self.byte + 1 {
            // Single-char command like `\\` or `\,`. Caller handles this.
            return self.byte + 1;
        }
        i
    }

    fn skip_ws_inline(&mut self) {
        while !self.at_end() {
            let b = self.bytes[self.byte];
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.advance(1);
            } else {
                break;
            }
        }
    }

    /// TeX removes comments before macro argument scanning. Skip whitespace
    /// and any `%...newline` runs so a continued environment invocation takes
    /// its real next token as the argument; `\%` is unaffected because the
    /// current byte is then a backslash.
    fn skip_tex_argument_space(&mut self) {
        loop {
            self.skip_ws_inline();
            if self.peek_byte() != Some(b'%') {
                break;
            }
            while !self.at_end() && self.peek_byte() != Some(b'\n') {
                self.advance(1);
            }
        }
    }

    fn skip_optional_arg(&mut self) -> Option<String> {
        self.optional_arg()
    }

    fn optional_arg(&mut self) -> Option<String> {
        self.skip_ws_inline();
        if self.peek_byte() != Some(b'[') {
            return None;
        }
        let start = self.byte;
        let mut i = self.byte + 1;
        let mut depth: i32 = 0;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b']' if depth == 0 => {
                    let inside = self.src[start + 1..i].to_string();
                    self.advance_to(i + 1);
                    return Some(inside);
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

    fn optional_arg_raw(&mut self) -> Option<String> {
        let start = self.byte;
        let inside = self.optional_arg()?;
        Some(format!("[{}]", inside)).map(|_| self.src[start..self.byte].to_string())
    }

    fn balanced_brace_arg(&mut self) -> Option<String> {
        self.skip_ws_inline();
        if self.peek_byte() != Some(b'{') {
            return None;
        }
        let start = self.byte;
        let end = tex_group_end(self.src, start, b'{', b'}')?;
        let inside = self.src[start + 1..end - 1].to_string();
        self.advance_to(end);
        Some(inside)
    }

    /// Read one undelimited TeX macro argument: a balanced braced group, one
    /// control-sequence token, or one character token. Whitespace before an
    /// undelimited argument is ignored.
    fn required_macro_arg(&mut self) -> Option<String> {
        self.skip_tex_argument_space();
        if self.peek_byte() == Some(b'{') {
            return self.balanced_brace_arg();
        }
        if self.at_end() {
            return None;
        }
        let start = self.byte;
        if self.peek_byte() == Some(b'\\') {
            let mut end = start + 1;
            if self
                .bytes
                .get(end)
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'@')
            {
                while end < self.bytes.len()
                    && (self.bytes[end].is_ascii_alphabetic() || self.bytes[end] == b'@')
                {
                    end += 1;
                }
            } else if end < self.bytes.len() {
                let ch = self.src[end..].chars().next().unwrap();
                end += ch.len_utf8();
            }
            self.advance_to(end);
            return Some(self.src[start..end].to_string());
        }
        let ch = self.src[start..].chars().next().unwrap();
        self.advance(ch.len_utf8());
        Some(ch.to_string())
    }

    fn brace_group_raw(&mut self) -> Option<String> {
        let start = self.byte;
        let _ = self.balanced_brace_arg()?;
        Some(self.src[start..self.byte].to_string())
    }

    /// Recompute line at a given byte offset by scanning from start of file.
    /// Only used after a rare backtrack — O(n) but called at most once per
    /// unknown command.
    fn pos_line_at(&self, byte: usize) -> u32 {
        self.pos_at_byte(byte).line
    }

    fn pos_col_at(&self, byte: usize) -> u32 {
        self.pos_at_byte(byte).col
    }

    fn pos_at_byte(&self, byte: usize) -> Pos {
        let mut line = self.start_line;
        let mut col = self.start_col;
        for source_byte in self.src.as_bytes()[..byte.min(self.bytes.len())]
            .iter()
            .copied()
        {
            if source_byte == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        Pos {
            line,
            col,
            byte: self.byte_base + byte as u32,
        }
    }
}

pub(crate) fn scheduled_hidden_resume(
    i: usize,
    hidden_ranges: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    while hidden_ranges.last().is_some_and(|(_, end)| *end <= i) {
        hidden_ranges.pop();
    }
    let (start, end) = *hidden_ranges.last()?;
    (i >= start).then(|| {
        hidden_ranges.pop();
        end
    })
}

fn find_matching_end_lexical_in(src: &str, from: usize, env: &str) -> Option<(usize, usize)> {
    find_matching_end_lexical_inner(src, from, env, 0)
}

/// Return the byte after a lexically live matching `\end{env}` only when that
/// closer is at or before `limit`. This keeps external live-edit guards in
/// lockstep with the parser's handling of comments, definitions, conditionals,
/// and stringified commands.
pub fn matching_environment_end_before(
    src: &str,
    from: usize,
    env: &str,
    limit: usize,
) -> Option<usize> {
    find_matching_end_lexical_in(src, from, env)
        .map(|(_, end_after)| end_after)
        .filter(|end_after| *end_after <= limit.min(src.len()))
}

fn find_matching_end_lexical_inner(
    src: &str,
    from: usize,
    env: &str,
    recursion: u32,
) -> Option<(usize, usize)> {
    if recursion > MAX_NESTING_DEPTH {
        return None;
    }
    let bytes = src.as_bytes();
    let mut depth = 1u32;
    let mut hidden_ranges: Vec<(usize, usize)> = Vec::new();
    let mut conditionals = ConditionalLookup::default();
    let mut i = from;
    while i < bytes.len() {
        if let Some(resume) = scheduled_hidden_resume(i, &mut hidden_ranges) {
            i = resume;
            continue;
        }
        match bytes[i] {
            b'%' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'\\' => {
                let word_start = i + 1;
                let mut word_end = word_start;
                while word_end < bytes.len()
                    && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
                {
                    word_end += 1;
                }
                if word_end == word_start {
                    i = (i + 2).min(bytes.len());
                    continue;
                }
                let word = &src[word_start..word_end];
                if word == "iffalse" {
                    i = conditionals
                        .get(src, i, word_end)
                        .map(|bounds| bounds.else_end.unwrap_or(bounds.fi_end))
                        .unwrap_or(bytes.len());
                    continue;
                }
                if word == "iftrue" {
                    if let Some(bounds) = conditionals.get(src, i, word_end) {
                        if let Some(else_start) = bounds.else_start {
                            hidden_ranges.push((else_start, bounds.fi_end));
                        }
                    }
                    i = word_end;
                    continue;
                }
                if let Some(end) = skip_command_macro_declaration(src, i) {
                    i = end;
                    continue;
                }
                if let Some(keyword_end) = environment_keyword_end(src, i) {
                    i = parse_env_macro(src, keyword_end)
                        .map(|(_, _, end)| end)
                        .unwrap_or(keyword_end);
                    continue;
                }
                if is_inline_literal_command(word) {
                    i = inline_literal_payload(src, word, word_end)
                        .map(|(_, end)| end)
                        .unwrap_or(bytes.len());
                    continue;
                }
                if word == "string" {
                    i = tex_token_end(src, skip_tex_space_and_comments(src, word_end));
                    continue;
                }
                if matches!(word, "detokenize" | "unexpanded") {
                    i = read_braced(src, skip_tex_space_and_comments(src, word_end))
                        .map(|(_, end)| end)
                        .unwrap_or(bytes.len());
                    continue;
                }
                let Some(token) = environment_token_at(src, i) else {
                    i = word_end;
                    continue;
                };
                match token.kind {
                    EnvironmentTokenKind::Begin if token.name == env => {
                        depth += 1;
                    }
                    EnvironmentTokenKind::Begin
                        if environment_is_line_delimited_literal(&token.name)
                            || SKIP_ENVS.contains(&token.name.as_str()) =>
                    {
                        i = literal_environment_bounds(src, token.end, &token.name)
                            .map(|(_, end_after)| end_after)
                            .unwrap_or(bytes.len());
                        continue;
                    }
                    EnvironmentTokenKind::Begin if environment_is_literal(&token.name) => {
                        i = find_matching_end_lexical_inner(
                            src,
                            token.end,
                            &token.name,
                            recursion + 1,
                        )
                        .map(|(_, end_after)| end_after)
                        .unwrap_or(bytes.len());
                        continue;
                    }
                    EnvironmentTokenKind::End if token.name == env => {
                        depth -= 1;
                        if depth == 0 {
                            return Some((i, token.end));
                        }
                    }
                    _ => {}
                }
                i = token.end;
            }
            _ => i += 1,
        }
    }
    None
}

/// Find the first live environment from `candidates`, skipping comments,
/// stored macro bodies, false conditional branches, and literal/code input.
/// Used by the float renderer to locate a real nested diagram.
pub(crate) fn first_supported_environment(
    src: &str,
    candidates: &[&str],
) -> Option<(String, String)> {
    let bytes = src.as_bytes();
    let mut hidden_ranges: Vec<(usize, usize)> = Vec::new();
    let mut conditionals = ConditionalLookup::default();
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(resume) = scheduled_hidden_resume(i, &mut hidden_ranges) {
            i = resume;
            continue;
        }
        match bytes[i] {
            b'%' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'\\' => {
                let word_start = i + 1;
                let mut word_end = word_start;
                while word_end < bytes.len()
                    && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
                {
                    word_end += 1;
                }
                if word_end == word_start {
                    i = (i + 2).min(bytes.len());
                    continue;
                }
                let word = &src[word_start..word_end];
                if word == "iffalse" {
                    i = conditionals
                        .get(src, i, word_end)
                        .map(|bounds| bounds.else_end.unwrap_or(bounds.fi_end))
                        .unwrap_or(bytes.len());
                    continue;
                }
                if word == "iftrue" {
                    if let Some(bounds) = conditionals.get(src, i, word_end) {
                        if let Some(else_start) = bounds.else_start {
                            hidden_ranges.push((else_start, bounds.fi_end));
                        }
                    }
                    i = word_end;
                    continue;
                }
                if let Some(end) = skip_command_macro_declaration(src, i) {
                    i = end;
                    continue;
                }
                if let Some(keyword_end) = environment_keyword_end(src, i) {
                    i = parse_env_macro(src, keyword_end)
                        .map(|(_, _, end)| end)
                        .unwrap_or(keyword_end);
                    continue;
                }
                if is_inline_literal_command(word) {
                    i = inline_literal_payload(src, word, word_end)
                        .map(|(_, end)| end)
                        .unwrap_or(bytes.len());
                    continue;
                }
                if word == "string" {
                    i = tex_token_end(src, skip_tex_space_and_comments(src, word_end));
                    continue;
                }
                if matches!(word, "detokenize" | "unexpanded") {
                    i = read_braced(src, skip_tex_space_and_comments(src, word_end))
                        .map(|(_, end)| end)
                        .unwrap_or(bytes.len());
                    continue;
                }
                let Some(token) = environment_token_at(src, i) else {
                    i = word_end;
                    continue;
                };
                if token.kind == EnvironmentTokenKind::Begin
                    && candidates.contains(&token.name.as_str())
                {
                    let (end_start, _end_after) =
                        find_matching_end_lexical_in(src, token.end, &token.name)?;
                    return Some((token.name, src[token.end..end_start].to_string()));
                }
                if token.kind == EnvironmentTokenKind::Begin
                    && (environment_is_line_delimited_literal(&token.name)
                        || SKIP_ENVS.contains(&token.name.as_str()))
                {
                    i = literal_environment_bounds(src, token.end, &token.name)
                        .map(|(_, end_after)| end_after)
                        .unwrap_or(bytes.len());
                    continue;
                }
                if token.kind == EnvironmentTokenKind::Begin && environment_is_literal(&token.name)
                {
                    i = find_matching_end_lexical_in(src, token.end, &token.name)
                        .map(|(_, end_after)| end_after)
                        .unwrap_or(bytes.len());
                    continue;
                }
                i = token.end;
            }
            _ => i += 1,
        }
    }
    None
}

/// Whether the live buffer's executable TeX has balanced math delimiters.
/// Stored definitions, false conditional branches, and literal/code input do
/// not participate. The daemon uses this to keep the last good preview while
/// the user is midway through typing a real math delimiter.
pub fn has_balanced_math_delimiters(source: &str, preamble: &str) -> bool {
    let mut literals = scan_literal_declarations(preamble);
    let source_literals = scan_literal_declarations(source);
    literals.environments.extend(source_literals.environments);
    literals.commands.extend(source_literals.commands);

    let bytes = source.as_bytes();
    let mut hidden_ranges: Vec<(usize, usize)> = Vec::new();
    let mut conditionals = ConditionalLookup::default();
    let mut in_inline = false;
    let mut in_display = false;
    let mut in_comment = false;
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(resume) = scheduled_hidden_resume(i, &mut hidden_ranges) {
            i = resume;
            continue;
        }
        let byte = bytes[i];
        if byte == b'\n' {
            in_comment = false;
            i += 1;
            continue;
        }
        if in_comment {
            i += 1;
            continue;
        }
        if byte == b'%' {
            in_comment = true;
            i += 1;
            continue;
        }
        if byte == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'(' | b')' => {
                    in_inline = !in_inline;
                    i += 2;
                    continue;
                }
                b'[' | b']' => {
                    in_display = !in_display;
                    i += 2;
                    continue;
                }
                next if !next.is_ascii_alphabetic() => {
                    i += 2;
                    continue;
                }
                _ => {}
            }

            let word_start = i + 1;
            let mut word_end = word_start;
            while word_end < bytes.len()
                && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
            {
                word_end += 1;
            }
            let word = &source[word_start..word_end];
            if word == "iffalse" {
                i = conditionals
                    .get(source, i, word_end)
                    .map(|bounds| bounds.else_end.unwrap_or(bounds.fi_end))
                    .unwrap_or(bytes.len());
                continue;
            }
            if word == "iftrue" {
                if let Some(bounds) = conditionals.get(source, i, word_end) {
                    if let Some(else_start) = bounds.else_start {
                        hidden_ranges.push((else_start, bounds.fi_end));
                    }
                }
                i = word_end;
                continue;
            }
            if let Some(end) = skip_command_macro_declaration(source, i) {
                i = end;
                continue;
            }
            if let Some(keyword_end) = environment_keyword_end(source, i) {
                i = parse_env_macro(source, keyword_end)
                    .map(|(_, _, end)| end)
                    .unwrap_or(keyword_end);
                continue;
            }
            if is_static_inline_literal_command(word) || literals.commands.contains(word) {
                i = inline_literal_payload_with_dynamic(
                    source,
                    word,
                    word_end,
                    literals.commands.contains(word),
                )
                .map(|(_, end)| end)
                .unwrap_or(bytes.len());
                continue;
            }
            if word == "string" {
                i = tex_token_end(source, skip_tex_space_and_comments(source, word_end));
                continue;
            }
            if matches!(word, "detokenize" | "unexpanded") {
                i = read_braced(source, skip_tex_space_and_comments(source, word_end))
                    .map(|(_, end)| end)
                    .unwrap_or(bytes.len());
                continue;
            }
            if word == "begin" {
                if let Some(token) = environment_token_at(source, i) {
                    if token.kind == EnvironmentTokenKind::Begin
                        && (environment_is_literal_with(&token.name, &literals.environments)
                            || SKIP_ENVS.contains(&token.name.as_str()))
                    {
                        i = if (token.name != "alltt"
                            && environment_is_literal_with(&token.name, &literals.environments))
                            || SKIP_ENVS.contains(&token.name.as_str())
                        {
                            literal_environment_bounds(source, token.end, &token.name)
                                .map(|(_, end)| end)
                                .unwrap_or(bytes.len())
                        } else {
                            find_matching_end_lexical_in(source, token.end, &token.name)
                                .map(|(_, end)| end)
                                .unwrap_or(bytes.len())
                        };
                        continue;
                    }
                    i = token.end;
                    continue;
                }
            }
            i = word_end;
            continue;
        }
        if byte == b'$' {
            if bytes.get(i + 1) == Some(&b'$') {
                in_display = !in_display;
                i += 2;
                continue;
            }
            in_inline = !in_inline;
        }
        i += 1;
    }
    !in_inline && !in_display
}

/// Return the last lexically live braced value for each requested control
/// word. Stored definitions, false conditional branches, comments, and
/// literal/code payloads are skipped as inert TeX input. Keys omit the leading
/// backslash; an explicit empty group is retained as `Some("")` by callers.
pub(crate) fn last_live_braced_command_values(
    source: &str,
    commands: &[&str],
) -> HashMap<String, String> {
    let src = crate::macros::strip_line_comments(source);
    let bytes = src.as_bytes();
    let literals = scan_literal_declarations(source);
    let mut values = HashMap::new();
    let mut hidden_ranges: Vec<(usize, usize)> = Vec::new();
    let mut conditionals = ConditionalLookup::default();
    let mut i = 0usize;

    while i < bytes.len() {
        if let Some(resume) = scheduled_hidden_resume(i, &mut hidden_ranges) {
            i = resume;
            continue;
        }
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }

        let word_start = i + 1;
        let mut word_end = word_start;
        while word_end < bytes.len()
            && (bytes[word_end].is_ascii_alphabetic() || bytes[word_end] == b'@')
        {
            word_end += 1;
        }
        if word_end == word_start {
            i = (i + 2).min(bytes.len());
            continue;
        }
        let word = &src[word_start..word_end];

        if word == "iffalse" {
            i = conditionals
                .get(&src, i, word_end)
                .map(|bounds| bounds.else_end.unwrap_or(bounds.fi_end))
                .unwrap_or(bytes.len());
            continue;
        }
        if word == "iftrue" {
            if let Some(bounds) = conditionals.get(&src, i, word_end) {
                if let Some(else_start) = bounds.else_start {
                    hidden_ranges.push((else_start, bounds.fi_end));
                }
            }
            i = word_end;
            continue;
        }
        if let Some(end) = skip_command_macro_declaration(&src, i) {
            i = end;
            continue;
        }
        if let Some(keyword_end) = environment_keyword_end(&src, i) {
            i = parse_env_macro(&src, keyword_end)
                .map(|(_, _, end)| end)
                .unwrap_or(keyword_end);
            continue;
        }
        if let Some((_, end)) = literal_environment_declaration_at(&src, word, word_end) {
            i = end;
            continue;
        }
        if let Some((_, end)) = inline_literal_declaration_at(&src, word, word_end) {
            i = end;
            continue;
        }
        if is_static_inline_literal_command(word) || literals.commands.contains(word) {
            i = inline_literal_payload_with_dynamic(
                &src,
                word,
                word_end,
                literals.commands.contains(word),
            )
            .map(|(_, end)| end)
            .unwrap_or(bytes.len());
            continue;
        }
        if word == "string" {
            i = tex_token_end(&src, skip_tex_space_and_comments(&src, word_end));
            continue;
        }
        if matches!(word, "detokenize" | "unexpanded") {
            i = read_braced(&src, skip_tex_space_and_comments(&src, word_end))
                .map(|(_, end)| end)
                .unwrap_or(bytes.len());
            continue;
        }
        if word == "begin" {
            if let Some(token) = environment_token_at(&src, i) {
                if token.kind == EnvironmentTokenKind::Begin
                    && (environment_is_literal_with(&token.name, &literals.environments)
                        || SKIP_ENVS.contains(&token.name.as_str()))
                {
                    i = if (token.name != "alltt"
                        && environment_is_literal_with(&token.name, &literals.environments))
                        || SKIP_ENVS.contains(&token.name.as_str())
                    {
                        literal_environment_bounds(&src, token.end, &token.name)
                            .map(|(_, end)| end)
                            .unwrap_or(bytes.len())
                    } else {
                        find_matching_end_lexical_in(&src, token.end, &token.name)
                            .map(|(_, end)| end)
                            .unwrap_or(bytes.len())
                    };
                    continue;
                }
                i = token.end;
                continue;
            }
        }

        if commands.contains(&word) {
            let mut argument_start = skip_tex_space_and_comments(&src, word_end);
            if bytes.get(argument_start) == Some(&b'[') {
                let Some(optional_end) = tex_group_end(&src, argument_start, b'[', b']') else {
                    i = word_end;
                    continue;
                };
                argument_start = skip_tex_space_and_comments(&src, optional_end);
            }
            if let Some((value, end)) = read_braced(&src, argument_start) {
                values.insert(word.to_string(), value);
                i = end;
                continue;
            }
        }
        i = word_end;
    }

    values
}

fn promote_letter_commands(nodes: &mut [Node]) {
    // Unsupported environments flatten their children between boundary
    // markers. Do not let an `\opening` inside one of those nested wrappers
    // take over the surrounding letter's structure.
    let mut unsupported_depth = 0usize;
    for node in nodes {
        match &node.kind {
            NodeKind::UnsupportedEnvBoundary {
                boundary: EnvironmentBoundary::Begin,
                ..
            } => {
                unsupported_depth += 1;
                continue;
            }
            NodeKind::UnsupportedEnvBoundary {
                boundary: EnvironmentBoundary::End | EnvironmentBoundary::MissingEnd,
                ..
            } => {
                unsupported_depth = unsupported_depth.saturating_sub(1);
                continue;
            }
            _ => {}
        }
        if unsupported_depth != 0 {
            continue;
        }

        let replacement = match &node.kind {
            NodeKind::OpaqueCmd { name, raw } if name == "opening" => {
                letter_command_arg(raw, name).map(|text| NodeKind::LetterOpening { text })
            }
            NodeKind::OpaqueCmd { name, raw } if name == "closing" => {
                letter_command_arg(raw, name).map(|text| NodeKind::LetterClosing { text })
            }
            _ => None,
        };
        if let Some(kind) = replacement {
            node.kind = kind;
        }
    }
}

fn letter_command_arg(raw: &str, name: &str) -> Option<String> {
    let prefix = format!(r"\{name}");
    if !raw.starts_with(&prefix) {
        return None;
    }
    read_braced(raw, skip_tex_space_and_comments(raw, prefix.len())).map(|(arg, _)| arg)
}

fn section_level(cmd: &str) -> Option<u8> {
    Some(match cmd {
        "part" => 0,
        "chapter" => 1,
        "section" => 2,
        "subsection" => 3,
        "subsubsection" => 4,
        "paragraph" => 5,
        "subparagraph" => 6,
        _ => return None,
    })
}

fn split_top_level_commas(src: &str) -> Vec<&str> {
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut parts = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(src[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(src[start..].trim());
    parts
}

fn option_key_value<'a>(part: &'a str, key: &str) -> Option<&'a str> {
    let (candidate, value) = part.split_once('=')?;
    if candidate.trim().eq_ignore_ascii_case(key) {
        Some(value.trim())
    } else {
        None
    }
}

fn is_single_braced_group(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 2 || bytes.first() != Some(&b'{') || bytes.last() != Some(&b'}') {
        return false;
    }

    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 && i + 1 != bytes.len() {
                    return false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    depth == 0
}

fn strip_wrapping_braces(s: &str) -> &str {
    let mut out = s.trim();
    while is_single_braced_group(out) {
        out = out[1..out.len() - 1].trim();
    }
    out
}

fn parse_proof_option(opt: Option<String>) -> (Option<String>, Option<Role>) {
    let Some(opt) = opt else {
        return (None, None);
    };
    let opt = opt.trim();
    if opt.is_empty() {
        return (None, None);
    }

    let has_metadata = split_top_level_commas(opt).iter().any(|part| {
        option_key_value(part, "role").is_some()
            || option_key_value(part, "name").is_some()
            || option_key_value(part, "of").is_some()
    });
    if !has_metadata {
        return (Some(opt.to_string()), None);
    }

    let mut role = None;
    let mut title_parts = Vec::new();
    for part in split_top_level_commas(opt) {
        if part.is_empty() {
            continue;
        }
        if let Some(value) = option_key_value(part, "role") {
            role = Some(Role::parse(strip_wrapping_braces(value)));
        } else if let Some(value) = option_key_value(part, "name") {
            let value = strip_wrapping_braces(value);
            if !value.is_empty() {
                title_parts.push(value.to_string());
            }
        } else if let Some(value) = option_key_value(part, "of") {
            let value = strip_wrapping_braces(value);
            if !value.is_empty() {
                let lower = value.to_ascii_lowercase();
                if lower.starts_with("of ") || lower.starts_with("proof ") {
                    title_parts.push(value.to_string());
                } else {
                    title_parts.push(format!("of {value}"));
                }
            }
        } else {
            title_parts.push(part.to_string());
        }
    }

    let of = if title_parts.is_empty() {
        None
    } else {
        Some(title_parts.join(", "))
    };
    (of, role)
}

fn list_kind_for(env: &str) -> Option<ListKind> {
    Some(match env {
        "enumerate" | "enumerate*" | "compactenum" | "asparaenum" => ListKind::Enumerate,
        "itemize" | "itemize*" | "compactitem" | "asparaitem" => ListKind::Itemize,
        "description" | "compactdesc" => ListKind::Description,
        _ => return None,
    })
}

fn contains_blank_line(s: &str) -> bool {
    let mut newlines = 0u8;
    for ch in s.chars() {
        match ch {
            '\n' => {
                newlines += 1;
                if newlines >= 2 {
                    return true;
                }
            }
            ' ' | '\t' | '\r' => {}
            _ => newlines = 0,
        }
    }
    false
}

/// Split an enumerate/itemize body into `(marker, slice_start, slice_end)`
/// chunks per `\item`. Skips over any nested `\begin{X}...\end{X}` so
/// nested lists don't trigger a top-level split.
fn split_list_items(src: &str) -> Vec<(Option<String>, usize, usize)> {
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    let mut items: Vec<(Option<String>, usize, usize)> = Vec::new();
    let mut current: Option<(Option<String>, usize)> = None;

    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if bytes[i..].starts_with(b"\\begin{") {
                depth += 1;
                i += b"\\begin{".len();
                continue;
            }
            if bytes[i..].starts_with(b"\\end{") {
                depth -= 1;
                i += b"\\end{".len();
                continue;
            }
            if depth == 0 && bytes[i..].starts_with(b"\\item") {
                let after = i + b"\\item".len();
                let next = bytes.get(after).copied();
                let is_word_boundary = match next {
                    Some(c) => !c.is_ascii_alphabetic(),
                    None => true,
                };
                if is_word_boundary {
                    if let Some((m, s)) = current.take() {
                        items.push((m, s, i));
                    }
                    let mut j = after;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    let marker = if j < bytes.len() && bytes[j] == b'[' {
                        let start_b = j + 1;
                        let mut k = start_b;
                        let mut bd = 0i32;
                        while k < bytes.len() {
                            match bytes[k] {
                                b'\\' if k + 1 < bytes.len() => {
                                    k += 2;
                                    continue;
                                }
                                b'{' => bd += 1,
                                b'}' => bd -= 1,
                                b']' if bd == 0 => break,
                                _ => {}
                            }
                            k += 1;
                        }
                        let m = src[start_b..k.min(bytes.len())].to_string();
                        j = (k + 1).min(bytes.len());
                        Some(m)
                    } else {
                        None
                    };
                    current = Some((marker, j));
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    if let Some((m, s)) = current {
        items.push((m, s, bytes.len()));
    }
    items
}

fn find_first_label(src: &str) -> Option<String> {
    first_label_span(src).map(|(label, _, _)| label)
}

fn take_first_direct_label(nodes: &mut Vec<Node>) -> Option<String> {
    let (index, label) = nodes.iter().enumerate().find_map(|(index, node)| {
        let NodeKind::OpaqueCmd { name, raw } = &node.kind else {
            return None;
        };
        (name == "label")
            .then(|| find_first_label(raw))
            .flatten()
            .map(|label| (index, label))
    })?;
    nodes.remove(index);
    Some(label)
}

fn first_label_span(src: &str) -> Option<(String, usize, usize)> {
    let needle = "\\label";
    let mut search_from = 0usize;
    while let Some(found) = src[search_from..].find(needle) {
        let i = search_from + found;
        if let Some(span) = label_span_at(src, i) {
            return Some(span);
        }
        search_from = i + needle.len();
    }
    None
}

fn initial_label_span(src: &str) -> Option<(String, usize, usize)> {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) == Some(&b'%') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        break;
    }

    if !src[i..].starts_with("\\label") {
        return None;
    }
    label_span_at(src, i)
}

fn label_span_at(src: &str, i: usize) -> Option<(String, usize, usize)> {
    let bytes = src.as_bytes();
    let after = i + "\\label".len();
    if bytes
        .get(after)
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'*')
    {
        return None;
    }

    let mut j = after;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    if bytes.get(j) != Some(&b'{') {
        return None;
    }

    let label_start = j + 1;
    let mut depth = 1i32;
    let mut k = label_start;
    while k < bytes.len() {
        match bytes[k] {
            b'\\' if k + 1 < bytes.len() => {
                k += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((src[label_start..k].trim().to_string(), i, k + 1));
                }
            }
            _ => {}
        }
        k += 1;
    }
    None
}

fn find_first_omitref(src: &str) -> Option<String> {
    // Brace-balanced: `\omitref{Rudin, \emph{PMA}, §3.27}` is one arg.
    let bytes = src.as_bytes();
    let needle = b"\\omitref";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let mut j = i + needle.len();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'{' {
                let start = j + 1;
                let mut depth = 1i32;
                let mut k = start;
                while k < bytes.len() {
                    match bytes[k] {
                        b'\\' if k + 1 < bytes.len() => k += 2,
                        b'{' => {
                            depth += 1;
                            k += 1;
                        }
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(src[start..k].to_string());
                            }
                            k += 1;
                        }
                        _ => k += 1,
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn extract_label(src: &str) -> Option<String> {
    find_first_label(src)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Preamble, ProjectFile};
    use std::path::PathBuf;

    fn parse(src: &str) -> Vec<Node> {
        let project = Project {
            root: PathBuf::from("t.tex"),
            preamble: Preamble {
                source: String::new(),
                file: PathBuf::from("t.tex"),
            },
            preamble_files: vec![],
            files: vec![ProjectFile {
                path: PathBuf::from("t.tex"),
                source: src.to_string(),
                start: Pos::ZERO,
                is_root_body: true,
            }],
            dependency_files: vec![],
            warnings: vec![],
        };
        let thms = TheoremRegistry::from_preamble(&project.preamble.source);
        parse_body(&project, &thms).unwrap()
    }

    fn parse_with_preamble(preamble: &str, src: &str) -> Vec<Node> {
        let project = Project {
            root: PathBuf::from("t.tex"),
            preamble: Preamble {
                source: preamble.to_string(),
                file: PathBuf::from("t.tex"),
            },
            preamble_files: vec![],
            files: vec![ProjectFile {
                path: PathBuf::from("t.tex"),
                source: src.to_string(),
                start: Pos::ZERO,
                is_root_body: true,
            }],
            dependency_files: vec![],
            warnings: vec![],
        };
        let thms = TheoremRegistry::from_preamble(&project.preamble.source);
        parse_body(&project, &thms).unwrap()
    }

    fn parse_with_env_overrides(preamble: &str, overrides: &[&str], src: &str) -> Vec<Node> {
        let project = Project {
            root: PathBuf::from("t.tex"),
            preamble: Preamble {
                source: preamble.to_string(),
                file: PathBuf::from("t.tex"),
            },
            preamble_files: vec![],
            files: vec![ProjectFile {
                path: PathBuf::from("t.tex"),
                source: src.to_string(),
                start: Pos::ZERO,
                is_root_body: true,
            }],
            dependency_files: vec![],
            warnings: vec![],
        };
        let layers: Vec<MacroOverride> = overrides
            .iter()
            .enumerate()
            .map(|(i, source)| MacroOverride {
                label: PathBuf::from(format!("override-{i}.tex")),
                source: (*source).to_string(),
            })
            .collect();
        let thms = TheoremRegistry::from_preamble(&project.preamble.source);
        parse_body_with_overrides(&project, &thms, &layers).unwrap()
    }

    #[test]
    fn commented_out_newenvironment_is_ignored() {
        let m = extract_env_macros("% \\newenvironment{dead}{X}{Y}\n");
        assert!(
            !m.contains_key("dead"),
            "commented-out \\newenvironment was honored"
        );
    }

    #[test]
    fn commented_out_def_does_not_shadow_live_one() {
        let m = extract_env_macros(concat!(
            "\\newenvironment{foo}{NEW}{E}\n",
            "% old version, kept for reference:\n",
            "% \\newenvironment{foo}{OLD}{E}\n",
        ));
        assert_eq!(
            m.get("foo").unwrap().begin,
            "NEW",
            "commented-out old def shadowed the live one"
        );
    }

    #[test]
    fn percent_continuation_newenvironment_is_parsed() {
        // Standard LaTeX style: trailing % to suppress spurious spaces.
        let m = extract_env_macros("\\newenvironment{cont}%\n  {B}%\n  {E}\n");
        let c = m.get("cont").expect("%-continued definition parsed");
        assert_eq!(c.begin, "B");
        assert_eq!(c.end, "E");
    }

    #[test]
    fn environment_scanner_ignores_prefixes_and_nested_definition_text() {
        let m = extract_env_macros(concat!(
            "\\newcommand{\\newenvironmenthelper}{still a command}\n",
            "\\newcommand{\\factory}{\\newenvironment{hidden}{}{}}\n",
            "\\def\\factory{\\newenvironment{hiddenDef}{}{}}\n",
            "\\NewDocumentCommand{\\factory}{m}{\\newenvironment{hiddenX}{}{}}\n",
            "\\DeclareMathOperator{\\factory}{\\newenvironment{hiddenOp}{}{}}\n",
            "\\DeclarePairedDelimiter{\\factory}",
            "{\\newenvironment{hiddenPair}{}{}}{)}\n",
        ));
        assert!(m.is_empty(), "nested text was treated as a declaration");
    }

    #[test]
    fn environment_scanner_honors_immediately_executed_wrappers() {
        let m = extract_env_macros("\\AtBeginDocument{\\newenvironment{wrapped}{BEGIN}{END}}\n");
        assert!(m.contains_key("wrapped"));
    }

    #[test]
    fn environment_scanner_ignores_false_conditional_branch() {
        let nodes = parse_with_preamble(
            concat!(
                "\\newenvironment{choice}{GOOD }{}\n",
                "\\iffalse\n",
                "\\renewenvironment{choice}{BAD }{}\n",
                "\\fi\n",
            ),
            "\\begin{choice}Body\\end{choice}\n",
        );
        assert!(tree_has_text(&nodes, "GOOD"));
        assert!(!tree_has_text(&nodes, "BAD"), "{nodes:#?}");
    }

    #[test]
    fn stored_macro_body_cannot_reclassify_environment_as_literal() {
        let nodes = parse_with_preamble(
            concat!(
                "\\newenvironment{Code}{}{}\n",
                "\\newcommand{\\factory}{\\lstnewenvironment{Code}{}{}}\n",
            ),
            "\\begin{Code}Live $x$.\\end{Code}\n",
        );
        assert!(tree_has_text(&nodes, "Live"));
        assert!(nodes
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "x")));
        assert!(!nodes
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::OpaqueEnv { env, .. } if env == "Code")));
    }

    #[test]
    fn tcolorbox_listing_declarations_accept_init_options_and_modern_names() {
        let declared = declared_literal_environments(concat!(
            "\\newtcblisting[auto counter]{CodeBox}{listing only}\n",
            "\\DeclareTCBListing{DeclaredBox}{m}{listing only,",
            "title={\\lstnewenvironment{HiddenBox}{}{}}}\n",
            "\\NewTCBListing[use counter from=CodeBox]{ModernBox}{O{}}{listing only}\n",
            "\\RenewTCBListing{RenewedBox}{m}{listing only}\n",
            "\\ProvideTCBListing{ProvidedBox}{m}{listing only}\n",
        ));
        for name in [
            "CodeBox",
            "DeclaredBox",
            "ModernBox",
            "RenewedBox",
            "ProvidedBox",
        ] {
            assert!(declared.contains(name), "missing {name}: {declared:?}");
        }
        assert!(!declared.contains("HiddenBox"), "{declared:?}");
    }

    #[test]
    fn environment_substitution_preserves_escaped_hashes() {
        assert_eq!(
            substitute_env_args(r"\#1 / #1 / ##", &["Ada".to_string()]),
            r"\#1 / Ada / #"
        );
        assert_eq!(
            substitute_env_args(r"#1X", &[r"\recipient".to_string()]),
            r"\recipient X"
        );
        assert_eq!(
            substitute_env_args(r"\prefix#1", &["X".to_string()]),
            r"\prefix X"
        );
    }

    #[test]
    fn recursive_fanout_environment_stops_at_the_expansion_budget() {
        let nodes = parse_with_preamble(
            concat!(
                "\\newenvironment{fanout}{",
                "\\begin{fanout}a\\end{fanout}",
                "\\begin{fanout}b\\end{fanout}",
                "}{}\n",
            ),
            "\\begin{fanout}root\\end{fanout}\n",
        );
        assert!(
            nodes.iter().any(
                |node| matches!(&node.kind, NodeKind::OpaqueEnv { env, .. } if env == "fanout")
            ),
            "recursive expansion did not fall back to an opaque environment"
        );
        assert!(
            nodes.len() <= MAX_USER_ENV_EXPANSIONS * 2 + 1,
            "recursive expansion escaped its work bound: {} nodes",
            nodes.len()
        );
    }

    #[test]
    fn expansion_budget_resets_for_each_outermost_environment() {
        let src = "\\begin{simple}x\\end{simple}\n".repeat(MAX_USER_ENV_EXPANSIONS + 1);
        let nodes = parse_with_preamble("\\newenvironment{simple}{}{}\n", &src);
        assert!(
            !nodes.iter().any(
                |node| matches!(&node.kind, NodeKind::OpaqueEnv { env, .. } if env == "simple")
            ),
            "ordinary sequential uses exhausted the recursion budget"
        );
    }

    #[test]
    fn user_environment_accepts_single_token_mandatory_argument() {
        let nodes = parse_with_preamble(
            "\\newenvironment{tagged}[1]{To #1: }{}\n",
            "\\begin{tagged}XBody\\end{tagged}\n",
        );
        assert!(tree_has_text(&nodes, "To X:"));
        assert!(tree_has_text(&nodes, "Body"));
    }

    #[test]
    fn user_environment_argument_skips_tex_comment_continuation() {
        let nodes = parse_with_preamble(
            "\\newenvironment{tagged}[1]{To #1: }{}\n",
            "\\begin{tagged}% continued\n  {Address}Body\\end{tagged}\n",
        );
        assert!(tree_has_text(&nodes, "To Address:"));
        assert!(tree_has_text(&nodes, "Body"));
    }

    #[test]
    fn environment_default_cannot_reference_parameters() {
        assert!(validate_environment_override_line("\\newenvironment{bad}[1][#1]{#1}{}").is_err());
        assert!(validate_environment_override_line("\\newenvironment{bad}[1][##]{#1}{}").is_err());
        assert!(
            validate_environment_override_line("\\newenvironment{literal}[1][\\#]{#1}{}").is_ok()
        );
    }

    #[test]
    fn user_env_body_on_same_line_as_begin_keeps_text() {
        // Begin code ending in a control word (\itshape) + body starting with
        // a letter: the seam needs a token boundary or the re-parse reads one
        // long command `\itshapeHello` and the word disappears.
        let nodes = parse_with_preamble(
            "\\newenvironment{referee}{\\itshape}{}\n",
            "\\begin{referee}Hello $x$\\end{referee}\n",
        );
        fn any_text(nodes: &[Node], needle: &str) -> bool {
            nodes.iter().any(|n| {
                matches!(&n.kind, NodeKind::Text(s) if s.contains(needle))
                    || any_text(&n.children, needle)
            })
        }
        assert!(
            any_text(&nodes, "Hello"),
            "body text 'Hello' lost after expansion"
        );
    }

    #[test]
    fn root_preamble_env_def_wins_over_included_file() {
        // \usepackage{local} then \renewenvironment in the document preamble:
        // the ROOT definition must win over the included file's.
        let tmp = std::env::temp_dir().join(format!("mp-envorder-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let inc = tmp.join("incmacros.tex");
        std::fs::write(&inc, "\\newenvironment{foo}{INC}{E}\n").unwrap();
        let root_file = tmp.join("t.tex");
        let project = Project {
            root: root_file.clone(),
            preamble: Preamble {
                source: "\\input{incmacros}\n\\renewenvironment{foo}{ROOT}{E}\n".to_string(),
                file: root_file.clone(),
            },
            preamble_files: vec![crate::project::PreambleFile {
                path: inc.clone(),
                source: std::fs::read_to_string(&inc).unwrap(),
            }],
            files: vec![ProjectFile {
                path: root_file,
                source: String::new(),
                start: Pos::ZERO,
                is_root_body: true,
            }],
            dependency_files: vec![],
            warnings: vec![],
        };
        let m = env_macros_for_project(&project, &[]);
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(m.get("foo").expect("foo defined").begin, "ROOT");
    }

    #[test]
    fn later_preview_env_override_wins_and_substitutes_optional_default() {
        let nodes = parse_with_env_overrides(
            "\\newenvironment{memo}[2][Source]{SOURCE #1/#2:}{}\n",
            &[
                "\\renewenvironment{memo}[2][Global]{GLOBAL #1/#2:}{}\n",
                "\\renewenvironment{memo}[2][Recommendation]{VIEW #1/#2:}{ END}\n",
            ],
            concat!(
                "\\begin{memo}{Ada}Body $x$.\\end{memo}\n",
                "\\begin{memo}[Confidential]{Emmy}More.\\end{memo}\n",
            ),
        );
        for expected in [
            "VIEW Recommendation/Ada:",
            "VIEW Confidential/Emmy:",
            "Body",
            "More",
            "END",
        ] {
            assert!(
                tree_has_text(&nodes, expected),
                "missing {expected:?}: {nodes:#?}"
            );
        }
        for shadowed in ["SOURCE", "GLOBAL"] {
            assert!(
                !tree_has_text(&nodes, shadowed),
                "lower-priority definition leaked: {nodes:#?}"
            );
        }
        assert!(
            nodes
                .iter()
                .any(|node| matches!(&node.kind, NodeKind::InlineMath(s) if s == "x")),
            "math inside overridden environment was not parsed: {nodes:#?}"
        );
    }

    #[test]
    fn inline_math() {
        let n = parse(r"hello $x+y$ world");
        assert!(matches!(&n[1].kind, NodeKind::InlineMath(s) if s == "x+y"));
    }

    #[test]
    fn scoped_text_color_group_stays_atomic_across_inline_math() {
        let n = parse(r"before {\color[rgb]{1,0.5,0} colored $x$ text} after");
        assert_eq!(n.len(), 3, "{n:#?}");
        assert!(matches!(&n[0].kind, NodeKind::Text(s) if s == "before "));
        assert!(matches!(
            &n[1].kind,
            NodeKind::TextColor {
                model: Some(model),
                color,
            } if model == "rgb" && color == "1,0.5,0"
        ));
        assert!(matches!(
            &n[1].children[0].kind,
            NodeKind::Text(s) if s == " colored "
        ));
        assert!(matches!(
            &n[1].children[1].kind,
            NodeKind::InlineMath(s) if s == "x"
        ));
        assert!(matches!(
            &n[1].children[2].kind,
            NodeKind::Text(s) if s == " text"
        ));
        assert!(matches!(&n[2].kind, NodeKind::Text(s) if s == " after"));
    }

    #[test]
    fn scoped_text_color_body_keeps_semantic_nodes_comments_and_literals() {
        let n = parse(concat!(
            "{\\color{red} see \\cite{smith} % a commented }\n",
            "\\verb|}| tail\n",
            "\\begin{equation}x=1\\label{eq:colored}\\end{equation}",
            "} after",
        ));
        assert_eq!(n.len(), 2, "{n:#?}");
        let colored = &n[0];
        assert!(matches!(&colored.kind, NodeKind::TextColor { color, .. } if color == "red"));
        assert!(
            colored
                .children
                .iter()
                .any(|node| matches!(&node.kind, NodeKind::Cite { keys } if keys == &["smith"])),
            "{n:#?}"
        );
        assert!(
            colored.children.iter().any(
                |node| matches!(&node.kind, NodeKind::Comment(text) if text.contains("commented"))
            ),
            "{n:#?}"
        );
        assert!(
            colored
                .children
                .iter()
                .any(|node| matches!(&node.kind, NodeKind::OpaqueCmd { name, raw } if name == "inline-literal" && raw == "}")),
            "{n:#?}"
        );
        assert!(
            colored.children.iter().any(|node| matches!(
                &node.kind,
                NodeKind::DisplayMath { label: Some(label), .. } if label == "eq:colored"
            )),
            "{n:#?}"
        );
        assert!(matches!(&n[1].kind, NodeKind::Text(s) if s == " after"));
    }

    #[test]
    fn scoped_text_color_ignores_braces_in_hidden_conditional_branches() {
        let false_branch = parse(r"before {\color{red}\iffalse }\fi visible} after");
        assert_eq!(false_branch.len(), 3, "{false_branch:#?}");
        assert!(matches!(
            &false_branch[1].kind,
            NodeKind::TextColor { color, .. } if color == "red"
        ));
        assert!(
            tree_has_text(&false_branch[1].children, "visible"),
            "{false_branch:#?}"
        );
        assert!(matches!(
            &false_branch[2].kind,
            NodeKind::Text(text) if text == " after"
        ));

        let true_branch = parse(r"{\color{blue}\iftrue shown\else }\fi still} outside");
        assert_eq!(true_branch.len(), 2, "{true_branch:#?}");
        assert!(matches!(
            &true_branch[0].kind,
            NodeKind::TextColor { color, .. } if color == "blue"
        ));
        assert!(
            tree_has_text(&true_branch[0].children, "shown")
                && tree_has_text(&true_branch[0].children, "still"),
            "{true_branch:#?}"
        );
        assert!(matches!(
            &true_branch[1].kind,
            NodeKind::Text(text) if text == " outside"
        ));
    }

    #[test]
    fn scoped_text_color_counts_definition_bodies_lexically() {
        let nodes = parse(r"{\color{red}\def\foo{\iftrue{\else}\fi} visible} outside");
        assert_eq!(nodes.len(), 2, "{nodes:#?}");
        assert!(matches!(
            &nodes[0].kind,
            NodeKind::TextColor { color, .. } if color == "red"
        ));
        assert!(tree_has_text(&nodes[0].children, "visible"), "{nodes:#?}");
        assert!(matches!(
            &nodes[1].kind,
            NodeKind::Text(text) if text == " outside"
        ));
    }

    #[test]
    fn executable_source_and_live_calls_ignore_dormant_tex() {
        let source = concat!(
            "foo% joined\nbar ",
            "\\newcommand{\\stored}{\\cite{defined}}",
            "\\iftrue live\\else \\cite{hidden}\\fi ",
            "\\iffalse false\\else \\cite{shown}\\fi ",
            "\\verb|\\cite{literal}%| ",
            "\\label% continued\n{tab:live}",
        );
        let live = executable_latex_source(source);
        assert!(live.contains("foobar"), "{live}");
        assert!(live.contains("live"), "{live}");
        assert!(!live.contains("defined"), "{live}");
        assert!(!live.contains("hidden"), "{live}");
        assert!(!live.contains("false"), "{live}");
        assert!(live.contains(r"\verb|\cite{literal}%|"), "{live}");

        let cites = live_braced_command_calls(source, &["cite"], 2);
        assert_eq!(
            cites
                .into_iter()
                .map(|call| call.value)
                .collect::<Vec<_>>(),
            ["shown"]
        );
        let labels = live_braced_command_calls(source, &["label"], 0);
        assert_eq!(labels[0].value, "tab:live");
    }

    #[test]
    fn deeply_nested_detokenize_group_matching_is_iterative() {
        let depth = 5_000;
        let source = format!(
            "{{\\color{{red}}{}x{}}}",
            "\\detokenize{".repeat(depth),
            "}".repeat(depth)
        );
        let end = tex_group_end(&source, 0, b'{', b'}').expect("balanced group");
        assert_eq!(end, source.len());
    }

    #[test]
    fn deeply_nested_live_conditionals_are_scanned_once() {
        let depth = 10_000;
        let mut source = String::from("{");
        source.push_str(&r"\iftrue ".repeat(depth));
        source.push_str("visible");
        source.push_str(&r"\else hidden } \fi".repeat(depth));
        source.push('}');

        CONDITIONAL_SCAN_STEPS.with(|steps| steps.set(0));
        let end = tex_group_end(&source, 0, b'{', b'}').expect("balanced outer group");
        assert_eq!(end, source.len());
        let group_steps = CONDITIONAL_SCAN_STEPS.with(Cell::get);
        assert!(
            group_steps <= source.len(),
            "group matcher rescanned conditional suffixes: {group_steps} steps for {} bytes",
            source.len()
        );

        CONDITIONAL_SCAN_STEPS.with(|steps| steps.set(0));
        let executable = executable_latex_source(&source);
        assert_eq!(executable, format!("{{{}visible}}", " ".repeat(depth)));
        let executable_steps = CONDITIONAL_SCAN_STEPS.with(Cell::get);
        assert!(
            executable_steps <= source.len(),
            "executable scanner rescanned conditional suffixes: {executable_steps} steps for {} bytes",
            source.len()
        );
    }

    #[test]
    fn ordinary_brace_runs_do_not_trigger_color_group_rescans() {
        let src = format!("{}x{}", "{".repeat(20_000), "}".repeat(20_000));
        let n = parse(&src);
        assert_eq!(n.len(), 1);
        assert!(matches!(&n[0].kind, NodeKind::Text(s) if s.len() == src.len()));
    }

    #[test]
    fn math_environment_ignores_stringified_fake_closer() {
        let n = parse(concat!(
            "\\begin{equation}\n",
            "\\text{\\texttt{\\string\\end{equation}}}+x=1\n",
            "\\end{equation}\n",
            "After.\n",
        ));
        assert!(n.iter().any(|node| matches!(
            &node.kind,
            NodeKind::DisplayMath { body, .. } if body.contains("+x=1")
        )));
        assert!(tree_has_text(&n, "After"));
    }

    #[test]
    fn unicode_text_is_preserved() {
        let n = parse("Café naïve §");
        assert!(matches!(&n[0].kind, NodeKind::Text(s) if s == "Café naïve §"));
    }

    #[test]
    fn tex_positions_use_neovim_utf8_byte_columns() {
        let nodes = parse("é before $x$");
        let math = nodes
            .iter()
            .find(|node| matches!(node.kind, NodeKind::InlineMath(_)))
            .expect("inline math node");

        assert_eq!(math.span.start.line, 1);
        assert_eq!(math.span.start.col, 11);
        assert_eq!(math.span.start.byte, 10);
        assert_eq!(math.span.end.col, 14);
        assert_eq!(math.span.end.byte, 13);
    }

    #[test]
    fn backslash_before_multibyte_char_does_not_panic() {
        // Regression: `\` followed by a multibyte char (`\é`, `\λ`, `\—`) used
        // to slice the source at a non-char boundary and panic — trivially
        // reachable whenever an author types accented text after a backslash.
        for src in [r"\é", r"\λ words", r"text \— more", r"a \§ b"] {
            let nodes = parse(src);
            assert!(!nodes.is_empty(), "parse of {src:?} produced no nodes");
        }
        let n = parse(r"\é");
        assert!(matches!(&n[0].kind, NodeKind::Text(s) if s.contains('é')));
    }

    #[test]
    fn inline_literal_unicode_delimiter_does_not_panic() {
        let src = r"\verbλa&bλ";
        assert_eq!(
            inline_literal_payload(src, "verb", r"\verb".len()),
            Some(("a&b".to_string(), src.len()))
        );
    }

    #[test]
    fn braced_inline_literal_keeps_percent_and_backslash_inert() {
        let src = "\\lstinline{50% \\\\ {nested}} after";
        assert_eq!(
            inline_literal_payload(src, "lstinline", r"\lstinline".len()),
            Some(("50% \\\\ {nested}".to_string(), src.find(" after").unwrap()))
        );
    }

    #[test]
    fn callout_env_parses_body_recursively_with_title() {
        let n = parse("\\begin{todo}[My title]\ntext $E=mc^2$ text\n\\end{todo}\n");
        let callout = n
            .iter()
            .find(|node| matches!(&node.kind, NodeKind::Callout { .. }))
            .expect("a Callout node");
        match &callout.kind {
            NodeKind::Callout { env, class, title } => {
                assert_eq!(env, "todo");
                assert_eq!(class, "todo");
                assert_eq!(title.as_deref(), Some("My title"));
            }
            _ => unreachable!(),
        }
        // Body is parsed (not dumped raw): the math is an InlineMath child.
        assert!(
            callout
                .children
                .iter()
                .any(|c| matches!(&c.kind, NodeKind::InlineMath(s) if s.contains("mc^2"))),
            "math inside the callout was not parsed: {:?}",
            callout.children
        );
    }

    #[test]
    fn extract_env_macros_parses_definitions() {
        let m = extract_env_macros(concat!(
            "\\newenvironment{referee}{\n\\begin{quote}\\itshape}{\n\\end{quote}}\n",
            "\\newenvironment{named}[1]{X#1}{Y}\n",
            "\\renewcommand{\\x}{y}\n", // not an environment — must be ignored
        ));
        let r = m.get("referee").expect("referee defined");
        // Surrounding whitespace (incl. the leading newlines) is trimmed so the
        // wrapped body keeps its real line numbers.
        assert_eq!(r.begin, "\\begin{quote}\\itshape");
        assert_eq!(r.end, "\\end{quote}");
        assert_eq!(r.nargs, 0);
        let n = m.get("named").expect("named defined");
        assert_eq!(n.nargs, 1);
        assert_eq!(n.begin, "X#1");
        assert_eq!(n.end, "Y");
        assert!(!m.contains_key("x"), "renewcommand must not be picked up");
    }

    #[test]
    fn quote_env_parses_body_into_children_so_math_renders() {
        // `quote`/`quotation` must NOT be opaque: the body is parsed so inline
        // and display math become real nodes (regression: math wasn't rendering).
        for env in ["quote", "quotation"] {
            let n = parse(&format!(
                "\\begin{{{env}}}\ninline $E=mc^2$ here\n\\begin{{equation}}\na=b\n\\end{{equation}}\n\\end{{{env}}}\n"
            ));
            let q = n
                .iter()
                .find(|node| matches!(&node.kind, NodeKind::Quote { env: e } if e == env))
                .unwrap_or_else(|| panic!("a Quote node for {env}"));
            assert!(
                q.children
                    .iter()
                    .any(|c| matches!(&c.kind, NodeKind::InlineMath(s) if s.contains("mc^2"))),
                "inline math inside {env} was not parsed: {:?}",
                q.children
            );
            assert!(
                q.children
                    .iter()
                    .any(|c| matches!(&c.kind, NodeKind::DisplayMath { .. })),
                "display math inside {env} was not parsed: {:?}",
                q.children
            );
        }
    }

    #[test]
    fn alignment_envs_keep_semantics_and_parse_their_body() {
        for (env, expected) in [
            ("center", TextAlignment::Center),
            ("flushleft", TextAlignment::FlushLeft),
            ("flushright", TextAlignment::FlushRight),
        ] {
            let n = parse(&format!(
                "\\begin{{{env}}}\nText $E=mc^2$ and \\ref{{eq:x}}.\n\\end{{{env}}}\n"
            ));
            let alignment = n
                .iter()
                .find(
                    |node| matches!(&node.kind, NodeKind::Alignment { kind } if *kind == expected),
                )
                .unwrap_or_else(|| panic!("an Alignment node for {env}"));
            assert!(alignment
                .children
                .iter()
                .any(|child| matches!(&child.kind, NodeKind::InlineMath(s) if s == "E=mc^2")));
            assert!(alignment
                .children
                .iter()
                .any(|child| matches!(&child.kind, NodeKind::Ref { key, .. } if key == "eq:x")));
        }
    }

    #[test]
    fn callout_default_title_and_non_title_optional_arg() {
        // No optional arg → env's default title.
        let n = parse("\\begin{note}\nx\n\\end{note}\n");
        let title = n.iter().find_map(|node| match &node.kind {
            NodeKind::Callout { title, .. } => Some(title.clone()),
            _ => None,
        });
        assert_eq!(title.flatten().as_deref(), Some("Note"));

        // `quoted`'s optional arg is a color, not a title — it must be consumed
        // and ignored, not rendered as the title.
        let n2 = parse("\\begin{quoted}[cyan!75!black]\nx\n\\end{quoted}\n");
        let t2 = n2.iter().find_map(|node| match &node.kind {
            NodeKind::Callout { title, .. } => Some(title.clone()),
            _ => None,
        });
        assert_eq!(t2.flatten().as_deref(), Some("Quote"));
    }

    #[test]
    fn verbatim_env_stays_opaque() {
        let n = parse("\\begin{verbatim}\n$x$\n\\end{verbatim}\n");
        assert!(n.iter().any(
            |node| matches!(&node.kind, NodeKind::OpaqueEnv { env, .. } if env == "verbatim")
        ));
    }

    #[test]
    fn literal_env_capture_ignores_inline_fake_closer() {
        let n = parse(concat!(
            "\\begin{verbatim}\n",
            "print(\"\\end{verbatim}\")\n",
            "still literal $raw$\n",
            "\\end{verbatim}\n",
            "After $live$.\n",
        ));
        assert!(n.iter().any(|node| matches!(
            &node.kind,
            NodeKind::OpaqueEnv { env, body }
                if env == "verbatim"
                    && body.contains(r#"print("\end{verbatim}")"#)
                    && body.contains("still literal $raw$")
        )));
        let math: Vec<&str> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::InlineMath(body) => Some(body.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(math, ["live"]);
    }

    #[test]
    fn alltt_uses_its_valid_inline_closer_but_keeps_body_opaque() {
        let n = parse("\\begin{alltt}raw $x$\\end{alltt} After $y$.\n");
        assert!(n.iter().any(|node| matches!(
            &node.kind,
            NodeKind::OpaqueEnv { env, body } if env == "alltt" && body.contains("raw $x$")
        )));
        assert!(tree_has_text(&n, "After"));
        let math: Vec<&str> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::InlineMath(body) => Some(body.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(math, ["y"]);
    }

    #[test]
    fn declared_literal_environments_stay_opaque() {
        let n = parse_with_preamble(
            concat!(
                "\\DefineVerbatimEnvironment{Transcript}{Verbatim}{}\n",
                "\\lstnewenvironment{Code}{}{}\n",
                "\\newminted{python}{}\n",
            ),
            concat!(
                "\\begin{Transcript}\n$x$ \\begin{tikzpicture}\n\\end{Transcript}\n",
                "\\begin{Code}\n$y$ \\begin{tikzpicture}\n\\end{Code}\n",
                "\\begin{pythoncode}\n$z$ \\begin{tikzpicture}\n\\end{pythoncode}\n",
            ),
        );
        for env in ["Transcript", "Code", "pythoncode"] {
            assert!(
                n.iter().any(
                    |node| matches!(&node.kind, NodeKind::OpaqueEnv { env: actual, .. } if actual == env)
                ),
                "declared literal environment {env} was not opaque: {n:#?}"
            );
            assert!(!n.iter().any(|node| matches!(
                &node.kind,
                NodeKind::UnsupportedEnvBoundary { env: actual, .. } if actual == env
            )));
        }
        assert!(!n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(_))));
    }

    #[test]
    fn unsupported_env_boundaries_wrap_normally_parsed_content() {
        let n = parse(concat!(
            "\\begin{mystery}[title]{argument}\n",
            "Text $x$ and \\ref{eq:k}.\n",
            "\\begin{inner}Nested.\\end{inner}\n",
            "\\end{mystery}\n",
        ));
        let boundaries: Vec<(&str, EnvironmentBoundary)> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::UnsupportedEnvBoundary { env, boundary } => {
                    Some((env.as_str(), *boundary))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            boundaries,
            [
                ("mystery", EnvironmentBoundary::Begin),
                ("inner", EnvironmentBoundary::Begin),
                ("inner", EnvironmentBoundary::End),
                ("mystery", EnvironmentBoundary::End),
            ]
        );
        assert!(tree_has_text(&n, "[title]{argument}"));
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "x")));
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::Ref { key, .. } if key == "eq:k")));
        assert!(!n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::OpaqueEnv { env, .. } if env == "mystery")));
    }

    #[test]
    fn unsupported_env_accepts_comment_between_begin_and_name() {
        let n = parse(concat!(
            "\\begin% continued control word\n",
            "{mystery}\n",
            "Body $x$.\n",
            "\\end{mystery}\n",
        ));
        let boundaries: Vec<EnvironmentBoundary> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::UnsupportedEnvBoundary { env, boundary } if env == "mystery" => {
                    Some(*boundary)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            boundaries,
            [EnvironmentBoundary::Begin, EnvironmentBoundary::End]
        );
        assert!(tree_has_text(&n, "Body"));
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "x")));
    }

    #[test]
    fn unsupported_env_matching_ignores_literal_and_commented_fake_ends() {
        let n = parse(
            r#"\begin{outer}
% \end{outer}
\verb|\end{outer}| still here.
\\end{outer} is text after a line break.
\begin{verbatim}
\end{outer}
\end{verbatim}
After $y$.
\end%
{outer}
"#,
        );
        let boundaries: Vec<EnvironmentBoundary> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::UnsupportedEnvBoundary { env, boundary } if env == "outer" => {
                    Some(*boundary)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            boundaries,
            [EnvironmentBoundary::Begin, EnvironmentBoundary::End]
        );
        assert!(tree_has_text(&n, "After"));
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "y")));
    }

    #[test]
    fn unsupported_env_matching_ignores_nonexecuted_end_tokens() {
        let n = parse(
            r#"\begin{outer}
\iffalse \end{outer} \fi
\def\x{\end{outer}}
\newcommand{\y}{\end{outer}}
\string\end{outer}
\detokenize{\end{outer}}
After $y$.
\end{outer}
"#,
        );
        let boundaries: Vec<EnvironmentBoundary> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::UnsupportedEnvBoundary { env, boundary } if env == "outer" => {
                    Some(*boundary)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            boundaries,
            [EnvironmentBoundary::Begin, EnvironmentBoundary::End]
        );
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "y")));
    }

    #[test]
    fn unsupported_env_matching_skips_false_branch_of_iftrue() {
        let n = parse(concat!(
            "\\begin{mystery}\n",
            "\\iftrue Visible one.\\else \\end{mystery}\\fi\n",
            "Visible two.\n",
            "\\end{mystery}\n",
        ));
        assert!(tree_has_text(&n, "Visible one"));
        assert!(tree_has_text(&n, "Visible two"));
        assert!(!tree_has_text(&n, "mystery"), "{n:#?}");
        let boundaries: Vec<EnvironmentBoundary> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::UnsupportedEnvBoundary { env, boundary } if env == "mystery" => {
                    Some(*boundary)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            boundaries,
            [EnvironmentBoundary::Begin, EnvironmentBoundary::End]
        );
    }

    #[test]
    fn inline_literal_commands_stay_inert_inside_unsupported_environment() {
        let n = parse(concat!(
            "\\begin{mystery}\n",
            "\\verb|literal $x$ \\begin{tikzpicture}|\n",
            r"\Verb[formatcom=\itshape]|also $v$ \end{mystery}|",
            "\n",
            "\\lstinline|listed $y$ \\end{mystery}|\n",
            "\\mintinline{python}{minted $z$ \\end{mystery}}\n",
            "After $live$.\n",
            "\\end{mystery}\n",
        ));
        let literals: Vec<&str> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::OpaqueCmd { name, raw } if name == "inline-literal" => Some(raw.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(literals.len(), 4, "{n:#?}");
        assert!(literals.iter().any(|raw| raw.contains("literal $x$")));
        assert!(literals.iter().any(|raw| raw.contains("listed $y$")));
        assert!(literals.iter().any(|raw| raw.contains("minted $z$")));
        let math: Vec<&str> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::InlineMath(body) => Some(body.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(math, ["live"]);
        assert!(!n.iter().any(
            |node| matches!(&node.kind, NodeKind::OpaqueEnv { env, .. } if env == "tikzpicture")
        ));
    }

    #[test]
    fn declared_inline_literal_commands_stay_inert() {
        let n = parse_with_preamble(
            "\\newmintinline[py]{python}{}\n\\newmint[code]{python}{}\n",
            concat!(
                "\\begin{mystery}\n",
                "\\py{literal $x$ \\end{mystery}}\n",
                "\\code|listed $y$ \\end{mystery}|\n",
                "After $live$.\n",
                "\\end{mystery}\n",
            ),
        );
        let literals: Vec<&str> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::OpaqueCmd { name, raw } if name == "inline-literal" => Some(raw.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(literals.len(), 2, "{n:#?}");
        assert!(literals.iter().any(|raw| raw.contains("literal $x$")));
        assert!(literals.iter().any(|raw| raw.contains("listed $y$")));
        let math: Vec<&str> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::InlineMath(body) => Some(body.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(math, ["live"]);
    }

    #[test]
    fn native_letter_retains_structure_and_parses_message_body() {
        let n = parse(concat!(
            "\\begin{letter}{Charles Babbage\\\\London}\n",
            "\\opening{Dear Charles,}\n",
            "\\verb|\\end{letter}| still inside.\n",
            "The engine satisfies $e^{i\\pi}+1=0$.\n",
            "\\closing{Yours sincerely,}\n",
            "\\end{letter}\n",
        ));
        let letter = n
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Letter { .. }))
            .expect("native letter");
        assert!(matches!(
            &letter.kind,
            NodeKind::Letter { recipient } if recipient == r"Charles Babbage\\London"
        ));
        assert!(letter.children.iter().any(|node| matches!(
            &node.kind,
            NodeKind::LetterOpening { text } if text == "Dear Charles,"
        )));
        assert!(letter.children.iter().any(|node| matches!(
            &node.kind,
            NodeKind::InlineMath(body) if body == r"e^{i\pi}+1=0"
        )));
        assert!(letter.children.iter().any(|node| matches!(
            &node.kind,
            NodeKind::LetterClosing { text } if text == "Yours sincerely,"
        )));
        assert!(!letter.children.iter().any(|node| matches!(
            &node.kind,
            NodeKind::UnsupportedEnvBoundary { env, .. } if env == "letter"
        )));
    }

    #[test]
    fn incomplete_letter_recipient_does_not_swallow_opening() {
        let n = parse("\\begin{letter}\\opening{Dear reader,}\nBody $x$.");
        let letter = n
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Letter { .. }))
            .expect("native letter");
        assert!(matches!(
            &letter.kind,
            NodeKind::Letter { recipient } if recipient.is_empty()
        ));
        assert!(letter
            .children
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::LetterOpening { .. })));
        assert!(letter
            .children
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "x")));
    }

    #[test]
    fn native_letter_commands_accept_comments_before_arguments() {
        let n = parse(concat!(
            "\\begin{letter}{Recipient}\n",
            "\\opening% greeting note\n",
            "{Dear reader,}\n",
            "Body.\n",
            "\\closing% closing note\n",
            "{Regards,}\n",
            "\\end{letter}\n",
        ));
        let letter = n
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Letter { .. }))
            .expect("native letter");
        assert!(letter.children.iter().any(|node| matches!(
            &node.kind,
            NodeKind::LetterOpening { text } if text == "Dear reader,"
        )));
        assert!(letter.children.iter().any(|node| matches!(
            &node.kind,
            NodeKind::LetterClosing { text } if text == "Regards,"
        )));
    }

    #[test]
    fn user_environment_matching_ignores_inline_literal_fake_closer() {
        let n = parse_with_preamble(
            "\\newenvironment{letterpreview}{\\begin{quote}}{\\end{quote}}\n",
            concat!(
                "\\begin{letterpreview}\n",
                "\\verb|\\end{letterpreview}| still inside. After $x$.\n",
                "\\end{letterpreview}\n",
            ),
        );
        let quote = n
            .iter()
            .find(|node| matches!(&node.kind, NodeKind::Quote { .. }))
            .expect("replacement quote");
        assert!(quote
            .children
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "x")));
        assert!(quote.children.iter().any(|node| matches!(
            &node.kind,
            NodeKind::OpaqueCmd { name, raw }
                if name == "inline-literal" && raw == r"\end{letterpreview}"
        )));
    }

    #[test]
    fn malformed_environment_tokens_do_not_hide_real_outer_closer() {
        let mut src = "\\begin{outer}\n".to_string();
        for _ in 0..2000 {
            src.push_str("\\begin{");
        }
        src.push_str("\n\\end{outer}\n");
        let n = parse(&src);
        let boundaries: Vec<EnvironmentBoundary> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::UnsupportedEnvBoundary { env, boundary } if env == "outer" => {
                    Some(*boundary)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            boundaries,
            [EnvironmentBoundary::Begin, EnvironmentBoundary::End]
        );
    }

    #[test]
    fn standard_math_wrappers_do_not_use_unsupported_fallback() {
        let n = parse(concat!(
            "\\begin{math}a+b\\end{math}\n",
            "\\begin{empheq}[box=\\fbox]{align}c&=d\\end{empheq}\n",
            "\\begin{circuitikz}\\draw (0,0)--(1,1);\\end{circuitikz}\n",
            "\\begin{forest}[A [B] [C]]\\end{forest}\n",
        ));
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "a+b")));
        assert!(n.iter().any(|node| matches!(
            &node.kind,
            NodeKind::DisplayMath { env: Some(env), body, .. }
                if env == "align" && body.contains("c&=d")
        )));
        assert!(n.iter().any(
            |node| matches!(&node.kind, NodeKind::OpaqueEnv { env, .. } if env == "circuitikz")
        ));
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::OpaqueEnv { env, .. } if env == "forest")));
        assert!(!n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::UnsupportedEnvBoundary { .. })));
    }

    #[test]
    fn specialized_diagram_capture_ignores_commented_and_stringified_closers() {
        let n = parse(concat!(
            "\\begin{tikzpicture}\n",
            "% \\end{tikzpicture}\n",
            "\\node{\\texttt{\\string\\end{tikzpicture}}};\n",
            "\\draw (0,0)--(1,1);\n",
            "\\end{tikzpicture}\n",
            "After.\n",
        ));
        assert!(n.iter().any(|node| matches!(
            &node.kind,
            NodeKind::OpaqueEnv { env, body }
                if env == "tikzpicture"
                    && body.contains(r"\draw (0,0)--(1,1);")
                    && body.contains(r"\string\end{tikzpicture}")
        )));
        assert!(tree_has_text(&n, "After"));
    }

    #[test]
    fn nested_same_name_unsupported_envs_balance_in_source_order() {
        let n = parse(concat!(
            "\\begin{mystery}Outer\n",
            "\\begin{mystery}Inner $z$.\\end{mystery}\n",
            "Tail.\\end{mystery}\n",
        ));
        let boundaries: Vec<EnvironmentBoundary> = n
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::UnsupportedEnvBoundary { env, boundary } if env == "mystery" => {
                    Some(*boundary)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            boundaries,
            [
                EnvironmentBoundary::Begin,
                EnvironmentBoundary::Begin,
                EnvironmentBoundary::End,
                EnvironmentBoundary::End,
            ]
        );
        assert!(tree_has_text(&n, "Outer"));
        assert!(tree_has_text(&n, "Inner"));
        assert!(tree_has_text(&n, "Tail"));
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "z")));
    }

    #[test]
    fn many_sequential_unsupported_envs_keep_independent_boundaries() {
        let mut src = String::new();
        for i in 0..1500 {
            src.push_str(&format!("\\begin{{wrapper}}item {i}\\end{{wrapper}}\n"));
        }
        let n = parse(&src);
        assert_eq!(
            n.iter()
                .filter(|node| matches!(
                    &node.kind,
                    NodeKind::UnsupportedEnvBoundary {
                        boundary: EnvironmentBoundary::Begin,
                        ..
                    }
                ))
                .count(),
            1500
        );
        assert_eq!(
            n.iter()
                .filter(|node| matches!(
                    &node.kind,
                    NodeKind::UnsupportedEnvBoundary {
                        boundary: EnvironmentBoundary::End,
                        ..
                    }
                ))
                .count(),
            1500
        );
        assert!(!n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::OpaqueEnv { .. })));
    }

    #[test]
    fn unclosed_unsupported_env_keeps_remainder_inert_and_marks_missing_end() {
        let n = parse("\\begin{mystery}Text $x$ and \\begin{tikzpicture}.");
        assert!(matches!(
            n.first().map(|node| &node.kind),
            Some(NodeKind::UnsupportedEnvBoundary {
                env,
                boundary: EnvironmentBoundary::Begin,
            }) if env == "mystery"
        ));
        assert!(n.iter().any(
            |node| matches!(&node.kind, NodeKind::OpaqueEnv { env, body } if env == "mystery" && body.contains("$x$"))
        ));
        assert!(matches!(
            n.last().map(|node| &node.kind),
            Some(NodeKind::UnsupportedEnvBoundary {
                env,
                boundary: EnvironmentBoundary::MissingEnd,
            }) if env == "mystery"
        ));
        assert!(!n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(_))));
    }

    #[test]
    fn literal_and_special_environments_never_take_transparent_fallback() {
        for env in LITERAL_ENVS.iter().chain(SPECIAL_OPAQUE_ENVS) {
            let src = format!(
                "\\begin{{{env}}}\n$x$ \\section{{No}} \\begin{{tikzpicture}}\n\\end{{{env}}}\n"
            );
            let n = parse(&src);
            assert!(
                n.iter()
                    .any(|node| matches!(&node.kind, NodeKind::OpaqueEnv { env: actual, .. } if actual == env)),
                "{env} did not stay opaque: {n:#?}"
            );
            assert!(
                !n.iter().any(|node| matches!(
                    &node.kind,
                    NodeKind::UnsupportedEnvBoundary { env: actual, .. } if actual == env
                )),
                "{env} got unsupported markers: {n:#?}"
            );
            assert!(
                !n.iter()
                    .any(|node| matches!(&node.kind, NodeKind::InlineMath(_))),
                "{env} parsed literal math: {n:#?}"
            );
        }
    }

    #[test]
    fn math_delimiter_balance_ignores_inert_source() {
        for source in [
            r"\newcommand{\dollar}{$}",
            r"\def\dollar{$}",
            "\\newcommand% note\n{\\dollar}{$}",
            "\\def% note\n\\dollar{$}",
            "\\newenvironment% note\n{money}{$}{$}",
            r"\iffalse $ \fi",
            r"\iftrue visible\else $ \fi",
            r"\string$",
            r"\detokenize{$}",
            "\\begin{alltt}raw $ and \\( text\\end{alltt}",
            "% unmatched $ in a comment\nVisible.",
        ] {
            assert!(
                has_balanced_math_delimiters(source, ""),
                "inert delimiter affected balance: {source:?}"
            );
        }

        assert!(has_balanced_math_delimiters(
            "\\begin{Transcript}$ raw\\end{Transcript}",
            "\\DefineVerbatimEnvironment{Transcript}{Verbatim}{}",
        ));
        assert!(!has_balanced_math_delimiters("live $x", ""));
        assert!(!has_balanced_math_delimiters(r"live \[x", ""));
        assert!(!has_balanced_math_delimiters(
            "\\newenvironment% note\n{mystery}{}{} live $x",
            "",
        ));
        assert!(!has_balanced_math_delimiters(
            r"\newenvironment{mystery} live $x",
            "",
        ));
    }

    #[test]
    fn live_braced_values_ignore_stored_definitions_and_literal_payloads() {
        let values = last_live_braced_command_values(
            concat!(
                "\\date{Live date}\n",
                "\\address{Live address}\n",
                "\\newcommand{\\stored}{\\date{Stored date}\\address{Stored address}}\n",
                "\\def\\alsoStored{\\signature{Stored signature}}\n",
                "\\newenvironment{storedenv}{\\name{Stored name}}{\\location{Stored location}}\n",
                "\\verb|\\date{Literal date}|\n",
                "\\detokenize{\\address{Literal address}}\n",
                "\\begin{verbatim}\n",
                "\\telephone{Literal telephone}\n",
                "\\end{verbatim}\n",
            ),
            &[
                "date",
                "name",
                "address",
                "signature",
                "location",
                "telephone",
            ],
        );

        assert_eq!(values.get("date").map(String::as_str), Some("Live date"));
        assert_eq!(
            values.get("address").map(String::as_str),
            Some("Live address")
        );
        for absent in ["name", "signature", "location", "telephone"] {
            assert!(!values.contains_key(absent), "{absent} leaked: {values:?}");
        }
    }

    #[test]
    fn live_braced_values_follow_conditionals_and_preserve_empty_setters() {
        let values = last_live_braced_command_values(
            concat!(
                "\\date{First date}\n",
                "\\iffalse\\date{Hidden date}\\else\\name{Visible name}\\fi\n",
                "\\iftrue\\address{Visible address}\\else\\address{Hidden address}\\fi\n",
                "\\signature{Before empty}\n",
                "\\iffalse\\signature{Hidden signature}\\fi\n",
                "\\signature{}\n",
                "\\date% continued setter\n",
                "{Last date}\n",
            ),
            &["date", "name", "address", "signature"],
        );

        assert_eq!(values.get("date").map(String::as_str), Some("Last date"));
        assert_eq!(values.get("name").map(String::as_str), Some("Visible name"));
        assert_eq!(
            values.get("address").map(String::as_str),
            Some("Visible address")
        );
        assert_eq!(values.get("signature").map(String::as_str), Some(""));
    }

    // Recursively true if any node's rendered-ish text contains `needle`.
    fn tree_has_text(nodes: &[Node], needle: &str) -> bool {
        nodes.iter().any(|n| {
            let here = match &n.kind {
                NodeKind::Text(s) => s.contains(needle),
                NodeKind::OpaqueEnv { body, .. } => body.contains(needle),
                NodeKind::OpaqueCmd { raw, .. } => raw.contains(needle),
                _ => false,
            };
            here || tree_has_text(&n.children, needle)
        })
    }

    #[test]
    fn comment_env_body_discarded() {
        // The `comment` package's env is dropped, not shown as an opaque block.
        let n = parse(concat!(
            "Before\n",
            "\\begin{comment}\n",
            "print(\"\\end{comment}\")\n",
            "SECRET $x$\n",
            "\\end{comment}\n",
            "After\n",
        ));
        assert!(tree_has_text(&n, "Before"));
        assert!(tree_has_text(&n, "After"));
        assert!(!tree_has_text(&n, "SECRET"), "comment body leaked: {n:#?}");
        assert!(
            !n.iter()
                .any(|x| matches!(&x.kind, NodeKind::OpaqueEnv { env, .. } if env == "comment")),
            "comment became an opaque block"
        );
    }

    #[test]
    fn iffalse_block_discarded() {
        let n = parse("Keep \\iffalse HIDDEN $y$ \\fi kept\n");
        assert!(tree_has_text(&n, "Keep"));
        assert!(tree_has_text(&n, "kept"));
        assert!(!tree_has_text(&n, "HIDDEN"), "iffalse body leaked: {n:#?}");
    }

    #[test]
    fn iffalse_nested_conditionals_balanced() {
        // The inner \iftrue…\fi must not let the wrong \fi close the block.
        let n = parse("A \\iffalse x \\iftrue y \\fi z \\fi B\n");
        assert!(tree_has_text(&n, "A"));
        assert!(tree_has_text(&n, "B"));
        for leaked in ["x", "y", "z"] {
            assert!(!tree_has_text(&n, leaked), "leaked {leaked:?}: {n:#?}");
        }
    }

    #[test]
    fn iffalse_else_renders_false_branch() {
        // `\iffalse` is false → the \else branch renders, the true branch drops.
        let n = parse("\\iffalse TRUEBRANCH \\else FALSEBRANCH \\fi\n");
        assert!(tree_has_text(&n, "FALSEBRANCH"), "else branch missing: {n:#?}");
        assert!(!tree_has_text(&n, "TRUEBRANCH"), "true branch leaked");
    }

    #[test]
    fn iftrue_content_still_renders() {
        // Only `\iffalse` is skipped; `\iftrue` content is kept (its markers are
        // inert unknown commands).
        let n = parse("\\iftrue KEEPME \\fi\n");
        assert!(tree_has_text(&n, "KEEPME"), "iftrue body dropped: {n:#?}");
    }

    #[test]
    fn deeply_nested_environments_do_not_overflow_stack() {
        // Regression: recognized container environments recursed one stack
        // frame per nesting level with no cap, so deeply nested input aborted
        // the process. Far past MAX_NESTING_DEPTH must still return (the excess
        // is captured as opaque blocks) rather than overflowing the stack.
        let depth = MAX_NESTING_DEPTH as usize + 5000;
        let mut src = String::with_capacity(depth * 28);
        for _ in 0..depth {
            src.push_str(r"\begin{center}");
        }
        src.push('x');
        for _ in 0..depth {
            src.push_str(r"\end{center}");
        }
        let nodes = parse(&src);
        assert!(!nodes.is_empty());
    }

    #[test]
    fn blank_line_between_inline_math_is_preserved() {
        let n = parse("$a$\n\n$b$");
        assert_eq!(n.len(), 3);
        assert!(matches!(&n[0].kind, NodeKind::InlineMath(s) if s == "a"));
        assert!(matches!(&n[1].kind, NodeKind::Text(s) if s == "\n\n"));
        assert!(matches!(&n[2].kind, NodeKind::InlineMath(s) if s == "b"));
    }

    #[test]
    fn display_math_brackets() {
        let n = parse(r"\[ a = b \]");
        assert!(
            matches!(&n[0].kind, NodeKind::DisplayMath { body, env, .. } if body.trim() == "a = b" && env.is_none())
        );
    }

    #[test]
    fn equation_env() {
        let n = parse("\\begin{equation}\n  E = mc^2 \\label{eq:e}\n\\end{equation}");
        match &n[0].kind {
            NodeKind::DisplayMath { env, label, .. } => {
                assert_eq!(env.as_deref(), Some("equation"));
                assert_eq!(label.as_deref(), Some("eq:e"));
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn subequations_preserve_group_boundary_and_label() {
        let n = parse(
            "\\begin{subequations}\n\\label{eq:group}\n\\begin{equation}\na=b\n\\end{equation}\nwith data\n\\begin{equation*}\nc=d\n\\end{equation*}\n\\end{subequations}",
        );

        let NodeKind::Subequations { label, .. } = &n[0].kind else {
            panic!("got {:?}", n[0].kind);
        };
        assert_eq!(label.as_deref(), Some("eq:group"));
        assert!(!n[0].children.iter().any(
            |node| matches!(&node.kind, NodeKind::OpaqueCmd { name, raw } if name == "label" && raw.contains("eq:group"))
        ));
        let displays: Vec<_> = n[0]
            .children
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::DisplayMath { env, label, .. } => {
                    Some((env.as_deref(), label.as_deref()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(displays.len(), 2);
        assert_eq!(displays[0], (Some("equation"), None));
        assert_eq!(displays[1], (Some("equation*"), None));
        assert!(n[0]
            .children
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::Text(s) if s.contains("with data"))));
    }

    #[test]
    fn theorem_with_role() {
        let src = "\\begin{theorem}[role=main]{Main result}\\label{thm:main}\nfoo\n\\end{theorem}";
        let n = parse_with_preamble("\\newtheorem{theorem}{Theorem}\n", src);
        match &n[0].kind {
            NodeKind::Theorem {
                role,
                name,
                label,
                env,
                ..
            } => {
                assert_eq!(env, "theorem");
                assert_eq!(*role, Role::Main);
                assert_eq!(name.as_deref(), Some("Main result"));
                assert_eq!(label.as_deref(), Some("thm:main"));
            }
            other => panic!("got {:?}", other),
        }
        assert!(!n[0].children.iter().any(
            |node| matches!(&node.kind, NodeKind::OpaqueCmd { name, raw } if name == "label" && raw.contains("thm:main"))
        ));
    }

    #[test]
    fn theorem_keeps_outer_and_nested_equation_labels_distinct() {
        let src = concat!(
            "\\begin{theorem}\n",
            "Statement.\\label{thm:outer}\n",
            "\\begin{equation}\\label{eq:nested}E=mc^2\\end{equation}\n",
            "\\end{theorem}",
        );
        let n = parse_with_preamble("\\newtheorem{theorem}{Theorem}\n", src);

        let NodeKind::Theorem { label, .. } = &n[0].kind else {
            panic!("got {:?}", n[0].kind);
        };
        assert_eq!(label.as_deref(), Some("thm:outer"));
        assert!(!n[0].children.iter().any(
            |node| matches!(&node.kind, NodeKind::OpaqueCmd { name, raw } if name == "label" && raw.contains("thm:outer"))
        ));
        assert!(n[0].children.iter().any(|node| matches!(
            &node.kind,
            NodeKind::DisplayMath { label: Some(label), body, .. }
                if label == "eq:nested" && body.contains(r"\label{eq:nested}")
        )));
    }

    #[test]
    fn theorem_with_omitted() {
        let src = "\\begin{theorem}[role=omitted]\\omitref{Rudin}\nstmt\n\\end{theorem}";
        let n = parse_with_preamble("\\newtheorem{theorem}{Theorem}\n", src);
        match &n[0].kind {
            NodeKind::Theorem { role, omit_ref, .. } => {
                assert_eq!(*role, Role::Omitted);
                assert_eq!(omit_ref.as_deref(), Some("Rudin"));
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn undeclared_standard_theorem_uses_transparent_fallback() {
        let n = parse("\\begin{theorem}Text $x$.\\end{theorem}");
        assert!(matches!(
            n.first().map(|node| &node.kind),
            Some(NodeKind::UnsupportedEnvBoundary {
                env,
                boundary: EnvironmentBoundary::Begin,
            }) if env == "theorem"
        ));
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "x")));
        assert!(matches!(
            n.last().map(|node| &node.kind),
            Some(NodeKind::UnsupportedEnvBoundary {
                env,
                boundary: EnvironmentBoundary::End,
            }) if env == "theorem"
        ));
        assert!(!n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::Theorem { .. })));
    }

    #[test]
    fn newenvironment_named_theorem_expands_instead_of_becoming_a_card() {
        let n = parse_with_preamble(
            "\\newenvironment{theorem}{Before }{ after}\n",
            "\\begin{theorem}Body $x$\\end{theorem}",
        );
        assert!(!n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::Theorem { .. })));
        assert!(!n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::UnsupportedEnvBoundary { .. })));
        assert!(n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::InlineMath(body) if body == "x")));
        assert!(tree_has_text(&n, "Before"));
        assert!(tree_has_text(&n, "Body"));
        assert!(tree_has_text(&n, "after"));
    }

    #[test]
    fn theorem_environment_lookup_is_exact() {
        let n = parse_with_preamble(
            "\\newtheorem{lemma}{Lemma}\n",
            "\\begin{lemma*}Text $x$.\\end{lemma*}",
        );
        assert!(n.iter().any(|node| matches!(
            &node.kind,
            NodeKind::UnsupportedEnvBoundary { env, .. } if env == "lemma*"
        )));
        assert!(!n
            .iter()
            .any(|node| matches!(&node.kind, NodeKind::Theorem { .. })));
    }

    #[test]
    fn section() {
        let n = parse(r"\section{Intro}\label{sec:intro}");
        match &n[0].kind {
            NodeKind::Section {
                level,
                title,
                label,
                starred,
                ..
            } => {
                assert_eq!(*level, 2);
                assert_eq!(title, "Intro");
                assert_eq!(label.as_deref(), Some("sec:intro"));
                assert!(!starred);
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn starred_sectioning_commands_are_headings() {
        for (command, expected_level) in [
            ("part", 0),
            ("chapter", 1),
            ("section", 2),
            ("subsection", 3),
            ("subsubsection", 4),
            ("paragraph", 5),
            ("subparagraph", 6),
        ] {
            let source = format!(r"\{command}*{{Unnumbered}}\label{{sec:{command}}}");
            let expected_label = format!("sec:{command}");
            let nodes = parse(&source);
            assert_eq!(nodes.len(), 1, "{command}* should be one heading node");
            match &nodes[0].kind {
                NodeKind::Section {
                    level,
                    title,
                    label,
                    starred,
                    ..
                } => {
                    assert_eq!(*level, expected_level, "wrong level for {command}*");
                    assert_eq!(title, "Unnumbered");
                    assert_eq!(label.as_deref(), Some(expected_label.as_str()));
                    assert!(starred, "{command}* lost its star");
                }
                other => panic!("{command}* parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn starred_section_allows_tex_space_or_comments_before_star() {
        for source in [
            "\\section *{Spaced}",
            "\\section % continued\n*{Comment continued}",
        ] {
            let nodes = parse(source);
            assert!(matches!(
                nodes.as_slice(),
                [Node {
                    kind: NodeKind::Section { starred: true, .. },
                    ..
                }]
            ));
        }
    }

    #[test]
    fn appendix_command_is_marker() {
        let n = parse(r"\appendix\section{Derivation}");
        assert!(matches!(&n[0].kind, NodeKind::Appendix));
        assert!(matches!(&n[1].kind, NodeKind::Section { title, .. } if title == "Derivation"));
    }

    #[test]
    fn ref_and_cite() {
        let n = parse(r"see \cref{thm:main} from \cite{Smith2024}");
        assert!(n.iter().any(
            |n| matches!(&n.kind, NodeKind::Ref { kind: RefKind::Cref, key } if key == "thm:main")
        ));
        assert!(n
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::Cite { keys } if keys == &["Smith2024"])));
    }

    #[test]
    fn proof() {
        let src = "\\begin{proof}[of Lemma 1]\nQED.\n\\end{proof}";
        let n = parse(src);
        match &n[0].kind {
            NodeKind::Proof { of, role } => {
                assert_eq!(of.as_deref(), Some("of Lemma 1"));
                assert_eq!(*role, None);
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn proof_with_manual_role() {
        let src =
            "\\begin{proof}[role=main, of={Proposition~\\ref{prop:main}}]\nQED.\n\\end{proof}";
        let n = parse(src);
        match &n[0].kind {
            NodeKind::Proof { of, role } => {
                assert_eq!(of.as_deref(), Some("of Proposition~\\ref{prop:main}"));
                assert_eq!(*role, Some(Role::Main));
            }
            other => panic!("got {:?}", other),
        }
    }
}
