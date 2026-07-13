
  // Shared memory tag. Server pushes its current resident size on every
  // event; we cache it so subsequent renders can re-print without waiting
  // for a fresh roundtrip.
  function memSuffix(mib) {
    if (typeof mib !== 'number' || isNaN(mib)) return '';
    return ' · ' + mib.toFixed(1) + ' MiB';
  }

  // Live-reload WebSocket. Reconnects with backoff if the server restarts.
  // MUST match WS_PROTOCOL_VERSION in crates/cli/src/serve.rs — a mismatch makes
  // the server full-reload every connect (an infinite reload loop). The
  // `client_ws_protocol_matches_server` test guards this.
  var WS_PROTOCOL_VERSION = '71';
  var status = document.getElementById('ws-status');
  function setStatus(cls, text) {
    if (!status) return;
    status.className = 'status ' + cls;
    status.textContent = text;
  }
  // Set once we've had a live connection. A *second* onopen means the
  // server went away and came back — almost always a `:MathPreviewRestart`.
  var everConnected = false;
  // The daemon said goodbye (deliberate editor-driven teardown: quitting nvim,
  // :MathPreviewStop, :bd). Close the viewer and don't try to reconnect.
  var serverSaidBye = false;
  function connect() {
    if (!window.WebSocket) return;
    var url = (location.protocol === 'https:' ? 'wss://' : 'ws://') +
      location.host + '/ws?v=' + encodeURIComponent(WS_PROTOCOL_VERSION);
    var ws;
    try { ws = new WebSocket(url); } catch (e) { return; }
    ws.onopen  = function() {
      if (everConnected) {
        // The page in front of us was rendered by the *old* daemon, so its
        // <head> — MathJax config, baked-in macros, client assets — is
        // stale; a WS reconnect only resumes body patches. Hard-reload to
        // pull the freshly served page from the restarted daemon.
        location.reload();
        return;
      }
      everConnected = true;
      setStatus('live', '● live');
    };
    ws.onclose = function() {
      if (serverSaidBye) {
        setStatus('dead', '○ preview ended');
        return;
      }
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
        // Deliberate teardown (quit nvim / :MathPreviewStop / :bd): close the
        // viewer like peek.nvim does. The Locus window closes via its IPC
        // channel; a browser tab closes itself with window.close(), which the
        // HTML spec allows for a tab whose session history has a single entry
        // — true for a freshly opened preview tab. If the browser refuses
        // (user navigated in this tab), fall through to a terminal status.
        // Crashes don't send bye, so the reconnect path still handles them.
        if (msg.event === 'bye') {
          serverSaidBye = true;
          if (window.ipc && typeof window.ipc.postMessage === 'function') {
            window.ipc.postMessage('close');
          } else {
            window.close();
          }
          setTimeout(function() { setStatus('dead', '○ preview ended'); }, 250);
          return;
        }
        if (typeof msg.rss_mib === 'number') window._lastRss = msg.rss_mib;
        if (msg.viewer_config) applyViewerConfig(msg.viewer_config);
        // Keep the log panel current as long as it's open. Cheap — one
        // /debug fetch per WS render. No-op when the panel is closed.
        if (typeof refreshLogPanelIfOpen === 'function') refreshLogPanelIfOpen();
        if (msg.event === 'patch' && Array.isArray(msg.ops)) {
          // Scope the per-patch passes to the blocks the patch touched — the
          // whole-page versions cost a full 20k-element pass + page-wide style
          // invalidation on EVERY keystroke of a large document.
          var touched = await applyPatch(msg.ops, msg.blocks);
          applyMode(currentProofMode, touched || undefined);
          setRefkeysVisible(refkeysVisible, false);
          if (touched) touched.forEach(function(root) { decorateRefkeyChips(root); });
          else decorateRefkeyChips(document.getElementById('page'));
          restoreSourceHighlight();
          restoreSourceRange();
          restoreMathSearchHighlights();
          restoreTextSearchHighlights();
          restoreEditorSearchHighlights();
          scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, true);
        } else if (msg.event === 'body-updated' && typeof msg.html === 'string') {
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
          // Anti-flash (see patch.js seedStaleMath): keep the outgoing render
          // of a changed equation visible until its re-typeset lands. Small
          // documents take this full-body path on every keystroke (the server
          // falls back to `body-updated` when the patch would cost more than
          // half the block count), so it needs the same seeding as applyPatch.
          // Clear BEFORE seeding: typesetClear must see the donor's container
          // still inside it.
          clearRemovedMath(leftoverMath(oldByHash));
          seedStaleMath(leftoverMath(oldByHash), needTypeset);

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
          queueUntypesetMath(page);

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
          decorateRefkeyChips(document.getElementById('page'));
          restoreSourceHighlight();
          restoreSourceRange();
          restoreMathSearchHighlights();
          restoreTextSearchHighlights();
          restoreEditorSearchHighlights();
          scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, true);
        } else if (msg.event === 'source-cursor') {
          // Cursor moved off a math row back to prose → drop the row band
          // (also when typing: restoreSourceRange re-applies the client's
          // CACHED band on every render, so a band kept here would be
          // resurrected forever — the server never re-resolves the cursor
          // after a render). Typing (the editor tags cursor moves caused by
          // edits) differs in one way only: no flash box — re-triggering it
          // on every keystroke reads as strobing — just follow the cursor.
          clearSourceRangeHighlights();
          if (msg.typing) {
            clearSourceActive();
            if (msg.element_id) scrollSourceElementOnly(msg.element_id, true);
          } else if (msg.element_id) {
            // A scroll_only event targets a block-level element (section
            // heading): follow it without the flash.
            if (msg.scroll_only) scrollSourceElementOnly(msg.element_id);
            else revealSourceElement(msg.element_id, true);
          }
        } else if (msg.event === 'source-range') {
          highlightSourceRange(msg.element_ids || [], true, msg.math_rows || [], msg.typing === true);
        } else if (msg.event === 'search-sync') {
          // The editor's `/` search pattern — highlight its matches in the
          // preview (empty string / omitted clears them).
          editorSearchHighlight(typeof msg.query === 'string' ? msg.query : '');
        } else if (msg.event === 'full-reload') {
          location.reload();
        } else if (msg.event === 'error') {
          setStatus('dead', '○ ' + (msg.message || 'render error'));
        }
      } catch (e) { console.error('mathpreview WS:', e); }
    };
  }
  try {
    // Config-supplied defaults apply only when the user hasn't already
    // picked a value via the in-browser toggle (which writes to
    // localStorage). The user's local choice always wins.
    var cfg = window.__mpConfig || {};
    var storedZoom = parseFloat(localStorage.getItem('mathpreview.userZoom'));
    if (isFinite(storedZoom) && storedZoom > 0) setUserZoom(storedZoom, false);
    var storedMode = localStorage.getItem('mathpreview.pageMode');
    setPageMode(storedMode || cfg.defaultPageMode || 'a4');
    var storedSideOpen = localStorage.getItem('mathpreview.sideOpen');
    setSideOpen(storedSideOpen === null ? window.innerWidth > 1340 : storedSideOpen === '1', false);
    setRefkeysVisible(localStorage.getItem('mathpreview.refkeys') === '1', false);
    setPageCrop(localStorage.getItem('mathpreview.crop') === '1', false);
    setLineNumbers(localStorage.getItem('mathpreview.lineNumbers') === '1', false);
    setMarginMode(localStorage.getItem('mathpreview.marginMode') === '1', false);
    setTopbarHidden(localStorage.getItem('mathpreview.topbarHidden') === '1', false);
    var storedTheme = localStorage.getItem('mathpreview.theme');
    if (storedTheme === 'dark' || storedTheme === 'light') {
      setTheme(storedTheme, false);
    } else {
      var cfgTheme = cfg.defaultTheme;
      if (cfgTheme === 'light' || cfgTheme === 'dark') {
        setTheme(cfgTheme, false);
      } else {
        // "system" (or unspecified) → match the OS preference.
        var prefersDark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
        setTheme(prefersDark ? 'dark' : 'light', false);
      }
    }
  } catch (e) {
    setPageMode('a4');
    setSideOpen(window.innerWidth > 1340, false);
    setRefkeysVisible(false, false);
    setLineNumbers(false, false);
    setMarginMode(false, false);
    setTopbarHidden(false, false);
    setTheme('light', false);
  }
  initCmdline();
  initSearchPanel();
  initMarginDnd();
  decorateRefkeyChips(document.getElementById('page'));
  syncTopbarHeight();
  scheduleNavigationRefresh();
  startMathObserver();
  refreshAfterInitialEngine(40);
  setTimeout(ensureInitialTypeset, 1200);
  window.addEventListener('load', function() {
    // Re-measure once fonts/layout have fully settled.
    syncTopbarHeight();
    scheduleNavigationRefresh();
  });
  window.addEventListener('resize', function() {
    updatePageScale();
    // Responsive wrapping changes the toolbar's height, so re-anchor the
    // floating side controls before refreshing navigation.
    syncTopbarHeight();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
    // Resizing reflows wrapped text, so the gutter line numbers must be
    // re-measured (rAF-coalesced; only when the gutter is shown) — and the
    // measured refkey layer with them.
    if (lineNumbersVisible) scheduleLineNumbers();
    if (refkeysVisible) scheduleRefkeys();
  });
  // Keep the topbar in view while a zoomed page (body.page-overwide) is
  // panned horizontally. `position: sticky; left: 0` can't do it — the
  // topbar fills body's width exactly, and sticky never pushes an element
  // outside its containing block — so translate it by the pan instead.
  // rAF-coalesced; the transform is cleared whenever the page fits, so the
  // topbar is a plain sticky element in the normal (unzoomed) case.
  (function() {
    var topbar = document.querySelector('.topbar');
    var queued = false;
    function pinTopbarX() {
      queued = false;
      if (!topbar) return;
      var x = document.body.classList.contains('page-overwide')
        ? (window.scrollX || 0) : 0;
      topbar.style.transform = x ? 'translateX(' + x + 'px)' : '';
    }
    window.addEventListener('scroll', function() {
      if (queued) return;
      queued = true;
      requestAnimationFrame(pinTopbarX);
    }, { passive: true });
  })();
  connect();
})();
