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

// Rebuild embedded assets first. Set MATHPREVIEW_TEST_BROWSER=webkit for Safari's engine.
const binary = resolve(process.env.MATHPREVIEW_CLI || 'target/debug/mathpreview-cli');
const browserType = { chromium, webkit }[process.env.MATHPREVIEW_TEST_BROWSER || 'chromium'];
let browser;
before(async () => { browser = await browserType.launch(); });
after(async () => { await browser?.close(); });

function documentSource(word = 'Original', macro = '\\alpha') {
  const paragraphs = Array.from({ length: 40 }, (_, index) =>
    `Paragraph ${index + 1}. ${word} text about the reading position. ` +
    'This sentence gives the page enough length to test a reader well below its beginning. '.repeat(5) +
    (index % 8 === 0 ? String.raw`The formula is $\wake + x^2$.` : '')
  ).join('\n\n');
  return String.raw`\documentclass{article}
\newcommand{\wake}{${macro}}
\begin{document}
${paragraphs}
\end{document}
`;
}

async function fixture(t, { holdInitially = false } = {}) {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-wake-test-'));
  const input = join(directory, 'paper.tex');
  await writeFile(input, documentSource());
  await writeFile(join(directory, '.mathpreview.toml'), '[viewer]\ntypeset-mode = "local"\n');
  const probe = createServer();
  probe.listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const port = probe.address().port;
  await new Promise(done => probe.close(done));
  const url = `http://127.0.0.1:${port}`;
  let daemon;
  let exited;
  async function start() {
    daemon = spawn(binary, ['serve', input, '--port', String(port), '--editor', ''], {
      env: { ...process.env, XDG_CONFIG_HOME: join(directory, 'config') },
      stdio: ['ignore', 'ignore', 'pipe'],
    });
    exited = once(daemon, 'exit');
    let logs = '';
    daemon.stderr.on('data', chunk => { logs += chunk; });
    for (let attempt = 0; ; attempt++) {
      assert.ok(attempt < 100 && daemon.exitCode === null, `Preview did not start: ${logs}`);
      try { if ((await fetch(`${url}/debug`)).ok) return; } catch { /* waiting for listener */ }
      await delay(50);
    }
  }
  async function stop() {
    if (daemon?.exitCode === null && daemon.signalCode === null) daemon.kill('SIGKILL');
    await exited;
  }
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(10000);
  const errors = [];
  page.on('pageerror', error => errors.push(String(error)));
  await page.addInitScript(hold => {
    const NativeWebSocket = window.WebSocket;
    window.__wakeSockets = [];
    // Consume the initial hold once: a required resync reload must be able to
    // connect normally, just like a new document after the network recovers.
    window.__holdReconnect = hold && sessionStorage.getItem('wake-initial-held') !== '1';
    if (hold) sessionStorage.setItem('wake-initial-held', '1');
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        if (window.__holdReconnect) {
          // A sleeping network rejects connection attempts. Real native sockets
          // are used before suspension and after reconnect is allowed again.
          const failed = { readyState: NativeWebSocket.CONNECTING };
          setTimeout(() => {
            failed.readyState = NativeWebSocket.CLOSED;
            if (failed.onclose) failed.onclose(new Event('close'));
          }, 20);
          return failed;
        }
        const socket = Reflect.construct(target, args);
        socket.addEventListener('message', event => {
          const message = JSON.parse(event.data);
          if (message.event !== 'patch' && message.event !== 'body-updated') return;
          window.__wakeLastRenderEvent = message.event;
          if (!window.__dropNextRender) return;
          window.__dropNextRender = false;
          window.__droppedRevision = message.revision;
          event.stopImmediatePropagation();
        });
        window.__wakeSockets.push(socket);
        return socket;
      },
    });
  }, holdInitially);
  t.after(async () => {
    await context.close();
    await stop();
    await rm(directory, { recursive: true, force: true });
    assert.deepEqual(errors, [], 'viewer must have no uncaught browser errors');
  });
  await start();
  await page.goto(url);
  async function live() {
    await page.waitForFunction(() => document.querySelector('#ws-status')?.classList.contains('live'));
  }
  if (!holdInitially) await live();
  async function disconnect() {
    await page.evaluate(() => {
      window.__holdReconnect = true;
      window.__wakeSockets.at(-1).close();
    });
    await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('disconnected'));
    for (let attempt = 0; ; attempt++) {
      assert.ok(attempt < 100, 'server must observe the disconnected browser');
      if ((await (await fetch(`${url}/debug`)).json()).clients === 0) return;
      await delay(20);
    }
  }
  async function reconnect() {
    await page.evaluate(() => { window.__holdReconnect = false; });
    await live();
  }
  async function update(source) {
    const response = await fetch(`${url}/buffer`, {
      method: 'POST', headers: { 'x-mathpreview-path': input }, body: source,
    });
    assert.equal(response.status, 204);
  }
  return { page, input, start, stop, live, disconnect, reconnect, update };
}

test('unchanged reconnect retains the reading position and rendered equations', { timeout: 40000 }, async t => {
  const { page, disconnect, reconnect, update } = await fixture(t);
  await page.locator('#page .math mjx-container svg').first().waitFor();
  const reading = page.locator('#page p').filter({ hasText: 'Paragraph 25.' });
  await reading.evaluate(node => {
    node.scrollIntoView({ block: 'center', behavior: 'instant' });
    window.__wakeReading = node;
    window.__wakeMath = document.querySelector('#page .math mjx-container');
    window.__wakeDocument = document;
  });
  await reading.locator('mjx-container svg').first().waitFor();
  await delay(250);
  for (const round of ['initial document', 'after a live patch', 'after a full body update']) {
    const before = await reading.evaluate(node => ({ y: scrollY, top: node.getBoundingClientRect().top }));
    await disconnect();
    await reconnect();
    await delay(300);
    const after = await page.evaluate(() => ({
      sameDocument: window.__wakeDocument === document,
      sameReading: window.__wakeReading?.isConnected === true,
      sameMath: window.__wakeMath?.isConnected === true,
      y: scrollY,
      top: window.__wakeReading?.getBoundingClientRect().top,
    }));
    assert.ok(after.sameDocument, `${round}: reconnect must not reload an unchanged daemon`);
    assert.ok(after.sameReading && after.sameMath, `${round}: already rendered DOM must survive`);
    assert.ok(Math.abs(after.y - before.y) <= 2, `${round}: scroll moved ${after.y - before.y}px`);
    assert.ok(Math.abs(after.top - before.top) <= 2, `${round}: reading anchor moved`);
    if (round === 'initial document') {
      // An unrelated late paragraph changes the committed revision. A later
      // reconnect must use the revision the client actually finished applying.
      await update(documentSource().replace('Paragraph 40.', 'Paragraph 40. Updated tail.'));
      await page.locator('#page p').filter({ hasText: 'Updated tail.' }).waitFor();
      assert.equal(await page.evaluate(() => window.__wakeLastRenderEvent), 'patch');
    } else if (round === 'after a live patch') {
      // Replacing every paragraph takes the full-body path, whose revision
      // bookkeeping must be just as safe to resume as incremental patches.
      await update(documentSource('Rewritten'));
      await page.waitForFunction(() => document.querySelector('#page')?.textContent.includes('Rewritten text'));
      assert.equal(await page.evaluate(() => window.__wakeLastRenderEvent), 'body-updated');
      assert.ok((await reading.textContent()).includes('Rewritten text'));
      await reading.evaluate(node => {
        node.scrollIntoView({ block: 'center', behavior: 'instant' });
        window.__wakeReading = node;
        window.__wakeMath = node.querySelector('mjx-container');
      });
      await reading.locator('mjx-container svg').first().waitFor();
      await delay(250);
    }
  }
});

test('reconnect resynchronizes edits missed while the socket was disconnected', { timeout: 30000 }, async t => {
  const { page, disconnect, reconnect, update } = await fixture(t);
  await page.evaluate(() => { window.__wakeDocument = document; });
  await disconnect();
  await update(documentSource('Changed while asleep'));
  assert.doesNotMatch(await page.locator('#page').innerText(), /Changed while asleep/);
  const navigation = page.waitForEvent('domcontentloaded');
  await reconnect();
  await navigation;
  await page.waitForFunction(() => document.querySelector('#ws-status')?.classList.contains('live'));
  assert.match(await page.locator('#page').innerText(), /Changed while asleep/);
  assert.equal(await page.evaluate(() => window.__wakeDocument === document), false,
    'a stale document must be replaced instead of applying deltas against the wrong base');
});

test('a real daemon restart reloads the page-level macro configuration', { timeout: 30000 }, async t => {
  const { page, input, start, stop, live, disconnect, reconnect } = await fixture(t);
  await page.locator('#page .math mjx-container svg').first().waitFor();
  assert.match(await page.evaluate(() => JSON.stringify(window.MathJax.config.tex.macros.wake)), /alpha/);
  const before = await page.locator('#page .math mjx-container').first().innerHTML();
  await page.evaluate(() => { window.__wakeDocument = document; });
  await disconnect();
  await stop();
  await writeFile(input, documentSource('Restarted', '\\beta'));
  await start();
  const navigation = page.waitForEvent('domcontentloaded');
  await reconnect();
  await navigation;
  await live();
  await page.locator('#page .math mjx-container svg').first().waitFor();
  assert.match(await page.evaluate(() => JSON.stringify(window.MathJax.config.tex.macros.wake)), /beta/);
  assert.equal(await page.evaluate(() => window.__wakeDocument === document), false);
  assert.match(await page.locator('#page').innerText(), /Restarted/);
  assert.notEqual(await page.locator('#page .math mjx-container').first().innerHTML(), before,
    'the restarted daemon must supply the new MathJax macro definition');
});

test('an edit between the page response and its first socket cannot leave stale content', { timeout: 30000 }, async t => {
  const { page, reconnect, update } = await fixture(t, { holdInitially: true });
  await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('disconnected'));
  assert.match(await page.locator('#page').innerText(), /Original text/);
  await update(documentSource('Changed before first connection'));
  const navigation = page.waitForEvent('domcontentloaded');
  await reconnect();
  await navigation;
  await page.waitForFunction(() => document.querySelector('#ws-status')?.classList.contains('live'));
  assert.match(await page.locator('#page').innerText(), /Changed before first connection/);
});

test('a patch with a missing predecessor triggers resync instead of corrupting the DOM', { timeout: 30000 }, async t => {
  const { page, live, update } = await fixture(t);
  await page.evaluate(() => { window.__dropNextRender = true; });
  const first = documentSource().replace('Paragraph 39.', 'Paragraph 39. First offline change.');
  await update(first);
  await page.waitForFunction(() => window.__dropNextRender === false);
  assert.doesNotMatch(await page.locator('#page').textContent(), /First offline change/);
  const navigation = page.waitForEvent('domcontentloaded');
  await update(first.replace('Paragraph 40.', 'Paragraph 40. Second offline change.'));
  await navigation;
  await live();
  const text = await page.locator('#page').textContent();
  assert.match(text, /First offline change/);
  assert.match(text, /Second offline change/);
});
