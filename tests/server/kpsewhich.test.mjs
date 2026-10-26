import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, unlink, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';

// The fake lookup tool and its environment belong only to each child process.
// These tests do not depend on, or modify, the user's TeX installation.
// Run after `cargo build`: node --test tests/server/kpsewhich.test.mjs
const binary = resolve(process.env.MATHPREVIEW_CLI || 'target/debug/mathpreview-cli');
const macro = (name, value) => `\\newcommand{\\${name}}{${value}}\n`;
const tex = (preamble, body = String.raw`$\PreviewCustom + \PreviewNested + \PreviewChild$`) =>
  `\\documentclass{article}\n${preamble}\n\\begin{document}\n${body}\n\\end{document}\n`;

async function fixture(t) {
  const directory = await realpath(await mkdtemp(join(tmpdir(), 'mathpreview-kpse-test-')));
  const project = join(directory, 'project');
  const external = join(directory, 'custom tex tree');
  const distribution = join(directory, 'texmf-dist');
  const tools = join(directory, 'tools');
  const emptyTools = join(directory, 'no-tools');
  const log = join(directory, 'lookups.log');
  await Promise.all([project, external, distribution, tools, emptyTools].map(path => mkdir(path)));
  const root = join(project, 'main.tex');
  const style = join(external, 'custom-notes.sty');
  const child = join(external, 'child-notes.sty');
  const nested = join(external, 'nested.tex');
  const localStyle = join(project, 'custom-notes.sty');
  await writeFile(style, macro('PreviewCustom', 'ExternalValue') + '\\input{nested}\n\\RequirePackage{child-notes}\n');
  await writeFile(child, macro('PreviewChild', 'ChildValue'));
  await writeFile(nested, macro('PreviewNested', 'NestedValue'));
  await writeFile(join(distribution, 'distribution.sty'), macro('DistributionInternal', 'MustNotLoad'));
  const executable = join(tools, 'kpsewhich');
  await writeFile(executable, String.raw`#!/bin/sh
case "$1" in
  --var-value=TEXMFDIST|--var-value=TEXMFMAIN)
    printf '%s\n' "$MATHPREVIEW_TEST_DIST"
    exit 0
    ;;
esac
printf '%s\n' "$PWD|$*" >> "$MATHPREVIEW_TEST_LOG"
case "$MATHPREVIEW_TEST_KPSE_MODE" in
  failure) exit 2 ;;
  hang) while :; do :; done ;;
esac
for arg do name="$arg"; done
if [ "$MATHPREVIEW_TEST_RELATIVE_INPUTS" = "1" ]; then
  lookup_root="${'${TEXINPUTS%:}'}"
else
  lookup_root="$MATHPREVIEW_TEST_EXTERNAL"
fi
case "$name" in
  custom-notes.sty) printf '%s\n' "$lookup_root/custom-notes.sty" ;;
  child-notes.sty) printf '%s\n' "$lookup_root/child-notes.sty" ;;
  distribution.sty) printf '%s\n' "$MATHPREVIEW_TEST_DIST/distribution.sty" ;;
  *) exit 1 ;;
esac
`);
  await chmod(executable, 0o755);
  const env = {
    ...process.env,
    PATH: tools,
    XDG_CONFIG_HOME: join(directory, 'config'),
    TEXINPUTS: `${external}:`,
    TEXMFHOME: external,
    MATHPREVIEW_TEST_EXTERNAL: external,
    MATHPREVIEW_TEST_DIST: distribution,
    MATHPREVIEW_TEST_LOG: log,
  };
  const processes = [];
  t.after(async () => {
    for (const { childProcess, exited } of processes) {
      if (childProcess.exitCode === null && childProcess.signalCode === null) childProcess.kill('SIGKILL');
      await exited;
    }
    await rm(directory, { recursive: true, force: true });
  });
  function launch(args, extraEnv = {}) {
    const childProcess = spawn(binary, args, {
      cwd: project, env: { ...env, ...extraEnv }, stdio: ['pipe', 'pipe', 'pipe'],
    });
    const exited = once(childProcess, 'close');
    processes.push({ childProcess, exited });
    return { childProcess, exited };
  }
  async function lookups() {
    try { return (await readFile(log, 'utf8')).trim().split('\n').filter(Boolean); }
    catch (error) { if (error.code === 'ENOENT') return []; throw error; }
  }
  async function convert(requests, extraEnv = {}) {
    const { childProcess, exited } = launch(['convert'], extraEnv);
    let output = '';
    let errorOutput = '';
    childProcess.stdout.on('data', data => { output += data; });
    childProcess.stderr.on('data', data => { errorOutput += data; });
    childProcess.stdin.end(requests.map((request, i) => JSON.stringify({
      protocol: 'mathpreview.converter/v1', id: `request-${i}`, path: root, ...request,
    })).join('\n') + '\n');
    const timer = setTimeout(() => childProcess.kill('SIGKILL'), 10000);
    let code;
    try { [code] = await exited; } finally { clearTimeout(timer); }
    assert.equal(code, 0, `Converter failed: ${errorOutput}`);
    const responses = output.trim().split('\n').map(line => JSON.parse(line));
    assert.equal(responses.length, requests.length);
    for (const response of responses) assert.equal(response.status, 'ok', JSON.stringify(response));
    return responses.map(response => response.result);
  }
  return { root, style, child, nested, localStyle, project, external, distribution, emptyTools,
    launch, lookups, convert };
}

function assertMacro(result, name, body) {
  assert.equal(result.runtime.math.macros.find(macro => macro.name === name)?.body, body,
    `Macro ${name} missing or stale, dependencies: ${JSON.stringify(result.dependencies)}`);
}

function assertDependency(result, path, kind, exists = true) {
  assert.ok(result.dependencies.some(dependency =>
    dependency.path === path && dependency.kind === kind && dependency.exists === exists),
  `Missing ${kind} dependency ${path}: ${JSON.stringify(result.dependencies)}`);
}

test('converter finds external styles, nested declarations, and caches only paths', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  const source = tex('\\usepackage{custom-notes}');
  const [first, repeated] = await f.convert([{ source }, { source }]);
  for (const result of [first, repeated]) {
    assertMacro(result, 'PreviewCustom', 'ExternalValue');
    assertMacro(result, 'PreviewChild', 'ChildValue');
    assertMacro(result, 'PreviewNested', 'NestedValue');
    for (const path of [f.style, f.child, f.nested]) {
      assertDependency(result, path, 'include');
      assertDependency(result, path, 'preamble');
    }
    assertDependency(result, f.localStyle, 'include', false);
  }
  const lookups = await f.lookups();
  assert.equal(lookups.filter(line => line.endsWith(' custom-notes.sty')).length, 1,
    `Repeated conversions should reuse the lookup: ${lookups}`);
  assert.ok(lookups[0].startsWith(`${f.project}|`), 'lookup runs relative to the document');
  assert.ok(lookups[0].includes('--no-mktex=tex') && lookups[0].includes('-- custom-notes.sty'));

  const [overridden] = await f.convert([{ source: tex('\\RequirePackage{custom-notes}'),
    file_overrides: { [f.style]: macro('PreviewCustom', 'UnsavedExternal') } }]);
  assertMacro(overridden, 'PreviewCustom', 'UnsavedExternal');
});

test('project-local styles and unsaved local buffers take priority over TeX lookup', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  const source = tex('\\usepackage{custom-notes}');
  await writeFile(f.localStyle, macro('PreviewCustom', 'ProjectValue'));
  const [disk] = await f.convert([{ source }]);
  assertMacro(disk, 'PreviewCustom', 'ProjectValue');
  assert.ok(!disk.dependencies.some(dependency => dependency.path === f.style));
  await unlink(f.localStyle);
  const [unsaved] = await f.convert([{ source,
    file_overrides: { [f.localStyle]: macro('PreviewCustom', 'UnsavedProject') } }]);
  assertMacro(unsaved, 'PreviewCustom', 'UnsavedProject');
  assert.deepEqual(await f.lookups(), [], 'local styles must not launch kpsewhich');
});

test('nested preambles resolve relative TeX search paths from the document root', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  await mkdir(join(f.project, 'sub'));
  await writeFile(join(f.project, 'sub', 'defs.tex'), '\\usepackage{custom-notes}\n');
  const [result] = await f.convert([{ source: tex('\\input{sub/defs}') }], {
    TEXINPUTS: '../custom tex tree:', MATHPREVIEW_TEST_RELATIVE_INPUTS: '1',
  });
  assertMacro(result, 'PreviewCustom', 'ExternalValue');
  assertMacro(result, 'PreviewChild', 'ChildValue');
  assertMacro(result, 'PreviewNested', 'NestedValue');
  assert.ok((await f.lookups()).every(line => line.startsWith(`${f.project}|`)),
    'recursive dependencies must not change kpsewhich working directory');
});

test('missing or failing kpsewhich stays optional and distribution styles stay opaque', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  const source = tex('\\usepackage{custom-notes}\n' + macro('RootStillWorks', 'RootValue'));
  for (const env of [{ PATH: f.emptyTools }, { MATHPREVIEW_TEST_KPSE_MODE: 'failure' }]) {
    const [result] = await f.convert([{ source }], env);
    assertMacro(result, 'RootStillWorks', 'RootValue');
    assert.ok(!result.runtime.math.macros.some(macro => macro.name === 'PreviewCustom'));
    assertDependency(result, f.localStyle, 'include', false);
  }
  const [result] = await f.convert([{ source: tex('\\usepackage{distribution}') }]);
  assert.ok(!result.runtime.math.macros.some(macro => macro.name === 'DistributionInternal'));
  assert.ok(!result.dependencies.some(dependency => dependency.path.startsWith(f.distribution)));
});

test('a stalled kpsewhich cannot hold conversion indefinitely', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  const start = Date.now();
  const [result] = await f.convert([{ source: tex('\\usepackage{custom-notes}') }],
    { MATHPREVIEW_TEST_KPSE_MODE: 'hang' });
  assert.ok(Date.now() - start < 5000, 'lookup deadline should fail closed promptly');
  assert.ok(!result.runtime.math.macros.some(macro => macro.name === 'PreviewCustom'));
});

test('external style changes reload live and creating a local style shadows the cached path', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await writeFile(f.root, tex('\\usepackage{custom-notes}'));
  const probe = createServer();
  probe.listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const port = probe.address().port;
  await new Promise(resolveClose => probe.close(resolveClose));
  const url = `http://127.0.0.1:${port}`;
  const { childProcess } = f.launch(['serve', f.root, '--port', String(port), '--editor', '']);
  childProcess.stdin.end();
  childProcess.stdout.resume();
  let logs = '';
  childProcess.stderr.on('data', data => { logs += data; });
  async function waitFor(predicate) {
    for (let i = 0; i < 120; i++) {
      assert.equal(childProcess.exitCode, null, `Daemon exited: ${logs}`);
      try { if (await predicate()) return; } catch { /* listener or latest render not ready */ }
      await delay(50);
    }
    assert.fail(`Expected live state was not observed: ${logs}`);
  }
  await waitFor(async () => (await fetch(`${url}/debug`)).ok);
  const debug = await (await fetch(`${url}/debug`)).json();
  assert.ok(debug.preamble_files.includes(f.style));
  assert.ok(debug.watched.includes(f.style));
  assert.ok(debug.watched.includes(f.localStyle));
  const socket = new WebSocket(`ws://127.0.0.1:${port}/ws?v=${debug.ws_protocol}`);
  t.after(() => socket.close());
  const events = [];
  socket.addEventListener('message', ({ data }) => events.push(JSON.parse(data)));
  await new Promise((resolveOpen, reject) => {
    socket.addEventListener('open', resolveOpen, { once: true });
    socket.addEventListener('error', reject, { once: true });
  });
  await writeFile(f.style, macro('PreviewCustom', 'UpdatedExternal'));
  await waitFor(async () => (await (await fetch(url)).text()).includes('UpdatedExternal'));
  assert.ok(events.some(event => event.event === 'full-reload'));

  const focus = (sequence, file, buffers = []) => fetch(`${url}/editor-state`, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ session: 'kpse-preamble-test', sequence, file, buffers }),
  });
  assert.equal((await focus(1, f.style)).status, 202);
  const intermediate = macro('PreviewCustom', 'IntermediateExternal');
  assert.equal((await fetch(`${url}/buffer`, {
    method: 'POST', headers: { 'x-mathpreview-path': f.style }, body: intermediate,
  })).status, 202);
  await delay(400);
  const heldPage = await (await fetch(url)).text();
  assert.ok(heldPage.includes('UpdatedExternal') && !heldPage.includes('IntermediateExternal'));
  assert.equal((await focus(2, f.root, [{
    file: f.style, source: macro('PreviewCustom', 'FinalExternal'),
  }])).status, 204);
  await waitFor(async () => (await (await fetch(url)).text()).includes('FinalExternal'));

  await writeFile(f.localStyle, macro('PreviewCustom', 'NewProjectStyle'));
  await waitFor(async () => (await (await fetch(url)).text()).includes('NewProjectStyle'));
  const updatedDebug = await (await fetch(`${url}/debug`)).json();
  assert.ok(updatedDebug.preamble_files.includes(f.localStyle));
  assert.ok(!updatedDebug.preamble_files.includes(f.style));
});
