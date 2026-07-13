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
    if (e.target && e.target.name === 'config-scope') {
      syncConfigCustomPathEnabled();
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
  document.addEventListener('click', function(e) {
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
    var printBtn = e.target.closest('#print-button');
    if (printBtn) {
      requestPrint(printBtn);
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
    var cropToggle = e.target.closest('#crop-toggle');
    if (cropToggle) {
      setPageCrop(!pageCropped, true);
      return;
    }
    var refkeyToggle = e.target.closest('#refkey-toggle');
    if (refkeyToggle) {
      setRefkeysVisible(!refkeysVisible, true);
      return;
    }
    var linenoToggle = e.target.closest('#lineno-toggle');
    if (linenoToggle) {
      setLineNumbers(!lineNumbersVisible, true);
      return;
    }
    var macrosToggle = e.target.closest('#macros-toggle');
    if (macrosToggle) {
      openMacrosDialog();
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
    var configToggle = e.target.closest('#config-toggle');
    if (configToggle) {
      openConfigDialog();
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
    var logToggle = e.target.closest('#log-toggle');
    if (logToggle) {
      toggleLogPanel();
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
    var marginToggle = e.target.closest('#margin-toggle');
    if (marginToggle) {
      setMarginMode(!marginMode, true);
      return;
    }
    var themeToggle = e.target.closest('#theme-toggle');
    if (themeToggle) {
      setTheme(themeMode === 'dark' ? 'light' : 'dark', true);
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
    // Browser print: typeset the whole document first (viewport-lazy leaves
    // off-screen math as raw source, which would print as LaTeX), then open
    // the dialog. Instant when everything is already typeset.
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey &&
        (e.key === 'p' || e.key === 'P')) {
      e.preventDefault();
      typesetAllForPrint().then(function(completed) {
        if (completed) window.print();
      });
      return;
    }
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
        else runSearch(e.shiftKey);
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

    // Keyboard activation for the .eq-refkey-chip span (the per-row
    // equation refkey chips are spans with tabindex=0, not <button>s,
    // so we wire Enter/Space ourselves).
    var eqChip = e.target.closest('.eq-refkey-chip[data-target]');
    if (eqChip && (e.key === 'Enter' || e.key === ' ')) {
      e.preventDefault();
      togglePinByRefkey(eqChip.dataset.target || eqChip.textContent || '');
      return;
    }

    if (handleZoomKeys(e)) {
      e.preventDefault();
      return;
    }

    // Cmd/Ctrl+M toggles margin mode. macOS browsers don't see Cmd+M (the OS
    // takes it for window-minimize) — that's fine; Ctrl+M covers Linux/Windows
    // and works everywhere the page receives it. Skip while typing.
    if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey &&
        (e.key === 'm' || e.key === 'M') && !isEditableTarget(e.target)) {
      e.preventDefault();
      setMarginMode(!marginMode, true);
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
  var mathObserver = null;
  var TYPESET_IDLE_MS = 120;
  var TYPESET_SMALL_IDLE_MS = 35;
  var TYPESET_BUSY_RETRY_MS = 80;

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
    var lifted = target.style.contentVisibility === '';
    if (lifted) target.style.contentVisibility = 'visible';
    try {
      nodes.forEach(syncMathSourceText);
      await window.__mpEngine.typeset(nodes);
    } catch (err) {
      console.error('mathpreview background fill:', err);
      typesetBusy = false;
      return; // stop on engine error rather than spinning
    } finally {
      if (lifted) target.style.contentVisibility = '';
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
    // Yield to the print flush and to an in-progress typeset batch.
    if (printFlushPromise || typesetBusy) { scheduleWindowDrain(150); return; }
    var blk = windowQueue.values().next().value;
    windowQueue.delete(blk);
    if (blk && blk.isConnected) {
      var nodes = Array.from(blk.querySelectorAll('.math[data-hash]')).filter(isRawMathNode);
      if (nodes.length) {
        typesetBusy = true;
        var lifted = blk.style.contentVisibility === '';
        if (lifted) blk.style.contentVisibility = 'visible';
        try {
          nodes.forEach(syncMathSourceText);
          await window.__mpEngine.typeset(nodes);
        } catch (err) {
          console.error('mathpreview window typeset:', err);
        } finally {
          if (lifted) blk.style.contentVisibility = '';
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
    setStatus('updating', '↻ typesetting ' + nodes.length + ' math');
    var tStart = performance.now();
    try {
      await window.__mpEngine.typeset(nodes);
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
      records.forEach(function(record) {
        record.addedNodes.forEach(function(node) { collectRawMath(node, nodes); });
      });
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
    setStatus('updating', '↻ patching');
    var page = document.getElementById('page');
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
          // quote). Document order finds the OUTERMOST container first, which
          // is the one the server chunked. A miss here would silently drop the
          // edit and leave the block stale until the next full update.
          var bsBody = bsBlock && bsBlock.querySelector
            ? bsBlock.querySelector('.proof-body, .thm-body, .callout-body, blockquote.quote')
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
    }

    queueTypeset(needTypeset);
    queueUntypesetMath(page);

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
