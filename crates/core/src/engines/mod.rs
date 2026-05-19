//! Rendering-engine abstraction.
//!
//! The renderer emits an engine-neutral wire format: each math expression is
//! a `<span class="math" data-tex="\(...\)" data-hash="...">` node. A
//! [`MathEngine`] picks up those nodes in the browser and produces visible
//! math. Today the only implementation is [`MathJaxEngine`], which loads
//! MathJax v4 SVG output. A future PDF.js or Texpresso path slots in as a new
//! impl without touching the AST → HTML walk.

mod mathjax;

pub use mathjax::MathJaxEngine;

use crate::macros::ExtractedPreamble;

/// What the renderer needs from a math engine to assemble the shell page and
/// drive client-side typesetting after WebSocket patches.
///
/// All three artifacts (`head_html`, `client_adapter_js`, `extra_css`) are
/// concatenated into the static HTML page. The renderer never calls into the
/// engine at AST-walk time; the engine is purely a frontend bundle.
pub trait MathEngine: std::fmt::Debug {
    /// Short identifier, used for logging and protocol routing.
    fn name(&self) -> &'static str;

    /// HTML fragment injected into `<head>` after the page CSS. MathJax: the
    /// inline `window.MathJax = {...}` config plus the `<script src=…>` tag.
    /// Future PDF.js / Texpresso engines emit their own loader scripts here.
    fn head_html(&self, preamble: &ExtractedPreamble) -> String;

    /// JS appended after the shared `CLIENT_JS` bundle. Must define
    /// `window.__mpEngine` with the shape:
    ///
    /// ```js
    /// window.__mpEngine = {
    ///   name: "...",                 // engine identifier
    ///   ready(cb)        -> boolean, // register cb to fire after initial
    ///                                // typeset completes; return true if
    ///                                // the engine is already loaded and cb
    ///                                // was registered (caller can stop
    ///                                // polling), false if not yet loaded.
    ///   isReady()        -> boolean, // can typeset() be called right now?
    ///   typesetClear(nodes),         // drop engine state attached to these
    ///                                // DOM nodes before they are removed.
    ///   typeset(nodes)   -> Promise, // typeset the given math nodes;
    ///                                // resolved when all are visible.
    /// };
    /// ```
    fn client_adapter_js(&self) -> String;

    /// CSS rules specific to this engine's output, appended after the default
    /// stylesheet (MathJax: rules targeting `mjx-container`).
    fn extra_css(&self) -> &'static str;
}

/// Concrete dispatch enum so [`crate::renderer::HtmlOptions`] stays `Clone`
/// without dragging in `dyn-clone`. Add a new variant per engine impl.
#[derive(Debug, Clone)]
pub enum Engine {
    MathJax(MathJaxEngine),
}

impl Engine {
    pub fn as_dyn(&self) -> &dyn MathEngine {
        match self {
            Engine::MathJax(e) => e,
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Engine::MathJax(MathJaxEngine::default())
    }
}
