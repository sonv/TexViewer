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

// `npm run test:paragraph` rebuilds the embedded renderer and viewer first.
const binary = resolve('target/debug/mathpreview-cli');
const browserName = process.env.MATHPREVIEW_TEST_BROWSER || 'chromium';
const browserType = { chromium, webkit }[browserName];
assert.ok(browserType, `Unsupported test browser: ${browserName}`);
let browser;
before(async () => { browser = await browserType.launch(); });
after(async () => { await browser?.close(); });

const padding = Array.from({ length: 30 }, (_, i) => `Stable paragraph ${i}.`).join('\n\n');
function tex(body, preamble = '') {
  return String.raw`\documentclass{article}
\newtheorem{theorem}{Theorem}
${preamble}
\begin{document}
${body}

${padding}
\end{document}
`;
}

async function fixture(t, source, filename = 'notes.tex') {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-paragraph-test-'));
  const input = join(directory, filename);
  await writeFile(input, source);
  const portProbe = createServer();
  portProbe.listen(0, '127.0.0.1');
  await once(portProbe, 'listening');
  const port = portProbe.address().port;
  await new Promise(resolveClose => portProbe.close(resolveClose));
  const url = `http://127.0.0.1:${port}`;
  const daemon = spawn(binary, ['serve', input, '--port', String(port)], {
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
    if (daemon.exitCode !== null || attempt === 100) throw new Error(`Preview did not start: ${log}`);
    try { if ((await fetch(url)).ok) break; } catch { /* wait for listener */ }
    await delay(50);
  }
  async function open() {
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('live'));
    await page.evaluate(() => document.fonts.ready);
  }
  async function update(nextSource) {
    events.length = 0;
    const response = await fetch(`${url}/buffer`, {
      method: 'POST', headers: { 'x-mathpreview-path': input }, body: nextSource,
    });
    assert.equal(response.status, 204);
    for (let attempt = 0; !events.includes('patch'); attempt++) {
      assert.ok(attempt < 100, `Expected a patch, got ${events.join(', ')}`);
      await delay(50);
    }
  }
  return { page, open, update, input };
}

async function runInGeometry(page, title, firstWord) {
  const heading = page.locator('#page .run-in-heading').filter({ hasText: title });
  await heading.scrollIntoViewIfNeeded();
  return heading.evaluate((el, word) => {
    const titleRange = document.createRange();
    titleRange.selectNodeContents(el);
    const titleRect = Array.from(titleRange.getClientRects()).at(-1);
    const walker = document.createTreeWalker(document.querySelector('#page'), NodeFilter.SHOW_TEXT);
    let node;
    while ((node = walker.nextNode())) {
      const at = node.textContent.indexOf(word);
      if (at < 0 || el.contains(node)) continue;
      const range = document.createRange();
      range.setStart(node, at);
      range.setEnd(node, at + word.length);
      const bodyRect = range.getBoundingClientRect();
      const container = el.closest('p, .proof-para, li');
      return { title: { left: titleRect.left, right: titleRect.right, top: titleRect.top, bottom: titleRect.bottom },
        body: { left: bodyRect.left, top: bodyRect.top, bottom: bodyRect.bottom },
        sameContainer: !!container && container.contains(node),
        display: getComputedStyle(el).display, role: el.getAttribute('role'), level: el.getAttribute('aria-level') };
    }
    throw new Error(`Missing body word: ${word}`);
  }, firstWord);
}

async function assertRunIn(page, title, firstWord, level = '5') {
  const geometry = await runInGeometry(page, title, firstWord);
  assert.equal(geometry.role, 'heading');
  assert.equal(geometry.level, level);
  assert.equal(geometry.display, 'inline');
  assert.ok(geometry.sameContainer, JSON.stringify(geometry));
  assert.ok(Math.abs(geometry.title.bottom - geometry.body.bottom) <= 4, JSON.stringify(geometry));
  assert.ok(geometry.body.left > geometry.title.right, JSON.stringify(geometry));
  return geometry;
}

async function screenshot(page, name) {
  const directory = process.env.MATHPREVIEW_TEST_SCREENSHOT_DIR;
  if (!directory) return;
  await mkdir(directory, { recursive: true });
  await page.screenshot({ path: join(directory, `${browserName}-${name}.png`), animations: 'disabled' });
}

test('paragraph titles run into their body without default section numbers', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, tex(String.raw`\section{Introduction}
SectionBody is a normal section paragraph.

\paragraph{Plan of the paper.} Opening explains the plan.

Following is a separate paragraph.`));
  await open();
  await assertRunIn(page, 'Plan of the paper.', 'Opening');
  assert.equal(await page.locator('#page .run-in-heading .sec-num').count(), 0);
  assert.equal(await page.locator('#page p.para-run-in').count(), 1);
  assert.equal(await page.locator('#page h2').filter({ hasText: 'Introduction' }).count(), 1);
  const headingBottom = await page.locator('#page h2').evaluate(el => el.getBoundingClientRect().bottom);
  const bodyTop = await page.getByText('SectionBody', { exact: true }).evaluate(el => el.getBoundingClientRect().top);
  assert.ok(bodyTop > headingBottom, 'ordinary section remains a block heading');
  await screenshot(page, 'paragraph-plan');
});

test('consecutive, starred, and indented subparagraph headings preserve paragraph boundaries', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, tex(String.raw`\paragraph{First.} FirstBody starts here.
\paragraph*{Second.} SecondBody starts here.
\subparagraph{Detail.} DetailBody starts here.`));
  await open();
  const first = await assertRunIn(page, 'First.', 'FirstBody');
  const second = await assertRunIn(page, 'Second.', 'SecondBody');
  const detail = await assertRunIn(page, 'Detail.', 'DetailBody', '6');
  assert.ok(second.title.top > first.title.bottom, 'consecutive heading starts a new paragraph');
  assert.ok(detail.title.top > second.title.bottom);
  assert.ok(detail.title.left > second.title.left + 10, 'subparagraph retains its indentation');
  assert.equal(await page.locator('#page .run-in-heading .sec-num').count(), 0);
  assert.equal(await page.locator('#page p.para-run-in').count(), 3);
});

test('nested theorem, proof, and list headings start a new line and run into body text', { timeout: 30000 }, async t => {
  const body = String.raw`\begin{theorem}
TheoremLead comes first.
\paragraph{Theorem plan.} TheoremBody follows.
\end{theorem}
\begin{proof}
ProofLead comes first.
\paragraph{Proof plan.} ProofBody follows.
\paragraph{Proof detail.} ProofDetail follows.
\end{proof}
\begin{itemize}
\item ListLead comes first.
\paragraph{List plan.} ListBody follows.
\end{itemize}`;
  const { page, open, update } = await fixture(t, tex(body));
  await open();
  const proofHeadRight = await page.locator('.proof-head').evaluate(el => el.getBoundingClientRect().right);
  const proofLeadLeft = await page.getByText('ProofLead', { exact: true }).evaluate(el => el.getBoundingClientRect().left);
  assert.ok(proofLeadLeft - proofHeadRight < 30,
    'the first proof line keeps a natural gap after Proof. instead of full-line justification');
  for (const [title, word, lead] of [
    ['Theorem plan.', 'TheoremBody', 'TheoremLead'],
    ['Proof plan.', 'ProofBody', 'ProofLead'],
    ['Proof detail.', 'ProofDetail', 'ProofBody'],
    ['List plan.', 'ListBody', 'ListLead'],
  ]) {
    const geometry = await assertRunIn(page, title, word);
    const preceding = await page.getByText(lead, { exact: true }).evaluate(el => el.getBoundingClientRect().bottom);
    assert.ok(geometry.title.top >= preceding - 1, `${title} starts below preceding prose`);
    const width = await page.locator('.run-in-heading').filter({ hasText: title }).evaluate(el => {
      const context = document.createElement('canvas').getContext('2d');
      context.font = getComputedStyle(el).font;
      return { actual: el.getBoundingClientRect().width, natural: context.measureText(el.textContent).width };
    });
    assert.ok(width.actual <= width.natural * 1.25 + 8,
      `${title} does not stretch its internal spaces across a justified short line: ${JSON.stringify(width)}`);
  }
  assert.equal(await page.locator('#page .run-in-heading .sec-num').count(), 0);
  await screenshot(page, 'paragraph-nested');
  await update(tex(body.replace('Proof detail.', 'Revised detail.').replace('ProofDetail follows.', 'RevisedBody follows.')));
  await page.locator('.run-in-heading').filter({ hasText: 'Revised detail.' }).waitFor();
  await assertRunIn(page, 'Revised detail.', 'RevisedBody');
  await assertRunIn(page, 'Proof plan.', 'ProofBody');
  assert.equal(await page.locator('.run-in-heading').filter({ hasText: 'Proof detail.' }).count(), 0);
});

for (const depth of [4, 5]) {
  test(`secnumdepth ${depth} opts into paragraph numbering without numbering starred headings`, { timeout: 30000 }, async t => {
    const { page, open } = await fixture(t, tex(String.raw`\section{One}
\subsection{Two}
\subsubsection{Three}
\paragraph{Numbered.} NumberedBody follows.
\paragraph*{Starred.} StarredBody follows.
\subparagraph{Detail.} DetailBody follows.`, `\\setcounter{secnumdepth}{${depth}}`));
    await open();
    assert.equal(await page.locator('.run-in-heading').filter({ hasText: 'Numbered.' }).locator('.sec-num').textContent(), '1.1.1.1');
    assert.equal(await page.locator('.run-in-heading').filter({ hasText: 'Starred.' }).locator('.sec-num').count(), 0);
    const detailNumber = page.locator('.run-in-heading').filter({ hasText: 'Detail.' }).locator('.sec-num');
    if (depth === 5) assert.equal(await detailNumber.textContent(), '1.1.1.1.1');
    else assert.equal(await detailNumber.count(), 0);
    await assertRunIn(page, 'Numbered.', 'NumberedBody');
    await assertRunIn(page, 'Detail.', 'DetailBody', '6');
  });
}

test('paragraph source jumps and live title updates keep fresh heading anchors', { timeout: 30000 }, async t => {
  const body = String.raw`\paragraph{Plan of the paper.}\label{para:plan}

% A heading still runs into its body across source-only whitespace.

Opening explains the plan.

See \ref{para:plan}.`;
  const source = tex(body);
  const { page, open, update, input } = await fixture(t, source);
  const jumps = [];
  for (const path of ['/jump', '/reveal-source']) {
    await page.route(`**${path}`, async route => {
      jumps.push(route.request().postDataJSON());
      await route.fulfill({ status: 204 });
    });
  }
  await open();
  await page.locator('.run-in-heading').click({ modifiers: ['Meta'] });
  for (let i = 0; jumps.length < 2; i++) {
    assert.ok(i < 100, 'both source endpoints receive heading location');
    await delay(25);
  }
  const sourcePath = await realpath(input);
  const line = source.split('\n').findIndex(value => value.includes('\\paragraph{')) + 1;
  assert.ok(jumps.every(jump => jump.file === sourcePath && jump.line === line));
  await update(tex(body.replace('Plan of the paper.', 'Updated plan.')));
  await page.locator('.run-in-heading').filter({ hasText: 'Updated plan.' }).waitFor();
  await assertRunIn(page, 'Updated plan.', 'Opening');
  assert.equal(await page.locator('.run-in-heading').filter({ hasText: 'Plan of the paper.' }).count(), 0);
  assert.equal(await page.locator('#page a.ref[data-target="para:plan"]').evaluate(el =>
    !!document.getElementById(el.getAttribute('href').slice(1))), true, 'reference retains a live target');
});

test('Markdown level-five headings remain block headings', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t, '##### Markdown heading\n\nMarkdownBody remains below.\n', 'notes.md');
  await open();
  assert.equal(await page.locator('#page .run-in-heading').count(), 0);
  const heading = page.locator('#page h5');
  assert.equal(await heading.textContent(), 'Markdown heading');
  const bottom = await heading.evaluate(el => el.getBoundingClientRect().bottom);
  const top = await page.getByText('MarkdownBody', { exact: true }).evaluate(el => el.getBoundingClientRect().top);
  assert.ok(top >= bottom, 'Markdown H5 remains above its paragraph');
});
