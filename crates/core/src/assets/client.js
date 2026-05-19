(function() {
  var currentProofMode = 'all';
  var currentSideTab = 'index';
  var currentPageMode = 'a4';
  var currentSideOpen = false;
  var refkeysVisible = false;
  var marginMode = false;
  var pinnedRefs = new Map();
  var hoverPreviewTimer = 0;
  var hoverPreviewEl = null;
  var hoverPreviewSource = null;
  var topbarHidden = false;
  var navRefreshTimer = 0;
  var activePageTimer = 0;
  var pageGuideLayoutHeightPx = 0;
  var pageGuideVisualHeightPx = 0;
  var pageGuideCount = 1;
  var currentPageScale = 1;
  var NAV_IDLE_MS = 220;
  var NAV_RENDER_IDLE_MS = 900;
  var NAV_RESIZE_IDLE_MS = 120;
  var A4_CSS_WIDTH = 794;
  var A4_RATIO = 297 / 210;
  var navNeedsIndex = true;
  var navNeedsPages = true;
  var lastHeadingSignature = '';
  var lastPageGuideSignature = '';
  var selectedMath = null;
  var activeSourceId = null;
  var sourceFlashTimer = 0;
  var vimPendingKey = '';
  var vimPendingTimer = 0;
  var lastSearchQuery = '';
  var mathSearchQuery = '';
  var mathSearchResults = [];
  var mathSearchIndex = -1;
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

  function pageTopY() {
    var page = pageEl();
    if (!page) return 0;
    return page.getBoundingClientRect().top + window.scrollY;
  }

  function topbarOffset() {
    if (topbarHidden || document.body.classList.contains('topbar-hidden')) return 12;
    var topbar = document.querySelector('.topbar');
    if (!topbar) return 58;
    return Math.max(12, Math.round(topbar.getBoundingClientRect().height + 8));
  }

  function scrollToPage(pageNo) {
    if (!pageGuideVisualHeightPx) refreshNavigation();
    recordViewerPlace();
    var y = pageTopY() + (pageNo - 1) * pageGuideVisualHeightPx - topbarOffset();
    window.scrollTo({ top: Math.max(0, y), behavior: 'smooth' });
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
    window.scrollTo({ left: place.x, top: place.y, behavior: 'smooth' });
    setStatus('live', '● previous place');
    return true;
  }

  function searchPanelEl() {
    return document.getElementById('search-panel');
  }

  function searchInputEl() {
    return document.getElementById('search-input');
  }

  function searchPanelIsOpen() {
    var panel = searchPanelEl();
    return !!(panel && !panel.hidden);
  }

  function openSearchPanel() {
    var panel = searchPanelEl();
    var input = searchInputEl();
    if (!panel || !input) return;
    panel.hidden = false;
    input.value = lastSearchQuery;
    input.focus({ preventScroll: true });
    input.select();
  }

  function closeSearchPanel() {
    var panel = searchPanelEl();
    var input = searchInputEl();
    if (panel) panel.hidden = true;
    if (input && document.activeElement === input) input.blur();
    clearSearchSession();
  }

  var TEX_SYMBOL_CODEPOINTS = {
    alpha: [0x03B1],
    beta: [0x03B2],
    gamma: [0x03B3],
    delta: [0x03B4],
    epsilon: [0x03F5, 0x03B5],
    varepsilon: [0x03B5, 0x03F5],
    zeta: [0x03B6],
    eta: [0x03B7],
    theta: [0x03B8],
    vartheta: [0x03D1],
    iota: [0x03B9],
    kappa: [0x03BA],
    lambda: [0x03BB],
    mu: [0x03BC],
    nu: [0x03BD],
    xi: [0x03BE],
    pi: [0x03C0],
    varpi: [0x03D6],
    rho: [0x03C1],
    varrho: [0x03F1],
    sigma: [0x03C3],
    varsigma: [0x03C2],
    tau: [0x03C4],
    upsilon: [0x03C5],
    phi: [0x03D5, 0x03C6],
    varphi: [0x03C6, 0x03D5],
    chi: [0x03C7],
    psi: [0x03C8],
    omega: [0x03C9],
    Gamma: [0x0393],
    Delta: [0x0394],
    Theta: [0x0398],
    Lambda: [0x039B],
    Xi: [0x039E],
    Pi: [0x03A0],
    Sigma: [0x03A3],
    Upsilon: [0x03A5],
    Phi: [0x03A6],
    Psi: [0x03A8],
    Omega: [0x03A9],
    partial: [0x2202],
    infty: [0x221E],
    nabla: [0x2207],
    grad: [0x2207],
    leq: [0x2264],
    geq: [0x2265],
    neq: [0x2260],
    times: [0x00D7],
    cdot: [0x22C5],
    pm: [0x00B1],
    mp: [0x2213],
    to: [0x2192],
    mapsto: [0x21A6],
    leftarrow: [0x2190],
    rightarrow: [0x2192],
    Leftrightarrow: [0x21D4],
    Rightarrow: [0x21D2],
    subset: [0x2282],
    subseteq: [0x2286],
    in: [0x2208],
    notin: [0x2209],
    forall: [0x2200],
    exists: [0x2203],
    emptyset: [0x2205],
    setminus: [0x2216],
    cup: [0x222A],
    cap: [0x2229],
    int: [0x222B],
    sum: [0x2211],
    prod: [0x220F]
  };

  function stripMathDelimiters(query) {
    return (query || '')
      .trim()
      .replace(/^\\\(/, '')
      .replace(/\\\)$/, '')
      .replace(/^\\\[/, '')
      .replace(/\\\]$/, '')
      .replace(/^\$+/, '')
      .replace(/\$+$/, '')
      .trim();
  }

  function looksLikeMathSearch(query) {
    return /\\|[_^{}$]/.test(query || '');
  }

  function glyphCharsForTeXCommand(command) {
    var cps = TEX_SYMBOL_CODEPOINTS[command];
    if (!cps) return [];
    return cps.map(function(cp) { return String.fromCodePoint(cp); });
  }

  function mathSearchSpec(query) {
    var core = stripMathDelimiters(query);
    var texNeedles = [];
    var glyphChars = [];
    if (core) texNeedles.push(core);
    if (query && query !== core) texNeedles.push(query);
    var command = /^\\([A-Za-z]+)$/.exec(core);
    if (command) {
      glyphChars = glyphCharsForTeXCommand(command[1]);
      texNeedles.push('\\' + command[1]);
    } else if (core && Array.from(core).length === 1) {
      glyphChars.push(core);
    } else if (/^[A-Za-z]+$/.test(core)) {
      glyphChars = glyphCharsForTeXCommand(core);
      if (glyphChars.length) texNeedles.push('\\' + core);
    }
    return {
      core: core,
      texNeedles: Array.from(new Set(texNeedles.filter(Boolean))),
      glyphChars: Array.from(new Set(glyphChars))
    };
  }

  function svgDataCodeForChar(ch) {
    return ch.codePointAt(0).toString(16).toUpperCase();
  }

  function mathGlyphMatches(math, glyphChars) {
    if (!glyphChars.length || !math || !math.querySelectorAll) return [];
    var wanted = new Set(glyphChars.map(svgDataCodeForChar));
    var matches = [];
    math.querySelectorAll('svg [data-c]').forEach(function(node) {
      var code = (node.getAttribute('data-c') || '').toUpperCase();
      if (wanted.has(code)) matches.push(node);
    });
    return matches;
  }

  function texContainsAny(tex, needles) {
    if (!tex || !needles.length) return false;
    return needles.some(function(needle) {
      return needle && tex.indexOf(needle) !== -1;
    });
  }

  function clearMathSearchHighlights() {
    document.querySelectorAll('.math-search-hit, .math-search-active').forEach(function(el) {
      el.classList.remove('math-search-hit', 'math-search-active');
    });
    document.querySelectorAll('.math-search-glyph-hit, .math-search-glyph-active').forEach(function(el) {
      el.classList.remove('math-search-glyph-hit', 'math-search-glyph-active');
    });
  }

  function clearSearchSession() {
    mathSearchQuery = '';
    mathSearchResults = [];
    mathSearchIndex = -1;
    clearMathSearchHighlights();
    var selection = window.getSelection ? window.getSelection() : null;
    if (selection && selection.removeAllRanges) selection.removeAllRanges();
  }

  function buildMathSearchResults(query) {
    var page = pageEl();
    if (!page) return [];
    var spec = mathSearchSpec(query);
    if (!spec.core) return [];
    var results = [];
    page.querySelectorAll('.math[data-tex]').forEach(function(math) {
      var tex = math.getAttribute('data-tex') || '';
      var glyphs = mathGlyphMatches(math, spec.glyphChars);
      if (glyphs.length) {
        glyphs.forEach(function(glyph) {
          results.push({ math: math, target: glyph, glyph: glyph });
        });
        return;
      }
      if (looksLikeMathSearch(query) && texContainsAny(tex, spec.texNeedles)) {
        results.push({ math: math, target: math, glyph: null });
      }
    });
    return results;
  }

  function mathResultTop(result) {
    var target = result && result.target;
    if (!target || !target.getBoundingClientRect) return 0;
    return target.getBoundingClientRect().top + window.scrollY;
  }

  function firstMathResultIndex(results, backwards) {
    if (!results.length) return -1;
    var y = window.scrollY + topbarOffset() + 1;
    if (backwards) {
      for (var i = results.length - 1; i >= 0; i--) {
        if (mathResultTop(results[i]) < y) return i;
      }
      return results.length - 1;
    }
    for (var j = 0; j < results.length; j++) {
      if (mathResultTop(results[j]) >= y) return j;
    }
    return 0;
  }

  function applyMathSearchHighlights(activeResult) {
    if (!searchPanelIsOpen()) {
      clearMathSearchHighlights();
      return;
    }
    clearMathSearchHighlights();
    mathSearchResults.forEach(function(result) {
      if (result.math && result.math.classList) result.math.classList.add('math-search-hit');
      if (result.glyph && result.glyph.classList) result.glyph.classList.add('math-search-glyph-hit');
    });
    if (!activeResult) return;
    if (activeResult.math && activeResult.math.classList) {
      activeResult.math.classList.add('math-search-active');
    }
    if (activeResult.glyph && activeResult.glyph.classList) {
      activeResult.glyph.classList.add('math-search-glyph-active');
    }
  }

  function runMathSearch(query, backwards) {
    if (!searchPanelIsOpen()) return false;
    var previousQuery = mathSearchQuery;
    var previousTarget = mathSearchResults[mathSearchIndex] && mathSearchResults[mathSearchIndex].target;
    mathSearchResults = buildMathSearchResults(query);
    if (!mathSearchResults.length) {
      mathSearchQuery = '';
      mathSearchIndex = -1;
      clearMathSearchHighlights();
      return false;
    }

    mathSearchQuery = query;
    var nextIndex = -1;
    if (previousQuery === query && previousTarget) {
      for (var i = 0; i < mathSearchResults.length; i++) {
        if (mathSearchResults[i].target === previousTarget) {
          nextIndex = backwards ? i - 1 : i + 1;
          break;
        }
      }
    }
    if (nextIndex < 0 || nextIndex >= mathSearchResults.length) {
      nextIndex = previousQuery === query && mathSearchIndex >= 0
        ? mathSearchIndex + (backwards ? -1 : 1)
        : firstMathResultIndex(mathSearchResults, backwards);
    }
    if (nextIndex < 0) nextIndex = mathSearchResults.length - 1;
    if (nextIndex >= mathSearchResults.length) nextIndex = 0;

    mathSearchIndex = nextIndex;
    var active = mathSearchResults[mathSearchIndex];
    applyMathSearchHighlights(active);
    scrollSourceIntoView(active.target || active.math);
    setStatus('live',
      '● math ' + (mathSearchIndex + 1) + '/' + mathSearchResults.length + ' ' + query);
    return true;
  }

  function restoreMathSearchHighlights() {
    if (!searchPanelIsOpen()) {
      clearMathSearchHighlights();
      return;
    }
    if (!mathSearchQuery) return;
    var oldIndex = mathSearchIndex;
    mathSearchResults = buildMathSearchResults(mathSearchQuery);
    if (!mathSearchResults.length) {
      clearMathSearchHighlights();
      mathSearchIndex = -1;
      return;
    }
    mathSearchIndex = Math.max(0, Math.min(oldIndex, mathSearchResults.length - 1));
    applyMathSearchHighlights(mathSearchResults[mathSearchIndex]);
  }

  function runSearch(backwards) {
    var input = searchInputEl();
    var query = input ? input.value.trim() : lastSearchQuery;
    if (!query) {
      openSearchPanel();
      return false;
    }
    lastSearchQuery = query;
    if (input && document.activeElement === input) input.blur();
    var recorded = recordViewerPlace();
    if (looksLikeMathSearch(query) && runMathSearch(query, backwards)) {
      return true;
    }
    if (mathSearchQuery) clearSearchSession();
    var found = false;
    try {
      if (window.find) {
        found = window.find(query, false, !!backwards, true, false, false, false);
      }
    } catch (e) {
      found = false;
    }
    if (!found && runMathSearch(query, backwards)) {
      return true;
    }
    if (!found && recorded) viewerJumpStack.pop();
    setStatus(found ? 'live' : 'dead', (found ? '● found ' : '○ no match ') + query);
    return found;
  }

  function handleVimNavigation(e) {
    if (e.defaultPrevented || e.altKey || e.metaKey || isEditableTarget(e.target)) return false;
    var vh = window.innerHeight || document.documentElement.clientHeight || 800;
    var vw = window.innerWidth || document.documentElement.clientWidth || 1000;
    var line = Math.max(28, Math.round(vh * 0.06));
    var col = Math.max(48, Math.round(vw * 0.08));

    if (e.ctrlKey) {
      if (e.key === 'd') {
        scrollByVim(0, Math.round(vh * 0.5));
        return true;
      }
      if (e.key === 'u') {
        scrollByVim(0, -Math.round(vh * 0.5));
        return true;
      }
      if (e.key === 'o') {
        restorePreviousPlace();
        return true;
      }
      return false;
    }

    switch (e.key) {
      case 'h':
        scrollByVim(-col, 0);
        return true;
      case 'j':
        scrollByVim(0, line);
        return true;
      case 'k':
        scrollByVim(0, -line);
        return true;
      case 'l':
        scrollByVim(col, 0);
        return true;
      case 'g':
        if (vimPendingKey === 'g') {
          clearVimPending();
          recordViewerPlace();
          window.scrollTo({ top: 0, left: window.scrollX, behavior: 'auto' });
        } else {
          setVimPending('g');
        }
        return true;
      case 'G':
        clearVimPending();
        recordViewerPlace();
        window.scrollTo({ top: document.documentElement.scrollHeight, left: window.scrollX, behavior: 'auto' });
        return true;
      case '/':
        clearVimPending();
        openSearchPanel();
        return true;
      case 'n':
        clearVimPending();
        runSearch(false);
        return true;
      case 'N':
        clearVimPending();
        runSearch(true);
        return true;
      default:
        clearVimPending();
        return false;
    }
  }

  function scrollSourceIntoView(target) {
    if (!target) return;
    var rect = target.getBoundingClientRect();
    var vh = window.innerHeight || document.documentElement.clientHeight || 800;
    var upper = vh * 0.25;
    var lower = vh * 0.75;
    var y = rect.top + Math.min(rect.height / 2, 12);
    if (y >= upper && y <= lower) return;
    recordViewerPlace();
    window.scrollTo({
      top: Math.max(0, window.scrollY + rect.top - upper),
      behavior: 'smooth'
    });
  }

  function visibleSyncElement(el) {
    if (!el) return null;
    if (el.classList && el.classList.contains('blk')) {
      return el.querySelector(':scope > :not(.page-guide-layer)') || el;
    }
    return el;
  }

  function revealSourceElement(id, shouldScroll) {
    if (!id) return;
    activeSourceId = id;
    document.querySelectorAll('.source-active').forEach(function(el) {
      el.classList.remove('source-active');
    });
    var raw = document.getElementById(id);
    var el = visibleSyncElement(raw);
    if (!el) return;
    el.classList.add('source-active');
    if (sourceFlashTimer) clearTimeout(sourceFlashTimer);
    sourceFlashTimer = setTimeout(function() {
      if (el && el.classList) el.classList.remove('source-active');
    }, 1800);
    if (shouldScroll) scrollSourceIntoView(el);
  }

  function restoreSourceHighlight() {
    if (activeSourceId) revealSourceElement(activeSourceId, false);
  }

  function sourceElementFromTarget(target) {
    if (!target || !target.closest) return null;
    var el = target.closest('#page [data-src]');
    var page = pageEl();
    return el && page && page.contains(el) ? el : null;
  }

  function parseDataSrc(src) {
    var m = /^(.+):(\d+):(\d+)$/.exec(src || '');
    if (!m) return null;
    return { file: m[1], line: parseInt(m[2], 10), col: parseInt(m[3], 10) };
  }

  async function postSourceJump(info) {
    if (!info) return;
    try {
      var res = await fetch('/jump', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(info)
      });
      if (!res.ok) throw new Error('jump failed');
      setStatus('live', '● source jump ' + info.line + ':' + info.col);
    } catch (e) {
      setStatus('dead', '○ source jump failed');
    }
  }

  function requestSourceJump(e) {
    var el = sourceElementFromTarget(e.target);
    var info = el ? parseDataSrc(el.getAttribute('data-src')) : null;
    if (!info) return false;
    e.preventDefault();
    e.stopPropagation();
    revealSourceElement(el.id, false);
    postSourceJump(info);
    return true;
  }

  function setSideOpen(open, persist) {
    currentSideOpen = !!open;
    document.body.classList.toggle('side-panel-open', currentSideOpen);
    document.body.classList.toggle('side-panel-closed', !currentSideOpen);
    var btn = document.getElementById('side-toggle');
    if (btn) {
      btn.classList.toggle('active', currentSideOpen);
      btn.setAttribute('aria-expanded', currentSideOpen ? 'true' : 'false');
    }
    if (persist) {
      try { localStorage.setItem('mathpreview.sideOpen', currentSideOpen ? '1' : '0'); } catch (e) {}
    }
  }

  function setRefkeysVisible(visible, persist) {
    refkeysVisible = !!visible;
    document.body.classList.toggle('refkey-visible', refkeysVisible);
    var page = pageEl();
    if (page) page.setAttribute('data-refkeys', refkeysVisible ? 'visible' : 'hidden');
    var btn = document.getElementById('refkey-toggle');
    if (btn) {
      btn.classList.toggle('active', refkeysVisible);
      btn.setAttribute('aria-pressed', refkeysVisible ? 'true' : 'false');
    }
    if (persist) {
      try { localStorage.setItem('mathpreview.refkeys', refkeysVisible ? '1' : '0'); } catch (e) {}
    }
  }

  function setMarginMode(on, persist) {
    marginMode = !!on;
    document.body.classList.toggle('margin-mode', marginMode);
    var btn = document.getElementById('margin-toggle');
    if (btn) {
      btn.classList.toggle('active', marginMode);
      btn.setAttribute('aria-pressed', marginMode ? 'true' : 'false');
    }
    if (persist) {
      try { localStorage.setItem('mathpreview.marginMode', marginMode ? '1' : '0'); } catch (e) {}
    }
    if (!marginMode) {
      closeAllMarginCards();
      clearSidenoteLayout();
    } else {
      scheduleSidenoteLayout();
    }
  }

  function marginEl() { return document.getElementById('margin'); }

  /// Toggle the `margin-has-cards` body class. Layout rules that shift
  /// the page content (`#page-shell`) live behind that class so the
  /// reading area stays centered when margin mode is on but no card is
  /// pinned. Called after every pinnedRefs mutation.
  function updateMarginCardsClass() {
    document.body.classList.toggle('margin-has-cards', pinnedRefs.size > 0);
  }

  function closeAllMarginCards() {
    var m = marginEl();
    if (m) m.innerHTML = '';
    pinnedRefs.clear();
    updateMarginCardsClass();
  }

  /// Returns the rendered DOM element referenced by a `<a class="ref|cite"
  /// href="#...">` link. For citations the target is the `<dt>` of a bib
  /// entry; we wrap it together with its `<dd>` sibling so the card shows
  /// the full reference.
  function resolveLinkTarget(link) {
    var href = link.getAttribute('href') || '';
    if (href.charAt(0) !== '#') return null;
    var id = href.slice(1);
    try { id = decodeURIComponent(id); } catch (e) {}
    return document.getElementById(id);
  }

  function clonePreviewContent(link, target) {
    // Citation: target is a <dt>, glue its <dd> sibling into the clone.
    if (link.classList.contains('cite') && target.tagName === 'DT') {
      var wrap = document.createElement('div');
      wrap.className = 'bib-preview';
      wrap.appendChild(target.cloneNode(true));
      var dd = target.nextElementSibling;
      if (dd && dd.tagName === 'DD') wrap.appendChild(dd.cloneNode(true));
      return wrap;
    }
    // Empty label-anchor — typically `\label{...}` placed at the top of a
    // `\begin{subequations}` group, where the label sits as a zero-content
    // marker BEFORE the actual `\begin{equation}` / `align` children.
    // Cloning the anchor alone yields an empty box, which is what the
    // user saw in hover / margin previews. Walk forward through its
    // following siblings and collect every math display that belongs to
    // the same logical group, stopping at the next label-anchor or any
    // non-math content (prose, the next paragraph, the block boundary).
    if (target.classList.contains('label-anchor') && !target.firstElementChild) {
      var bundle = document.createElement('div');
      bundle.className = 'subeq-preview';
      var el = target.nextElementSibling;
      while (el) {
        if (el.classList.contains('label-anchor')) break;
        if (el.classList.contains('source-space') ||
            el.classList.contains('src-word')) {
          el = el.nextElementSibling;
          continue;
        }
        if (el.classList.contains('math')) {
          bundle.appendChild(el.cloneNode(true));
          el = el.nextElementSibling;
          continue;
        }
        break;
      }
      if (bundle.children.length) return bundle;
    }
    return target.cloneNode(true);
  }

  function pinKeyFor(link) {
    return link.getAttribute('data-target') ||
           link.getAttribute('data-key') ||
           link.getAttribute('href') || '';
  }

  function buildMarginCard(link, clone) {
    var card = document.createElement('div');
    card.className = 'margin-card';
    card.dataset.pinKey = pinKeyFor(link);

    var head = document.createElement('div');
    head.className = 'margin-card-header';
    // Title: LaTeX `\label{...}` / cite-key in a monospace chip. The
    // rendered "Theorem 2.1" / "(3.1)" label is intentionally NOT shown
    // here — the card body already displays it.
    var title = document.createElement('span');
    title.className = 'margin-card-title';
    var rawKey = link.getAttribute('data-target') || link.getAttribute('data-key') || '';
    if (rawKey) {
      var keyChip = document.createElement('code');
      keyChip.className = 'margin-card-key';
      keyChip.textContent = rawKey;
      title.appendChild(keyChip);
    } else {
      title.textContent = (link.textContent || '').trim() || pinKeyFor(link);
    }
    var close = document.createElement('button');
    close.type = 'button';
    close.className = 'margin-card-close';
    close.setAttribute('aria-label', 'unpin');
    close.title = 'unpin';
    close.textContent = '×';
    head.appendChild(title);
    head.appendChild(close);

    var body = document.createElement('div');
    body.className = 'margin-card-body';
    body.appendChild(clone);

    card.appendChild(head);
    card.appendChild(body);
    return card;
  }

  function togglePinReference(link) {
    var key = pinKeyFor(link);
    if (!key) return;
    if (pinnedRefs.has(key)) {
      var existing = pinnedRefs.get(key);
      if (existing && existing.parentNode) existing.parentNode.removeChild(existing);
      pinnedRefs.delete(key);
      updateMarginCardsClass();
      return;
    }
    var target = resolveLinkTarget(link);
    if (!target) return;
    if (!marginMode) setMarginMode(true, true);
    var clone = clonePreviewContent(link, target);
    var card = buildMarginCard(link, clone);
    var margin = marginEl();
    if (!margin) return;
    margin.appendChild(card);
    pinnedRefs.set(key, card);
    updateMarginCardsClass();
  }

  function isPinnableLink(target) {
    if (!target || !target.closest) return null;
    return target.closest('#page a.ref[href^="#"], #page a.cite[href^="#"]');
  }

  function hideHoverPreview() {
    if (hoverPreviewTimer) {
      clearTimeout(hoverPreviewTimer);
      hoverPreviewTimer = 0;
    }
    if (hoverPreviewEl && hoverPreviewEl.parentNode) {
      hoverPreviewEl.parentNode.removeChild(hoverPreviewEl);
    }
    hoverPreviewEl = null;
    hoverPreviewSource = null;
  }

  function positionHoverPreview(el, anchor) {
    var rect = anchor.getBoundingClientRect();
    var pad = 8;
    el.style.left = '0px';
    el.style.top = '0px';
    var pw = el.offsetWidth;
    var ph = el.offsetHeight;
    var vw = window.innerWidth;
    var vh = window.innerHeight;
    var left = Math.min(rect.left, vw - pw - pad);
    var top = rect.bottom + 4;
    if (top + ph + pad > vh) top = Math.max(pad, rect.top - ph - 4);
    if (left < pad) left = pad;
    el.style.left = left + 'px';
    el.style.top = top + 'px';
  }

  function showHoverPreviewFor(link) {
    hideHoverPreview();
    var target = resolveLinkTarget(link);
    if (!target) return;
    var clone = clonePreviewContent(link, target);
    var box = document.createElement('div');
    box.className = 'hover-preview';
    box.appendChild(clone);
    document.body.appendChild(box);
    positionHoverPreview(box, link);
    hoverPreviewEl = box;
    hoverPreviewSource = link;
  }

  function scheduleHoverPreview(link) {
    if (hoverPreviewSource === link && hoverPreviewEl) return;
    if (hoverPreviewTimer) clearTimeout(hoverPreviewTimer);
    hoverPreviewTimer = setTimeout(function() {
      hoverPreviewTimer = 0;
      showHoverPreviewFor(link);
    }, 250);
  }

  function setTopbarHidden(hidden, persist) {
    topbarHidden = !!hidden;
    document.body.classList.toggle('topbar-hidden', topbarHidden);
    var stripe = document.getElementById('topbar-stripe');
    if (stripe) {
      stripe.setAttribute('aria-expanded', topbarHidden ? 'false' : 'true');
    }
    if (persist) {
      try { localStorage.setItem('mathpreview.topbarHidden', topbarHidden ? '1' : '0'); } catch (e) {}
    }
    updatePageScale();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
  }

  function setPageMode(mode) {
    currentPageMode = mode === 'dynamic' ? 'dynamic' : 'a4';
    document.body.classList.toggle('page-mode-a4', currentPageMode === 'a4');
    document.body.classList.toggle('page-mode-dynamic', currentPageMode === 'dynamic');
    document.querySelectorAll('.page-mode-toggle button').forEach(function(btn) {
      var active = btn.getAttribute('data-page-mode') === currentPageMode;
      btn.classList.toggle('active', active);
    });
    var toggle = document.querySelector('.page-mode-toggle');
    if (toggle) toggle.setAttribute('data-page-mode', currentPageMode);
    try { localStorage.setItem('mathpreview.pageMode', currentPageMode); } catch (e) {}
    lastPageGuideSignature = '';
    updatePageScale();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
  }

  function updatePageScale(contentHeight) {
    var page = pageEl();
    var shell = pageShellEl();
    if (!page || !shell) return;
    if (currentPageMode === 'a4') {
      var available = Math.max(320, document.documentElement.clientWidth - 32);
      currentPageScale = Math.min(1, available / A4_CSS_WIDTH);
      document.documentElement.style.setProperty('--page-scale', currentPageScale.toFixed(4));
      shell.style.width = Math.round(A4_CSS_WIDTH * currentPageScale) + 'px';
      if (typeof contentHeight !== 'number') contentHeight = page.scrollHeight;
      shell.style.height = Math.ceil(contentHeight * currentPageScale) + 'px';
    } else {
      currentPageScale = 1;
      document.documentElement.style.setProperty('--page-scale', '1');
      shell.style.width = '';
      shell.style.height = '';
    }
  }

  function pageGuideMetrics() {
    if (currentPageMode === 'a4') {
      var layoutHeight = A4_CSS_WIDTH * A4_RATIO;
      return {
        layoutHeight: layoutHeight,
        visualHeight: layoutHeight * currentPageScale
      };
    }
    var dynamicHeight = Math.max(560, Math.min(1100, window.innerHeight - 84));
    return {
      layoutHeight: dynamicHeight,
      visualHeight: dynamicHeight
    };
  }

  function setSideTab(tab) {
    currentSideTab = tab === 'pages' ? 'pages' : 'index';
    document.querySelectorAll('.side-tab').forEach(function(btn) {
      var active = btn.getAttribute('data-side-tab') === currentSideTab;
      btn.classList.toggle('active', active);
      btn.setAttribute('aria-selected', active ? 'true' : 'false');
    });
    var index = document.getElementById('side-index');
    var pages = document.getElementById('side-pages');
    if (index) index.hidden = currentSideTab !== 'index';
    if (pages) pages.hidden = currentSideTab !== 'pages';
    try { localStorage.setItem('mathpreview.sideTab', currentSideTab); } catch (e) {}
    updateActivePage();
  }

  function headingSignature(headings) {
    return headings.map(function(heading) {
      return heading.id + '|' + headingLevel(heading) + '|' + cleanNavText(heading.textContent);
    }).join('\n');
  }

  function rebuildIndex(force) {
    var page = pageEl();
    var index = document.getElementById('side-index');
    if (!page || !index) return;
    var headings = Array.from(page.querySelectorAll(headingSelector()));
    var signature = headingSignature(headings);
    if (!force && signature === lastHeadingSignature) return;
    lastHeadingSignature = signature;
    index.replaceChildren();
    if (!headings.length) {
      var empty = document.createElement('div');
      empty.className = 'side-empty';
      empty.textContent = 'No sections';
      index.appendChild(empty);
      return;
    }
    headings.forEach(function(heading) {
      if (!heading.id) return;
      var item = document.createElement('a');
      item.href = '#' + encodeURIComponent(heading.id);
      item.className = 'side-link side-level-' + headingLevel(heading);
      item.textContent = cleanNavText(heading.textContent);
      index.appendChild(item);
    });
  }

  function rebuildPageGuides() {
    var page = pageEl();
    var pages = document.getElementById('side-pages');
    if (!page || !pages) return;
    pages.setAttribute('aria-label', currentPageMode === 'a4' ? 'A4 pages' : 'dynamic pages');

    var totalHeight = page.scrollHeight;
    updatePageScale(totalHeight);
    var metrics = pageGuideMetrics();
    pageGuideLayoutHeightPx = metrics.layoutHeight;
    pageGuideVisualHeightPx = metrics.visualHeight;
    pageGuideCount = Math.max(1, Math.ceil(totalHeight / pageGuideLayoutHeightPx));
    var signature = currentPageMode + '|' + pageGuideCount + '|' + Math.round(pageGuideLayoutHeightPx);
    if (signature === lastPageGuideSignature) {
      updateActivePage();
      return;
    }
    lastPageGuideSignature = signature;

    var oldLayer = page.querySelector('.page-guide-layer');
    if (oldLayer) oldLayer.remove();

    var layer = document.createElement('div');
    layer.className = 'page-guide-layer';
    layer.setAttribute('aria-hidden', 'true');
    for (var i = 1; i < pageGuideCount; i++) {
      var guide = document.createElement('div');
      guide.className = 'page-guide';
      guide.style.top = Math.round(i * pageGuideLayoutHeightPx) + 'px';
      var label = document.createElement('span');
      label.textContent = (currentPageMode === 'a4' ? 'A4 page ' : 'Page ') + (i + 1);
      guide.appendChild(label);
      layer.appendChild(guide);
    }
    page.appendChild(layer);

    pages.replaceChildren();
    for (var p = 1; p <= pageGuideCount; p++) {
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'side-link page-link';
      btn.setAttribute('data-page-jump', String(p));
      btn.textContent = (currentPageMode === 'a4' ? 'A4 ' : 'Page ') + p;
      pages.appendChild(btn);
    }
    updateActivePage();
  }

  function refreshNavigation() {
    navRefreshTimer = 0;
    if (navNeedsIndex) rebuildIndex(false);
    if (navNeedsPages) rebuildPageGuides();
    navNeedsIndex = false;
    navNeedsPages = false;
    scheduleSidenoteLayout();
  }

  function clearSidenoteLayout() {
    var chips = document.querySelectorAll('main#page .sidenote');
    for (var i = 0; i < chips.length; i++) {
      chips[i].style.transform = '';
    }
  }

  /// Stack overlapping sidenote chips dynamically.
  ///
  /// `\AB{...}` and `\SV[date]{...}` next to each other in the source
  /// share the same inline static-position, so the absolutely-positioned
  /// chips land at the same y-coordinate and overlay one another. CSS
  /// `margin-top` on adjacent siblings can't fix this — static-position
  /// computation ignores sibling margins for inline-blocks on the same
  /// line.
  ///
  /// Algorithm: group chips by their natural `offsetTop`. Inside each
  /// group, walk in document order and translateY each subsequent chip
  /// by the cumulative height of the chips that precede it in the same
  /// group plus a small gap. Heights are read AFTER resetting transforms
  /// so we measure the chip's true rendered height — which differs
  /// between collapsed (~24 px) and expanded (variable) states.
  function layoutSidenotes() {
    var chips = document.querySelectorAll('main#page .sidenote');
    if (!chips.length) return;
    // Reset transforms first so offsetHeight / offsetTop measure the
    // natural box without the previously-applied translate.
    clearSidenoteLayout();
    if (!marginMode || !document.body.classList.contains('margin-mode')) return;
    // Force the browser to flush style/layout invalidations before we
    // read offsetTop / offsetHeight.
    void document.body.offsetHeight;
    // Walk chips in document order. Maintain the running "lowest y so
    // far occupied by a previous chip's bottom edge"; whenever the
    // next chip's natural offsetTop would overlap that, translateY it
    // downward by the difference. This covers BOTH cases that overlap
    // in practice:
    //   * Multiple chips on the same source line share `offsetTop`,
    //     so the second one starts inside the first one's box and
    //     gets pushed below it.
    //   * Chips in adjacent paragraphs with small inter-paragraph
    //     spacing can have non-equal offsetTop yet still visually
    //     overlap because a tall chip's content extends past the
    //     next chip's natural y; the inequality `top < prevBottom`
    //     catches that and shifts.
    var prevBottom = -Infinity;
    var gap = 4;
    for (var j = 0; j < chips.length; j++) {
      var chip = chips[j];
      var top = chip.offsetTop;
      var height = chip.offsetHeight;
      var minTop = prevBottom + gap;
      if (top < minTop) {
        chip.style.transform = 'translateY(' + (minTop - top) + 'px)';
        prevBottom = minTop + height;
      } else {
        prevBottom = top + height;
      }
    }
  }

  /// Run `layoutSidenotes` on the next animation frame.
  /// Coalesces several invocations (e.g., toggling multiple chips
  /// quickly) into one re-layout.
  var sidenoteLayoutScheduled = false;
  function scheduleSidenoteLayout() {
    if (sidenoteLayoutScheduled) return;
    sidenoteLayoutScheduled = true;
    requestAnimationFrame(function() {
      sidenoteLayoutScheduled = false;
      layoutSidenotes();
    });
  }

  function scheduleNavigationRefresh(delay, includeIndex) {
    navNeedsPages = true;
    if (includeIndex !== false) navNeedsIndex = true;
    if (navRefreshTimer) clearTimeout(navRefreshTimer);
    navRefreshTimer = setTimeout(refreshNavigation, typeof delay === 'number' ? delay : NAV_IDLE_MS);
  }

  function updateActivePage() {
    if (!pageGuideVisualHeightPx) return;
    var current = Math.floor((window.scrollY + topbarOffset() + 12 - pageTopY()) / pageGuideVisualHeightPx) + 1;
    current = Math.min(pageGuideCount, Math.max(1, current));
    document.querySelectorAll('.page-link').forEach(function(btn) {
      btn.classList.toggle('active', btn.getAttribute('data-page-jump') === String(current));
    });
  }

  function scheduleActivePageUpdate() {
    if (activePageTimer) return;
    activePageTimer = requestAnimationFrame(function() {
      activePageTimer = 0;
      updateActivePage();
    });
  }

  function refreshAfterInitialEngine(tries) {
    if (window.__mpEngine && window.__mpEngine.ready(function() {
      queueInitialTypeset();
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, true);
    })) return;
    if (tries > 0) {
      setTimeout(function() { refreshAfterInitialEngine(tries - 1); }, 150);
    }
  }

  function theoremRole(thm) {
    if (!thm) return null;
    if (thm.classList.contains('role-main')) return 'main';
    if (thm.classList.contains('role-supporting')) return 'supporting';
    if (thm.classList.contains('role-standard')) return 'standard';
    if (thm.classList.contains('role-omitted')) return 'omitted';
    return null;
  }

  function roleFromRefs(root) {
    if (!root || !root.querySelectorAll) return null;
    var refs = root.querySelectorAll(".ref[href^='#'], .ref[data-target]");
    for (var i = 0; i < refs.length; i++) {
      var href = refs[i].getAttribute('href') || '';
      var id = href.charAt(0) === '#' ? href.slice(1) : '';
      if (!id && refs[i].dataset.target) {
        id = refs[i].dataset.target.replace(/[^A-Za-z0-9_-]/g, '-');
      }
      if (!id) continue;
      try { id = decodeURIComponent(id); } catch (e) {}
      var target = document.getElementById(id);
      if (!target) continue;
      if (target.classList.contains('thm')) return theoremRole(target);
      var thm = target.closest ? target.closest('.thm') : null;
      if (thm) return theoremRole(thm);
    }
    return null;
  }

  function theoremRoleInBlock(block) {
    if (!block) return null;
    if (block.classList && block.classList.contains('thm')) {
      return theoremRole(block);
    }
    var thm = block.querySelector ? block.querySelector('.thm') : null;
    return thm ? theoremRole(thm) : null;
  }

  function isEmptyBlock(block) {
    if (!block) return true;
    if (block.querySelector && block.querySelector('.thm, .proof, .math, .sec-h0, .sec-h1, .sec-h2, .sec-h3, .sec-h4, .sec-h5, .sec-h6')) {
      return false;
    }
    return !(block.textContent || '').trim();
  }

  function precedingTheoremRole(proof) {
    // Top-level render blocks are wrapped in <article class="blk"> for
    // patching, so the proof's logical predecessor is usually in a previous
    // block wrapper rather than as proof.previousElementSibling.
    var block = proof.closest('.blk') || proof;
    var el = block.previousElementSibling;
    while (el) {
      var role = theoremRoleInBlock(el);
      if (role) return role;
      if (!isEmptyBlock(el)) return null;
      el = el.previousElementSibling;
    }
    return null;
  }

  function referencedTheoremRole(proof) {
    return roleFromRefs(proof.querySelector('.proof-head'));
  }

  function sectionProofRole(proof) {
    var block = proof.closest('.blk') || proof;
    var el = block.previousElementSibling;
    while (el) {
      var section = el.querySelector ? el.querySelector('.sec-h0, .sec-h1, .sec-h2, .sec-h3, .sec-h4, .sec-h5, .sec-h6') : null;
      if (section) {
        if (/\bProof\s+of\b/i.test(section.textContent || '')) {
          return roleFromRefs(section);
        }
        return null;
      }
      el = el.previousElementSibling;
    }
    return null;
  }

  function applyMode(mode) {
    currentProofMode = mode;
    document.getElementById('page').setAttribute('data-proof-mode', mode);
    document.querySelectorAll('.proof').forEach(function(p) {
      var role = theoremRole(p) || referencedTheoremRole(p) || precedingTheoremRole(p) || sectionProofRole(p);
      var folded;
      if (mode === 'all')        folded = false;
      else if (mode === 'main')  folded = (role !== 'main');
      else                       folded = (role !== 'main' && role !== 'supporting');
      if (role === null) folded = false;
      p.classList.toggle('folded', folded);
    });
    scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
  }

  async function restartServer() {
    var btn = document.getElementById('server-restart');
    if (btn) btn.disabled = true;
    setStatus('updating', '↻ restarting');
    try {
      var res = await fetch('/restart', { method: 'POST', cache: 'no-store' });
      if (!res.ok) throw new Error('restart failed');
    } catch (e) {
      if (btn) btn.disabled = false;
      setStatus('dead', '○ restart failed');
      return;
    }
    setTimeout(function() {
      var started = performance.now();
      function poll() {
        fetch('/?restart=' + Date.now(), { cache: 'no-store' })
          .then(function(res) {
            if (!res.ok) throw new Error('not ready');
            location.reload();
          })
          .catch(function() {
            if (performance.now() - started > 20000) {
              if (btn) btn.disabled = false;
              setStatus('dead', '○ restart timeout');
              return;
            }
            setTimeout(poll, 300);
          });
      }
      poll();
    }, 700);
  }

  var manualStopRequested = false;
  function setStopButtonMode(stopped) {
    var btn = document.getElementById('server-stop');
    if (!btn) return;
    btn.textContent = stopped ? 'start' : 'stop';
    btn.title = stopped ? 'reload when preview server is running' : 'stop preview server';
    btn.classList.toggle('is-start', stopped);
  }

  function startServer() {
    var stopBtn = document.getElementById('server-stop');
    if (stopBtn) stopBtn.disabled = true;
    setStatus('updating', '↻ waiting');
    var started = performance.now();
    function poll() {
      fetch('/?start=' + Date.now(), { cache: 'no-store' })
        .then(function(res) {
          if (!res.ok) throw new Error('not ready');
          location.reload();
        })
        .catch(function() {
          if (performance.now() - started > 20000) {
            if (stopBtn) stopBtn.disabled = false;
            setStatus('dead', '○ start unavailable');
            return;
          }
          setTimeout(poll, 300);
        });
    }
    poll();
  }

  async function stopServer() {
    var stopBtn = document.getElementById('server-stop');
    var restartBtn = document.getElementById('server-restart');
    if (stopBtn) stopBtn.disabled = true;
    if (restartBtn) restartBtn.disabled = true;
    manualStopRequested = true;
    setStatus('updating', '↻ stopping');
    try {
      var res = await fetch('/stop', { method: 'POST', cache: 'no-store' });
      if (!res.ok) throw new Error('stop failed');
      if (stopBtn) stopBtn.disabled = false;
      setStopButtonMode(true);
      setStatus('dead', '○ stopped');
    } catch (e) {
      manualStopRequested = false;
      if (stopBtn) stopBtn.disabled = false;
      if (restartBtn) restartBtn.disabled = false;
      setStopButtonMode(false);
      setStatus('dead', '○ stop failed');
    }
  }

  function closestMath(node) {
    if (!node) return null;
    var el = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
    return el && el.closest ? el.closest('.math[data-tex]') : null;
  }

  function rangeIntersectsNode(range, node) {
    try { return range.intersectsNode(node); }
    catch (e) { return false; }
  }

  function selectedMathNodes(selection) {
    var page = pageEl() || document;
    var result = [];
    var seen = new Set();
    var math = page.querySelectorAll('.math[data-tex]');
    for (var r = 0; r < selection.rangeCount; r++) {
      var range = selection.getRangeAt(r);
      math.forEach(function(node) {
        if (seen.has(node) || !rangeIntersectsNode(range, node)) return;
        seen.add(node);
        result.push(node);
      });
    }
    return result;
  }

  function mathCopyTex(node) {
    return node ? (node.getAttribute('data-tex') || '') : '';
  }

  function clearSelectedMath() {
    if (selectedMath) selectedMath.classList.remove('math-selected');
    selectedMath = null;
  }

  function focusMathNode(math) {
    if (!math || !math.focus) return;
    try { math.focus({ preventScroll: true }); }
    catch (e) { math.focus(); }
  }

  function fragmentLatexText(node) {
    if (!node) return '';
    if (node.nodeType === Node.TEXT_NODE) return node.nodeValue || '';
    if (node.nodeType !== Node.ELEMENT_NODE && node.nodeType !== Node.DOCUMENT_FRAGMENT_NODE) {
      return '';
    }
    if (node.nodeType === Node.ELEMENT_NODE) {
      if (node.matches && node.matches('.math[data-tex]')) {
        var tex = mathCopyTex(node);
        return node.classList.contains('display') ? '\n' + tex + '\n' : tex;
      }
      if (node.matches && node.matches('.para-indent-marker, .page-guide-layer, .fold-marker')) {
        return '';
      }
      if (node.hidden) return '';
      if (node.tagName === 'BR') return '\n';
    }

    var text = '';
    var child = node.firstChild;
    while (child) {
      text += fragmentLatexText(child);
      child = child.nextSibling;
    }

    if (node.nodeType === Node.ELEMENT_NODE) {
      var tag = node.tagName;
      if (/^(P|DIV|ARTICLE|SECTION|H[1-6]|LI|DT|DD|TR)$/.test(tag) && text && !/\n$/.test(text)) {
        text += '\n';
      }
    }
    return text;
  }

  function normalizeCopiedLatex(text) {
    return (text || '')
      .replace(/\u00a0/g, ' ')
      .replace(/[ \t]+\n/g, '\n')
      .replace(/\n{3,}/g, '\n\n')
      .trim();
  }

  function selectionIsExactNode(selection, node) {
    if (!selection || selection.rangeCount !== 1 || !node || !node.parentNode) return false;
    var range = selection.getRangeAt(0);
    return range.startContainer === node.parentNode &&
      range.endContainer === node.parentNode &&
      range.endOffset === range.startOffset + 1 &&
      node.parentNode.childNodes[range.startOffset] === node;
  }

  function copySelectionAsLatex(e) {
    var selection = window.getSelection ? window.getSelection() : null;
    if (!selection || !selection.rangeCount) return;

    var activeMath = closestMath(document.activeElement);
    if (selection.isCollapsed && activeMath) {
      e.clipboardData.setData('text/plain', mathCopyTex(activeMath));
      e.preventDefault();
      return;
    }
    if (selectedMath &&
        selectedMath.isConnected &&
        selectedMath.classList.contains('math-selected') &&
        selectionIsExactNode(selection, selectedMath)) {
      e.clipboardData.setData('text/plain', mathCopyTex(selectedMath));
      e.preventDefault();
      return;
    }
    if (selection.isCollapsed) return;

    var mathNodes = selectedMathNodes(selection);
    if (!mathNodes.length) return;

    var range = selection.getRangeAt(0);
    var fragment = range.cloneContents();
    var text = fragmentLatexText(fragment);
    if (!text || !fragment.querySelector || !fragment.querySelector('.math[data-tex]')) {
      var commonMath = closestMath(range.commonAncestorContainer);
      if (commonMath) {
        text = mathCopyTex(commonMath);
      }
    }
    if (!text) {
      text = mathNodes.map(mathCopyTex).filter(Boolean).join('\n\n');
    }

    e.clipboardData.setData('text/plain', normalizeCopiedLatex(text));
    e.preventDefault();
  }

  function selectMathNode(math) {
    if (!math) return;
    clearSelectedMath();
    selectedMath = math;
    math.classList.add('math-selected');
    focusMathNode(math);
    var range = document.createRange();
    range.selectNode(math);
    var selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  }

  // Event delegation survives `#page` innerHTML replacement.
  document.addEventListener('copy', copySelectionAsLatex);
  document.addEventListener('mousedown', function(e) {
    clearSelectedMath();
  });
  document.addEventListener('dblclick', function(e) {
    requestSourceJump(e);
  });
  document.addEventListener('mouseover', function(e) {
    var link = isPinnableLink(e.target);
    if (link) scheduleHoverPreview(link);
  });
  document.addEventListener('mouseout', function(e) {
    var link = isPinnableLink(e.target);
    if (!link) return;
    var related = e.relatedTarget;
    if (related && link.contains(related)) return;
    hideHoverPreview();
  });
  document.addEventListener('scroll', hideHoverPreview, { passive: true });
  document.addEventListener('click', function(e) {
    var restart = e.target.closest('#server-restart');
    if (restart) {
      restartServer();
      return;
    }
    var stop = e.target.closest('#server-stop');
    if (stop) {
      if (manualStopRequested) startServer();
      else stopServer();
      return;
    }
    var topbarStripe = e.target.closest('#topbar-stripe');
    if (topbarStripe) {
      setTopbarHidden(!topbarHidden, true);
      return;
    }
    // Sidenote chip (\SV / \AB / \sidenote) — toggle its content
    // visibility. Uses aria-expanded on the button + hidden on the
    // content span + a data-open flag on the wrapper so CSS can
    // re-style the marker as a tab header when expanded.
    // Click anywhere on a sidenote chip jumps nvim to the `\SV{...}` /
    // `\AB{...}` / `\sidenote{...}` source line. There is no open/close
    // anymore — chips are always expanded and their dynamic positions
    // are recomputed by `layoutSidenotes` on every render/resize.
    var sidenoteChip = e.target.closest('.sidenote');
    if (sidenoteChip) {
      e.preventDefault();
      requestSourceJump(e);
      return;
    }
    var sideToggle = e.target.closest('#side-toggle');
    if (sideToggle) {
      setSideOpen(!currentSideOpen, true);
      return;
    }
    var pageMode = e.target.closest('.page-mode-toggle button');
    if (pageMode) {
      setPageMode(pageMode.getAttribute('data-page-mode'));
      return;
    }
    var refkeyToggle = e.target.closest('#refkey-toggle');
    if (refkeyToggle) {
      setRefkeysVisible(!refkeysVisible, true);
      return;
    }
    var marginToggle = e.target.closest('#margin-toggle');
    if (marginToggle) {
      setMarginMode(!marginMode, true);
      return;
    }
    var marginClose = e.target.closest('.margin-card-close');
    if (marginClose) {
      var card = marginClose.closest('.margin-card');
      if (card) {
        var key = card.dataset.pinKey;
        if (key && pinnedRefs.has(key)) pinnedRefs.delete(key);
        if (card.parentNode) card.parentNode.removeChild(card);
        updateMarginCardsClass();
      }
      return;
    }
    if ((e.altKey || e.metaKey) && requestSourceJump(e)) {
      return;
    }
    // In margin mode, plain click on a \ref or \cite pins it to the
    // margin column instead of scrolling to the anchor. Off-mode keeps
    // the existing scroll-to-anchor behavior (handled below).
    var pinnable = isPinnableLink(e.target);
    if (pinnable && marginMode) {
      e.preventDefault();
      hideHoverPreview();
      togglePinReference(pinnable);
      return;
    }
    var clickedMath = e.target.closest('.math[data-tex]');
    if (clickedMath) {
      if (e.shiftKey) {
        e.preventDefault();
        selectMathNode(clickedMath);
        return;
      }
      focusMathNode(clickedMath);
      return;
    }
    var sideTab = e.target.closest('.side-tab');
    if (sideTab) {
      setSideTab(sideTab.getAttribute('data-side-tab'));
      return;
    }
    var pageJump = e.target.closest('[data-page-jump]');
    if (pageJump) {
      scrollToPage(parseInt(pageJump.getAttribute('data-page-jump'), 10));
      return;
    }
    var indexLink = e.target.closest('#side-index a');
    if (indexLink && (indexLink.getAttribute('href') || '').charAt(0) === '#') {
      e.preventDefault();
      var id = indexLink.getAttribute('href').slice(1);
      try { id = decodeURIComponent(id); } catch (err) {}
      scrollToTarget(document.getElementById(id));
      return;
    }
    var pageHashLink = e.target.closest('#page a[href^=\'#\']');
    if (pageHashLink) {
      recordViewerPlace();
      return;
    }
    var btn = e.target.closest('.proof-toggle button');
    if (btn) {
      var mode = btn.getAttribute('data-mode');
      applyMode(mode);
      document.querySelectorAll('.proof-toggle button').forEach(function(x) {
        x.classList.toggle('active', x === btn);
      });
      document.querySelector('.proof-toggle').setAttribute('data-mode', mode);
      return;
    }
    var head = e.target.closest('.proof-head');
    if (head) {
      head.closest('.proof').classList.toggle('folded');
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
    }
  });
  document.addEventListener('keydown', function(e) {
    var searchInput = searchInputEl();
    if (e.target === searchInput) {
      if (e.key === 'Escape') {
        e.preventDefault();
        closeSearchPanel();
        return;
      }
      if (e.key === 'Enter') {
        e.preventDefault();
        runSearch(e.shiftKey);
        return;
      }
      return;
    }

    var head = e.target.closest('.proof-head');
    if (head && (e.key === 'Enter' || e.key === ' ')) {
      e.preventDefault();
      head.closest('.proof').classList.toggle('folded');
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
      return;
    }

    if (handleVimNavigation(e)) {
      e.preventDefault();
    }
  });

  var pendingTypeset = new Set();
  var typesetTimer = 0;
  var typesetBusy = false;
  var initialTypesetQueued = false;
  var TYPESET_IDLE_MS = 300;

  function clearRemovedMath(nodes) {
    if (!nodes.length || !window.__mpEngine) return;
    window.__mpEngine.typesetClear(nodes);
  }

  function leftoverMath(oldByHash) {
    var leftovers = [];
    oldByHash.forEach(function(pool) {
      for (var i = 0; i < pool.length; i++) leftovers.push(pool[i]);
    });
    return leftovers;
  }

  function removeIndexedMath(root, oldByHash) {
    root.querySelectorAll('.math[data-hash]').forEach(function(oldEl) {
      var pool = oldByHash.get(oldEl.dataset.hash);
      if (!pool) return;
      for (var i = 0; i < pool.length; i++) {
        if (pool[i] === oldEl) {
          pool.splice(i, 1);
          break;
        }
      }
      if (!pool.length) oldByHash.delete(oldEl.dataset.hash);
    });
  }

  function copyAttr(dst, src, name) {
    var value = src.getAttribute(name);
    if (value === null) dst.removeAttribute(name);
    else dst.setAttribute(name, value);
  }

  function syncReusedMathNode(oldEl, newEl) {
    oldEl.id = newEl.id;
    copyAttr(oldEl, newEl, 'data-src');
    copyAttr(oldEl, newEl, 'data-refkey');
    copyAttr(oldEl, newEl, 'data-tex');
    copyAttr(oldEl, newEl, 'title');
    copyAttr(oldEl, newEl, 'tabindex');
  }

  function pageBlocks(page) {
    return Array.prototype.filter.call(page.children, function(el) {
      return el.classList && el.classList.contains('blk');
    });
  }

  function syncReusedBlock(oldBlock, newBlock) {
    oldBlock.id = newBlock.id;
    oldBlock.className = newBlock.className;
    copyAttr(oldBlock, newBlock, 'data-blockhash');
    copyAttr(oldBlock, newBlock, 'data-src');
    syncBlockSourceAnchorsFromBlock(oldBlock, newBlock);
  }

  function syncBlockSourceAnchors(block, anchors) {
    if (!anchors) return;
    var els = block.querySelectorAll('[id][data-src]');
    for (var i = 0; i < els.length && i < anchors.length; i++) {
      if (anchors[i].id) els[i].id = anchors[i].id;
      if (anchors[i].src) els[i].setAttribute('data-src', anchors[i].src);
      else els[i].removeAttribute('data-src');
    }
  }

  function syncBlockSourceAnchorsFromBlock(oldBlock, newBlock) {
    var oldEls = oldBlock.querySelectorAll('[id][data-src]');
    var newEls = newBlock.querySelectorAll('[id][data-src]');
    for (var i = 0; i < oldEls.length && i < newEls.length; i++) {
      oldEls[i].id = newEls[i].id;
      copyAttr(oldEls[i], newEls[i], 'data-src');
    }
  }

  function syncPatchBlockMetadata(page, blocks) {
    if (!blocks || !blocks.length) return;
    var els = pageBlocks(page);
    for (var i = 0; i < els.length && i < blocks.length; i++) {
      els[i].id = blocks[i].id;
      els[i].setAttribute('data-blockhash', blocks[i].hash);
      if (blocks[i].src) els[i].setAttribute('data-src', blocks[i].src);
      else els[i].removeAttribute('data-src');
      syncBlockSourceAnchors(els[i], blocks[i].anchors);
    }
  }

  function indexMathByHash(root, oldByHash) {
    root.querySelectorAll('.math[data-hash]').forEach(function(oldEl) {
      var arr = oldByHash.get(oldEl.dataset.hash);
      if (!arr) { arr = []; oldByHash.set(oldEl.dataset.hash, arr); }
      arr.push(oldEl);
    });
  }

  function queueTypeset(nodes) {
    nodes.forEach(function(node) {
      pendingTypeset.add(node);
      node.classList.add('math-pending');
    });
    if (!pendingTypeset.size) {
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
      return;
    }
    if (typesetTimer) clearTimeout(typesetTimer);
    typesetTimer = setTimeout(flushTypeset, TYPESET_IDLE_MS);
  }

  function queueInitialTypeset() {
    if (initialTypesetQueued) return;
    var page = pageEl();
    if (!page) return;
    initialTypesetQueued = true;
    queueTypeset(Array.from(page.querySelectorAll('.math[data-hash]')));
  }

  async function flushTypeset() {
    typesetTimer = 0;
    if (typesetBusy) {
      typesetTimer = setTimeout(flushTypeset, 80);
      return;
    }
    if (!pendingTypeset.size) return;
    if (!window.__mpEngine || !window.__mpEngine.isReady()) {
      typesetTimer = setTimeout(flushTypeset, TYPESET_IDLE_MS);
      return;
    }

    var nodes = Array.from(pendingTypeset).filter(function(node) {
      return node && node.isConnected;
    });
    pendingTypeset.clear();
    if (!nodes.length) return;

    typesetBusy = true;
    setStatus('updating', '↻ typesetting ' + nodes.length + ' math');
    var tStart = performance.now();
    try {
      await window.__mpEngine.typeset(nodes);
      var ms = Math.round(performance.now() - tStart);
      nodes.forEach(function(node) { node.classList.remove('math-pending'); });
      restoreMathSearchHighlights();
      setStatus('live',
        '● live / idle typeset ' + ms + 'ms (' + nodes.length + ' math)' +
        memSuffix(window._lastRss));
    } catch (e) {
      console.error('mathpreview engine:', e);
      setStatus('dead', '○ engine error');
    } finally {
      typesetBusy = false;
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
      if (pendingTypeset.size && !typesetTimer) {
        typesetTimer = setTimeout(flushTypeset, TYPESET_IDLE_MS);
      }
    }
  }

  // Apply a server-computed block-level patch. Ops are positional ranges
  // against top-level .blk elements; after applying them we retag block ids
  // by position. This preserves shifted unchanged blocks without the id
  // collisions caused by insertion-before-existing-content edits.
  async function applyPatch(ops, blocksMeta) {
    var tStart = performance.now();
    setStatus('updating', '↻ patching');
    var page = document.getElementById('page');
    var tpl = document.createElement('template');
    var needTypeset = [];
    var reusedMath = 0, totalMath = 0;
    var replacedBlocks = 0, insertedBlocks = 0, removedBlocks = 0;
    var reusedBlocks = 0;
    var reusedSubBlockAttr = 'data-mp-reused-subblock';
    var hasRebuild = ops.some(function(op) { return op.type === 'rebuild'; });
    var detachPage = ops.length > 8 || hasRebuild;
    var pageParent = detachPage ? page.parentNode : null;
    var pageNextSibling = detachPage ? page.nextSibling : null;
    if (pageParent) pageParent.removeChild(page);
    var oldGuideLayer = page.querySelector('.page-guide-layer');
    if (oldGuideLayer) oldGuideLayer.remove();

    // PRE-SCAN: index math from every block that any op will drop, into a
    // single shared pool. This lets math that "moves" between distant
    // range ops (e.g. across two disjoint edits) transplant instead of
    // re-typesetting, and it gives a rebuild plan's html-slots access to
    // math from any non-reused block in the rebuilt slice.
    var initialBlocks = pageBlocks(page);
    var sharedOldByHash = new Map();
    for (var k = 0; k < ops.length; k++) {
      var pop = ops[k];
      if (pop.type === 'range') {
        var rStart = Math.max(0, Math.min(pop.index || 0, initialBlocks.length));
        var rRemove = Math.max(0, Math.min(pop.remove || 0, initialBlocks.length - rStart));
        for (var r = 0; r < rRemove; r++) {
          var rb = initialBlocks[rStart + r];
          if (rb) indexMathByHash(rb, sharedOldByHash);
        }
      } else if (pop.type === 'rebuild') {
        var rebuildReused = new Set();
        (pop.plan || []).forEach(function(slot) {
          if (typeof slot.src === 'number') rebuildReused.add(slot.src);
        });
        for (var s = 0; s < (pop.old_count || 0); s++) {
          var srcIdx = (pop.start || 0) + s;
          if (!rebuildReused.has(srcIdx)) {
            var sb = initialBlocks[srcIdx];
            if (sb) indexMathByHash(sb, sharedOldByHash);
          }
        }
      }
    }

    function transplantMath(scope) {
      scope.querySelectorAll('.math[data-hash]').forEach(function(newEl) {
        if (newEl.closest('span.proof-para[' + reusedSubBlockAttr + ']')) return;
        totalMath++;
        var pool = sharedOldByHash.get(newEl.dataset.hash);
        if (pool && pool.length > 0) {
          var oldEl = pool.shift();
          syncReusedMathNode(oldEl, newEl);
          newEl.replaceWith(oldEl);
          reusedMath++;
        } else {
          needTypeset.push(newEl);
        }
      });
    }

    function clearReusedSubBlockMarkers(scope) {
      scope.querySelectorAll('span.proof-para[' + reusedSubBlockAttr + ']').forEach(function(el) {
        el.removeAttribute(reusedSubBlockAttr);
      });
    }

    /// Sub-block reuse: for every (old block, new fragment block) pair,
    /// transplant `.proof-para` children whose `data-subhash` matches
    /// between sides. This keeps the existing DOM (and all the typeset
    /// math nodes inside) for paragraphs that didn't change, so a
    /// single-paragraph edit inside a long proof avoids DOM replacement
    /// and re-typesetting for unchanged proof paragraphs. The incoming
    /// HTML for the changed block is still parsed as one fragment; this
    /// optimization reduces the later swap/typeset work. Called BEFORE
    /// `transplantMath(frag)` so moved old paragraphs (which already
    /// contain typeset math) can be removed from the math pool and
    /// skipped by the math transplant pass.
    function transplantSubBlocks(oldBlock, newBlock) {
      if (!oldBlock || !newBlock) return;
      var oldParas = oldBlock.querySelectorAll('span.proof-para[data-subhash]');
      if (!oldParas.length) return;
      var newParas = newBlock.querySelectorAll('span.proof-para[data-subhash]');
      if (!newParas.length) return;
      var byHash = new Map();
      for (var i = 0; i < oldParas.length; i++) {
        var h = oldParas[i].getAttribute('data-subhash');
        if (!byHash.has(h)) byHash.set(h, []);
        byHash.get(h).push(oldParas[i]);
      }
      var reused = 0;
      for (var j = 0; j < newParas.length; j++) {
        var newPara = newParas[j];
        var hash = newPara.getAttribute('data-subhash');
        var pool = byHash.get(hash);
        if (pool && pool.length) {
          var oldPara = pool.shift();
          removeIndexedMath(oldPara, sharedOldByHash);
          oldPara.setAttribute(reusedSubBlockAttr, '1');
          newPara.replaceWith(oldPara);
          reused++;
        }
      }
      return reused;
    }

    try {
      for (var i = 0; i < ops.length; i++) {
        var op = ops[i];
        if (op.type === 'range') {
          var blocks = pageBlocks(page);
          var start = Math.max(0, Math.min(op.index || 0, blocks.length));
          var removeCount = Math.max(0, Math.min(op.remove || 0, blocks.length - start));
          var anchor = blocks[start + removeCount] || null;

          tpl.innerHTML = op.html || '';
          var frag = tpl.content;
          var inserted = frag.querySelectorAll('.blk').length;
          // Sub-block reuse: for each positionally-paired (old, new) block,
          // transplant `proof-para` children with matching data-subhash
          // BEFORE the math transplant so reused paragraphs come along
          // with their already-typeset math intact.
          var newFragBlocks = frag.querySelectorAll('article.blk');
          var pairCount = Math.min(removeCount, newFragBlocks.length);
          for (var pp = 0; pp < pairCount; pp++) {
            transplantSubBlocks(blocks[start + pp], newFragBlocks[pp]);
          }
          transplantMath(frag);
          clearReusedSubBlockMarkers(frag);

          for (var d = 0; d < removeCount; d++) {
            if (blocks[start + d] && blocks[start + d].parentNode === page) {
              blocks[start + d].remove();
              removedBlocks++;
            }
          }
          if (inserted) {
            page.insertBefore(frag, anchor);
            insertedBlocks += inserted;
          }
          replacedBlocks += Math.min(removeCount, inserted);
        } else if (op.type === 'rebuild') {
          var rbBlocks = pageBlocks(page);
          var rbStart = Math.max(0, Math.min(op.start || 0, rbBlocks.length));
          var rbCount = Math.max(0, Math.min(op.old_count || 0, rbBlocks.length - rbStart));
          var rbAnchor = rbBlocks[rbStart + rbCount] || null;

          // Index the old slice by absolute src index so plan Reuse slots
          // can pull the exact DOM subtree (preserving typeset MathJax).
          var rbOldByIdx = new Map();
          for (var s2 = 0; s2 < rbCount; s2++) {
            var rbOld = rbBlocks[rbStart + s2];
            if (rbOld) {
              rbOldByIdx.set(rbStart + s2, rbOld);
              rbOld.remove();
              removedBlocks++;
            }
          }

          (op.plan || []).forEach(function(slot) {
            if (typeof slot.src === 'number') {
              var b = rbOldByIdx.get(slot.src);
              if (b) {
                page.insertBefore(b, rbAnchor);
                reusedBlocks++;
                // Reused old block survived the detach above; count it back
                // out of removedBlocks so the status pill stays honest.
                removedBlocks--;
              }
            } else if (typeof slot.html === 'string') {
              tpl.innerHTML = slot.html;
              var children = Array.from(tpl.content.children);
              children.forEach(function(c) {
                transplantMath(c);
                page.insertBefore(c, rbAnchor);
                insertedBlocks++;
              });
            }
          });
        }
      }
      clearRemovedMath(leftoverMath(sharedOldByHash));
      syncPatchBlockMetadata(page, blocksMeta);
    } finally {
      if (pageParent) {
        if (pageNextSibling) pageParent.insertBefore(page, pageNextSibling);
        else pageParent.appendChild(page);
      }
    }

    queueTypeset(needTypeset);

    var total = Math.round(performance.now() - tStart);
    setStatus('live',
      '● ' + total + 'ms · ' + replacedBlocks + 'r' +
      (reusedBlocks ? '/=' + reusedBlocks : '') +
      (insertedBlocks ? '/+' + insertedBlocks : '') +
      (removedBlocks > 0 ? '/-' + removedBlocks : '') +
      ' / typeset ' + (needTypeset.length ? 'queued' : '0') +
      ' (' + needTypeset.length + ' math' +
      (reusedMath ? ', reused ' + reusedMath + '/' + totalMath : '') + ')' +
      memSuffix(window._lastRss));
  }

  // Shared memory tag. Server pushes its current resident size on every
  // event; we cache it so subsequent renders can re-print without waiting
  // for a fresh roundtrip.
  function memSuffix(mib) {
    if (typeof mib !== 'number' || isNaN(mib)) return '';
    return ' · ' + mib.toFixed(1) + ' MiB';
  }

  // Live-reload WebSocket. Reconnects with backoff if the server restarts.
  var WS_PROTOCOL_VERSION = '31';
  var status = document.getElementById('ws-status');
  function setStatus(cls, text) {
    if (!status) return;
    status.className = 'status ' + cls;
    status.textContent = text;
  }
  function connect() {
    if (!window.WebSocket) return;
    var url = (location.protocol === 'https:' ? 'wss://' : 'ws://') +
      location.host + '/ws?v=' + encodeURIComponent(WS_PROTOCOL_VERSION);
    var ws;
    try { ws = new WebSocket(url); } catch (e) { return; }
    ws.onopen  = function() { setStatus('live', '● live'); };
    ws.onclose = function() {
      if (manualStopRequested) {
        setStatus('dead', '○ stopped');
        return;
      }
      setStatus('dead', '○ disconnected');
      setTimeout(connect, 1000);
    };
    ws.onerror = function() { setStatus('dead', '○ error'); };
    ws.onmessage = async function(ev) {
      try {
        var msg = JSON.parse(ev.data);
        if (typeof msg.rss_mib === 'number') window._lastRss = msg.rss_mib;
        if (msg.event === 'patch') {
          await applyPatch(msg.ops, msg.blocks);
          applyMode(currentProofMode);
          setRefkeysVisible(refkeysVisible, false);
          restoreSourceHighlight();
          restoreMathSearchHighlights();
          scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, true);
        } else if (msg.event === 'body-updated') {
          var tStart = performance.now();
          setStatus('updating', '↻ updating');
          var page = document.getElementById('page');

          // Detach #page from the live document for the duration of the
          // mutations. Off-document mutations don't trigger layout/style
          // invalidation, so 300+ node transplants run an order of
          // magnitude faster than they would in-document.
          var pageParent = page.parentNode;
          var pageNextSibling = page.nextSibling;
          pageParent.removeChild(page);

          // Index existing blocks by rendered content hash. Whole-block reuse
          // is much cheaper than diffing every MathJax node when a full update
          // is unavoidable.
          var oldBlocksByHash = new Map();
          pageBlocks(page).forEach(function(block) {
            var hash = block.getAttribute('data-blockhash');
            if (!hash) return;
            var arr = oldBlocksByHash.get(hash);
            if (!arr) { arr = []; oldBlocksByHash.set(hash, arr); }
            arr.push(block);
          });
          var tIndex = performance.now();

          // Parse new HTML into a detached <template> (faster than <div>).
          var tpl = document.createElement('template');
          tpl.innerHTML = msg.html;
          var buf = tpl.content;
          var tParse = performance.now();

          var reusedBlocks = 0;
          buf.querySelectorAll('.blk[data-blockhash]').forEach(function(newBlock) {
            var pool = oldBlocksByHash.get(newBlock.getAttribute('data-blockhash'));
            if (pool && pool.length > 0) {
              var oldBlock = pool.shift();
              syncReusedBlock(oldBlock, newBlock);
              oldBlock.setAttribute('data-mp-reused-block', '1');
              newBlock.replaceWith(oldBlock);
              reusedBlocks++;
            }
          });

          // For remaining changed blocks, transplant matching old math nodes.
          var needTypeset = [];
          var oldByHash = new Map();
          oldBlocksByHash.forEach(function(pool) {
            for (var i = 0; i < pool.length; i++) indexMathByHash(pool[i], oldByHash);
          });
          var newMath = buf.querySelectorAll('.math[data-hash]');
          newMath.forEach(function(newEl) {
            var block = newEl.closest('.blk');
            if (block && block.getAttribute('data-mp-reused-block') === '1') return;
            var pool = oldByHash.get(newEl.dataset.hash);
            if (pool && pool.length > 0) {
              var oldEl = pool.shift();
              syncReusedMathNode(oldEl, newEl);
              newEl.replaceWith(oldEl);
            } else {
              needTypeset.push(newEl);
            }
          });
          var tDiff = performance.now();
          clearRemovedMath(leftoverMath(oldByHash));

          page.replaceChildren(buf);

          // Reattach #page in its original position. One layout pass for
          // the whole update, not 300+.
          if (pageNextSibling) pageParent.insertBefore(page, pageNextSibling);
          else pageParent.appendChild(page);
          page.querySelectorAll('[data-mp-reused-block]').forEach(function(block) {
            block.removeAttribute('data-mp-reused-block');
          });
          var tSwap = performance.now();

          queueTypeset(needTypeset);

          var tDone = performance.now();
          var total = Math.round(tDone - tStart);
          var reused = newMath.length - needTypeset.length;
          setStatus('live',
            '● ' + total + 'ms · idx ' + Math.round(tIndex - tStart) +
            ' / parse ' + Math.round(tParse - tIndex) +
            ' / diff ' + Math.round(tDiff - tParse) +
            ' / swap ' + Math.round(tSwap - tDiff) +
            ' / typeset ' + (needTypeset.length ? 'queued' : '0') +
            ' (reused ' + reused + '/' + newMath.length +
            (reusedBlocks ? ', blocks ' + reusedBlocks : '') + ')' +
            memSuffix(window._lastRss));
          applyMode(currentProofMode);
          setRefkeysVisible(refkeysVisible, false);
          restoreSourceHighlight();
          restoreMathSearchHighlights();
          scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, true);
        } else if (msg.event === 'source-cursor') {
          if (msg.element_id) {
            revealSourceElement(msg.element_id, true);
          }
        } else if (msg.event === 'full-reload') {
          location.reload();
        } else if (msg.event === 'error') {
          setStatus('dead', '○ ' + (msg.message || 'render error'));
        }
      } catch (e) { console.error('mathpreview WS:', e); }
    };
  }
  try {
    setPageMode(localStorage.getItem('mathpreview.pageMode') || 'a4');
    setSideTab(localStorage.getItem('mathpreview.sideTab') || 'index');
    var storedSideOpen = localStorage.getItem('mathpreview.sideOpen');
    setSideOpen(storedSideOpen === null ? window.innerWidth > 1340 : storedSideOpen === '1', false);
    setRefkeysVisible(localStorage.getItem('mathpreview.refkeys') === '1', false);
    setMarginMode(localStorage.getItem('mathpreview.marginMode') === '1', false);
    setTopbarHidden(localStorage.getItem('mathpreview.topbarHidden') === '1', false);
  } catch (e) {
    setPageMode('a4');
    setSideTab('index');
    setSideOpen(window.innerWidth > 1340, false);
    setRefkeysVisible(false, false);
    setMarginMode(false, false);
    setTopbarHidden(false, false);
  }
  scheduleNavigationRefresh();
  refreshAfterInitialEngine(40);
  window.addEventListener('load', scheduleNavigationRefresh);
  window.addEventListener('resize', function() {
    updatePageScale();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
  });
  window.addEventListener('scroll', scheduleActivePageUpdate, { passive: true });
  connect();
})();
