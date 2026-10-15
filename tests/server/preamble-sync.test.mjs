import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, writeFile, rm, realpath } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';

// Uses Node's built-in WebSocket (Node 22+) without launching a browser.
// Run after `cargo build`: node --test tests/server/preamble-sync.test.mjs
const binary = resolve(process.env.MATHPREVIEW_CLI || 'target/debug/mathpreview-cli');
const macro = (name, value) => `\\newcommand{\\${name}}{${value}}\n`;

async function fixture(t) {
  const directory = await realpath(await mkdtemp(join(tmpdir(), 'mathpreview-preamble-ws-')));
  let daemon;
  let exited;
  let socket;
  t.after(async () => {
    socket?.close();
    if (daemon && daemon.exitCode === null) daemon.kill('SIGTERM');
    if (exited) await exited;
    await rm(directory, { recursive: true, force: true });
  });
  const root = join(directory, 'main.tex');
  const a = join(directory, 'a.tex');
  const b = join(directory, 'b.sty');
  const body = join(directory, 'body.tex');
  await writeFile(root, String.raw`\documentclass{article}
\input{a}
\input{b.sty}
\begin{document}
$\PreviewA + \PreviewB$
\input{body}
\end{document}
`);
  await writeFile(a, macro('PreviewA', 'OriginalAlpha'));
  await writeFile(b, macro('PreviewB', 'OriginalBeta'));
  await writeFile(body, 'OriginalBody\n');
  const probe = createServer();
  probe.listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const port = probe.address().port;
  await new Promise(resolveClose => probe.close(resolveClose));
  const url = `http://127.0.0.1:${port}`;
  daemon = spawn(binary, ['serve', root, '--port', String(port), '--editor', ''], {
    env: { ...process.env, XDG_CONFIG_HOME: join(directory, 'config') },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  let logs = '';
  daemon.stderr.on('data', data => { logs += data; });
  exited = once(daemon, 'exit');
  for (let i = 0; ; i++) {
    assert.ok(i < 100 && daemon.exitCode === null, `Daemon did not start: ${logs}`);
    try { if ((await fetch(`${url}/debug`)).ok) break; } catch { /* listener not bound yet */ }
    await delay(50);
  }
  const debug = async () => (await fetch(`${url}/debug`)).json();
  const state = await debug();
  socket = new WebSocket(`ws://127.0.0.1:${port}/ws?v=${state.ws_protocol}`);
  const events = [];
  socket.addEventListener('message', ({ data }) => events.push(JSON.parse(data)));
  await new Promise((resolveOpen, reject) => {
    socket.addEventListener('open', resolveOpen, { once: true });
    socket.addEventListener('error', reject, { once: true });
  });
  const rendered = () => events.filter(event =>
    ['patch', 'body-updated', 'full-reload', 'error'].includes(event.event));
  async function focus(sequence, file, buffers = []) {
    return fetch(`${url}/editor-state`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ session: 'preamble-ws-test', sequence, file, buffers }),
    });
  }
  async function push(file, source, sequence) {
    const headers = { 'x-mathpreview-path': file };
    if (sequence !== undefined) {
      headers['x-mathpreview-editor-session'] = 'preamble-ws-test';
      headers['x-mathpreview-editor-sequence'] = String(sequence);
    }
    return fetch(`${url}/buffer`, { method: 'POST', headers, body: source });
  }
  async function waitForRender(count) {
    for (let i = 0; rendered().length < count; i++) {
      assert.ok(i < 100, `Missing render event ${count}: ${JSON.stringify(events)}\n${logs}`);
      await delay(25);
    }
  }
  const page = async () => (await fetch(url)).text();
  return { directory, root, a, b, body, debug, rendered, focus, push, waitForRender, page };
}

test('preamble edits hold all renders and resume with exactly one atomic reload', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  assert.equal((await f.focus(1, f.a)).status, 202);
  assert.equal((await f.debug()).preamble_edit_paused, true);
  assert.equal((await f.push(f.a, macro('PreviewA', 'IntermediateAlpha'))).status, 202);
  await writeFile(f.b, macro('PreviewB', 'IntermediateBeta'));
  await delay(700);
  assert.deepEqual(f.rendered(), [], 'Preamble typing or disk save emitted a render event while paused');
  assert.equal((await f.focus(2, join(f.directory, 'unrelated.tex'))).status, 400);
  assert.equal((await f.debug()).preamble_edit_paused, true);

  const final = [
    { file: f.a, source: macro('PreviewA', 'FinalAlpha') },
    { file: f.b, source: macro('PreviewB', 'FinalBeta') },
  ];
  assert.equal((await f.focus(3, f.body, final)).status, 204);
  await f.waitForRender(1);
  await delay(400);
  assert.deepEqual(f.rendered().map(event => event.event), ['full-reload'],
    'The complete preamble batch must produce exactly one reload');
  const page = await f.page();
  assert.ok(page.includes('FinalAlpha') && page.includes('FinalBeta'));
  assert.ok(!page.includes('IntermediateAlpha') && !page.includes('IntermediateBeta'));

  assert.equal((await f.focus(4, f.body, final)).status, 204);
  assert.equal((await f.push(f.a, macro('PreviewA', 'StaleAlpha'), 2)).status, 204);
  await delay(400);
  assert.equal(f.rendered().length, 1, 'Duplicate focus or stale buffer caused another reload');
  assert.equal((await f.push(f.body, 'UpdatedBody\n', 5)).status, 204);
  await f.waitForRender(2);
  assert.equal(f.rendered().length, 2);
  assert.ok(['patch', 'body-updated'].includes(f.rendered()[1].event));
  assert.ok((await f.page()).includes('UpdatedBody'));
});

test('standalone CLI watching remains live without an editor focus handshake', { timeout: 10000 }, async t => {
  const f = await fixture(t);
  assert.equal((await f.debug()).preamble_edit_paused, false);
  await writeFile(f.a, macro('PreviewA', 'ExternalAlpha'));
  await f.waitForRender(1);
  assert.equal(f.rendered()[0].event, 'full-reload');
  assert.ok((await f.page()).includes('ExternalAlpha'));
});
