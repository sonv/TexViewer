(function() {
  var currentProofMode = 'all';
  var currentPageMode = 'a4';
  var currentSideOpen = false;
  var refkeysVisible = false;
  var lineNumbersVisible = false;
  var lineNumbersScheduled = false;
  var marginMode = false;
  var themeMode = 'light';
  var pinnedRefs = new Map();
  var hoverPreviewTimer = 0;
  var hoverPreviewEl = null;
  var hoverPreviewSource = null;
  var topbarHidden = false;
  var navRefreshTimer = 0;
  // === PAGE-BREAK GUIDES (optional; config `page_guides`) ====================
  // A removable feature. To delete it entirely, remove: this block, the
  // matching "PAGE-BREAK GUIDES" blocks in viewer.js and footer.js, the
  // `.page-guide*` rules in default.css, the `config-page-guides` checkbox in
  // shell.rs, and the `page_guides` field in config.rs + serve.rs. See
  // PERFORMANCE.md ("Page-break guides").
  var pageGuidesEnabled = false;
  // An IntersectionObserver + a set of the blocks it has seen enter the
  // viewport. A "seen" block has been rendered at least once, so its measured
  // height is real — not a content-visibility estimate. Guides are drawn only
  // across the contiguous run of seen, typeset blocks from the top, where the
  // cumulative layout is trustworthy. See rebuildPageGuides in viewer.js.
  var guideObserver = null;
  var guideSeen = null;
  var lastPageGuideSignature = '';
  var guideRebuildScheduled = false;
  // Printable A4 content height as a fraction of page width: (297 − 2×17mm) /
  // 210mm. Matches the @page margin in default.css so a guide marks where
  // Cmd+P actually breaks.
  var A4_PRINT_CONTENT_RATIO = (297 - 2 * 17) / 210;
  // === END PAGE-BREAK GUIDES ================================================
  var currentUserZoom = 1;
  var ZOOM_MIN = 0.5;
  var ZOOM_MAX = 3;
  var ZOOM_STEP = 0.1;
  var NAV_IDLE_MS = 220;
  var NAV_RENDER_IDLE_MS = 900;
  var NAV_RESIZE_IDLE_MS = 120;
  var A4_CSS_WIDTH = 794;
  var DYNAMIC_BASE_WIDTH = 720;
  var navNeedsIndex = true;
  var lastHeadingSignature = '';
  var selectedMath = null;
  var activeSourceId = null;
  var sourceFlashTimer = 0;
  // Element ids of the editor's current visual selection (persistent highlight,
  // re-applied across re-renders). Empty when no selection is active.
  var activeSourceRangeIds = [];
  // Per-block selected rows for multi-row math (align/gather): [{id,count,rows}].
  var activeMathRows = [];
  var vimPendingKey = '';
  var vimPendingTimer = 0;
  var lastSearchQuery = '';
  var mathSearchQuery = '';
  var mathSearchResults = [];
  var mathSearchIndex = -1;
  // Plain-text search: our own match list (Ranges) so `/` cycles at the ends
  // and shows current/total — native window.find gives neither.
  var textSearchQuery = '';
  var textSearchResults = [];
  var textSearchIndex = -1;
  // Word-completion suggestions shown under the `/` box as you type.
  var searchSuggestions = [];
  var searchSuggestIndex = -1;
  // Editor-driven search: the nvim `/` pattern, pushed by the plugin and
  // highlighted (all matches) in the preview like vim's hlsearch.
  var editorSearchQuery = '';
  var viewerJumpStack = [];

  function pageEl() {
    return document.getElementById('page');
  }

  function pageShellEl() {
    return document.getElementById('page-shell');
  }

  function cleanNavText(text) {
    return (text || '').replace(/\s+/g, ' ').trim();
  }

  function headingLevel(heading) {
    for (var i = 0; i < heading.classList.length; i++) {
      var m = /^sec-h(\d+)$/.exec(heading.classList[i]);
      if (m) return parseInt(m[1], 10);
    }
    return 2;
  }

  function headingSelector() {
    return '.sec-h0, .sec-h1, .sec-h2, .sec-h3, .sec-h4, .sec-h5, .sec-h6';
  }

  function topbarOffset() {
    if (topbarHidden || document.body.classList.contains('topbar-hidden')) return 12;
    var topbar = document.querySelector('.topbar');
    if (!topbar) return 58;
    return Math.max(12, Math.round(topbar.getBoundingClientRect().height + 8));
  }

  function scrollToTarget(target) {
    if (!target) return;
    recordViewerPlace();
    var y = target.getBoundingClientRect().top + window.scrollY - topbarOffset();
    window.scrollTo({ top: Math.max(0, y), behavior: 'smooth' });
  }

  function scrollByVim(dx, dy) {
    window.scrollBy({ left: dx, top: dy, behavior: 'auto' });
  }

  function clearVimPending() {
    vimPendingKey = '';
    if (vimPendingTimer) {
      clearTimeout(vimPendingTimer);
      vimPendingTimer = 0;
    }
  }

  function setVimPending(key) {
    clearVimPending();
    vimPendingKey = key;
    vimPendingTimer = setTimeout(clearVimPending, 750);
  }

  function isEditableTarget(target) {
    if (!target || !target.closest) return false;
    return !!target.closest('input, textarea, select, [contenteditable=""], [contenteditable="true"]');
  }

  function recordViewerPlace() {
    var place = { x: window.scrollX || 0, y: window.scrollY || 0 };
    var last = viewerJumpStack.length ? viewerJumpStack[viewerJumpStack.length - 1] : null;
    if (last && Math.abs(last.x - place.x) < 24 && Math.abs(last.y - place.y) < 24) {
      return false;
    }
    viewerJumpStack.push(place);
    if (viewerJumpStack.length > 100) viewerJumpStack.shift();
    return true;
  }

  function restorePreviousPlace() {
    clearVimPending();
    var place = viewerJumpStack.pop();
    if (!place) {
      setStatus('dead', '○ no previous place');
      return false;
    }
