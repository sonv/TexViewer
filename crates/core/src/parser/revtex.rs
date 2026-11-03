//! REVTeX declares front matter in the document, often before the abstract.
//! Record only complete, top-level calls before the actual title command. The
//! body parser consumes their exact ranges, preserving surrounding prose and
//! the original file positions even when an input divides the front matter.

use super::*;

pub(super) struct FrontMatter {
    pub(super) ends: HashMap<PathBuf, HashMap<u32, u32>>,
    title: (PathBuf, u32),
}

impl FrontMatter {
    pub(super) fn for_project(project: &Project) -> Option<Self> {
        let preamble = &project.preamble.source;
        if !preamble.contains("revtex") {
            return None;
        }
        let mut revtex = false;
        visit_commands(preamble, &mut 0, |start, word, end| {
            if word == "documentclass" {
                if let Some((_, value, _)) = argument(preamble, end) {
                    revtex = matches!(value.trim(), "revtex4" | "revtex4-1" | "revtex4-2");
                    return Control::Stop;
                }
            }
            Control::Next(stored_definition_end(preamble, start).unwrap_or(end))
        });
        if !revtex {
            return None;
        }

        let mut ends: HashMap<PathBuf, HashMap<u32, u32>> = HashMap::new();
        let mut nesting = 0;
        for file in &project.files {
            let mut title = None;
            visit_commands(&file.source, &mut nesting, |start, word, after_word| {
                if word == "maketitle" && !file.source[after_word..].starts_with('*') {
                    title = Some((file.path.clone(), file.start.byte + start as u32));
                    return Control::Stop;
                }
                let definition = stored_definition_end(&file.source, start);
                let field_end = match word {
                    "title" | "author" | "affiliation" | "email" | "date" | "thanks" => {
                        argument(&file.source, after_word).map(|(_, _, end)| end)
                    }
                    "noaffiliation" if !file.source[after_word..].starts_with('*') => {
                        Some(after_word)
                    }
                    "newcommand" | "renewcommand" | "providecommand" => definition,
                    _ => None,
                };
                if let Some(end) = field_end {
                    ends.entry(file.path.clone())
                        .or_default()
                        .insert(file.start.byte + start as u32, file.start.byte + end as u32);
                }
                Control::Next(field_end.or(definition).unwrap_or(after_word))
            });
            if let Some(title) = title {
                return (!ends.is_empty()).then_some(Self { ends, title });
            }
        }
        None
    }

    pub(super) fn attach(self, nodes: &mut Vec<Node>) {
        let Some(title_index) = nodes.iter().position(|n| {
            matches!(n.kind, NodeKind::MakeTitle)
                && n.span.file == self.title.0
                && n.span.start.byte == self.title.1
        }) else {
            return;
        };
        let mut following = nodes.split_off(title_index);
        nodes.retain(|n| {
            if matches!(n.kind, NodeKind::OpaqueCmd { .. })
                && self
                    .ends
                    .get(&n.span.file)
                    .and_then(|e| e.get(&n.span.start.byte))
                    == Some(&n.span.end.byte)
            {
                following[0].children.push(n.clone());
                false
            } else {
                true
            }
        });
        nodes.append(&mut following);
    }
}

pub(crate) fn revtex_title_argument(node: &Node) -> Option<(Option<String>, String)> {
    let NodeKind::OpaqueCmd { name, raw } = &node.kind else {
        return None;
    };
    if name == "noaffiliation" {
        return Some((None, String::new()));
    }
    let (optional, value, end) = argument(raw, 1 + name.len())?;
    (end == raw.len()).then_some((optional, value))
}

fn argument(source: &str, after_word: usize) -> Option<(Option<String>, String, usize)> {
    let mut at = skip_tex_space_and_comments(source, after_word);
    let optional = if source.as_bytes().get(at) == Some(&b'[') {
        let end = citation_optional_arg_end(source, at)?;
        let text = executable_latex_source(&source[at + 1..end - 1]);
        at = skip_tex_space_and_comments(source, end);
        Some(text)
    } else {
        None
    };
    let end = tex_group_end(source, at, b'{', b'}')?;
    Some((
        optional,
        executable_latex_source(&source[at + 1..end - 1]),
        end,
    ))
}

enum Control {
    Next(usize),
    Stop,
}

/// Visit executable, ungrouped control words outside environments and math.
/// Each client can consume a command's exact arguments or stop at its target.
fn visit_commands(
    src: &str,
    nesting: &mut usize,
    mut visit: impl FnMut(usize, &str, usize) -> Control,
) {
    let b = src.as_bytes();
    let mut i = 0;
    let mut hidden = Vec::new();
    let mut conditionals = ConditionalLookup::default();
    let mut math_close: Option<&str> = None;
    while i < b.len() {
        if let Some(end) = scheduled_hidden_resume(i, &mut hidden) {
            i = end;
            continue;
        }
        if b[i] == b'%' {
            i = skip_tex_space_and_comments(src, i);
            continue;
        }
        if b[i] == b'$' {
            let delimiter = if b.get(i + 1) == Some(&b'$') {
                "$$"
            } else {
                "$"
            };
            if math_close == Some(delimiter) {
                math_close = None;
            } else if math_close.is_none() {
                math_close = Some(delimiter);
            }
            i += delimiter.len();
            continue;
        }
        if b[i] == b'{' && math_close.is_none() {
            i = tex_group_end(src, i, b'{', b'}').unwrap_or(b.len());
            continue;
        }
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < b.len() && (b[i].is_ascii_alphabetic() || b[i] == b'@') {
            i += 1;
        }
        if i == start + 1 {
            if i < b.len() {
                i += src[i..].chars().next().expect("nonempty").len_utf8();
            }
            let symbol = &src[start..i];
            if math_close == Some(symbol) {
                math_close = None;
            } else if math_close.is_none() {
                math_close = match symbol {
                    r"\(" => Some(r"\)"),
                    r"\[" => Some(r"\]"),
                    _ => None,
                };
            }
            continue;
        }
        if math_close.is_some() {
            continue;
        }
        let word = &src[start + 1..i];
        if word == "iffalse" {
            i = conditionals
                .get(src, start, i)
                .map(|bounds| bounds.else_end.unwrap_or(bounds.fi_end))
                .unwrap_or(b.len());
            continue;
        }
        if word == "iftrue" {
            if let Some(bounds) = conditionals.get(src, start, i) {
                if let Some(start) = bounds.else_start {
                    hidden.push((start, bounds.fi_end));
                }
            }
            continue;
        }
        if is_inline_literal_command(word) {
            i = inline_literal_payload(src, word, i)
                .map(|(_, end)| end)
                .unwrap_or(b.len());
            continue;
        }
        if word == "string" {
            i = tex_token_end(src, skip_tex_space_and_comments(src, i));
            continue;
        }
        if let Some(span) = inert_environment_span_at(src, start) {
            i = span.end;
            continue;
        }
        if let Some(token) = environment_token_at(src, start) {
            match token.kind {
                EnvironmentTokenKind::Begin => {
                    if let Some((_, end)) =
                        find_matching_end_lexical_in(src, token.end, &token.name)
                    {
                        i = end;
                        continue;
                    }
                    // An include can split an environment across fragments.
                    *nesting += 1;
                }
                EnvironmentTokenKind::End => *nesting = nesting.saturating_sub(1),
            }
            i = token.end;
            continue;
        }
        if word == "begingroup" || word == "bgroup" {
            *nesting += 1;
            continue;
        }
        if word == "endgroup" || word == "egroup" {
            *nesting = nesting.saturating_sub(1);
            continue;
        }
        if *nesting == 0 {
            match visit(start, word, i) {
                Control::Next(end) => {
                    // Only a control word can introduce an optional argument.
                    // Literal brackets in prose are ordinary TeX characters.
                    let at = skip_tex_space_and_comments(src, end);
                    i = if end == i && b.get(at) == Some(&b'[') {
                        citation_optional_arg_end(src, at).unwrap_or(end)
                    } else {
                        end
                    };
                }
                Control::Stop => return,
            }
        } else if let Some(end) = stored_definition_end(src, start) {
            i = end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::load_project_from_source;

    fn parse(preamble: &str, body: &str) -> Vec<Node> {
        let source = format!("{preamble}\n\\begin{{document}}\n{body}\n\\end{{document}}\n");
        let project = load_project_from_source(Path::new("paper.tex"), source).unwrap();
        parse_body(&project, &TheoremRegistry::from_project(&project)).unwrap()
    }

    #[test]
    fn revtex_fields_consume_exact_arguments_and_keep_their_positions() {
        let body = r#"\title% comment
  [Short]{Long $N$}{preserve-this-group}
\newcommand{\note}% comment before replacement
  {Shared note.}
\author{Alice}\thanks{\note}\affiliation{A}
\author{Bob}\email[Contact: ]{bob@example.test}\noaffiliation
Prose with a half-open interval [a,b).
\begin{abstract}The interval [0,1) is half open.\end{abstract}
\maketitle
\author{After}"#;
        for class in ["revtex4", "revtex4-1", "revtex4-2"] {
            let nodes = parse(&format!("\\documentclass[preprint]{{{class}}}"), body);
            let title = nodes
                .iter()
                .find(|n| matches!(n.kind, NodeKind::MakeTitle))
                .unwrap();
            let names: Vec<_> = title
                .children
                .iter()
                .map(|n| match &n.kind {
                    NodeKind::OpaqueCmd { name, .. } => name.as_str(),
                    _ => panic!("unexpected title field"),
                })
                .collect();
            assert_eq!(
                names,
                [
                    "title",
                    "newcommand",
                    "author",
                    "thanks",
                    "affiliation",
                    "author",
                    "email",
                    "noaffiliation"
                ]
            );
            assert_eq!(
                revtex_title_argument(&title.children[0]),
                Some((Some("Short".into()), "Long $N$".into()))
            );
            assert_eq!(title.children[0].span.start.line, 3);
            assert_eq!(title.children[0].span.end.line, 4);
            assert!(nodes.iter().any(
                |n| matches!(&n.kind, NodeKind::Text(s) if s.contains("preserve-this-group"))
            ));
            assert!(nodes.iter().any(|n| matches!(&n.kind, NodeKind::OpaqueCmd { name, raw } if name == "author" && raw.contains("After"))));
            assert!(
                matches!(&title.children[1].kind, NodeKind::OpaqueCmd { raw, .. } if raw.contains("% comment before replacement"))
            );
        }
    }

    #[test]
    fn revtex_collection_ignores_inert_grouped_and_nested_declarations() {
        let body = r#"% \author{Commented}
{\author{Grouped}\maketitle}
\iffalse\author{Inactive}\maketitle\fi
\verb|\author{Literal}\maketitle|
\string\author{Stringified}
\begin{custom}\author{Nested}\maketitle\end{custom}
\newcommand{\factory}{\author{Stored}\maketitle}
\iftrue\author{Live}\else\author{Inactive}\fi
\maketitle"#;
        let nodes = parse(r"\documentclass{revtex4-2}", body);
        let owners: Vec<_> = nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::MakeTitle) && !n.children.is_empty())
            .collect();
        assert_eq!(owners.len(), 1);
        let title = owners[0];
        assert_eq!(title.span.start.line, 11);
        assert_eq!(title.children.len(), 2);
        assert_eq!(
            revtex_title_argument(&title.children[1]),
            Some((None, "Live".into()))
        );

        for preamble in [
            r"\newcommand{\hidden}{\documentclass{revtex4-2}}\documentclass{article}",
            r"\iffalse\documentclass{revtex4-2}\fi\documentclass{article}",
            r"{\documentclass{revtex4-2}}\documentclass{article}",
        ] {
            let nodes = parse(preamble, r"\author{Body author}\maketitle");
            assert!(nodes
                .iter()
                .any(|n| matches!(&n.kind, NodeKind::OpaqueCmd { name, .. } if name == "author")));
        }
    }
}
