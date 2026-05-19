(function() {
  function ready(cb) {
    if (window.MathJax && window.MathJax.startup && window.MathJax.startup.promise) {
      window.MathJax.startup.promise.then(cb);
      return true;
    }
    return false;
  }
  function isReady() {
    return !!(window.MathJax && window.MathJax.typesetPromise);
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
  function typesetClear(nodes) {
    if (!nodes.length || !window.MathJax || !window.MathJax.typesetClear) return;
    try { window.MathJax.typesetClear(mathSourceNodes(nodes)); }
    catch (e) { console.warn('mathpreview engine clear:', e); }
  }
  function typeset(nodes) {
    return window.MathJax.typesetPromise(mathSourceNodes(nodes));
  }
  window.__mpEngine = {
    name: 'mathjax',
    ready: ready,
    isReady: isReady,
    typesetClear: typesetClear,
    typeset: typeset
  };
})();
