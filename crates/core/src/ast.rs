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
    /// Other `\begin{env}` ... `\end{env}` passed through opaquely.
    OpaqueEnv { env: String, body: String },
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
