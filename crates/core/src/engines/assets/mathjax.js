(function() {
  function ready(cb) {
    if (window.MathJax && window.MathJax.startup && window.MathJax.startup.promise) {
      window.MathJax.startup.promise.then(cb);
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
  function typeset(nodes) {
    var sources = mathSourceNodes(nodes);
    if (window.MathJax.tex2svgPromise) {
      return Promise.all(sources.map(function(source) {
        if (source.querySelector('mjx-container')) return Promise.resolve();
        return window.MathJax.tex2svgPromise(sourceTex(source), { display: sourceDisplay(source) })
          .then(function(svg) { source.replaceChildren(svg); });
      }));
    }
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
