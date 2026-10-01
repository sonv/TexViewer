import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { after, before, test } from 'node:test';
import { chromium, webkit } from 'playwright';

// Exercise the real embedded bundle, daemon patches, and vendored MathJax.
// `npm run test:hover` builds the CLI first so edits to include_str! assets
// cannot accidentally be tested against an old binary.
const binary = resolve('target/debug/mathpreview-cli');
const popup = '.hover-preview';
const browserName = process.env.MATHPREVIEW_TEST_BROWSER || 'chromium';
const browserType = { chromium, webkit }[browserName];
assert.ok(browserType, `Unsupported test browser: ${browserName}`);
let browser;
before(async () => { browser = await browserType.launch(); });
after(async () => { await browser?.close(); });

function documentSource(math) {
  // Enough independent blocks to exercise `patch`, not just `body-updated`.
  return String.raw`\documentclass{article}
\begin{document}
${math}
See \eqref{eq:first} and \eqref{eq:alias}.

${Array.from({ length: 20 }, (_, i) => `Stable paragraph ${i}.`).join('\n\n')}
\end{document}
`;
}

const equation = String.raw`\begin{equation}\label{eq:first}E=mc^2\label{eq:alias}\end{equation}`;
const probabilityAlign = String.raw`\begin{align}
  \PP(T_N\le a-K\sqrt L)&\le\frac{e^{-K^2}}{1-\delta},
  \label{eq:main-early}\\
  \PP(T_N>a+K\sqrt L)&\le\frac{\sqrt\pi}{2\delta K}.
  \label{eq:main-late}
\end{align}`;
const probabilityReferences = String.raw`See \eqref{eq:main-early} and \eqref{eq:main-late}.`;

function probabilitySource(body) {
  return documentSource(body).replace('\\begin{document}', String.raw`\newcommand{\PP}{\mathbb{P}}
\begin{document}`);
}

async function fixture(t, math = equation, source = documentSource) {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-hover-test-'));
  const input = join(directory, 'notes.tex');
  await writeFile(input, source(math));
  const portProbe = createServer();
  portProbe.listen(0, '127.0.0.1');
  await once(portProbe, 'listening');
  const port = portProbe.address().port;
  await new Promise((resolveClose) => portProbe.close(resolveClose));
  const url = `http://127.0.0.1:${port}`;
  const daemon = spawn(binary, ['serve', input, '--port', String(port)], {
    // Isolate tests from personal macros and viewer configuration.
    env: { ...process.env, XDG_CONFIG_HOME: join(directory, 'config') },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  let log = '';
  daemon.stderr.on('data', chunk => { log += chunk; });
  const exited = once(daemon, 'exit');
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  const page = await context.newPage();
  page.setDefaultTimeout(10000);
  const errors = [];
  const events = [];
  page.on('pageerror', error => errors.push(String(error)));
  page.on('websocket', socket => socket.on('framereceived', ({ payload }) => {
    try { events.push(JSON.parse(String(payload)).event); } catch { /* non-JSON frame */ }
  }));
  t.after(async () => {
    await context.close();
    daemon.kill('SIGTERM');
    await exited;
    await rm(directory, { recursive: true, force: true });
    assert.deepEqual(errors, [], 'viewer has no uncaught browser errors');
  });
  for (let attempt = 0; ; attempt++) {
    if (daemon.exitCode !== null || attempt === 100) {
      throw new Error(`Preview did not start: ${log}`);
    }
    try {
      if ((await fetch(url)).ok) break;
    } catch { /* wait for the listener */ }
    await delay(50);
  }
  async function open(fragment = '') {
    await page.goto(url + fragment, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('live'));
  }
  async function update(nextMath) {
    events.length = 0;
    const response = await fetch(`${url}/buffer`, {
      method: 'POST',
      headers: { 'x-mathpreview-path': input },
      body: source(nextMath),
    });
    assert.equal(response.status, 204);
    for (let attempt = 0; !events.includes('patch'); attempt++) {
      assert.ok(attempt < 100, `Expected a patch, got ${events.join(', ')}`);
      await delay(50);
    }
  }
  return { page, open, update };
}

async function holdMathJax(page) {
  let release;
  const gate = new Promise(resolveGate => { release = resolveGate; });
  await page.route('**/tex-svg.js', async route => {
    await gate;
    await route.continue();
  });
  return release;
}

async function hover(page, selector) {
  // Dispatch the delegated event without moving the pointer: a layout change
  // while typesetting should not turn a readiness test into a mouseout test.
  await page.locator(selector).dispatchEvent('mouseout');
  await page.locator(selector).dispatchEvent('mouseover');
  await page.waitForSelector(popup);
}

async function keys(page) {
  return page.locator(`${popup} .hover-preview-keys code`).allTextContents();
}

async function assertLabelOnly(page, expectedKeys) {
  assert.deepEqual(await keys(page), expectedKeys);
  assert.equal(await page.locator(`${popup}.equation-label-preview`).count(), 1);
  assert.equal(await page.locator(`${popup} .hover-preview-body, ${popup} .math, ${popup} svg`).count(), 0);
  assert.equal(await page.locator(popup).getAttribute('aria-hidden'), null);
  assert.equal(await page.locator(popup).getAttribute('inert'), null);
  assert.equal(await page.locator(popup).getAttribute('role'), 'group');
  assert.equal(await page.locator(popup).getAttribute('aria-label'), 'Equation source labels');
  assert.deepEqual(await page.locator(`${popup} .equation-label-copy`).evaluateAll(buttons =>
    buttons.map(button => ({ key: button.dataset.refkey, role: button.getAttribute('role'), tabIndex: button.tabIndex }))),
  expectedKeys.map(key => ({ key, role: 'button', tabIndex: 0 })));
}

async function stubClipboard(page, behavior = 'resolve') {
  await page.evaluate(mode => {
    window.hoverTestCopies = [];
    window.hoverTestPendingCopies = [];
    window.hoverTestLastActivationTrusted = false;
    for (const type of ['click', 'keydown']) {
      document.addEventListener(type, event => {
        window.hoverTestLastActivationTrusted = event.isTrusted;
      }, true);
    }
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: mode === 'unavailable' ? undefined : {
        writeText(text) {
          window.hoverTestCopies.push({ text, trusted: window.hoverTestLastActivationTrusted });
          if (mode === 'reject') return Promise.reject(new DOMException('Permission denied', 'NotAllowedError'));
          if (mode === 'pending') return new Promise((resolveCopy, rejectCopy) => {
            window.resolveHoverTestCopy = resolveCopy;
            window.rejectHoverTestCopy = rejectCopy;
            window.hoverTestPendingCopies.push({ resolveCopy, rejectCopy });
          });
          return Promise.resolve();
        },
      },
    });
  }, behavior);
}

async function captureKeyboardCopy(page, modifier = process.platform === 'darwin' ? 'Meta' : 'Control') {
  await page.evaluate(() => {
    window.hoverTestKeyboardCopy = null;
    document.addEventListener('copy', event => {
      window.hoverTestKeyboardCopy = {
        text: event.clipboardData.getData('text/plain'),
        selection: window.getSelection().toString(),
        trusted: event.isTrusted,
      };
      // Observe the real key gesture without writing test data to the user's
      // system clipboard. The viewer's earlier copy listener has already run.
      event.preventDefault();
    }, { once: true });
  });
  await page.keyboard.press(`${modifier}+c`);
  await page.waitForFunction(() => window.hoverTestKeyboardCopy !== null);
  return page.evaluate(() => window.hoverTestKeyboardCopy);
}

async function pointerIntoLabelPopup(page, numberSelector, key) {
  await page.locator(numberSelector).hover();
  await page.waitForSelector(`${popup}.equation-label-preview`);
  const number = await page.locator(numberSelector).boundingBox();
  const box = await page.locator(popup).boundingBox();
  const gapY = box.y >= number.y + number.height
    ? (number.y + number.height + box.y) / 2
    : (box.y + box.height + number.y) / 2;
  await page.mouse.move(number.x + number.width / 2, gapY);
  await delay(80); // cross the real positioning gap within the 250ms grace
  const button = page.locator(`${popup} .equation-label-copy[data-refkey="${key}"]`);
  await button.hover();
  await delay(300); // staying in the popup cancels the source's leave timer
  assert.equal(await button.count(), 1);
  return button;
}

async function assertHighlightedRow(page, expectedRow, expectedRows = 2) {
  await page.waitForSelector(`${popup} svg .hover-preview-row-target`);
  const actual = await page.locator(`${popup} svg`).evaluate(svg => {
    const table = svg.querySelector('[data-mml-node="mtable"]');
    const rows = table ? Array.from(table.children).filter(row =>
      row.matches('[data-mml-node="mtr"], [data-mml-node="mlabeledtr"]')) : [];
    const targets = Array.from(svg.querySelectorAll('.hover-preview-row-target'));
    return { rows: rows.length, targets: targets.map(target => rows.indexOf(target)) };
  });
  assert.deepEqual(actual, { rows: expectedRows, targets: [expectedRow] });
  const band = page.locator(`${popup} .hover-preview-row-target > rect.hover-preview-row-highlight`);
  assert.equal(await band.count(), 1, 'the referenced row has a visible highlight band');
  assert.ok(await band.evaluate(rect => {
    const bounds = rect.getBoundingClientRect();
    return bounds.width > 0 && bounds.height > 0 && getComputedStyle(rect).fill !== 'none';
  }), 'the mounted highlight has nonzero geometry');
  assert.equal(await page.locator('#page .hover-preview-row-target').count(), 0, 'highlight belongs only to the preview clone');
}

const distantParagraphs = Array.from({ length: 160 }, (_, i) => `Distant paragraph ${i}.`).join('\n\n');
const distantReferences = String.raw`See \eqref{eq:first}, \eqref{eq:other}, \eqref{eq:proof}, and \ref{thm:far}.`;
const distantTheorem = String.raw`\begin{theorem}\label{thm:far}
${equation}
\begin{equation}\label{eq:other}a=b\end{equation}
\begin{proof}
\begin{equation}\label{eq:proof}c=d\end{equation}
\end{proof}
\end{theorem}`;
function distantSource(body) {
  return String.raw`\documentclass{article}
\newtheorem{theorem}{Theorem}
\begin{document}
${body}
\end{document}`;
}
const distantBody = `${distantReferences}\n\n${distantParagraphs}\n\n${distantTheorem}
\\begin{equation}\\label{eq:remote}u=v\\end{equation}`;

async function assertCold(page, selector) {
  assert.equal(await page.locator(`${selector} svg`).count(), 0, `${selector} should remain lazy`);
}

test('typing or deleting a space between emphasis and inline math updates the live preview', { timeout: 30000 }, async t => {
  const snippet = String.raw`\emph{at or after} $a$`;
  const { page, open, update } = await fixture(t, snippet);
  await open();
  const separator = () => page.locator('#page em').evaluate(el => {
    const next = el.parentElement.nextSibling;
    return next.nodeType === Node.TEXT_NODE ? next.textContent : '';
  });
  assert.equal(await separator(), ' ');
  await update(snippet.replace('} $', '}$'));
  assert.equal(await separator(), '');
  await update(snippet);
  assert.equal(await separator(), ' ');
});

test('hover renders a never-visited distant equation without rendering its neighbors', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, distantBody, distantSource);
  await open();
  await page.waitForFunction(() => window.__mpEngine.isReady());
  await assertCold(page, '#eq-first');
  const distance = await page.locator('#eq-first').evaluate(el => el.closest('main#page > .blk').getBoundingClientRect().top);
  assert.ok(distance > 1800, `source is only ${distance}px away`);
  assert.ok(await page.locator('#eq-first').evaluate(el => el.closest('main#page > .blk') === document.querySelector('#eq-other').closest('main#page > .blk')));
  const before = await page.evaluate(() => window.scrollY);
  await hover(page, '#page a.ref[data-target="eq:first"]');
  await page.waitForSelector(`${popup} svg`);
  await page.waitForSelector('#eq-first svg', { state: 'attached' });
  await delay(400); // include deferred synthetic content-visibility events
  await assertCold(page, '#eq-other');
  await assertCold(page, '#eq-proof');
  await assertCold(page, '#eq-remote');
  assert.deepEqual(await keys(page), ['eq:first']);
  assert.equal(await page.evaluate(() => window.scrollY), before);
  assert.equal(await page.locator('#eq-first').evaluate(el => el.closest('main#page > .blk').style.contentVisibility), '');
});

test('distant theorem demand excludes hidden proofs, but a direct proof equation reference works', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, distantBody, distantSource);
  await open();
  await hover(page, '#page a.ref[data-target="thm:far"]');
  await page.waitForFunction(() => document.querySelectorAll('.hover-preview .math.display svg').length === 2);
  await assertCold(page, '#eq-proof');
  await hover(page, '#page a.ref[data-target="eq:proof"]');
  await page.waitForSelector(`${popup} svg`);
  assert.deepEqual(await keys(page), ['eq:proof']);
});

test('dismissed or superseded distant demand is cancelled before engine readiness', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, distantBody, distantSource);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  const first = '#page a.ref[data-target="eq:first"]';
  await hover(page, first);
  await page.locator(first).dispatchEvent('mouseout');
  await hover(page, '#page a.ref[data-target="eq:other"]');
  release();
  await page.waitForSelector(`${popup} svg`);
  await delay(400);
  assert.deepEqual(await keys(page), ['eq:other']);
  await assertCold(page, '#eq-first');
  await assertCold(page, '#eq-proof');
});

test('a patch removing a distant target cancels its queued demand', { timeout: 30000 }, async t => {
  const { page, open, update } = await fixture(t, distantBody, distantSource);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#page a.ref[data-target="eq:first"]');
  await update(distantBody.replace(equation, 'Equation removed.'));
  await page.waitForSelector(popup, { state: 'detached' });
  release();
  await page.waitForFunction(() => window.__mpEngine.isReady());
  await delay(400);
  assert.equal(await page.locator(popup).count(), 0);
});

async function holdNextTypeset(page) {
  await page.waitForFunction(() => window.__mpEngine.isReady());
  await page.evaluate(() => {
    const engine = window.__mpEngine;
    const original = engine.typeset.bind(engine);
    engine.typeset = async nodes => {
      engine.typeset = original;
      await new Promise(resolveGate => { window.resumeDemandTest = resolveGate; });
      return original(nodes);
    };
  });
}

test('a live patch during demand does not enroll unrelated distant math', { timeout: 30000 }, async t => {
  const { page, open, update } = await fixture(t, distantBody, distantSource);
  await open();
  await holdNextTypeset(page);
  const reference = '#page a.ref[data-target="eq:first"]';
  await hover(page, reference);
  await page.waitForFunction(() => typeof window.resumeDemandTest === 'function');
  await update(distantBody.replace('Distant paragraph 0.', 'Edited paragraph 0.'));
  await page.locator(reference).dispatchEvent('mouseout');
  await page.evaluate(() => window.resumeDemandTest());
  await page.waitForSelector('#eq-first svg', { state: 'attached' });
  await delay(500);
  await assertCold(page, '#eq-other');
  await assertCold(page, '#eq-proof');
  assert.equal(await page.locator(popup).count(), 0);
});

test('a real scroll racing demand completion dismisses the hover', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, distantBody, distantSource);
  await open();
  await holdNextTypeset(page);
  await hover(page, '#page a.ref[data-target="eq:first"]');
  await page.waitForFunction(() => typeof window.resumeDemandTest === 'function');
  await page.evaluate(() => {
    window.scrollBy(0, 200);
    window.resumeDemandTest();
  });
  await page.waitForSelector(popup, { state: 'detached' });
  await page.waitForSelector('#eq-first svg', { state: 'attached' });
  await delay(300);
  assert.equal(await page.locator(popup).count(), 0);
  assert.ok(await page.evaluate(() => window.scrollY >= 199));
});

test('an engine failure restores lifted containment and leaves the shared queue usable', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, distantBody, distantSource);
  await open();
  await page.waitForFunction(() => window.__mpEngine.isReady());
  await page.evaluate(() => {
    const engine = window.__mpEngine;
    const original = engine.typeset.bind(engine);
    engine.typeset = async () => {
      engine.typeset = original;
      throw new Error('Intentional hover test failure');
    };
  });
  await hover(page, '#page a.ref[data-target="eq:first"]');
  await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('engine error'));
  assert.deepEqual(await page.locator('#eq-first').evaluate(el => {
    const style = el.closest('main#page > .blk').style;
    return [style.contentVisibility, style.contain];
  }), ['', '']);
  await hover(page, '#page a.ref[data-target="eq:other"]');
  await page.waitForSelector(`${popup} svg`);
  assert.deepEqual(await keys(page), ['eq:other']);
});

test('an unvisited source above the reader renders without dismissing the popup or moving the reference', { timeout: 30000 }, async t => {
  const body = `${distantParagraphs}\n\n${distantTheorem}\n\n${distantParagraphs}\n\n\\section{References}\\label{bottom}\n${distantReferences}`;
  const { page, open } = await fixture(t, body, distantSource);
  await open('#bottom');
  const reference = '#page a.ref[data-target="eq:first"]';
  await page.locator(reference).scrollIntoViewIfNeeded();
  await page.waitForFunction(() => window.__mpEngine.isReady());
  await delay(400);
  await assertCold(page, '#eq-first');
  const before = await page.locator(reference).evaluate(el => el.getBoundingClientRect().top);
  await hover(page, reference);
  await page.waitForSelector(`${popup} svg`);
  await delay(400);
  assert.equal(await page.locator(popup).count(), 1);
  const afterTop = await page.locator(reference).evaluate(el => el.getBoundingClientRect().top);
  assert.ok(Math.abs(afterTop - before) < 2, `reference moved by ${afterTop - before}px`);
  await assertCold(page, '#eq-other');
  await page.evaluate(() => window.scrollBy(0, -200));
  await page.waitForSelector(popup, { state: 'detached' });
});

test('the previous viewer shell is asked to reload onto the fixed client', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  await open();
  const event = await page.evaluate(() => new Promise((resolveEvent, reject) => {
    const socket = new WebSocket(`ws://${location.host}/ws?v=87`);
    const timer = setTimeout(() => { socket.close(); reject(new Error('No reload message')); }, 5000);
    socket.onmessage = ({ data }) => {
      clearTimeout(timer);
      socket.close();
      resolveEvent(JSON.parse(data).event);
    };
  }));
  assert.equal(event, 'full-reload');
});

test('number hover shows labels only before and after MathJax is ready', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#eq-first .eq-num');
  await assertLabelOnly(page, ['eq:first', 'eq:alias']);
  release();
  await page.waitForSelector('#eq-first svg');
  await assertLabelOnly(page, ['eq:first', 'eq:alias']);
});

test('a number-only hover never demands a distant equation', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, distantBody, distantSource);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#eq-first .eq-num');
  await assertLabelOnly(page, ['eq:first', 'eq:alias']);
  release();
  await page.waitForFunction(() => window.__mpEngine.isReady());
  await delay(500);
  await assertCold(page, '#eq-first');
  await assertLabelOnly(page, ['eq:first', 'eq:alias']);
});

test('the pointer crosses from a number to its popup and copies only the clicked alias', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  await open();
  await page.waitForSelector('#eq-first svg');
  await stubClipboard(page);
  const alias = await pointerIntoLabelPopup(page, '#eq-first .eq-num', 'eq:alias');
  await assertLabelOnly(page, ['eq:first', 'eq:alias']);
  assert.deepEqual(await page.evaluate(() => window.hoverTestCopies), [], 'hovering does not access the clipboard');
  await alias.click();
  await page.waitForFunction(() => document.querySelector('.equation-label-copy-status')?.textContent === 'Copied eq:alias');
  assert.deepEqual(await page.evaluate(() => window.hoverTestCopies), [{ text: 'eq:alias', trusted: true }]);
  await page.locator(`${popup} .equation-label-copy[data-refkey="eq:first"]`).click();
  await page.waitForFunction(() => document.querySelector('.equation-label-copy-status')?.textContent === 'Copied eq:first');
  assert.deepEqual(await page.evaluate(() => window.hoverTestCopies.map(copy => copy.text)), ['eq:alias', 'eq:first']);
  await assertLabelOnly(page, ['eq:first', 'eq:alias']);
});

test('dragging across a visible label selects text without automatically writing the clipboard', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  await open();
  await page.waitForSelector('#eq-first svg');
  await stubClipboard(page);
  const alias = await pointerIntoLabelPopup(page, '#eq-first .eq-num', 'eq:alias');
  const code = await alias.locator('code').boundingBox();
  await page.mouse.move(code.x + 1, code.y + code.height / 2);
  await page.mouse.down();
  await page.mouse.move(code.x + code.width - 1, code.y + code.height / 2, { steps: 12 });
  await page.mouse.up();
  const selected = await page.evaluate(() => window.getSelection().toString());
  assert.deepEqual(await page.evaluate(() => window.hoverTestCopies), [], 'pointer dragging is selection, never a clipboard write');
  assert.ok(selected.length >= 4 && 'eq:alias'.includes(selected), `expected a label text selection, got ${JSON.stringify(selected)}`);
  await assertLabelOnly(page, ['eq:first', 'eq:alias']);
  // A subsequent deliberate click still copies, even with selected label text.
  await alias.click();
  await page.waitForFunction(() => window.hoverTestCopies.length === 1);
  assert.equal(await page.evaluate(() => window.hoverTestCopies[0].text), 'eq:alias');
});

for (const clipboardMode of ['reject', 'unavailable']) {
  test(`clipboard ${clipboardMode} selects the visible label for keyboard copy without copying selected math`, { timeout: 30000 }, async t => {
    const { page, open } = await fixture(t, `${probabilityAlign}\n${probabilityReferences}`, probabilitySource);
    await open();
    await page.waitForSelector('#eq-main-early svg');
    // MathJax groups deliberately use pointer-events:none; the real SVG
    // receives this coordinate click and the viewer resolves its row.
    const row = await page.locator('#eq-main-early svg [data-mml-node="mtable"] > [data-mml-node="mtr"]').first().boundingBox();
    await page.mouse.click(row.x + row.width / 2, row.y + row.height / 2);
    assert.equal(await page.locator('#eq-main-early rect.mp-row-select').count(), 1);
    await stubClipboard(page, clipboardMode);
    const label = await pointerIntoLabelPopup(page, '#eq-main-early .eq-num-row:nth-child(2)', 'eq:main-late');
    await label.click();
    await page.waitForFunction(() => window.getSelection()?.toString() === 'eq:main-late');
    assert.match(await page.locator(`${popup} [role="status"]`).textContent(), /Selected label.*Ctrl\+C.*⌘C/);
    await delay(300);
    await assertLabelOnly(page, ['eq:main-late']);
    const copy = await captureKeyboardCopy(page);
    assert.equal(copy.trusted, true);
    assert.equal(copy.text || copy.selection, 'eq:main-late');
    assert.ok(!copy.text.includes('\\PP'), 'the earlier math-row selection cannot hijack label copy');
    assert.deepEqual(await page.evaluate(() => window.hoverTestCopies.map(item => item.text)), clipboardMode === 'reject' ? ['eq:main-late'] : []);
  });
}

test('a label popup dismisses after leaving both surfaces, outside click, Escape, and scrolling', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, `${equation}\n\n${distantParagraphs}`);
  await open();
  await page.waitForSelector('#eq-first svg');
  await pointerIntoLabelPopup(page, '#eq-first .eq-num', 'eq:first');
  await page.mouse.move(5, 850);
  await page.waitForSelector(popup, { state: 'detached' });
  await pointerIntoLabelPopup(page, '#eq-first .eq-num', 'eq:first');
  await page.mouse.click(5, 850);
  await page.waitForSelector(popup, { state: 'detached' });
  await pointerIntoLabelPopup(page, '#eq-first .eq-num', 'eq:first');
  await page.keyboard.press('Escape');
  await page.waitForSelector(popup, { state: 'detached' });
  await page.mouse.move(5, 850);
  await pointerIntoLabelPopup(page, '#eq-first .eq-num', 'eq:first');
  await page.evaluate(() => window.scrollBy(0, 150));
  await page.waitForSelector(popup, { state: 'detached' });
});

test('labeled numbers and their copy buttons support keyboard focus and activation', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  await open();
  await page.waitForSelector('#eq-first svg');
  await stubClipboard(page);
  const number = page.locator('#eq-first .eq-num');
  assert.equal(await number.getAttribute('tabindex'), '0');
  await number.focus();
  await page.keyboard.press('Enter');
  await page.waitForFunction(() => document.activeElement?.matches('.equation-label-copy[data-refkey="eq:first"]'));
  await page.keyboard.press('Shift+Tab');
  assert.equal(await page.evaluate(() => document.activeElement === document.querySelector('#eq-first .eq-num')), true);
  await page.keyboard.press('Tab');
  await page.waitForFunction(() => document.activeElement?.matches('.equation-label-copy[data-refkey="eq:first"]'));
  await page.keyboard.press('Enter');
  await page.waitForFunction(() => window.hoverTestCopies.length === 1);
  await page.keyboard.press('Tab');
  assert.equal(await page.evaluate(() => document.activeElement.dataset.refkey), 'eq:alias');
  await page.keyboard.press('Space');
  await page.waitForFunction(() => window.hoverTestCopies.length === 2);
  assert.deepEqual(await page.evaluate(() => window.hoverTestCopies.map(copy => copy.text)), ['eq:first', 'eq:alias']);
  assert.equal(await page.evaluate(() => window.hoverTestCopies.every(copy => copy.trusted)), true, 'keyboard copies run from trusted key activation');
  await page.mouse.move(5, 850);
  await delay(300);
  await assertLabelOnly(page, ['eq:first', 'eq:alias']);
  await page.keyboard.press('Escape');
  await page.waitForSelector(popup, { state: 'detached' });
  assert.equal(await page.evaluate(() => document.activeElement === document.querySelector('#eq-first')), true, 'Escape restores the existing math focus target without reopening the popup');
  // MathJax has its own focusable menu between the math wrapper and number.
  await number.focus();
  await page.keyboard.press('Tab');
  await page.waitForFunction(() => document.activeElement?.matches('.equation-label-copy[data-refkey="eq:first"]'));
});

test('number clicks still select the equation row for copy and modified clicks still reveal source', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, `${probabilityAlign}\n${probabilityReferences}`, probabilitySource);
  const jumps = [];
  for (const path of ['/jump', '/reveal-source']) {
    await page.route(`**${path}`, async route => {
      jumps.push({ path, body: route.request().postDataJSON() });
      await route.fulfill({ status: 204 });
    });
  }
  await open();
  await page.waitForSelector('#eq-main-early svg');
  const number = page.locator('#eq-main-early .eq-num-row:nth-child(2)');
  await number.click();
  assert.equal(await page.locator('#eq-main-early rect.mp-row-select').count(), 1);
  const copy = await captureKeyboardCopy(page);
  assert.match(copy.text, /\\PP\(T_N>a\+K\\sqrt L\)/);
  assert.ok(copy.text.includes('eq:main-late'));
  assert.ok(!copy.text.includes('eq:main-early'));
  await number.click({ modifiers: ['Meta'] });
  await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('editor'));
  assert.deepEqual(jumps.map(jump => jump.path).sort(), ['/jump', '/reveal-source']);
  assert.ok(jumps.every(jump => Number.isInteger(jump.body.line) && jump.body.line > 0));
});

test('a live label change closes a focused copy popup and removes its stale controls', { timeout: 30000 }, async t => {
  const { page, open, update } = await fixture(t);
  await open();
  await page.waitForSelector('#eq-first svg');
  await hover(page, '#eq-first .eq-num');
  await page.locator(`${popup} .equation-label-copy[data-refkey="eq:alias"]`).focus();
  await update(equation.replace('eq:alias', 'eq:renamed'));
  await page.waitForSelector(popup, { state: 'detached' });
  assert.equal(await page.locator('.equation-label-copy').count(), 0);
  assert.equal(await page.evaluate(() => document.activeElement?.isConnected), true);
  await hover(page, '#eq-first .eq-num');
  await assertLabelOnly(page, ['eq:first', 'eq:renamed']);
});

test('a delayed clipboard failure cannot change a replacement label popup', { timeout: 30000 }, async t => {
  const math = String.raw`${equation}
\begin{equation}\label{eq:other}a=b\end{equation}`;
  const { page, open } = await fixture(t, math);
  await open();
  await page.waitForSelector('#eq-first svg');
  await stubClipboard(page, 'pending');
  const alias = await pointerIntoLabelPopup(page, '#eq-first .eq-num', 'eq:alias');
  await alias.click();
  await page.waitForFunction(() => typeof window.rejectHoverTestCopy === 'function');
  await page.keyboard.press('Escape');
  await page.waitForSelector(popup, { state: 'detached' });
  await pointerIntoLabelPopup(page, '#eq-other .eq-num', 'eq:other');
  await page.evaluate(() => window.rejectHoverTestCopy(new DOMException('Permission denied', 'NotAllowedError')));
  await delay(300);
  await assertLabelOnly(page, ['eq:other']);
  assert.equal(await page.locator(`${popup} [role="status"]`).textContent(), 'Click a label to copy');
  assert.notEqual(await page.evaluate(() => window.getSelection().toString()), 'eq:alias');
});

test('out-of-order clipboard results preserve the latest clicked alias status', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  await open();
  await page.waitForSelector('#eq-first svg');
  await stubClipboard(page, 'pending');
  const alias = await pointerIntoLabelPopup(page, '#eq-first .eq-num', 'eq:alias');
  await alias.click();
  await page.locator(`${popup} .equation-label-copy[data-refkey="eq:first"]`).click();
  await page.waitForFunction(() => window.hoverTestPendingCopies.length === 2);
  await page.evaluate(() => window.hoverTestPendingCopies[1].resolveCopy());
  await page.waitForFunction(() => document.querySelector('.equation-label-copy-status')?.textContent === 'Copied eq:first');
  await page.evaluate(() => window.hoverTestPendingCopies[0].rejectCopy(new DOMException('Permission denied', 'NotAllowedError')));
  await delay(300);
  assert.equal(await page.locator(`${popup} [role="status"]`).textContent(), 'Copied eq:first');
  assert.notEqual(await page.evaluate(() => window.getSelection().toString()), 'eq:alias');
});

test('a late clipboard rejection cannot steal focus after Tab moves to another alias', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  await open();
  await page.waitForSelector('#eq-first svg');
  await stubClipboard(page, 'pending');
  const first = await pointerIntoLabelPopup(page, '#eq-first .eq-num', 'eq:first');
  await first.click();
  await page.waitForFunction(() => window.hoverTestPendingCopies.length === 1);
  await page.keyboard.press('Tab');
  assert.equal(await page.evaluate(() => document.activeElement.dataset.refkey), 'eq:alias');
  await page.evaluate(() => window.hoverTestPendingCopies[0].rejectCopy(new DOMException('Permission denied', 'NotAllowedError')));
  await delay(300);
  assert.equal(await page.evaluate(() => document.activeElement.dataset.refkey), 'eq:alias');
  assert.equal(await page.locator(`${popup} [role="status"]`).textContent(), 'Click a label to copy');
  assert.notEqual(await page.evaluate(() => window.getSelection().toString()), 'eq:first');
});

test('alias reference hover also follows delayed MathJax', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#page a.ref[data-target="eq:alias"]');
  assert.deepEqual(await keys(page), ['eq:alias']);
  assert.equal(await page.locator(popup).getAttribute('aria-hidden'), 'true');
  assert.equal(await page.locator(popup).getAttribute('inert'), '');
  assert.equal(await page.locator(`${popup} .equation-label-copy`).count(), 0);
  release();
  await page.waitForSelector(`${popup} .math.display svg`);
});

test('leaving a number dismisses its label without later resurrection', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#eq-first .eq-num');
  await page.locator('#eq-first .eq-num').dispatchEvent('mouseout');
  await page.waitForSelector(popup, { state: 'detached' });
  release();
  await page.waitForSelector('#eq-first svg');
  await delay(350);
  assert.equal(await page.locator(popup).count(), 0, 'typesetting must not resurrect a dismissed hover');
});

test('removing an equation during a patch dismisses its number label', { timeout: 30000 }, async t => {
  const { page, open, update } = await fixture(t);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#eq-first .eq-num');
  await update('The displayed equation was removed.');
  await page.waitForSelector('#eq-first', { state: 'detached' });
  await page.waitForSelector(popup, { state: 'detached' });
  release();
  await page.waitForFunction(() => window.__mpEngine.isReady());
  await delay(350);
  assert.equal(await page.locator(popup).count(), 0);
});

test('an equation inside a proof still refreshes when previewed by reference', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, String.raw`\begin{proof}
${equation}
\end{proof}`);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#page a.ref[data-target="eq:first"]');
  assert.deepEqual(await keys(page), ['eq:first']);
  release();
  await page.waitForSelector('#eq-first svg');
  await page.waitForSelector(`${popup} svg`);
});

test('live label moves change the hovered row without a reload', { timeout: 30000 }, async t => {
  const beforeMove = String.raw`\begin{align}
a&=b\label{eq:moved}\\
c&=d
\end{align}`;
  const afterMove = String.raw`\begin{align}
a&=b\\
c&=d\label{eq:moved}
\end{align}`;
  const { page, open, update } = await fixture(t, beforeMove);
  await open();
  await page.waitForSelector('#eq-moved svg');
  await hover(page, '#eq-moved .eq-num-row:first-child');
  await assertLabelOnly(page, ['eq:moved']);
  await update(afterMove);
  await page.waitForFunction(() => document.querySelector('#eq-moved')?.dataset.tex.includes('c&=d\\label{eq:moved}'));
  await page.waitForSelector(popup, { state: 'detached' });
  await page.locator('#eq-moved .eq-num-row:first-child').dispatchEvent('mouseover');
  await delay(350);
  assert.equal(await page.locator(popup).count(), 0, 'old row is now unlabeled');
  await hover(page, '#eq-moved .eq-num-row:nth-child(2)');
  await assertLabelOnly(page, ['eq:moved']);
});

test('commented labels never appear in align or gather hover headers', { timeout: 30000 }, async t => {
  const math = String.raw`\begin{align}
a&=b\label{eq:align} % \label{eq:align-ghost}
\end{align}
\begin{gather}
c=d\label{eq:gather} % \label{eq:gather-ghost}
\end{gather}`;
  const { page, open } = await fixture(t, math);
  await open();
  await hover(page, '#eq-align .eq-num-row');
  await assertLabelOnly(page, ['eq:align']);
  await page.locator('#eq-align .eq-num-row').dispatchEvent('mouseout');
  await hover(page, '#eq-gather .eq-num-row');
  await assertLabelOnly(page, ['eq:gather']);
});

test('each probability estimate reference highlights its own align row, while numbers show only labels', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, `${probabilityAlign}\n${probabilityReferences}`, probabilitySource);
  await open();
  for (const [row, key] of ['eq:main-early', 'eq:main-late'].entries()) {
    await hover(page, `#page a.ref[data-target="${key}"]`);
    await assertHighlightedRow(page, row);
    assert.deepEqual(await keys(page), [key]);
    assert.equal(await page.locator(`${popup} [data-mml-node="merror"]`).count(), 0, 'the custom probability macro renders');
    assert.equal(await page.locator(`${popup} .math.display[id]`).count(), 0);
    await hover(page, `#eq-main-early .eq-num-row:nth-child(${row + 1})`);
    await assertLabelOnly(page, [key]);
  }
});

test('at 200 percent hover size both row bands fit without clipping by an inner ancestor', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, `${probabilityAlign}\n${probabilityReferences}`, probabilitySource);
  await open();
  await page.evaluate(() => document.documentElement.style.setProperty('--hover-preview-scale', '2'));
  for (const [row, key] of ['eq:main-early', 'eq:main-late'].entries()) {
    await hover(page, `#page a.ref[data-target="${key}"]`);
    await assertHighlightedRow(page, row);
    const geometry = await page.locator(`${popup} rect.hover-preview-row-highlight`).evaluate(band => {
      const box = band.closest('.hover-preview');
      const bounds = band.getBoundingClientRect();
      const popupBounds = box.getBoundingClientRect();
      const clipped = [];
      for (let ancestor = band.parentElement; ancestor && ancestor !== box; ancestor = ancestor.parentElement) {
        const style = getComputedStyle(ancestor);
        const ancestorBounds = ancestor.getBoundingClientRect();
        const clipsX = style.overflowX !== 'visible' &&
          (bounds.left < ancestorBounds.left - 0.5 || bounds.right > ancestorBounds.right + 0.5);
        const clipsY = style.overflowY !== 'visible' &&
          (bounds.top < ancestorBounds.top - 0.5 || bounds.bottom > ancestorBounds.bottom + 0.5);
        if (clipsX || clipsY) clipped.push(`${ancestor.tagName}.${ancestor.getAttribute('class') || ''} [overflow: ${style.overflowX}/${style.overflowY}]`);
      }
      return {
        fitsX: bounds.left >= popupBounds.left && bounds.right <= popupBounds.right,
        fitsY: bounds.top >= popupBounds.top && bounds.bottom <= popupBounds.bottom,
        clipped,
      };
    });
    assert.deepEqual(geometry, { fitsX: true, fitsY: true, clipped: [] });
  }
});

test('wide highlighted alignments are clipped by the outer popup without escaping the viewport', { timeout: 30000 }, async t => {
  const wideExpression = Array.from({ length: 60 }, (_, i) => `x_{${i}}`).join('+');
  const math = String.raw`\begin{align}
y&=${wideExpression}\label{eq:wide}\\
z&=0\label{eq:short}
\end{align}
See \eqref{eq:wide}.`;
  const { page, open } = await fixture(t, math);
  await page.setViewportSize({ width: 800, height: 900 });
  await open();
  await page.evaluate(() => document.documentElement.style.setProperty('--hover-preview-scale', '2'));
  await hover(page, '#page a.ref[data-target="eq:wide"]');
  await assertHighlightedRow(page, 0);
  const geometry = await page.locator(popup).evaluate(box => {
    const bounds = box.getBoundingClientRect();
    const math = box.querySelector('.math.display');
    const band = box.querySelector('rect.hover-preview-row-highlight').getBoundingClientRect();
    return {
      boundedX: bounds.left >= 0 && bounds.right <= window.innerWidth,
      boundedY: bounds.top >= 0 && bounds.bottom <= window.innerHeight,
      scrollsX: box.scrollWidth > box.clientWidth,
      outerOverflow: getComputedStyle(box).overflowX,
      innerOverflow: getComputedStyle(math).overflowX,
      bandExceedsBox: band.width > box.clientWidth,
      pageBounded: document.documentElement.scrollWidth <= window.innerWidth,
    };
  });
  assert.deepEqual(geometry, {
    boundedX: true, boundedY: true, scrollsX: true, outerOverflow: 'auto',
    innerOverflow: 'visible', bandExceedsBox: true, pageBounded: true,
  });
});

test('distant align references gain the right row highlight when delayed MathJax finishes', { timeout: 30000 }, async t => {
  const body = `${probabilityReferences}\n\n${distantParagraphs}\n\n${probabilityAlign}\n${equation}`;
  const { page, open } = await fixture(t, body, probabilitySource);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await assertCold(page, '#eq-main-early');
  const before = await page.evaluate(() => window.scrollY);
  await hover(page, '#page a.ref[data-target="eq:main-late"]');
  assert.deepEqual(await keys(page), ['eq:main-late']);
  assert.equal(await page.locator(`${popup} .hover-preview-row-target`).count(), 0);
  release();
  await assertHighlightedRow(page, 1);
  await assertCold(page, '#eq-first');
  assert.equal(await page.evaluate(() => window.scrollY), before);
  await hover(page, '#page a.ref[data-target="eq:main-early"]');
  await assertHighlightedRow(page, 0);
});

test('a primary label and its aliases on the second row select the outer align row, not a nested matrix', { timeout: 30000 }, async t => {
  const math = String.raw`\begin{align}
A&=\begin{pmatrix}a&b\\c&d\end{pmatrix}\\
B&=C\label{eq:second}\label{eq:second-alias}
\end{align}
See \eqref{eq:second} and \eqref{eq:second-alias}.`;
  const { page, open } = await fixture(t, math);
  await open();
  for (const key of ['eq:second', 'eq:second-alias']) {
    await hover(page, `#page a.ref[data-target="${key}"]`);
    await assertHighlightedRow(page, 1);
    assert.ok(await page.locator(`${popup} [data-mml-node="mtable"]`).count() > 1, 'fixture contains a nested matrix');
    assert.deepEqual(await keys(page), [key]);
  }
  await hover(page, '#eq-second .eq-num-row:nth-child(2)');
  await assertLabelOnly(page, ['eq:second', 'eq:second-alias']);
  await page.locator('#eq-second .eq-num-row:nth-child(2)').dispatchEvent('mouseout');
  await page.locator('#eq-second .eq-num-row:first-child').dispatchEvent('mouseover');
  await delay(350);
  assert.equal(await page.locator(popup).count(), 0, 'the unlabeled first row has no label tooltip');
});

test('gather references keep their row mapping across an unnumbered row', { timeout: 30000 }, async t => {
  const math = String.raw`\begin{gather}
a=b\label{eq:top}\\
c=d\notag\\
e=f\label{eq:bottom}
\end{gather}
See \eqref{eq:top} and \eqref{eq:bottom}.`;
  const { page, open } = await fixture(t, math);
  await open();
  await hover(page, '#page a.ref[data-target="eq:top"]');
  await assertHighlightedRow(page, 0, 3);
  await hover(page, '#page a.ref[data-target="eq:bottom"]');
  await assertHighlightedRow(page, 2, 3);
  await hover(page, '#eq-top .eq-num-row:nth-child(3)');
  await assertLabelOnly(page, ['eq:bottom']);
});

test('moving a referenced label dismisses its old highlight and highlights the new row on next hover', { timeout: 30000 }, async t => {
  const beforeMove = String.raw`\begin{align}
a&=b\label{eq:moved}\\
c&=d
\end{align}
See \eqref{eq:moved}.`;
  const afterMove = beforeMove.replace('a&=b\\label{eq:moved}', 'a&=b').replace('c&=d', 'c&=d\\label{eq:moved}');
  const { page, open, update } = await fixture(t, beforeMove);
  await open();
  await hover(page, '#page a.ref[data-target="eq:moved"]');
  await assertHighlightedRow(page, 0);
  await update(afterMove);
  await page.waitForSelector(popup, { state: 'detached' });
  await hover(page, '#page a.ref[data-target="eq:moved"]');
  await assertHighlightedRow(page, 1);
});

test('an unrelated live patch preserves a highlighted reference preview', { timeout: 30000 }, async t => {
  const math = `${probabilityAlign}\n${probabilityReferences}\n\nUnrelated text before editing.`;
  const { page, open, update } = await fixture(t, math, probabilitySource);
  await open();
  await hover(page, '#page a.ref[data-target="eq:main-late"]');
  await assertHighlightedRow(page, 1);
  await update(math.replace('Unrelated text before editing.', 'Unrelated text after editing.'));
  await assertHighlightedRow(page, 1);
  assert.deepEqual(await keys(page), ['eq:main-late']);
});

test('reference row highlighting does not copy an unrelated editor or copy selection', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, `${probabilityAlign}\n${probabilityReferences}`, probabilitySource);
  await open();
  await page.waitForSelector('#eq-main-early svg');
  await page.locator('#eq-main-early').evaluate(math => {
    math.classList.add('source-active', 'math-selected');
    const row = math.querySelector('[data-mml-node="mtr"]');
    for (const className of ['mp-row-hl', 'mp-row-select']) {
      const rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
      rect.setAttribute('class', className);
      row.prepend(rect);
    }
  });
  await hover(page, '#page a.ref[data-target="eq:main-late"]');
  await assertHighlightedRow(page, 1);
  assert.equal(await page.locator(`${popup} .source-active, ${popup} .math-selected, ${popup} rect.mp-row-hl, ${popup} rect.mp-row-select`).count(), 0);
  assert.equal(await page.locator('#eq-main-early.source-active.math-selected').count(), 1, 'source selection is untouched');
  assert.equal(await page.locator('#eq-main-early rect.mp-row-hl, #eq-main-early rect.mp-row-select').count(), 2);
});

test('a stale align preview waits for the new row layout before highlighting', { timeout: 30000 }, async t => {
  const math = `${probabilityAlign}\n${probabilityReferences}`;
  const { page, open, update } = await fixture(t, math, probabilitySource);
  await open();
  await page.waitForSelector('#eq-main-early svg');
  await holdNextTypeset(page);
  await update(math.replace('\\begin{align}', String.raw`\begin{align}
z&=0\\`));
  await page.waitForSelector('#eq-main-early[data-mp-stale]');
  await page.waitForFunction(() => typeof window.resumeDemandTest === 'function');
  await hover(page, '#page a.ref[data-target="eq:main-late"]');
  assert.equal(await page.locator(`${popup} .hover-preview-row-target`).count(), 0, 'old two-row SVG cannot identify the new third row');
  await page.evaluate(() => window.resumeDemandTest());
  await assertHighlightedRow(page, 2, 3);
  assert.deepEqual(await keys(page), ['eq:main-late']);
});

test('an unlabeled single equation number opens no empty tooltip', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, String.raw`\begin{equation}a=b\end{equation}`);
  await open();
  await page.locator('#page .eq-num').dispatchEvent('mouseover');
  await delay(350);
  assert.equal(await page.locator(popup).count(), 0);
});

test('hover opened during a live stale-math placeholder updates to the new equation', { timeout: 30000 }, async t => {
  const { page, open, update } = await fixture(t);
  await open();
  await page.waitForSelector('#eq-first svg');
  // Hold only this typesetting batch; MathJax itself and the full patch
  // pipeline remain real. The daemon's anti-flash SVG is then observable.
  await page.evaluate(() => {
    const engine = window.__mpEngine;
    const original = engine.typeset.bind(engine);
    engine.typeset = async nodes => {
      await new Promise(resolveGate => { window.resumeHoverTestTypeset = resolveGate; });
      return original(nodes);
    };
  });
  await update(equation.replace('mc^2', 'mc^3'));
  await page.waitForSelector('#eq-first[data-mp-stale]');
  await page.waitForFunction(() => typeof window.resumeHoverTestTypeset === 'function');
  await hover(page, '#page a.ref[data-target="eq:first"]');
  assert.equal(await page.locator(`${popup} [data-mp-stale]`).count(), 1);
  assert.ok(await page.locator(`${popup} [data-c="32"]`).count(), 'placeholder still shows exponent 2');
  await page.evaluate(() => window.resumeHoverTestTypeset());
  await page.waitForFunction(() => {
    const math = document.querySelector('#eq-first');
    const preview = document.querySelector('.hover-preview-body .math.display');
    return math && preview && !math.hasAttribute('data-mp-stale') &&
      !preview.hasAttribute('data-mp-stale') &&
      preview.querySelector('[data-c="33"]');
  });
  assert.equal(await page.locator(`${popup} [data-c="32"]`).count(), 0);
  assert.deepEqual(await keys(page), ['eq:first']);
});
