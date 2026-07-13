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
  // Available width (CSS px) the math can use, for MathJax line-breaking.
  // tex2svg renders each equation standalone, so we must tell it the column
  // width or `displayOverflow: 'linebreak'` has nothing to break against.
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
  // The current page zoom (main#page's CSS `zoom`, mirrored in --page-scale).
  function pageZoom() {
    var v = parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue('--page-scale')
    );
    return v > 0 ? v : 1;
  }
  // Context font metrics (em = font size, ex = x-height), always in UNZOOMED
  // CSS px. tex2svg renders each equation standalone at MathJax's own default
  // em; a normal equation then rescales into the page through its ex-based
  // width/height, but a full-width display (multline, or one MathJax
  // line-breaks) uses width:100% with NO viewBox and skips that rescale — so it
  // would render at ~18px regardless of the document font. Passing the real
  // em/ex makes every equation, full-width included, match the surrounding
  // text.
  //
  // Measured with getBoundingClientRect (spec: reports the ZOOMED box on every
  // engine) divided by the page zoom, NOT offsetHeight/getComputedStyle: under
  // CSS `zoom`, WebKit inflates those while Blink doesn't, so only the
  // rect÷zoom path yields the same unzoomed px on both. A plain em/ex-sized
  // block scales like text (once), so this stays exact even while the page is
  // zoomed. Cached per computed font-size string (stable per font × zoom).
  var emExCache = {};
  function contextEmEx(el) {
    var key = getComputedStyle(el).fontSize;
    var hit = emExCache[key];
    if (hit) return hit;
    var z = pageZoom();
    var probe = document.createElement('div');
    probe.style.cssText =
      'display:inline-block;width:0;height:10em;visibility:hidden;padding:0;border:0;';
    el.appendChild(probe);
    var em = probe.getBoundingClientRect().height / z / 10;
    probe.style.height = '10ex';
    var ex = probe.getBoundingClientRect().height / z / 10;
    el.removeChild(probe);
    if (!(em > 0)) em = parseFloat(key) || 16;
    if (!(ex > 0)) ex = em * 0.43;
    var res = { em: em, ex: ex };
    emExCache[key] = res;
    return res;
  }
  // Pin the outer <svg> to px sized from the UNZOOMED ex, so CSS `zoom` scales
  // it as a pure geometric transform — exactly like text. MathJax sizes the
  // svg in `ex` units (width="50.242ex"); under the spec-compliant CSS `zoom`
  // in macOS WebKit, an SVG's intrinsic ex-based width resolves against the
  // zoom-inflated font AND the box is geometrically zoomed, so equations grew
  // by zoom² while text grew by zoom (Blink and WebKitGTK don't double-count,
  // so they were already fine — and px vs ex is identical there). exPx is the
  // context ex in unzoomed px; a full-width line-broken svg (width:100%, no
  // viewBox) keeps its fluid width and only pins height.
  function pinSvgPx(root, exPx) {
    if (!root || !(exPx > 0) || !root.querySelector) return;
    var tag = root.tagName && root.tagName.toLowerCase();
    var container = tag === 'mjx-container' ? root : root.querySelector('mjx-container');
    var svg = tag === 'svg' ? root : (container ? container.querySelector('svg') : root.querySelector('svg'));
    if (!svg) return;
    // Full-width line-broken displays (multline, wrapped equations) lay out at
    // width:100% with no viewBox (mathjax.css keys off the container's
    // width="full"); pinning their width would break the fluid layout, so pin
    // height only. Every other display keeps aspect ratio (both are ex).
    var full = container && container.getAttribute('width') === 'full';
    var hAttr = svg.getAttribute('height');
    var wAttr = svg.getAttribute('width');
    if (hAttr && /ex$/.test(hAttr)) svg.style.height = (parseFloat(hAttr) * exPx).toFixed(3) + 'px';
    if (!full && wAttr && /ex$/.test(wAttr)) svg.style.width = (parseFloat(wAttr) * exPx).toFixed(3) + 'px';
    // Inline math carries an ex-based `vertical-align` (baseline offset); pin
    // it too, or it would drift against the (px-scaling) text baseline under
    // the same WebKit zoom double-count.
    var va = svg.style.verticalAlign;
    if (va && /ex$/.test(va)) svg.style.verticalAlign = (parseFloat(va) * exPx).toFixed(3) + 'px';
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
      // Skip the width hint entirely when wrapping is off, so nothing changes
      // for the overflow/scroll path.
      var wrap = !(window.__mpConfig && window.__mpConfig.wrapEquations === false);
      return sources.reduce(function(chain, source) {
        return chain.then(function() {
        if (source.querySelector('mjx-container') && !sourceIsStale(source)) {
          return Promise.resolve();
        }
        var mm = contextEmEx(source);
        var opts = { display: sourceDisplay(source), em: mm.em, ex: mm.ex };
        if (wrap) {
          var cw = availWidth(source);
          if (cw) opts.containerWidth = cw;
        }
        return window.MathJax.tex2svgPromise(sourceTex(source), opts)
          .then(function(svg) {
            source.replaceChildren(svg);
            pinSvgPx(svg, mm.ex);
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
    return window.MathJax.typesetPromise(sources).then(function() {
      // Same zoom-invariant px pin as the tex2svg path (see pinSvgPx).
      sources.forEach(function(source) {
        pinSvgPx(source, contextEmEx(source).ex);
      });
    });
  }
  window.__mpEngine = {
    name: 'mathjax',
    ready: ready,
    isReady: isReady,
    typesetClear: typesetClear,
    typeset: typeset
  };
})();
