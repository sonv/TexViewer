//! REVTeX declares the title and authors in the document body and prints them
//! at `\maketitle`, often after the abstract has already been read.

use crate::ast::{Node, NodeKind, Span};
use crate::macros::{self, ExtractedPreamble, MacroOverride};
use crate::parser::{self, revtex_title_argument};
use crate::project::Project;
use crate::TheoremRegistry;
use anyhow::Result;
use std::path::Path;

/// Parse the LaTeX body for rendering. The caller supplies a fresh copy of
/// any cached preamble because REVTeX title declarations can change with each
/// body edit. All source, conversion API and live preview paths use this step.
pub fn parse_latex_body(
    project: &Project,
    theorems: &TheoremRegistry,
    overrides: &[MacroOverride],
    preamble: &mut ExtractedPreamble,
) -> Result<Vec<Node>> {
    let mut body = parser::parse_body_with_overrides(project, theorems, overrides)?;
    let Some(title_node) = body
        .iter_mut()
        .find(|node| matches!(node.kind, NodeKind::MakeTitle) && !node.children.is_empty())
    else {
        return Ok(body);
    };

    macros::apply_title_definitions(
        preamble,
        title_node.children.iter().filter_map(title_definition),
        overrides,
    );
    // Keep each declaration's original span and command text for title links
    // and citation discovery. Definitions themselves have no visible content.
    title_node
        .children
        .retain(|node| title_definition(node).is_none());
    if let Some((short, _)) = title_node
        .children
        .iter()
        .rev()
        .find(|node| matches!(&node.kind, NodeKind::OpaqueCmd { name, .. } if name == "title"))
        .and_then(revtex_title_argument)
    {
        preamble.title_short = short;
    }
    Ok(body)
}

fn title_definition(node: &Node) -> Option<(&Path, &str)> {
    match &node.kind {
        NodeKind::OpaqueCmd { name, raw }
            if matches!(
                name.trim_end_matches('*'),
                "newcommand" | "renewcommand" | "providecommand"
            ) =>
        {
            Some((&node.span.file, raw))
        }
        _ => None,
    }
}

pub(crate) struct Field<'a> {
    pub text: String,
    pub span: &'a Span,
}

pub(crate) struct Email<'a> {
    pub field: Field<'a>,
    pub prefix: Option<String>,
}

pub(crate) struct Entry<'a> {
    pub field: Field<'a>,
    pub emails: Vec<Email<'a>>,
    pub note_numbers: Vec<(usize, &'a Span)>,
}

impl<'a> Entry<'a> {
    fn new(field: Field<'a>) -> Self {
        Self {
            field,
            emails: Vec::new(),
            note_numbers: Vec::new(),
        }
    }
}

pub(crate) struct Author<'a> {
    pub name: Entry<'a>,
    pub affiliations: Vec<usize>,
}

#[derive(Default)]
pub(crate) struct RevtexTitle<'a> {
    pub title: Option<Entry<'a>>,
    pub authors: Vec<Author<'a>>,
    pub affiliations: Vec<Entry<'a>>,
    pub notes: Vec<Field<'a>>,
    pub date: Option<Field<'a>>,
}

// REVTeX attaches each \thanks or \email to the most recent title, author,
// or affiliation declaration.
#[derive(Clone, Copy)]
enum Owner {
    Title,
    Author,
    Affiliation,
}

impl<'a> RevtexTitle<'a> {
    fn owner_mut(&mut self, owner: Owner) -> Option<&mut Entry<'a>> {
        match owner {
            Owner::Title => self.title.as_mut(),
            Owner::Author => self.authors.last_mut().map(|author| &mut author.name),
            Owner::Affiliation => self.affiliations.last_mut(),
        }
    }

    fn note_number(&mut self, field: Field<'a>) -> usize {
        if let Some(index) = self
            .notes
            .iter()
            .position(|note| note.text.trim() == field.text.trim())
        {
            return index + 1;
        }
        self.notes.push(field);
        self.notes.len()
    }
}

pub(crate) fn title_details(declarations: &[Node]) -> RevtexTitle<'_> {
    let mut title = RevtexTitle::default();
    let mut group_start = 0;
    let mut group_has_affiliation = false;
    let mut owner = None;
    for node in declarations {
        let NodeKind::OpaqueCmd { name, .. } = &node.kind else {
            continue;
        };
        let Some((prefix, text)) = revtex_title_argument(node) else {
            continue;
        };
        let field = Field {
            text,
            span: &node.span,
        };
        match name.as_str() {
            "title" => {
                title.title = Some(Entry::new(field));
                owner = Some(Owner::Title);
            }
            "date" => title.date = Some(field),
            "author" => {
                if group_has_affiliation {
                    group_start = title.authors.len();
                    group_has_affiliation = false;
                }
                title.authors.push(Author {
                    name: Entry::new(field),
                    affiliations: Vec::new(),
                });
                owner = Some(Owner::Author);
            }
            // Consecutive authors share the following affiliation(s).
            "affiliation" => {
                for author in &mut title.authors[group_start..] {
                    author.affiliations.push(title.affiliations.len());
                }
                title.affiliations.push(Entry::new(field));
                group_has_affiliation = true;
                owner = Some(Owner::Affiliation);
            }
            "noaffiliation" => {
                group_has_affiliation = true;
                group_start = title.authors.len();
                owner = None;
            }
            "email" => {
                if let Some(entry) = owner.and_then(|owner| title.owner_mut(owner)) {
                    entry.emails.push(Email { field, prefix });
                }
            }
            "thanks" => {
                if let Some(owner) = owner {
                    let number = title.note_number(field);
                    if let Some(entry) = title.owner_mut(owner) {
                        if !entry
                            .note_numbers
                            .iter()
                            .any(|(existing, _)| *existing == number)
                        {
                            entry.note_numbers.push((number, &node.span));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn shared_affiliations_and_macro_override_precedence() {
        let source = r"\documentclass{revtex4-2}
\begin{document}
\title[Short]{Long title}
\newcommand{\credit}{manuscript credit}
\author{A}\author{B}
\affiliation{University}\affiliation{Laboratory}
\author{C}\affiliation{Institute}
\maketitle
\end{document}";
        let project =
            crate::project::load_project_from_source(Path::new("title.tex"), source.into())
                .unwrap();
        let overrides = [MacroOverride {
            label: "viewer-overrides.tex".into(),
            source: r"\renewcommand{\credit}{viewer credit}".into(),
        }];
        let mut preamble = macros::extract_preamble_with_overrides(&project, &overrides).unwrap();
        let nodes = parse_latex_body(
            &project,
            &TheoremRegistry::from_project(&project),
            &overrides,
            &mut preamble,
        )
        .unwrap();
        let title = nodes
            .iter()
            .find(|n| matches!(n.kind, NodeKind::MakeTitle))
            .unwrap();
        let front = title_details(&title.children);
        let affiliations: Vec<Vec<_>> = front
            .authors
            .iter()
            .map(|author| {
                author
                    .affiliations
                    .iter()
                    .map(|&index| front.affiliations[index].field.text.as_str())
                    .collect()
            })
            .collect();
        assert_eq!(
            affiliations,
            [
                vec!["University", "Laboratory"],
                vec!["University", "Laboratory"],
                vec!["Institute"]
            ]
        );
        assert_eq!(preamble.title_short.as_deref(), Some("Short"));
        let credits: Vec<_> = preamble
            .macros
            .iter()
            .filter(|m| m.name == "credit")
            .collect();
        assert_eq!(credits.len(), 1);
        assert_eq!(credits[0].body, "viewer credit");
    }
}
