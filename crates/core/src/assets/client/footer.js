
  // Shared memory tag. Server pushes its current resident size on every
  // event; we cache it so subsequent renders can re-print without waiting
  // for a fresh roundtrip.
  function memSuffix(mib) {
    if (typeof mib !== 'number' || isNaN(mib)) return '';
    return ' · ' + mib.toFixed(1) + ' MiB';
  }

  // Live-reload WebSocket. Reconnects with backoff if the server restarts.
  var WS_PROTOCOL_VERSION = '63';
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
        if (msg.viewer_config) applyViewerConfig(msg.viewer_config);
        // Keep the log panel current as long as it's open. Cheap — one
        // /debug fetch per WS render. No-op when the panel is closed.
        if (typeof refreshLogPanelIfOpen === 'function') refreshLogPanelIfOpen();
        if (msg.event === 'patch') {
          await applyPatch(msg.ops, msg.blocks);
          applyMode(currentProofMode);
          setRefkeysVisible(refkeysVisible, false);
          decorateRefkeyChips(document.getElementById('page'));
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
    // Config-supplied defaults apply only when the user hasn't already
    // picked a value via the in-browser toggle (which writes to
    // localStorage). The user's local choice always wins.
    var cfg = window.__mpConfig || {};
    var storedZoom = parseFloat(localStorage.getItem('mathpreview.userZoom'));
    if (isFinite(storedZoom) && storedZoom > 0) setUserZoom(storedZoom, false);
    var storedMode = localStorage.getItem('mathpreview.pageMode');
    setPageMode(storedMode || cfg.defaultPageMode || 'a4');
    setSideTab(localStorage.getItem('mathpreview.sideTab') || 'index');
    var storedSideOpen = localStorage.getItem('mathpreview.sideOpen');
    setSideOpen(storedSideOpen === null ? window.innerWidth > 1340 : storedSideOpen === '1', false);
    setRefkeysVisible(localStorage.getItem('mathpreview.refkeys') === '1', false);
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
    setSideTab('index');
    setSideOpen(window.innerWidth > 1340, false);
    setRefkeysVisible(false, false);
    setLineNumbers(false, false);
    setMarginMode(false, false);
    setTopbarHidden(false, false);
    setTheme('light', false);
  }
  initCmdline();
  initMarginDnd();
  decorateRefkeyChips(document.getElementById('page'));
  scheduleNavigationRefresh();
  startMathObserver();
  refreshAfterInitialEngine(40);
  setTimeout(ensureInitialTypeset, 1200);
  window.addEventListener('load', scheduleNavigationRefresh);
  window.addEventListener('resize', function() {
    updatePageScale();
    scheduleNavigationRefresh(NAV_RESIZE_IDLE_MS, false);
  });
  window.addEventListener('scroll', scheduleActivePageUpdate, { passive: true });
  connect();
})();
