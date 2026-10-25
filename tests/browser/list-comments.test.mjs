import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { after, before, test } from 'node:test';
import { chromium, webkit } from 'playwright';

// Rebuild mathpreview-cli before running: it embeds the parser and viewer.
const binary = resolve('target/debug/mathpreview-cli');
const browserName = process.env.MATHPREVIEW_TEST_BROWSER || 'chromium';
const browserType = { chromium, webkit }[browserName];
assert.ok(browserType, `Unsupported test browser: ${browserName}`);
let browser;
before(async () => { browser = await browserType.launch(); });
after(async () => { await browser?.close(); });

function tex(body) {
  return String.raw`\documentclass{article}
\begin{document}
${body}
\end{document}
`;
}

function position(source, needle) {
  const offset = source.indexOf(needle);
  assert.ok(offset >= 0, `Missing source text: ${needle}`);
  const before = source.slice(0, offset);
  return {
    line: before.split('\n').length,
    col: Buffer.byteLength(before.slice(before.lastIndexOf('\n') + 1)) + 1,
  };
}

async function fixture(t, source) {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-list-comments-test-'));
  const input = join(directory, 'notes.tex');
  await writeFile(input, source);
  await writeFile(join(directory, '.mathpreview.toml'), '[viewer]\nrender-tikz = false\n');
  const portProbe = createServer();
  portProbe.listen(0, '127.0.0.1');
  await once(portProbe, 'listening');
  const port = portProbe.address().port;
  await new Promise(resolveClose => portProbe.close(resolveClose));
  const url = `http://127.0.0.1:${port}`;
  // Source navigation must not launch the user's editor.
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
  async function update(nextSource) {
    const start = events.length;
    const response = await fetch(`${url}/buffer`, {
      method: 'POST', headers: { 'x-mathpreview-path': input }, body: nextSource,
    });
    assert.equal(response.status, 204);
    // A one-block document legitimately uses the full-body update path.
    await nextEvent(start, event => event.event === 'patch' || event.event === 'body-updated');
  }
  let lastJump = 0;
  async function assertSource(word, currentSource) {
    const target = page.locator('#page .src-word').filter({ hasText: new RegExp(`^${word}$`) });
    const file = await realpath(input);
    const point = position(currentSource, word);
    assert.equal(await target.getAttribute('data-src'), `${file}:${point.line}:${point.col}`);
    const start = events.length;
    const response = await fetch(`${url}/cursor`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ file, line: point.line, col: point.col + 1 }),
    });
    assert.equal(response.status, 204);
    const event = await nextEvent(start, value => value.event === 'source-cursor' &&
      value.file === file && value.line === point.line && value.col === point.col + 1);
    assert.equal(event.element_id, await target.getAttribute('id'));
    await target.click({ modifiers: ['Meta'] });
    const jump = await fetch(`${url}/jump?after=${lastJump}&wait=2000`);
    assert.equal(jump.status, 200);
    const result = await jump.json();
    assert.ok(result.seq > lastJump);
    lastJump = result.seq;
    assert.deepEqual({ file: result.file, line: result.line, col: result.col }, { file, ...point });
  }
  return { page, open, update, assertSource };
}

test('commented enumerate items stay hidden through live comment toggles', { timeout: 30000 }, async t => {
  const source = tex(String.raw`\begin{enumerate}
  \item first
  % \item Hiddenitem should be commented
\end{enumerate}`);
  const { page, open, update } = await fixture(t, source);
  await open();
  const items = page.locator('#page ol.enumerate > li');
  assert.equal(await items.count(), 1);
  assert.match(await items.first().textContent(), /first/);
  assert.doesNotMatch(await page.locator('#page').textContent(), /Hiddenitem/);

  await update(source.replace('% \\item Hiddenitem', '\\item Hiddenitem'));
  await page.waitForFunction(() => document.querySelectorAll('#page ol.enumerate > li').length === 2);
  assert.match(await items.nth(1).textContent(), /Hiddenitem/);
  await update(source);
  await page.waitForFunction(() => document.querySelectorAll('#page ol.enumerate > li').length === 1);
  assert.doesNotMatch(await page.locator('#page').textContent(), /Hiddenitem/);
});

test('commented environment delimiters do not alter nested lists or escaped percent text', { timeout: 30000 }, async t => {
  const source = tex(String.raw`\begin{enumerate}
\item Outerfirst 50\% retained.
% \begin{itemize} \item Hiddenouter
\begin{itemize}
\item Innerfirst
% \end{itemize} \item Hiddeninner
\item Innersecond
\end{itemize}
% \end{enumerate} \begin{description}
\item Outersecond
\end{enumerate}`);
  const { page, open } = await fixture(t, source);
  await open();
  const outer = page.locator('#page ol.enumerate > li');
  assert.equal(await outer.count(), 2);
  assert.equal(await outer.first().locator('ul.itemize > li').count(), 2);
  assert.match(await outer.first().textContent(), /50% retained/);
  assert.match(await outer.nth(1).textContent(), /Outersecond/);
  assert.doesNotMatch(await page.locator('#page').textContent(), /Hiddenouter|Hiddeninner/);
});

test('source navigation stays on the real line after Unicode comments and live edits', { timeout: 30000 }, async t => {
  const source = tex(String.raw`\begin{enumerate}
\item Firstword
% café λ 日本語 \item Hiddenword
\item Secondword follows.
\end{enumerate}`);
  const { page, open, update, assertSource } = await fixture(t, source);
  await open();
  assert.equal(await page.locator('#page ol.enumerate > li').count(), 2);
  await assertSource('Secondword', source);
  const revised = source.replace('% café', '% added 日本語 comment\n% café').replace('Secondword', 'Revisedword');
  await update(revised);
  await page.waitForFunction(() => document.querySelector('#page')?.textContent.includes('Revisedword'));
  assert.equal(await page.locator('#page ol.enumerate > li').count(), 2);
  await assertSource('Revisedword', revised);
});
