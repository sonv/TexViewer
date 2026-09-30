import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { after, before, test } from 'node:test';
import { chromium } from 'playwright';

// Exercise the real embedded bundle, daemon patches, and vendored MathJax.
// `npm run test:hover` builds the CLI first so edits to include_str! assets
// cannot accidentally be tested against an old binary.
const binary = resolve('target/debug/mathpreview-cli');
const popup = '.hover-preview';
let browser;
before(async () => { browser = await chromium.launch(); });
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

async function fixture(t, math = equation) {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-hover-test-'));
  const input = join(directory, 'notes.tex');
  await writeFile(input, documentSource(math));
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
  async function open() {
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('live'));
  }
  async function update(nextMath) {
    events.length = 0;
    const response = await fetch(`${url}/buffer`, {
      method: 'POST',
      headers: { 'x-mathpreview-path': input },
      body: documentSource(nextMath),
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
  await page.locator(selector).dispatchEvent('mouseover');
  await page.waitForSelector(popup);
}

async function keys(page) {
  return page.locator(`${popup} .hover-preview-keys code`).allTextContents();
}

test('the previous viewer shell is asked to reload onto the fixed client', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  await open();
  const event = await page.evaluate(() => new Promise((resolveEvent, reject) => {
    const socket = new WebSocket(`ws://${location.host}/ws?v=83`);
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
