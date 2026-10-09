//! Float presentation stays driven by its raw LaTeX body. Semantic children
//! supply original-file positions for the parts actually shown in the preview.

use super::math::{
    label_alias_anchors, render_float_asset, render_latex_text_with_math, strip_labels,
};
use super::table::{first_nested_tabular, render_tabular};
use super::util::{data_src, escape_attr, escape_html, refkey_attr, sanitize_id};
use super::{
    first_nested_tikz, record, record_container, record_sync, tikz_html, write_children, Node,
    NodeKind, RenderCtx, Span, SyncKind,
};
use std::fmt::Write;

#[cfg(test)]
mod tests;

/// Do not search the arguments of opaque commands or other opaque environments:
/// they may contain examples, stored TeX, or a different float's contents.
fn find_source_node<'a>(nodes: &'a [Node], predicate: &impl Fn(&Node) -> bool) -> Option<&'a Node> {
    for node in nodes {
        if predicate(node) {
            return Some(node);
        }
        if !matches!(
            node.kind,
            NodeKind::OpaqueCmd { .. } | NodeKind::OpaqueEnv { .. }
        ) {
            if let Some(found) = find_source_node(&node.children, predicate) {
                return Some(found);
            }
        }
    }
    None
}

fn environment_span<'a>(nodes: &'a [Node], env: &str, body: &str) -> Option<&'a Span> {
    find_source_node(nodes, &|node| match &node.kind {
        NodeKind::OpaqueEnv {
            env: name,
            body: source,
        } if name == env => source == body,
        NodeKind::OpaqueCmd { name, .. } if name == "inline-literal" => false,
        NodeKind::OpaqueCmd { raw, .. } | NodeKind::OpaqueEnv { body: raw, .. } => {
            crate::parser::first_supported_environment(raw, &[env])
                .is_some_and(|(_, source)| source == body)
        }
        _ => false,
    })
    // An earlier unsupported wrapper can contain the selected asset. Stop
    // there instead of attaching it to a later identical diagram/table.
    .filter(|node| matches!(&node.kind, NodeKind::OpaqueEnv { env: name, .. } if name == env))
    .map(|node| &node.span)
}

fn command_source_node<'a>(
    nodes: &'a [Node],
    command: &str,
    selected: &crate::parser::LiveBracedCommand,
) -> Option<&'a Node> {
    find_source_node(nodes, &|node| {
        let raw = match &node.kind {
            NodeKind::OpaqueCmd { name, raw } if name != "inline-literal" => raw,
            NodeKind::OpaqueEnv { body, .. } => body,
            _ => return false,
        };
        crate::parser::live_braced_command_calls(raw, &[command], 1)
            .first()
            .is_some_and(|call| call == selected)
    })
    .filter(|node| {
        matches!(&node.kind, NodeKind::OpaqueCmd { name, .. }
        if name.trim_end_matches('*') == command)
    })
}

// The inline renderer understands declarations whose scope crosses math.
// Keep those rare captions atomic rather than losing font/color state while
// splitting the caption into independently rendered source leaves.
fn caption_needs_atomic_render(source: &str) -> bool {
    [
        "bf",
        "bfseries",
        "em",
        "it",
        "itshape",
        "emshape",
        "tt",
        "ttfamily",
        "sc",
        "scshape",
        "rm",
        "rmfamily",
        "sf",
        "sffamily",
        "color",
        "normalcolor",
    ]
    .iter()
    .any(|command| {
        let needle = format!("\\{command}");
        source.match_indices(&needle).any(|(start, _)| {
            source
                .as_bytes()
                .get(start + needle.len())
                .is_none_or(|byte| !byte.is_ascii_alphabetic() && *byte != b'@')
        })
    })
}

fn mapped_asset(html: &str, span: Option<&Span>, ctx: &mut RenderCtx<'_>) -> String {
    let Some(span) = span else {
        return html.to_string();
    };
    let id = ctx.idgen.next("float-asset");
    record(ctx, &id, span, None);
    let opening = format!(
        r#"<div class="float-asset" id="{}" data-src="{}""#,
        escape_attr(&id),
        escape_attr(&data_src(span)),
    );
    if html.starts_with(r#"<div class="float-asset""#) {
        html.replacen(r#"<div class="float-asset""#, &opening, 1)
    } else {
        format!("{opening}>{html}</div>")
    }
}

// Float labels already belong to the figure wrapper / alias anchors. Rendering
// them again inside the caption would create duplicate DOM ids. Opaque inline
// formatting commands also need their nested labels removed from visible text.
fn caption_children(nodes: &[Node]) -> Vec<Node> {
    nodes
        .iter()
        .filter(|node| !matches!(&node.kind, NodeKind::OpaqueCmd { name, .. } if name == "label"))
        .map(|node| {
            let mut node = node.clone();
            if let NodeKind::OpaqueCmd { name, raw } = &mut node.kind {
                if name != "inline-literal" {
                    *raw = strip_labels(&crate::parser::executable_latex_source(raw));
                }
            } else if let NodeKind::InlineMath(body) = &mut node.kind {
                *body = strip_labels(&crate::parser::executable_latex_source(body));
            }
            node.children = caption_children(&node.children);
            node
        })
        .collect()
}

pub(super) fn write_float_placeholder(
    out: &mut String,
    node: &Node,
    env: &str,
    body: &str,
    ctx: &mut RenderCtx<'_>,
) {
    let live_body = crate::parser::executable_latex_source(body);
    let float_labels = crate::parser::live_braced_command_calls(&live_body, &["label"], 0);
    let primary_label = float_labels.first().map(|call| call.value.as_str());
    let id = primary_label
        .map(sanitize_id)
        .unwrap_or_else(|| ctx.idgen.next("float"));
    record_container(ctx, &id, &node.span, primary_label);
    write!(
        out,
        r#"<figure class="float-placeholder float-{env}" id="{id}"{refkey} data-env="{env}" data-src="{src}">{aliases}"#,
        env = escape_attr(env),
        id = escape_attr(&id),
        refkey = refkey_attr(primary_label),
        src = escape_attr(&data_src(&node.span)),
        aliases = label_alias_anchors(&live_body, primary_label),
    )
    .unwrap();

    // Preserve the existing table > diagram > image priority. A tabular may
    // itself contain TikZ; that diagram must not also appear outside the table.
    let tabular_asset = first_nested_tabular(body).and_then(|(env, body)| {
        render_tabular(&env, &body, ctx.labels)
            .map(|html| mapped_asset(&html, environment_span(&node.children, &env, &body), ctx))
    });
    let asset_html = tabular_asset.or_else(|| {
        first_nested_tikz(body).map(|(env, body)| {
            // Legacy ASTs and unsupported wrappers need the broad fallback.
            // When the diagram has source children, use its own source span.
            let span = environment_span(&node.children, &env, &body).unwrap_or(&node.span);
            tikz_html(&env, &body, span, ctx)
        })
    });
    if let Some(html) = asset_html {
        out.push_str(&html);
    } else {
        // Raw discovery remains authoritative even when the first image is
        // inside a wrapper the source parser does not understand. Do not map
        // that image to a later, unrelated includegraphics command.
        let asset = crate::parser::live_braced_command_calls(&live_body, &["includegraphics"], 1)
            .into_iter()
            .next();
        if let Some(asset) = asset {
            let asset_node = command_source_node(&node.children, "includegraphics", &asset);
            let html = render_float_asset(&asset.value, asset.optional.as_deref());
            if !html.is_empty() {
                out.push_str(&mapped_asset(&html, asset_node.map(|node| &node.span), ctx));
            }
        }
    }

    let caption = crate::parser::live_braced_command_calls(&live_body, &["caption"], 1)
        .into_iter()
        .next();
    let caption_node = caption
        .as_ref()
        .and_then(|caption| command_source_node(&node.children, "caption", caption));
    if let Some(caption_node) = caption_node {
        let id = ctx.idgen.next("caption");
        record_container(ctx, &id, &caption_node.span, None);
        // Follow a standalone \caption[short]{ header without making the
        // entire multiline caption a point/range highlight target.
        let mut header = caption_node.span.clone();
        if let Some(first) = caption_node.children.first() {
            header.end = first.span.start;
        }
        record_sync(ctx, &id, &header, None, SyncKind::Block);
        write!(
            out,
            r#"<figcaption id="{}" data-src="{}">"#,
            escape_attr(&id),
            escape_attr(&data_src(&caption_node.span))
        )
        .unwrap();
    } else {
        out.push_str("<figcaption>");
    }
    let starred = caption_node.map_or_else(
        || caption.as_ref().is_some_and(|call| call.starred),
        |node| matches!(&node.kind, NodeKind::OpaqueCmd { name, .. } if name.ends_with('*')),
    );
    if !starred {
        let kind = if env.trim_end_matches('*') == "table" {
            "Table"
        } else {
            "Figure"
        };
        let number = ctx.labels.float_number_for_span(&node.span).or_else(|| {
            primary_label.and_then(|label| ctx.labels.number.get(label).map(String::as_str))
        });
        let kind_label = number
            .map(|number| format!("{kind} {}.", escape_html(number)))
            .unwrap_or_else(|| format!("{kind}."));
        write!(out, r#"<span class="float-kind">{kind_label}</span> "#).unwrap();
    }
    let atomic_caption = caption
        .as_ref()
        .is_some_and(|call| caption_needs_atomic_render(&call.value));
    if let Some(caption_node) =
        caption_node.filter(|node| !atomic_caption && !node.children.is_empty())
    {
        write_children(out, &caption_children(&caption_node.children), ctx);
    } else if let Some(caption) = caption {
        let html = render_latex_text_with_math(strip_labels(&caption.value).trim(), ctx.labels);
        if let Some(caption_node) = caption_node {
            let id = ctx.idgen.next("srcw");
            record(ctx, &id, &caption_node.span, None);
            write!(
                out,
                r#"<span class="src-word" id="{}" data-src="{}">{html}</span>"#,
                escape_attr(&id),
                escape_attr(&data_src(&caption_node.span))
            )
            .unwrap();
        } else {
            out.push_str(&html);
        }
    } else {
        out.push_str("content omitted from preview");
    }
    out.push_str("</figcaption></figure>\n");
}
