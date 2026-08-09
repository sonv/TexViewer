(function() {
  var currentProofMode = 'all';
  var currentPageMode = 'a4';
  var currentSideOpen = false;
  var refkeysVisible = false;
  var lineNumbersVisible = false;
  var lineNumbersScheduled = false;
  var refkeyBlockMetrics = new WeakMap();
  var lineNumberBlockMetrics = new WeakMap();
  var overlayPrelayoutToken = 0;
  var marginMode = false;
  var themeMode = 'light';
  var pinnedRefs = new Map();
  var hoverPreviewTimer = 0;
  var hoverPreviewEl = null;
  var hoverPreviewSource = null;
  var topbarHidden = false;
  var navRefreshTimer = 0;
  var zoomCommitTimer = 0;
  var zoomPreviewAnchor = null;
  var zoomAnchorRestoreRaf = 0;
  var zoomAnchorVerifyRaf = 0;
  var currentUserZoom = 1;
  var committedPageScale = 1;
  var ZOOM_MIN = 0.5;
  var ZOOM_MAX = 3;
  var ZOOM_STEP = 0.1;
  var NAV_IDLE_MS = 220;
  var NAV_RENDER_IDLE_MS = 900;
  var NAV_RESIZE_IDLE_MS = 120;
  var A4_CSS_WIDTH = 794;
  var DYNAMIC_BASE_WIDTH = 720;
  // Crop-to-content ("c"): trim the paper margins around the text column.
  // The page narrows by exactly the horizontal padding saved, so the column
  // keeps its width and nothing reflows. CROP_PAD is the cropped --page-pad-x
  // — MUST match default.css's `body.page-crop` rules (12px pad; the A4 width
  // rule's 104px = 2×(64−12)).
  var CROP_PAD = 12;
  var pageCropped = false;
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
  var keySequenceCandidates = [];
  var keySequenceExactFallback = null;
  var keySequenceTimer = 0;
  var viewerKeySequenceTimeoutMs = 750;
  var lastSearchQuery = '';
  var mathSearchQuery = '';
  var mathSearchResults = [];
  var mathSearchIndex = -1;
  // Plain-text search: our own match list (Ranges) so `/` cycles at the ends
  // and shows current/total.
  var textSearchQuery = '';
  var textSearchResults = [];
  var textSearchIndex = -1;
  // Word-completion suggestions shown under the `/` box as you type.
  var searchSuggestions = [];
  var searchSuggestIndex = -1;
  // Editor-driven search: the nvim `/` pattern, pushed by the plugin and
  // highlighted (all matches) in the preview like vim's hlsearch.
  var editorSearchQuery = '';
  var editorSearchWholeStart = false;
  var editorSearchWholeEnd = false;
  var editorSearchCaseSensitive = false;
  // Vim-style jump history. `viewerJumpIndex === viewerJumpList.length`
  // denotes the live, not-yet-captured destination after the newest jump.
  // The first backward traversal captures that destination, which gives the
  // list a real forward entry without making ordinary scrolling a jump.
  var VIEWER_JUMP_LIMIT = 100;
  var viewerJumpList = [];
  var viewerJumpIndex = 0;

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
    var rect = target.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0 && target.scrollIntoView) {
      // Descendants of a content-visibility-skipped block have no usable
      // geometry until they are brought into the rendering window.
      target.scrollIntoView({ block: 'start', behavior: 'auto' });
      rect = target.getBoundingClientRect();
    }
    var y = rect.top + window.scrollY - topbarOffset();
    window.scrollTo({ top: Math.max(0, y), behavior: 'auto' });
  }

  function scrollByVim(dx, dy) {
    window.scrollBy({ left: dx, top: dy, behavior: 'auto' });
  }

  function clearKeySequencePending() {
    keySequenceCandidates = [];
    keySequenceExactFallback = null;
    if (keySequenceTimer) {
      clearTimeout(keySequenceTimer);
      keySequenceTimer = 0;
    }
  }

  function armKeySequenceTimeout() {
    if (keySequenceTimer) clearTimeout(keySequenceTimer);
    keySequenceTimer = setTimeout(function() {
      keySequenceTimer = 0;
      var fallback = keySequenceExactFallback;
      if (fallback) {
        runViewerAction(fallback.action, fallback.invocation);
      } else {
        clearViewerKeyPending();
      }
    }, viewerKeySequenceTimeoutMs);
  }

  function isEditableTarget(target) {
    if (!target || !target.closest) return false;
    return !!target.closest(
      'input, textarea, select, [contenteditable]:not([contenteditable="false"])'
    );
  }

  function isViewerInteractiveTarget(target) {
    if (!target || !target.closest) return false;
    return !!target.closest(
      'button, a[href], summary, [role="button"], [role="link"], audio[controls], video[controls]'
    );
  }

  function isViewerKeySuppressedTarget(target, key) {
    if (isEditableTarget(target)) return true;
    if (!isViewerInteractiveTarget(target)) return false;
    // Focus may remain on a toolbar button or link after a click. Continue to
    // allow normal viewer letters such as j/k, but yield the keys with native
    // activation/navigation meaning for the focused control.
    return [
      'Enter', ' ', 'Spacebar', 'ArrowLeft', 'ArrowRight', 'ArrowUp',
      'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown',
    ].indexOf(key) >= 0;
  }

  function currentViewerPlace() {
    var place = { x: window.scrollX || 0, y: window.scrollY || 0 };
    var page = pageEl();
    if (!page || !document.elementFromPoint) return place;
    var pageRect = page.getBoundingClientRect();
    var viewportX = Math.max(
      pageRect.left + 1,
      Math.min(pageRect.right - 1, (window.innerWidth || 1000) * 0.5)
    );
    var viewportY = Math.min(
      (window.innerHeight || 800) - 1,
      topbarOffset() + 18
    );
    var hit = document.elementFromPoint(viewportX, viewportY);
    var anchor = hit && hit.closest && hit.closest('#page [data-src][id]');
    if (!anchor) return place;
    var rect = anchor.getBoundingClientRect();
    place.anchorId = anchor.id;
    place.anchorSrc = anchor.getAttribute('data-src') || '';
    place.anchorFingerprint = viewerAnchorFingerprint(anchor);
    place.anchorViewportY = rect.top;
    return place;
  }

  function viewerAnchorFingerprint(anchor) {
    if (!anchor) return '';
    var context = anchor.closest && anchor.closest(
      'p, li, .proof-para, .sec-h0, .sec-h1, .sec-h2, .sec-h3, .sec-h4, .sec-h5, .sec-h6'
    );
    return anchor.tagName + '|' + cleanNavText(anchor.textContent).slice(0, 64) + '|' +
      cleanNavText(context && context.textContent).slice(0, 192);
  }

  function viewerSourceParts(src) {
    var match = /^(.*):(\d+):(\d+)$/.exec(src || '');
    return match ? { file: match[1], line: parseInt(match[2], 10) } : null;
  }

  // Shared by jumplist replay and user marks. Callers decide whether the
  // movement itself should first record a jump origin.
  function restoreViewerPlace(place, behavior) {
    if (!place || !isFinite(place.x) || !isFinite(place.y)) return false;
    var top = place.y;
    var page = pageEl();
    var anchor = place.anchorId && document.getElementById(place.anchorId);
    var fingerprint = place.anchorFingerprint || '';
    if (anchor && fingerprint && viewerAnchorFingerprint(anchor) !== fingerprint) {
      // Generated ids contain a block ordinal and can be reused by different
      // content after an insertion. A semantic fingerprint distinguishes that
      // case from the same element merely receiving a shifted data-src line.
      anchor = null;
    }
    if (!anchor && page && place.anchorSrc) {
      anchor = Array.from(page.querySelectorAll('[data-src]')).find(function(el) {
        return el.getAttribute('data-src') === place.anchorSrc &&
          (!fingerprint || viewerAnchorFingerprint(el) === fingerprint);
      });
    }
    if (!anchor && page && fingerprint) {
      // If edits shifted both the generated id and source line, recover the
      // unchanged semantic anchor. This path is jump-only, never a typing hot
      // path, and computes geometry for at most the matching duplicates.
      var semanticMatches = Array.from(page.querySelectorAll('[data-src][id]')).filter(
        function(el) { return viewerAnchorFingerprint(el) === fingerprint; }
      );
      var storedSource = viewerSourceParts(place.anchorSrc);
      if (storedSource) {
        semanticMatches = semanticMatches.filter(function(el) {
          var source = viewerSourceParts(el.getAttribute('data-src'));
          return source && source.file === storedSource.file;
        });
      }
      var bestLineDistance = Infinity;
      var bestVisualDistance = Infinity;
      semanticMatches.forEach(function(el) {
        var source = viewerSourceParts(el.getAttribute('data-src'));
        var lineDistance = storedSource && source
          ? Math.abs(source.line - storedSource.line) : 0;
        var rect = el.getBoundingClientRect();
        var visualDistance = rect.width || rect.height
          ? Math.abs(window.scrollY + rect.top - place.y) : Infinity;
        if (lineDistance < bestLineDistance ||
            (lineDistance === bestLineDistance && visualDistance < bestVisualDistance)) {
          bestLineDistance = lineDistance;
          bestVisualDistance = visualDistance;
          anchor = el;
        }
      });
    }
    if (anchor && isFinite(place.anchorViewportY)) {
      var rect = anchor.getBoundingClientRect();
      if (rect.width || rect.height) {
        top = window.scrollY + rect.top - place.anchorViewportY;
      }
    }
    window.scrollTo({
      left: Math.max(0, place.x),
      top: Math.max(0, top),
      behavior: behavior || 'auto',
    });
    return true;
  }

  function sameViewerPlace(a, b) {
    return !!(a && b && Math.abs(a.x - b.x) < 24 && Math.abs(a.y - b.y) < 24);
  }

  function appendViewerJumpPlace(place) {
    viewerJumpList.push(place);
    if (viewerJumpList.length > VIEWER_JUMP_LIMIT) {
      viewerJumpList.shift();
      viewerJumpIndex = Math.max(0, viewerJumpIndex - 1);
    }
  }

  // Capture the origin immediately before a real jump (search, source sync,
  // heading/hash navigation, gg/G). If the user previously walked backward,
  // a new jump branches here and discards the now-unreachable forward tail,
  // matching Vim's jumplist rather than ping-ponging between two positions.
  function recordViewerPlace() {
    var place = currentViewerPlace();
    if (viewerJumpIndex < viewerJumpList.length) {
      viewerJumpList.length = viewerJumpIndex + 1;
    }
    var last = viewerJumpList.length
      ? viewerJumpList[viewerJumpList.length - 1]
      : null;
    if (!sameViewerPlace(last, place)) appendViewerJumpPlace(place);
    // The navigation about to run lands at a live destination that will be
    // captured lazily on the first backward traversal.
    viewerJumpIndex = viewerJumpList.length;
    return !sameViewerPlace(last, place);
  }

  // Search records before it knows whether a match exists. Preserve the whole
  // state so a failed search does not silently destroy a forward branch.
  function checkpointViewerJumps() {
    return { list: viewerJumpList.slice(), index: viewerJumpIndex };
  }

  function rollbackViewerJumps(checkpoint) {
    if (!checkpoint) return;
    viewerJumpList = checkpoint.list;
    viewerJumpIndex = checkpoint.index;
  }

  function moveViewerJump(direction, count) {
    clearKeySequencePending();
    count = Math.max(1, Math.floor(count || 1));
    if (direction > 0 && viewerJumpIndex === viewerJumpList.length) {
      setStatus('dead', '○ no next place');
      return false;
    }
    if (direction < 0 && viewerJumpIndex === viewerJumpList.length) {
      if (!viewerJumpList.length) {
        setStatus('dead', '○ no previous place');
        return false;
      }
      var current = currentViewerPlace();
      var newest = viewerJumpList.length
        ? viewerJumpList[viewerJumpList.length - 1]
        : null;
      if (!sameViewerPlace(newest, current)) appendViewerJumpPlace(current);
      viewerJumpIndex = viewerJumpList.length - 1;
    }

    var targetIndex = Math.max(
      0,
      Math.min(viewerJumpList.length - 1, viewerJumpIndex + direction * count)
    );
    if (targetIndex === viewerJumpIndex) {
      setStatus('dead', direction < 0 ? '○ no previous place' : '○ no next place');
      return false;
    }
    viewerJumpIndex = targetIndex;
    var place = viewerJumpList[viewerJumpIndex];
    restoreViewerPlace(place);
    setStatus('live', direction < 0 ? '● previous place' : '● next place');
    return true;
  }

  function restorePreviousPlace(count) {
    return moveViewerJump(-1, count);
  }

  function restoreNextPlace(count) {
    return moveViewerJump(1, count);
  }
