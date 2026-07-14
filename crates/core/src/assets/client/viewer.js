    var current = { x: window.scrollX || 0, y: window.scrollY || 0 };
    viewerJumpStack.push(current);
    if (viewerJumpStack.length > 100) viewerJumpStack.shift();
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
    clearSearchSuggestions();
  }

  function closeSearchPanel() {
    var panel = searchPanelEl();
    var input = searchInputEl();
    if (panel) panel.hidden = true;
    if (input && document.activeElement === input) input.blur();
    clearSearchSuggestions();
    clearSearchSession();
  }

  // Wire the `/` input's live suggestion refresh (called once at startup).
  function initSearchPanel() {
    var input = searchInputEl();
    if (!input || input.dataset.searchInit === '1') return;
    input.dataset.searchInit = '1';
    input.addEventListener('input', refreshSearchSuggestions);
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

  /// Parse an explicit math-search sigil out of the raw query string.
  ///   `m:foo`   → force math mode, search "foo"
  ///   `$foo$`   → force math mode, search "foo" (LaTeX-style wrap)
  ///   `$foo`    → ditto (trailing `$` optional)
  /// Anything else passes through as a plain search. The sigil makes the
  /// search routine skip `window.find`, which otherwise gets stuck cycling
  /// through body-text matches and never reaches the SVG math glyphs —
  /// the original reason `/n` would "miss" italic-n inside an equation.
  function parseSearchQuery(query) {
    var trimmed = (query || '').trim();
    if (!trimmed) return { core: '', forceMath: false };
    var prefix = /^m:(.+)$/.exec(trimmed);
    if (prefix) return { core: prefix[1].trim(), forceMath: true };
    if (trimmed.charAt(0) === '$') {
      return { core: stripMathDelimiters(trimmed), forceMath: true };
    }
    return { core: trimmed, forceMath: false };
  }

  /// Map a base codepoint to the set of "stylistic variant" codepoints
  /// MathJax may emit for it. Math italic letters live in the
  /// Mathematical Alphanumeric Symbols block (U+1D400..U+1D7FF), not at
  /// the base ASCII / Greek codepoint, so a literal `n` in the search box
  /// would never match an italic-n SVG glyph (`data-c="1D45B"`) without
  /// this expansion. Returns codepoints, not characters — caller decides.
  ///
  /// Coverage: A-Z, a-z, 0-9 (13 / 13 / 5 stylistic ranges respectively),
  /// plus uppercase and lowercase Greek (5 ranges each). The "reserved"
  /// holes inside these ranges (e.g. italic-h at U+1D455 is reserved
  /// because the character is actually U+210E) are filled by
  /// [`MATH_BMP_HOLES`] for the affected Latin letters.
  function expandMathGlyphCodepoints(cp) {
    var out = [cp];
    var i;
    if (cp >= 0x41 && cp <= 0x5A) {
      i = cp - 0x41;
      [0x1D400, 0x1D434, 0x1D468, 0x1D49C, 0x1D4D0, 0x1D504, 0x1D538,
       0x1D56C, 0x1D5A0, 0x1D5D4, 0x1D608, 0x1D63C, 0x1D670].forEach(function(b) {
        out.push(b + i);
      });
    } else if (cp >= 0x61 && cp <= 0x7A) {
      i = cp - 0x61;
      [0x1D41A, 0x1D44E, 0x1D482, 0x1D4B6, 0x1D4EA, 0x1D51E, 0x1D552,
       0x1D586, 0x1D5BA, 0x1D5EE, 0x1D622, 0x1D656, 0x1D68A].forEach(function(b) {
        out.push(b + i);
      });
    } else if (cp >= 0x30 && cp <= 0x39) {
      i = cp - 0x30;
      [0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6].forEach(function(b) {
        out.push(b + i);
      });
    } else if (cp >= 0x0391 && cp <= 0x03A9) {
      i = cp - 0x0391;
      [0x1D6A8, 0x1D6E2, 0x1D71C, 0x1D756, 0x1D790].forEach(function(b) {
        out.push(b + i);
      });
    } else if (cp >= 0x03B1 && cp <= 0x03C9) {
      i = cp - 0x03B1;
      [0x1D6C2, 0x1D6FC, 0x1D736, 0x1D770, 0x1D7AA].forEach(function(b) {
        out.push(b + i);
      });
    }
    return out;
  }

  /// Letters whose math-stylistic variant lives at a BMP codepoint
  /// outside the Mathematical Alphanumeric Symbols block. Without this
  /// the script / fraktur / double-struck / italic-h cases would silently
  /// miss when the search query is the underlying ASCII letter.
  var MATH_BMP_HOLES = {
    'h': [0x210E],
    'B': [0x212C], 'C': [0x2102, 0x212D], 'E': [0x2130], 'F': [0x2131],
    'H': [0x210B, 0x210C, 0x210D], 'I': [0x2110, 0x2111],
    'L': [0x2112], 'M': [0x2133], 'N': [0x2115], 'P': [0x2119],
    'Q': [0x211A], 'R': [0x211B, 0x211C, 0x211D],
    'Z': [0x2124, 0x2128],
    'e': [0x212F], 'g': [0x210A], 'o': [0x2134]
  };

  function mathSearchSpec(query) {
    var core = stripMathDelimiters(query);
    var texNeedles = [];
    var glyphCps = new Set();
    if (core) texNeedles.push(core);
    if (query && query !== core) texNeedles.push(query);
    function addBaseCp(cp) {
      expandMathGlyphCodepoints(cp).forEach(function(c) { glyphCps.add(c); });
    }
    var command = /^\\([A-Za-z]+)$/.exec(core);
    if (command) {
      (TEX_SYMBOL_CODEPOINTS[command[1]] || []).forEach(addBaseCp);
      texNeedles.push('\\' + command[1]);
    } else if (core && Array.from(core).length === 1) {
      addBaseCp(core.codePointAt(0));
      var holes = MATH_BMP_HOLES[core];
      if (holes) holes.forEach(function(c) { glyphCps.add(c); });
    } else if (/^[A-Za-z]+$/.test(core)) {
      var cps = TEX_SYMBOL_CODEPOINTS[core] || [];
      if (cps.length) {
        cps.forEach(addBaseCp);
        texNeedles.push('\\' + core);
      }
    }
    var glyphChars = [];
    glyphCps.forEach(function(cp) { glyphChars.push(String.fromCodePoint(cp)); });
    return {
      core: core,
      texNeedles: Array.from(new Set(texNeedles.filter(Boolean))),
      glyphChars: glyphChars
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

  // End the plain-text session (state + paint + counter). Called when a math
  // search takes over — leaving it alive would keep stale text highlights and
  // let restoreTextSearchHighlights clobber the math counter on re-render.
  function clearTextSession() {
    textSearchQuery = '';
    textSearchResults = [];
    textSearchIndex = -1;
    clearTextHighlights();
  }

  function clearSearchSession() {
    mathSearchQuery = '';
    mathSearchResults = [];
    mathSearchIndex = -1;
    clearMathSearchHighlights();
    clearTextSession();
    updateSearchCount(-1, 0);
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
      updateSearchCount(-1, 0);
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
    updateSearchCount(mathSearchIndex, mathSearchResults.length);
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

  // --- Plain-text search (vim `/`): own match list so it cycles at the ends
  // and shows current/total, with all matches highlighted via the CSS Custom
  // Highlight API (no DOM mutation) and the active one distinct. ---

  function supportsHighlightApi() {
    return typeof Highlight !== 'undefined' && typeof CSS !== 'undefined' && CSS.highlights;
  }

  // Flatten the page's visible text into one string plus a node index, so a
  // query can match ACROSS the per-word `src-word` spans (and the spaces
  // between them). Skips math source, chrome, and hidden/overlay text.
  function collectSearchText() {
    var page = pageEl();
    if (!page) return { text: '', nodes: [] };
    var nodes = [];
    var parts = [];
    var offset = 0;
    var walker = document.createTreeWalker(page, NodeFilter.SHOW_TEXT, {
      acceptNode: function(node) {
        if (!node.nodeValue) return NodeFilter.FILTER_REJECT;
        var p = node.parentElement;
        if (p && p.closest &&
            p.closest('.math-source, .search-panel, .side-panel, .topbar, .lineno-layer, ' +
                      '.refkey-chip, .eq-refkey-list, .footnote-pop, ' +
                      '.sidenote, .proof.folded .proof-body, [aria-hidden="true"]')) {
          // Skip math LaTeX source, chrome, and content that's hidden until
          // hover/margin-mode/unfold (a match there would count but scroll to
          // nowhere and corrupt the current/total ordering). Scoped to the
          // folded proof's BODY so its visible "Proof." head stays searchable.
          return NodeFilter.FILTER_REJECT;
        }
        return NodeFilter.FILTER_ACCEPT;
      }
    });
    var node;
    while ((node = walker.nextNode())) {
      nodes.push({ node: node, start: offset, len: node.nodeValue.length });
      parts.push(node.nodeValue);
      offset += node.nodeValue.length;
    }
    return { text: parts.join(''), nodes: nodes };
  }

  // Map a flat offset to a { node, offset, idx } DOM position. `atEnd` picks the
  // side when the offset lands on a node boundary: for a match END keep it at
  // the end of the previous node (`<=`); for a match START move it to the start
  // of the next node (`<`), so the range can't anchor behind skipped/hidden DOM
  // (inline math, an aria-hidden break) between two flat-adjacent text nodes.
  // `from` is a scan-start hint: match offsets ascend monotonically, so callers
  // resume from the previous hit's node instead of rescanning from 0 (keeps
  // buildTextMatches linear on match-heavy docs).
  function locateFlat(nodes, pos, atEnd, from) {
    for (var i = from || 0; i < nodes.length; i++) {
      var n = nodes[i];
      var end = n.start + n.len;
      if (atEnd ? pos <= end : pos < end) {
        return { node: n.node, offset: pos - n.start, idx: i };
      }
    }
    var last = nodes[nodes.length - 1];
    return last ? { node: last.node, offset: last.len, idx: nodes.length - 1 } : null;
  }

  function buildTextMatches(query) {
    var q = query || '';
    if (!q) return [];
    var flat = collectSearchText();
    // Case-insensitive regex over the ORIGINAL text (not a toLowerCase copy,
    // whose length can differ — e.g. İ — and would shift every later offset).
    var re;
    try {
      re = new RegExp(q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'gi');
    } catch (e) {
      return [];
    }
    var results = [];
    var cursor = 0;
    var m;
    // Cap the match list: a 1-2 char pattern on a 60-page document would
    // otherwise build tens of thousands of Ranges (and Highlight entries)
    // per rebuild. 5000 highlights is already far beyond what's scannable.
    var MAX_TEXT_MATCHES = 5000;
    while (results.length < MAX_TEXT_MATCHES && (m = re.exec(flat.text))) {
      var a = locateFlat(flat.nodes, m.index, false, cursor);
      var b = a && locateFlat(flat.nodes, m.index + m[0].length, true, a.idx);
      if (a && b) {
        cursor = a.idx;
        try {
          var r = document.createRange();
          r.setStart(a.node, a.offset);
          r.setEnd(b.node, b.offset);
          results.push(r);
        } catch (e) {}
      }
      // Non-overlapping, like a plain search (lastIndex already sits past the
      // match; guard against a pathological zero-length match).
      if (m.index === re.lastIndex) re.lastIndex++;
    }
    return results;
  }

  // Bounded Damerau (optimal string alignment) distance: true when the edit
  // distance(a, b) <= max, counting an adjacent transposition as one edit
  // (so "gaint" ~ "giant"). Transpositions are among the most common typos.
  // Early-exits when a whole DP row exceeds the budget, so it's cheap for the
  // common no-match case (short strings, small max).
  function withinEditDistance(a, b, max) {
    var la = a.length, lb = b.length;
    if (Math.abs(la - lb) > max) return false;
    if (la === 0) return lb <= max;
    if (lb === 0) return la <= max;
    var prevPrev = null;
    var prev = new Array(lb + 1);
    for (var j = 0; j <= lb; j++) prev[j] = j;
    for (var i = 1; i <= la; i++) {
      var cur = new Array(lb + 1);
      cur[0] = i;
      var rowMin = i;
      var ca = a.charCodeAt(i - 1);
      for (var k = 1; k <= lb; k++) {
        var cb = b.charCodeAt(k - 1);
        var cost = ca === cb ? 0 : 1;
        var v = Math.min(prev[k] + 1, cur[k - 1] + 1, prev[k - 1] + cost);
        if (i > 1 && k > 1 && prevPrev &&
            ca === b.charCodeAt(k - 2) && a.charCodeAt(i - 2) === cb) {
          v = Math.min(v, prevPrev[k - 2] + 1); // adjacent transposition
        }
        cur[k] = v;
        if (v < rowMin) rowMin = v;
      }
      if (rowMin > max) return false;
      prevPrev = prev;
      prev = cur;
    }
    return prev[lb] <= max;
  }

  // A query word fuzzy-matches a document word if it's a substring (covers
  // exact + prefix + contained) or within an adaptive edit distance (typo
  // tolerance). Short queries (<3 chars) only match as substrings — fuzzing
  // them would match almost everything.
  function tokenFuzzyMatch(q, d) {
    if (d.indexOf(q) !== -1) return true;
    if (q.length < 3) return false;
    return withinEditDistance(q, d, q.length <= 5 ? 1 : 2);
  }

  // Word tokens (letter/number runs) with their offsets in the flat text.
  function tokenizeWords(text) {
    var toks = [];
    var re = /[\p{L}\p{N}]+/gu;
    var m;
    while ((m = re.exec(text))) {
      toks.push({ start: m.index, end: m.index + m[0].length, lower: m[0].toLowerCase() });
    }
    return toks;
  }

  // Fuzzy counterpart of buildTextMatches: match each query word against the
  // document's words with typo tolerance; a multi-word query matches a run of
  // consecutive words. Returns the same kind of DOM Range array, so all the
  // downstream highlight / cycle / counter machinery is unchanged.
  function buildFuzzyMatches(query) {
    var qTokens = (query || '').toLowerCase().split(/\s+/).filter(Boolean);
    if (!qTokens.length) return [];
    var flat = collectSearchText();
    var docTokens = tokenizeWords(flat.text);
    var n = qTokens.length;
    var results = [];
    var cursor = 0;
    var MAX_FUZZY_MATCHES = 3000;
    for (var i = 0; i + n <= docTokens.length && results.length < MAX_FUZZY_MATCHES; i++) {
      var ok = true;
      for (var t = 0; t < n; t++) {
        if (!tokenFuzzyMatch(qTokens[t], docTokens[i + t].lower)) { ok = false; break; }
      }
      if (!ok) continue;
      var a = locateFlat(flat.nodes, docTokens[i].start, false, cursor);
      var b = a && locateFlat(flat.nodes, docTokens[i + n - 1].end, true, a.idx);
      if (a && b) {
        cursor = a.idx;
        try {
          var r = document.createRange();
          r.setStart(a.node, a.offset);
          r.setEnd(b.node, b.offset);
          results.push(r);
        } catch (e) {}
      }
      if (n > 1) i += n - 1; // non-overlapping multi-word spans
    }
    return results;
  }

  // The `/` panel is always fuzzy (typo-tolerant). Exact literal search is the
  // browser's own Cmd/Ctrl+F. `buildTextMatches` is still used for the
  // editor-driven (nvim `/`) highlight, which mirrors vim's literal pattern.
  function buildSearchMatches(query) {
    return buildFuzzyMatches(query);
  }

  // --- Word suggestions: as you type in the `/` box, offer document words that
  // (fuzzily) match the word you're on, so you can complete to the real term. ---

  var SEARCH_MAX_SUGGESTIONS = 8;
  var searchVocab = null;
  var searchVocabSig = '';
  // Unique document words (≥3 chars) with a first-seen casing and a frequency,
  // cached until the flattened text changes.
  function getSearchVocab() {
    var flat = collectSearchText();
    var sig = flat.text.length + ':' + flat.text.slice(0, 96);
    if (searchVocab && searchVocabSig === sig) return searchVocab;
    var byLower = new Map();
    var re = /[\p{L}\p{N}]+/gu;
    var m;
    while ((m = re.exec(flat.text))) {
      if (m[0].length < 3) continue;
      var lw = m[0].toLowerCase();
      var e = byLower.get(lw);
      if (e) e.freq++;
      else byLower.set(lw, { w: m[0], lower: lw, freq: 1 });
    }
    searchVocab = Array.from(byLower.values());
    searchVocabSig = sig;
    return searchVocab;
  }

  // Rank vocab words as completions of the last query word: prefix > substring
  // > typo (edit distance), with frequency and brevity as tiebreakers.
  function suggestWords(query, limit) {
    var qTokens = (query || '').toLowerCase().split(/\s+/).filter(Boolean);
    if (!qTokens.length) return [];
    var q = qTokens[qTokens.length - 1];
    if (q.length < 2) return [];
    var vocab = getSearchVocab();
    var scored = [];
    for (var i = 0; i < vocab.length; i++) {
      var w = vocab[i];
      if (w.lower === q) continue; // already typed exactly — nothing to complete
      var base;
      if (w.lower.indexOf(q) === 0) base = 3;          // prefix
      else if (w.lower.indexOf(q) !== -1) base = 2;    // substring
      else if (q.length >= 3 && withinEditDistance(q, w.lower, q.length <= 5 ? 1 : 2)) base = 1; // typo
      else continue;
      scored.push({ w: w.w, score: base * 1000 + Math.min(w.freq, 60) - w.lower.length * 0.5 });
    }
    scored.sort(function(a, b) { return b.score - a.score; });
    return scored.slice(0, limit).map(function(s) { return s.w; });
  }

  function searchSuggestEl() { return document.getElementById('search-suggest'); }

  function clearSearchSuggestions() {
    searchSuggestions = [];
    searchSuggestIndex = -1;
    var el = searchSuggestEl();
    if (el) { el.hidden = true; el.replaceChildren(); }
  }

  function renderSearchSuggestions() {
    var el = searchSuggestEl();
    if (!el) return;
    if (!searchSuggestions.length) { clearSearchSuggestions(); return; }
    el.hidden = false;
    el.replaceChildren();
    searchSuggestions.forEach(function(word, i) {
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'search-suggestion';
      if (i === searchSuggestIndex) btn.classList.add('active');
      btn.dataset.suggestIndex = String(i);
      btn.textContent = word;
      el.appendChild(btn);
    });
  }

  // Recompute suggestions from the current input (called on each keystroke).
  // Skipped for math queries (`m:`/`$`/`\cmd`), which aren't word searches.
  function refreshSearchSuggestions() {
    var input = searchInputEl();
    if (!input || !searchPanelIsOpen()) { clearSearchSuggestions(); return; }
    var v = input.value;
    if (!v || looksLikeMathSearch(v) || /^\s*m:/.test(v)) { clearSearchSuggestions(); return; }
    searchSuggestions = suggestWords(v, SEARCH_MAX_SUGGESTIONS);
    searchSuggestIndex = -1;
    renderSearchSuggestions();
  }

  function cycleSearchSuggestion(delta) {
    if (!searchSuggestions.length) return false;
    var n = searchSuggestions.length;
    if (searchSuggestIndex < 0) searchSuggestIndex = delta > 0 ? 0 : n - 1;
    else searchSuggestIndex = ((searchSuggestIndex + delta) % n + n) % n;
    renderSearchSuggestions();
    return true;
  }

  // Replace the last word of the query with the chosen suggestion, then search.
  function acceptSearchSuggestion(i) {
    var input = searchInputEl();
    if (!input || i < 0 || i >= searchSuggestions.length) return false;
    input.value = input.value.replace(/\S*$/, searchSuggestions[i]);
    clearSearchSuggestions();
    input.focus({ preventScroll: true });
    runSearch(false);
    return true;
  }

  // First match at/after the current viewport top (so a fresh `/` lands on the
  // nearest match, then cycles from there).
  function firstTextResultIndex(results, backwards) {
    if (!results.length) return -1;
    var y = window.scrollY + topbarOffset() + 1;
    function top(r) { return r.getBoundingClientRect().top + window.scrollY; }
    if (backwards) {
      for (var i = results.length - 1; i >= 0; i--) if (top(results[i]) < y) return i;
      return results.length - 1;
    }
    for (var j = 0; j < results.length; j++) if (top(results[j]) >= y) return j;
    return 0;
  }

  function clearTextHighlights() {
    if (supportsHighlightApi()) {
      CSS.highlights.delete('mp-search-hit');
      CSS.highlights.delete('mp-search-active');
    }
  }

  function applyTextHighlights() {
    if (!supportsHighlightApi()) {
      // Fallback: select the active match (native highlight); cycle + counter
      // still work, just no all-match highlight.
      var active = textSearchResults[textSearchIndex];
      var sel = window.getSelection && window.getSelection();
      if (active && sel) { sel.removeAllRanges(); sel.addRange(active.cloneRange()); }
      return;
    }
    var hit = new Highlight();
    var act = new Highlight();
    // Explicit paint order (default is registration order, which flips across
    // delete/re-add cycles): the active match must stay above the all-match
    // tint AND above the editor-search green when they overlap.
    hit.priority = 1;
    act.priority = 2;
    for (var i = 0; i < textSearchResults.length; i++) {
      if (i === textSearchIndex) act.add(textSearchResults[i]);
      else hit.add(textSearchResults[i]);
    }
    CSS.highlights.set('mp-search-hit', hit);
    CSS.highlights.set('mp-search-active', act);
  }

  function scrollRangeIntoView(range) {
    if (!range || !range.getBoundingClientRect) return;
    var rect = range.getBoundingClientRect();
    // Degenerate rect = the match sits in a content-visibility-skipped block;
    // scroll the containing element instead (this forces it to render).
    if (rect.width === 0 && rect.height === 0) {
      var el = range.startContainer && (range.startContainer.nodeType === 1
        ? range.startContainer : range.startContainer.parentElement);
      if (el && el.scrollIntoView) el.scrollIntoView({ block: 'center', behavior: 'smooth' });
      return;
    }
    if (rect.top >= topbarOffset() + 8 && rect.bottom <= window.innerHeight - 8) return;
    var y = rect.top + window.scrollY - Math.max(topbarOffset() + 16, window.innerHeight * 0.3);
    window.scrollTo({ top: Math.max(0, y), behavior: 'smooth' });
  }

  // Shared current/total pill in the search panel; empty when idle.
  function updateSearchCount(index, total) {
    var el = document.getElementById('search-count');
    if (el) el.textContent = total > 0 ? (index + 1) + '/' + total : '';
  }

  function runPlainSearch(query, backwards) {
    var sameQuery = textSearchQuery === query;
    textSearchResults = buildSearchMatches(query);
    textSearchQuery = query;
    if (!textSearchResults.length) {
      textSearchIndex = -1;
      clearTextHighlights();
      updateSearchCount(-1, 0);
      return false;
    }
    var next;
    if (sameQuery && textSearchIndex >= 0) {
      next = textSearchIndex + (backwards ? -1 : 1);
      if (next < 0) next = textSearchResults.length - 1;         // wrap at the top
      if (next >= textSearchResults.length) next = 0;            // wrap at the end
    } else {
      next = firstTextResultIndex(textSearchResults, backwards);
    }
    textSearchIndex = next;
    applyTextHighlights();
    scrollRangeIntoView(textSearchResults[textSearchIndex]);
    updateSearchCount(textSearchIndex, textSearchResults.length);
    return true;
  }

  // --- Editor-driven search highlight: nvim's `/` pattern, pushed by the plugin
  // over the WS. Highlighted (all matches) like vim's hlsearch, in a distinct
  // color from the in-viewer `/` panel. Passive — no navigation, since the
  // cursor-sync already scrolls the preview to the current match. ---

  function editorSearchHighlight(query) {
    // Keep the pattern verbatim — a leading/trailing space is meaningful in a
    // search (`/ the `); only a whitespace-ONLY pattern means clear.
    editorSearchQuery = (query || '');
    if (!editorSearchQuery.trim() || !supportsHighlightApi()) {
      clearEditorSearchHighlight();
      return;
    }
    var hl = new Highlight();
    // Below the panel search's highlights (hit=1, active=2), so the in-viewer
    // search the user is actively navigating always paints on top.
    hl.priority = 0;
    buildTextMatches(editorSearchQuery).forEach(function(r) { hl.add(r); });
    CSS.highlights.set('mp-editor-search', hl);
  }

  function clearEditorSearchHighlight() {
    editorSearchQuery = '';
    if (supportsHighlightApi()) CSS.highlights.delete('mp-editor-search');
  }

  // Rebuild after a live re-render (the old Ranges point at replaced nodes).
  // DEFERRED off the patch path: the rebuild walks the whole document, which
  // on a large doc would add a full-text search to every keystroke's render.
  // Coalescing a typing burst into one rebuild ~120ms after the last patch
  // keeps typing latency flat; dead Ranges simply don't paint in the interim.
  var editorSearchRestoreTimer = 0;
  function restoreEditorSearchHighlights() {
    if (!editorSearchQuery) return;
    if (editorSearchRestoreTimer) clearTimeout(editorSearchRestoreTimer);
    editorSearchRestoreTimer = setTimeout(function() {
      editorSearchRestoreTimer = 0;
      if (editorSearchQuery) editorSearchHighlight(editorSearchQuery);
    }, 120);
  }

  // Rebuild after a live re-render (the old Ranges point at replaced nodes).
  function restoreTextSearchHighlights() {
    if (!searchPanelIsOpen() || !textSearchQuery) { clearTextHighlights(); return; }
    var old = textSearchIndex;
    textSearchResults = buildSearchMatches(textSearchQuery);
    if (!textSearchResults.length) {
      textSearchIndex = -1;
      clearTextHighlights();
      updateSearchCount(-1, 0);
      return;
    }
    textSearchIndex = Math.max(0, Math.min(old, textSearchResults.length - 1));
    applyTextHighlights();
    updateSearchCount(textSearchIndex, textSearchResults.length);
  }

  function runSearch(backwards) {
    var input = searchInputEl();
    var query = input ? input.value.trim() : lastSearchQuery;
    if (!query) {
      openSearchPanel();
      return false;
    }
    clearSearchSuggestions();
    lastSearchQuery = query;
    if (input && document.activeElement === input) input.blur();
    var recorded = recordViewerPlace();
    var parsed = parseSearchQuery(query);
    // Explicit math-mode sigil: skip `window.find` entirely. Otherwise
    // single-letter searches (e.g. `m:n` to find italic-n inside an
    // equation) get stuck cycling through body-text matches first.
    if (parsed.forceMath) {
      if (parsed.core && runMathSearch(parsed.core, backwards)) {
        // The math session owns the panel now; a lingering text session would
        // keep stale paint and clobber the counter on the next re-render.
        clearTextSession();
        return true;
      }
      if (mathSearchQuery) clearSearchSession();
      if (recorded) viewerJumpStack.pop();
      setStatus('dead', '○ no math match ' + parsed.core);
      return false;
    }
    // Math-looking queries (`\cmd`, `_`, `^`, `{}`, `$`) still route to math
    // search — those special characters aren't word searches, so fuzzy text
    // matching wouldn't apply anyway. Plain words go to the fuzzy text search.
    if (looksLikeMathSearch(query) && runMathSearch(query, backwards)) {
      clearTextSession();
      return true;
    }
    // Fully end any prior math session (not just its highlight classes) before a
    // plain search, or restoreMathSearchHighlights would revive the old glyph
    // paint on the next live re-render. The math fallback below repopulates this
    // state if it fires.
    if (mathSearchQuery) {
      mathSearchQuery = '';
      mathSearchResults = [];
      mathSearchIndex = -1;
      clearMathSearchHighlights();
    }
    var found = runPlainSearch(query, backwards);
    if (!found && runMathSearch(query, backwards)) {
      clearTextSession();
      return true;
    }
    if (!found && recorded) viewerJumpStack.pop();
    setStatus(
      found ? 'live' : 'dead',
      found
        ? '● ' + (textSearchIndex + 1) + '/' + textSearchResults.length + ' ' + query
        : '○ no match ' + query
    );
    return found;
  }

  // Close the viewer — the `q` key and the `:q` command. Browser scripts may
  // only close windows they opened (or single-history tabs, which a fresh
  // preview tab is), so
  // window.close() is best-effort — if we're still here a beat later, tell
  // the user how to really close via `hint`.
  function closeViewer(hint) {
    window.close();
    setTimeout(function() {
      var mac = /Mac/.test(navigator.platform || '');
      hint('the browser blocks tabs from closing themselves — press ' +
        (mac ? '⌘W' : 'Ctrl+W'));
    }, 150);
  }

  // `typing` marks a follow caused by an edit (the plugin tags those cursor
  // moves). Typing must not restart a scroll animation on every keystroke:
  // near the end of the document the 25%-anchor target is unreachable (the
  // page can't scroll that far) and the mid-typeset placeholder makes the
  // element's rect oscillate, so an eager scroll re-fires forever and the
  // whole viewport swims while you type. While typing, scroll only when the
  // element is truly out of view — and never launch a scroll the page can't
  // actually perform.
  function scrollSourceIntoView(target, typing) {
    if (!target) return;
    var rect = target.getBoundingClientRect();
    // Inside a content-visibility-skipped (off-screen) block, geometry reads
    // return a degenerate rect — scrollIntoView still resolves correctly and
    // forces the block to render.
    if (rect.width === 0 && rect.height === 0 && target.scrollIntoView) {
      recordViewerPlace();
      target.scrollIntoView({ block: 'center', behavior: 'smooth' });
      return;
    }
    var vh = window.innerHeight || document.documentElement.clientHeight || 800;
    var upper = typing ? 8 : vh * 0.25;
    var lower = typing ? vh - 8 : vh * 0.75;
    var y = rect.top + Math.min(rect.height / 2, 12);
    if (y >= upper && y <= lower) return;
    var maxScroll = Math.max(0, document.documentElement.scrollHeight - vh);
    var top = Math.min(maxScroll, Math.max(0, window.scrollY + rect.top - vh * 0.25));
    if (Math.abs(top - window.scrollY) < 4) return;
    recordViewerPlace();
    window.scrollTo({ top: top, behavior: 'smooth' });
  }

  // While a burst of typing stays inside one element, follow it at most once:
  // the first keystroke may scroll it into view, the rest hold still. Every
  // event refreshes the window, so re-following needs a pause in typing.
  var typingFollow = { key: null, at: 0 };
  function typingFollowAllowed(key) {
    var now = Date.now();
    var same = typingFollow.key === key && now - typingFollow.at < 1500;
    typingFollow.key = key;
    typingFollow.at = now;
    return !same;
  }

  function visibleSyncElement(el) {
    if (!el) return null;
    if (el.classList && el.classList.contains('blk')) {
      return el.querySelector(':scope > *') || el;
    }
    return el;
  }

  // The cursor-flash BOX, drawn in a page-level layer. An outline on the
  // flashed element is clipped by the render blocks' paint containment
  // wherever it crosses the block's box — for a paragraph that fills its
  // block (the common case) ALL FOUR edges land outside and the "box" was
  // just the background tint. Same doctrine as the refkey/lineno layers
  // (DEVELOPMENT.md, "The margin overlays"): measure, divide by the zoom
  // scale, draw in a layer nothing contains.
  var flashBoxTarget = null;
  var flashTrackRaf = 0;
  function removeFlashBox() {
    flashBoxTarget = null;
    if (flashTrackRaf) { cancelAnimationFrame(flashTrackRaf); flashTrackRaf = 0; }
    var page = pageEl();
    var l = page && page.querySelector(':scope > .flash-layer');
    if (l) l.remove();
  }
  // Measure the target and place the box. Returns the layer-relative
  // geometry key ('' while the target has no geometry — a
  // content-visibility-skipped block), so the tracker can compare frames.
  function positionFlashBox(page, layer) {
    var r = flashBoxTarget.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) {
      layer.replaceChildren();
      return '';
    }
    // Origin: the LAYER's own rect, not the page's — `inset: 0` anchors the
    // layer to the page's padding box, and the page's border would shift
    // every box by its width otherwise. Same for the scale denominator:
    // clientHeight is the padding box; offsetHeight (border box) would run
    // the scale slightly low and sag the box toward the page bottom.
    var pr = layer.getBoundingClientRect();
    var scale = page.clientHeight > 0 ? pr.height / page.clientHeight : 1;
    if (!isFinite(scale) || scale <= 0) scale = 1;
    var box = layer.firstElementChild;
    if (!box) {
      box = document.createElement('div');
      box.className = 'flash-box';
      layer.appendChild(box);
    }
    var pad = 4; // the outline-offset the box replaces
    box.style.left = Math.round((r.left - pr.left) / scale - pad) + 'px';
    box.style.top = Math.round((r.top - pr.top) / scale - pad) + 'px';
    box.style.width = Math.round(r.width / scale + 2 * pad) + 'px';
    box.style.height = Math.round(r.height / scale + 2 * pad) + 'px';
    return ((r.left - pr.left) / scale).toFixed(1) + ',' +
      ((r.top - pr.top) / scale).toFixed(1) + ',' +
      (r.width / scale).toFixed(1) + ',' + (r.height / scale).toFixed(1);
  }
  function drawFlashBox(el) {
    if (el) flashBoxTarget = el;
    var page = pageEl();
    if (!page || !flashBoxTarget) return;
    if (!flashBoxTarget.isConnected) {
      // The flashed element was edited away mid-flash (a patch removed its
      // block) — a box left behind would outline whatever reflowed into its
      // place.
      removeFlashBox();
      return;
    }
    var layer = page.querySelector(':scope > .flash-layer');
    if (!layer) {
      layer = document.createElement('div');
      layer.className = 'flash-layer';
      page.appendChild(layer);
    }
    var lastKey = positionFlashBox(page, layer);
    // The outline this replaces was painted on the element and tracked
    // every reflow for free; a positioned box must track them itself.
    // During the short flash window, follow the element through ANY layout
    // shift — lazy typesets landing, proof folds, a skipped block gaining
    // geometry after the scroll renders it — by re-measuring each frame.
    // The key is layer-relative, so plain scrolling neither redraws nor
    // writes. rAF freezes on hidden pages, where a misplaced transient box
    // can't be seen anyway.
    if (!flashTrackRaf) {
      var track = function() {
        if (!flashBoxTarget) { flashTrackRaf = 0; return; }
        if (!flashBoxTarget.isConnected) { removeFlashBox(); return; }
        var p = pageEl();
        var l = p && p.querySelector(':scope > .flash-layer');
        if (l) {
          var now = flashBoxTarget.getBoundingClientRect();
          var origin = l.getBoundingClientRect();
          var scale = p.clientHeight > 0 ? origin.height / p.clientHeight : 1;
          if (!isFinite(scale) || scale <= 0) scale = 1;
          var key = ((now.left - origin.left) / scale).toFixed(1) + ',' +
            ((now.top - origin.top) / scale).toFixed(1) + ',' +
            (now.width / scale).toFixed(1) + ',' + (now.height / scale).toFixed(1);
          if (key !== lastKey) lastKey = positionFlashBox(p, l);
        }
        flashTrackRaf = requestAnimationFrame(track);
      };
      flashTrackRaf = requestAnimationFrame(track);
    }
  }

  function revealSourceElement(id, shouldScroll) {
    if (!id) return;
    activeSourceId = id;
    document.querySelectorAll('.source-active').forEach(function(el) {
      el.classList.remove('source-active');
    });
    var raw = document.getElementById(id);
    var el = visibleSyncElement(raw);
    // Unresolvable id (the flashed element was edited away): drop the box
    // too, or it would sit over whatever reflowed into its place.
    if (!el) { removeFlashBox(); return; }
    el.classList.add('source-active');
    // Spaces keep their small ::after caret (class-driven) instead of a box.
    if (el.classList.contains('source-space')) removeFlashBox();
    else drawFlashBox(el);
    if (sourceFlashTimer) clearTimeout(sourceFlashTimer);
    sourceFlashTimer = setTimeout(function() {
      if (el && el.classList) el.classList.remove('source-active');
      removeFlashBox();
      // The flash is over — forget it, or restoreSourceHighlight would
      // resurrect a long-faded box on the next re-render (visible as a
      // one-beat blink when a patch lands just before the first
      // typing-tagged cursor event clears things).
      activeSourceId = null;
      sourceFlashTimer = 0;
    }, 1800);
    if (shouldScroll) scrollSourceIntoView(el);
  }

  function restoreSourceHighlight() {
    if (activeSourceId) revealSourceElement(activeSourceId, false);
  }

  // Scroll a sync element into view WITHOUT the flash — the cursor sits on a
  // block-level element (a section heading) that deliberately doesn't flash,
  // but the viewer must still follow it (e.g. a search wrapping to the top).
  function scrollSourceElementOnly(id, typing) {
    if (!id) return;
    if (typing && !typingFollowAllowed('el:' + id)) return;
    var el = visibleSyncElement(document.getElementById(id));
    if (el) scrollSourceIntoView(el, typing);
  }

  // Highlight every element of the editor's visual selection. Unlike the cursor
  // flash, this is persistent (no timeout) — it stays until the daemon sends an
  // empty list (selection dismissed) or a new range. `ids` come pre-resolved
  // from the daemon's SyncIndex range lookup.
  // Clear the flashing point highlight (cursor on prose / single equation).
  function clearSourceActive() {
    if (sourceFlashTimer) { clearTimeout(sourceFlashTimer); sourceFlashTimer = 0; }
    document.querySelectorAll('.source-active').forEach(function(el) {
      el.classList.remove('source-active');
    });
    activeSourceId = null;
    removeFlashBox();
  }

  // Clear the line/row band (visual selection or cursor-on-math-row).
  function clearSourceRangeHighlights() {
    document.querySelectorAll('.source-range').forEach(function(el) {
      el.classList.remove('source-range');
    });
    clearMathRowHighlights();
    activeSourceRangeIds = [];
    activeMathRows = [];
  }

  function highlightSourceRange(ids, shouldScroll, mathRows, typing) {
    // Cursor moved onto a math row → drop any lingering prose flash.
    clearSourceActive();
    document.querySelectorAll('.source-range').forEach(function(el) {
      el.classList.remove('source-range');
    });
    activeSourceRangeIds = Array.isArray(ids) ? ids : [];
    activeMathRows = Array.isArray(mathRows) ? mathRows : [];
    var first = null;
    activeSourceRangeIds.forEach(function(id) {
      var el = visibleSyncElement(document.getElementById(id));
      if (!el) return;
      el.classList.add('source-range');
      if (!first) first = el;
    });
    highlightMathRows(activeMathRows);
    if (!first && activeMathRows.length) {
      var mb = document.getElementById(activeMathRows[0].id);
      if (mb) first = mb;
    }
    if (shouldScroll && first) {
      // Typing inside a multi-row env updates the band on every keystroke;
      // gate the scroll the same way as the prose path (once per burst, and
      // only when out of view) or the row band strobes the viewport.
      if (!typing || typingFollowAllowed('rng:' + (first.id || activeSourceRangeIds[0] || ''))) {
        scrollSourceIntoView(first, typing);
      }
    }
  }

  // Remove all per-row math highlight rectangles and the whole-block box.
  function clearMathRowHighlights() {
    document.querySelectorAll('rect.mp-row-hl').forEach(function(r) {
      if (r.parentNode) r.parentNode.removeChild(r);
    });
    document.querySelectorAll('.math.display.mp-math-active').forEach(function(el) {
      el.classList.remove('mp-math-active');
    });
  }

  // The rendered rows of a multi-row math block: the direct
  // [data-mml-node="mtr"/"mlabeledtr"] rows of the OUTERMOST mtable (first in
  // document order — the align/gather itself). A descendant query would also
  // grab mtr from a nested matrix/cases/aligned inside a row, inflating the
  // count and breaking the index→row mapping, so keep only rows whose nearest
  // mtable IS the outermost. Shared by the forward highlight
  // (highlightMathRows) and the backward click→row mapping (mathRowFromClick)
  // — both directions must count rows identically or their index schemes
  // drift apart.
  function mathRowGroups(block) {
    var svg = block ? block.querySelector('svg') : null;
    var mtable = svg ? svg.querySelector('[data-mml-node="mtable"]') : null;
    var groups = [];
    if (mtable) {
      mtable.querySelectorAll('[data-mml-node="mtr"],[data-mml-node="mlabeledtr"]')
        .forEach(function(g) {
          if (g.closest('[data-mml-node="mtable"]') === mtable) groups.push(g);
        });
    }
    return groups;
  }

  // Which rendered row of a multi-row math block a click landed on — the
  // backward-search counterpart of highlightMathRows' index scheme. Returns
  // { row, count } or null (single-row block, or the click missed every row).
  // A click inside a nested table (cases/matrix) walks up to the enclosing
  // outer row; a click on the equation-number / refkey gutter maps through
  // its per-row wrapper, which the renderer emits one per row. A click that
  // hits no row's INK but lands inside the SVG resolves by vertical
  // position: a row's <g> only covers its glyphs, so most of a row — the
  // space between and around symbols — would otherwise not count, making
  // airy rows (wide integrals, sparse alignment columns) nearly unhittable.
  function mathRowFromClick(block, target, clientY) {
    var groups = mathRowGroups(block);
    if (groups.length < 2 || !target || !target.closest) return null;
    var rowSel = '[data-mml-node="mtr"],[data-mml-node="mlabeledtr"]';
    var g = target.closest(rowSel);
    while (g && groups.indexOf(g) === -1) {
      g = g.parentElement && g.parentElement.closest(rowSel);
    }
    if (g) return { row: groups.indexOf(g), count: groups.length };
    var gutterRow = target.closest('.eq-num-row, .eq-refkey-row');
    if (gutterRow && gutterRow.parentElement && block.contains(gutterRow)) {
      var i = Array.prototype.indexOf.call(gutterRow.parentElement.children, gutterRow);
      if (i >= 0 && i < groups.length) return { row: i, count: groups.length };
    }
    // Geometric fallback: nearest row band by click Y, bounded to the SVG
    // (clicks on the block's outer padding keep selecting the whole env).
    var svg = block.querySelector('svg');
    if (svg && isFinite(clientY)) {
      var sr = svg.getBoundingClientRect();
      if (clientY >= sr.top && clientY <= sr.bottom) {
        var best = -1, bestDist = Infinity;
        for (var j = 0; j < groups.length; j++) {
          var r = groups[j].getBoundingClientRect();
          if (!r.height) continue;
          var d = clientY < r.top ? r.top - clientY
                : clientY > r.bottom ? clientY - r.bottom : 0;
          if (d < bestDist) { bestDist = d; best = j; }
        }
        if (best !== -1) return { row: best, count: groups.length };
      }
    }
    return null;
  }

  // Highlight the selected rows of multi-row math blocks (align/gather). Each
  // entry is { id, count, rows } from the daemon: `count` is the number of rows
  // it expects (a guard against MathJax merging/dropping rows) and `rows` the
  // selected indices. We insert a translucent <rect> behind each selected SVG
  // table row (data-mml-node="mtr"); the rect lives INSIDE the row's <g>, so it
  // inherits the row transform and scales with the page zoom — no coordinate
  // conversion needed. If the rendered row count doesn't match what the daemon
  // expected, fall back to highlighting the whole block.
  function highlightMathRows(mathRows) {
    clearMathRowHighlights();
    (mathRows || []).forEach(function(mr) {
      if (!mr || !mr.id) return;
      var block = document.getElementById(mr.id);
      if (!block) return;
      // Box the whole equation block (CSS outline on its SVG); the rows below
      // get the fill. One clean box beats per-row strokes that clip mid-edge.
      block.classList.add('mp-math-active');
      var groups = mathRowGroups(block);
      if (!groups.length || groups.length !== mr.count) {
        block.classList.add('source-range');
        return;
      }
      (mr.rows || []).forEach(function(i) {
        var g = groups[i];
        if (!g || !g.getBBox) return;
        var bb;
        try { bb = g.getBBox(); } catch (e) { return; }
        if (!bb || !isFinite(bb.width) || bb.width <= 0 || bb.height <= 0) return;
        var padY = bb.height * 0.12;
        var padX = bb.height * 0.1;
        var rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
        rect.setAttribute('class', 'mp-row-hl');
        rect.setAttribute('x', bb.x - padX);
        rect.setAttribute('y', bb.y - padY);
        rect.setAttribute('width', bb.width + padX * 2);
        rect.setAttribute('height', bb.height + padY * 2);
        g.insertBefore(rect, g.firstChild);
      });
    });
  }

  // Re-apply the selection highlight after a re-render rebuilds the DOM (no
  // scroll — don't yank the view on every keystroke), mirroring
  // restoreSourceHighlight for the cursor.
  function restoreSourceRange() {
    if (activeSourceRangeIds.length || activeMathRows.length) {
      highlightSourceRange(activeSourceRangeIds, false, activeMathRows);
    }
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

  // Source-jump trigger picked by the user via [viewer.source-jump]
  // trigger in config.toml. The daemon injects the choice as
  // `window.__mpConfig.sourceJumpTrigger`; we default to `cmd-click`
  // when the config block isn't present (older daemons, render-only
  // pipeline, etc.).
  function sourceJumpTrigger() {
    var cfg = window.__mpConfig && window.__mpConfig.sourceJumpTrigger;
    return cfg || 'cmd-click';
  }

  // Whether the current event matches the configured reveal-source
  // trigger. `eventType` is "click" or "dblclick" depending on which
  // listener invoked it — the four trigger options split between those
  // two event types.
  function matchesRevealTrigger(e, eventType) {
    var t = sourceJumpTrigger();
    if (eventType === 'dblclick') {
      return t === 'double-click';
    }
    // For click-based triggers, require the matching modifier and the
    // absence of the others so the user doesn't double-fire (e.g.
    // Shift+Cmd+click won't trigger when "cmd-click" is the chosen
    // gesture — that's likely a text-selection chord).
    if (eventType !== 'click') return false;
    switch (t) {
      case 'cmd-click':
        return (e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey;
      case 'ctrl-click':
        return e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey;
      case 'alt-click':
        return e.altKey && !e.metaKey && !e.ctrlKey && !e.shiftKey;
      default:
        return false;
    }
  }

  async function postRevealSource(info) {
    if (!info) return;
    try {
      var res = await fetch('/reveal-source', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(info)
      });
      if (res.status === 204) {
        setStatus('live', '● editor → ' + info.line + ':' + info.col);
      }
      // Failures are swallowed silently because `requestRevealSource`
      // also fires `/jump` in parallel: if the editor-spawn path
      // (`--editor` template) isn't wired up — typically a missing
      // $NVIM_LISTEN_ADDRESS — the nvim plugin still picks up the
      // polled jump and the user gets the navigation regardless.
      // Logging the spawn failure to console keeps it diagnosable
      // without spamming the status pill.
      else {
        try {
          var body = await res.json();
          if (body && body.error) console.warn('reveal-source:', body.error);
        } catch (e) {}
      }
    } catch (e) {
      console.warn('reveal-source:', e && e.message || e);
    }
  }

  function requestRevealSource(e) {
    var el = sourceElementFromTarget(e.target);
    var info = el ? parseDataSrc(el.getAttribute('data-src')) : null;
    if (!info) return false;
    e.preventDefault();
    e.stopPropagation();
    // A click inside a multi-row align/gather also names the exact rendered
    // row; the daemon resolves it to that row's own source line through the
    // sync index (always as fresh as the render — nothing to go stale in the
    // DOM). The parsed data-src above stays in the payload as the daemon's
    // fallback anchor when the row can't be resolved.
    if (el.id && el.classList.contains('math')) {
      var rowInfo = mathRowFromClick(el, e.target, e.clientY);
      if (rowInfo) {
        info.element_id = el.id;
        info.math_row = rowInfo.row;
        info.row_count = rowInfo.count;
      }
    }
    revealSourceElement(el.id, false);
    // Fire both endpoints: `/jump` lands the request on whichever nvim
    // plugin is polling for it (the path that already works without
    // any editor-spawn config); `/reveal-source` is the best-effort
    // active-spawn path that runs the `--editor` template if the
    // shell has $NVIM_LISTEN_ADDRESS (or whatever the template needs).
    // Either succeeding is enough — failures of `/reveal-source` are
    // surfaced on the status pill but don't suppress the `/jump`.
    postSourceJump(info);
    postRevealSource(info);
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
    var changed = refkeysVisible !== !!visible;
    refkeysVisible = !!visible;
    document.body.classList.toggle('refkey-visible', refkeysVisible);
    // A user-visible toggle prelays the whole layer before its first paint.
    // Same-state render updates keep the trailing coalescing cadence.
    scheduleRefkeys(changed ? 0 : undefined);
    var page = pageEl();
    // Same-value setAttribute still dirties attribute-selector styles for the
    // whole page — and this runs on every patch. Only write on a real change.
    var want = refkeysVisible ? 'visible' : 'hidden';
    if (page && page.getAttribute('data-refkeys') !== want) {
      page.setAttribute('data-refkeys', want);
    }
    var btn = document.getElementById('refkey-toggle');
    if (btn) {
      btn.classList.toggle('active', refkeysVisible);
      btn.setAttribute('aria-pressed', refkeysVisible ? 'true' : 'false');
    }
    if (persist) {
      try { localStorage.setItem('mathpreview.refkeys', refkeysVisible ? '1' : '0'); } catch (e) {}
    }
  }

  function setLineNumbers(visible, persist) {
    lineNumbersVisible = !!visible;
    document.body.classList.toggle('lineno-visible', lineNumbersVisible);
    var btn = document.getElementById('lineno-toggle');
    if (btn) {
      btn.classList.toggle('active', lineNumbersVisible);
      btn.setAttribute('aria-pressed', lineNumbersVisible ? 'true' : 'false');
    }
    if (lineNumbersVisible) {
      scheduleLineNumbers();
    } else {
      clearLineNumbers();
    }
    if (persist) {
      try { localStorage.setItem('mathpreview.lineNumbers', lineNumbersVisible ? '1' : '0'); } catch (e) {}
    }
  }

  function setTheme(mode, persist) {
    themeMode = (mode === 'dark') ? 'dark' : 'light';
    // Toggle on <html> too, so `html { background: var(--bg) }` flips — the
    // root element resolves --bg at its own level, not the body's.
    document.documentElement.classList.toggle('theme-dark', themeMode === 'dark');
    document.body.classList.toggle('theme-dark', themeMode === 'dark');
    var btn = document.getElementById('theme-toggle');
    if (btn) {
      var on = themeMode === 'dark';
      btn.classList.toggle('active', on);
      btn.setAttribute('aria-pressed', on ? 'true' : 'false');
      setViewerActionTitle(btn, on ? 'light mode' : 'dark mode');
      btn.setAttribute('aria-label', on ? 'switch to light mode' : 'switch to dark mode');
      var icon = btn.querySelector('.theme-toggle-icon');
      if (icon) icon.textContent = on ? '☀' : '☾';
    }
    if (persist) {
      try { localStorage.setItem('mathpreview.theme', themeMode); } catch (e) {}
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
    // Margin mode toggles the pin-on-click interaction (and the inline
    // sidenote layout). It does NOT remove already-pinned cards — those stay
    // visible so you can turn margin mode off to click through to the document,
    // then turn it back on, without losing your pins. `:clear` is the explicit
    // way to remove all cards. Column visibility is driven by whether cards
    // exist (see updateMarginCardsClass), not by this mode flag.
    if (!marginMode) {
      clearSidenoteLayout();
    } else {
      scheduleSidenoteLayout();
    }
  }

  /// The cards stack lives inside #margin-cards so the toolbar (typed-
  /// refkey input + feedback span) at the top of #margin survives a
  /// "close all" wipe and isn't accidentally rebuilt on every card pin.
  function marginCardsEl() { return document.getElementById('margin-cards'); }
  function marginCardsLeftEl() { return document.getElementById('margin-cards-left'); }
  function marginCardsContainers() {
    return [marginCardsEl(), marginCardsLeftEl()].filter(Boolean);
  }

  /// Per-card left/right dock side, persisted by pin key so a card returns to
  /// the side you last dragged it to (within and across sessions). Default
  /// right — the historical single-column position.
  function loadMarginSides() {
    try { return JSON.parse(localStorage.getItem('mathpreview.marginSides') || '{}') || {}; }
    catch (e) { return {}; }
  }
  function marginSideFor(key) {
    return loadMarginSides()[key] === 'left' ? 'left' : 'right';
  }
  function setMarginSide(key, side) {
    if (!key) return;
    var m = loadMarginSides();
    m[key] = (side === 'left') ? 'left' : 'right';
    try { localStorage.setItem('mathpreview.marginSides', JSON.stringify(m)); } catch (e) {}
  }
  function marginContainerForSide(side) {
    return side === 'left' ? marginCardsLeftEl() : marginCardsEl();
  }
  /// Append a freshly built card to its remembered column.
  function placeMarginCard(card, key) {
    var side = marginSideFor(key);
    card.dataset.side = side;
    var container = marginContainerForSide(side) || marginCardsEl();
    if (container) container.appendChild(card);
  }

  /// Drive margin-column visibility from card presence (NOT from margin mode),
  /// so pinned cards survive turning the pin-on-click mode off. Each column is
  /// shown only when it actually holds a card. `margin-has-cards` is kept for
  /// any consumers that care whether anything is pinned at all.
  function updateMarginCardsClass() {
    var right = marginCardsEl();
    var left = marginCardsLeftEl();
    var rightHas = !!(right && right.querySelector('.margin-card'));
    var leftHas = !!(left && left.querySelector('.margin-card'));
    document.body.classList.toggle('margin-has-cards', pinnedRefs.size > 0);
    document.body.classList.toggle('margin-right-has-cards', rightHas);
    document.body.classList.toggle('margin-left-has-cards', leftHas);
    // A column widens only to HOST a card that's been individually pinned
    // (`.margin-card.expanded`); un-pinned siblings stay at gutter width and hug
    // the page edge (see CSS), so pinning one card doesn't expand the rest.
    // Recomputed from the cards actually present each time, so closing or
    // dragging the expanded card can't strand the column wide.
    var rightExp = !!(right && right.querySelector('.margin-card.expanded'));
    var leftExp = !!(left && left.querySelector('.margin-card.expanded'));
    document.body.classList.toggle('margin-right-expanded', rightExp);
    document.body.classList.toggle('margin-left-expanded', leftExp);
    // Each horizontal-expand button reflects ITS OWN card's expanded state.
    document.querySelectorAll('.margin-card').forEach(function(card) {
      var btn = card.querySelector('.margin-card-expand');
      if (!btn) return;
      var on = card.classList.contains('expanded');
      btn.classList.toggle('active', on);
      btn.setAttribute('aria-pressed', on ? 'true' : 'false');
      btn.setAttribute(
        'aria-label', on ? 'collapse to the margin' : 'expand horizontally over the text'
      );
      btn.title = on ? 'collapse to the margin' : 'expand horizontally over the text';
    });
  }

  /// Toggle whether THIS card expands past the gutter, over the text. Per-card:
  /// only the clicked card grows. The column widens just enough to host it (see
  /// updateMarginCardsClass), while unexpanded sibling cards stay at gutter
  /// width, so expanding one card no longer expands the others.
  function toggleMarginExpand(card) {
    if (!card) return;
    card.classList.toggle('expanded');
    updateMarginCardsClass();
  }

  function closeAllMarginCards() {
    marginCardsContainers().forEach(function(c) { c.innerHTML = ''; });
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
    // Magnify: opens this note centered on screen, enlarged, for reading.
    var zoom = document.createElement('button');
    zoom.type = 'button';
    zoom.className = 'margin-card-zoom';
    zoom.setAttribute('aria-label', 'magnify');
    zoom.title = 'magnify (read full size)';
    zoom.textContent = '⤢';
    // Horizontal expand: widen THIS card past the gutter, over the text, so a
    // narrow note can be read in place. Per-card — siblings stay put.
    var expand = document.createElement('button');
    expand.type = 'button';
    expand.className = 'margin-card-expand';
    expand.setAttribute('aria-label', 'expand horizontally over the text');
    expand.setAttribute('aria-pressed', 'false');
    expand.title = 'expand horizontally over the text';
    expand.textContent = '↔';
    var close = document.createElement('button');
    close.type = 'button';
    close.className = 'margin-card-close';
    close.setAttribute('aria-label', 'unpin');
    close.title = 'unpin';
    close.textContent = '×';
    head.appendChild(grip);
    head.appendChild(title);
    head.appendChild(zoom);
    head.appendChild(expand);
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
    var cards = document.querySelectorAll('.margin-cards .margin-card');
    cards.forEach(function(c) {
      c.classList.remove('drop-above');
      c.classList.remove('drop-below');
    });
  }
  function dropPositionFor(card, clientY) {
    var rect = card.getBoundingClientRect();
    return clientY < rect.top + rect.height / 2 ? 'above' : 'below';
  }
  /// Drag-to-reorder AND drag-across-columns for margin cards. Both the right
  /// (#margin-cards) and left (#margin-cards-left) stacks get the same handlers,
  /// so a card can be reordered within a column or moved to the other side; the
  /// target column is whichever container received the drop (e.currentTarget).
  /// The card's remembered side is updated so it returns there next time.
  function initMarginDnd() {
    marginCardsContainers().forEach(function(cards) {
      if (cards.dataset.dndInit === '1') return;
      cards.dataset.dndInit = '1';
      cards.addEventListener('dragstart', function(e) {
        var card = e.target && e.target.closest && e.target.closest('.margin-card');
        if (!card) return;
        dragSourceCard = card;
        card.classList.add('dragging');
        // Reveal both columns as drop targets for the duration of the drag —
        // an empty column is display:none and otherwise couldn't catch the
        // first card dropped onto it (CSS shows them under .margin-dragging).
        document.body.classList.add('margin-dragging');
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
        var dest = e.currentTarget;
        var card = e.target && e.target.closest && e.target.closest('.margin-card');
        if (card && card !== dragSourceCard) {
          var pos = dropPositionFor(card, e.clientY);
          if (pos === 'above') dest.insertBefore(dragSourceCard, card);
          else dest.insertBefore(dragSourceCard, card.nextSibling);
        } else if (card !== dragSourceCard) {
          // Dropped on empty container space → append to that column's end.
          dest.appendChild(dragSourceCard);
        }
        var side = (dest === marginCardsLeftEl()) ? 'left' : 'right';
        dragSourceCard.dataset.side = side;
        setMarginSide(dragSourceCard.dataset.pinKey, side);
        clearDropIndicators();
        dragSourceCard.classList.remove('dragging');
        dragSourceCard = null;
        document.body.classList.remove('margin-dragging');
        updateMarginCardsClass();
      });
      cards.addEventListener('dragend', function() {
        clearDropIndicators();
        document.body.classList.remove('margin-dragging');
        if (dragSourceCard) {
          dragSourceCard.classList.remove('dragging');
          dragSourceCard = null;
        }
      });
    });
  }

  /// Open a margin card's content centered on screen, enlarged, for reading
  /// (#4). Clones the card body into a modal <dialog>; Esc / backdrop / the
  /// close button dismiss it (wired in patch.js). Cloning keeps the live card
  /// (and its already-typeset math) intact.
  function openMarginZoom(card) {
    var dlg = document.getElementById('margin-zoom-dialog');
    var body = document.getElementById('margin-zoom-body');
    if (!dlg || !body || !card) return;
    var src = card.querySelector('.margin-card-body');
    body.innerHTML = '';
    if (src) {
      var clone = src.cloneNode(true);
      // Drop the in-column constraints (max-height / overflow / 1em sizing) so
      // the dialog's own enlarged, scrollable sizing governs the magnified view.
      clone.classList.remove('margin-card-body');
      body.appendChild(clone);
    }
    if (typeof dlg.showModal === 'function' && !dlg.open) dlg.showModal();
  }
  function closeMarginZoom() {
    var dlg = document.getElementById('margin-zoom-dialog');
    if (dlg && dlg.open) dlg.close();
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
    if (!marginCardsEl()) return;
    placeMarginCard(card, key);
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
    if (!marginCardsEl()) return { ok: false, reason: 'no-margin' };
    placeMarginCard(card, key);
    pinnedRefs.set(key, card);
    updateMarginCardsClass();
    return { ok: true, reason: 'pinned' };
  }

  /// Refkey chips are toggles: the first activation opens the target's margin
  /// card and the second closes that same card. Keep `pinByRefkey` idempotent
  /// for the explicit `:pin` command; only direct chip activation uses this.
  function togglePinByRefkey(rawKey) {
    var key = (rawKey || '').trim();
    if (!key) return { ok: false, reason: 'empty' };
    if (pinnedRefs.has(key)) return unpinByRefkey(key);
    return pinByRefkey(key);
  }

  function unpinByRefkey(rawKey) {
    var key = (rawKey || '').trim();
    if (!key) return { ok: false, reason: 'empty' };
    if (!pinnedRefs.has(key)) return { ok: false, reason: 'not-pinned' };
    var card = pinnedRefs.get(key);
    if (card && card.parentNode) card.parentNode.removeChild(card);
    pinnedRefs.delete(key);
    updateMarginCardsClass();
    return { ok: true, reason: 'unpinned' };
  }

  /// Vim-style command line. Hidden by default; `:` (from outside an
  /// editable target) opens it with the prompt focused. The supported
  /// command set is intentionally small — anything richer should be
  /// designed before adding here.
  ///   :pin <key>     pin <key>'s target as a margin card
  ///   :unpin <key>   remove the matching card (no-op if not pinned)
  ///   :clear         remove all pinned cards
  ///   :q / :quit     close the viewer tab via window.close(), allowed for a
  ///                  single-history tab — otherwise show the ⌘W/Ctrl+W hint
  /// Enter executes, Esc closes, empty-Backspace also closes.
  var cmdlineFeedbackTimer = 0;
  var cmdlineSuggestions = [];
  var cmdlineSuggestionIndex = -1;
  var CMDLINE_MAX_SUGGESTIONS = 12;
  function cmdlineEl() { return document.getElementById('cmdline'); }
  function cmdlineInputEl() { return document.getElementById('cmdline-input'); }
  function cmdlineFeedbackEl() { return document.getElementById('cmdline-feedback'); }
  function cmdlineSuggestionsEl() { return document.getElementById('cmdline-suggestions'); }

  /// Collect every available refkey from the page in one pass. Source set:
  ///   * `[data-refkey]` on theorems, sections, equations, floats
  ///   * `[data-target]` on per-row `.eq-refkey-chip` (align / gather)
  ///   * `[data-key]` on `<dt>` bib entries
  /// Returned sorted alphabetically; dedup via Set.
  function collectAllRefkeys() {
    var keys = new Set();
    var page = document.getElementById('page');
    if (!page) return [];
    page.querySelectorAll('[data-refkey]:not(.label-anchor)').forEach(function(el) {
      var k = el.getAttribute('data-refkey');
      if (k) keys.add(k);
    });
    page.querySelectorAll('.eq-refkey-chip[data-target]').forEach(function(el) {
      var k = el.getAttribute('data-target');
      if (k) keys.add(k);
    });
    page.querySelectorAll('dt[data-key]').forEach(function(el) {
      var k = el.getAttribute('data-key');
      if (k) keys.add(k);
    });
    return Array.from(keys).sort();
  }

  /// Fuzzy match score: substring hits rank above subsequence hits, and
  /// among ties shorter candidates win (closer to a prefix completion).
  /// Returns 0 for no match.
  function fuzzyScore(query, candidate) {
    if (!query) return 1;
    var q = query.toLowerCase();
    var c = candidate.toLowerCase();
    var idx = c.indexOf(q);
    if (idx !== -1) {
      // Substring: prefix hits beat mid-string hits; shorter beats longer.
      return 1000 - idx * 10 - Math.max(0, c.length - q.length);
    }
    // Subsequence walk (every char of q appears in c in order).
    var qi = 0;
    for (var i = 0; i < c.length && qi < q.length; i++) {
      if (c.charCodeAt(i) === q.charCodeAt(qi)) qi++;
    }
    if (qi !== q.length) return 0;
    return 100 - Math.max(0, c.length - q.length);
  }

  function suggestionsForArg(arg) {
    var keys = collectAllRefkeys();
    var scored = [];
    for (var i = 0; i < keys.length; i++) {
      var s = fuzzyScore(arg, keys[i]);
      if (s > 0) scored.push({ key: keys[i], score: s });
    }
    scored.sort(function(a, b) {
      if (b.score !== a.score) return b.score - a.score;
      return a.key.localeCompare(b.key);
    });
    return scored.map(function(s) { return s.key; });
  }

  function parseCmdline(text) {
    var t = text || '';
    var space = t.indexOf(' ');
    var cmd = (space < 0 ? t : t.slice(0, space)).toLowerCase();
    var arg = space < 0 ? '' : t.slice(space + 1);
    return { cmd: cmd, arg: arg, hasSpace: space >= 0 };
  }

  /// Show suggestions for `:pin <prefix>` / `:unpin <prefix>` only.
  /// `:clear` and unknown commands → hide the strip.
  function refreshCmdlineSuggestions() {
    var strip = cmdlineSuggestionsEl();
    var input = cmdlineInputEl();
    if (!strip || !input) return;
    var parsed = parseCmdline(input.value);
    var wantsArg = (parsed.cmd === 'pin' || parsed.cmd === 'p' ||
                    parsed.cmd === 'unpin' || parsed.cmd === 'u');
    if (!wantsArg || !parsed.hasSpace) {
      cmdlineSuggestions = [];
      cmdlineSuggestionIndex = -1;
      strip.hidden = true;
      strip.replaceChildren();
      return;
    }
    var matches = suggestionsForArg(parsed.arg.trim());
    // For :unpin, narrow to currently-pinned keys so completion is
    // useful — `:unpin <Tab>` should cycle what's actually on screen.
    if (parsed.cmd === 'unpin' || parsed.cmd === 'u') {
      matches = matches.filter(function(k) { return pinnedRefs.has(k); });
    }
    cmdlineSuggestions = matches;
    cmdlineSuggestionIndex = -1;
    renderCmdlineSuggestions();
  }
  function renderCmdlineSuggestions() {
    var strip = cmdlineSuggestionsEl();
    if (!strip) return;
    if (!cmdlineSuggestions.length) {
      strip.hidden = true;
      strip.replaceChildren();
      return;
    }
    strip.hidden = false;
    strip.replaceChildren();
    var show = cmdlineSuggestions.slice(0, CMDLINE_MAX_SUGGESTIONS);
    show.forEach(function(key, i) {
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'cmdline-suggestion';
      if (i === cmdlineSuggestionIndex) btn.classList.add('active');
      btn.dataset.suggestionIndex = String(i);
      btn.textContent = key;
      strip.appendChild(btn);
    });
    if (cmdlineSuggestions.length > CMDLINE_MAX_SUGGESTIONS) {
      var more = document.createElement('span');
      more.className = 'cmdline-suggestion-overflow';
      more.textContent = '+' + (cmdlineSuggestions.length - CMDLINE_MAX_SUGGESTIONS) + ' more';
      strip.appendChild(more);
    }
  }
  function cycleCmdlineSuggestion(delta) {
    if (!cmdlineSuggestions.length) return false;
    var max = Math.min(cmdlineSuggestions.length, CMDLINE_MAX_SUGGESTIONS);
    if (cmdlineSuggestionIndex < 0) {
      cmdlineSuggestionIndex = delta > 0 ? 0 : max - 1;
    } else {
      cmdlineSuggestionIndex = ((cmdlineSuggestionIndex + delta) % max + max) % max;
    }
    var input = cmdlineInputEl();
    if (input) {
      var parsed = parseCmdline(input.value);
      // Replace the arg with the selected suggestion but keep the
      // original command spelling (so `:p ` doesn't turn into `:pin `).
      var head = parsed.hasSpace
        ? input.value.slice(0, input.value.indexOf(' ') + 1)
        : input.value + ' ';
      input.value = head + cmdlineSuggestions[cmdlineSuggestionIndex];
      // Caret to end so further typing extends the completion.
      try {
        input.setSelectionRange(input.value.length, input.value.length);
      } catch (e) {}
    }
    renderCmdlineSuggestions();
    return true;
  }
  function setCmdlineFeedback(text, isError) {
    var fb = cmdlineFeedbackEl();
    if (!fb) return;
    fb.textContent = text || '';
    fb.classList.toggle('error', !!isError);
    if (cmdlineFeedbackTimer) clearTimeout(cmdlineFeedbackTimer);
    if (text && !isError) {
      cmdlineFeedbackTimer = setTimeout(function() {
        if (cmdlineFeedbackEl()) cmdlineFeedbackEl().textContent = '';
        cmdlineFeedbackTimer = 0;
      }, 1800);
    }
  }
  function macrosDialogEl() { return document.getElementById('macros-dialog'); }
  function macrosDialogInputEl() { return document.getElementById('macros-dialog-input'); }
  function macrosDialogFeedbackEl() { return document.getElementById('macros-dialog-feedback'); }

  function setMacrosDialogFeedback(msg, ok) {
    var fb = macrosDialogFeedbackEl();
    if (!fb) return;
    fb.textContent = msg || '';
    fb.classList.toggle('ok', !!ok);
  }

  function openMacrosDialog() {
    var dlg = macrosDialogEl();
    if (!dlg || typeof dlg.showModal !== 'function') return;
    setMacrosDialogFeedback('', false);
    syncMacrosMode();
    syncMacrosCustomPathEnabled();
    // Pre-fill the editor with the active mode's file so you edit rather than
    // start blank. Only when empty, so reopening after a cancel doesn't
    // clobber unsubmitted text.
    reloadActiveScopeFile();
    dlg.showModal();
    setTimeout(function() {
      var input = macrosDialogInputEl();
      if (input) input.focus();
    }, 0);
  }

  function currentMacroMode() {
    var r = document.querySelector('input[name="macro-mode"]:checked');
    return r ? r.value : 'tex';
  }

  // Scope + custom path without raising feedback (used for read/prefill).
  function currentScopeSelection() {
    var r = document.querySelector('input[name="scope"]:checked');
    var scope = r ? r.value : 'project';
    var path = null;
    if (scope === 'custom') {
      var pe = document.getElementById('macros-dialog-custom-path');
      path = (pe && (pe.value || '').trim()) || null;
    }
    return { scope: scope, path: path };
  }

  // Load the current scope's macros file into the editor (TeX mode only).
  // Skips when the textarea already has unsaved text (unless `force`), or for
  // a custom scope with no path yet. Failures are silent.
  async function loadMacrosForScope(force) {
    if (currentMacroMode() !== 'tex') return;
    var input = macrosDialogInputEl();
    if (!input) return;
    if (!force && (input.value || '').trim()) return;
    var sc = currentScopeSelection();
    if (sc.scope === 'custom' && !sc.path) return;
    try {
      var payload = { scope: sc.scope };
      if (sc.path) payload.path = sc.path;
      var res = await fetch('/macros/read', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload),
      });
      if (!res.ok) return;
      var body = await res.json();
      input.value = (body && body.content) || '';
      if (body && body.exists) {
        setMacrosDialogFeedback('Loaded existing macros — Save replaces the file.', true);
      }
    } catch (e) { /* non-fatal */ }
  }

  // Text→HTML mode: load the config TOML file into its editor (same
  // empty/force rules as the TeX loader).
  async function loadTomlForScope(force) {
    if (currentMacroMode() !== 'html') return;
    var input = document.getElementById('macros-toml-input');
    if (!input) return;
    if (!force && (input.value || '').trim()) return;
    var sc = currentScopeSelection();
    if (sc.scope === 'custom' && !sc.path) return;
    try {
      var payload = { scope: sc.scope };
      if (sc.path) payload.path = sc.path;
      var res = await fetch('/config/read', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload),
      });
      if (!res.ok) return;
      var body = await res.json();
      input.value = (body && body.content) || '';
      if (body && body.exists) {
        setMacrosDialogFeedback('Loaded config — Save replaces the file.', true);
      }
    } catch (e) { /* non-fatal */ }
  }

  // Load whichever file the active mode edits (.tex for TeX, .toml for HTML).
  function reloadActiveScopeFile(force) {
    if (currentMacroMode() === 'html') loadTomlForScope(force);
    else loadMacrosForScope(force);
  }

  // Toggle the dialog between "TeX macro" (\newcommand → .tex) and
  // "Text → HTML" (a [text-macros] template → .toml), and swap the scope
  // filenames / custom-path placeholder to match.
  function syncMacrosMode() {
    var mode = currentMacroMode();
    var html = mode === 'html';
    var texEl = document.getElementById('macros-mode-tex');
    var htmlEl = document.getElementById('macros-mode-html');
    if (texEl) texEl.hidden = html;
    if (htmlEl) htmlEl.hidden = !html;
    var pathInput = document.getElementById('macros-dialog-custom-path');
    if (pathInput) {
      pathInput.placeholder = html ? '~/my.toml or extras/config.toml'
                                   : '~/my-macros.tex or extras/macros.tex';
    }
    updateMacrosScopeFile();
    setMacrosDialogFeedback('', false);
    // Prefill the file for whichever mode we just switched into.
    reloadActiveScopeFile();
  }

  // Show the resolved file name for the active tab (or reveal the path input
  // for the Custom tab), matching the current TeX/HTML mode.
  function updateMacrosScopeFile() {
    var html = currentMacroMode() === 'html';
    var sc = currentScopeSelection();
    var isCustom = sc.scope === 'custom';
    var code = document.getElementById('macros-scope-file');
    var path = document.getElementById('macros-dialog-custom-path');
    if (code) {
      code.hidden = isCustom;
      if (!isCustom) {
        var f;
        if (sc.scope === 'global') {
          f = html ? '~/.config/mathpreview/config.toml' : '~/.config/mathpreview/macros.tex';
        } else {
          f = html ? '.mathpreview.toml' : '.mathpreview-macros.tex';
        }
        code.textContent = '→ ' + f;
      }
    }
    if (path) path.hidden = !isCustom;
  }

  // The Custom tab reveals + enables its path input; other tabs hide it.
  function syncMacrosCustomPathEnabled() {
    var pathInput = document.getElementById('macros-dialog-custom-path');
    if (!pathInput) return;
    var radio = document.querySelector('input[name="scope"]:checked');
    var isCustom = radio && radio.value === 'custom';
    pathInput.disabled = !isCustom;
    pathInput.hidden = !isCustom;
    updateMacrosScopeFile();
    if (isCustom) {
      setTimeout(function() { pathInput.focus(); }, 0);
    }
  }

  function loadMacrosDialogFile() {
    var picker = document.getElementById('macros-dialog-file');
    if (picker) picker.click();
  }

  // Hand the daemon a file path to *use* as a live override layer
  // without copying its contents. The browser file picker only gives
  // us the file name (security), not an absolute path, so this works
  // off the custom-path text input — the user types where the file
  // actually lives on their filesystem.
  async function registerMacrosOverride() {
    var pathInput = document.getElementById('macros-dialog-custom-path');
    var path = pathInput && (pathInput.value || '').trim();
    if (!path) {
      setMacrosDialogFeedback(
        'Type the file\'s filesystem path in the "Custom path" field, then click "Use as override".',
        false
      );
      return;
    }
    setMacrosDialogFeedback('Registering…', true);
    try {
      var res = await fetch('/macros/register', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ path: path }),
      });
      var body = null;
      try { body = await res.json(); } catch (e) {}
      if (!res.ok) {
        setMacrosDialogFeedback((body && body.error) || 'register failed', false);
        return;
      }
      var file = body && body.file ? body.file : path;
      setStatus('live', '● using ' + file + ' as override');
      setMacrosDialogFeedback('Registered ' + file, true);
    } catch (e) {
      setMacrosDialogFeedback(String(e && e.message || e), false);
    }
  }

  async function onMacrosFilePicked(e) {
    var file = e.target.files && e.target.files[0];
    if (!file) return;
    try {
      var text = await file.text();
      var input = macrosDialogInputEl();
      if (input) {
        // Append rather than replace so the user doesn't lose what
        // they've typed; gives them control.
        input.value = input.value
          ? input.value.replace(/\s+$/, '') + '\n' + text
          : text;
      }
      // Pre-populate the custom-path input so "Save" with scope=custom
      // writes back to the same file they just loaded — common
      // workflow when editing an existing override.
      var pathInput = document.getElementById('macros-dialog-custom-path');
      if (pathInput && file.name) pathInput.value = file.name;
      setMacrosDialogFeedback('Loaded ' + file.name, true);
    } catch (err) {
      setMacrosDialogFeedback('Read failed: ' + (err && err.message || err), false);
    }
    e.target.value = '';
  }

  function closeMacrosDialog() {
    var dlg = macrosDialogEl();
    if (!dlg) return;
    if (dlg.open) dlg.close();
  }

  // Resolve the chosen scope + custom path, or null if a required custom
  // path is missing (after showing feedback).
  function macrosDialogScope() {
    var scopeInput = document.querySelector('input[name="scope"]:checked');
    var scope = scopeInput ? scopeInput.value : 'project';
    var path = null;
    if (scope === 'custom') {
      var pathInput = document.getElementById('macros-dialog-custom-path');
      path = pathInput && (pathInput.value || '').trim();
      if (!path) {
        setMacrosDialogFeedback('Custom path is required for scope=custom.', false);
        return null;
      }
    }
    return { scope: scope, path: path };
  }

  async function submitMacrosDialog() {
    if (currentMacroMode() === 'html') {
      return submitTomlEdit();
    }
    var input = macrosDialogInputEl();
    if (!input) return;
    var line = (input.value || '').trim();
    if (!line) {
      setMacrosDialogFeedback('Enter a \\newcommand line first.', false);
      return;
    }
    var sc = macrosDialogScope();
    if (!sc) return;
    // The editor holds the whole file (we pre-load it), so save replaces
    // rather than appends — otherwise re-saving would duplicate every line.
    var payload = { scope: sc.scope, line: line, replace: true };
    if (sc.path) payload.path = sc.path;
    setMacrosDialogFeedback('Saving…', true);
    try {
      var res = await fetch('/macros/append', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload),
      });
      var body = null;
      try { body = await res.json(); } catch (e) {}
      if (!res.ok) {
        setMacrosDialogFeedback((body && body.error) || 'save failed', false);
        return;
      }
      var file = body && body.file ? ' → ' + body.file : '';
      setStatus('live', '● saved macros' + file);
      // Keep the editor content (it now matches the file on disk).
      closeMacrosDialog();
    } catch (e) {
      setMacrosDialogFeedback(String(e && e.message || e), false);
    }
  }

  // "Text → HTML" mode: write the edited config TOML back via /config/write
  // (whole-file replace, validated as TOML server-side).
  async function submitTomlEdit() {
    var input = document.getElementById('macros-toml-input');
    if (!input) return;
    var sc = macrosDialogScope();
    if (!sc) return;
    var payload = { scope: sc.scope, content: input.value || '' };
    if (sc.path) payload.path = sc.path;
    setMacrosDialogFeedback('Saving…', true);
    try {
      var res = await fetch('/config/write', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload),
      });
      var body = null;
      try { body = await res.json(); } catch (e) {}
      if (!res.ok) {
        setMacrosDialogFeedback((body && body.error) || 'save failed', false);
        return;
      }
      var file = body && body.file ? ' → ' + body.file : '';
      setStatus('live', '● saved config' + file);
      // Keep the editor content (it now matches the file on disk).
      closeMacrosDialog();
    } catch (e) {
      setMacrosDialogFeedback(String(e && e.message || e), false);
    }
  }

  // Quote a value for TOML. A single-quoted literal allows any char except an
  // apostrophe and control characters (tab excepted). Anything else — including
  // a stray CR, form feed, or other control char — must be a basic
  // (double-quoted) string with escapes, or the server's TOML parse rejects it.
  function tomlQuote(s) {
    var needsBasic = false;
    for (var i = 0; i < s.length; i++) {
      var c = s.charCodeAt(i);
      if (c === 39 || (c < 32 && c !== 9) || c === 127) { needsBasic = true; break; }
    }
    if (!needsBasic) return "'" + s + "'";
    var out = '"';
    for (var j = 0; j < s.length; j++) {
      var ch = s[j], cc = s.charCodeAt(j);
      if (ch === '\\') out += '\\\\';
      else if (ch === '"') out += '\\"';
      else if (cc === 10) out += '\\n';
      else if (cc === 13) out += '\\r';
      else if (cc === 9) out += '\\t';
      else if (cc < 32 || cc === 127) out += '\\u' + cc.toString(16).padStart(4, '0');
      else out += ch;
    }
    return out + '"';
  }

  // Insert `line` at the end of the [text-macros] table in `text`, creating
  // the table if it isn't there yet.
  function insertUnderTextMacros(text, line) {
    var lines = text.split('\n');
    var hdr = -1;
    for (var i = 0; i < lines.length; i++) {
      if (/^\s*\[\s*text[-_]macros\s*\]\s*$/.test(lines[i])) { hdr = i; break; }
    }
    if (hdr === -1) {
      var prefix = text.replace(/\s+$/, '');
      return (prefix ? prefix + '\n\n' : '') + '[text-macros]\n' + line + '\n';
    }
    var nextTable = lines.length;
    for (var j = hdr + 1; j < lines.length; j++) {
      if (/^\s*\[/.test(lines[j])) { nextTable = j; break; }
    }
    var at = nextTable;
    while (at > hdr + 1 && lines[at - 1].trim() === '') at--;
    lines.splice(at, 0, line);
    return lines.join('\n');
  }

  // "Add" button in Text→HTML mode: build a [text-macros] line from the
  // name/template inputs and insert it into the editor (the user then Saves).
  function addTextMacroFromForm() {
    var nameEl = document.getElementById('macros-html-name');
    var tplEl = document.getElementById('macros-html-template');
    var editor = document.getElementById('macros-toml-input');
    if (!nameEl || !tplEl || !editor) return;
    var name = (nameEl.value || '').trim().replace(/^\\+/, '');
    var tpl = (tplEl.value || '').trim();
    if (!name) {
      setMacrosDialogFeedback('Enter a command name.', false);
      return;
    }
    if (!/^[A-Za-z@]+$/.test(name)) {
      setMacrosDialogFeedback('Name must be letters only — no backslash, digits, or spaces.', false);
      return;
    }
    if (!tpl) {
      setMacrosDialogFeedback('Enter an HTML template (use #1, #2 for arguments).', false);
      return;
    }
    editor.value = insertUnderTextMacros(editor.value, name + ' = ' + tomlQuote(tpl));
    nameEl.value = '';
    tplEl.value = '';
    setMacrosDialogFeedback('Added \\' + name + ' to the editor — click Save to write it.', true);
    nameEl.focus();
  }

  function configDialogEl() { return document.getElementById('config-dialog'); }
  function configFeedbackEl() { return document.getElementById('config-dialog-feedback'); }

  function setConfigFeedback(msg, ok) {
    var fb = configFeedbackEl();
    if (!fb) return;
    fb.textContent = msg || '';
    fb.classList.toggle('ok', !!ok);
  }

  function currentConfigMode() {
    var radio = document.querySelector('input[name="config-mode"]:checked');
    return radio ? radio.value : 'viewer';
  }

  function currentConfigScope() {
    var radio = document.querySelector('input[name="config-scope"]:checked');
    var scope = radio ? radio.value : 'project';
    var path = null;
    if (scope === 'custom') {
      var pathInput = document.getElementById('config-dialog-custom-path');
      path = pathInput && (pathInput.value || '').trim() || null;
    }
    return { scope: scope, path: path };
  }

  function viewerConfigEditorEl() {
    return document.getElementById('config-viewer-toml');
  }

  var configLoadSeq = 0;

  function withDefaultKeybindings(content, defaultConfig) {
    var marker = defaultConfig.indexOf('[keybindings]');
    if (marker < 0) return content;
    var start = defaultConfig.lastIndexOf('\n# One shortcut', marker);
    if (start < 0) start = marker;
    else start += 1;
    var keybindings = defaultConfig.slice(start).trim();
    var tableMatch = /^\s*\[keybindings\]\s*(?:#.*)?$/m.exec(content);
    if (!tableMatch) {
      return content.replace(/[\r\n]+$/, '') + '\n\n' + keybindings + '\n';
    }

    // Complete a partial table without touching any overrides already there.
    // The keybindings table is flat, so line-level insertion is sufficient and
    // avoids reserializing the user's comments before the server validates it.
    var bodyStart = tableMatch.index + tableMatch[0].length;
    var tail = content.slice(bodyStart);
    var nextTable = /^\s*\[[^\]\r\n]+\]\s*(?:#.*)?$/m.exec(tail);
    var bodyEnd = nextTable ? bodyStart + nextTable.index : content.length;
    var existing = {};
    content.slice(bodyStart, bodyEnd).split(/\r?\n/).forEach(function(line) {
      var match = /^\s*["']?([A-Za-z0-9-]+)["']?\s*=/.exec(line);
      if (match) existing[match[1]] = true;
    });
    var missing = [];
    defaultConfig.slice(marker).split(/\r?\n/).forEach(function(line) {
      var match = /^\s*([A-Za-z0-9-]+)\s*=/.exec(line);
      if (match && !existing[match[1]]) missing.push(line);
    });
    if (!missing.length) return content;
    var before = content.slice(0, bodyEnd).replace(/[\r\n]+$/, '');
    var after = content.slice(bodyEnd);
    return before + '\n\n# Unlisted built-in bindings (editable).\n'
      + missing.join('\n') + '\n' + after;
  }

  // The rendered textarea starts with the complete built-in config. Preserve
  // that text once, then replace the editor with the selected scope's file
  // when it exists. This makes an absent project/global file immediately
  // useful without hiding an existing partial config behind generated values.
  function prefillConfigDialog() {
    var cfg = window.__mpConfig || {};
    var editor = viewerConfigEditorEl();
    if (editor && editor._defaultConfig === undefined) {
      editor._defaultConfig = editor.value;
    }
    var fontSize = parseInt(
      getComputedStyle(document.documentElement).getPropertyValue('--body-font-size'),
      10
    );
    if (!isFinite(fontSize) || fontSize <= 0) fontSize = 18;
    var fs = document.getElementById('config-font-size');
    if (fs) fs.value = String(fontSize);
    var uiFontSize = parseInt(
      getComputedStyle(document.documentElement).getPropertyValue('--ui-font-size'),
      10
    );
    if (!isFinite(uiFontSize) || uiFontSize <= 0) uiFontSize = 12;
    var uifs = document.getElementById('config-ui-font-size');
    if (uifs) uifs.value = String(uiFontSize);
    // Empty keeps the selected file's existing TOML value, or keeps following
    // document geometry when it has no explicit page-margin setting.
    var pm = document.getElementById('config-page-margin');
    if (pm) {
      pm.value = '';
      var eff = cfg.pageMarginMm;
      pm.placeholder = (typeof eff === 'number')
        ? 'unchanged (now ' + eff + 'mm)'
        : 'auto (default ~17mm)';
    }
    var trig = document.getElementById('config-source-jump-trigger');
    if (trig) trig.value = cfg.sourceJumpTrigger || 'cmd-click';
    var mode = document.getElementById('config-default-page-mode');
    if (mode) mode.value = cfg.defaultPageMode || 'a4';
    var theme = document.getElementById('config-default-theme');
    if (theme) theme.value = cfg.defaultTheme || 'system';
    var thmNum = document.getElementById('config-theorem-numbering');
    if (thmNum) thmNum.value = cfg.theoremNumbering || 'auto';
    var tsMode = document.getElementById('config-typeset-mode');
    if (tsMode) tsMode.value = cfg.typesetMode || 'local';
    var wrap = document.getElementById('config-wrap-equations');
    if (wrap) wrap.checked = cfg.wrapEquations !== false;
    var mjx = document.getElementById('config-mathjax-config');
    if (mjx) {
      mjx.value = cfg.mathjaxConfig || '';
      mjx.dataset.initial = mjx.value;   // so submit only writes it if changed
    }
    // Read-only view of the full generated `window.MathJax = {…}` config (the
    // text of the engine's config <script>), so you can see everything that's
    // in effect — macros, packages, output settings — and copy/tweak it.
    var cur = document.getElementById('config-mathjax-current');
    var src = document.getElementById('mp-mathjax-config');
    if (cur) cur.value = src ? (src.textContent || '').replace(/^\s+|\s+$/g, '') : '(not available)';
  }

  async function loadViewerConfigForScope(force) {
    if (currentConfigMode() !== 'viewer') return;
    var editor = viewerConfigEditorEl();
    if (!editor) return;
    var sc = currentConfigScope();
    if (sc.scope === 'custom' && !sc.path) {
      setConfigFeedback('Enter a custom path to load or save that config.', false);
      return;
    }
    var scopeKey = sc.scope + ':' + (sc.path || '');
    if (!force && editor.dataset.loadedScope === scopeKey) return;
    var seq = ++configLoadSeq;
    setConfigFeedback('Loading config…', true);
    try {
      var payload = { scope: sc.scope };
      if (sc.path) payload.path = sc.path;
      var res = await fetch('/config/read', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload),
      });
      var body = null;
      try { body = await res.json(); } catch (e) {}
      if (seq !== configLoadSeq) return;
      if (!res.ok) {
        setConfigFeedback((body && body.error) || 'config load failed', false);
        return;
      }
      var existingContent = body && body.exists ? (body.content || '') : '';
      var preparedContent = withDefaultKeybindings(
        existingContent,
        editor._defaultConfig || ''
      );
      var addedKeybindings = body && body.exists && preparedContent !== existingContent;
      editor.value = body && body.exists
        ? preparedContent
        : (editor._defaultConfig || '');
      editor.dataset.loadedScope = scopeKey;
      var file = body && body.file ? ' — ' + body.file : '';
      setConfigFeedback(
        body && body.exists
          ? 'Loaded existing config' + file + (addedKeybindings
            ? ' and completed the default keybindings in this unsaved draft.'
            : '. Save updates this file.')
          : 'No config at this scope' + file + ' — showing built-in defaults.',
        true
      );
    } catch (e) {
      if (seq === configLoadSeq) {
        setConfigFeedback(String(e && e.message || e), false);
      }
    }
  }

  function syncConfigMode(forceReload) {
    // Switching away cancels an in-flight scope read so it cannot overwrite
    // the other tab's feedback or a newer draft.
    configLoadSeq++;
    var mode = currentConfigMode();
    var viewerPanel = document.getElementById('config-mode-viewer');
    var mathjaxPanel = document.getElementById('config-mode-mathjax');
    if (viewerPanel) viewerPanel.hidden = mode !== 'viewer';
    if (mathjaxPanel) mathjaxPanel.hidden = mode !== 'mathjax';
    var save = document.getElementById('config-dialog-save');
    if (save) save.textContent = mode === 'viewer' ? 'Save viewer config' : 'Save MathJax config';
    setConfigFeedback('', false);
    if (mode === 'viewer') loadViewerConfigForScope(!!forceReload);
  }

  function syncConfigCustomPathEnabled() {
    var pathInput = document.getElementById('config-dialog-custom-path');
    if (!pathInput) return;
    var radio = document.querySelector('input[name="config-scope"]:checked');
    var isCustom = radio && radio.value === 'custom';
    pathInput.disabled = !isCustom;
    if (isCustom) {
      setTimeout(function() { pathInput.focus(); }, 0);
    }
  }

  function openConfigDialog() {
    var dlg = configDialogEl();
    if (!dlg || typeof dlg.showModal !== 'function') return;
    setConfigFeedback('', false);
    prefillConfigDialog();
    syncConfigCustomPathEnabled();
    syncConfigMode(true);
    dlg.showModal();
    setTimeout(function() {
      var target = currentConfigMode() === 'viewer'
        ? document.getElementById('config-font-size')
        : document.getElementById('config-mathjax-config');
      if (target) target.focus();
    }, 0);
  }

  function closeConfigDialog() {
    configLoadSeq++;
    var dlg = configDialogEl();
    if (dlg && dlg.open) dlg.close();
  }

  async function submitConfigDialog() {
    var sc = currentConfigScope();
    if (sc.scope === 'custom' && !sc.path) {
      setConfigFeedback('Custom path is required for scope=custom.', false);
      return;
    }
    if (currentConfigMode() === 'viewer') {
      var editor = viewerConfigEditorEl();
      if (!editor) return;
      var values = {};
      var fontSize = parseInt(document.getElementById('config-font-size').value, 10);
      if (isFinite(fontSize) && fontSize > 0) values['viewer.font-size'] = fontSize;
      var uiFontSize = parseInt(document.getElementById('config-ui-font-size').value, 10);
      if (isFinite(uiFontSize) && uiFontSize > 0) {
        values['viewer.ui-font-size'] = uiFontSize;
      }
      var pmEl = document.getElementById('config-page-margin');
      if (pmEl && pmEl.value.trim() !== '') {
        var pmVal = parseInt(pmEl.value, 10);
        if (!isFinite(pmVal) || pmVal < 5 || pmVal > 60) {
          setConfigFeedback('Page margin must be 5–60 mm (or empty to keep the TOML value).', false);
          return;
        }
        values['viewer.page-margin'] = pmVal;
      }
      var trig = document.getElementById('config-source-jump-trigger').value;
      if (trig) values['viewer.source-jump.trigger'] = trig;
      var mode = document.getElementById('config-default-page-mode').value;
      if (mode) values['viewer.default-page-mode'] = mode;
      var theme = document.getElementById('config-default-theme').value;
      if (theme) values['viewer.default-theme'] = theme;
      var thmNum = document.getElementById('config-theorem-numbering').value;
      if (thmNum) values['viewer.theorem-numbering'] = thmNum;
      var typesetMode = document.getElementById('config-typeset-mode').value;
      if (typesetMode) values['viewer.typeset-mode'] = typesetMode;
      var wrap = document.getElementById('config-wrap-equations');
      if (wrap) values['viewer.wrap-equations'] = !!wrap.checked;
      var writePayload = {
        scope: sc.scope,
        content: editor.value || '',
        values: values,
      };
      if (sc.path) writePayload.path = sc.path;
      setConfigFeedback('Validating and saving…', true);
      try {
        var writeRes = await fetch('/config/write', {
          method: 'POST',
          cache: 'no-store',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(writePayload),
        });
        var writeBody = null;
        try { writeBody = await writeRes.json(); } catch (e) {}
        if (!writeRes.ok) {
          setConfigFeedback((writeBody && writeBody.error) || 'save failed', false);
          return;
        }
        var writtenFile = writeBody && writeBody.file ? writeBody.file : '(file)';
        editor.dataset.loadedScope = sc.scope + ':' + (sc.path || '');
        setStatus('live', '● wrote viewer config → ' + writtenFile);
        closeConfigDialog();
      } catch (e) {
        setConfigFeedback(String(e && e.message || e), false);
      }
      return;
    }

    var payload = { scope: sc.scope, values: {} };
    if (sc.path) payload.path = sc.path;
    var mjxEl = document.getElementById('config-mathjax-config');
    // Only write it when changed, so opening the MathJax tab to inspect the
    // generated config does not inject an empty override into another scope.
    if (mjxEl && mjxEl.value !== (mjxEl.dataset.initial || '')) {
      payload.values['viewer.mathjax-config'] = mjxEl.value;
    }
    if (!Object.keys(payload.values).length) {
      setConfigFeedback('No MathJax override changes to save.', true);
      return;
    }
    setConfigFeedback('Saving…', true);
    try {
      var res = await fetch('/config/set', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload),
      });
      var body = null;
      try { body = await res.json(); } catch (e) {}
      if (!res.ok) {
        setConfigFeedback((body && body.error) || 'save failed', false);
        return;
      }
      var file = body && body.file ? body.file : '(file)';
      setStatus('live', '● wrote MathJax config → ' + file);
      closeConfigDialog();
      // The raw MathJax config lives in the page head. The re-render's
      // viewer_config push reloads when the value changed.
    } catch (e) {
      setConfigFeedback(String(e && e.message || e), false);
    }
  }

  // Apply a freshly-pushed viewer config snapshot from the daemon
  // without reloading the tab. `font_size` updates the CSS variable
  // that the body font reads from; `source_jump_trigger` updates the
  // `__mpConfig` object the click handlers consult; default page mode
  // / theme are init-only on the client (the user's localStorage
  // toggles already win for the current tab) so we just stash them
  // for new windows / private mode that read this tab's state.
  function applyViewerConfig(cfg) {
    if (!cfg) return;
    if (typeof cfg.font_size === 'number') {
      var nextFontSize = cfg.font_size + 'px';
      if (document.documentElement.style.getPropertyValue('--body-font-size') !==
          nextFontSize) {
        document.documentElement.style.setProperty('--body-font-size', nextFontSize);
        // Font size changes line heights/wrapping and key chip geometry, so both
        // page-level overlay layers must re-measure before the next paint.
        invalidateOverlayMetrics();
        if (lineNumbersVisible) scheduleLineNumbers();
        if (refkeysVisible) scheduleRefkeys(0);
      }
    }
    if (typeof cfg.ui_font_size === 'number') {
      // Scales the toolbar (topbar) and the index/pages side panel (TOC);
      // independent of the document body font. Doesn't affect the rendered
      // paper, so no line-number re-measure is needed — but it changes the
      // toolbar's height, so re-anchor the floating side controls.
      document.documentElement.style.setProperty(
        '--ui-font-size', cfg.ui_font_size + 'px'
      );
      syncTopbarHeight();
    }
    window.__mpConfig = window.__mpConfig || {};
    if (cfg.source_jump_trigger) window.__mpConfig.sourceJumpTrigger = cfg.source_jump_trigger;
    if (cfg.default_page_mode)   window.__mpConfig.defaultPageMode  = cfg.default_page_mode;
    if (cfg.default_theme)       window.__mpConfig.defaultTheme     = cfg.default_theme;
    if (cfg.keybindings && typeof cfg.keybindings === 'object') {
      window.__mpConfig.keybindings = cfg.keybindings;
      setViewerKeybindings(cfg.keybindings);
    }
    // Equation wrapping (and any raw MathJax config) is baked into the <head>
    // at page load — live body pushes can't change it. So when the resolved
    // value flips, reload the page so the new MathJax config takes effect.
    // This covers every edit path: config dialog, the .mathpreview.toml file,
    // and the macros dialog's TOML editor.
    if (typeof cfg.wrap_equations === 'boolean') {
      var was = window.__mpConfig.wrapEquations;
      window.__mpConfig.wrapEquations = cfg.wrap_equations;
      if (was !== undefined && was !== cfg.wrap_equations) {
        location.reload();
        return;
      }
    }
    if (typeof cfg.mathjax_config === 'string') {
      var wasMjx = window.__mpConfig.mathjaxConfig;
      window.__mpConfig.mathjaxConfig = cfg.mathjax_config;
      if (wasMjx !== undefined && wasMjx !== cfg.mathjax_config) {
        location.reload();
        return;
      }
    }
    // Typeset mode is pure client behavior (which blocks to render), so it
    // applies live with no reload — switching to 'background' kicks the fill.
    if (typeof cfg.typeset_mode === 'string') {
      setTypesetMode(cfg.typeset_mode);
    }
    // The page margin is baked into config_css in the <head> (screen padding
    // + @page print margin), which a body patch can't touch — reload when the
    // effective value flips. null (default/no override) is a valid value, so
    // compare with a sentinel to catch default→set and set→default both.
    if ('page_margin_mm' in cfg) {
      var wasMargin = window.__mpConfig.pageMarginMm;
      window.__mpConfig.pageMarginMm = cfg.page_margin_mm;
      if (wasMargin !== undefined && wasMargin !== cfg.page_margin_mm) {
        location.reload();
        return;
      }
    }
  }

  function logPanelEl() { return document.getElementById('log-panel'); }

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
  }

  function renderLogState(snapshot) {
    var lines = [];
    function row(key, value, cls) {
      var v = escapeHtml(value);
      if (cls) v = '<span class="' + cls + '">' + v + '</span>';
      lines.push('<span class="log-key">' + escapeHtml(key) + ':</span> ' + v);
    }
    function section(title) {
      lines.push('<span class="log-section">' + escapeHtml(title) + '</span>');
    }
    section('viewer config (live)');
    var vc = snapshot.viewer_config || {};
    row('font-size', vc.font_size + ' px');
    row('ui-font-size', vc.ui_font_size + ' px');
    row('source-jump trigger', vc.source_jump_trigger);
    row('default page mode', vc.default_page_mode);
    row('default theme', vc.default_theme);
    row('WS protocol', snapshot.ws_protocol);
    section('editor template');
    lines.push(escapeHtml(snapshot.editor_cmd || '(none)'));
    section('config files (cascade order, last wins)');
    (snapshot.config_paths || []).forEach(function(c) {
      var marker = c.exists ? '<span class="log-ok">[applied]</span>'
                            : '<span class="log-bad">[missing]</span>';
      lines.push(marker + ' ' + escapeHtml(c.path));
    });
    section('macro override files');
    (snapshot.macro_paths || []).forEach(function(m) {
      var marker = m.exists ? '<span class="log-ok">[applied]</span>'
                            : '<span class="log-bad">[missing]</span>';
      var src = ' <span class="log-key">(' + escapeHtml(m.source) + ')</span>';
      lines.push(marker + src + ' ' + escapeHtml(m.path));
    });
    return lines.join('\n');
  }

  function renderLogEntries(snapshot) {
    var entries = snapshot.log || [];
    if (!entries.length) return '(no events yet)';
    return entries.map(function(e) {
      var d = new Date(e.ts_ms);
      var hh = String(d.getHours()).padStart(2, '0');
      var mm = String(d.getMinutes()).padStart(2, '0');
      var ss = String(d.getSeconds()).padStart(2, '0');
      var prefix = '[' + hh + ':' + mm + ':' + ss + ' ' + e.level + '] ';
      return escapeHtml(prefix + e.message);
    }).join('\n');
  }

  async function refreshLogPanel() {
    var stateEl = document.getElementById('log-panel-state');
    var entriesEl = document.getElementById('log-panel-entries');
    if (!stateEl || !entriesEl) return;
    try {
      var res = await fetch('/debug', { cache: 'no-store' });
      var snapshot = await res.json();
      stateEl.innerHTML = renderLogState(snapshot);
      entriesEl.textContent = renderLogEntries(snapshot);
      entriesEl.scrollTop = entriesEl.scrollHeight;
      // Sync the verbose checkbox with whatever the daemon reports —
      // covers the case where the user toggled it in another tab.
      var cb = document.getElementById('log-panel-verbose');
      if (cb) cb.checked = !!snapshot.debug_logging;
    } catch (e) {
      stateEl.textContent = 'fetch /debug failed: ' + (e && e.message || e);
    }
  }

  async function toggleLogVerbose(enabled) {
    try {
      await fetch('/debug/mode', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ enabled: !!enabled }),
      });
      refreshLogPanel();
    } catch (e) {
      console.warn('debug/mode:', e && e.message || e);
    }
  }

  // Toggling the panel also toggles the body class so the page can
  // shift right and keep the reading column visible alongside it
  // rather than buried underneath.
  function setLogPanelOpen(open) {
    var panel = logPanelEl();
    if (!panel) return;
    panel.toggleAttribute('hidden', !open);
    document.body.classList.toggle('log-panel-open', !!open);
    var btn = document.getElementById('log-toggle');
    if (btn) btn.classList.toggle('active', !!open);
    if (typeof updatePageScale === 'function') updatePageScale();
    if (open) refreshLogPanel();
  }

  function toggleLogPanel() {
    var panel = logPanelEl();
    if (!panel) return;
    setLogPanelOpen(panel.hasAttribute('hidden'));
  }

  function closeLogPanel() { setLogPanelOpen(false); }

  function refreshLogPanelIfOpen() {
    var panel = logPanelEl();
    if (panel && !panel.hasAttribute('hidden')) refreshLogPanel();
  }

  function openCmdline(initial) {
    var line = cmdlineEl();
    var input = cmdlineInputEl();
    if (!line || !input) return;
    line.hidden = false;
    input.value = initial || '';
    setCmdlineFeedback('', false);
    refreshCmdlineSuggestions();
    // Defer focus so the `:` that opened the cmdline (or any other
    // synthetic key from the dispatcher) isn't typed into the input.
    setTimeout(function() {
      input.focus();
      input.select();
    }, 0);
  }
  function closeCmdline() {
    var line = cmdlineEl();
    var input = cmdlineInputEl();
    if (line) line.hidden = true;
    if (input) {
      input.value = '';
      input.blur();
    }
    setCmdlineFeedback('', false);
    cmdlineSuggestions = [];
    cmdlineSuggestionIndex = -1;
    var strip = cmdlineSuggestionsEl();
    if (strip) {
      strip.hidden = true;
      strip.replaceChildren();
    }
  }
  function runCmd(raw) {
    var text = (raw || '').trim();
    if (!text) return closeCmdline();
    var space = text.indexOf(' ');
    var cmd = (space < 0 ? text : text.slice(0, space)).toLowerCase();
    var arg = space < 0 ? '' : text.slice(space + 1).trim();
    if (cmd === 'pin' || cmd === 'p') {
      if (!arg) {
        setCmdlineFeedback('usage: :pin <key>', true);
        return;
      }
      var r = pinByRefkey(arg);
      if (r.ok) {
        closeCmdline();
        if (r.reason === 'already-pinned') setCmdlineFeedback('already pinned', false);
      } else if (r.reason === 'not-found') {
        setCmdlineFeedback('no \\label by that name', true);
      } else {
        setCmdlineFeedback('pin failed: ' + r.reason, true);
      }
      return;
    }
    if (cmd === 'unpin' || cmd === 'u') {
      if (!arg) {
        setCmdlineFeedback('usage: :unpin <key>', true);
        return;
      }
      var u = unpinByRefkey(arg);
      if (u.ok) closeCmdline();
      else setCmdlineFeedback(u.reason === 'not-pinned' ? 'not pinned' : 'unpin failed', true);
      return;
    }
    if (cmd === 'clear') {
      closeAllMarginCards();
      closeCmdline();
      return;
    }
    if (cmd === 'q' || cmd === 'quit') {
      closeViewer(function(msg) { setCmdlineFeedback(msg, true); });
      return;
    }
    setCmdlineFeedback('unknown command: ' + cmd, true);
  }
  function initCmdline() {
    var input = cmdlineInputEl();
    if (!input || input.dataset.cmdlineInit === '1') return;
    input.dataset.cmdlineInit = '1';
    input.addEventListener('keydown', function(e) {
      if (e.key === 'Enter') {
        e.preventDefault();
        runCmd(input.value);
      } else if (e.key === 'Escape') {
        e.preventDefault();
        closeCmdline();
      } else if (e.key === 'Backspace' && input.value === '') {
        e.preventDefault();
        closeCmdline();
      } else if (e.key === 'Tab') {
        e.preventDefault();
        cycleCmdlineSuggestion(e.shiftKey ? -1 : 1);
      } else if (e.key === 'ArrowDown') {
        if (cmdlineSuggestions.length) {
          e.preventDefault();
          cycleCmdlineSuggestion(1);
        }
      } else if (e.key === 'ArrowUp') {
        if (cmdlineSuggestions.length) {
          e.preventDefault();
          cycleCmdlineSuggestion(-1);
        }
      }
    });
    input.addEventListener('input', function() {
      refreshCmdlineSuggestions();
    });
    // Clicking a suggestion chip commits it and runs the command.
    var strip = cmdlineSuggestionsEl();
    if (strip) {
      strip.addEventListener('mousedown', function(e) {
        // mousedown (not click) so the input doesn't lose focus first.
        var btn = e.target && e.target.closest && e.target.closest('.cmdline-suggestion');
        if (!btn) return;
        e.preventDefault();
        var idx = parseInt(btn.dataset.suggestionIndex, 10);
        if (isNaN(idx) || idx < 0) return;
        cmdlineSuggestionIndex = idx - 1; // cycle of +1 lands on idx
        cycleCmdlineSuggestion(1);
        runCmd(input.value);
      });
    }
    // Closing on blur would be nice but it fights with the user clicking
    // the feedback span / loose focus mid-edit. Stay open until Enter or
    // Esc explicitly closes.
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

  // The keys chips live in a PAGE-LEVEL layer (like the line-number gutter),
  // not inside the render blocks: blocks have paint containment
  // (content-visibility), which clips any ink outside their box — chips
  // hanging into the margin from inside a block rendered as sliver stubs.
  // The layer is a direct child of main#page, so nothing contains it; chips
  // are absolutely positioned at their anchor's measured y, right-aligned
  // into the real margin. Under crop there is no margin, so they overlay the
  // content edge instead (translucent via CSS). Their block-local positions
  // are cached before scrolling and the page-level layer is rebuilt from that
  // cache after edits/typesetting — zoom scales it without a rebuild.
  function clearRefkeyLayer() {
    var l = document.querySelector('main#page > .refkey-layer');
    if (l) l.remove();
  }

  function topLevelOverlayBlocks(page) {
    return Array.prototype.filter.call(page.children, function(el) {
      return el.classList && el.classList.contains('blk');
    });
  }

  // Drop cached block-local overlay geometry. With no argument every block is
  // invalidated (font/column-width changes); DOM/typeset mutations pass only
  // their affected blocks. Rebuild scheduling stays coalesced by the existing
  // line-number rAF and refkey timer.
  function invalidateOverlayMetrics(blocks, rebuild) {
    if (!blocks) {
      refkeyBlockMetrics = new WeakMap();
      lineNumberBlockMetrics = new WeakMap();
    } else {
      blocks.forEach(function(blk) {
        refkeyBlockMetrics.delete(blk);
        lineNumberBlockMetrics.delete(blk);
      });
    }
    if (rebuild !== false) {
      if (lineNumbersVisible) scheduleLineNumbers();
      if (refkeysVisible) scheduleRefkeys();
    }
  }

  // Measure missing overlay geometry before scrolling. We temporarily lift
  // content-visibility only for blocks with no cache, force one shared layout,
  // record block-local positions, then immediately restore containment. This
  // does NOT typeset math: patch.js ignores the synthetic un-skip while the
  // marker is present, so equations remain governed by typeset-mode. DOM,
  // MathJax, fold, font and width changes invalidate the affected cache before
  // this function measures it again.
  function ensureOverlayMetrics(page) {
    if (!refkeysVisible && !lineNumbersVisible) return;
    var blocks = topLevelOverlayBlocks(page);
    var targets = [];
    var lifted = [];
    var token = ++overlayPrelayoutToken;
    blocks.forEach(function(blk) {
      var missing = (refkeysVisible && !refkeyBlockMetrics.has(blk)) ||
        (lineNumbersVisible && !lineNumberBlockMetrics.has(blk));
      if (!missing) return;
      targets.push(blk);
      lifted.push({
        blk: blk,
        contentVisibility: blk.style.contentVisibility,
        contain: blk.style.contain,
      });
      blk.__mpOverlayPrelayoutToken = token;
      // `content-visibility:auto` implicitly supplies layout/style/paint
      // containment even while a block is active. Switching it to `visible`
      // removes that containment, which lets a heading's or display math's
      // vertical margins collapse through `.blk`. Measuring in that altered
      // layout cached both anchors and intrinsic heights above their real
      // positions. Preserve the active block's containment explicitly while
      // forcing its contents to lay out.
      blk.style.contain = 'layout style paint';
      blk.style.contentVisibility = 'visible';
    });
    if (!targets.length) return;

    // Flush once after every missing block has been lifted. Measuring blocks
    // one at a time would turn this into a layout-thrashing scroll substitute.
    void page.offsetHeight;
    var pageRect = page.getBoundingClientRect();
    var scale = page.offsetHeight > 0 ? pageRect.height / page.offsetHeight : 1;
    if (!isFinite(scale) || scale <= 0) scale = 1;
    var pageFontSize = parseFloat(getComputedStyle(page).fontSize) || 18;
    var chipFontSize = pageFontSize * (11 / 18);
    var chipHeight = chipFontSize * 1.35 + 4;
    var chipHalfHeight = chipHeight / 2;
    var chipStackStep = Math.ceil(chipHeight + 1);
    // `contain-intrinsic-size: auto 180px` does not learn a remembered size
    // while we override content-visibility to `visible` (not `auto`). Record
    // the exact box as an explicit fallback before restoring containment;
    // otherwise WebKit replaces 180px estimates with real block heights only
    // when scrolling or zooming activates them, shifting every later line.
    lifted.forEach(function(entry) {
      var rect = entry.blk.getBoundingClientRect();
      entry.intrinsicWidth = Math.max(1, rect.width / scale);
      entry.intrinsicHeight = Math.max(0, rect.height / scale);
    });
    targets.forEach(function(blk) {
      if (refkeysVisible) {
        refkeyBlockMetrics.set(
          blk,
          measureRefkeyBlock(blk, scale, chipHalfHeight, chipStackStep)
        );
      }
      if (lineNumbersVisible) {
        lineNumberBlockMetrics.set(blk, measureLineNumberBlock(blk, scale));
      }
    });
    lifted.forEach(function(entry) {
      // Use an explicit two-axis fallback instead of `auto <length>`: the
      // latter prefers WebKit's remembered size, which may still describe the
      // pre-edit block. A targeted cache invalidation remeasures and replaces
      // this inline value after content, font, fold, or column-width changes.
      entry.blk.style.setProperty(
        'contain-intrinsic-size',
        entry.intrinsicWidth.toFixed(3) + 'px ' +
          entry.intrinsicHeight.toFixed(3) + 'px'
      );
      entry.blk.style.contentVisibility = entry.contentVisibility;
      entry.blk.style.contain = entry.contain;
    });
    // contentvisibilityautostatechange is delivered asynchronously in WebKit.
    // Keep the marker through that delivery window so this cheap geometry pass
    // cannot accidentally opt every equation into eager MathJax rendering.
    setTimeout(function() {
      lifted.forEach(function(entry) {
        if (entry.blk.__mpOverlayPrelayoutToken === token) {
          delete entry.blk.__mpOverlayPrelayoutToken;
        }
      });
    }, 250);
  }

  // -1 = nothing pending, 0 = immediate (pre-paint) pending, 180 = trailing.
  var refkeysPending = -1;
  var refkeysTimer = 0;
  function scheduleRefkeys(delayMs) {
    // Two cadences. Render-path callers (patch apply, typeset landing,
    // nav refresh) take a trailing timer: one layout pass costs ~80ms of
    // forced layout on a long paper and every keystroke schedules twice
    // (patch, then its typeset ~40-80ms later) — the delay coalesces those
    // into ONE pass, where rAF would run at the next frame and do both.
    // Interactive geometry changes (crop/mode/zoom) pass 0: they snap the
    // page at the next paint, so the chips must move in the same frame
    // (rAF, pre-paint) or they sit visibly misplaced for the trailing
    // delay. An immediate request upgrades a pending trailing one.
    var delay = delayMs === 0 ? 0 : 180;
    if (refkeysPending === 0) return;
    if (refkeysPending > 0) {
      if (delay > 0) return;
      clearTimeout(refkeysTimer);
    }
    refkeysPending = delay;
    var run = function() {
      if (refkeysPending === -1) return;
      refkeysPending = -1;
      clearTimeout(refkeysTimer);
      refkeysTimer = 0;
      layoutRefkeys();
    };
    if (delay === 0) {
      // The timer covers background/hidden pages, where rAF freezes but
      // layout still matters when a keys toggle arrives via live config.
      requestAnimationFrame(run);
      refkeysTimer = setTimeout(run, 250);
    } else {
      refkeysTimer = setTimeout(run, 180);
    }
  }

  function measureRefkeyBlock(blk, scale, chipHalfHeight, chipStackStep) {
    var blockRect = blk.getBoundingClientRect();
    if (blockRect.width === 0 && blockRect.height === 0) return [];
    var entries = [];
    var anchors = [];
    if (blk.matches('[data-refkey]:not(.label-anchor)')) anchors.push(blk);
    blk.querySelectorAll('[data-refkey]:not(.label-anchor)').forEach(function(el) {
      anchors.push(el);
    });
    anchors.forEach(function(el) {
      var key = el.getAttribute('data-refkey');
      if (!key) return;
      var r = el.getBoundingClientRect();
      if (r.width === 0 && r.height === 0) return;
      var top = (r.top - blockRect.top) / scale + 1;
      if (el.classList.contains('math') && el.classList.contains('display')) {
        top = (r.top + r.height / 2 - blockRect.top) / scale - chipHalfHeight;
      } else if (el.classList.contains('thm')) {
        top += chipHalfHeight;
      }
      entries.push({ key: key, top: top });
    });

    var displays = [];
    if (blk.matches('.math.display')) displays.push(blk);
    blk.querySelectorAll('.math.display').forEach(function(math) {
      displays.push(math);
    });
    displays.forEach(function(math) {
      var list = math.querySelector('.eq-refkey-list');
      if (!list || !list.children.length) return;
      var rows = list.children;
      var mathRect = math.getBoundingClientRect();
      if (mathRect.width === 0 && mathRect.height === 0) return;
      var groups = mathRowGroups(math);
      for (var i = 0; i < rows.length; i++) {
        var rowChips = rows[i].querySelectorAll('.eq-refkey-chip[data-target]');
        if (!rowChips.length) continue;
        var y = null;
        var g = groups[i];
        if (g && g.getBoundingClientRect) {
          var gr = g.getBoundingClientRect();
          if (gr.height > 0) {
            y = (gr.top + gr.height / 2 - blockRect.top) / scale - chipHalfHeight;
          }
        }
        if (y == null) {
          y = (mathRect.top - blockRect.top +
            (i + 0.5) * (mathRect.height / rows.length)) / scale - chipHalfHeight;
        }
        for (var c = 0; c < rowChips.length; c++) {
          entries.push({
            key: rowChips[c].dataset.target,
            top: y + c * chipStackStep,
          });
        }
      }
    });
    return entries;
  }

  function layoutRefkeys() {
    var page = pageEl();
    if (!page) return;
    clearRefkeyLayer();
    if (!refkeysVisible) return;
    ensureOverlayMetrics(page);
    var pageRect = page.getBoundingClientRect();
    if (pageRect.width === 0) return;
    // #page is CSS-zoomed: rects are rendered coords, children position in
    // local coords — divide measured offsets by the scale (see the lineno
    // layer for the same dance). Computed paddingLeft is already local.
    var scale = page.offsetHeight > 0 ? pageRect.height / page.offsetHeight : 1;
    if (!isFinite(scale) || scale <= 0) scale = 1;
    var pageStyle = getComputedStyle(page);
    var padLeft = parseFloat(pageStyle.paddingLeft) || 0;
    var cropped = document.body.classList.contains('page-crop');
    var layer = document.createElement('div');
    layer.className = 'refkey-layer';
    function addChip(key, topLocal) {
      var chip = document.createElement('button');
      chip.type = 'button';
      chip.className = 'refkey-chip';
      chip.dataset.target = key;
      chip.title = 'click to pin ' + key + ' to margin';
      chip.textContent = key;
      chip.style.top = Math.round(topLocal) + 'px';
      if (cropped) {
        // No margin to sit in: overlay the content's left edge.
        chip.style.left = padLeft + 'px';
      } else {
        // Right-align the chip so it ends just before the text column,
        // whatever its width — no measuring needed. Long keys extend PAST
        // the paper's left edge into the gutter: reading the whole key
        // beats fitting the page (the layer hangs outside every block's
        // containment and the page doesn't clip its own overflow). The
        // clamp is the VIEWPORT's left edge — body's overflow-x clip cuts
        // there silently, so cap at the real gutter and let the ellipsis
        // signal truncation in narrow windows (pageRect.left is rendered
        // px; the chip lays out in zoom-local px).
        chip.style.left = (padLeft - 8) + 'px';
        chip.style.transform = 'translateX(-100%)';
        chip.style.maxWidth =
          Math.max(24, padLeft - 8 + Math.max(0, pageRect.left) / scale - 4) + 'px';
      }
      layer.appendChild(chip);
    }
    // Positions are cached relative to their top-level block. The browser can
    // keep that block skipped after this pass; its page-level chips are already
    // present and only need the block's cheap outer rect to stay aligned.
    topLevelOverlayBlocks(page).forEach(function(blk) {
      var blockRect = blk.getBoundingClientRect();
      if (blockRect.width === 0 && blockRect.height === 0) return;
      var blockTop = (blockRect.top - pageRect.top) / scale;
      var entries = refkeyBlockMetrics.get(blk) || [];
      entries.forEach(function(entry) {
        addChip(entry.key, blockTop + entry.top);
      });
    });
    if (layer.children.length) page.appendChild(layer);
  }

  // Kept as the render-path entry point (patch/body-updated call it with the
  // touched root): the layer is measured from live geometry, so any render
  // just means "rebuild the layer".
  function decorateRefkeyChips(_root) {
    scheduleRefkeys();
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

  // Keep the `--topbar-height` layout variable in sync with the toolbar's
  // ACTUAL rendered height. The floating side controls anchor to it — the toc
  // pill (`top: calc(var(--topbar-height) + 4px)`), the index/pages side panel,
  // the search panel, and the margin column. The toolbar's height is
  // content-driven and now varies with the `ui-font-size` knob (and with
  // responsive wrapping), so a hard-coded constant would let those controls
  // drift over/under the toolbar's real bottom. Measuring is cheap and has no
  // feedback loop: the variable doesn't affect the toolbar's own height.
  function syncTopbarHeight() {
    var topbar = document.querySelector('.topbar');
    if (!topbar) return;
    // While the banner is hidden it has zero height — leave the variable at its
    // last visible value so the side controls keep their place (the dedicated
    // `body.topbar-hidden` rules handle the page-shell / margin offsets).
    if (topbarHidden || document.body.classList.contains('topbar-hidden')) return;
    var h = Math.round(topbar.getBoundingClientRect().height);
    if (h > 0) {
      document.documentElement.style.setProperty('--topbar-height', h + 'px');
    }
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
    syncTopbarHeight();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
    if (lineNumbersVisible) scheduleLineNumbers();
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
    invalidateOverlayMetrics();
    updatePageScale();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
    if (lineNumbersVisible) scheduleLineNumbers();
    if (refkeysVisible) scheduleRefkeys(0);
  }

  // Crop-to-content ("c", TeXpresso-style): trim the paper margins around the
  // text column so the reading area is all text. The column width — and thus
  // line wrapping — is unchanged: CSS shrinks the padding and updatePageScale
  // shrinks the page by the same amount (CROP_DX).
  function setPageCrop(on, persist) {
    pageCropped = !!on;
    document.body.classList.toggle('page-crop', pageCropped);
    var cropBtn = document.getElementById('crop-toggle');
    if (cropBtn) {
      cropBtn.classList.toggle('active', pageCropped);
      cropBtn.setAttribute('aria-pressed', pageCropped ? 'true' : 'false');
    }
    if (persist) {
      try { localStorage.setItem('mathpreview.crop', pageCropped ? '1' : '0'); } catch (e) {}
    }
    updatePageScale();
    syncTopbarHeight();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
    if (lineNumbersVisible) scheduleLineNumbers();
    if (refkeysVisible) scheduleRefkeys(0);
  }

  function pageScalePlan(userZoom) {
    var vw = document.documentElement.clientWidth || 0;
    var available = Math.max(320, vw - 32);
    var cropDx = cropDxNow();
    if (currentPageMode === 'a4') {
      var baseW = A4_CSS_WIDTH - cropDx;
      var pageScale = Math.min(1, available / baseW) * userZoom;
      return {
        viewportWidth: vw,
        pageScale: pageScale,
        shellWidth: baseW * pageScale,
        naturalWidth: null,
      };
    }

    // Dynamic mode keeps one natural column width and scales that rendered
    // geometry as a whole. Making the natural width inversely proportional to
    // userZoom reflowed the text at every zoom commit, forcing line-number and
    // refkey reconstruction and changing the visible line breaks. A viewport
    // resize can still change this width and legitimately reflow the column.
    var naturalWidth = Math.min(DYNAMIC_BASE_WIDTH, available);
    var pageW = naturalWidth - cropDx;
    return {
      viewportWidth: vw,
      pageScale: userZoom,
      shellWidth: pageW * userZoom,
      naturalWidth: pageW,
    };
  }

  // Return the text character rect nearest a viewport point, provided it lives
  // in the paper's reading content. Both APIs are needed: caretRangeFromPoint
  // is WebKit's long-standing spelling; caretPositionFromPoint is standard.
  function textRectAtPoint(page, x, y) {
    var node = null;
    var offset = 0;
    if (document.caretPositionFromPoint) {
      var pos = document.caretPositionFromPoint(x, y);
      if (pos) {
        node = pos.offsetNode;
        offset = pos.offset;
      }
    } else if (document.caretRangeFromPoint) {
      var caret = document.caretRangeFromPoint(x, y);
      if (caret) {
        node = caret.startContainer;
        offset = caret.startOffset;
      }
    }
    if (!node || node.nodeType !== Node.TEXT_NODE || !page.contains(node)) return null;
    var parent = node.parentElement;
    if (!parent || parent.closest(
      '.lineno-layer, .refkey-layer, .flash-layer, .sidenote, .margin-card'
    )) return null;
    var value = node.nodeValue || '';
    if (!value.length || !/\S/.test(value)) return null;
    var index = Math.max(0, Math.min(value.length - 1, offset));
    if (/\s/.test(value.charAt(index))) {
      var found = -1;
      for (var d = 1; d <= 24 && found < 0; d++) {
        if (index + d < value.length && /\S/.test(value.charAt(index + d))) found = index + d;
        else if (index - d >= 0 && /\S/.test(value.charAt(index - d))) found = index - d;
      }
      if (found < 0) return null;
      index = found;
    }
    var range = document.createRange();
    range.setStart(node, index);
    range.setEnd(node, index + 1);
    var rect = range.getBoundingClientRect();
    if (!(rect.height > 0)) return null;
    return { rect: rect, parent: parent };
  }

  // Find the first real text line at the reading boundary. elementFromPoint
  // returns the paragraph box even in inter-line whitespace; anchoring that
  // blank coordinate keeps the paragraph but lets its top line drift as it is
  // scaled. Sampling caret rects makes the visible character line authoritative.
  function firstVisibleTextAnchor(page, pageRect, readingTop, vh, vw, viewportX) {
    var xs = [
      viewportX,
      pageRect.left + pageRect.width * 0.25,
      pageRect.left + pageRect.width * 0.75,
      pageRect.left + 80,
      pageRect.right - 80,
    ];
    var best = null;
    var maxY = Math.min(Math.max(0, vh - 1), readingTop + 96);
    for (var y = readingTop; y <= maxY; y += 6) {
      for (var i = 0; i < xs.length; i++) {
        var x = Math.max(0, Math.min(Math.max(0, vw - 1), xs[i]));
        var hit = textRectAtPoint(page, x, y);
        if (!hit || hit.rect.bottom < readingTop - 1 || hit.rect.top > maxY) continue;
        if (!best || hit.rect.top < best.rect.top) best = hit;
      }
      if (best && best.rect.top <= y + 1) break;
    }
    return best;
  }

  // Keep the reading boundary immediately below the topbar stationary while
  // zooming. Browser engines refine it to the first visible text line because
  // committed CSS-zoom layout can settle differently.
  // Clamp to a paper edge when that edge is visible. Coordinates live in the
  // page's unzoomed local space so one anchor survives the whole preview burst.
  function captureZoomAnchor(page) {
    var rect = page.getBoundingClientRect();
    var heightScale = rect.height / Math.max(page.offsetHeight, 1);
    var widthScale = rect.width / Math.max(page.offsetWidth, 1);
    var vh = window.innerHeight || 0;
    var vw = document.documentElement.clientWidth || window.innerWidth || 0;
    var readingTop = Math.max(0, Math.min(vh, topbarOffset()));
    var viewportY = Math.min(vh, readingTop + 1);
    viewportY = Math.max(rect.top, Math.min(rect.bottom, viewportY));
    var viewportX = Math.max(rect.left, Math.min(rect.right, vw / 2));

    var textAnchor = firstVisibleTextAnchor(
      page, rect, readingTop, vh, vw, viewportX
    );
    if (textAnchor) viewportY = textAnchor.rect.top;

    // CSS-zoom engines retain the live source element under the reading line
    // in both page modes. Dynamic can reflow it; A4 keeps its wrapping but
    // content-visibility can still replace estimated heights above it during
    // real zoom layout. The page-local point remains the fallback.
    var hitY = Math.max(0, Math.min(Math.max(0, vh - 1), viewportY));
    var hitX = Math.max(0, Math.min(Math.max(0, vw - 1), viewportX));
    var hit = textAnchor ? textAnchor.parent : document.elementFromPoint(hitX, hitY);
    var element = hit && page.contains(hit) && hit.closest ? hit.closest('[data-src]') : null;
    var elementRatioY = null;
    if (element && page.contains(element)) {
      var elementRect = element.getBoundingClientRect();
      if (elementRect.height > 1) {
        elementRatioY = Math.max(
          0,
          Math.min(1, (viewportY - elementRect.top) / elementRect.height)
        );
      }
    }
    return {
      viewportX: viewportX,
      viewportY: viewportY,
      localX: (viewportX - rect.left) / Math.max(widthScale, 1e-6),
      localY: (viewportY - rect.top) / Math.max(heightScale, 1e-6),
      element: element,
      elementRatioY: elementRatioY,
    };
  }

  function restoreZoomAnchor(page, anchor) {
    if (!anchor) return;
    var rect = page.getBoundingClientRect();
    var heightScale = rect.height / Math.max(page.offsetHeight, 1);
    var widthScale = rect.width / Math.max(page.offsetWidth, 1);
    var targetX = rect.left + anchor.localX * widthScale;
    var targetY = rect.top + anchor.localY * heightScale;
    // A browser CSS-zoom commit may settle differently, so retain the live
    // element anchor when possible.
    if (anchor.element && anchor.element.isConnected &&
        anchor.elementRatioY !== null) {
      var elementRect = anchor.element.getBoundingClientRect();
      if (elementRect.height > 1) {
        targetY = elementRect.top + anchor.elementRatioY * elementRect.height;
      }
    }
    var dx = targetX - anchor.viewportX;
    var dy = targetY - anchor.viewportY;
    if (Math.abs(dx) < 0.5) dx = 0;
    if (Math.abs(dy) < 0.5) dy = 0;
    if (dx || dy) window.scrollBy({ left: dx, top: dy, behavior: 'auto' });
  }

  function cancelZoomAnchorRestore() {
    if (zoomAnchorRestoreRaf) {
      cancelAnimationFrame(zoomAnchorRestoreRaf);
      zoomAnchorRestoreRaf = 0;
    }
    if (zoomAnchorVerifyRaf) {
      cancelAnimationFrame(zoomAnchorVerifyRaf);
      zoomAnchorVerifyRaf = 0;
    }
  }

  // CSS-zoom engines can expose final layout later, so the first rAF restores
  // after invalidation and the second catches a late layout/scroll-anchor
  // adjustment. A new key burst
  // cancels both so an old anchor cannot fight the next preview.
  function scheduleZoomAnchorRestore(page, anchor) {
    cancelZoomAnchorRestore();
    if (!anchor) return;
    zoomAnchorRestoreRaf = requestAnimationFrame(function() {
      zoomAnchorRestoreRaf = 0;
      if (zoomPreviewAnchor || !page.isConnected) return;
      restoreZoomAnchor(page, anchor);
      zoomAnchorVerifyRaf = requestAnimationFrame(function() {
        zoomAnchorVerifyRaf = 0;
        if (zoomPreviewAnchor || !page.isConnected) return;
        restoreZoomAnchor(page, anchor);
      });
    });
  }

  function clearZoomPreview(page) {
    if (zoomCommitTimer) {
      clearTimeout(zoomCommitTimer);
      zoomCommitTimer = 0;
    }
    zoomPreviewAnchor = null;
    if (!page) return;
    page.style.transform = '';
    page.style.transformOrigin = '';
    page.style.willChange = '';
  }

  function updatePageScale(_contentHeight) {
    var page = pageEl();
    var shell = pageShellEl();
    if (!page || !shell) return;
    var anchor = zoomPreviewAnchor;
    cancelZoomAnchorRestore();
    clearZoomPreview(page);
    var plan = pageScalePlan(currentUserZoom);
    // Browser tabs use CSS `zoom`, whose layout box scales automatically.
    // Crop narrows the paper by the padding it removes (CSS drops --page-pad-x
    // to 12px), keeping the text column — and its wrapping — identical. See
    // cropDxNow for the media-query mirroring.
    if (plan.naturalWidth !== null) {
      document.documentElement.style.setProperty(
        '--page-natural-width', Math.round(plan.naturalWidth) + 'px'
      );
    }
    var pageScaleCss = plan.pageScale.toFixed(4);
    committedPageScale = parseFloat(pageScaleCss);
    document.documentElement.style.setProperty('--page-scale', pageScaleCss);
    shell.style.width = Math.round(plan.shellWidth) + 'px';
    shell.style.height = '';
    // Gutter beside the centered page — the default width of each margin column,
    // so notes sit in the whitespace without overlapping the text (a card's pin
    // button expands its column past this, over the text).
    var gutter = Math.max(0, Math.floor((plan.viewportWidth - plan.shellWidth) / 2));
    document.documentElement.style.setProperty('--margin-gutter', gutter + 'px');
    // A page zoomed wider than the viewport needs a horizontally
    // USER-scrollable viewport: body's default `overflow-x: clip` reaches the
    // viewport as `hidden` — fine for scrollBy (h/l keys) but dead to the
    // mouse/trackpad — so this class swaps it for `auto` (see default.css).
    // Runs on zoom and window resize, so the class tracks the fit exactly.
    document.body.classList.toggle(
      'page-overwide',
      plan.viewportWidth > 0 && plan.shellWidth > plan.viewportWidth + 1
    );
    scheduleZoomAnchorRestore(page, anchor);
    // Zoom/crop/mode changes move the flashed element — track it.
    if (flashBoxTarget) drawFlashBox();
  }

  function clampUserZoom(z) {
    if (!isFinite(z)) return 1;
    return Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, z));
  }

  function setUserZoom(z, persist) {
    currentUserZoom = clampUserZoom(z);
    updatePageScale();
    if (persist) {
      try { localStorage.setItem('mathpreview.userZoom', String(currentUserZoom)); } catch (e) {}
    }
  }

  function commitUserZoom() {
    zoomCommitTimer = 0;
    updatePageScale();
    // Line numbers, refkeys and sidenotes are children of the page and share
    // its scale. Their local geometry is unchanged, so rebuilding any overlay
    // here would only repeat an expensive document walk after every key burst.
  }

  function previewUserZoom(z, persist) {
    var page = pageEl();
    if (!page) {
      setUserZoom(z, persist);
      return;
    }
    cancelZoomAnchorRestore();
    currentUserZoom = clampUserZoom(z);
    var targetScale = pageScalePlan(currentUserZoom).pageScale;
    var previewScale = targetScale / Math.max(committedPageScale, 1e-6);
    if (!zoomPreviewAnchor) zoomPreviewAnchor = captureZoomAnchor(page);
    // CSS zoom lays out the entire paper and is expensive on long documents.
    // Transform the already-laid-out paper immediately, then commit once after
    // the user pauses, when the preview is replaced with committed CSS zoom.
    page.style.transformOrigin =
      zoomPreviewAnchor.localX.toFixed(3) + 'px ' +
      zoomPreviewAnchor.localY.toFixed(3) + 'px';
    page.style.transform = 'scale(' + previewScale.toFixed(6) + ')';
    page.style.willChange = 'transform';
    if (zoomCommitTimer) clearTimeout(zoomCommitTimer);
    zoomCommitTimer = setTimeout(commitUserZoom, NAV_RESIZE_IDLE_MS);
    if (persist) {
      try { localStorage.setItem('mathpreview.userZoom', String(currentUserZoom)); } catch (e) {}
    }
  }

  function bumpUserZoom(delta) {
    var next = Math.round((currentUserZoom + delta) * 100) / 100;
    previewUserZoom(next, true);
  }

  function resetUserZoom() {
    previewUserZoom(1, true);
  }

  // Width the crop removes from the page, or 0 when not cropped.
  function cropDxNow() {
    if (!pageCropped) return 0;
    // Read the REAL uncropped margin (--page-pad-x-base: the daemon may have
    // overridden it from page-margin config / the document's geometry, and
    // dynamic mode pins it to 37.8px at element level) rather than assuming
    // 64. Crop drops --page-pad-x to --crop-pad, so the page narrows by
    // 2×(base − crop) — mirror that exactly. Reading the base off main#page's
    // computed style sees whichever override applies: it survives the crop
    // (only --page-pad-x is overridden) and tracks the current page mode.
    var page = pageEl();
    var cs = page ? getComputedStyle(page) : null;
    var basePadX = cs ? parseFloat(cs.getPropertyValue('--page-pad-x-base')) : NaN;
    if (!isFinite(basePadX)) basePadX = 64;
    return 2 * (basePadX - CROP_PAD);
  }

  function fitToWidth() {
    var available = Math.max(320, document.documentElement.clientWidth - 32);
    var base = (currentPageMode === 'a4') ? A4_CSS_WIDTH : DYNAMIC_BASE_WIDTH;
    previewUserZoom(available / (base - cropDxNow()), true);
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

  function refreshNavigation() {
    navRefreshTimer = 0;
    if (navNeedsIndex) rebuildIndex(false);
    navNeedsIndex = false;
    scheduleSidenoteLayout();
    if (lineNumbersVisible) scheduleLineNumbers();
    if (refkeysVisible) scheduleRefkeys();
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

  function clearLineNumbers() {
    var page = pageEl();
    if (!page) return;
    var old = page.querySelector('.lineno-layer');
    if (old) old.remove();
  }

  /// Does `node` live inside a region we should not line-number? The gutter
  /// layers, the absolutely-positioned margin column / sidenotes / refkey
  /// chips, and folded proof bodies all sit off the main text flow, so their
  /// client rects would land at misleading y-coordinates.
  var LINENO_SKIP = '.lineno-layer, .sidenote, .margin-col,' +
    ' .margin-card, .refkey-chip, .eq-refkey-chip, .proof-body.folded,' +
    ' .para-indent-marker';
  function linenoSkip(node, page) {
    var el = node.parentNode;
    while (el && el !== page) {
      if (el.nodeType === 1 && el.matches && el.matches(LINENO_SKIP)) return true;
      el = el.parentNode;
    }
    return false;
  }

  function measureLineNumberBlock(blk, scale) {
    var blockRect = blk.getBoundingClientRect();
    if (blockRect.width === 0 && blockRect.height === 0) return [];
    var walker = document.createTreeWalker(blk, NodeFilter.SHOW_TEXT, {
      acceptNode: function(node) {
        if (!node.nodeValue || !/\S/.test(node.nodeValue)) return NodeFilter.FILTER_REJECT;
        if (linenoSkip(node, blk)) return NodeFilter.FILTER_REJECT;
        return NodeFilter.FILTER_ACCEPT;
      }
    });
    var entries = [];
    var range = document.createRange();
    var node;
    while ((node = walker.nextNode())) {
      range.selectNodeContents(node);
      var rects = range.getClientRects();
      for (var r = 0; r < rects.length; r++) {
        var rect = rects[r];
        if (rect.width === 0 && rect.height === 0) continue;
        entries.push({
          top: (rect.top - blockRect.top) / scale,
          h: rect.height / scale,
        });
      }
    }
    return entries;
  }

  /// Number every *visual* (wrapped) line of rendered text, lineno-style.
  ///
  /// MathJax emits SVG with no text nodes, so display equations are not
  /// numbered (matching LaTeX's `lineno`, which skips display math by
  /// default); a paragraph with inline math still numbers normally because
  /// its surrounding text nodes produce line rects. We walk every text node
  /// in `#page`, collect a client rect per wrapped line fragment, then merge
  /// fragments that share a visual line (e.g. `text \emph{x} text`) by
  /// comparing top offsets. Each block's fragment positions are cached during
  /// a lightweight pre-layout so scrolling never has to discover them.
  function layoutLineNumbers() {
    var page = pageEl();
    if (!page) return;
    clearLineNumbers();
    if (!lineNumbersVisible) return;

    ensureOverlayMetrics(page);
    var pageRect = page.getBoundingClientRect();
    // #page is scaled by CSS zoom;
    // getClientRects returns rendered coords, but a child's `top` is in the
    // page's unscaled local space.
    // Convert measured offsets to local space so the gutter aligns at any scale.
    // Self-correcting: this ratio is 1 when the page isn't scaled, so the
    // common 1:1 case is unchanged.
    var scale = page.offsetHeight > 0 ? pageRect.height / page.offsetHeight : 1;
    if (!isFinite(scale) || scale <= 0) scale = 1;
    var entries = [];
    topLevelOverlayBlocks(page).forEach(function(blk) {
      var blockRect = blk.getBoundingClientRect();
      if (blockRect.width === 0 && blockRect.height === 0) return;
      var blockTop = (blockRect.top - pageRect.top) / scale;
      var local = lineNumberBlockMetrics.get(blk) || [];
      local.forEach(function(entry) {
        entries.push({ top: blockTop + entry.top, h: entry.h });
      });
    });
    if (!entries.length) return;

    entries.sort(function(a, b) { return a.top - b.top; });

    // Merge fragments on the same visual line: a new line starts only when
    // the top jumps by more than half the previous line's height.
    var lines = [];
    for (var i = 0; i < entries.length; i++) {
      var e = entries[i];
      var last = lines.length ? lines[lines.length - 1] : null;
      if (!last || e.top - last.top > Math.max(4, last.h * 0.5)) {
        lines.push({ top: e.top, h: e.h });
      } else if (e.h > last.h) {
        last.h = e.h;
      }
    }

    var layer = document.createElement('div');
    layer.className = 'lineno-layer';
    layer.setAttribute('aria-hidden', 'true');
    for (var k = 0; k < lines.length; k++) {
      var num = document.createElement('span');
      num.className = 'lineno-num';
      // Center the (fixed 11px) number within the line's measured height so it
      // tracks the text baseline at any body font size, instead of sitting at
      // the top of a tall line.
      num.style.top = Math.round(lines[k].top) + 'px';
      num.style.height = Math.round(lines[k].h) + 'px';
      num.style.lineHeight = Math.round(lines[k].h) + 'px';
      num.textContent = String(k + 1);
      layer.appendChild(num);
    }
    page.appendChild(layer);
  }

  function scheduleLineNumbers() {
    if (lineNumbersScheduled) return;
    lineNumbersScheduled = true;
    requestAnimationFrame(function() {
      lineNumbersScheduled = false;
      layoutLineNumbers();
    });
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
    if (includeIndex !== false) navNeedsIndex = true;
    if (navRefreshTimer) clearTimeout(navRefreshTimer);
    navRefreshTimer = setTimeout(refreshNavigation, typeof delay === 'number' ? delay : NAV_IDLE_MS);
  }
