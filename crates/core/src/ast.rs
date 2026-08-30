//! AST node types with source-position metadata on every node.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 1-indexed line and column, plus byte offset within the source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
    pub byte: u32,
}

impl Pos {
    pub const ZERO: Pos = Pos {
        line: 1,
        col: 1,
        byte: 0,
    };
}

/// Source range tied back to a specific file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub file: PathBuf,
    pub start: Pos,
    pub end: Pos,
}

/// Roles understood by the renderer. Unknown role strings degrade to `Standard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Main,
    Supporting,
    Standard,
    Omitted,
}

impl Role {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "main" => Role::Main,
            "supporting" => Role::Supporting,
            "standard" => Role::Standard,
            "omitted" => Role::Omitted,
            _ => Role::Standard,
        }
    }

    pub fn as_css_class(self) -> &'static str {
        match self {
            Role::Main => "role-main",
            Role::Supporting => "role-supporting",
            Role::Standard => "role-standard",
            Role::Omitted => "role-omitted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub kind: NodeKind,
    pub span: Span,
    pub children: Vec<Node>,
}

/// Which diagnostic marker represents an otherwise-unsupported environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentBoundary {
    Begin,
    End,
    MissingEnd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    /// Top-level document body.
    Document,
    /// Raw text run.
    Text(String),
    /// `\section{...}`, `\subsection{...}`, etc.
    Section {
        level: u8,
        title: String,
        label: Option<String>,
        number: Option<String>,
    },
    /// `\appendix` switches following section-scoped counters to appendix
    /// numbering (`A`, `A.1`, ...). It emits no visible content itself.
    Appendix,
    /// `\begin{theorem}[role=...]{name}` ... `\end{theorem}` (and friends).
    Theorem {
        env: String, // "theorem" / "lemma" / "proposition" / ...
        /// Heading word resolved from the preamble's `\newtheorem` title (or a
        /// default), e.g. "Theorem", "Lemma", "Satz". Set by the parser so the
        /// renderer doesn't need the theorem registry.
        #[serde(default)]
        kind_word: String,
        role: Role,
        name: Option<String>,
        label: Option<String>,
        number: Option<String>,
        omit_ref: Option<String>,
    },
    /// `\begin{proof}[role=...,name=...]` ... `\end{proof}`.
    Proof {
        of: Option<String>,
        role: Option<Role>,
    },
    /// Inline math `\(...\)` or `$...$`.
    InlineMath(String),
    /// Display math `\[...\]`, `$$...$$`, or `\begin{equation}`/`align`/etc.
    DisplayMath {
        body: String,
        env: Option<String>,
        label: Option<String>,
        number: Option<String>,
        row_numbers: Vec<Option<String>>,
    },
    /// `\begin{subequations}` ... `\end{subequations}`. The environment owns
    /// one parent equation number; numbered child equations get alphabetic
    /// suffixes such as `1a`, `1b`.
    Subequations {
        label: Option<String>,
        number: Option<String>,
    },
    /// `\ref{key}` / `\eqref{key}` / `\cref{key}` / `\autoref{key}`.
    Ref { kind: RefKind, key: String },
    /// `\cite{a,b,c}` etc.
    Cite { keys: Vec<String> },
    /// A raw/special `\begin{env}` ... `\end{env}` whose body must not be
    /// parsed as ordinary TeX (verbatim/code, floats, TikZ, or a recursion
    /// safety fallback).
    OpaqueEnv { env: String, body: String },
    /// Visible diagnostic boundary for an otherwise-unsupported environment.
    /// The body is parsed normally and flattened between its Begin and End
    /// markers so math, references, and nested structure remain useful.
    UnsupportedEnvBoundary {
        env: String,
        boundary: EnvironmentBoundary,
    },
    /// A recognized annotation / callout box (`\begin{todo}[title]` … etc.).
    /// Unlike `OpaqueEnv`, the body is parsed into `children` so math and nested
    /// content render. `title` is the resolved `[title]` (or a default); `class`
    /// is the CSS modifier (`todo`, `note`, `added`, …).
    Callout {
        env: String,
        class: String,
        title: Option<String>,
    },
    /// `\begin{quote}` / `\begin{quotation}` — an indented block quotation.
    /// Like `Callout`, the body is parsed into `children` so math and nested
    /// content render (an `OpaqueEnv` would emit the body as plain text, leaving
    /// math un-typeset). `env` distinguishes `quote` from `quotation`.
    Quote { env: String },
    /// `center`, `flushleft`, and `flushright` retain their TeX alignment while
    /// their body is parsed normally, so nested math and references still work.
    Alignment { kind: TextAlignment },
    /// A TeX-scoped `{\color[model]{name} ...}` group. Its body is parsed into
    /// children so citations, labels, display math, and nested structures keep
    /// their normal semantics while inheriting the color.
    TextColor {
        model: Option<String>,
        color: String,
    },
    /// A standard `letter` document-class letter. The retained container lets
    /// the renderer reproduce the address/date/recipient/closing geometry
    /// while its body remains ordinary parsed TeX.
    Letter { recipient: String },
    /// `\opening{...}` inside a native `letter`.
    LetterOpening { text: String },
    /// `\closing{...}` inside a native `letter`.
    LetterClosing { text: String },
    /// `\begin{abstract}` ... `\end{abstract}` rendered as front matter.
    Abstract,
    /// `\command[opt]{arg}{arg}` passed through opaquely.
    OpaqueCmd { name: String, raw: String },
    /// LaTeX comment; usually discarded by the renderer.
    Comment(String),
    /// `\printbibliography` or `\bibliography{...}` — placeholder for the
    /// rendered references section. Entries come from the project's loaded
    /// `.bib` files.
    Bibliography,
    /// `\maketitle` — placeholder for the title block. Body sourced from
    /// `\title{…}` / `\author{…}` / `\date{…}` in the preamble.
    MakeTitle,
    /// `\begin{enumerate|itemize|description}` … `\end`. Children are
    /// `ListItem` nodes in document order.
    List { kind: ListKind },
    /// `\item[marker] …` — child of a `List`. The marker is the optional
    /// argument used by `description` lists; numbering for enumerate is
    /// handled by the renderer via `<ol>`.
    ListItem { marker: Option<String> },
    /// A Markdown paragraph. Unlike LaTeX's implicit paragraph runs, the
    /// CommonMark frontend retains paragraph boundaries explicitly.
    MarkdownParagraph,
    /// A Markdown ATX or Setext heading. Inline formatting lives in
    /// `children`, so headings such as `## *Fast* math` keep their markup.
    MarkdownHeading {
        level: u8,
        /// Document-unique, human-readable fragment name derived from the
        /// visible heading text (for example `Fast math` → `fast-math`).
        anchor: String,
    },
    /// A configured fenced Markdown block (`:::name Optional title` … `:::`).
    /// The frontend recognizes only block names enabled in the resolved
    /// Markdown configuration; the body remains ordinary parsed Markdown.
    MarkdownCustomBlock {
        name: String,
        title: Option<String>,
        /// Position- and config-independent hash of the exact authored body.
        /// Live reveal state is restored only when this identity is unchanged.
        #[serde(default)]
        content_key: String,
    },
    /// Plain Markdown prose. It must never pass through the LaTeX text-mode
    /// command parser; the renderer only HTML-escapes it.
    MarkdownText(String),
    MarkdownEmphasis,
    MarkdownStrong,
    MarkdownStrikethrough,
    MarkdownLink {
        destination: String,
        title: Option<String>,
    },
    MarkdownImage {
        destination: String,
        title: Option<String>,
    },
    MarkdownBlockQuote,
    MarkdownInlineCode(String),
    MarkdownCodeBlock {
        language: Option<String>,
        code: String,
    },
    MarkdownList {
        ordered: bool,
        start: Option<u64>,
    },
    MarkdownListItem,
    MarkdownTaskMarker {
        checked: bool,
    },
    MarkdownTable {
        alignments: Vec<MarkdownAlignment>,
    },
    MarkdownTableHead,
    MarkdownTableRow,
    MarkdownTableCell,
    MarkdownDefinitionList,
    MarkdownDefinitionTerm,
    MarkdownDefinitionDescription,
    MarkdownSuperscript,
    MarkdownSubscript,
    MarkdownSoftBreak,
    MarkdownHardBreak,
    MarkdownRule,
    MarkdownFootnoteDefinition {
        label: String,
        /// Collision-free DOM-id suffix assigned by the Markdown frontend.
        target: String,
    },
    MarkdownFootnoteReference {
        label: String,
        /// DOM-id suffix of the parser-matched footnote definition.
        target: String,
    },
    /// Raw HTML is deliberately retained as inert source text. The renderer
    /// escapes it instead of inserting it into the document DOM.
    MarkdownRawHtml(String),
    MarkdownRawHtmlBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkdownAlignment {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextAlignment {
    Center,
    FlushLeft,
    FlushRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListKind {
    /// Numbered — `\begin{enumerate}` and friends.
    Enumerate,
    /// Bulleted — `\begin{itemize}`, etc.
    Itemize,
    /// Term/definition — `\begin{description}`.
    Description,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Ref,
    Eqref,
    Cref,
    Autoref,
    Pageref,
    Nameref,
}

impl RefKind {
    pub fn from_command(name: &str) -> Option<Self> {
        match name {
            "ref" => Some(Self::Ref),
            "eqref" => Some(Self::Eqref),
            "cref" | "Cref" => Some(Self::Cref),
            "autoref" => Some(Self::Autoref),
            "pageref" => Some(Self::Pageref),
            "nameref" => Some(Self::Nameref),
            _ => None,
        }
    }
}

/// Theorem-like environments that accept the `[role=...]` extension.
pub const THEOREM_LIKES: &[&str] = &[
    "theorem",
    "thm",
    "lemma",
    "lem",
    "proposition",
    "prop",
    "corollary",
    "cor",
    "definition",
    "defn",
    "defi",
    "remark",
    "rem",
    "example",
    "ex",
    "claim",
    "fact",
    "conjecture",
];
