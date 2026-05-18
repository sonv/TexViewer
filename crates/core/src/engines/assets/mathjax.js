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
  function typesetClear(nodes) {
    if (!nodes.length || !window.MathJax || !window.MathJax.typesetClear) return;
    try { window.MathJax.typesetClear(nodes); }
    catch (e) { console.warn('mathpreview engine clear:', e); }
  }
  function typeset(nodes) {
    return window.MathJax.typesetPromise(nodes);
  }
  window.__mpEngine = {
    name: 'mathjax',
    ready: ready,
    isReady: isReady,
    typesetClear: typesetClear,
    typeset: typeset
  };
})();
