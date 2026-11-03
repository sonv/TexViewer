import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium, webkit } from 'playwright';

const binary = resolve('target/debug/mathpreview-cli');
const browserType = { chromium, webkit }[process.env.MATHPREVIEW_TEST_BROWSER || 'chromium'];
const source = String.raw`\documentclass[preprint]{revtex4-2}
\begin{document}
\title{Experiments on $N$ sites}
\newcommand{\cofirstnote}{These authors shared \textbf{responsibility}.}
\author{A. Bell}
\thanks{\cofirstnote}
\affiliation{First Institute}
\author{B. Chang}
\thanks{\cofirstnote}
\affiliation{Second Institute}
\author{C. Davis}
\email{davis@example.test}
\affiliation{Third Institute}
\date{November 3}
\begin{abstract}
An abstract about the experiment.
\end{abstract}
\maketitle
\section{Results}
The body follows the abstract.
\end{document}`;

test('REVTeX body declarations form one title block and follow live edits', { timeout: 30000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-revtex-test-'));
  const input = join(directory, 'paper.tex');
  t.after(() => rm(directory, { recursive: true, force: true }));
  await writeFile(input, source);
  const probe = createServer();
  probe.listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const port = probe.address().port;
  await new Promise(done => probe.close(done));
  const url = `http://127.0.0.1:${port}`;
  const daemon = spawn(binary, ['serve', input, '--port', String(port), '--editor', ''], {
    env: { ...process.env, XDG_CONFIG_HOME: join(directory, 'config') },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  const exited = once(daemon, 'close');
  let logs = '';
  daemon.stderr.on('data', chunk => { logs += chunk; });
  t.after(async () => { daemon.kill('SIGTERM'); await exited; });
  for (let attempt = 0; ; attempt++) {
    assert.ok(attempt < 100 && daemon.exitCode === null, `Preview did not start: ${logs}`);
    try { if ((await fetch(url)).ok) break; } catch { /* waiting for listener */ }
    await delay(50);
  }

  const browser = await browserType.launch();
  t.after(() => browser.close());
  const page = await browser.newPage();
  page.setDefaultTimeout(10000);
  await page.goto(url);
  await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('live'));
  const body = page.locator('#page');
  const titleBlock = body.locator('.title-block');
  assert.equal(await titleBlock.count(), 1, 'REVTeX title declarations must render at maketitle');
  assert.match(await titleBlock.locator('.paper-title').innerText(), /Experiments on/);
  await titleBlock.locator('.paper-title mjx-container svg').waitFor();
  const authors = titleBlock.locator('.paper-author');
  assert.equal(await authors.count(), 3);
  const names = ['A. Bell', 'B. Chang', 'C. Davis'];
  const institutions = ['First Institute', 'Second Institute', 'Third Institute'];
  for (const [index, name] of names.entries()) {
    assert.ok((await authors.nth(index).innerText()).includes(name));
    assert.equal((await authors.nth(index).locator('.paper-address').innerText()).trim(), institutions[index]);
  }
  const firstMark = await authors.nth(0).locator('sup').innerText();
  assert.ok(firstMark.trim(), 'first author has a contribution note marker');
  assert.equal(await authors.nth(1).locator('sup').innerText(), firstMark);
  assert.equal(await authors.nth(2).locator('a[href="mailto:davis@example.test"]').count(), 1);
  const bodyText = await body.innerText();
  for (const text of ['Experiments on', ...names, ...institutions, 'These authors shared responsibility.']) {
    assert.equal(bodyText.split(text).length - 1, 1, `${text} should appear once in the paper`);
  }
  assert.equal(await titleBlock.locator('.paper-date').innerText(), 'November 3');
  assert.ok(bodyText.indexOf('Experiments on') < bodyText.indexOf('An abstract about the experiment.'));
  assert.ok(bodyText.indexOf('An abstract about the experiment.') < bodyText.indexOf('The body follows the abstract.'));

  let jumpSeq = 0;
  const canonical = await realpath(input);
  async function checkJump(element, line) {
    await element.click({ modifiers: ['Meta'] });
    const response = await fetch(`${url}/jump?after=${jumpSeq}&wait=2000`);
    assert.equal(response.status, 200, 'click queues a source jump');
    const jump = await response.json();
    jumpSeq = jump.seq;
    assert.deepEqual([jump.file, jump.line, jump.col], [canonical, line, 1]);
  }
  // Both authors share the displayed note, but each marker belongs to its
  // own thanks declaration. All jumps use the viewer's actual click handler.
  await checkJump(titleBlock.locator('.paper-title'), 3);
  await checkJump(authors.nth(0).locator('.paper-author-name'), 5);
  await checkJump(authors.nth(1).locator('.paper-address'), 10);
  await checkJump(authors.nth(1).locator('sup'), 9);

  const edit = source.replace('\\title{Experiments on', '\n\\title{Measurements on')
    .replace('responsibility', 'credit');
  const response = await fetch(`${url}/buffer`, {
    method: 'POST', headers: { 'x-mathpreview-path': input }, body: edit,
  });
  assert.equal(response.status, 204);
  await titleBlock.locator('.paper-title').filter({ hasText: 'Measurements on' }).waitFor();
  const updated = await body.innerText();
  assert.equal(updated.split('These authors shared credit.').length - 1, 1);
  assert.doesNotMatch(updated, /Experiments on|responsibility/);
  assert.match(updated, /An abstract about the experiment\./);
  assert.match(updated, /The body follows the abstract\./);
  await checkJump(authors.nth(1).locator('sup'), 10);
});
