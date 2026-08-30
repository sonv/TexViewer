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

  // Row-level copy selection inside a multi-row display block: set by a plain
  // click on a rendered row, cleared by clicking the same row again (widening
  // back to the whole environment), clicking elsewhere, or the block being
  // replaced by a re-render. While set, ⌘C copies just that row's source,
  // sliced out of data-tex via the renderer's data-row-tex-spans offsets.
  var selectedMathRow = null; // { block, row }

  function clearSelectedMathRow() {
    document.querySelectorAll('rect.mp-row-select').forEach(function(r) {
      if (r.parentNode) r.parentNode.removeChild(r);
    });
    selectedMathRow = null;
  }

  // The row's source: data-row-tex-spans holds BYTE offsets into the raw
  // data-tex string (computed in Rust) — JS strings are UTF-16, so slice
  // through a UTF-8 round-trip.
  function mathRowTex(block, row) {
    var spans = (block.getAttribute('data-row-tex-spans') || '').split(',');
    var m = /^(\d+):(\d+)$/.exec(spans[row] || '');
    if (!m) return '';
    var bytes = new TextEncoder().encode(block.getAttribute('data-tex') || '');
    return new TextDecoder().decode(bytes.subarray(+m[1], +m[2]));
  }

  // Mark `row` of `block` as the copy target: a thin underline beneath the
  // row (a filled box over the math read as noise) plus the block's focus
  // outline. Geometry is relative to the row's ink box, so it scales with
  // the page zoom like everything else in the SVG.
  function selectMathRow(block, row) {
    clearSelectedMath();
    clearSelectedMathRow();
    var groups = mathRowGroups(block);
    var g = groups[row];
    if (!g || !g.getBBox) return false;
    var bb;
    try { bb = g.getBBox(); } catch (e) { return false; }
    if (!bb || !isFinite(bb.width) || bb.width <= 0 || bb.height <= 0) return false;
    var padX = bb.height * 0.1;
    var thickness = bb.height * 0.05;
    var gap = bb.height * 0.08;
    // MathJax's SVG root flips the Y axis (glyph coords are y-up), so the
    // VISUAL bottom of the row is the local-coordinate minimum when the
    // cumulative transform has a negative y scale.
    var ctm = g.getCTM ? g.getCTM() : null;
    var yUnder = (ctm && ctm.d < 0)
      ? bb.y - gap - thickness
      : bb.y + bb.height + gap;
    var rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
    rect.setAttribute('class', 'mp-row-select');
    rect.setAttribute('x', bb.x - padX);
    rect.setAttribute('y', yUnder);
    rect.setAttribute('width', bb.width + padX * 2);
    rect.setAttribute('height', thickness);
    rect.setAttribute('rx', thickness / 2);
    g.insertBefore(rect, g.firstChild);
    selectedMathRow = { block: block, row: row };
    focusMathNode(block);
    // Collapse any text selection so the copy handler takes the focused-math
    // path (which prefers the row) instead of the range path.
    var sel = window.getSelection();
    if (sel) sel.removeAllRanges();
    return true;
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
      if (node.matches && node.matches('.para-indent-marker, .fold-marker')) {
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
    // A replaced block orphans its row selection (the band may linger inside
    // the stale-typeset placeholder): drop the state so ⌘C falls through to
    // the normal paths instead of silently doing nothing.
    if (selectedMathRow && !selectedMathRow.block.isConnected) clearSelectedMathRow();
    // A row selected for copy (its band is showing) wins — the band is the
    // visual contract for what ⌘C grabs, focus or not. Two exceptions: a
    // real dragged text selection, and focus in an editable control (the
    // search box / cmdline select their text INSIDE the input, invisible to
    // getSelection — the row must not hijack that copy). This runs BEFORE
    // the rangeCount guard: selectMathRow clears all ranges, so rangeCount
    // is legitimately 0 while a row is selected.
    var noRealSelection = !selection || !selection.rangeCount || selection.isCollapsed;
    if (noRealSelection && selectedMathRow &&
        !isEditableTarget(document.activeElement)) {
      var rowTex = mathRowTex(selectedMathRow.block, selectedMathRow.row);
      if (rowTex) {
        e.clipboardData.setData('text/plain', rowTex);
        e.preventDefault();
        return;
      }
    }
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
    // Whole-node selection supersedes a row selection — the row band must
    // not keep promising a row copy that ⌘C won't deliver.
    clearSelectedMathRow();
    selectedMath = math;
    math.classList.add('math-selected');
    focusMathNode(math);
    var range = document.createRange();
    range.selectNode(math);
    var selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  }

  // File > Print (no keystroke to intercept): the dialog can't be delayed, so
  // this print may show off-screen math untypeset — start the full flush and
  // tell the user a re-print will be complete.
  window.addEventListener('beforeprint', function() {
    if (document.body.classList.contains('print-preparing')) return;
    var page = pageEl();
    var raw = page ? Array.from(page.querySelectorAll('.math[data-hash]')).filter(isRawMathNode) : [];
    if (raw.length) {
      typesetAllForPrint(
        'This print may be missing equations: they typeset on demand while you '
        + 'read, and the print dialog opened before the whole document was '
        + 'typeset. When this finishes, print again for a complete printout.'
      ).then(function(completed) {
        if (completed) {
          setStatus('live', '● live — math finished typesetting; print again for a complete printout');
        }
      });
    }
  });

  // Event delegation survives `#page` innerHTML replacement.
  document.addEventListener('copy', copySelectionAsLatex);
  document.addEventListener('mousedown', function(e) {
    clearSelectedMath();
  });
  document.addEventListener('input', function(e) {
    if (e.target && (e.target.id === 'macros-dialog-input' ||
                     e.target.id === 'macros-toml-input')) {
      markMacroEditorDirty(e.target);
    }
    if (e.target && e.target.id === 'config-hover-preview-scale') {
      e.target.dataset.dirty = 'true';
    }
  });
  // Macros dialog wiring: file picker + scope radio change.
  document.addEventListener('change', function(e) {
    if (e.target && e.target.id === 'macros-dialog-file') {
      onMacrosFilePicked(e);
      return;
    }
    if (e.target && e.target.name === 'macro-mode') {
      syncMacrosMode();
      return;
    }
    if (e.target && e.target.name === 'scope') {
      syncMacrosCustomPathEnabled();
      // Switching scope shows that file's existing contents.
      reloadActiveScopeFile(true);
      return;
    }
    if (e.target && e.target.id === 'macros-dialog-custom-path') {
      // Do not overwrite text imported/typed before the path was chosen. A
      // clean editor loads the target; a dirty one is protected at Save by
      // the server's expected-content check.
      reloadActiveScopeFile(false);
      return;
    }
    if (e.target && e.target.name === 'config-scope') {
      syncConfigCustomPathEnabled();
      loadViewerConfigForScope(true);
      return;
    }
    if (e.target && e.target.id === 'config-render-tikz') {
      e.target.dataset.dirty = 'true';
      return;
    }
    if (e.target && e.target.id === 'config-fancy-theorems') {
      e.target.dataset.dirty = 'true';
      return;
    }
    if (e.target && e.target.id === 'config-markdown-colon-fences') {
      e.target.dataset.dirty = 'true';
      return;
    }
    if (e.target && e.target.name === 'config-mode') {
      syncConfigMode(false);
      return;
    }
    if (e.target && e.target.id === 'config-dialog-custom-path') {
      loadViewerConfigForScope(true);
      return;
    }
    if (e.target && e.target.id === 'log-panel-verbose') {
      toggleLogVerbose(e.target.checked);
      return;
    }
  });
  document.addEventListener('dblclick', function(e) {
    // Source-jump fires only when the configured trigger matches.
    // The old unconditional `requestSourceJump(e)` fallback was a
    // hold-over from when reveal-source needed a working polling
    // backup; now that the configured-trigger path fires both
    // `/jump` and `/reveal-source` together (v0.1.16), the
    // double-click fallback just duplicated the action and
    // confused the trigger choice.
    if (matchesRevealTrigger(e, 'dblclick')) {
      requestRevealSource(e);
    }
  });
  // A footnote popover is CSS :hover/:focus-within, centered with
  // translateX(-50%). Near a page edge (or under body{overflow-x:clip}) the
  // overhanging half would be clipped with no scroll recovery, so clamp the
  // shown popover into the viewport — the only complete fix, since CSS can't
  // know the marker's X. Reset to the centered base, measure, then nudge in.
  function positionFootnotePopover(fn) {
    var pop = fn.querySelector('.footnote-pop');
    if (!pop) return;
    pop.style.transform = '';
    var r = pop.getBoundingClientRect();
    var m = 8, dx = 0;
    if (r.right > window.innerWidth - m) dx = (window.innerWidth - m) - r.right;
    if (r.left + dx < m) dx = m - r.left;
    if (dx) pop.style.transform = 'translateX(calc(-50% + ' + Math.round(dx) + 'px))';
  }
  function clearFootnotePopover(fn) {
    var pop = fn.querySelector('.footnote-pop');
    if (pop) pop.style.transform = '';
    fn._fnPos = false;
  }
  document.addEventListener('mouseover', function(e) {
    var fn = e.target && e.target.closest && e.target.closest('.footnote');
    if (fn && !fn._fnPos) { fn._fnPos = true; positionFootnotePopover(fn); }
    var link = isPinnableLink(e.target);
    if (link) scheduleHoverPreview(link);
  });
  document.addEventListener('mouseout', function(e) {
    var fn = e.target && e.target.closest && e.target.closest('.footnote');
    if (fn && !(e.relatedTarget && fn.contains(e.relatedTarget))) clearFootnotePopover(fn);
    var link = isPinnableLink(e.target);
    if (!link) return;
    var related = e.relatedTarget;
    if (related && link.contains(related)) return;
    hideHoverPreview();
  });
  document.addEventListener('focusin', function(e) {
    var fn = e.target && e.target.closest && e.target.closest('.footnote');
    if (fn) positionFootnotePopover(fn);
  });
  document.addEventListener('focusout', function(e) {
    var fn = e.target && e.target.closest && e.target.closest('.footnote');
    if (fn && !(e.relatedTarget && fn.contains(e.relatedTarget))) clearFootnotePopover(fn);
  });
  document.addEventListener('scroll', hideHoverPreview, { passive: true });

  // Every keyboard feature and every fixed viewer button routes through this
  // action registry. `[keybindings]` only names actions; it never needs to know
  // which DOM element or implementation function currently powers a control.
  var VIEWER_ACTION_ORDER = [
    'scroll-left', 'scroll-down', 'scroll-up', 'scroll-right',
    'half-page-down', 'half-page-up', 'full-page-down', 'full-page-up',
    'jump-back', 'jump-forward', 'previous-place',
    'go-top', 'go-bottom', 'horizontal-start', 'horizontal-end',
    'previous-paragraph', 'next-paragraph', 'previous-heading', 'next-heading',
    'align-anchor-top', 'align-anchor-center', 'align-anchor-bottom',
    'open-search', 'open-search-backward', 'open-command',
    'search-next', 'search-previous', 'search-word-forward',
    'search-word-backward', 'set-mark', 'jump-mark-line', 'jump-mark-exact',
    'toggle-toc', 'toggle-topbar',
    'toggle-crop', 'close-viewer', 'page-a4', 'page-dynamic',
    'zoom-in', 'zoom-out', 'zoom-reset', 'zoom-fit-width',
    'browser-print', 'toggle-margin', 'toggle-keys', 'toggle-lines',
    'open-macros', 'open-config', 'toggle-log', 'toggle-theme',
    'proof-main', 'proof-supporting', 'proof-all', 'print-pdf',
    'restart-server', 'stop-server',
    // Keep newly inherited defaults after every pre-existing action. If an old
    // partial config already assigned J/K elsewhere, that explicit action
    // retains conflict priority until the user disables or remaps it.
    'five-lines-down', 'five-lines-up',
  ];

  function applyProofModeFromAction(mode) {
    applyMode(mode);
    document.querySelectorAll('.proof-toggle button').forEach(function(btn) {
      btn.classList.toggle('active', btn.getAttribute('data-mode') === mode);
    });
    var toggle = document.querySelector('.proof-toggle');
    if (toggle) toggle.setAttribute('data-mode', mode);
  }

  function viewerLineStep() {
    var vh = window.innerHeight || document.documentElement.clientHeight || 800;
    return Math.max(28, Math.round(vh * 0.06));
  }

  function viewerColumnStep() {
    var vw = window.innerWidth || document.documentElement.clientWidth || 1000;
    return Math.max(48, Math.round(vw * 0.08));
  }

  function viewerTextLineStep() {
    var page = pageEl();
    if (!page) return viewerLineStep();
    var style = getComputedStyle(page);
    var lineHeight = parseFloat(style.lineHeight);
    if (!isFinite(lineHeight) || lineHeight <= 0) {
      var fontSize = parseFloat(style.fontSize);
      lineHeight = (isFinite(fontSize) && fontSize > 0 ? fontSize : 18) * 1.42;
    }
    // Computed lengths are in the page's local CSS coordinates. Convert one
    // body-text line to viewport coordinates from the zoom state instead of
    // measuring the whole page on every repeated J/K keypress. During the
    // short compositor preview, the planned scale is the visual scale; after
    // its CSS-zoom commit, committedPageScale is authoritative.
    var scale = zoomPreviewAnchor
      ? pageScalePlan(currentUserZoom).pageScale
      : committedPageScale;
    if (!isFinite(scale) || scale <= 0) scale = 1;
    return Math.max(1, lineHeight * scale);
  }

  function viewerFiveLineStep() {
    return Math.max(1, Math.round(viewerTextLineStep() * 5));
  }

  function viewerActionCount(ctx) {
    return ctx && isFinite(ctx.count) ? Math.max(1, Math.floor(ctx.count)) : 1;
  }

  function viewerCountedDistance(step, ctx) {
    return Math.max(1, Math.round(step * viewerActionCount(ctx)));
  }

  function viewerNavigationAnchor() {
    var page = pageEl();
    if (!page) return null;
    var rect = page.getBoundingClientRect();
    var x = Math.max(
      rect.left + 1,
      Math.min(rect.right - 1, Math.round((window.innerWidth || 1000) * 0.5))
    );
    var top = topbarOffset();
    var y = Math.max(top + 1, Math.round(top + ((window.innerHeight || 800) - top) * 0.5));
    var hit = document.elementFromPoint(x, y);
    if (!hit || !hit.closest) return null;
    var anchor = hit.closest('#page [data-src], #page p, #page li');
    if (anchor) return anchor;
    // On inter-paragraph whitespace, caret hit-testing usually resolves the
    // nearest text position even though elementFromPoint only sees the paper.
    var caretNode = null;
    if (document.caretPositionFromPoint) {
      var position = document.caretPositionFromPoint(x, y);
      caretNode = position && position.offsetNode;
    } else if (document.caretRangeFromPoint) {
      var range = document.caretRangeFromPoint(x, y);
      caretNode = range && range.startContainer;
    }
    var caretEl = caretNode && (caretNode.nodeType === 1
      ? caretNode : caretNode.parentElement);
    anchor = caretEl && caretEl.closest &&
      caretEl.closest('#page [data-src], #page p, #page li');
    if (anchor) return anchor;
    // A coarse block still preserves the correct document insertion point;
    // viewerSemanticTarget handles its first/last descendant directionally.
    return hit.closest('#page .blk');
  }

  function viewerSemanticTarget(selector, direction, count) {
    var page = pageEl();
    if (!page) return null;
    var targets = Array.from(page.querySelectorAll(selector));
    if (!targets.length) return null;
    var anchor = viewerNavigationAnchor();
    if (!anchor) return null;
    var index = direction > 0 ? -1 : targets.length;
    var located = false;
    var anchorInsideTarget = targets.some(function(target) {
      return target === anchor || target.contains(anchor);
    });
    var coarseAnchor = !anchorInsideTarget && targets.some(function(target) {
      return anchor.contains(target);
    });
    if (coarseAnchor) {
      var referenceTop = topbarOffset();
      var referenceY = referenceTop +
        ((window.innerHeight || 800) - referenceTop) * 0.5;
      var beforeInside = -1;
      var afterInside = -1;
      for (var b = 0; b < targets.length; b++) {
        if (anchor.contains(targets[b])) {
          var insideRect = targets[b].getBoundingClientRect();
          if (insideRect.width === 0 && insideRect.height === 0) continue;
          if (insideRect.top <= referenceY && insideRect.bottom >= referenceY) {
            index = b;
            located = true;
            break;
          }
          if (insideRect.bottom < referenceY) beforeInside = b;
          if (afterInside < 0 && insideRect.top > referenceY) afterInside = b;
        }
      }
      if (!located && (beforeInside >= 0 || afterInside >= 0)) {
        index = direction > 0
          ? (beforeInside >= 0 ? beforeInside : afterInside - 1)
          : (afterInside >= 0 ? afterInside : beforeInside + 1);
        located = true;
      }
    }
    for (var i = 0; i < targets.length; i++) {
      if (located) break;
      var target = targets[i];
      if (target === anchor || target.contains(anchor)) {
        index = i;
        located = true;
        break;
      }
      if (anchor && (anchor.compareDocumentPosition(target) & Node.DOCUMENT_POSITION_FOLLOWING)) {
        index = direction > 0 ? i - 1 : i;
        located = true;
        break;
      }
    }
    if (!located && direction > 0) index = targets.length - 1;
    var next = index + direction * viewerActionCount({ count: count });
    if (next < 0 || next >= targets.length) return null;
    return targets[next];
  }

  function navigateViewerSemantic(selector, direction, ctx) {
    var target = viewerSemanticTarget(selector, direction, viewerActionCount(ctx));
    if (!target) {
      setStatus('dead', '○ no navigation target');
      return false;
    }
    scrollToTarget(target);
    return true;
  }

  function alignViewerAnchor(fraction) {
    var target = viewerNavigationAnchor();
    if (!target || target === pageEl()) return false;
    var rect = target.getBoundingClientRect();
    var top = topbarOffset();
    var available = Math.max(1, (window.innerHeight || 800) - top);
    var targetY = top + available * fraction;
    window.scrollTo({
      left: window.scrollX,
      top: Math.max(0, window.scrollY + rect.top - targetY),
      behavior: 'auto',
    });
    return true;
  }

  function viewerSourceLine(src) {
    var match = /:(\d+):(\d+)$/.exec(src || '');
    return match ? parseInt(match[1], 10) : 0;
  }

  function scrollToViewerSourceLine(line) {
    var page = pageEl();
    if (!page) return false;
    var best = null;
    var bestLine = Infinity;
    var last = null;
    var lastLine = 0;
    page.querySelectorAll('[data-src]').forEach(function(el) {
      var candidate = viewerSourceLine(el.getAttribute('data-src'));
      if (candidate > lastLine) {
        last = el;
        lastLine = candidate;
      }
      if (candidate >= line && candidate < bestLine) {
        best = el;
        bestLine = candidate;
      }
    });
    if (!best) best = last;
    if (!best) return false;
    scrollToTarget(best);
    return true;
  }

  function viewerSearchWord(backwards, ctx) {
    var selection = window.getSelection && window.getSelection();
    var query = selection ? String(selection).replace(/\s+/g, ' ').trim() : '';
    if (!query) {
      var anchor = viewerNavigationAnchor();
      var word = anchor && anchor.closest && anchor.closest('.src-word');
      query = word ? cleanNavText(word.textContent) : '';
    }
    if (!query) {
      setStatus('dead', '○ select a word to search');
      return false;
    }
    openSearchPanel.backwards = !!backwards;
    lastSearchQuery = query;
    var input = searchInputEl();
    if (input) input.value = query;
    return runSearch(backwards, viewerActionCount(ctx));
  }

  function repeatViewerSearch(backwards, ctx) {
    return runSearch(backwards, viewerActionCount(ctx));
  }

  var viewerMarks = Object.create(null);

  function viewerMarkName(name) {
    return typeof name === 'string' && name.length === 1 ? name : '';
  }

  function setViewerMark(name) {
    name = viewerMarkName(name);
    if (!name) return false;
    viewerMarks[name] = currentViewerPlace();
    setStatus('live', '● mark ' + name + ' set');
    return true;
  }

  function jumpToViewerMark(name, exact) {
    name = viewerMarkName(name);
    var place = name && viewerMarks[name];
    if (!place) {
      setStatus('dead', '○ mark ' + (name || '?') + ' is not set');
      return false;
    }
    recordViewerPlace();
    var restored = restoreViewerPlace(Object.assign({}, place, {
      x: exact ? place.x : 0,
    }));
    if (restored) setStatus('live', '● mark ' + name + ' restored');
    return restored;
  }

  var viewerActions = {
    'scroll-left': function(ctx) {
      scrollByVim(-viewerCountedDistance(viewerColumnStep(), ctx), 0);
    },
    'scroll-down': function(ctx) {
      scrollByVim(0, viewerCountedDistance(viewerTextLineStep(), ctx));
    },
    'scroll-up': function(ctx) {
      scrollByVim(0, -viewerCountedDistance(viewerTextLineStep(), ctx));
    },
    'scroll-right': function(ctx) {
      scrollByVim(viewerCountedDistance(viewerColumnStep(), ctx), 0);
    },
    // Compatibility actions for configs written during v2.1.21 development.
    // The built-in J/K bindings are aliases to 5j/5k instead.
    'five-lines-down': function(ctx) {
      scrollByVim(0, viewerCountedDistance(viewerFiveLineStep(), ctx));
    },
    'five-lines-up': function(ctx) {
      scrollByVim(0, -viewerCountedDistance(viewerFiveLineStep(), ctx));
    },
    'half-page-down': function(ctx) {
      var vh = window.innerHeight || document.documentElement.clientHeight || 800;
      scrollByVim(0, viewerCountedDistance(vh * 0.5, ctx));
    },
    'half-page-up': function(ctx) {
      var vh = window.innerHeight || document.documentElement.clientHeight || 800;
      scrollByVim(0, -viewerCountedDistance(vh * 0.5, ctx));
    },
    'full-page-down': function(ctx) {
      var vh = window.innerHeight || document.documentElement.clientHeight || 800;
      scrollByVim(0, viewerCountedDistance(vh, ctx));
    },
    'full-page-up': function(ctx) {
      var vh = window.innerHeight || document.documentElement.clientHeight || 800;
      scrollByVim(0, -viewerCountedDistance(vh, ctx));
    },
    'jump-back': function(ctx) {
      restorePreviousPlace(viewerActionCount(ctx));
    },
    'jump-forward': function(ctx) {
      restoreNextPlace(viewerActionCount(ctx));
    },
    'previous-place': function() { restorePreviousPlace(); },
    'go-top': function(ctx) {
      if (ctx && ctx.explicitCount && scrollToViewerSourceLine(ctx.count)) return;
      recordViewerPlace();
      window.scrollTo({ top: 0, left: window.scrollX, behavior: 'auto' });
    },
    'go-bottom': function(ctx) {
      if (ctx && ctx.explicitCount && scrollToViewerSourceLine(ctx.count)) return;
      recordViewerPlace();
      window.scrollTo({
        top: document.documentElement.scrollHeight,
        left: window.scrollX,
        behavior: 'auto',
      });
    },
    'horizontal-start': function() {
      window.scrollTo({ left: 0, top: window.scrollY, behavior: 'auto' });
    },
    'horizontal-end': function() {
      window.scrollTo({
        left: document.documentElement.scrollWidth,
        top: window.scrollY,
        behavior: 'auto',
      });
    },
    'previous-paragraph': function(ctx) { navigateViewerSemantic('p', -1, ctx); },
    'next-paragraph': function(ctx) { navigateViewerSemantic('p', 1, ctx); },
    'previous-heading': function(ctx) { navigateViewerSemantic(headingSelector(), -1, ctx); },
    'next-heading': function(ctx) { navigateViewerSemantic(headingSelector(), 1, ctx); },
    'align-anchor-top': function() { alignViewerAnchor(0); },
    'align-anchor-center': function() { alignViewerAnchor(0.5); },
    'align-anchor-bottom': function() { alignViewerAnchor(1); },
    'open-search': function() { openSearchPanel(); },
    'open-search-backward': function() { openSearchPanel(true); },
    'open-command': function() { openCmdline(''); },
    'search-next': function(ctx) {
      repeatViewerSearch(!!openSearchPanel.backwards, ctx);
    },
    'search-previous': function(ctx) {
      repeatViewerSearch(!openSearchPanel.backwards, ctx);
    },
    'search-word-forward': function(ctx) { viewerSearchWord(false, ctx); },
    'search-word-backward': function(ctx) { viewerSearchWord(true, ctx); },
    'set-mark': function(ctx) { setViewerMark(ctx && ctx.char); },
    'jump-mark-line': function(ctx) { jumpToViewerMark(ctx && ctx.char, false); },
    'jump-mark-exact': function(ctx) { jumpToViewerMark(ctx && ctx.char, true); },
    'toggle-toc': function() { setSideOpen(!currentSideOpen, true); },
    'toggle-topbar': function() { setTopbarHidden(!topbarHidden, true); },
    'toggle-crop': function() { setPageCrop(!pageCropped, true); },
    'close-viewer': function() {
      closeViewer(function(msg) { setStatus('dead', '○ ' + msg); });
    },
    'page-a4': function() { setPageMode('a4'); },
    'page-dynamic': function() { setPageMode('dynamic'); },
    'zoom-in': function() { bumpUserZoom(ZOOM_STEP); },
    'zoom-out': function() { bumpUserZoom(-ZOOM_STEP); },
    'zoom-reset': function() { resetUserZoom(); },
    'zoom-fit-width': function() { fitToWidth(); },
    'browser-print': function() {
      typesetAllForPrint().then(function(completed) {
        if (completed) window.print();
      });
    },
    'toggle-margin': function() { setMarginMode(!marginMode, true); },
    'toggle-keys': function() { setRefkeysVisible(!refkeysVisible, true); },
    'toggle-lines': function() { setLineNumbers(!lineNumbersVisible, true); },
    'open-macros': function() { openMacrosDialog(); },
    'open-config': function() { openConfigDialog(); },
    'toggle-log': function() { toggleLogPanel(); },
    'toggle-theme': function() {
      setTheme(themeMode === 'dark' ? 'light' : 'dark', true);
    },
    'proof-main': function() { applyProofModeFromAction('main'); },
    'proof-supporting': function() { applyProofModeFromAction('supporting'); },
    'proof-all': function() { applyProofModeFromAction('all'); },
    'print-pdf': function() {
      var btn = document.getElementById('print-button');
      if (btn) requestPrint(btn);
    },
    'restart-server': function() { restartServer(); },
    'stop-server': function() {
      if (manualStopRequested) startServer();
      else stopServer();
    },
  };

  var VIEWER_MAX_COUNT = 9999;
  var viewerCountDigits = '';
  var viewerDigitFallbackTimer = 0;

  function clearViewerDigitFallback() {
    if (viewerDigitFallbackTimer) {
      clearTimeout(viewerDigitFallbackTimer);
      viewerDigitFallbackTimer = 0;
    }
  }

  function clearViewerKeyPending() {
    clearKeySequencePending();
    clearViewerDigitFallback();
    viewerCountDigits = '';
  }

  function runViewerAction(action, invocation) {
    var run = viewerActions[action];
    if (!run) return false;
    var fromKey = invocation && invocation.fromKey;
    var typedCount = fromKey && viewerCountDigits
      ? parseInt(viewerCountDigits, 10)
      : 1;
    if (!isFinite(typedCount) || typedCount < 1) typedCount = 1;
    var fixedCount = invocation && invocation.fixedCount
      ? invocation.fixedCount
      : 1;
    var count = Math.min(VIEWER_MAX_COUNT, typedCount * fixedCount);
    var ctx = {
      count: count,
      explicitCount: !!(fromKey && viewerCountDigits) || fixedCount !== 1,
      char: invocation && invocation.char || '',
    };
    clearViewerKeyPending();
    run(ctx);
    return true;
  }

  function normalizedBindingKey(key) {
    var lower = key.toLowerCase();
    if (lower === 'space' || lower === 'spacebar') return ' ';
    if (lower === 'esc') return 'Escape';
    if (lower === 'plus') return '+';
    if (lower === 'minus') return '-';
    if (lower === 'equal' || lower === 'equals') return '=';
    if (lower === 'return') return 'Enter';
    return key;
  }

  function parseBindingStep(raw) {
    var rest = (raw || '').trim();
    if (!rest) return null;
    var step = { mod: false, ctrl: false, meta: false, alt: false, shift: false, key: '' };
    var modifier;
    while ((modifier = /^(Mod|Ctrl|Control|Meta|Cmd|Command|Alt|Option|Shift)\+/i.exec(rest))) {
      var name = modifier[1].toLowerCase();
      if (name === 'mod') step.mod = true;
      else if (name === 'ctrl' || name === 'control') step.ctrl = true;
      else if (name === 'meta' || name === 'cmd' || name === 'command') step.meta = true;
      else if (name === 'alt' || name === 'option') step.alt = true;
      else if (name === 'shift') step.shift = true;
      rest = rest.slice(modifier[0].length);
    }
    if (!rest) return null;
    if (rest.toLowerCase() === '<char>') {
      if (step.mod || step.ctrl || step.meta || step.alt || step.shift) return null;
      step.capture = 'char';
      return step;
    }
    step.key = normalizedBindingKey(rest);

    // `KeyboardEvent.key` already contains the shifted printable glyph. Turn
    // Shift+g / Shift+= into G / + so both notations match the same event.
    if (step.shift) {
      if (/^[a-z]$/.test(step.key)) {
        step.key = step.key.toUpperCase();
        step.shift = false;
      } else {
        var shifted = {
          '`': '~', '1': '!', '2': '@', '3': '#', '4': '$', '5': '%',
          '6': '^', '7': '&', '8': '*', '9': '(', '0': ')', '-': '_',
          '=': '+', '[': '{', ']': '}', '\\': '|', ';': ':', "'": '"',
          ',': '<', '.': '>', '/': '?',
        };
        if (Object.prototype.hasOwnProperty.call(shifted, step.key)) {
          step.key = shifted[step.key];
          step.shift = false;
        }
      }
    }
    return step;
  }

  function parseBinding(raw) {
    if (typeof raw !== 'string' || !raw.trim()) return null;
    var parts = raw.trim().split(/\s+/);
    var steps = [];
    for (var i = 0; i < parts.length; i++) {
      var step = parseBindingStep(parts[i]);
      if (!step) return null;
      steps.push(step);
    }
    if (steps.filter(function(step) { return step.capture === 'char'; }).length > 1) {
      return null;
    }
    return steps;
  }

  function eventKey(e) {
    return normalizedBindingKey(e.key || '');
  }

  function keyEncodesShift(key) {
    if (/^[A-Z]$/.test(key)) return true;
    return key.length === 1 && '~!@#$%^&*()_+{}|:"<>?'.indexOf(key) >= 0;
  }

  function isAppleKeyboardPlatform() {
    return /Mac|iPhone|iPad|iPod/.test(
      navigator.platform || navigator.userAgent || ''
    );
  }

  function bindingStepMatches(step, e) {
    var apple = isAppleKeyboardPlatform();
    var wantsCtrl = step.ctrl || (step.mod && !apple);
    var wantsMeta = step.meta || (step.mod && apple);
    if (!!e.ctrlKey !== wantsCtrl || !!e.metaKey !== wantsMeta || !!e.altKey !== step.alt) {
      return false;
    }
    if (step.capture === 'char') {
      return eventKey(e).length === 1;
    }
    if (!keyEncodesShift(step.key) && !!e.shiftKey !== step.shift) return false;
    return eventKey(e) === step.key;
  }

  function bindingStepsEqual(left, right) {
    if (!left || !right || left.length !== right.length) return false;
    for (var i = 0; i < left.length; i++) {
      var a = effectiveBindingStep(left[i]);
      var b = effectiveBindingStep(right[i]);
      if (a.key !== b.key || a.capture !== b.capture || a.ctrl !== b.ctrl ||
          a.meta !== b.meta || a.alt !== b.alt || a.shift !== b.shift) return false;
    }
    return true;
  }

  function effectiveBindingStep(step) {
    var apple = isAppleKeyboardPlatform();
    return {
      ctrl: step.ctrl || (step.mod && !apple),
      meta: step.meta || (step.mod && apple),
      alt: step.alt,
      shift: step.shift,
      key: step.key,
      capture: step.capture || '',
    };
  }

  var viewerKeybindings = {};
  var viewerKeybindingAliases = {};
  var viewerKeybindingSignature = '';
  var compiledKeybindings = [];

  function displayBinding(binding) {
    var apple = isAppleKeyboardPlatform();
    return binding.replace(/Mod\+/g, apple ? '⌘' : 'Ctrl+');
  }

  function refreshViewerActionHint(btn) {
    if (!btn) return;
    var action = btn.getAttribute('data-viewer-action');
    if (!action) return;
    if (!btn.hasAttribute('data-viewer-action-title')) {
      btn.setAttribute('data-viewer-action-title', btn.getAttribute('title') || '');
    }
    var base = btn.getAttribute('data-viewer-action-title') || '';
    var bindings = viewerKeybindings[action] || [];
    var hint = bindings.length
      ? (bindings.length === 1 ? 'key: ' : 'keys: ') + bindings.map(displayBinding).join(', ')
      : '';
    btn.setAttribute('title', base && hint ? base + ' (' + hint + ')' : (base || hint));
  }

  function setViewerActionTitle(btn, title) {
    if (!btn) return;
    btn.setAttribute('data-viewer-action-title', title || '');
    refreshViewerActionHint(btn);
  }

  function refreshViewerActionHints() {
    document.querySelectorAll('[data-viewer-action]').forEach(refreshViewerActionHint);
  }

  function parseAliasExpansion(raw) {
    if (typeof raw !== 'string' || !raw.trim()) return null;
    var text = raw.trim();
    var fixedCount = 1;
    var count = /^([1-9][0-9]*)(?=[^0-9\s])/.exec(text);
    if (count) {
      fixedCount = Math.min(VIEWER_MAX_COUNT, parseInt(count[1], 10));
      text = text.slice(count[1].length);
    }
    var steps = parseBinding(text);
    return steps ? { steps: steps, fixedCount: fixedCount } : null;
  }

  function resolveKeybindingAlias(definition, direct, definitions, trail) {
    if (trail.indexOf(definition.source) >= 0) return null;
    var parsed = parseAliasExpansion(definition.target);
    if (!parsed) return null;
    var action = direct.find(function(binding) {
      return bindingStepsEqual(binding.steps, parsed.steps);
    });
    if (action) {
      return {
        action: action.action,
        fixedCount: parsed.fixedCount,
        requiresChar: action.steps.some(function(step) { return step.capture === 'char'; }),
      };
    }
    var nested = definitions.find(function(candidate) {
      return bindingStepsEqual(candidate.steps, parsed.steps);
    });
    if (!nested) return null;
    var resolved = resolveKeybindingAlias(
      nested,
      direct,
      definitions,
      trail.concat(definition.source)
    );
    if (!resolved) return null;
    resolved.fixedCount = Math.min(
      VIEWER_MAX_COUNT,
      resolved.fixedCount * parsed.fixedCount
    );
    return resolved;
  }

  function bindingStepsIdentity(steps) {
    return JSON.stringify(steps.map(function(step) {
      var effective = effectiveBindingStep(step);
      return [
        effective.ctrl, effective.meta, effective.alt, effective.shift,
        effective.key, effective.capture,
      ];
    }));
  }

  function setViewerKeybindings(bindings, aliases, timeoutMs) {
    var nextBindings = (bindings && typeof bindings === 'object') ? bindings : {};
    var nextAliases = (aliases && typeof aliases === 'object') ? aliases : {};
    var nextTimeout = Number(timeoutMs);
    if (!isFinite(nextTimeout) || nextTimeout < 100 || nextTimeout > 5000) nextTimeout = 750;
    nextTimeout = Math.round(nextTimeout);
    var nextSignature = JSON.stringify([nextBindings, nextAliases, nextTimeout]);
    if (nextSignature === viewerKeybindingSignature) return false;
    viewerKeybindingSignature = nextSignature;
    viewerKeySequenceTimeoutMs = nextTimeout;
    viewerKeybindings = nextBindings;
    viewerKeybindingAliases = nextAliases;
    compiledKeybindings = [];
    var keybindingWarnings = [];
    VIEWER_ACTION_ORDER.forEach(function(action) {
      var raw = viewerKeybindings[action];
      if (typeof raw === 'string') raw = [raw];
      if (!Array.isArray(raw)) return;
      raw.forEach(function(binding) {
        var steps = parseBinding(binding);
        if (steps) compiledKeybindings.push({ action: action, binding: binding, steps: steps });
        else console.warn('mathpreview: ignored invalid keybinding', action, binding);
      });
    });
    var directOwners = new Map();
    compiledKeybindings = compiledKeybindings.filter(function(binding) {
      var identity = bindingStepsIdentity(binding.steps);
      var owner = directOwners.get(identity);
      if (!owner) {
        directOwners.set(identity, binding);
        return true;
      }
      if (owner.action !== binding.action) {
        keybindingWarnings.push(
          binding.binding + ' is assigned to both ' + owner.action + ' and ' + binding.action
        );
      }
      return false;
    });
    var direct = compiledKeybindings.slice();
    var aliasDefinitions = [];
    Object.keys(viewerKeybindingAliases).forEach(function(source) {
      var target = viewerKeybindingAliases[source];
      var steps = parseBinding(source);
      if (typeof target === 'string' && steps) {
        aliasDefinitions.push({ source: source, target: target, steps: steps });
      } else {
        console.warn('mathpreview: ignored invalid keybinding alias', source, target);
      }
    });
    aliasDefinitions.forEach(function(definition) {
      var resolved = resolveKeybindingAlias(definition, direct, aliasDefinitions, []);
      if (!resolved) {
        console.warn(
          'mathpreview: keybinding alias has no configured target or contains a cycle',
          definition.source,
          definition.target
        );
        keybindingWarnings.push(
          definition.source + ' alias has no configured target or contains a cycle'
        );
        return;
      }
      if (resolved.requiresChar &&
          !definition.steps.some(function(step) { return step.capture === 'char'; })) {
        keybindingWarnings.push(
          definition.source + ' cannot expand to wildcard mapping ' + definition.target
        );
        return;
      }
      var aliasIdentity = bindingStepsIdentity(definition.steps);
      var existing = compiledKeybindings.find(function(binding) {
        return bindingStepsIdentity(binding.steps) === aliasIdentity;
      });
      if (existing) {
        if (!existing.alias) {
          // Explicit action bindings intentionally outrank aliases, but make
          // the shadowing visible instead of silently choosing by registry order.
          keybindingWarnings.push(
            definition.source + ' alias is shadowed by action ' + existing.action
          );
        } else {
          keybindingWarnings.push(
            definition.source + ' duplicates alias ' + existing.binding
          );
        }
        return;
      }
      compiledKeybindings.push({
        action: resolved.action,
        binding: definition.source,
        steps: definition.steps,
        fixedCount: resolved.fixedCount,
        alias: true,
      });
    });
    clearViewerKeyPending();
    refreshViewerActionHints();
    if (keybindingWarnings.length) {
      console.warn('mathpreview: keybinding conflicts', keybindingWarnings);
      setTimeout(function() {
        setStatus('dead', '○ keybinding conflict: ' + keybindingWarnings[0]);
      }, 0);
    }
    return true;
  }

  function candidateInvocation(binding, e, captures) {
    var char = captures && captures.char || '';
    if (binding.steps.length === 1 && binding.steps[0].capture === 'char') {
      char = eventKey(e);
    }
    return {
      fromKey: true,
      fixedCount: binding.fixedCount || 1,
      char: char,
    };
  }

  function preferLiteralBindingStep(left, right, stepIndex) {
    var leftStep = left.steps[stepIndex];
    var rightStep = right.steps[stepIndex];
    return (leftStep.capture ? 1 : 0) - (rightStep.capture ? 1 : 0);
  }

  function viewerSequenceWaitsForCharacter() {
    if (keySequenceExactFallback || !keySequenceCandidates.length) return false;
    return keySequenceCandidates.every(function(candidate) {
      var step = candidate.binding.steps[candidate.next];
      return step && step.capture === 'char' &&
        candidate.next + 1 === candidate.binding.steps.length;
    });
  }

  function startKeybindingSequence(e) {
    var matches = compiledKeybindings.filter(function(binding) {
      return bindingStepMatches(binding.steps[0], e);
    });
    // A literal mapping is more specific than a `<char>` wildcard. Keep
    // action-order priority only when both candidates have equal specificity.
    matches.sort(function(left, right) {
      return preferLiteralBindingStep(left, right, 0);
    });
    if (!matches.length) return false;
    var exact = matches.find(function(binding) { return binding.steps.length === 1; });
    var longer = matches.filter(function(binding) { return binding.steps.length > 1; });
    if (exact && !longer.length) {
      return runViewerAction(exact.action, candidateInvocation(exact, e));
    }
    keySequenceCandidates = longer.map(function(binding) {
      var captures = {};
      if (binding.steps[0].capture === 'char') captures.char = eventKey(e);
      return { binding: binding, next: 1, captures: captures };
    });
    keySequenceExactFallback = exact ? {
      action: exact.action,
      invocation: candidateInvocation(exact, e),
    } : null;
    armKeySequenceTimeout(viewerSequenceWaitsForCharacter());
    return true;
  }

  function continueKeybindingSequence(e) {
    var matches = keySequenceCandidates.filter(function(candidate) {
      return bindingStepMatches(candidate.binding.steps[candidate.next], e);
    });
    matches.sort(function(left, right) {
      return preferLiteralBindingStep(left.binding, right.binding, left.next);
    });
    if (!matches.length) {
      var fallback = keySequenceExactFallback;
      if (fallback) runViewerAction(fallback.action, fallback.invocation);
      else clearViewerKeyPending();
      // A mismatch ends the old command. Reprocess the key as the start of a
      // fresh command, but never leak the abandoned command's count into it.
      return handleViewerKeybindings(e) || !!fallback;
    }
    var exact = matches.find(function(candidate) {
      return candidate.next + 1 === candidate.binding.steps.length;
    });
    var longer = matches.filter(function(candidate) {
      return candidate.next + 1 < candidate.binding.steps.length;
    });
    if (exact && !longer.length) {
      if (exact.binding.steps[exact.next].capture === 'char') {
        exact.captures.char = eventKey(e);
      }
      return runViewerAction(
        exact.binding.action,
        candidateInvocation(exact.binding, e, exact.captures)
      );
    }
    keySequenceCandidates = longer.map(function(candidate) {
      var captures = Object.assign({}, candidate.captures);
      if (candidate.binding.steps[candidate.next].capture === 'char') {
        captures.char = eventKey(e);
      }
      return {
        binding: candidate.binding,
        next: candidate.next + 1,
        captures: captures,
      };
    });
    if (exact) {
      var exactCaptures = Object.assign({}, exact.captures);
      if (exact.binding.steps[exact.next].capture === 'char') {
        exactCaptures.char = eventKey(e);
      }
      keySequenceExactFallback = {
        action: exact.binding.action,
        invocation: candidateInvocation(exact.binding, e, exactCaptures),
      };
    } else {
      keySequenceExactFallback = null;
    }
    armKeySequenceTimeout(viewerSequenceWaitsForCharacter());
    return true;
  }

  function handleViewerKeybindings(e) {
    if (e.defaultPrevented || e.isComposing ||
        isViewerKeySuppressedTarget(e.target, e.key)) {
      if (keySequenceCandidates.length || viewerCountDigits) clearViewerKeyPending();
      return false;
    }
    // Never run page-level actions behind an open modal. Native dialog keys
    // (Escape, Enter, Space, Tab) and form accessibility remain untouched.
    if (document.querySelector('dialog[open]')) {
      if (keySequenceCandidates.length || viewerCountDigits) clearViewerKeyPending();
      return false;
    }
    if (e.key === 'Escape' && (keySequenceCandidates.length || viewerCountDigits)) {
      clearViewerKeyPending();
      return true;
    }
    var key = eventKey(e);
    // Physical modifier keydowns precede combinations such as `3J`. They are
    // not commands and must not discard the count before the printable key.
    if (key === 'Shift' || key === 'Control' || key === 'Alt' || key === 'Meta') {
      return false;
    }
    if (keySequenceCandidates.length) return continueKeybindingSequence(e);
    if (!e.ctrlKey && !e.metaKey && !e.altKey && /^[0-9]$/.test(key) &&
        (viewerCountDigits || key !== '0')) {
      clearViewerDigitFallback();
      viewerCountDigits = (viewerCountDigits + key).replace(/^0+/, '');
      if (!viewerCountDigits) viewerCountDigits = '0';
      if (parseInt(viewerCountDigits, 10) > VIEWER_MAX_COUNT) {
        viewerCountDigits = String(VIEWER_MAX_COUNT);
      }
      // Old configs may still bind a bare 1–9 (v2.1.20 used `4` for A4).
      // Counts take precedence when another key follows; an otherwise-bare
      // legacy digit fires after the normal mapping timeout.
      if (viewerCountDigits.length === 1) {
        var digitBinding = compiledKeybindings.find(function(binding) {
          return binding.steps.length === 1 && bindingStepMatches(binding.steps[0], e);
        });
        if (digitBinding) {
          // A timed-out bare digit is the configured mapping itself, not a
          // count applied to that mapping (for example a legacy `4 = page-a4`).
          var digitInvocation = { fromKey: false, fixedCount: 1, char: '' };
          viewerDigitFallbackTimer = setTimeout(function() {
            viewerDigitFallbackTimer = 0;
            runViewerAction(digitBinding.action, digitInvocation);
          }, viewerKeySequenceTimeoutMs);
        }
      }
      return true;
    }
    clearViewerDigitFallback();
    var handled = startKeybindingSequence(e);
    if (!handled && viewerCountDigits) viewerCountDigits = '';
    return handled;
  }

  setViewerKeybindings(
    (window.__mpConfig || {}).keybindings,
    (window.__mpConfig || {}).keybindingAliases,
    (window.__mpConfig || {}).keySequenceTimeoutMs
  );

  // Markdown custom blocks use generated, semantic markup rather than raw
  // author HTML.  Keep their reveal state in one helper so mouse activation,
  // live-patch state transfer, and full-body replacement all update the same
  // accessibility attributes.  `inert` prevents links and controls inside a
  // blurred body from receiving focus before the reader reveals it.
  function setMarkdownCustomBlockRevealed(block, revealed) {
    if (!block || !block.classList.contains('md-custom-reveal-blur')) return;
    var toggle = block.querySelector('.md-custom-toggle');
    var body = block.querySelector('.md-custom-body');
    if (!toggle || !body) return;
    block.classList.toggle('is-concealed', !revealed);
    toggle.setAttribute('aria-expanded', revealed ? 'true' : 'false');
    if (revealed) {
      body.removeAttribute('inert');
      body.removeAttribute('aria-hidden');
    } else {
      body.setAttribute('inert', '');
      body.setAttribute('aria-hidden', 'true');
    }
    var state = toggle.querySelector('.md-custom-toggle-state');
    if (state) state.textContent = revealed ? 'Hide' : 'Reveal';
  }

  function markdownCustomBlocksInRoots(roots) {
    var blocks = [];
    Array.from(roots || []).forEach(function(root) {
      if (!root || !root.querySelectorAll) return;
      if (root.matches && root.matches('.md-custom-reveal-blur[data-md-custom-name]')) {
        blocks.push(root);
      }
      root.querySelectorAll('.md-custom-reveal-blur[data-md-custom-name]').forEach(function(block) {
        blocks.push(block);
      });
    });
    return blocks;
  }

  function markdownCustomBlockIdentity(block) {
    return (block.dataset.mdCustomName || '') + '\u0000' +
      (block.dataset.mdCustomTitle || '');
  }

  function markdownCustomBlockContentIdentity(block) {
    return markdownCustomBlockIdentity(block) + '\u0000' +
      (block.dataset.mdCustomContent || '');
  }

  // Snapshot interactive state independently of top-level replacement
  // positions. Authored content is the safety boundary: config-only updates
  // and structural moves retain the body key, while editing or replacing a
  // spoiler changes it and intentionally restores the concealed default.
  function snapshotMarkdownCustomBlockState(oldRoots) {
    return markdownCustomBlocksInRoots(oldRoots).map(function(block) {
      return {
        contentIdentity: markdownCustomBlockContentIdentity(block),
        revealed: !block.classList.contains('is-concealed'),
      };
    });
  }

  // Pair only exact authored-content groups with unchanged cardinality. When
  // equal bodies repeat, document order is a safe tie-breaker because every
  // candidate reveals the same content. A count change remains concealed:
  // there is no sound way to identify which duplicate survived.
  function assignExactMarkdownContentState(snapshot, incoming) {
    var oldGroups = new Map();
    var newGroups = new Map();
    snapshot.forEach(function(prior) {
      var value = prior.contentIdentity;
      if (!oldGroups.has(value)) oldGroups.set(value, []);
      oldGroups.get(value).push(prior);
    });
    incoming.forEach(function(item) {
      var value = item.contentIdentity;
      if (!newGroups.has(value)) newGroups.set(value, []);
      newGroups.get(value).push(item);
    });
    newGroups.forEach(function(newMatches, value) {
      var oldMatches = oldGroups.get(value) || [];
      if (!oldMatches.length || oldMatches.length !== newMatches.length) return;
      for (var i = 0; i < oldMatches.length; i++) {
        newMatches[i].prior = oldMatches[i];
      }
    });
  }

  function restoreMarkdownCustomBlockState(snapshot, newRoots) {
    if (!snapshot || !snapshot.length) return;
    var incoming = markdownCustomBlocksInRoots(newRoots).map(function(block) {
      return {
        block: block,
        contentIdentity: markdownCustomBlockContentIdentity(block),
        prior: null,
      };
    });

    assignExactMarkdownContentState(snapshot, incoming);

    incoming.forEach(function(item) {
      if (item.prior && item.prior.revealed) {
        setMarkdownCustomBlockRevealed(item.block, true);
      }
    });
  }

  function copyMarkdownCustomBlockState(oldRoots, newRoots) {
    restoreMarkdownCustomBlockState(
      snapshotMarkdownCustomBlockState(oldRoots),
      newRoots
    );
  }

  document.addEventListener('focusin', function(e) {
    if (isEditableTarget(e.target) || isViewerInteractiveTarget(e.target)) {
      clearViewerKeyPending();
    }
  });
  window.addEventListener('blur', clearViewerKeyPending);

  document.addEventListener('click', function(e) {
    clearViewerKeyPending();
    // Any click outside the row-selected block drops the row selection and
    // its band — whichever branch below ends up handling the click.
    if (selectedMathRow &&
        !(selectedMathRow.block.isConnected && selectedMathRow.block.contains(e.target))) {
      clearSelectedMathRow();
    }
    // Refkey chip in the left margin → toggle its target as a margin card.
    // Same path as the typed-refkey input; works for theorems, sections,
    // floats, equations, and the per-row .eq-refkey-chip in multi-row
    // math environments.
    var refkeyChip = e.target.closest('.refkey-chip, .eq-refkey-chip[data-target]');
    if (refkeyChip) {
      e.preventDefault();
      e.stopPropagation();
      togglePinByRefkey(refkeyChip.dataset.target || refkeyChip.textContent || '');
      return;
    }
    var actionButton = e.target.closest('[data-viewer-action]');
    if (actionButton) {
      e.preventDefault();
      runViewerAction(actionButton.getAttribute('data-viewer-action'));
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
    var macrosCancel = e.target.closest('#macros-dialog-cancel');
    if (macrosCancel) {
      e.preventDefault();
      closeMacrosDialog();
      return;
    }
    var macrosSave = e.target.closest('#macros-dialog-save');
    if (macrosSave) {
      e.preventDefault();
      submitMacrosDialog();
      return;
    }
    var macrosLoad = e.target.closest('#macros-dialog-loadbtn');
    if (macrosLoad) {
      e.preventDefault();
      loadMacrosDialogFile();
      return;
    }
    var htmlAdd = e.target.closest('#macros-html-add-btn');
    if (htmlAdd) {
      e.preventDefault();
      addTextMacroFromForm();
      return;
    }
    var macrosUse = e.target.closest('#macros-dialog-usebtn');
    if (macrosUse) {
      e.preventDefault();
      registerMacrosOverride();
      return;
    }
    var configCancel = e.target.closest('#config-dialog-cancel');
    if (configCancel) {
      e.preventDefault();
      closeConfigDialog();
      return;
    }
    var configSave = e.target.closest('#config-dialog-save');
    if (configSave) {
      e.preventDefault();
      submitConfigDialog();
      return;
    }
    var logRefresh = e.target.closest('#log-panel-refresh');
    if (logRefresh) {
      e.preventDefault();
      refreshLogPanel();
      return;
    }
    var logClose = e.target.closest('#log-panel-close');
    if (logClose) {
      e.preventDefault();
      closeLogPanel();
      return;
    }
    var marginZoom = e.target.closest('.margin-card-zoom');
    if (marginZoom) {
      var zcard = marginZoom.closest('.margin-card');
      if (zcard) openMarginZoom(zcard);
      return;
    }
    var marginExpand = e.target.closest('.margin-card-expand');
    if (marginExpand) {
      var expandedCard = marginExpand.closest('.margin-card');
      if (expandedCard) toggleMarginExpand(expandedCard);
      return;
    }
    // Dismiss the magnify overlay: explicit close button, or a click on the
    // dialog backdrop (the <dialog> element itself, outside its content box).
    if (e.target.closest('#margin-zoom-close') || e.target.id === 'margin-zoom-dialog') {
      closeMarginZoom();
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
    // Reveal-source (editor spawn) fires only on the configured
    // gesture in `[viewer.source-jump] trigger = "..."`. No
    // fallbacks — the previous "alt-click also fires polling /jump"
    // shortcut duplicated the action when alt-click wasn't your
    // chosen trigger, and made the trigger setting feel non-
    // exclusive. The configured-trigger path itself fires both
    // /jump and /reveal-source so the nvim-plugin route is still
    // covered (v0.1.16).
    if (matchesRevealTrigger(e, 'click') && requestRevealSource(e)) {
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
      // Multi-row block: a plain click selects the CLICKED ROW for copy;
      // clicking the same row again widens back to the whole environment.
      // Only the FIRST click of a burst toggles (e.detail ≤ 1): the second
      // click of a double-click would immediately undo the first — and with
      // the double-click source-jump trigger, leave on/off depending on
      // what was selected before the jump.
      var rowInfo = (e.detail <= 1 && clickedMath.hasAttribute('data-row-tex-spans'))
        ? mathRowFromClick(clickedMath, e.target, e.clientY) : null;
      if (rowInfo) {
        if (selectedMathRow && selectedMathRow.block === clickedMath &&
            selectedMathRow.row === rowInfo.row) {
          clearSelectedMathRow();
          focusMathNode(clickedMath);
        } else if (!selectMathRow(clickedMath, rowInfo.row)) {
          focusMathNode(clickedMath);
        }
        return;
      }
      focusMathNode(clickedMath);
      return;
    }
    var suggestBtn = e.target.closest('#search-suggest .search-suggestion');
    if (suggestBtn) {
      acceptSearchSuggestion(parseInt(suggestBtn.dataset.suggestIndex, 10));
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
    var customToggle = e.target.closest('.md-custom-toggle');
    if (customToggle) {
      e.preventDefault();
      var customBlock = customToggle.closest('.md-custom-block');
      var reveal = customToggle.getAttribute('aria-expanded') !== 'true';
      setMarkdownCustomBlockRevealed(customBlock, reveal);
      var customTopBlock = customBlock && customBlock.closest('.blk');
      if (customTopBlock) invalidateOverlayMetrics([customTopBlock]);
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
      return;
    }
    var head = e.target.closest('.proof-head');
    if (head) {
      var clickedProof = head.closest('.proof');
      clickedProof.classList.toggle('folded');
      var clickedProofBlock = clickedProof.closest('.blk');
      if (clickedProofBlock) invalidateOverlayMetrics([clickedProofBlock]);
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
    }
  });
  document.addEventListener('keydown', function(e) {
    var searchInput = searchInputEl();
    if (e.target === searchInput) {
      var hasSuggest = searchSuggestions.length > 0;
      if (e.key === 'ArrowDown' && hasSuggest) {
        e.preventDefault();
        cycleSearchSuggestion(1);
        return;
      }
      if (e.key === 'ArrowUp' && hasSuggest) {
        e.preventDefault();
        cycleSearchSuggestion(-1);
        return;
      }
      if (e.key === 'Tab' && hasSuggest) {
        // Accept the highlighted suggestion, or the top one if none highlighted.
        e.preventDefault();
        acceptSearchSuggestion(searchSuggestIndex >= 0 ? searchSuggestIndex : 0);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        // First Esc dismisses the suggestion list; a second closes the panel.
        if (hasSuggest) clearSearchSuggestions();
        else closeSearchPanel();
        return;
      }
      if (e.key === 'Enter') {
        e.preventDefault();
        // Enter accepts a highlighted suggestion; otherwise runs the search.
        if (searchSuggestIndex >= 0) acceptSearchSuggestion(searchSuggestIndex);
        else runSearch(e.shiftKey ? !openSearchPanel.backwards : !!openSearchPanel.backwards);
        return;
      }
      return;
    }

    var head = e.target.closest('.proof-head');
    if (head && (e.key === 'Enter' || e.key === ' ')) {
      e.preventDefault();
      var keyedProof = head.closest('.proof');
      keyedProof.classList.toggle('folded');
      var keyedProofBlock = keyedProof.closest('.blk');
      if (keyedProofBlock) invalidateOverlayMetrics([keyedProofBlock]);
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
      return;
    }

    // Keyboard activation for the .eq-refkey-chip span (the per-row
    // equation refkey chips are spans with tabindex=0, not <button>s,
    // so we wire Enter/Space ourselves).
    var eqChip = e.target.closest('.eq-refkey-chip[data-target]');
    if (eqChip && (e.key === 'Enter' || e.key === ' ')) {
      e.preventDefault();
      togglePinByRefkey(eqChip.dataset.target || eqChip.textContent || '');
      return;
    }

    if (handleViewerKeybindings(e)) {
      e.preventDefault();
    }
  });

  var pendingTypeset = new Set();
  var typesetTimer = 0;
  var typesetBusy = false;
  var initialTypesetQueued = false;
  var mathObserver = null;
  var TYPESET_IDLE_MS = 120;
  var TYPESET_SMALL_IDLE_MS = 35;
  var TYPESET_BUSY_RETRY_MS = 80;

  // Native TikZ is much slower than MathJax, so diagrams are fetched one at a
  // time after the visible math queue has drained. Each content hash owns one
  // fetch for the lifetime of the page. Its SVG is retained as a blob URL:
  // scrolling away and back — or receiving a DOM patch containing the same
  // diagram — reuses the already-loaded bytes without asking the server to
  // run TeX again.
  var TIKZ_WINDOW = '150% 0px';
  var tikzStates = new Map();
  var tikzVisibleQueue = new Set();
  var tikzIdleQueue = new Set();
  var tikzVisibleObserver = null;
  var tikzWindowObserver = null;
  var tikzObserved = new Set();
  var tikzDrainTimer = 0;
  var tikzIdleHandle = 0;
  var tikzBusy = false;
  // Give MathJax a bounded head start. A blocked/broken MathJax asset must not
  // prevent native TikZ—which has no MathJax dependency—from ever rendering.
  var tikzMathStartupDeadline = performance.now() + 7000;

  function tikzStateFor(diagram) {
    if (!diagram || !diagram.isConnected) return null;
    var hash = diagram.getAttribute('data-tikz-hash') || '';
    var image = diagram.querySelector('.tikz-image[data-tikz-src]');
    var src = image && image.getAttribute('data-tikz-src');
    if (!/^[0-9a-fA-F]{16}$/.test(hash) || !src) return null;
    var state = tikzStates.get(hash);
    if (!state) {
      state = { hash: hash, src: src, status: 'new', priority: 'idle', objectUrl: '' };
      tikzStates.set(hash, state);
    }
    return state;
  }

  function liveTikzDiagrams(hash) {
    var page = pageEl();
    if (!page) return [];
    return Array.from(
      page.querySelectorAll('.tikz-diagram[data-tikz-hash="' + hash + '"]')
    );
  }

  function finishShowingTikz(diagram, image) {
    image.hidden = false;
    image.removeAttribute('data-tikz-loading');
    var pending = diagram.querySelector('.tikz-pending');
    if (pending) pending.hidden = true;
    diagram.removeAttribute('aria-busy');
    var block = diagram.closest('main#page > .blk');
    if (block) {
      scheduleBlockIntrinsicSizePriming([block]);
      invalidateOverlayMetrics([block], false);
    }
    scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
  }

  function showLoadedTikz(diagram, state) {
    var image = diagram.querySelector('.tikz-image[data-tikz-src]');
    if (!image || !state.objectUrl) return;
    if (image.getAttribute('src') === state.objectUrl) {
      if (image.complete && image.naturalWidth > 0) finishShowingTikz(diagram, image);
      return;
    }
    image.setAttribute('data-tikz-loading', '1');
    image.addEventListener('load', function() {
      finishShowingTikz(diagram, image);
    }, { once: true });
    image.addEventListener('error', function() {
      showTikzFailure(diagram);
    }, { once: true });
    image.src = state.objectUrl;
  }

  function showTikzFailure(diagram) {
    var image = diagram.querySelector('.tikz-image[data-tikz-src]');
    if (image) image.removeAttribute('data-tikz-loading');
    var pending = diagram.querySelector('.tikz-pending');
    if (pending) {
      pending.hidden = false;
      pending.textContent = 'TikZ diagram could not be loaded. Reload to try again.';
    }
    diagram.removeAttribute('aria-busy');
  }

  function unobserveTikz(diagram) {
    if (tikzVisibleObserver) tikzVisibleObserver.unobserve(diagram);
    if (tikzWindowObserver) tikzWindowObserver.unobserve(diagram);
    tikzObserved.delete(diagram);
  }

  function queueTikz(diagram, priority) {
    var state = tikzStateFor(diagram);
    if (!state) return;
    if (state.status === 'loaded') {
      unobserveTikz(diagram);
      showLoadedTikz(diagram, state);
      return;
    }
    if (state.status === 'failed' || state.status === 'loading') {
      unobserveTikz(diagram);
      if (state.status === 'failed') showTikzFailure(diagram);
      return;
    }
    state.status = 'queued';
    if (priority === 'visible') {
      state.priority = 'visible';
      tikzIdleQueue.delete(state.hash);
      tikzVisibleQueue.add(state.hash);
      liveTikzDiagrams(state.hash).forEach(unobserveTikz);
      scheduleTikzDrain(0);
    } else if (state.priority !== 'visible') {
      state.priority = 'idle';
      tikzIdleQueue.add(state.hash);
      if (tikzWindowObserver) tikzWindowObserver.unobserve(diagram);
      scheduleTikzIdle();
    }
  }

  function ensureTikzObservers() {
    if (tikzVisibleObserver || typeof IntersectionObserver === 'undefined') return;
    tikzVisibleObserver = new IntersectionObserver(function(entries) {
      entries.forEach(function(entry) {
        if (entry.isIntersecting) queueTikz(entry.target, 'visible');
      });
    });
    tikzWindowObserver = new IntersectionObserver(function(entries) {
      entries.forEach(function(entry) {
        if (entry.isIntersecting) queueTikz(entry.target, 'idle');
      });
    }, { rootMargin: TIKZ_WINDOW });
  }

  function collectTikzDiagrams(root) {
    if (!root || root.nodeType !== 1) return [];
    var diagrams = [];
    if (root.matches && root.matches('.tikz-diagram[data-tikz-hash]')) {
      diagrams.push(root);
    }
    if (root.querySelectorAll) {
      root.querySelectorAll('.tikz-diagram[data-tikz-hash]').forEach(function(diagram) {
        diagrams.push(diagram);
      });
    }
    return diagrams;
  }

  function observeTikz(root) {
    ensureTikzObservers();
    collectTikzDiagrams(root).forEach(function(diagram) {
      var state = tikzStateFor(diagram);
      if (!state) return;
      if (state.status === 'loaded') {
        showLoadedTikz(diagram, state);
      } else if (state.status === 'failed') {
        showTikzFailure(diagram);
      } else if (typeof IntersectionObserver === 'undefined') {
        queueTikz(diagram, 'idle');
      } else {
        // A near-viewport diagram may already be in the idle queue. Keep the
        // exact-viewport observer armed so it can still be promoted.
        tikzObserved.add(diagram);
        tikzVisibleObserver.observe(diagram);
        if (state.status === 'new') tikzWindowObserver.observe(diagram);
      }
    });
  }

  function pruneTikzStates() {
    var page = pageEl();
    if (!page) return;
    Array.from(tikzObserved).forEach(function(diagram) {
      if (!diagram.isConnected || !page.contains(diagram)) unobserveTikz(diagram);
    });
    var liveHashes = new Set();
    collectTikzDiagrams(page).forEach(function(diagram) {
      var hash = diagram.getAttribute('data-tikz-hash') || '';
      if (hash) liveHashes.add(hash);
    });
    tikzStates.forEach(function(state, hash) {
      if (liveHashes.has(hash) || state.status === 'loading') return;
      tikzVisibleQueue.delete(hash);
      tikzIdleQueue.delete(hash);
      if (state.objectUrl) window.URL.revokeObjectURL(state.objectUrl);
      tikzStates.delete(hash);
    });
  }

  function startTikzScheduler() {
    var page = pageEl();
    if (page) observeTikz(page);
  }

  function visibleMathHasTikzPriority() {
    var mathStartupPending = !initialTypesetQueued &&
      performance.now() < tikzMathStartupDeadline;
    return mathStartupPending || !!printFlushPromise || typesetBusy ||
      !!typesetTimer || pendingTypeset.size > 0;
  }

  function idleMathHasTikzPriority() {
    return visibleMathHasTikzPriority() || windowQueue.size > 0;
  }

  function nextTikzState(queue, priority) {
    while (queue.size) {
      var hash = queue.values().next().value;
      queue.delete(hash);
      var state = tikzStates.get(hash);
      if (state && state.status === 'queued' && state.priority === priority) return state;
    }
    return null;
  }

  function scheduleTikzDrain(delay) {
    if (tikzDrainTimer) return;
    tikzDrainTimer = setTimeout(drainVisibleTikz, delay);
  }

  function drainVisibleTikz() {
    tikzDrainTimer = 0;
    if (!tikzVisibleQueue.size) {
      scheduleTikzIdle();
      return;
    }
    if (tikzBusy || visibleMathHasTikzPriority()) {
      scheduleTikzDrain(TYPESET_BUSY_RETRY_MS);
      return;
    }
    var state = nextTikzState(tikzVisibleQueue, 'visible');
    if (state) loadTikzState(state);
    else scheduleTikzIdle();
  }

  function scheduleTikzIdle() {
    if (tikzIdleHandle || !tikzIdleQueue.size) return;
    var run = function() {
      tikzIdleHandle = 0;
      drainIdleTikz();
    };
    if (window.requestIdleCallback) {
      tikzIdleHandle = window.requestIdleCallback(run, { timeout: 1500 });
    } else {
      tikzIdleHandle = setTimeout(run, 500);
    }
  }

  function drainIdleTikz() {
    if (tikzVisibleQueue.size) {
      scheduleTikzDrain(0);
      return;
    }
    if (tikzBusy || idleMathHasTikzPriority()) {
      // requestIdleCallback can fire repeatedly while MathJax awaits a
      // promise. Add a small backoff instead of spinning idle callbacks.
      tikzIdleHandle = setTimeout(function() {
        tikzIdleHandle = 0;
        scheduleTikzIdle();
      }, TYPESET_BUSY_RETRY_MS);
      return;
    }
    var state = nextTikzState(tikzIdleQueue, 'idle');
    if (state) loadTikzState(state);
  }

  function loadTikzState(state) {
    var diagrams = liveTikzDiagrams(state.hash);
    if (!diagrams.length) {
      state.status = 'new';
      state.priority = 'idle';
      if (tikzVisibleQueue.size) scheduleTikzDrain(0);
      else scheduleTikzIdle();
      return;
    }
    diagrams.forEach(unobserveTikz);
    state.status = 'loading';
    tikzBusy = true;
    fetch(state.src, { cache: 'force-cache' })
      .then(function(response) {
        if (!response.ok) throw new Error('HTTP ' + response.status);
        return response.blob();
      })
      .then(function(svg) {
        state.objectUrl = window.URL.createObjectURL(svg);
        state.status = 'loaded';
        liveTikzDiagrams(state.hash).forEach(function(diagram) {
          showLoadedTikz(diagram, state);
        });
      })
      .catch(function(error) {
        console.error('mathpreview TikZ:', error);
        state.status = 'failed';
        liveTikzDiagrams(state.hash).forEach(showTikzFailure);
      })
      .finally(function() {
        tikzBusy = false;
        pruneTikzStates();
        if (tikzVisibleQueue.size) scheduleTikzDrain(0);
        else scheduleTikzIdle();
      });
  }

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
    // The row-copy offsets index into data-tex, which can drift under an
    // equal hash (the hash covers the RENDERED math — a \ref key rename that
    // resolves to the same number changes the raw source only). Stale
    // offsets against a fresh data-tex would slice garbage, so they travel
    // together.
    copyAttr(oldEl, newEl, 'data-row-tex-spans');
    copyAttr(oldEl, newEl, 'data-mathjax-tex');
    copyAttr(oldEl, newEl, 'title');
    copyAttr(oldEl, newEl, 'tabindex');
    // MathJax reads the TeX from the element's text/HTML content, not from
    // `data-tex`. If a raw math node is reused before its deferred typeset
    // pass runs, keep that raw source synchronized with the fresh server
    // node. Already-typeset nodes keep their <mjx-container> subtree.
    if (!oldEl.querySelector('mjx-container')) {
      oldEl.innerHTML = newEl.innerHTML;
    }
  }

  function pageBlocks(page) {
    return Array.prototype.filter.call(page.children, function(el) {
      return el.classList && el.classList.contains('blk');
    });
  }

  // A typeset batch (or a priming pass) changes block heights. When that
  // happens at or above the viewport top, the content the reader is on
  // shifts — Chromium's native scroll anchoring compensates, WebKit has
  // none, so Space / Shift+Space page motions drifted around long
  // equations. Capture the first element fully below the viewport top
  // before the work and restore its on-screen position after: the
  // correction is the element's MEASURED displacement, so wherever native
  // anchoring already fixed it the delta is ~0 and this is a no-op.
  //
  // When a block straddles the viewport top, descend into it: anchoring the
  // NEXT top-level block would misattribute growth in the straddler's
  // visible lower part (below the reading point) as displacement and scroll
  // for content the reader watched grow on screen.
  function descendToViewportTopAnchor(root, top) {
    var node = root;
    for (var depth = 0; depth < 12; depth++) {
      var children = node.children;
      var straddler = null;
      for (var i = 0; i < children.length; i++) {
        var rect = children[i].getBoundingClientRect();
        if (!(rect.height > 0) || rect.bottom <= top) continue;
        if (rect.top >= top) return { el: children[i], top: rect.top };
        straddler = children[i];
        break;
      }
      if (!straddler) return null;
      node = straddler;
    }
    return null;
  }

  function captureTypesetViewportAnchor() {
    if (window.scrollY < 0.5) return null;
    var page = pageEl();
    if (!page) return null;
    var top = topbarOffset();
    var blocks = pageBlocks(page);
    for (var i = 0; i < blocks.length; i++) {
      var rect = blocks[i].getBoundingClientRect();
      if (!(rect.height > 0) || rect.bottom <= top) continue;
      var el = blocks[i];
      var elTop = rect.top;
      if (rect.top < top) {
        var refined = descendToViewportTopAnchor(blocks[i], top);
        if (!refined) continue;
        el = refined.el;
        elTop = refined.top;
      }
      return {
        el: el,
        top: elTop,
        scrollX: window.scrollX,
        scrollY: window.scrollY,
      };
    }
    return null;
  }

  function settleTypesetViewportAnchor(anchor) {
    if (!anchor || !anchor.el.isConnected) return;
    // Any interleaved scroll makes the viewport-relative delta unsound: a
    // user keypress/fling landing in an engine yield window must not be
    // reverted, and on Chromium the native anchoring adjustment itself
    // moves scrollY (bailing there loses nothing — delta was ~0 by
    // design). On WebKit a pure layout shift leaves scrollY untouched, so
    // the stationary-reader correction still fires.
    if (Math.abs(window.scrollX - anchor.scrollX) >= 1 ||
        Math.abs(window.scrollY - anchor.scrollY) >= 1) {
      return;
    }
    var delta = anchor.el.getBoundingClientRect().top - anchor.top;
    if (Math.abs(delta) >= 0.5) {
      window.scrollBy({ left: 0, top: delta, behavior: 'auto' });
    }
  }

  // MathJax can finish while a lazy block is temporarily forced visible.
  // WebKit does not reliably teach `contain-intrinsic-size:auto` the height
  // measured in that state, so cache it explicitly before restoring
  // content-visibility. Otherwise the block falls back to 180px off-screen
  // and changes the document geometry when page scrolling activates it.
  function seedTypesetBlockIntrinsicSizes(nodes) {
    var blocks = new Set();
    nodes.forEach(function(node) {
      var block = node.closest && node.closest('main#page > .blk');
      if (block && block.isConnected) blocks.add(block);
    });
    seedCurrentBlockIntrinsicSizes(blocks);
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
      // A `0` entry means this position's metadata (id/hash/src/anchors) is
      // unchanged from the last broadcast — skip it entirely. On a large doc
      // that's nearly every block for a within-line edit, avoiding tens of
      // thousands of setAttribute calls per keystroke.
      var b = blocks[i];
      if (!b) continue;
      els[i].id = b.id;
      els[i].setAttribute('data-blockhash', b.hash);
      if (b.src) els[i].setAttribute('data-src', b.src);
      else els[i].removeAttribute('data-src');
      syncBlockSourceAnchors(els[i], b.anchors);
    }
  }

  function indexMathByHash(root, oldByHash) {
    function add(oldEl) {
      var arr = oldByHash.get(oldEl.dataset.hash);
      if (!arr) { arr = []; oldByHash.set(oldEl.dataset.hash, arr); }
      arr.push(oldEl);
    }
    // The root can BE a math node: display math inside a chunked body
    // (proof/theorem/callout/quote) is emitted as a direct sibling of the
    // proof-para chunks, so a blocksub op's removed child is sometimes the
    // math element itself — querySelectorAll alone would skip it, losing it
    // as a reuse/stale donor (cf. collectRawMath, which handles root-self
    // the same way).
    if (root.matches && root.matches('.math[data-hash]')) add(root);
    root.querySelectorAll('.math[data-hash]').forEach(add);
  }

  function isUntypesetMathNode(node) {
    return !!(node && node.isConnected && isRawMathNode(node));
  }

  // A STALE node (data-mp-stale, see seedStaleMath) holds the previous
  // render's <mjx-container> as a placeholder while its new TeX waits in the
  // typeset queue — it still needs a typeset, so it counts as raw here even
  // though it contains a container.
  function isRawMathNode(node) {
    return !!(node && node.matches &&
      node.matches('.math[data-hash]') &&
      (node.hasAttribute('data-mp-stale') || !node.querySelector('mjx-container')));
  }

  function syncMathSourceText(node) {
    if (!isRawMathNode(node)) return;
    // A stale node's .math-source holds the previous render, not source
    // text — leave it visible; the engine reads the TeX to typeset from
    // data-mathjax-tex, not from the content.
    if (node.hasAttribute('data-mp-stale')) return;
    var tex = node.getAttribute('data-mathjax-tex');
    var source = node.querySelector('.math-source');
    if (source && tex !== null && source.textContent !== tex) {
      source.textContent = tex;
    }
  }

  // Anti-flash for live typing: when an edited equation's hash changes, the
  // fresh server node arrives raw and would show its LaTeX source text until
  // the (debounced) typeset queue re-renders it — a visible flash on every
  // keystroke inside a long equation. Instead, move the outgoing node's
  // <mjx-container> into the incoming node as a placeholder and mark it
  // data-mp-stale: the previous render stays visible and is swapped for the
  // new one in a single replaceChildren when the typeset lands.
  // Pairing is by element id (label-derived ids are stable; positional
  // `<prefix>-g<block>-<n>` ids are stable for a within-equation edit) with
  // matching display-ness, so a ghost of a DIFFERENT equation can't show —
  // unpaired receivers just fall back to today's raw-source behavior.
  function seedStaleMath(donors, receivers) {
    if (!receivers.length) return;
    var donorById = new Map();
    donors.forEach(function(d) {
      if (d.id && !donorById.has(d.id) && d.querySelector('mjx-container')) {
        donorById.set(d.id, d);
      }
    });
    if (!donorById.size) return;
    receivers.forEach(function(r) {
      if (!r.id || !isRawMathNode(r) || r.querySelector('mjx-container')) return;
      var donor = donorById.get(r.id);
      if (!donor) return;
      if (donor.classList.contains('display') !== r.classList.contains('display')) return;
      var source = r.querySelector('.math-source');
      var container = donor.querySelector('mjx-container');
      if (!source || !container) return;
      donorById.delete(r.id);
      // A row-copy selection band inside the donor must not ride along: the
      // selection state points at the (now replaced) old block, so a band in
      // the placeholder would promise a row copy that ⌘C can't deliver.
      container.querySelectorAll('rect.mp-row-select').forEach(function(sel) {
        if (sel.parentNode) sel.parentNode.removeChild(sel);
      });
      source.replaceChildren(container);
      r.setAttribute('data-mp-stale', '1');
    });
  }

  function queueUntypesetMath(root) {
    if (!root || !root.querySelectorAll) return;
    var nodes = Array.from(root.querySelectorAll('.math[data-hash]')).filter(isRawMathNode);
    queueTypeset(nodes);
  }

  function scheduleTypesetFlush(delay) {
    if (!pendingTypeset.size || typesetTimer) return;
    typesetTimer = setTimeout(flushTypeset, delay);
  }

  // Viewport-lazy typesetting. Blocks have `content-visibility: auto`, so
  // math inside a SKIPPED (far off-screen) block is not typeset up front —
  // MathJax's measurements inside skipped subtrees are slow AND wasted (a
  // 60-page paper has thousands of equations; cold-load typeset measured 170s
  // eager vs. near-instant lazy). Each such block typesets the moment it
  // first becomes relevant (scrolled near, focused, scrollIntoView'd), via
  // contentvisibilityautostatechange. Browsers without the event (or
  // checkVisibility) fall back to eager typesetting. Known tradeoff: the
  // browser's own Cmd+P prints never-scrolled math untypeset — the toolbar
  // print button (real latexmk) is unaffected.
  var lazyTypesetOk = typeof ContentVisibilityAutoStateChangeEvent !== 'undefined' &&
    Element.prototype.checkVisibility;
  function inSkippedBlock(node) {
    if (!lazyTypesetOk || !node.checkVisibility) return false;
    try { return !node.checkVisibility({ contentVisibilityAuto: true }); }
    catch (e) { return false; }
  }
  function deferTypesetUntilVisible(node) {
    var blk = node.closest && node.closest('main#page > .blk');
    if (!blk) return false;
    if (!blk.__mpLazyTypeset) {
      blk.__mpLazyTypeset = true;
      blk.addEventListener('contentvisibilityautostatechange', function onState(e) {
        if (e.skipped) return;
        // viewer.js may briefly lift a skipped block to cache lightweight key
        // and line-number geometry. That pass must not turn local typesetting
        // into an eager whole-document MathJax run.
        if (blk.__mpOverlayPrelayoutToken) return;
        blk.__mpLazyTypeset = false;
        blk.removeEventListener('contentvisibilityautostatechange', onState);
        queueUntypesetMath(blk);
      });
    }
    return true;
  }

  function queueTypeset(nodes) {
    nodes.forEach(function(node) {
      if (!isRawMathNode(node)) return;
      if (inSkippedBlock(node) && deferTypesetUntilVisible(node)) return;
      syncMathSourceText(node);
      pendingTypeset.add(node);
      node.classList.add('math-pending');
    });
    if (!pendingTypeset.size) {
      // Everything this round was deferred. For a node in a genuinely
      // content-visibility-skipped block that's correct — the block's
      // state-change listener will queue it on un-skip. But checkVisibility
      // is also false for nodes hidden by display:none inside a RENDERED
      // block (folded proof bodies, footnote popovers), and for those the
      // state-change event never fires. With no flush, nothing would re-arm
      // the viewport window observer either, so a raw — or worse, stale
      // (data-mp-stale, showing the pre-edit equation) — node could wait
      // forever. Re-arm the observer here: drainWindowTypeset ignores
      // display state and typesets hidden nodes correctly, and for far-away
      // skipped blocks the observer only fires when they near the viewport,
      // so lazy loading is preserved.
      observeTypesetWindow();
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, false);
      return;
    }
    scheduleTypesetFlush(pendingTypeset.size <= 4 ? TYPESET_SMALL_IDLE_MS : TYPESET_IDLE_MS);
  }

  function queueInitialTypeset() {
    if (initialTypesetQueued) return;
    var page = pageEl();
    if (!page) return;
    initialTypesetQueued = true;
    queueUntypesetMath(page);
  }

  // Cmd/Ctrl+P support under viewport-lazy typesetting: typeset EVERYTHING
  // (batched, with progress in the status pill) before opening the print
  // dialog, so the printout is complete while normal viewing stays lazy.
  // `print-preparing` lifts content-visibility during the flush — MathJax
  // measuring inside skipped subtrees is pathologically slow. One-time per
  // session: typeset SVGs persist, so subsequent prints start instantly.
  var printFlushPromise = null;
  var printFlushCancelled = false;
  // The progress dialog: tells the user WHY the print dialog hasn't opened yet
  // (lazy typesetting means the whole document must typeset once), shows live
  // progress, and offers Cancel. `note` swaps the message for the File > Print
  // path, where the browser's dialog can't be delayed.
  function printPrepDialog() { return document.getElementById('print-prep-dialog'); }
  function openPrintPrepDialog(total, note) {
    var dlg = printPrepDialog();
    if (!dlg || typeof dlg.showModal !== 'function') return;
    var noteEl = document.getElementById('print-prep-note');
    if (noteEl && note) noteEl.textContent = note;
    var bar = document.getElementById('print-prep-progress');
    if (bar) { bar.max = total; bar.value = 0; }
    updatePrintPrepProgress(0, total);
    if (!dlg.open) dlg.showModal();
  }
  function updatePrintPrepProgress(done, total) {
    var bar = document.getElementById('print-prep-progress');
    if (bar) bar.value = done;
    var count = document.getElementById('print-prep-count');
    if (count) count.textContent = done + ' / ' + total + ' equations';
  }
  function closePrintPrepDialog() {
    var dlg = printPrepDialog();
    if (dlg && dlg.open) dlg.close();
  }
  // Cancel button and Esc (the dialog's native `cancel` event) both abort the
  // flush between batches; already-typeset math stays (it's pure win).
  (function wirePrintPrepCancel() {
    var btn = document.getElementById('print-prep-cancel');
    if (btn) btn.addEventListener('click', function() {
      printFlushCancelled = true;
      closePrintPrepDialog();
    });
    var dlg = printPrepDialog();
    if (dlg) dlg.addEventListener('cancel', function() { printFlushCancelled = true; });
  })();

  // Windowed typesetting: typeset only the region around the viewport — the
  // visible blocks plus a buffer above and below — rather than the whole
  // document. An IntersectionObserver with a generous rootMargin (the
  // TYPESET_WINDOW below) reports each block as it approaches the viewport; we
  // typeset that block then, lifting its containment just for the typeset
  // (MathJax measures pathologically slowly inside content-visibility-skipped
  // subtrees) and restoring it after. The rest of the document stays untypeset
  // until you scroll to it — memory and CPU track what you actually read, and
  // Cmd+P flushes the whole document on demand (see typesetAllForPrint).
  var TYPESET_WINDOW = '150% 0px'; // ~1.5 viewports of buffer above and below
  var windowQueue = new Set();
  var windowDrainTimer = 0;
  var typesetObserver = null;
  function ensureTypesetObserver() {
    if (typesetObserver || typeof IntersectionObserver === 'undefined') return;
    typesetObserver = new IntersectionObserver(function(entries) {
      entries.forEach(function(e) {
        if (!e.isIntersecting) return;
        typesetObserver.unobserve(e.target); // typeset once; re-observed if re-rendered
        if (e.target.querySelector('.math[data-hash] .math-source')) {
          windowQueue.add(e.target);
        }
      });
      if (windowQueue.size) scheduleWindowDrain(0);
    }, { rootMargin: TYPESET_WINDOW });
  }
  // (Re)observe every top-level block that still holds raw math. Cheap and
  // idempotent — the observer ignores already-observed elements, and blocks
  // replaced by a patch are new nodes that get observed here.
  function observeTypesetWindow() {
    ensureTypesetObserver();
    if (!typesetObserver) return;
    var page = pageEl();
    if (!page) return;
    pageBlocks(page).forEach(function(blk) {
      if (blk.querySelector('.math[data-hash] .math-source')) typesetObserver.observe(blk);
    });
  }
  function scheduleWindowDrain(delay) {
    if (windowDrainTimer) return;
    windowDrainTimer = setTimeout(drainWindowTypeset, delay);
  }

  // 'local' (default) = window only; 'background' = also fill the rest while
  // idle. Read from the config; live-updated by applyViewerConfig / setTypesetMode.
  function typesetMode() {
    return (window.__mpConfig && window.__mpConfig.typesetMode) === 'background'
      ? 'background' : 'local';
  }
  // Background fill: in 'background' mode, after the window is handled, march
  // through the remaining raw blocks one at a time during idle moments —
  // lifting each block's containment just for its typeset (as the window drain
  // does) — so deep sections and Cmd+P never wait. Self-gates on the mode, so
  // switching to 'local' stops it; yields to typing (typesetBusy) and prints.
  var bgFillTimer = 0;
  function scheduleBgFill(delay) {
    if (bgFillTimer || typesetMode() !== 'background') return;
    bgFillTimer = setTimeout(bgFillStep, delay);
  }
  async function bgFillStep() {
    bgFillTimer = 0;
    if (typesetMode() !== 'background') return;
    if (printFlushPromise || typesetBusy || windowQueue.size) { scheduleBgFill(600); return; }
    var page = pageEl();
    if (!page) return;
    var blocks = pageBlocks(page);
    var target = null;
    for (var i = 0; i < blocks.length; i++) {
      if (blocks[i].querySelector('.math[data-hash] .math-source') &&
          Array.prototype.some.call(
            blocks[i].querySelectorAll('.math[data-hash]'), isRawMathNode)) {
        target = blocks[i];
        break;
      }
    }
    if (!target) return; // whole document typeset — done until the next render
    var nodes = Array.from(target.querySelectorAll('.math[data-hash]'))
      .filter(isRawMathNode).slice(0, 40); // cap so one huge block can't jank
    typesetBusy = true;
    var anchor = captureTypesetViewportAnchor();
    var lifted = target.style.contentVisibility === '';
    var originalContain = target.style.contain;
    if (lifted) {
      target.style.contain = 'layout style paint';
      target.style.contentVisibility = 'visible';
    }
    try {
      nodes.forEach(syncMathSourceText);
      await window.__mpEngine.typeset(nodes);
      seedTypesetBlockIntrinsicSizes(nodes);
    } catch (err) {
      console.error('mathpreview background fill:', err);
      typesetBusy = false;
      return; // stop on engine error rather than spinning
    } finally {
      if (lifted) {
        target.style.contentVisibility = '';
        target.style.contain = originalContain;
      }
      settleTypesetViewportAnchor(anchor);
      typesetBusy = false;
    }
    scheduleBgFill(120);
  }
  function setTypesetMode(mode) {
    window.__mpConfig = window.__mpConfig || {};
    window.__mpConfig.typesetMode = (mode === 'background') ? 'background' : 'local';
    if (window.__mpConfig.typesetMode === 'background') scheduleBgFill(600);
  }
  async function drainWindowTypeset() {
    windowDrainTimer = 0;
    if (!windowQueue.size) return;
    // Yield to the print flush, an in-progress typeset batch, and visible
    // native diagrams. The exact-visible math queue above still runs first;
    // this queue is the wider 150%-viewport look-ahead buffer.
    if (printFlushPromise || typesetBusy || tikzVisibleQueue.size || tikzBusy) {
      scheduleWindowDrain(150);
      return;
    }
    var blk = windowQueue.values().next().value;
    windowQueue.delete(blk);
    if (blk && blk.isConnected) {
      var nodes = Array.from(blk.querySelectorAll('.math[data-hash]')).filter(isRawMathNode);
      if (nodes.length) {
        typesetBusy = true;
        var anchor = captureTypesetViewportAnchor();
        var lifted = blk.style.contentVisibility === '';
        var originalContain = blk.style.contain;
        if (lifted) {
          blk.style.contain = 'layout style paint';
          blk.style.contentVisibility = 'visible';
        }
        try {
          nodes.forEach(syncMathSourceText);
          await window.__mpEngine.typeset(nodes);
          seedTypesetBlockIntrinsicSizes(nodes);
        } catch (err) {
          console.error('mathpreview typeset:', err);
        } finally {
          if (lifted) {
            blk.style.contentVisibility = '';
            blk.style.contain = originalContain;
          }
          settleTypesetViewportAnchor(anchor);
          typesetBusy = false;
        }
      }
    }
    if (windowQueue.size) scheduleWindowDrain(0);
  }

  // Resolves to true when the full flush completed, false when cancelled.
  function typesetAllForPrint(note) {
    if (printFlushPromise) return printFlushPromise;
    var page = pageEl();
    var nodes = page
      ? Array.from(page.querySelectorAll('.math[data-hash]')).filter(isRawMathNode)
      : [];
    if (!nodes.length) return Promise.resolve(true);
    printFlushCancelled = false;
    printFlushPromise = (async function() {
      document.body.classList.add('print-preparing');
      openPrintPrepDialog(nodes.length, note);
      var completed = false;
      try {
        while (typesetBusy) {
          await new Promise(function(r) { setTimeout(r, 120); });
        }
        typesetBusy = true;
        var BATCH = 200;
        for (var i = 0; i < nodes.length; i += BATCH) {
          if (printFlushCancelled) break;
          var batch = nodes.slice(i, i + BATCH).filter(isUntypesetMathNode);
          if (batch.length) {
            batch.forEach(syncMathSourceText);
            setStatus('updating',
              '↻ preparing print: typeset ' + Math.min(i + BATCH, nodes.length) + '/' + nodes.length);
            await window.__mpEngine.typeset(batch);
            seedTypesetBlockIntrinsicSizes(batch);
          }
          updatePrintPrepProgress(Math.min(i + BATCH, nodes.length), nodes.length);
        }
        completed = !printFlushCancelled;
        setStatus('live', completed ? '● live (print ready)' : '● live (print prep cancelled)');
      } catch (e) {
        console.error('mathpreview print typeset:', e);
        setStatus('dead', '○ print typeset error');
      } finally {
        typesetBusy = false;
        document.body.classList.remove('print-preparing');
        closePrintPrepDialog();
        printFlushPromise = null;
      }
      return completed;
    })();
    return printFlushPromise;
  }

  async function flushTypeset() {
    typesetTimer = 0;
    if (typesetBusy) {
      typesetTimer = setTimeout(flushTypeset, TYPESET_BUSY_RETRY_MS);
      return;
    }
    if (!pendingTypeset.size) return;
    if (!window.__mpEngine || !window.__mpEngine.isReady()) {
      typesetTimer = setTimeout(flushTypeset, TYPESET_IDLE_MS);
      return;
    }

    var nodes = Array.from(pendingTypeset).filter(isUntypesetMathNode);
    pendingTypeset.clear();
    if (!nodes.length) return;

    typesetBusy = true;
    var anchor = captureTypesetViewportAnchor();
    setStatus('updating', '↻ typesetting ' + nodes.length + ' math');
    var tStart = performance.now();
    try {
      await window.__mpEngine.typeset(nodes);
      seedTypesetBlockIntrinsicSizes(nodes);
      settleTypesetViewportAnchor(anchor);
      var ms = Math.round(performance.now() - tStart);
      nodes.forEach(function(node) { node.classList.remove('math-pending'); });
      restoreMathSearchHighlights();
      restoreTextSearchHighlights();
      restoreEditorSearchHighlights();
      // Math is only now typeset; re-apply any per-row selection highlight that
      // fell back to whole-block because the SVG didn't exist yet.
      restoreSourceRange();
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
        scheduleTypesetFlush(TYPESET_IDLE_MS);
      }
      // Watch the rest of the document so each block typesets as it nears the
      // viewport; in 'background' mode also fill the rest while idle.
      observeTypesetWindow();
      scheduleBgFill(3000);
    }
  }

  function collectRawMath(node, out) {
    if (!node || node.nodeType !== 1) return;
    if (isRawMathNode(node)) out.push(node);
    if (!node.querySelectorAll) return;
    node.querySelectorAll('.math[data-hash]').forEach(function(math) {
      if (isRawMathNode(math)) out.push(math);
    });
  }

  function startMathObserver() {
    var page = pageEl();
    if (!page || !window.MutationObserver) return;
    if (mathObserver) mathObserver.disconnect();
    mathObserver = new MutationObserver(function(records) {
      var nodes = [];
      var metricBlocks = new Set();
      records.forEach(function(record) {
        var target = record.target.nodeType === Node.ELEMENT_NODE
          ? record.target : record.target.parentElement;
        var metricBlock = target && target.closest
          ? target.closest('main#page > .blk') : null;
        if (metricBlock) metricBlocks.add(metricBlock);
        record.addedNodes.forEach(function(node) { collectRawMath(node, nodes); });
      });
      // Patches/typeset completion already schedule the normal trailing
      // navigation refresh. Invalidate now but do not pull the expensive
      // page-level layer rebuild into the current typing frame.
      if (metricBlocks.size) {
        invalidateOverlayMetrics(Array.from(metricBlocks), false);
      }
      if (nodes.length) queueTypeset(nodes);
    });
    mathObserver.observe(page, { childList: true, subtree: true });
  }

  // Apply a server-computed block-level patch. Ops are positional ranges
  // against top-level .blk elements; after applying them we retag block ids
  // by position. This preserves shifted unchanged blocks without the id
  // collisions caused by insertion-before-existing-content edits.
  async function applyPatch(ops, blocksMeta) {
    var tStart = performance.now();
    var page = document.getElementById('page');
    var viewportAnchor = ops.length ? beginLivePatchViewportAnchor(page) : null;
    setStatus('updating', '↻ patching');
    var tpl = document.createElement('template');
    var needTypeset = [];
    var reusedMath = 0, totalMath = 0;
    var replacedBlocks = 0, insertedBlocks = 0, removedBlocks = 0;
    var reusedBlocks = 0;
    var reusedSubBlockAttr = 'data-mp-reused-subblock';
    // Blocks this patch actually touched — callers scope their whole-page
    // passes (proof re-fold, chip decoration) to these. Null when a rebuild
    // op makes precise attribution impractical (rare; callers fall back to
    // the whole page).
    var touchedRoots = [];
    var hasRebuild = ops.some(function(op) { return op.type === 'rebuild'; });
    var detachPage = ops.length > 8 || hasRebuild;
    var pageParent = detachPage ? page.parentNode : null;
    var pageNextSibling = detachPage ? page.nextSibling : null;
    // Once #page is detached its boxes measure zero. Bulk/rebuild patches
    // therefore snapshot only on that uncommon path, before detachment. The
    // normal one-range typing path measures just the paired block below.
    var detachedBlockSizes = null;
    if (detachPage) {
      detachedBlockSizes = new WeakMap();
      pageBlocks(page).forEach(function(block) {
        detachedBlockSizes.set(block, snapshotBlockIntrinsicSize(block));
      });
    }
    if (pageParent) pageParent.removeChild(page);

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
          copyMarkdownCustomBlockState(
            blocks.slice(start, start + removeCount),
            newFragBlocks
          );
          var pairCount = Math.min(removeCount, newFragBlocks.length);
          for (var pp = 0; pp < pairCount; pp++) {
            var pairedOldBlock = blocks[start + pp];
            var pairedSize = detachedBlockSizes
              ? detachedBlockSizes.get(pairedOldBlock)
              : snapshotBlockIntrinsicSize(pairedOldBlock);
            seedBlockIntrinsicSize(newFragBlocks[pp], pairedSize);
            transplantSubBlocks(pairedOldBlock, newFragBlocks[pp]);
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
            newFragBlocks.forEach(function(b) { touchedRoots.push(b); });
            page.insertBefore(frag, anchor);
            insertedBlocks += inserted;
          }
          replacedBlocks += Math.min(removeCount, inserted);
        } else if (op.type === 'blocksub') {
          // In-block sub-diff: the server kept the proof/theorem scaffolding
          // and only re-sent a contiguous run of changed body-child elements.
          // We splice them into the existing block element, so the typeset
          // MathJax in every unchanged proof paragraph stays put.
          var bsBlocks = pageBlocks(page);
          var bsBlock = bsBlocks[op.index || 0];
          // Every chunk-capable block type's body container (must match the
          // server's write_chunked_children callers: proof, theorem, callout,
          // quote, letter). Document order finds the OUTERMOST container first, which
          // is the one the server chunked. A miss here would silently drop the
          // edit and leave the block stale until the next full update.
          var bsBody = bsBlock && bsBlock.querySelector
            ? bsBlock.querySelector('.proof-body, .thm-body, .callout-body, blockquote.quote, .letter-body')
            : null;
          if (!bsBody && bsBlock) {
            console.warn('mathpreview: no sub-diff container in', bsBlock.id, '- block left stale');
          }
          if (bsBlock) touchedRoots.push(bsBlock);
          if (bsBody) {
            var bsKids = bsBody.children;
            var bsStart = Math.max(0, Math.min(op.child_index || 0, bsKids.length));
            var bsRemove = Math.max(0, Math.min(op.remove || 0, bsKids.length - bsStart));
            var bsBefore = bsStart > 0 ? bsKids[bsStart - 1] : null;
            var bsAfter = bsKids[bsStart + bsRemove] || null;

            // Pool math from the elements about to be removed so unchanged
            // math inside the edited paragraph transplants (keeps its SVG).
            var bsPool = new Map();
            for (var bsd = 0; bsd < bsRemove; bsd++) {
              var bsRmEl = bsKids[bsStart + bsd];
              if (bsRmEl) indexMathByHash(bsRmEl, bsPool);
            }

            tpl.innerHTML = op.html || '';
            var bsFrag = tpl.content;
            var bsTypesetFrom = needTypeset.length;
            bsFrag.querySelectorAll('.math[data-hash]').forEach(function(newEl) {
              totalMath++;
              var pool = bsPool.get(newEl.dataset.hash);
              if (pool && pool.length > 0) {
                var oldEl = pool.shift();
                syncReusedMathNode(oldEl, newEl);
                newEl.replaceWith(oldEl);
                reusedMath++;
              } else {
                needTypeset.push(newEl);
              }
            });
            // Clear BEFORE seeding: typesetClear must see the donor's
            // container still inside it, or (in the typesetPromise engine
            // path) the moved container's MathItem stays registered forever.
            clearRemovedMath(leftoverMath(bsPool));
            seedStaleMath(leftoverMath(bsPool), needTypeset.slice(bsTypesetFrom));

            // Remove old nodes in [bsBefore, bsAfter), carrying whitespace
            // text nodes between elements along with them.
            var bsCur = bsBefore ? bsBefore.nextSibling : bsBody.firstChild;
            while (bsCur && bsCur !== bsAfter) {
              var bsNext = bsCur.nextSibling;
              bsBody.removeChild(bsCur);
              bsCur = bsNext;
            }
            if (bsAfter) bsBody.insertBefore(bsFrag, bsAfter);
            else bsBody.appendChild(bsFrag);

            clearRemovedMath(leftoverMath(bsPool));
            replacedBlocks++;
          }
        } else if (op.type === 'rebuild') {
          var rbBlocks = pageBlocks(page);
          var rbStart = Math.max(0, Math.min(op.start || 0, rbBlocks.length));
          var rbCount = Math.max(0, Math.min(op.old_count || 0, rbBlocks.length - rbStart));
          var rbAnchor = rbBlocks[rbStart + rbCount] || null;

          // Index the old slice by absolute src index so plan Reuse slots
          // can pull the exact DOM subtree (preserving typeset MathJax).
          var rbOldByIdx = new Map();
          var rbReusedSources = new Set();
          (op.plan || []).forEach(function(slot) {
            if (typeof slot.src === 'number') rbReusedSources.add(slot.src);
          });
          var rbReplacementSizes = [];
          var rbReplacementBlocks = [];
          for (var s2 = 0; s2 < rbCount; s2++) {
            var rbOld = rbBlocks[rbStart + s2];
            if (rbOld) {
              var rbAbsoluteIndex = rbStart + s2;
              rbOldByIdx.set(rbAbsoluteIndex, rbOld);
              if (!rbReusedSources.has(rbAbsoluteIndex)) {
                rbReplacementBlocks.push(rbOld);
                rbReplacementSizes.push(
                  detachedBlockSizes
                    ? detachedBlockSizes.get(rbOld)
                    : snapshotBlockIntrinsicSize(rbOld)
                );
              }
              rbOld.remove();
              removedBlocks++;
            }
          }
          var rbMarkdownState = snapshotMarkdownCustomBlockState(rbReplacementBlocks);
          var rbPreparedInserts = new Map();
          var rbAllInsertRoots = [];
          (op.plan || []).forEach(function(slot, planIndex) {
            if (typeof slot.html !== 'string') return;
            tpl.innerHTML = slot.html;
            var prepared = Array.from(tpl.content.children);
            rbPreparedInserts.set(planIndex, prepared);
            prepared.forEach(function(child) { rbAllInsertRoots.push(child); });
          });
          restoreMarkdownCustomBlockState(rbMarkdownState, rbAllInsertRoots);

          (op.plan || []).forEach(function(slot, planIndex) {
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
              var children = rbPreparedInserts.get(planIndex) || [];
              children.forEach(function(c) {
                seedBlockIntrinsicSize(c, rbReplacementSizes.shift());
                transplantMath(c);
                touchedRoots.push(c);
                page.insertBefore(c, rbAnchor);
                insertedBlocks++;
              });
            }
          });
        }
      }
      // Clear BEFORE seeding (see the blocksub site): typesetClear must see
      // the donor's container still inside it.
      clearRemovedMath(leftoverMath(sharedOldByHash));
      seedStaleMath(leftoverMath(sharedOldByHash), needTypeset);
      syncPatchBlockMetadata(page, blocksMeta);
    } finally {
      if (pageParent) {
        if (pageNextSibling) pageParent.insertBefore(page, pageNextSibling);
        else pageParent.appendChild(page);
      }
      primeStructuralBlockIntrinsicSizes(touchedRoots);
      settleLivePatchViewportAnchor(page, viewportAnchor);
    }

    queueTypeset(needTypeset);
    queueUntypesetMath(page);
    pruneTikzStates();
    if (hasRebuild) observeTikz(page);
    else touchedRoots.forEach(observeTikz);

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
    return hasRebuild ? null : touchedRoots;
  }
