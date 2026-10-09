use std::path::Path;

use crate::ast::Pos;
use crate::renderer::{HtmlOptions, RenderOutput, RenderedBlock};
use crate::sync::{SyncEntry, SyncKind};

const FILE: &str = "figure-sync.tex";

#[test]
fn opaque_wrappers_keep_the_first_content_without_matching_later_duplicate_sources() {
    for later in ["first", "second"] {
        let out = render(&format!(
            "\\begin{{figure}}\n\\fbox{{\\includegraphics{{first.svg}}}}\n\\includegraphics{{{later}.svg}}\n\\parbox{{5cm}}{{\\caption{{first}}}}\n\\caption{{{later}}}\n\\end{{figure}}\n"
        ));
        assert!(out.body_html.contains(r#"src="/assets/first.svg""#));
        assert!(!out.body_html.contains(r#"src="/assets/second.svg""#));
        assert!(out.body_html.contains("</span> first</figcaption>"));
        assert!(!out
            .sync
            .entries
            .iter()
            .any(|entry| entry.element_id.starts_with("float-asset-")
                || entry.element_id.starts_with("caption-")));
    }
}

#[test]
fn inline_math_caption_labels_belong_only_to_the_float() {
    let out = render("\\begin{figure}\\caption{Curve $x\\label{fig:math}$.}\\end{figure}");
    assert_eq!(out.body_html.matches(r#"id="fig-math""#).count(), 1);
    assert!(out.body_html.contains(r#"data-tex="\(x\)""#));
    assert!(!out.body_html.contains(r"\label"));
}

fn render(source: &str) -> RenderOutput {
    crate::render_project_from_source(Path::new(FILE), source.to_string(), &HtmlOptions::default())
        .unwrap()
}

fn position(source: &str, byte: usize) -> Pos {
    let before = &source[..byte];
    Pos {
        line: before.bytes().filter(|byte| *byte == b'\n').count() as u32 + 1,
        col: before.rsplit('\n').next().unwrap().len() as u32 + 1,
        byte: byte as u32,
    }
}

fn leaf_at<'a>(out: &'a RenderOutput, source: &str, needle: &str) -> &'a SyncEntry {
    // Probe inside the visible token rather than the zero-width source-space
    // anchor that can share its first column after an authored line break.
    let offset = needle.chars().next().unwrap().len_utf8();
    let pos = position(source, source.find(needle).unwrap() + offset);
    out.sync
        .lookup_leaf_by_source_position(Path::new(FILE), pos.line, pos.col)
        .unwrap_or_else(|| panic!("missing leaf for {needle:?}: {:?}", out.sync.entries))
}

fn assert_span(entry: &SyncEntry, source: &str, raw: &str) {
    let start = source.find(raw).unwrap();
    assert_eq!(entry.file, Path::new(FILE));
    assert_eq!(entry.start, position(source, start));
    assert_eq!(entry.end, position(source, start + raw.len()));
}

fn assert_anchor(out: &RenderOutput, entry: &SyncEntry) {
    let src = format!("{}:{}:{}", FILE, entry.start.line, entry.start.col);
    let id = format!(r#"id="{}""#, entry.element_id);
    let attributes = out
        .body_html
        .split_once(&id)
        .unwrap()
        .1
        .split('>')
        .next()
        .unwrap();
    assert!(
        attributes.contains(&format!(r#"data-src="{src}""#)),
        "{attributes}"
    );
    assert_eq!(
        out.body_html
            .matches(&format!(r#"id="{}""#, entry.element_id))
            .count(),
        1
    );
    assert!(out
        .blocks
        .iter()
        .flat_map(|block| &block.source_anchors)
        .any(|anchor| { anchor.id == entry.element_id && anchor.src == src }));
}

fn float_block(out: &RenderOutput) -> &RenderedBlock {
    out.blocks
        .iter()
        .find(|block| block.html.contains("<figure "))
        .unwrap()
}

#[test]
fn original_positions_survive_unicode_comments_and_optional_titles() {
    let source = concat!(
        "\\begin{document}\nα \\begin% opening\n{figure*}[ht]\n",
        "\\includegraphics% asset\n[width=.5\\textwidth]% option\n{plot.png}\n",
        "\\caption% caption\n[Short $s$]% title\n{Résumé $x+1$ and words.}\\label{fig:plot}\n",
        "\\end{figure*}\n\\end{document}\n",
    );
    let out = render(source);
    let figure = out.sync.lookup_by_label("fig:plot").unwrap();
    assert_eq!(figure.kind, SyncKind::Container);
    assert_span(
        figure,
        source,
        &source[source.find("\\begin% opening").unwrap()
            ..source.find("\\end{figure*}").unwrap() + "\\end{figure*}".len()],
    );
    let image = leaf_at(&out, source, "\\includegraphics");
    assert!(image.element_id.starts_with("float-asset-g"));
    assert_span(
        image,
        source,
        "\\includegraphics% asset\n[width=.5\\textwidth]% option\n{plot.png}",
    );
    let word = leaf_at(&out, source, "Résumé");
    assert_span(word, source, "Résumé");
    let math = leaf_at(&out, source, "$x+1$");
    assert_span(math, source, "$x+1$");
    for entry in [figure, image, word, math] {
        assert_anchor(&out, entry);
    }
    assert!(!out.body_html.contains("Short"));
    assert!(out.body_html.contains(r#"src="/assets/plot.png""#));
}

#[test]
fn caption_selection_targets_content_without_selecting_the_whole_caption() {
    let source = concat!(
        "\\begin{figure}\n\\includegraphics{plot.png}\n",
        "\\caption[Short]{\nFirst words.\nLater $x$ ends.}\n\\end{figure}\n",
    );
    let out = render(source);
    let caption = out
        .sync
        .entries
        .iter()
        .find(|entry| entry.element_id.starts_with("caption-") && entry.kind == SyncKind::Container)
        .unwrap();
    let later = leaf_at(&out, source, "Later");
    let selected = out.sync.leaves_in_range(
        Path::new(FILE),
        later.start.line,
        later.start.col + 1,
        later.end.line,
        later.end.col - 1,
    );
    assert_eq!(selected, [later.element_id.clone()]);
    let first = position(source, source.find("First").unwrap());
    let last = position(source, source.find("ends.").unwrap() + "ends.".len());
    let content =
        out.sync
            .leaves_in_range(Path::new(FILE), first.line, first.col, last.line, last.col);
    assert!(!content.contains(&caption.element_id), "{content:?}");
    assert!(
        !content.iter().any(|id| id.starts_with("float")),
        "{content:?}"
    );
    let header = position(source, source.find("\\caption").unwrap());
    assert!(out
        .sync
        .lookup_leaf_by_source_position(Path::new(FILE), header.line, header.col)
        .is_none());
    assert_eq!(
        out.sync.leaves_in_range(
            Path::new(FILE),
            header.line,
            header.col,
            header.line,
            u32::MAX
        ),
        [caption.element_id.clone()]
    );
}

#[test]
fn nested_tikz_owns_only_its_diagram_source() {
    let source = concat!(
        "\\begin{figure}\n\\centering\n",
        "\\begin{tikzpicture}\n\\draw (0,0)--(1,1);\n\\end{tikzpicture}\n",
        "\\caption{Distinct caption.}\\label{fig:diagram}\n\\end{figure}\n",
    );
    let out = render(source);
    let diagram = leaf_at(&out, source, "\\draw");
    assert!(diagram.element_id.starts_with("tikz-g"));
    assert_span(
        diagram,
        source,
        "\\begin{tikzpicture}\n\\draw (0,0)--(1,1);\n\\end{tikzpicture}",
    );
    let caption = leaf_at(&out, source, "Distinct");
    assert_span(caption, source, "Distinct");
    assert_ne!(diagram.element_id, caption.element_id);
    let selected = out.sync.leaves_in_range(
        Path::new(FILE),
        caption.start.line,
        1,
        caption.start.line,
        u32::MAX,
    );
    assert!(!selected.contains(&diagram.element_id));
    assert_anchor(&out, diagram);
    assert_anchor(&out, caption);
}

#[test]
fn table_asset_and_caption_have_separate_anchors_and_unique_nested_labels() {
    let source = concat!(
        "\\begin{table}\n\\begin{tabular}{ll}\nA&B\\\\\n\\end{tabular}\n",
        "\\caption{A \\emph{nested \\label{tab:nested}} caption.\\label% label comment\n{tab:alias}}\n",
        "\\end{table}\n",
    );
    let out = render(source);
    let table = leaf_at(&out, source, "A&B");
    assert!(table.element_id.starts_with("float-asset-g"));
    assert_span(
        table,
        source,
        "\\begin{tabular}{ll}\nA&B\\\\\n\\end{tabular}",
    );
    let caption = leaf_at(&out, source, "caption.");
    assert_ne!(caption.element_id, table.element_id);
    assert!(out.body_html.contains("<em>nested "));
    for label in ["tab-nested", "tab-alias"] {
        assert_eq!(
            out.body_html.matches(&format!(r#"id="{label}""#)).count(),
            1,
            "{}",
            out.body_html
        );
    }
    let figcaption = out.body_html.split("<figcaption").nth(1).unwrap();
    assert!(!figcaption.contains("tab:alias"), "{figcaption}");
    assert!(!figcaption.contains("tab:nested"), "{figcaption}");
    assert!(!figcaption.contains(r"\label"), "{figcaption}");
    assert_anchor(&out, table);
    assert_anchor(&out, caption);
}

#[test]
fn earlier_block_insertions_only_shift_float_source_metadata() {
    for asset in [
        "\\includegraphics{plot.png}",
        "\\begin{tikzpicture}\\draw (0,0)--(1,1);\\end{tikzpicture}",
    ] {
        for label in ["", "\\label{fig:stable}"] {
            let float = format!("\\begin{{figure}}\n{asset}\n\\caption{{Stable $x$ caption.}}{label}\n\\end{{figure}}\n");
            let before = render(&format!("Before.\n\n{float}"));
            let after = render(&format!("Earlier.\n\nBefore.\n\n{float}"));
            let old = float_block(&before);
            let new = float_block(&after);
            assert_eq!(old.diff_hash, new.diff_hash, "{asset} {label}");
            assert_ne!(old.hash, new.hash);
            assert_ne!(old.id, new.id);
            assert_eq!(old.source_anchors.len(), new.source_anchors.len());
            for (old, new) in old.source_anchors.iter().zip(&new.source_anchors) {
                let (old_location, old_col) = old.src.rsplit_once(':').unwrap();
                let (new_location, new_col) = new.src.rsplit_once(':').unwrap();
                let (old_file, old_line) = old_location.rsplit_once(':').unwrap();
                let (new_file, new_line) = new_location.rsplit_once(':').unwrap();
                assert_eq!(old_file, new_file);
                assert_eq!(old_col, new_col);
                assert_eq!(
                    new_line.parse::<u32>().unwrap(),
                    old_line.parse::<u32>().unwrap() + 2
                );
                if old.id == "fig-stable" {
                    assert_eq!(old.id, new.id);
                } else {
                    assert_ne!(old.id, new.id);
                    assert!(new.id.contains("-g"), "{}", new.id);
                }
                assert_eq!(
                    after
                        .body_html
                        .matches(&format!(r#"id="{}""#, new.id))
                        .count(),
                    1
                );
            }
        }
    }
}

#[test]
fn caption_literals_and_comment_continued_commands_preserve_visible_content() {
    let source = concat!(
        "\\begin{equation}x=1\\label{eq:x}\\end{equation}\n",
        "\\begin{figure}\\caption{",
        "\\string\\ref \\detokenize% continued\n{\\caption{Literal}} ",
        "\\unexpanded{\\includegraphics{literal.png}} ",
        "\\ref% reference\n{eq:% inside key\nx} \\cite% citation\n[see]% note\n{sou% inside key\nrce} ",
        "\\textbf% formatting\n{Bold} ",
        "A\\label% label\n{fig:inside} caption.",
        "}\\end{figure}\n",
    );
    let out = render(source);
    let caption = out.body_html.split("<figcaption").nth(1).unwrap();
    assert!(caption.contains(r"\ref"), "{caption}");
    assert!(caption.contains(r"\caption{Literal}"), "{caption}");
    assert!(
        caption.contains(r"\includegraphics{literal.png}"),
        "{caption}"
    );
    assert!(
        caption.contains(r#"data-target="eq:x" data-kind="ref">1</a>"#),
        "{caption}"
    );
    assert!(caption.contains(r#"data-key="source">1</a>"#), "{caption}");
    assert!(caption.contains("<strong>Bold</strong>"), "{caption}");
    assert!(!caption.contains("fig:inside"), "{caption}");
    assert_eq!(out.body_html.matches(r#"id="fig-inside""#).count(), 1);
    assert!(!out.body_html.contains("/assets/literal.png"));
}

#[test]
fn included_float_anchors_use_the_child_files_own_coordinates() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("mathpreview-float-sync-{nonce}"));
    std::fs::create_dir(&directory).unwrap();
    let child = directory.join("figure.tex");
    std::fs::write(
        &child,
        "% child header\n\\begin{figure}\n\\includegraphics{plot.png}\n\\caption{Child words.}\\label{fig:child}\n\\end{figure}\n",
    )
    .unwrap();
    let child = child.canonicalize().unwrap();
    let result = crate::render_project_from_source(
        &directory.join("main.tex"),
        "\\begin{document}\nRoot text.\n\n\\input{figure}\n\\end{document}".into(),
        &HtmlOptions::default(),
    );
    std::fs::remove_file(&child).unwrap();
    std::fs::remove_dir(&directory).unwrap();
    let out = result.unwrap();
    let figure = out.sync.lookup_by_label("fig:child").unwrap();
    assert_eq!(figure.file, child);
    assert_eq!((figure.start.line, figure.start.col), (2, 1));
    let image = out
        .sync
        .lookup_leaf_by_source_position(&child, 3, 2)
        .unwrap();
    let caption = out
        .sync
        .lookup_leaf_by_source_position(&child, 4, 11)
        .unwrap();
    assert!(image.element_id.starts_with("float-asset-g"));
    assert_eq!((image.start.line, image.start.col), (3, 1));
    assert_eq!((caption.start.line, caption.start.col), (4, 10));
    assert_ne!(image.element_id, caption.element_id);
}

#[test]
fn literal_percent_labels_and_urls_are_not_rewritten_in_captions() {
    let out = render(concat!(
        "\\begin{figure}\\caption{",
        "\\verb|100% \\label{shown}| and ",
        "\\url{https://example.test/a%20b?x=1} ",
        "\\detokenize{\\label{also-shown}}",
        "}\\label{fig:literal}\\end{figure}",
    ));
    let caption = out.body_html.split("<figcaption").nth(1).unwrap();
    assert!(caption.contains(r"100% \label{shown}</code>"), "{caption}");
    assert!(
        caption.contains(r#"href="https://example.test/a%20b?x=1""#),
        "{caption}"
    );
    assert!(caption.contains(r"\label{also-shown}"), "{caption}");
    assert!(!out.body_html.contains(r#"id="shown""#));
    assert!(!out.body_html.contains(r#"id="also-shown""#));
    assert_eq!(out.body_html.matches(r#"id="fig-literal""#).count(), 1);
}

#[test]
fn scoped_caption_font_and_color_keep_their_scope_across_math() {
    for (declaration, opening, closing) in [
        (r"\bfseries", "<strong>", "</strong>"),
        (
            r"\color{ForestGreen}",
            r#"style="color:#009B55""#,
            "</span>",
        ),
    ] {
        let command = format!(r"\caption{{Before {{{declaration} styled $x$ end}} tail.}}");
        let source = format!("\\begin{{figure}}\n{command}\n\\end{{figure}}");
        let out = render(&source);
        let caption = out.body_html.split("<figcaption").nth(1).unwrap();
        let start = caption.find(opening).unwrap_or_else(|| panic!("{caption}"));
        let math = caption.find(r#"data-tex="\(x\)""#).unwrap();
        let end = caption
            .find(&format!("end{closing} tail."))
            .unwrap_or_else(|| panic!("{caption}"));
        assert!(start < math && math < end, "{caption}");
        assert!(!caption.contains(declaration), "{caption}");
        // Declarations need the legacy whole-caption inline pass to retain
        // style state; the source fallback is deliberately atomic here.
        let anchor = leaf_at(&out, &source, "styled");
        assert_span(anchor, &source, &command);
        assert_eq!(anchor.element_id, leaf_at(&out, &source, "$x$").element_id);
        assert_anchor(&out, anchor);
    }
}
