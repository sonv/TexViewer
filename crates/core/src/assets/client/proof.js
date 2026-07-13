  function refreshAfterInitialEngine(tries) {
    if (window.__mpEngine && window.__mpEngine.isReady && window.__mpEngine.isReady()) {
      queueInitialTypeset();
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, true);
      return;
    }
    if (window.__mpEngine && window.__mpEngine.ready(function() {
      queueInitialTypeset();
      scheduleNavigationRefresh(NAV_RENDER_IDLE_MS, true);
    })) return;
    if (tries > 0) {
      setTimeout(function() { refreshAfterInitialEngine(tries - 1); }, 150);
    }
  }

  function ensureInitialTypeset() {
    if (initialTypesetQueued) return;
    refreshAfterInitialEngine(1);
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

  // `roots` (optional element array) scopes the re-fold to freshly patched
  // subtrees. Without it, every keystroke's patch paid a whole-page `.proof`
  // walk (each with sibling-walking role resolution) plus a same-value
  // data-proof-mode write — a page-wide style invalidation on a large doc.
  function applyMode(mode, roots) {
    currentProofMode = mode;
    var page = document.getElementById('page');
    if (page && page.getAttribute('data-proof-mode') !== mode) {
      page.setAttribute('data-proof-mode', mode);
    }
    var proofs = [];
    if (roots && roots.length) {
      roots.forEach(function(root) {
        if (!root || !root.querySelectorAll) return;
        if (root.classList && root.classList.contains('proof')) proofs.push(root);
        root.querySelectorAll('.proof').forEach(function(p) { proofs.push(p); });
      });
    } else {
      document.querySelectorAll('.proof').forEach(function(p) { proofs.push(p); });
    }
    proofs.forEach(function(p) {
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

  /// Trigger a PDF compile on the daemon (`latexmk -pdf`, falling back
  /// to `pdflatex` if latexmk isn't on `$PATH`) and open the produced
  /// PDF in a new tab via a blob URL. The daemon discovers the output
  /// path by parsing the run log, so custom `.latexmkrc` `$out_dir`
  /// settings work without configuration here.
  async function requestPrint(btn) {
    if (btn.classList.contains('busy')) return;
    btn.classList.add('busy');
    var originalText = btn.textContent;
    btn.textContent = 'compiling…';
    try {
      var res = await fetch('/print', { method: 'POST', cache: 'no-store' });
      var contentType = res.headers.get('content-type') || '';
      if (contentType.indexOf('application/pdf') === 0) {
        var blob = await res.blob();
        var url = URL.createObjectURL(blob);
        var win = window.open(url, '_blank');
        // Popup blockers sometimes refuse the open() above. Fall back to
        // a same-tab navigation so the user always sees the PDF.
        if (!win) window.location.href = url;
        // Release the object URL once the tab has had time to load it, so
        // repeated prints don't pin one PDF blob in memory per print.
        setTimeout(function() { URL.revokeObjectURL(url); }, 60000);
        btn.textContent = originalText;
        btn.classList.remove('busy');
        return;
      }
      var body = await res.json().catch(function() { return {}; });
      var msg = (body && body.error) ? body.error : ('print failed (HTTP ' + res.status + ')');
      console.error('mathpreview print:', msg);
      btn.textContent = 'compile failed';
      btn.title = msg;
      setTimeout(function() {
        btn.textContent = originalText;
        btn.classList.remove('busy');
      }, 3500);
    } catch (e) {
      console.error('mathpreview print:', e);
      btn.textContent = 'print error';
      setTimeout(function() {
        btn.textContent = originalText;
        btn.classList.remove('busy');
      }, 2000);
    }
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
    setViewerActionTitle(
      btn,
      stopped ? 'reload when preview server is running' : 'stop preview server'
    );
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
