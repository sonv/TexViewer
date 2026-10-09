import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { after, before, test } from 'node:test';
import { chromium, webkit } from 'playwright';

// The CLI embeds both source anchors and the viewer. `npm run test:figures`
// rebuilds it before exercising the real cursor and jump endpoints.
const binary = resolve('target/debug/mathpreview-cli');
const browserName = process.env.MATHPREVIEW_TEST_BROWSER || 'chromium';
const browserType = { chromium, webkit }[browserName];
assert.ok(browserType, `Unsupported test browser: ${browserName}`);
let browser;
before(async () => { browser = await browserType.launch(); });
after(async () => { await browser?.close(); });

const image = '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#1598ad"/></svg>';
const padding = Array.from({ length: 30 }, (_, i) => `Stable paragraph ${i}.`).join('\n\n');
function tex(body) {
  return String.raw`\documentclass{article}
\usepackage{graphicx}
\begin{document}
Before the figures.

${body}

${padding}
\end{document}
`;
}

const figure = String.raw`\begin{figure}
\centering
\includegraphics[width=3cm]{demo.svg}
\caption[Short description]{Opening words.
Laterword gapword concludes the caption.
\emph{Finalword} adds detail.}
\label{fig:sample}
\end{figure}`;

function position(source, needle) {
  const offset = source.indexOf(needle);
  assert.ok(offset >= 0, `Missing source text: ${needle}`);
  const before = source.slice(0, offset);
  return {
    line: before.split('\n').length,
    col: Buffer.byteLength(before.slice(before.lastIndexOf('\n') + 1)) + 1,
  };
}

async function fixture(t, source, files = {}) {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-figure-sync-test-'));
  const input = join(directory, 'notes.tex');
  await writeFile(input, source);
  await writeFile(join(directory, 'demo.svg'), image);
  // Never compile project TeX or inherit a user's diagram-rendering choice.
  await writeFile(join(directory, '.mathpreview.toml'), '[viewer]\nrender-tikz = false\n');
  for (const [name, body] of Object.entries(files)) await writeFile(join(directory, name), body);
  const portProbe = createServer();
  portProbe.listen(0, '127.0.0.1');
  await once(portProbe, 'listening');
  const port = portProbe.address().port;
  await new Promise(resolveClose => portProbe.close(resolveClose));
  const url = `http://127.0.0.1:${port}`;
  // Cmd-click also sends /reveal-source. Disable its editor command while
  // keeping /jump live, so these tests cannot open or change a user's editor.
  const daemon = spawn(binary, ['serve', input, '--port', String(port), '--editor', ''], {
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
    try { events.push(JSON.parse(String(payload))); } catch { /* non-JSON frame */ }
  }));
  t.after(async () => {
    await context.close();
    daemon.kill('SIGTERM');
    await exited;
    await rm(directory, { recursive: true, force: true });
    assert.deepEqual(errors, [], 'viewer has no uncaught browser errors');
  });
  for (let attempt = 0; ; attempt++) {
    if (daemon.exitCode !== null || attempt === 100) throw new Error(`Preview did not start: ${log}`);
    try { if ((await fetch(url)).ok) break; } catch { /* wait for listener */ }
    await delay(50);
  }
  async function nextEvent(start, predicate) {
    for (let attempt = 0; attempt < 100; attempt++) {
      const event = events.slice(start).find(predicate);
      if (event) return event;
      await delay(25);
    }
    assert.fail(`Expected WebSocket event; got ${JSON.stringify(events.slice(start))}`);
  }
  async function open() {
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('live'));
    await page.evaluate(() => document.fonts.ready);
  }
  async function cursor(currentSource, needle, name = 'notes.tex') {
    const file = await realpath(join(directory, name));
    const point = position(currentSource, needle);
    // Normal prose places a zero-width source-space anchor at the start of
    // a continued source line. Enter the word to test its own highlight.
    if (!needle.startsWith('\\')) point.col++;
    const start = events.length;
    const response = await fetch(`${url}/cursor`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ file, ...point }),
    });
    assert.equal(response.status, 204);
    const event = await nextEvent(start, value => value.event === 'source-cursor' &&
      value.file === file && value.line === point.line && value.col === point.col);
    assert.ok(event.element_id, `Cursor must resolve ${needle}: ${JSON.stringify(event)}`);
    assert.equal(event.scroll_only, false, `Cursor must highlight ${needle}`);
    return event;
  }
  let lastJump = 0;
  async function jump(click, currentSource, needle, name = 'notes.tex') {
    await click();
    const response = await fetch(`${url}/jump?after=${lastJump}&wait=2000`);
    assert.equal(response.status, 200, 'a real browser click reaches the daemon jump queue');
    const result = await response.json();
    assert.ok(result.seq > lastJump);
    lastJump = result.seq;
    assert.deepEqual({ file: result.file, line: result.line, col: result.col }, {
      file: await realpath(join(directory, name)), ...position(currentSource, needle),
    });
  }
  async function update(nextSource, name = 'notes.tex') {
    const start = events.length;
    const response = await fetch(`${url}/buffer`, {
      method: 'POST', headers: { 'x-mathpreview-path': join(directory, name) }, body: nextSource,
    });
    assert.equal(response.status, 204);
    return nextEvent(start, event => event.event === 'patch');
  }
  return { page, open, cursor, jump, update };
}

function captionWord(page, text) {
  return page.locator('#page figcaption .src-word').filter({ hasText: new RegExp(`^${text}$`) });
}

async function screenshot(page, name) {
  const directory = process.env.MATHPREVIEW_TEST_SCREENSHOT_DIR;
  if (!directory) return;
  await mkdir(directory, { recursive: true });
  await page.screenshot({ path: join(directory, `${browserName}-${name}.png`), animations: 'disabled' });
}

async function assertFlash(page, event, target) {
  assert.equal(event.element_id, await target.getAttribute('id'));
  await page.waitForFunction(id => {
    const el = document.getElementById(id);
    const flash = document.querySelector('#page > .flash-layer .flash-box');
    if (!el?.classList.contains('source-active') || !flash) return false;
    const r = el.getBoundingClientRect();
    const box = flash.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && r.top >= 0 && r.bottom <= innerHeight &&
      box.width > 0 && box.height > 0 && Math.abs(box.top - r.top) < 12 &&
      Math.abs(box.bottom - r.bottom) < 12;
  }, event.element_id);
}

test('images and multiline caption words round-trip through real cursor and jump endpoints', { timeout: 30000 }, async t => {
  const source = tex(figure);
  const { page, open, cursor, jump } = await fixture(t, source);
  await open();
  const asset = page.locator('#page .float-asset');
  await assertFlash(page, await cursor(source, '\\includegraphics'), asset);
  await jump(() => asset.locator('img').click({ modifiers: ['Meta'] }), source, '\\includegraphics');
  for (const word of ['Opening', 'Laterword', 'Finalword']) {
    const target = captionWord(page, word);
    await assertFlash(page, await cursor(source, word), target);
    // Formatted text shares the normal prose command anchor: its displayed
    // word resolves to the beginning of \emph, on the same caption line.
    await jump(() => target.click({ modifiers: ['Meta'] }), source,
      word === 'Finalword' ? '\\emph{Finalword}' : word);
  }
  await screenshot(page, 'figure-caption-sync');
  assert.doesNotMatch(await page.locator('figcaption').textContent(), /Short description/);

  // Hit the actual bare space, near the next word. It must not fall back to
  // the caption command's first line, even though both are in one figure.
  await captionWord(page, 'gapword').scrollIntoViewIfNeeded();
  const point = await page.locator('figcaption').evaluate(el => {
    const words = Array.from(el.querySelectorAll('.src-word'));
    const left = words.find(word => word.textContent === 'Laterword').getBoundingClientRect();
    const right = words.find(word => word.textContent === 'gapword').getBoundingClientRect();
    if (right.left <= left.right || Math.abs(left.top - right.top) > 2) throw new Error('Expected a visible caption gap');
    return { x: left.right + (right.left - left.right) * 0.8, y: (right.top + right.bottom) / 2 };
  });
  await jump(async () => {
    await page.keyboard.down('Meta');
    try { await page.mouse.click(point.x, point.y); } finally { await page.keyboard.up('Meta'); }
  }, source, 'gapword');
});

test('cursor follows cold figures below and above the viewport with a visible flash', { timeout: 30000 }, async t => {
  const distant = Array.from({ length: 120 }, (_, i) => `Distant paragraph ${i}.`).join('\n\n');
  const below = figure.replace('fig:sample', 'fig:below').replace('demo.svg', 'below.svg').replace('Opening', 'Belowword');
  const source = tex(`${figure}\n\n${distant}\n\n${below}`);
  const { page, open, cursor } = await fixture(t, source, { 'below.svg': image });
  await open();
  const bottom = captionWord(page, 'Belowword');
  assert.equal(await bottom.count(), 1, 'offscreen content has a source target before it is laid out');
  const initialScroll = await page.evaluate(() => scrollY);
  await assertFlash(page, await cursor(source, 'Belowword'), bottom);
  const bottomScroll = await page.evaluate(() => scrollY);
  assert.ok(bottomScroll > initialScroll + 900, `Expected downward follow: ${initialScroll} -> ${bottomScroll}`);
  const top = captionWord(page, 'Opening');
  await assertFlash(page, await cursor(source, 'Opening'), top);
  assert.ok(await page.evaluate(() => scrollY) < bottomScroll - 900, 'cursor follows the upper figure back into view');
});

test('source-only line inserts refresh figure metadata without replacing its DOM', { timeout: 30000 }, async t => {
  const source = tex(figure);
  const { page, open, cursor, jump, update } = await fixture(t, source);
  await open();
  const asset = page.locator('#page .float-asset');
  const word = captionWord(page, 'Laterword');
  await asset.evaluate(el => { el.__figureIdentity = true; });
  await word.evaluate(el => { el.__figureIdentity = true; });
  const revised = source.replace('\\begin{figure}', '% First inserted source line.\n% Second inserted source line.\n\\begin{figure}');
  await update(revised);
  assert.equal(await asset.evaluate(el => el.__figureIdentity), true, 'image wrapper is reused');
  assert.equal(await word.evaluate(el => el.__figureIdentity), true, 'caption word is reused');
  await assertFlash(page, await cursor(revised, '\\includegraphics'), asset);
  await jump(() => asset.locator('img').click({ modifiers: ['Meta'] }), revised, '\\includegraphics');
  await assertFlash(page, await cursor(revised, 'Laterword'), word);
  await jump(() => word.click({ modifiers: ['Meta'] }), revised, 'Laterword');
});

test('included figures preserve optional and starred captions through live updates', { timeout: 30000 }, async t => {
  const included = `${figure}\n\n${String.raw`\begin{figure}
\includegraphics{demo.svg}
\caption*{Unnumbered text.
Includedword remains in the included file.}
\end{figure}`}`;
  const { page, open, cursor, jump, update } = await fixture(t, tex(String.raw`\input{figures}`), { 'figures.tex': included });
  await open();
  const starred = page.locator('figcaption').filter({ hasText: 'Unnumbered' });
  assert.equal(await starred.locator('.float-kind').count(), 0);
  assert.doesNotMatch(await page.locator('figcaption').first().textContent(), /Short description/);
  const first = captionWord(page, 'Laterword');
  await assertFlash(page, await cursor(included, 'Laterword', 'figures.tex'), first);
  await jump(() => first.click({ modifiers: ['Meta'] }), included, 'Laterword', 'figures.tex');
  const word = captionWord(page, 'Includedword');
  await assertFlash(page, await cursor(included, 'Includedword', 'figures.tex'), word);
  await jump(() => word.click({ modifiers: ['Meta'] }), included, 'Includedword', 'figures.tex');
  const revised = `% New source line.\n${included.replace('Includedword', 'Revisedword')}`;
  await update(revised, 'figures.tex');
  const changed = captionWord(page, 'Revisedword');
  await changed.waitFor();
  await assertFlash(page, await cursor(revised, 'Revisedword', 'figures.tex'), changed);
  await jump(() => changed.click({ modifiers: ['Meta'] }), revised, 'Revisedword', 'figures.tex');
});

test('a disabled nested TikZ placeholder and its caption use independent source spans', { timeout: 30000 }, async t => {
  const source = tex(String.raw`\begin{figure}
\centering
% The diagram starts after the float, including its leading source comment.
\begin{tikzpicture}
\draw (0,0) circle (1cm);
\end{tikzpicture}
\caption{A diagram.
Diagramword explains the circle.}
\label{fig:diagram}
\end{figure}`);
  const { page, open, cursor, jump } = await fixture(t, source);
  await open();
  const diagram = page.locator('#page .tikz-diagram');
  assert.match(await diagram.textContent(), /TikZ preview disabled/);
  assert.equal(await diagram.locator('img').count(), 0);
  await assertFlash(page, await cursor(source, '\\draw'), diagram);
  await jump(() => diagram.locator('strong').click({ modifiers: ['Meta'] }), source, '\\begin{tikzpicture}');
  const word = captionWord(page, 'Diagramword');
  await assertFlash(page, await cursor(source, 'Diagramword'), word);
  await jump(() => word.click({ modifiers: ['Meta'] }), source, 'Diagramword');
});
