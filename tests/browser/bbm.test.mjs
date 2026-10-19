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

// Rebuild the embedded assets with `cargo build -p mathpreview-cli` first.
// Run with MATHPREVIEW_TEST_BROWSER=webkit to cover the second browser engine.
const binary = resolve('target/debug/mathpreview-cli');
const browserName = process.env.MATHPREVIEW_TEST_BROWSER || 'chromium';
const browserType = { chromium, webkit }[browserName];
assert.ok(browserType, `Unsupported test browser: ${browserName}`);
let browser;
before(async () => { browser = await browserType.launch(); });
after(async () => { await browser?.close(); });

const bbmMath = String.raw`$\mathbbm{1}$, $\mathbbmss{1}$, $\mathbbmtt{1}$,
$\mathversion{bold}\mathbbm{1}\mathversion{normal}$, and
$\mathversion{bold}\mathbbmss{1}\mathversion{normal}$.`;
const bbmVariants = [['MB-BN'], ['MB-BSS'], ['MB-BM'], ['MB-BB'], ['MB-BSSB']];
function documentSource(preamble, body = bbmMath) {
  return String.raw`\documentclass{article}
${preamble}
\begin{document}
${body}
\end{document}
`;
}

async function fixture(t, source, files = {}) {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-bbm-test-'));
  const input = join(directory, 'notes.tex');
  await writeFile(input, source);
  for (const [name, body] of Object.entries(files)) await writeFile(join(directory, name), body);
  const probe = createServer();
  probe.listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const port = probe.address().port;
  await new Promise(resolveClose => probe.close(resolveClose));
  const url = `http://127.0.0.1:${port}`;
  const daemon = spawn(binary, ['serve', input, '--port', String(port), '--editor', ''], {
    env: { ...process.env, XDG_CONFIG_HOME: join(directory, 'config') },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  let log = '';
  daemon.stderr.on('data', chunk => { log += chunk; });
  const exited = once(daemon, 'exit');
  const context = await browser.newContext();
  const remoteRequests = [];
  const failedAssets = [];
  const loadedAssets = [];
  const errors = [];
  const events = [];
  // An accidentally inherited CDN font URL must fail, even on online CI hosts.
  await context.route('**/*', async route => {
    if (new URL(route.request().url()).origin !== url) {
      remoteRequests.push(route.request().url());
      await route.abort();
    } else {
      await route.continue();
    }
  });
  context.on('response', response => {
    if (!response.url().includes('/vendor/')) return;
    if (response.status() >= 400) failedAssets.push(`${response.status()} ${response.url()}`);
    else loadedAssets.push(new URL(response.url()).pathname);
  });
  const page = await context.newPage();
  page.setDefaultTimeout(10000);
  page.on('pageerror', error => errors.push(String(error)));
  page.on('websocket', socket => socket.on('framereceived', ({ payload }) => {
    try { events.push(JSON.parse(String(payload))); } catch { /* non-JSON frame */ }
  }));
  t.after(async () => {
    await context.close();
    daemon.kill('SIGTERM');
    await exited;
    await rm(directory, { recursive: true, force: true });
    assert.deepEqual(remoteRequests, [], 'MathJax and bbm fonts are completely local');
    assert.deepEqual(failedAssets, [], 'all requested vendored assets exist');
    assert.deepEqual(errors, [], 'viewer has no uncaught browser errors');
  });
  for (let attempt = 0; ; attempt++) {
    if (daemon.exitCode !== null || attempt === 100) throw new Error(`Preview did not start: ${log}`);
    try {
      if ((await fetch(url)).ok) break;
    } catch { /* wait for listener */ }
    await delay(50);
  }
  async function open() {
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('live'));
  }
  async function updateWithReload(nextSource) {
    events.length = 0;
    const navigation = page.waitForEvent('domcontentloaded');
    const response = await fetch(`${url}/buffer`, {
      method: 'POST',
      headers: { 'x-mathpreview-path': input },
      body: nextSource,
    });
    assert.equal(response.status, 204);
    await navigation;
    // Package changes travel in viewer_config and make the client reload its
    // head, rather than using the server's macro-change full-reload event.
    assert.ok(events.some(message => message.viewer_config?.mathjax_packages?.includes('[tex]/bbm')),
      `Expected a package-list update before navigation, got ${JSON.stringify(events)}`);
  }
  return { page, open, updateWithReload, loadedAssets };
}

async function assertBbm(page, loadedAssets) {
  await page.waitForFunction(count => document.querySelectorAll('#page .math mjx-container svg').length === count,
    bbmVariants.length);
  const output = await page.locator('#page .math').evaluateAll(nodes => nodes.map(node => ({
    errors: node.querySelectorAll('[data-mml-node="merror"], mjx-merror').length,
    text: node.textContent,
    glyphs: Array.from(node.querySelectorAll('svg use'), use => {
      const href = use.getAttribute('xlink:href') || use.getAttribute('href');
      const path = href?.startsWith('#') && document.getElementById(href.slice(1));
      return path?.getAttribute('d') ? href : null;
    }),
  })));
  assert.equal(output.length, bbmVariants.length);
  for (const [index, formula] of output.entries()) {
    assert.equal(formula.errors, 0, JSON.stringify(formula));
    assert.doesNotMatch(formula.text, /\\mathbbm|Undefined control sequence|Math input error/);
    for (const variant of bbmVariants[index]) {
      assert.ok(formula.glyphs.some(glyph => glyph?.includes(`-${variant}-`)),
        `Expected an actual ${variant} font glyph, got ${JSON.stringify(formula)}`);
    }
  }
  assert.ok(loadedAssets.includes('/vendor/mathjax/input/tex/extensions/bbm.js'),
    'the explicitly requested TeX extension loads locally');
  assert.ok(loadedAssets.some(path => path.startsWith('/vendor/mathjax/mathjax-bbm-font-extension/')),
    'the bbm font data loads locally');
}

test('usepackage{bbm} renders the bbm alphabets and bold variants without network access', { timeout: 30000 }, async t => {
  const { page, open, loadedAssets } = await fixture(t, documentSource(String.raw`\usepackage{bbm}`));
  await open();
  await assertBbm(page, loadedAssets);
});

test('RequirePackage{bbm} is chased through nested preamble inputs', { timeout: 30000 }, async t => {
  const { page, open, loadedAssets } = await fixture(t, documentSource(String.raw`\input{preamble}`), {
    'preamble.tex': String.raw`\input{fonts}`,
    'fonts.tex': String.raw`\RequirePackage{bbm}`,
  });
  await open();
  await assertBbm(page, loadedAssets);
});

test('adding bbm in an unsaved buffer reloads its TeX extension and local fonts', { timeout: 30000 }, async t => {
  const { page, open, updateWithReload, loadedAssets } = await fixture(t, documentSource('', '$x$'));
  await open();
  await page.waitForSelector('#page .math mjx-container svg');
  assert.ok(!loadedAssets.some(path => /bbm/.test(path)), 'ordinary math does not preload bbm');
  await updateWithReload(documentSource(String.raw`\usepackage{bbm}`));
  await assertBbm(page, loadedAssets);
});
