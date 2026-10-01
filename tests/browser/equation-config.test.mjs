import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { after, before, test } from 'node:test';
import { chromium, webkit } from 'playwright';

// Rebuild the CLI before this suite: its HTML and client scripts are embedded.
const binary = resolve('target/debug/mathpreview-cli');
const browserName = process.env.MATHPREVIEW_TEST_BROWSER || 'chromium';
const browserType = { chromium, webkit }[browserName];
assert.ok(browserType, `Unsupported test browser: ${browserName}`);
let browser;
before(async () => { browser = await browserType.launch(); });
after(async () => { await browser?.close(); });

const documentSource = String.raw`\documentclass{article}
\usepackage{amsmath}
\newtheorem{theorem}{Theorem}[section]
\begin{document}
\section{First}
\begin{theorem}\label{thm:first}First theorem.\end{theorem}
\begin{equation}\label{eq:first}a=b\end{equation}
\begin{align}
c&=d\label{eq:row-one}\\
e&=f\label{eq:row-two}
\end{align}
\begin{subequations}\label{eq:group}
\begin{equation}\label{eq:child-one}g=h\end{equation}
\begin{equation}\label{eq:child-two}i=j\end{equation}
\end{subequations}
\section{Second}
\begin{theorem}\label{thm:second}Second theorem.\end{theorem}
\begin{equation}\label{eq:second}k=l\end{equation}
\begin{equation}\label{eq:manual}m=n\tag{KEEP}\end{equation}
References: \eqref{eq:first}, \eqref{eq:row-one}, \eqref{eq:row-two},
\eqref{eq:group}, \eqref{eq:child-one}, \eqref{eq:child-two},
\eqref{eq:second}, and \eqref{eq:manual}.
\end{document}
`;

const sectionNumbers = ['(1.1)', '(1.2)', '(1.3)', '(1.4a)', '(1.4b)', '(2.1)'];
const continuousNumbers = ['(1)', '(2)', '(3)', '(4a)', '(4b)', '(5)'];
const sectionRefs = ['(1.1)', '(1.2)', '(1.3)', '(1.4)', '(1.4a)', '(1.4b)', '(2.1)', '(KEEP)'];
const continuousRefs = ['(1)', '(2)', '(3)', '(4)', '(4a)', '(4b)', '(5)', '(KEEP)'];

async function fixture(t, globalConfig = '') {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-equation-config-'));
  const input = join(directory, 'notes.tex');
  const configDirectory = join(directory, 'config');
  const globalPath = join(configDirectory, 'mathpreview', 'config.toml');
  const projectPath = join(directory, '.mathpreview.toml');
  await writeFile(input, documentSource);
  await mkdir(join(configDirectory, 'mathpreview'), { recursive: true });
  if (globalConfig) await writeFile(globalPath, globalConfig);

  const portProbe = createServer();
  portProbe.listen(0, '127.0.0.1');
  await once(portProbe, 'listening');
  const port = portProbe.address().port;
  await new Promise(resolveClose => portProbe.close(resolveClose));
  const url = `http://127.0.0.1:${port}`;
  const daemon = spawn(binary, ['serve', input, '--port', String(port)], {
    env: { ...process.env, XDG_CONFIG_HOME: configDirectory },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  let log = '';
  daemon.stderr.on('data', chunk => { log += chunk; });
  const exited = once(daemon, 'exit');
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  const page = await context.newPage();
  page.setDefaultTimeout(10000);
  const errors = [];
  const writes = [];
  page.on('pageerror', error => errors.push(String(error)));
  page.on('request', request => {
    if (request.url() === `${url}/config/write`) writes.push(request.postDataJSON());
  });
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
    await page.waitForSelector('#eq-first svg');
  }
  async function openConfig() {
    await page.locator('#config-toggle').click();
    await page.waitForFunction(() => {
      const editor = document.querySelector('#config-viewer-toml');
      return editor && !editor.readOnly && editor.dataset.loadedScope === 'project:';
    });
  }
  async function save() {
    const response = page.waitForResponse(`${url}/config/write`);
    await page.locator('#config-dialog-save').click();
    assert.equal((await response).status(), 200);
    await page.waitForSelector('#config-dialog[open]', { state: 'detached' });
  }
  async function assertNumbering(mode) {
    const numbers = mode === 'continuous' ? continuousNumbers : sectionNumbers;
    const refs = mode === 'continuous' ? continuousRefs : sectionRefs;
    try {
      await page.waitForFunction(expected => {
        const actual = Array.from(document.querySelectorAll('#page .eq-num, #page .eq-num-row'))
          .map(el => el.textContent);
        return JSON.stringify(actual) === JSON.stringify(expected);
      }, numbers);
    } catch (error) {
      assert.deepEqual(await page.locator('#page .eq-num, #page .eq-num-row').allTextContents(), numbers);
      throw error;
    }
    assert.deepEqual(await page.locator('#page a.ref[data-kind="eqref"]').allTextContents(), refs);
    assert.match(await page.locator('#eq-manual').getAttribute('data-mathjax-tex'), /\\tag\{KEEP\}/,
      'explicit tags remain owned by MathJax');
    assert.deepEqual(await page.locator('#page .thm-num').allTextContents(), ['1.1', '2.1'],
      'equation numbering does not change theorem numbering');
    assert.equal(await page.evaluate(() => window.__mpConfig.equationNumbering), mode);
    const debug = await (await fetch(`${url}/debug`)).json();
    assert.equal(debug.viewer_config.equation_numbering, mode);
  }
  return { page, open, openConfig, save, assertNumbering, writes, projectPath, globalPath };
}

test('equation numbering dropdown updates equations, rows, groups, and refs live and persists', { timeout: 30000 }, async t => {
  const { page, open, openConfig, save, assertNumbering, writes, projectPath } = await fixture(t);
  await open();
  await assertNumbering('section');
  await openConfig();
  assert.equal(await page.locator('#config-equation-numbering').inputValue(), 'section');
  await page.locator('#config-equation-numbering').selectOption('continuous');
  await save();
  assert.equal(writes.at(-1).values['viewer.equation-numbering'], 'continuous');
  await assertNumbering('continuous');
  assert.match(await readFile(projectPath, 'utf8'), /equation-numbering\s*=\s*"continuous"/);

  // Reload from the persisted config, rather than relying on the broadcast.
  await open();
  await assertNumbering('continuous');
  await openConfig();
  assert.equal(await page.locator('#config-equation-numbering').inputValue(), 'continuous');
  await page.locator('#config-font-size').fill('20');
  await save();
  assert.equal(Object.hasOwn(writes.at(-1).values, 'viewer.equation-numbering'), false);
  assert.match(await readFile(projectPath, 'utf8'), /equation-numbering\s*=\s*"continuous"/);
  await assertNumbering('continuous');
  await openConfig();
  await page.locator('#config-equation-numbering').selectOption('section');
  await save();
  await assertNumbering('section');
  assert.match(await readFile(projectPath, 'utf8'), /equation-numbering\s*=\s*"section"/);
});

test('an untouched equation dropdown preserves an advanced TOML edit', { timeout: 30000 }, async t => {
  const { page, open, openConfig, save, assertNumbering, writes, projectPath } = await fixture(t);
  await open();
  await openConfig();
  await page.locator('#config-viewer-toml').fill('[viewer]\nequation-numbering = "continuous"\n');
  // The unchanged control still shows the old effective value, not the draft.
  assert.equal(await page.locator('#config-equation-numbering').inputValue(), 'section');
  await save();
  assert.equal(Object.hasOwn(writes.at(-1).values, 'viewer.equation-numbering'), false);
  await assertNumbering('continuous');
  assert.match(await readFile(projectPath, 'utf8'), /equation-numbering\s*=\s*"continuous"/);
});

test('saving another option does not copy inherited equation numbering into a project', { timeout: 30000 }, async t => {
  const { page, open, openConfig, save, assertNumbering, writes, projectPath, globalPath } = await fixture(t,
    '[viewer]\nequation-numbering = "continuous"\n');
  await open();
  await assertNumbering('continuous');
  await openConfig();
  assert.equal(await page.locator('#config-equation-numbering').inputValue(), 'continuous');
  await page.locator('#config-font-size').fill('19');
  await save();
  assert.equal(Object.hasOwn(writes.at(-1).values, 'viewer.equation-numbering'), false);
  assert.doesNotMatch(await readFile(projectPath, 'utf8'), /^\s*equation-numbering\s*=/m);
  await assertNumbering('continuous');

  // Prove inheritance still works after the unrelated project save.
  await writeFile(globalPath, '[viewer]\nequation-numbering = "section"\n');
  await assertNumbering('section');
  await openConfig();
  assert.equal(await page.locator('#config-equation-numbering').inputValue(), 'section');
});
