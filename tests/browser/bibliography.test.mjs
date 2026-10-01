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

// The CLI embeds the renderer and viewer bundle. Rebuild with
// `npm run test:bibliography` before running these real-daemon checks.
const binary = resolve('target/debug/mathpreview-cli');
const browserName = process.env.MATHPREVIEW_TEST_BROWSER || 'chromium';
const browserType = { chromium, webkit }[browserName];
assert.ok(browserType, `Unsupported test browser: ${browserName}`);
let browser;
before(async () => { browser = await browserType.launch(); });
after(async () => { await browser?.close(); });

const padding = Array.from({ length: 30 }, (_, i) => `Stable paragraph ${i}.`).join('\n\n');
const distantPadding = Array.from({ length: 160 }, (_, i) => `Distant paragraph ${i}.`).join('\n\n');
const inlineBibliography = String.raw`\begin{thebibliography}{99}
\bibitem{uncited} Uncited Author. An uncited reference.
\bibitem{first} First Author. \emph{First title}. 2024.
\bibitem{second} Second Author. \textbf{Second title}. 2025.
\bibitem[Custom-26]{custom} Custom Author. A custom reference.
\end{thebibliography}`;

function documentSource(bibliography = inlineBibliography, citations = String.raw`See \cite{second}, then \cite{first}, then \cite{custom}.`, gap = padding) {
  return String.raw`\documentclass{article}
\begin{document}
${citations}

${gap}

${bibliography}
\end{document}
`;
}

async function fixture(t, source = documentSource(), files = {}) {
  const directory = await mkdtemp(join(tmpdir(), 'mathpreview-bibliography-test-'));
  const input = join(directory, 'notes.tex');
  await writeFile(input, source);
  for (const [name, body] of Object.entries(files)) await writeFile(join(directory, name), body);
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
    try {
      if ((await fetch(url)).ok) break;
    } catch { /* wait for the listener */ }
    await delay(50);
  }
  async function open() {
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => document.querySelector('#ws-status')?.textContent.includes('live'));
  }
  async function update(nextSource, name = 'notes.tex') {
    events.length = 0;
    const response = await fetch(`${url}/buffer`, {
      method: 'POST',
      headers: { 'x-mathpreview-path': join(directory, name) },
      body: nextSource,
    });
    assert.equal(response.status, 204);
    for (let attempt = 0; !events.includes('patch'); attempt++) {
      assert.ok(attempt < 100, `Expected a patch, got ${events.join(', ')}`);
      await delay(50);
    }
  }
  return { page, open, update, input, directory };
}

async function hoverCitation(page, key) {
  const link = page.locator(`#page a.cite[data-key="${key}"]`).first();
  await link.dispatchEvent('mouseout');
  await link.dispatchEvent('mouseover');
  await page.waitForSelector('.hover-preview .bib-preview');
}

async function assertCitation(page, key, expected) {
  const link = page.locator(`#page a.cite[data-key="${key}"]`).first();
  assert.equal(await link.textContent(), expected);
  const target = await link.evaluate(el => {
    const label = document.getElementById(el.getAttribute('href').slice(1));
    return label && { key: label.dataset.key, label: label.textContent, next: label.nextElementSibling?.tagName };
  });
  assert.deepEqual(target, { key, label: `[${expected}]`, next: 'DD' });
}

async function screenshot(page, name) {
  const directory = process.env.MATHPREVIEW_TEST_SCREENSHOT_DIR;
  if (!directory) return;
  await mkdir(directory, { recursive: true });
  await page.screenshot({ path: join(directory, `${browserName}-${name}.png`), animations: 'disabled' });
}

test('inline bibliography keeps list-order labels, uncited entries, and formatted bodies', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  await open();
  await assertCitation(page, 'second', '3');
  await assertCitation(page, 'first', '2');
  await assertCitation(page, 'custom', 'Custom-26');
  assert.deepEqual(await page.locator('#page dt.bib-label').evaluateAll(labels => labels.map(el => el.dataset.key)),
    ['uncited', 'first', 'second', 'custom']);
  assert.equal(await page.locator('#page dt[data-key="first"] + dd em').textContent(), 'First title');
  assert.equal(await page.locator('#page dt[data-key="second"] + dd strong').textContent(), 'Second title');
  assert.doesNotMatch(await page.locator('#page .references').textContent(), /\\(?:begin|end|bibitem)|\{99\}/);
  await page.locator('#page .references').scrollIntoViewIfNeeded();
  await screenshot(page, 'inline-bibliography');
});

test('explicit labels do not advance the ordinary bibliography counter', { timeout: 30000 }, async t => {
  const bibliography = String.raw`\begin{thebibliography}{9}
\bibitem[Manual]{manual} A manually labeled entry.
\bibitem{first} The first numbered entry.
\bibitem[Other]{other} Another manually labeled entry.
\bibitem{second} The second numbered entry.
\end{thebibliography}`;
  const { page, open } = await fixture(t, documentSource(bibliography,
    String.raw`See \cite{second}, \cite{manual}, \cite{first}, and \cite{other}.`));
  await open();
  await assertCitation(page, 'manual', 'Manual');
  await assertCitation(page, 'first', '1');
  await assertCitation(page, 'other', 'Other');
  await assertCitation(page, 'second', '2');
  assert.equal(new Set(await page.locator('#page dt.bib-label').evaluateAll(labels => labels.map(el => el.id))).size, 4,
    'custom and numbered labels retain distinct link targets');
});

test('long manual labels wrap without overlapping the entry or escaping the preview', { timeout: 30000 }, async t => {
  const longLabel = 'LongAuthorAndCollaborators'.repeat(6);
  const bibliography = `\\begin{thebibliography}{9}\n\\bibitem[${longLabel}]{long} An entry next to a long label.\n\\end{thebibliography}`;
  const { page, open } = await fixture(t, documentSource(bibliography, String.raw`See \cite{long}.`, ''));
  await open();
  const inlineLayout = await page.locator('#page a.cite[data-key="long"]').evaluate(link => {
    const paragraph = link.closest('p').getBoundingClientRect();
    return { left: paragraph.left, right: paragraph.right,
      lines: Array.from(link.getClientRects(), rect => ({ left: rect.left, right: rect.right })) };
  });
  assert.ok(inlineLayout.lines.length > 1, 'long inline citation wraps across lines');
  assert.ok(inlineLayout.lines.every(line => line.left >= inlineLayout.left - 1 && line.right <= inlineLayout.right + 1),
    JSON.stringify(inlineLayout));
  const layout = await page.locator('#page dl.bib-list').evaluate(list => {
    const dt = list.querySelector('dt').getBoundingClientRect();
    const dd = list.querySelector('dd').getBoundingClientRect();
    const box = list.getBoundingClientRect();
    return { dt: { left: dt.left, right: dt.right, top: dt.top, bottom: dt.bottom },
      dd: { left: dd.left, right: dd.right, top: dd.top, bottom: dd.bottom },
      left: box.left, right: box.right, overflow: list.scrollWidth > list.clientWidth + 1 };
  });
  assert.ok(layout.dt.right <= layout.dd.left + 1 || layout.dt.bottom <= layout.dd.top + 1, JSON.stringify(layout));
  assert.ok(layout.dt.left >= layout.left - 1 && layout.dt.right <= layout.right + 1, JSON.stringify(layout));
  assert.ok(!layout.overflow, JSON.stringify(layout));
  await screenshot(page, 'long-label-page');
  await hoverCitation(page, 'long');
  assert.equal(await page.locator('.hover-preview dt').textContent(), `[${longLabel}]`);
  assert.ok(await page.locator('.hover-preview').evaluate(el => el.scrollWidth <= el.clientWidth + 1),
    'long label wraps within the hover card');
  await screenshot(page, 'long-label-hover');
  await page.locator('#page a.cite[data-key="long"]').dispatchEvent('mouseout');
  await page.locator('#margin-toggle').click();
  await page.locator('#page a.cite[data-key="long"]').dispatchEvent('click');
  const card = page.locator('.margin-card[data-pin-key="long"]');
  await card.waitFor();
  assert.ok(await card.locator('.margin-card-body').evaluate(el => el.scrollWidth <= el.clientWidth + 1),
    'long label wraps within the margin card');
  await screenshot(page, 'long-label-margin');
});

test('citation hover clones only the matching full entry and margin mode pins it', { timeout: 30000 }, async t => {
  const { page, open } = await fixture(t);
  await open();
  await hoverCitation(page, 'second');
  assert.equal(await page.locator('.hover-preview dt').textContent(), '[3]');
  assert.match(await page.locator('.hover-preview dd').textContent(), /Second Author.*Second title/);
  assert.doesNotMatch(await page.locator('.hover-preview').textContent(), /First Author|Custom Author|Uncited Author/);
  await page.locator('#page a.cite[data-key="second"]').dispatchEvent('mouseout');
  await page.locator('#margin-toggle').click();
  await page.locator('#page a.cite[data-key="second"]').click();
  const card = page.locator('.margin-card[data-pin-key="second"]');
  await card.waitFor();
  assert.equal(await card.locator('.margin-card-key').textContent(), 'second');
  assert.equal(await card.locator('dt').textContent(), '[3]');
  assert.match(await card.locator('dd').textContent(), /Second Author.*Second title/);
});

test('hover renders math in a distant inline bibliography without scrolling to it', { timeout: 30000 }, async t => {
  const bibliography = String.raw`\begin{thebibliography}{9}
\bibitem{math} Math Author. A result about $E=mc^2$. \url{https://example.org/paper%20name?q=one%26two}.
\href{https://example.org/a%20b}{Read this paper}. Tail survives.
\bibitem{neighbor} Neighbor Author. A different result about $a=b$.
\end{thebibliography}`;
  const { page, open } = await fixture(t, documentSource(bibliography, String.raw`See \cite{math}.`, distantPadding));
  await open();
  await page.waitForFunction(() => window.__mpEngine.isReady());
  const source = '#page dt[data-key="math"] + dd';
  const neighbor = '#page dt[data-key="neighbor"] + dd';
  assert.equal(await page.locator(`${source} svg`).count(), 0, 'distant entry starts untypeset');
  assert.ok(await page.locator(source).evaluate(el => el.getBoundingClientRect().top > 1800));
  const beforeScroll = await page.evaluate(() => window.scrollY);
  await hoverCitation(page, 'math');
  await page.waitForSelector('.hover-preview dd svg');
  assert.match(await page.locator('.hover-preview dd').textContent(), /Math Author/);
  assert.deepEqual(await page.locator('.hover-preview dd a').evaluateAll(links => links.map(link => link.getAttribute('href'))),
    ['https://example.org/paper%20name?q=one%26two', 'https://example.org/a%20b']);
  assert.match(await page.locator('.hover-preview dd').textContent(), /Read this paper.*Tail survives/);
  assert.equal(await page.locator(`${neighbor} svg`).count(), 0, 'unreferenced neighbor stays lazy');
  assert.equal(await page.evaluate(() => window.scrollY), beforeScroll);
});

test('unchanged bibliography nodes keep fresh source positions after an earlier blank-line edit', { timeout: 30000 }, async t => {
  const source = documentSource();
  const { page, open, update } = await fixture(t, source);
  await open();
  const selector = '#page dt[data-key="first"] + dd';
  const beforePosition = await page.locator(selector).evaluate(el => {
    el.__bibliographyTestIdentity = true;
    const pieces = el.getAttribute('data-src').split(':');
    return { line: Number(pieces.at(-2)), col: Number(pieces.at(-1)), id: el.id };
  });
  await update(source.replace('\\begin{document}\n', '\\begin{document}\n\n\n'));
  await page.waitForFunction(({ selector, line }) => {
    const el = document.querySelector(selector);
    return Number(el?.getAttribute('data-src').split(':').at(-2)) === line;
  }, { selector, line: beforePosition.line + 2 });
  const afterPosition = await page.locator(selector).evaluate(el => {
    const pieces = el.getAttribute('data-src').split(':');
    return { line: Number(pieces.at(-2)), col: Number(pieces.at(-1)), id: el.id,
      sameNode: el.__bibliographyTestIdentity === true };
  });
  assert.deepEqual(afterPosition, { ...beforePosition, line: beforePosition.line + 2, sameNode: true });
  const labelLine = await page.locator('#page dt[data-key="first"]').evaluate(el =>
    Number(el.getAttribute('data-src').split(':').at(-2)));
  assert.equal(labelLine, afterPosition.line);
});

test('included bibliography body and source jump use the included file, including live edits', { timeout: 30000 }, async t => {
  const included = String.raw`\begin{thebibliography}{9}
\bibitem{included}
Included Author. \emph{Original included title}.
\end{thebibliography}
`;
  const { page, open, update, directory } = await fixture(t,
    documentSource(String.raw`\input{references}`, String.raw`See \cite{included}.`), { 'references.tex': included });
  const jumps = [];
  for (const path of ['/jump', '/reveal-source']) {
    await page.route(`**${path}`, async route => {
      jumps.push({ path, body: route.request().postDataJSON() });
      await route.fulfill({ status: 204 });
    });
  }
  await open();
  await assertCitation(page, 'included', '1');
  const entry = page.locator('#page dt[data-key="included"] + dd');
  await entry.locator('em').click({ modifiers: ['Meta'] });
  for (let attempt = 0; jumps.length < 2; attempt++) {
    assert.ok(attempt < 100, 'both source-jump endpoints were called');
    await delay(25);
  }
  assert.deepEqual(jumps.map(jump => jump.path).sort(), ['/jump', '/reveal-source']);
  const includedPath = await realpath(join(directory, 'references.tex'));
  assert.ok(jumps.every(jump => jump.body.file === includedPath && jump.body.line === 3),
    JSON.stringify(jumps));
  await update(included.replace('Original included title', 'Revised included title'), 'references.tex');
  await assertCitation(page, 'included', '1');
  await page.locator('#page a.cite[data-key="included"]').scrollIntoViewIfNeeded();
  // Allow scroll and content-visibility events to settle before starting a
  // hover, since any genuine page scroll intentionally dismisses its bubble.
  await delay(350);
  await hoverCitation(page, 'included');
  assert.match(await page.locator('.hover-preview dd').textContent(), /Revised included title/);
  assert.doesNotMatch(await page.locator('.hover-preview dd').textContent(), /Original included title/);
});

test('live body, label, and key edits do not leave stale citation previews or links', { timeout: 30000 }, async t => {
  const { page, open, update } = await fixture(t);
  await open();
  await hoverCitation(page, 'custom');
  const revised = inlineBibliography.replace('Custom-26', 'Revised-27').replace('A custom reference.', 'A revised reference.');
  await update(documentSource(revised));
  // Live patches may refresh or dismiss the existing bubble. Reopen through
  // the live link and ensure neither the old label nor body can reappear.
  await hoverCitation(page, 'custom');
  await assertCitation(page, 'custom', 'Revised-27');
  assert.equal(await page.locator('.hover-preview dt').textContent(), '[Revised-27]');
  assert.match(await page.locator('.hover-preview dd').textContent(), /A revised reference/);
  assert.doesNotMatch(await page.locator('.hover-preview').textContent(), /Custom-26|A custom reference/);
  const renamed = revised.replace('{custom}', '{renamed}');
  await update(documentSource(renamed));
  await page.waitForSelector('#page dt[data-key="renamed"]', { state: 'attached' });
  assert.equal(await page.locator('#page a.cite[data-key="custom"]').count(), 0);
  assert.equal(await page.locator('#page .cite.missing[data-key="custom"]').count(), 1);
  assert.equal(await page.locator('#page dt[data-key="custom"]').count(), 0);
  assert.equal(await page.locator('#page dt[data-key="renamed"]').count(), 1);
  assert.equal(await page.locator('.hover-preview').count(), 0, 'removed key dismisses its hover');
  await update(documentSource(renamed, String.raw`See \cite{second}, then \cite{first}, then \cite{renamed}.`));
  await assertCitation(page, 'renamed', 'Revised-27');
  await hoverCitation(page, 'renamed');
  assert.match(await page.locator('.hover-preview dd').textContent(), /A revised reference/);
});

const bibtex = String.raw`@book{zulu,
  author = {Zulu, Zoe},
  title = {Zulu book},
  publisher = {Test Press},
  year = {2020}
}
@book{alpha,
  author = {Alpha, Ann},
  title = {Alpha book},
  publisher = {Test Press},
  year = {2021}
}
`;

for (const [style, expected] of [['plain', ['2', '1']], ['unsrt', ['1', '2']], ['alpha', ['Zul20', 'Alp21']]]) {
  test(`existing BibTeX ${style} bibliography keeps citation labels and hover content`, { timeout: 30000 }, async t => {
    const { page, open } = await fixture(t, documentSource(
      `\\bibliographystyle{${style}}\n\\bibliography{references}`,
      String.raw`See \cite{zulu}, then \cite{alpha}.`), { 'references.bib': bibtex });
    await open();
    await assertCitation(page, 'zulu', expected[0]);
    await assertCitation(page, 'alpha', expected[1]);
    await hoverCitation(page, 'zulu');
    assert.match(await page.locator('.hover-preview dd').textContent(), /Zoe Zulu.*Zulu book/);
    assert.doesNotMatch(await page.locator('.hover-preview dd').textContent(), /Alpha book/);
  });
}

test('existing biblatex author-year citations retain labels and complete previews', { timeout: 30000 }, async t => {
  const source = documentSource(String.raw`\printbibliography`, String.raw`See \cite{zulu}, then \cite{alpha}.`)
    .replace('\\begin{document}', String.raw`\usepackage[style=authoryear]{biblatex}
\addbibresource{references.bib}
\begin{document}`);
  const { page, open } = await fixture(t, source, { 'references.bib': bibtex });
  await open();
  assert.equal(await page.locator('#page a.cite[data-key="zulu"]').textContent(), 'Zulu, 2020');
  assert.equal(await page.locator('#page a.cite[data-key="alpha"]').textContent(), 'Alpha, 2021');
  assert.equal(await page.locator('#page dt[data-key="alpha"]').textContent(), 'Alpha, 2021.');
  await hoverCitation(page, 'alpha');
  assert.match(await page.locator('.hover-preview dd').textContent(), /Ann Alpha.*Alpha book/);
});
