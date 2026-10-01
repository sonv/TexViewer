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
    const socket = new WebSocket(`ws://${location.host}/ws?v=85`);
    const timer = setTimeout(() => { socket.close(); reject(new Error('No reload message')); }, 5000);
    socket.onmessage = ({ data }) => {
      clearTimeout(timer);
      socket.close();
      resolveEvent(JSON.parse(data).event);
    };
  }));
  assert.equal(event, 'full-reload');
});

test('number hover follows delayed MathJax and preserves aliases safely', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#eq-first .eq-num');
  assert.deepEqual(await keys(page), ['eq:first', 'eq:alias']);
  assert.equal(await page.locator(`${popup} svg`).count(), 0);
  release();
  await page.waitForSelector('#eq-first svg');
  await page.waitForSelector(`${popup} svg`);
  assert.deepEqual(await keys(page), ['eq:first', 'eq:alias']);
  assert.equal(await page.locator(popup).getAttribute('aria-hidden'), 'true');
  assert.equal(await page.locator(popup).getAttribute('inert'), '');
  assert.equal(await page.locator(`${popup} [tabindex]`).count(), 0);
  assert.equal(await page.locator(`${popup} .math.display[id]`).count(), 0);
});

test('alias reference hover also follows delayed MathJax', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#page a.ref[data-target="eq:alias"]');
  assert.deepEqual(await keys(page), ['eq:alias']);
  release();
  await page.waitForSelector(`${popup} .math.display svg`);
});

test('leaving the number cancels its pending rendering observation', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  const release = await holdMathJax(page);
  t.after(release);
  await open();
  await hover(page, '#eq-first .eq-num');
  await page.locator('#eq-first .eq-num').dispatchEvent('mouseout');
  assert.equal(await page.locator(popup).count(), 0);
  release();
  await page.waitForSelector('#eq-first svg');
  await delay(350);
  assert.equal(await page.locator(popup).count(), 0, 'typesetting must not resurrect a dismissed hover');
});

test('removing an equation during a patch cancels its pending hover', { timeout: 30000 }, async t => {
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
  assert.deepEqual(await keys(page), ['eq:moved']);
  await update(afterMove);
  await page.waitForFunction(() => document.querySelector('#eq-moved')?.dataset.tex.includes('c&=d\\label{eq:moved}'));
  await page.waitForSelector(popup, { state: 'detached' });
  await page.locator('#eq-moved .eq-num-row:first-child').dispatchEvent('mouseover');
  await delay(350);
  assert.equal(await page.locator(popup).count(), 0, 'old row is now unlabeled');
  await hover(page, '#eq-moved .eq-num-row:nth-child(2)');
  assert.deepEqual(await keys(page), ['eq:moved']);
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
  assert.deepEqual(await keys(page), ['eq:align']);
  await page.locator('#eq-align .eq-num-row').dispatchEvent('mouseout');
  await hover(page, '#eq-gather .eq-num-row');
  assert.deepEqual(await keys(page), ['eq:gather']);
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
  await hover(page, '#eq-first .eq-num');
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
  assert.deepEqual(await keys(page), ['eq:first', 'eq:alias']);
});
