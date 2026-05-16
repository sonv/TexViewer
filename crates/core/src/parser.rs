//! Hand-rolled LaTeX body parser.
//!
//! Scope (DESIGN §7): document structure, theorem-like environments with
//! `[role=...]`, sectioning, math delimiters and named math environments,
//! `\ref` / `\eqref` / `\cref` / `\Cref` / `\autoref` / `\pageref` / `\nameref`,
//! `\cite` / `\citet` / `\citep` / `\parencite` / `\textcite`, `\label`, and
//! `\omitref`. Everything else passes through as opaque tokens. Source
//! positions survive to every leaf.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::ast::{ListKind, Node, NodeKind, Pos, RefKind, Role, Span, THEOREM_LIKES};
use crate::project::Project;

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
    "split",
];

/// Parse every project file's body into a flat node list, preserving include
/// order.
pub fn parse_body(project: &Project) -> Result<Vec<Node>> {
    let mut nodes = Vec::new();
    for f in &project.files {
        let mut p = Parser::new_at(&f.source, f.path.clone(), f.start);
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
}

impl<'a> Parser<'a> {
    fn new_at(src: &'a str, file: PathBuf, start: Pos) -> Self {
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
            } else if b.is_ascii() || is_utf8_leading_byte(b) {
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
            if let Some(env) = stop_env {
                if self.starts_with(&format!("\\end{{{env}}}")) {
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
                    let slice = &self.src[self.byte..(self.byte + 2).min(self.bytes.len())];
                    text_buf.push_str(slice);
                    self.advance(slice.len());
                    continue;
                }
                let cmd = self.src[self.byte + 1..cmd_name_end].to_string();
                let cmd_start = self.pos();

                if cmd == "begin" {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.parse_environment(out, cmd_start);
                    continue;
                }

                // Sectioning.
                if let Some(level) = section_level(&cmd) {
                    flush_text(&mut text_buf, &mut text_start, out, self.pos(), &self.file);
                    self.advance_to(cmd_name_end);
                    self.skip_optional_arg();
                    let title = self.balanced_brace_arg().unwrap_or_default();
                    let mut node = Node {
                        kind: NodeKind::Section {
                            level,
                            title,
                            label: None,
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
        let env = match self.balanced_brace_arg() {
            Some(e) => e.trim().to_string(),
            None => return,
        };

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
                },
                span: self.span_from(start),
                children: vec![],
            });
            return;
        }

        // Theorem-like with optional [role=...]{name}.
        if THEOREM_LIKES.contains(&strip_star(&env)) {
            self.parse_theorem(out, start, env);
            return;
        }

        if env == "proof" {
            self.parse_proof(out, start);
            return;
        }

        if let Some(kind) = list_kind_for(&env) {
            self.parse_list(out, start, env, kind);
            return;
        }

        if env == "document" {
            // Defensive: someone slipped a nested document env. Treat as a
            // transparent block.
            let body_end = self.find_matching_end(&env);
            let mut children = Vec::new();
            let inner = self.src[self.byte..body_end].to_string();
            let mut sub = Parser::new_at(
                &self.src[self.byte..body_end],
                self.file.clone(),
                self.pos(),
            );
            sub.parse_block_into(&mut children, None);
            self.advance_to(body_end);
            self.advance(format!("\\end{{{env}}}").len());
            out.push(Node {
                kind: NodeKind::Document,
                span: self.span_from(start),
                children,
            });
            drop(inner);
            return;
        }

        // Opaque environment — capture body, leave for the renderer.
        let body_end = self.find_matching_end(&env);
        let body = self.src[self.byte..body_end].to_string();
        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());
        out.push(Node {
            kind: NodeKind::OpaqueEnv { env, body },
            span: self.span_from(start),
            children: vec![],
        });
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

        // Pre-scan inside for \label{...} and \omitref{...}.
        let label = find_first_label(inner_src);
        let omit_ref = if matches!(role, Role::Omitted) {
            find_first_omitref(inner_src)
        } else {
            None
        };

        let mut children = Vec::new();
        // Parse inner content with a sub-parser so nested commands work.
        let mut sub = Parser::new_at(inner_src, self.file.clone(), self.pos());
        sub.parse_block_into(&mut children, None);

        self.advance_to(body_end);
        self.advance(format!("\\end{{{env}}}").len());

        out.push(Node {
            kind: NodeKind::Theorem {
                env: strip_star(&env).to_string(),
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
        let of = self.optional_arg();

        let body_end = self.find_matching_end("proof");
        let inner = &self.src[self.byte..body_end];
        let mut children = Vec::new();
        let mut sub = Parser::new_at(inner, self.file.clone(), self.pos());
        sub.parse_block_into(&mut children, None);

        self.advance_to(body_end);
        self.advance("\\end{proof}".len());

        out.push(Node {
            kind: NodeKind::Proof { of },
            span: self.span_from(start),
            children,
        });
    }

    /// Find the end byte of the matching `\end{env}` from `self.byte`,
    /// respecting nested `\begin{env}` / `\end{env}` of the same env name.
    /// Returns the byte index of `\end`. If unmatched, returns end-of-source.
    fn find_matching_end(&self, env: &str) -> usize {
        let begin_tok = format!("\\begin{{{env}}}");
        let end_tok = format!("\\end{{{env}}}");
        let mut depth: i32 = 1;
        let mut i = self.byte;
        let bytes = self.bytes;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                if bytes[i..].starts_with(begin_tok.as_bytes()) {
                    depth += 1;
                    i += begin_tok.len();
                    continue;
                }
                if bytes[i..].starts_with(end_tok.as_bytes()) {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                    i += end_tok.len();
                    continue;
                }
            }
            i += 1;
        }
        bytes.len()
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
        let mut depth: i32 = 0;
        let mut i = self.byte;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b'\\' if i + 1 < self.bytes.len() => {
                    i += 2;
                    continue;
                }
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let inside = self.src[start + 1..i].to_string();
                        self.advance_to(i + 1);
                        return Some(inside);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
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
        for ch in self.src[..byte.min(self.bytes.len())].chars() {
            if ch == '\n' {
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

fn strip_star(env: &str) -> &str {
    env.strip_suffix('*').unwrap_or(env)
}

fn list_kind_for(env: &str) -> Option<ListKind> {
    Some(match env {
        "enumerate" | "enumerate*" | "compactenum" | "asparaenum" => ListKind::Enumerate,
        "itemize" | "itemize*" | "compactitem" | "asparaitem" => ListKind::Itemize,
        "description" | "compactdesc" => ListKind::Description,
        _ => return None,
    })
}

fn is_utf8_leading_byte(b: u8) -> bool {
    (b & 0b1100_0000) != 0b1000_0000
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

static LABEL_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"\\label\s*\{\s*([^}]+?)\s*\}").unwrap());

fn find_first_label(src: &str) -> Option<String> {
    LABEL_RE
        .captures(src)
        .map(|c| c.get(1).unwrap().as_str().to_string())
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
            files: vec![ProjectFile {
                path: PathBuf::from("t.tex"),
                source: src.to_string(),
                start: Pos::ZERO,
                is_root_body: true,
            }],
            warnings: vec![],
        };
        parse_body(&project).unwrap()
    }

    #[test]
    fn inline_math() {
        let n = parse(r"hello $x+y$ world");
        assert!(matches!(&n[1].kind, NodeKind::InlineMath(s) if s == "x+y"));
    }

    #[test]
    fn unicode_text_is_preserved() {
        let n = parse("Café naïve §");
        assert!(matches!(&n[0].kind, NodeKind::Text(s) if s == "Café naïve §"));
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
    fn theorem_with_role() {
        let src = "\\begin{theorem}[role=main]{Main result}\\label{thm:main}\nfoo\n\\end{theorem}";
        let n = parse(src);
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
    }

    #[test]
    fn theorem_with_omitted() {
        let src = "\\begin{theorem}[role=omitted]\\omitref{Rudin}\nstmt\n\\end{theorem}";
        let n = parse(src);
        match &n[0].kind {
            NodeKind::Theorem { role, omit_ref, .. } => {
                assert_eq!(*role, Role::Omitted);
                assert_eq!(omit_ref.as_deref(), Some("Rudin"));
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn section() {
        let n = parse(r"\section{Intro}\label{sec:intro}");
        match &n[0].kind {
            NodeKind::Section {
                level,
                title,
                label,
                ..
            } => {
                assert_eq!(*level, 2);
                assert_eq!(title, "Intro");
                assert_eq!(label.as_deref(), Some("sec:intro"));
            }
            other => panic!("got {:?}", other),
        }
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
            NodeKind::Proof { of } => assert_eq!(of.as_deref(), Some("of Lemma 1")),
            other => panic!("got {:?}", other),
        }
    }
}
