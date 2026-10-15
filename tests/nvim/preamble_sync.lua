-- Headless integration regression for deferred, atomic preamble updates.
-- Run from the repository root after `cargo build`:
--   MATHPREVIEW_CLI=target/debug/mathpreview-cli \
--     nvim --headless -u NONE -i NONE -l tests/nvim/preamble_sync.lua

local repo = vim.fn.getcwd()
vim.opt.runtimepath:prepend(repo)
vim.o.hidden = true

local cli = vim.env.MATHPREVIEW_CLI
if not cli or cli == "" then cli = repo .. "/target/debug/mathpreview-cli" end
cli = vim.fn.fnamemodify(cli, ":p")
assert(vim.fn.executable(cli) == 1, "build mathpreview-cli before running this test")

local scratch = vim.fn.tempname() .. "-mathpreview-preamble-sync"
assert(vim.fn.mkdir(scratch, "p") == 1, "could not create scratch directory")
local uv = vim.uv or vim.loop
scratch = assert(uv.fs_realpath(scratch), "could not canonicalize scratch directory")
local original_config_home = vim.env.XDG_CONFIG_HOME
vim.env.XDG_CONFIG_HOME = scratch .. "/config"
local jobs = {}
local snapshots = {}
local original_system = vim.system
vim.system = function(args, opts, on_exit)
  local url = args[#args]
  if type(url) == "string" and url:sub(-13) == "/editor-state" and opts and opts.stdin then
    local decoded, payload = pcall(vim.json.decode, opts.stdin)
    if decoded then snapshots[#snapshots + 1] = { url = url, payload = payload } end
  end
  return original_system(args, opts, on_exit)
end
local original_jobstart = vim.fn.jobstart
vim.fn.jobstart = function(args, opts)
  local job = original_jobstart(args, opts)
  if type(args) == "table" and args[1] == cli and args[2] == "serve" and job > 0 then
    jobs[#jobs + 1] = job
  end
  return job
end

local mp = require("mathpreview")
mp.setup({
  cmd = cli,
  auto_open_browser = false,
  stale_check = false,
  sync = true,
  raise_on_jump = false,
  close_on_exit = true,
  debounce_ms = 200,
  cursor_debounce_ms = 30,
  jump_wait_ms = 1000,
  jump_retry_ms = 10,
})

local function write(path, lines)
  assert(vim.fn.writefile(lines, path) == 0, "could not write " .. path)
end

local function curl(port, route, payload)
  local args = { "curl", "--silent", "--show-error", "--fail-with-body", "--max-time", "1" }
  local opts = { text = true }
  if payload then
    vim.list_extend(args, {
      "--header", "content-type: application/json", "--data-binary", "@-", "-X", "POST",
    })
    opts.stdin = vim.json.encode(payload)
  end
  args[#args + 1] = "http://127.0.0.1:" .. tostring(port) .. route
  local result = vim.system(args, opts):wait()
  return result.code, result.stdout or "", result.stderr or ""
end

local function debug_state(port)
  local code, body = curl(port, "/debug")
  if code ~= 0 then return nil end
  local ok, decoded = pcall(vim.json.decode, body)
  return ok and decoded or nil
end

local function wait_for(predicate, message, diagnostics)
  assert(vim.wait(7000, predicate, 50),
    message .. "\nstatus: " .. vim.inspect(mp.status())
      .. (diagnostics and ("\ndebug: " .. vim.inspect(diagnostics())) or ""))
end

local function pause(ms)
  vim.wait(ms or 650, function() return false end, 25)
end

local function contains(page, needle)
  return page:find(needle, 1, true) ~= nil
end

local function expect_page(port, needles, message)
  local found
  wait_for(function()
    local code, page = curl(port, "/")
    if code ~= 0 then return false end
    for _, needle in ipairs(needles) do
      if not contains(page, needle) then return false end
    end
    found = page
    return true
  end, message)
  return found
end

local function expect_frozen(port, expected, message)
  -- Poll throughout a period longer than both the editor and watcher
  -- debounces. Checking only the final page would miss transient renders.
  for _ = 1, 8 do
    pause(100)
    local code, page = curl(port, "/")
    assert(code == 0 and page == expected, message)
  end
end

local function edit(path, ft)
  vim.cmd("edit " .. vim.fn.fnameescape(path))
  vim.bo.filetype = ft or "tex"
  return vim.api.nvim_get_current_buf()
end

local function change(buf, lines)
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.api.nvim_exec_autocmds("TextChanged", { buffer = buf })
end

local function start(path, ft)
  local buf = edit(path, ft)
  mp.start()
  wait_for(function()
    return mp.status().daemon_root == path and mp.status().daemon_port ~= nil
  end, "daemon did not register for " .. path)
  local port = mp.status().daemon_port
  wait_for(function()
    local state = debug_state(port)
    return state and state.editor_state_supported == true
  end, "daemon did not advertise editor-state support for " .. path)
  return buf, port
end

local function expect_paused(port)
  wait_for(function()
    local state = debug_state(port)
    return state and state.preamble_edit_paused == true
  end, "preamble focus did not pause rendering")
end

local ok, err = xpcall(function()
  local root = scratch .. "/main.tex"
  local preamble = scratch .. "/preamble.tex"
  local nested = scratch .. "/nested.sty"
  local body = scratch .. "/body.tex"
  local alias = scratch .. "/preamble-alias.tex"
  local other = scratch .. "/other.md"
  write(root, {
    [[\documentclass{article}]],
    [[\input{preamble}]],
    [[\begin{document}]],
    [[Root body. $\PreviewA + \PreviewB$]],
    [[\input{body}]],
    [[\end{document}]],
  })
  write(preamble, { [[\newcommand{\PreviewA}{InitialAlpha}]], [[\input{nested.sty}]] })
  write(nested, { [[\newcommand{\PreviewB}{InitialBeta}]] })
  write(body, { "Original body include." })
  write(other, { "# Other project", "", "Original Markdown body." })
  local linked, link_err = uv.fs_symlink(preamble, alias)
  assert(linked, "could not create preamble symlink: " .. tostring(link_err))

  local root_buf, port = start(root)
  local baseline = expect_page(port, { "InitialAlpha", "InitialBeta" },
    "initial preamble definitions were not rendered")
  local state = debug_state(port)
  assert(vim.tbl_contains(state.preamble_files or {}, vim.fn.resolve(preamble)),
    "preamble dependency was not advertised")
  assert(vim.tbl_contains(state.preamble_files or {}, vim.fn.resolve(nested)),
    "nested .sty dependency was not advertised")
  assert(vim.tbl_contains(state.body_files or {}, vim.fn.resolve(body)),
    "body include was not advertised separately from preambles")

  -- Visiting an alias must pause its canonical dependency before any edits.
  -- Saving even a syntactically unfinished command must not let the file
  -- watcher render a different project than the one still shown in the tab.
  local preamble_buf = edit(alias)
  expect_paused(port)
  change(preamble_buf, { [[\newcommand{\PreviewA}{IntermediateAlpha}]], [[\input{nested.sty}]] })
  expect_frozen(port, baseline, "typing a valid preamble definition changed the preview")
  change(preamble_buf, { [[\newcommand{\PreviewA}{UnfinishedAlpha]], [[\input{nested.sty}]] })
  vim.cmd("write")
  expect_frozen(port, baseline, "saving an unfinished preamble changed the preview")
  change(preamble_buf, { [[\newcommand{\PreviewA}{FinalAlpha}]], [[\input{nested.sty}]] })

  -- Several unsaved preamble buffers belong to one pending snapshot. Moving
  -- between them is not a return to the document and must not render either.
  local nested_buf = edit(nested)
  change(nested_buf, { [[\newcommand{\PreviewB}{FinalBeta}]] })
  expect_frozen(port, baseline, "moving between preamble files released pending changes")
  local unrelated = vim.api.nvim_create_buf(true, false)
  vim.api.nvim_buf_set_lines(unrelated, 0, -1, false, { "unrelated scratch buffer" })
  vim.api.nvim_set_current_buf(unrelated)
  expect_frozen(port, baseline, "an unrelated buffer released pending preamble changes")

  -- Another live daemon must remain independent, including its normal
  -- Markdown per-keystroke updates, while this TeX project stays paused.
  local other_buf, other_port = start(other, "markdown")
  change(other_buf, { "# Other project", "", "MarkdownStillLive" })
  expect_page(other_port, { "MarkdownStillLive" }, "Markdown stopped updating during a preamble pause")
  expect_frozen(port, baseline, "another preview project released pending preamble changes")

  -- No further edit/save event is needed: returning to a body include applies
  -- the latest unsaved contents of BOTH preamble files together.
  local before_resume = #snapshots
  local body_buf = edit(body)
  expect_page(port, { "FinalAlpha", "FinalBeta" },
    "returning to a body include did not apply all unsaved preamble buffers")
  local resume_batches = 0
  for i = before_resume + 1, #snapshots do
    local snapshot = snapshots[i]
    if snapshot.url == "http://127.0.0.1:" .. tostring(port) .. "/editor-state"
        and vim.fn.resolve(snapshot.payload.file) == vim.fn.resolve(body) then
      resume_batches = resume_batches + 1
      local sources = {}
      for _, buffer in ipairs(snapshot.payload.buffers or {}) do
        sources[vim.fn.resolve(buffer.file)] = buffer.source
      end
      assert(sources[vim.fn.resolve(preamble)]
          and contains(sources[vim.fn.resolve(preamble)], "FinalAlpha")
          and sources[vim.fn.resolve(nested)]
          and contains(sources[vim.fn.resolve(nested)], "FinalBeta"),
        "resume sent separate or incomplete preamble snapshots")
    end
  end
  assert(resume_batches >= 1, "returning to the body did not send an atomic editor snapshot")
  assert(vim.bo[preamble_buf].modified and vim.bo[nested_buf].modified,
    "preamble synchronization unexpectedly saved editor buffers")
  change(body_buf, { "BodyStillLive" })
  baseline = expect_page(port, { "BodyStillLive", "FinalAlpha", "FinalBeta" },
    "body includes no longer update live after resuming")

  -- Switching back before the debounce expires must commit the final buffer,
  -- not race a stale single-file timer against the all-source snapshot.
  vim.api.nvim_set_current_buf(preamble_buf)
  change(preamble_buf, { [[\newcommand{\PreviewA}{FastReturnAlpha}]], [[\input{nested.sty}]] })
  vim.api.nvim_set_current_buf(root_buf)
  baseline = expect_page(port, { "FastReturnAlpha", "FinalBeta" },
    "rapid return lost the final preamble edit")
  expect_frozen(port, baseline, "an old debounce changed the page after the resume snapshot")

  -- Browser inverse sync opens the existing unsaved preamble alias. This
  -- bypasses normal daemon activation internally, but must still pause it.
  local jumps_before = mp.status().jumps
  local jump_code, _, jump_err = curl(port, "/jump", {
    file = vim.fn.resolve(preamble), line = 1, col = 1,
  })
  assert(jump_code == 0, "could not queue a preamble inverse-sync jump: " .. jump_err)
  wait_for(function() return mp.status().jumps > jumps_before end,
    "plugin did not consume the preamble inverse-sync jump")
  assert(vim.api.nvim_get_current_buf() == preamble_buf,
    "inverse sync did not reuse the unsaved preamble alias")
  expect_paused(port)
  change(preamble_buf, { [[\newcommand{\PreviewA}{RestartAlpha}]], [[\input{nested.sty}]] })
  vim.api.nvim_set_current_buf(nested_buf)
  change(nested_buf, { [[\newcommand{\PreviewB}{RestartBeta}]] })
  expect_frozen(port, baseline, "inverse-sync preamble editing changed the preview")

  -- Restart must re-register the focused preamble and retain snapshots of all
  -- modified dependencies, not just the currently focused nested .sty file.
  -- A new process may initially read disk; returning to the document must
  -- restore the complete in-memory project without requiring a new keystroke.
  mp.restart()
  wait_for(function()
    local state_now = debug_state(port)
    return state_now and state_now.preamble_edit_paused == true
      and mp.status().daemon_port == port
  end, "restarted daemon did not restore the preamble editing pause", function()
    local state_now = debug_state(port)
    return state_now and {
      pid = state_now.pid,
      preamble_edit_paused = state_now.preamble_edit_paused,
      preamble_edit_paused = state_now.preamble_edit_paused,
      preamble_files = state_now.preamble_files,
      body_files = state_now.body_files,
    }
  end)
  vim.api.nvim_set_current_buf(root_buf)
  expect_page(port, { "RestartAlpha", "RestartBeta", "BodyStillLive" },
    "restart lost unsaved preamble/body snapshots")
  assert(vim.bo[preamble_buf].modified and vim.bo[nested_buf].modified,
    "restart cleared the pending preamble buffers' modified state")

  -- A later focus snapshot is authoritative about discarded buffers too.
  -- First put valid definitions on disk, then stage A by moving to B and
  -- discard A with :bdelete! before resuming. Its abandoned override must not
  -- survive just because the daemon received it during the earlier switch.
  vim.api.nvim_set_current_buf(preamble_buf)
  change(preamble_buf, { [[\newcommand{\PreviewA}{DiskAlpha}]], [[\input{nested.sty}]] })
  vim.cmd("write")
  vim.api.nvim_set_current_buf(nested_buf)
  change(nested_buf, { [[\newcommand{\PreviewB}{DiskBeta}]] })
  vim.cmd("write")
  vim.api.nvim_set_current_buf(root_buf)
  expect_page(port, { "DiskAlpha", "DiskBeta" }, "could not establish saved discard baseline")
  vim.api.nvim_set_current_buf(preamble_buf)
  change(preamble_buf, { [[\newcommand{\PreviewA}{AbandonedAlpha}]], [[\input{nested.sty}]] })
  vim.api.nvim_set_current_buf(nested_buf)
  change(nested_buf, { [[\newcommand{\PreviewB}{KeptBeta}]] })
  pause()
  vim.api.nvim_buf_delete(preamble_buf, { force = true })
  vim.api.nvim_set_current_buf(root_buf)
  baseline = expect_page(port, { "DiskAlpha", "KeptBeta" },
    "discarding a staged preamble did not restore its disk contents")
  assert(not contains(baseline, "AbandonedAlpha"), "discarded preamble text remained in the preview")

  -- Focus transitions must preserve the existing incomplete-edit guard.
  -- A snapshot containing unfinished body math must not slip through
  -- merely because its preamble changes are ready to commit.
  vim.api.nvim_set_current_buf(body_buf)
  change(body_buf, { [[IncompleteBody $x]] })
  preamble_buf = edit(alias)
  expect_paused(port)
  change(preamble_buf, { [[\newcommand{\PreviewA}{AfterIncompleteAlpha}]], [[\input{nested.sty}]] })
  vim.api.nvim_set_current_buf(root_buf)
  expect_frozen(port, baseline, "focus snapshot rendered incomplete body math")
  vim.api.nvim_set_current_buf(body_buf)
  change(body_buf, { [[CompletedBody $x$]] })
  expect_page(port, { "AfterIncompleteAlpha", "KeptBeta", "CompletedBody" },
    "completing the body did not apply the pending preamble and body changes")

  -- A loaded but unmodified preamble buffer may be stale after an external
  -- edit. Focus snapshots must not silently overwrite the newer disk source
  -- with those old in-memory bytes.
  vim.api.nvim_set_current_buf(preamble_buf)
  vim.cmd("write")
  vim.api.nvim_set_current_buf(nested_buf)
  vim.cmd("write")
  vim.api.nvim_set_current_buf(root_buf)
  expect_page(port, { "AfterIncompleteAlpha", "KeptBeta", "CompletedBody" },
    "could not establish the external-edit baseline")
  write(preamble, { [[\newcommand{\PreviewA}{ExternalAlpha}]], [[\input{nested.sty}]] })
  expect_page(port, { "ExternalAlpha", "KeptBeta" }, "external preamble edit did not reach the viewer")
  assert(not vim.bo[preamble_buf].modified, "external-edit fixture preamble should be unmodified")
  vim.api.nvim_set_current_buf(nested_buf)
  expect_paused(port)
  change(nested_buf, { [[\newcommand{\PreviewB}{ExternalCaseBeta}]] })
  vim.api.nvim_set_current_buf(root_buf)
  baseline = expect_page(port, { "ExternalAlpha", "ExternalCaseBeta" },
    "an unmodified loaded buffer overwrote a newer preamble file on disk")
  assert(not contains(baseline, "AfterIncompleteAlpha"),
    "stale unmodified preamble text was reintroduced by the focus snapshot")

  -- Explicitly stop both owned daemons before deleting scratch source files.
  mp.stop()
  vim.api.nvim_set_current_buf(other_buf)
  mp.stop()

  -- The user can enter an input before startup's asynchronous /debug role
  -- discovery returns. Do not send that first edit through the legacy live
  -- path while the plugin still does not know that it is a preamble file.
  local startup_root = scratch .. "/startup.tex"
  local startup_preamble = scratch .. "/startup-preamble.tex"
  write(startup_root, {
    [[\documentclass{article}]],
    [[\input{startup-preamble}]],
    [[\begin{document}]],
    [[$\Bootstrap$]],
    [[\end{document}]],
  })
  write(startup_preamble, { [[\newcommand{\Bootstrap}{BootstrapOriginal}]] })
  local startup_root_buf = edit(startup_root)
  mp.start()
  local startup_preamble_buf = edit(startup_preamble)
  change(startup_preamble_buf, { [[\newcommand{\Bootstrap}{BootstrapEdited}]] })
  wait_for(function()
    return mp.status().daemon_root == startup_root and mp.status().daemon_port ~= nil
  end, "startup did not activate the project after immediate preamble focus")
  local startup_port = mp.status().daemon_port
  expect_paused(startup_port)
  local startup_page = expect_page(startup_port, { "BootstrapOriginal" },
    "startup published an edit before discovering its preamble role")
  assert(not contains(startup_page, "BootstrapEdited"), "startup leaked an early preamble edit")
  vim.api.nvim_set_current_buf(startup_root_buf)
  expect_page(startup_port, { "BootstrapEdited" }, "startup lost the queued preamble edit")

  -- A newly added input is not in the last /debug dependency set yet. Switch
  -- into it immediately, before the root's debounce and role refresh run.
  -- Once discovered it must be held as a preamble, not flushed as ordinary
  -- live body text merely because that first edit was queued while unknown.
  local late_preamble = scratch .. "/late-preamble.tex"
  write(late_preamble, { [[\newcommand{\Late}{LateOriginal}]] })
  change(startup_root_buf, {
    [[\documentclass{article}]],
    [[\input{startup-preamble}]],
    [[\input{late-preamble}]],
    [[\begin{document}]],
    [[$\Bootstrap + \Late$]],
    [[\end{document}]],
  })
  local late_buf = edit(late_preamble)
  change(late_buf, { [[\newcommand{\Late}{LateEdited}]] })
  wait_for(function()
    local state_now = debug_state(startup_port)
    return state_now and state_now.preamble_edit_paused == true
      and vim.tbl_contains(state_now.preamble_files or {}, late_preamble)
  end, "newly discovered preamble did not acquire an editing hold")
  baseline = expect_page(startup_port, { "LateOriginal" },
    "new preamble edit leaked before its dependency role was discovered")
  assert(not contains(baseline, "LateEdited"), "new preamble was flushed through the body path")
  expect_frozen(startup_port, baseline, "queued edit bypassed its newly discovered preamble role")
  vim.api.nvim_set_current_buf(startup_root_buf)
  expect_page(startup_port, { "BootstrapEdited", "LateEdited" },
    "returning from a newly discovered preamble lost its pending edit")
  mp.stop()

  -- A single preamble can belong to two live documents. Focusing it pauses
  -- both previews, but returning to one document must not release the other.
  local shared = scratch .. "/shared.tex"
  local shared_a = scratch .. "/shared-a.tex"
  local shared_b = scratch .. "/shared-b.tex"
  write(shared, { [[\newcommand{\Shared}{SharedOriginal}]] })
  for _, path in ipairs({ shared_a, shared_b }) do
    write(path, {
      [[\documentclass{article}]],
      [[\input{shared}]],
      [[\begin{document}]],
      [[$\Shared$]],
      [[\end{document}]],
    })
  end
  local shared_a_buf, shared_a_port = start(shared_a)
  local shared_a_page = expect_page(shared_a_port, { "SharedOriginal" }, "shared project A did not start")
  local shared_b_buf, shared_b_port = start(shared_b)
  local shared_b_page = expect_page(shared_b_port, { "SharedOriginal" }, "shared project B did not start")
  local shared_buf = edit(shared)
  expect_paused(shared_a_port)
  expect_paused(shared_b_port)
  change(shared_buf, { [[\newcommand{\Shared}{SharedEdited}]] })
  expect_frozen(shared_a_port, shared_a_page, "shared preamble edit changed project A while paused")
  expect_frozen(shared_b_port, shared_b_page, "shared preamble edit changed project B while paused")
  vim.api.nvim_set_current_buf(shared_a_buf)
  expect_page(shared_a_port, { "SharedEdited" }, "returning to project A did not apply its shared preamble")
  expect_frozen(shared_b_port, shared_b_page, "returning to project A incorrectly resumed project B")
  assert(debug_state(shared_b_port).preamble_edit_paused,
    "project B lost its hold before returning to its document")
  vim.api.nvim_set_current_buf(shared_b_buf)
  expect_page(shared_b_port, { "SharedEdited" }, "returning to project B lost the shared preamble edit")
  mp.stop()
  vim.api.nvim_set_current_buf(shared_a_buf)
  mp.stop()
end, debug.traceback)

-- Keep failure cleanup as strong as success cleanup. Only job IDs spawned by
-- this test are stopped, so a user's already-running preview is untouched.
pcall(vim.api.nvim_exec_autocmds, "VimLeavePre", {})
for _, job in ipairs(jobs) do pcall(vim.fn.jobstop, job) end
if #jobs > 0 then pcall(vim.fn.jobwait, jobs, 2000) end
vim.fn.jobstart = original_jobstart
vim.system = original_system
vim.env.XDG_CONFIG_HOME = original_config_home
vim.fn.delete(scratch, "rf")
if not ok then error(err, 0) end
print("mathpreview preamble-sync regression: ok")
