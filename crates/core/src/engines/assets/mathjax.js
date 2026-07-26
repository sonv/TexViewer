(function() {
  function ready(cb) {
    if (isReady()) {
      setTimeout(cb, 0);
      return true;
    }
    if (window.MathJax && window.MathJax.startup && window.MathJax.startup.promise) {
      window.MathJax.startup.promise
        .then(cb)
        .catch(function(e) {
          console.warn('mathpreview engine startup:', e);
          if (isReady()) cb();
        });
      return true;
    }
    return false;
  }
  function isReady() {
    return !!(window.MathJax && (window.MathJax.tex2svgPromise || window.MathJax.typesetPromise));
  }
  function mathSourceNodes(nodes) {
    var out = [];
    var seen = new Set();
    nodes.forEach(function(node) {
      var source = node && node.matches && node.matches('.math[data-hash]') ?
        node.querySelector('.math-source') : node;
      if (source && !seen.has(source)) {
        seen.add(source);
        out.push(source);
      }
    });
    return out;
  }
  function stripOuterDelimiters(tex) {
    tex = tex || '';
    var trimmed = tex.trim();
    if (trimmed.slice(0, 2) === '\\(' && trimmed.slice(-2) === '\\)') {
      return trimmed.slice(2, -2);
    }
    if (trimmed.slice(0, 2) === '\\[' && trimmed.slice(-2) === '\\]') {
      return trimmed.slice(2, -2);
    }
    return trimmed;
  }
  function sourceWrapper(source) {
    return source && source.closest ? source.closest('.math[data-hash]') : null;
  }
  function sourceTex(source) {
    var wrapper = sourceWrapper(source);
    return stripOuterDelimiters(
      (wrapper && wrapper.getAttribute('data-mathjax-tex')) || source.textContent || ''
    );
  }
  function sourceDisplay(source) {
    var wrapper = sourceWrapper(source);
    return !!(wrapper && wrapper.classList.contains('display'));
  }
  function typesetClear(nodes) {
    if (!nodes.length || !window.MathJax || !window.MathJax.typesetClear) return;
    try { window.MathJax.typesetClear(mathSourceNodes(nodes)); }
    catch (e) { console.warn('mathpreview engine clear:', e); }
  }
  // Available width (CSS px) a display can use for MathJax line-breaking.
  // tex2svg renders each equation standalone, so we must tell displays the
  // column width or `displayOverflow: 'linebreak'` has nothing to break
  // against. Inline math deliberately receives no width hint: TeX treats it
  // as one unbreakable atom while the surrounding prose wraps around it.
  // clientWidth is the padding-box width (excludes horizontal overflow), so
  // it reports the column even when the old (unwrapped) SVG overflows. Walk a
  // few ancestors in case the source span itself is inline (width 0).
  function availWidth(el) {
    var node = el;
    for (var i = 0; node && i < 5; i++) {
      var w = node.clientWidth;
      if (w && w > 1) return w;
      node = node.parentNode;
    }
    return 0;
  }
  // Context font metrics (em = font size, ex = x-height, in unzoomed px).
  // tex2svg renders each equation standalone at MathJax's own default em; a
  // normal equation then rescales into the page through its ex-based
  // width/height, but a full-width display (multline, or one MathJax
  // line-breaks) uses width:100% with NO viewBox and skips that rescale — so it
  // would render at ~18px regardless of the document font. Passing the real
  // em/ex makes every equation, full-width included, match the surrounding
  // text. `offsetHeight` is unzoomed layout px (matches getComputedStyle),
  // measured over 10ex for sub-pixel accuracy; cached per distinct font size.
  var exByEm = {};
  function contextEmEx(el) {
    var em = parseFloat(getComputedStyle(el).fontSize);
    if (!(em > 0)) em = 16;
    var ex = exByEm[em];
    if (ex == null) {
      var probe = document.createElement('div');
      probe.style.cssText =
        'display:inline-block;width:0;height:10ex;visibility:hidden;padding:0;border:0;';
      el.appendChild(probe);
      ex = (probe.offsetHeight || em * 4.3) / 10;
      el.removeChild(probe);
      exByEm[em] = ex;
    }
    return { em: em, ex: ex };
  }
  // A STALE source (wrapper carries data-mp-stale) holds the PREVIOUS
  // render's <mjx-container> as an anti-flash placeholder while its new TeX
  // waits to typeset (see patch.js seedStaleMath) — so "already has a
  // container" must not skip it, and a successful typeset clears the marker.
  function sourceIsStale(source) {
    var wrapper = sourceWrapper(source);
    return !!(wrapper && wrapper.hasAttribute('data-mp-stale'));
  }
  function clearStale(source) {
    var wrapper = sourceWrapper(source);
    if (wrapper) wrapper.removeAttribute('data-mp-stale');
  }
  function typeset(nodes) {
    var sources = mathSourceNodes(nodes);
    if (window.MathJax.tex2svgPromise) {
      // Width hints belong only to wrappable displays. Inline math remains a
      // single TeX atom, and wrapping-off keeps the display overflow/scroll
      // path unchanged.
      var wrap = !(window.__mpConfig && window.__mpConfig.wrapEquations === false);
      return sources.reduce(function(chain, source) {
        return chain.then(function() {
        if (source.querySelector('mjx-container') && !sourceIsStale(source)) {
          return Promise.resolve();
        }
        var mm = contextEmEx(source);
        var display = sourceDisplay(source);
        var opts = { display: display, em: mm.em, ex: mm.ex };
        if (wrap && display) {
          var cw = availWidth(source);
          if (cw) opts.containerWidth = cw;
        }
        return window.MathJax.tex2svgPromise(sourceTex(source), opts)
          .then(function(svg) {
            source.replaceChildren(svg);
            clearStale(source);
          })
          .catch(function(e) {
            // Keep the stale marker on failure: the node still counts as raw
            // (isRawMathNode), so a later queue pass retries it — the same
            // retry semantics an untypeset raw node gets.
            console.warn('mathpreview engine item:', e);
          });
        });
      }, Promise.resolve());
    }
    // typesetPromise reads the TeX from the element's content — restore the
    // raw source over any stale placeholder before handing the nodes over.
    sources.forEach(function(source) {
      if (!sourceIsStale(source)) return;
      var wrapper = sourceWrapper(source);
      source.textContent = (wrapper && wrapper.getAttribute('data-mathjax-tex')) || '';
      clearStale(source);
    });
    return window.MathJax.typesetPromise(sources);
  }
  window.__mpEngine = {
    name: 'mathjax',
    ready: ready,
    isReady: isReady,
    typesetClear: typesetClear,
    typeset: typeset
  };
})();
