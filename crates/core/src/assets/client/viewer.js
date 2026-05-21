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

  /// The cards stack lives inside #margin-cards so the toolbar (typed-
  /// refkey input + feedback span) at the top of #margin survives a
  /// "close all" wipe and isn't accidentally rebuilt on every card pin.
  function marginCardsEl() { return document.getElementById('margin-cards'); }

  /// Toggle the `margin-has-cards` body class. Layout rules that shift
  /// the page content (`#page-shell`) live behind that class so the
  /// reading area stays centered when margin mode is on but no card is
  /// pinned. Called after every pinnedRefs mutation.
  function updateMarginCardsClass() {
    document.body.classList.toggle('margin-has-cards', pinnedRefs.size > 0);
  }

  function closeAllMarginCards() {
    var cards = marginCardsEl();
    if (cards) cards.innerHTML = '';
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
    card.draggable = true;

    var head = document.createElement('div');
    head.className = 'margin-card-header';
    // Drag grip on the left side of the header. Visually signals that
    // the card is draggable; the entire card is the actual drag source
    // (HTML5 dnd doesn't let us scope draggable to a sub-element, but
    // text selection inside the body still takes precedence over drag
    // start because the browser checks mousedown intent).
    var grip = document.createElement('span');
    grip.className = 'margin-card-grip';
    grip.setAttribute('aria-hidden', 'true');
    grip.title = 'drag to reorder';
    grip.textContent = '⋮⋮';
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
    head.appendChild(grip);
    head.appendChild(title);
    head.appendChild(close);

    var body = document.createElement('div');
    body.className = 'margin-card-body';
    body.appendChild(clone);

    card.appendChild(head);
    card.appendChild(body);
    return card;
  }

  /// Drag-to-reorder for margin cards. Delegated to #margin-cards so the
  /// handlers attach once even though cards come and go. The dragged card
  /// gets `.dragging`; the card under the cursor gets `.drop-above` or
  /// `.drop-below` based on which half of its bounding box the cursor is in.
  /// pinnedRefs is a key → element Map; its order doesn't drive layout
  /// (the DOM does), so we just move the node and let the Map keep its
  /// entry intact.
  var dragSourceCard = null;
  function clearDropIndicators() {
    var cards = document.querySelectorAll('#margin-cards .margin-card');
    cards.forEach(function(c) {
      c.classList.remove('drop-above');
      c.classList.remove('drop-below');
    });
  }
  function dropPositionFor(card, clientY) {
    var rect = card.getBoundingClientRect();
    return clientY < rect.top + rect.height / 2 ? 'above' : 'below';
  }
  function initMarginDnd() {
    var cards = marginCardsEl();
    if (!cards || cards.dataset.dndInit === '1') return;
    cards.dataset.dndInit = '1';
    cards.addEventListener('dragstart', function(e) {
      var card = e.target && e.target.closest && e.target.closest('.margin-card');
      if (!card || card.parentNode !== cards) return;
      dragSourceCard = card;
      card.classList.add('dragging');
      // Some browsers need at least one setData call to start a drag.
      try { e.dataTransfer.setData('text/plain', card.dataset.pinKey || ''); } catch (err) {}
      e.dataTransfer.effectAllowed = 'move';
    });
    cards.addEventListener('dragover', function(e) {
      if (!dragSourceCard) return;
      var card = e.target && e.target.closest && e.target.closest('.margin-card');
      e.preventDefault(); // required to enable drop
      e.dataTransfer.dropEffect = 'move';
      clearDropIndicators();
      if (!card || card === dragSourceCard) return;
      var pos = dropPositionFor(card, e.clientY);
      card.classList.add(pos === 'above' ? 'drop-above' : 'drop-below');
    });
    cards.addEventListener('dragleave', function(e) {
      // Only clear when leaving the cards container entirely (not when
      // moving between child cards), otherwise the indicator flickers.
      if (e.target === cards) clearDropIndicators();
    });
    cards.addEventListener('drop', function(e) {
      if (!dragSourceCard) return;
      e.preventDefault();
      var card = e.target && e.target.closest && e.target.closest('.margin-card');
      if (card && card !== dragSourceCard) {
        var pos = dropPositionFor(card, e.clientY);
        if (pos === 'above') cards.insertBefore(dragSourceCard, card);
        else cards.insertBefore(dragSourceCard, card.nextSibling);
      } else if (!card) {
        // Dropped on the container, not on a card → append to end.
        cards.appendChild(dragSourceCard);
      }
      clearDropIndicators();
      dragSourceCard.classList.remove('dragging');
      dragSourceCard = null;
    });
    cards.addEventListener('dragend', function() {
      clearDropIndicators();
      if (dragSourceCard) {
        dragSourceCard.classList.remove('dragging');
        dragSourceCard = null;
      }
    });
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
    var cards = marginCardsEl();
    if (!cards) return;
    cards.appendChild(card);
    pinnedRefs.set(key, card);
    updateMarginCardsClass();
  }

  /// Synthesise a `<a class="ref" data-target="…">` for an arbitrary refkey
  /// and pin its target as a margin card, so the user can pull up a
  /// statement by typing its `\label{…}` instead of scrolling to a `\ref`.
  /// Lookup mirrors the click path: try the element whose `data-refkey`
  /// matches first, then the sanitized id (so `prop:foo` finds `id="prop-foo"`),
  /// then a `<dt data-key>` for `\bibitem`-style cite keys.
  function pinByRefkey(rawKey) {
    var key = (rawKey || '').trim();
    if (!key) return { ok: false, reason: 'empty' };
    var sanitized = key.replace(/[^a-zA-Z0-9_-]/g, '-');
    var target = document.querySelector('#page [data-refkey="' + cssEscape(key) + '"]') ||
                 document.getElementById(sanitized) ||
                 document.querySelector('#page dt[data-key="' + cssEscape(key) + '"]');
    if (!target) return { ok: false, reason: 'not-found' };
    if (pinnedRefs.has(key)) {
      var existing = pinnedRefs.get(key);
      if (existing && existing.scrollIntoView) {
        existing.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
      }
      return { ok: true, reason: 'already-pinned' };
    }
    var isBib = target.tagName === 'DT';
    var synthetic = document.createElement('a');
    synthetic.className = isBib ? 'cite' : 'ref';
    synthetic.setAttribute('href', '#' + sanitized);
    synthetic.setAttribute(isBib ? 'data-key' : 'data-target', key);
    if (!marginMode) setMarginMode(true, true);
    var clone = clonePreviewContent(synthetic, target);
    var card = buildMarginCard(synthetic, clone);
    var cards = marginCardsEl();
    if (!cards) return { ok: false, reason: 'no-margin' };
    cards.appendChild(card);
    pinnedRefs.set(key, card);
    updateMarginCardsClass();
    return { ok: true, reason: 'pinned' };
  }

  /// CSS.escape() is widely supported but ancient browsers / headless
  /// environments lack it. Fall back to escaping just the characters that
  /// can appear in a LaTeX label (`:`, `.`, `/`) so the attribute selector
  /// parses.
  function cssEscape(value) {
    if (typeof CSS !== 'undefined' && CSS.escape) return CSS.escape(value);
    return String(value).replace(/[^a-zA-Z0-9_-]/g, function(ch) {
      return '\\' + ch;
    });
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

