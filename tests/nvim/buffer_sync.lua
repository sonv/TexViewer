-- Headless integration regression for editor-buffer routing.
-- Run from the repository root after `cargo build`:
--   MATHPREVIEW_CLI=target/debug/mathpreview-cli \
--     nvim --headless -u NONE -i NONE -l tests/nvim/buffer_sync.lua

local repo = vim.fn.getcwd()
vim.opt.runtimepath:prepend(repo)
vim.o.hidden = true

local cli = vim.env.MATHPREVIEW_CLI
if not cli or cli == "" then cli = repo .. "/target/debug/mathpreview-cli" end
cli = vim.fn.fnamemodify(cli, ":p")
assert(vim.fn.executable(cli) == 1, "build mathpreview-cli before running this test")

local scratch = vim.fn.tempname() .. "-mathpreview-buffer-sync"
assert(vim.fn.mkdir(scratch, "p") == 1, "could not create scratch directory")

local function write(path, lines)
  assert(vim.fn.writefile(lines, path) == 0, "could not write " .. path)
end

local startup_path = scratch .. "/startup.md"
local unsaved_tex_path = scratch .. "/never-saved.tex"
local a_path = scratch .. "/a.md"
local b_path = scratch .. "/b.md"
local canonical_md_path = scratch .. "/canonical.md"
local alias_md_path = scratch .. "/alias.md"
write(startup_path, { "# disk startup" })
write(a_path, { "# A disk" })
write(b_path, { "# B disk" })
write(canonical_md_path, {
  "# Canonical disk file",
  "",
  "canonical disk target",
})
local symlink_ok, symlink_err = (vim.uv or vim.loop).fs_symlink(
  canonical_md_path, alias_md_path)
assert(symlink_ok, "could not create Markdown symlink: " .. tostring(symlink_err))

local mp = require("mathpreview")
mp.setup({
  cmd = cli,
  auto_open_browser = false,
  stale_check = false,
  sync = false,
  close_on_exit = true,
  debounce_ms = 250,
})

local function curl(port, route)
  local result = vim.system({
    "curl", "--silent", "--max-time", "1",
    "http://127.0.0.1:" .. tostring(port) .. route,
  }, { text = true }):wait()
  return result.code, result.stdout or ""
end

local function post_json(port, route, payload)
  local result = vim.system({
    "curl", "--silent", "--show-error", "--max-time", "2",
    "--header", "content-type: application/json",
    "--data-binary", "@-",
    "-X", "POST",
    "http://127.0.0.1:" .. tostring(port) .. route,
  }, { text = true, stdin = vim.json.encode(payload) }):wait()
  return result.code, result.stdout or "", result.stderr or ""
end

local function wait_for(predicate, message)
  assert(vim.wait(5000, predicate, 50),
    message .. "\nstatus: " .. vim.inspect(mp.status()))
end

local function start(path)
  vim.cmd("edit " .. vim.fn.fnameescape(path))
  vim.bo.filetype = "markdown"
  local buf = vim.api.nvim_get_current_buf()
  mp.start()
  wait_for(function() return mp.status().daemon_port ~= nil end,
    "daemon did not register for " .. path)
  local port = mp.status().daemon_port
  wait_for(function() return curl(port, "/debug") == 0 end,
    "daemon did not bind for " .. path)
  return buf, port
end

-- Existing files may already be modified before :MathPreview attaches its
-- TextChanged autocmd. Startup must push this captured buffer, not disk bytes.
vim.cmd("edit " .. vim.fn.fnameescape(startup_path))
vim.bo.filetype = "markdown"
local startup_buf = vim.api.nvim_get_current_buf()
vim.api.nvim_buf_set_lines(startup_buf, 0, -1, false,
  { "# modified before start", "", "startup-buffer-sentinel" })
mp.start()
vim.cmd("enew")
local bystander_buf = vim.api.nvim_get_current_buf()
wait_for(function() return mp.status().daemon_port ~= nil end,
  "startup daemon did not register")
local startup_port = mp.status().daemon_port
wait_for(function()
  local code, page = curl(startup_port, "/")
  return code == 0 and page:find("startup%-buffer%-sentinel") ~= nil
end, "startup served disk text instead of the modified buffer")

-- Restart has its own async gap. It must retain the same authoritative buffer
-- even if the replacement daemon re-reads the older file from disk first.
vim.api.nvim_buf_set_lines(startup_buf, 0, -1, false,
  { "# modified before restart", "", "restart-buffer-sentinel" })
mp.restart()
vim.api.nvim_set_current_buf(bystander_buf)
local restart_port = startup_port -- restart deliberately rebinds the open tab's port
wait_for(function()
  local code, page = curl(restart_port, "/")
  return code == 0 and page:find("restart%-buffer%-sentinel") ~= nil
end, "restart served disk text instead of the modified buffer")
vim.api.nvim_set_current_buf(startup_buf)
wait_for(function() return mp.status().daemon_port == restart_port end,
  "replacement daemon did not reactivate on returning to its buffer")
mp.stop()

-- Preserve the pre-existing TeX behavior for a named buffer whose file does
-- not exist yet: the placeholder daemon must receive the editor snapshot.
vim.cmd("edit " .. vim.fn.fnameescape(unsaved_tex_path))
vim.bo.filetype = "tex"
local unsaved_tex_buf = vim.api.nvim_get_current_buf()
vim.api.nvim_buf_set_lines(unsaved_tex_buf, 0, -1, false, {
  [[\documentclass{article}]],
  [[\begin{document}]],
  "unsaved-tex-sentinel",
  [[\end{document}]],
})
mp.start()
wait_for(function() return mp.status().daemon_port ~= nil end,
  "never-saved TeX daemon did not register")
local unsaved_tex_port = mp.status().daemon_port
wait_for(function()
  local code, page = curl(unsaved_tex_port, "/")
  return code == 0 and page:find("unsaved%-tex%-sentinel") ~= nil
end, "never-saved TeX buffer was not pushed after startup")
mp.stop()

-- A single global debounce used to let B cancel A, and its delayed callback
-- read whichever buffer/daemon happened to be current. Both captured buffers
-- must now reach their own daemons after a rapid A -> B switch.
local a_buf, a_port = start(a_path)
local b_buf, b_port = start(b_path)
vim.api.nvim_set_current_buf(a_buf)
vim.api.nvim_buf_set_lines(a_buf, 0, -1, false,
  { "# A changed", "", "buffer-a-sentinel" })
vim.api.nvim_exec_autocmds("TextChanged", { buffer = a_buf })
vim.api.nvim_set_current_buf(b_buf)
vim.api.nvim_buf_set_lines(b_buf, 0, -1, false,
  { "# B changed", "", "buffer-b-sentinel" })
vim.api.nvim_exec_autocmds("TextChanged", { buffer = b_buf })

wait_for(function()
  local code, page = curl(a_port, "/")
  return code == 0 and page:find("buffer%-a%-sentinel") ~= nil
end, "buffer A update was cancelled or routed to another daemon")
wait_for(function()
  local code, page = curl(b_port, "/")
  return code == 0 and page:find("buffer%-b%-sentinel") ~= nil
end, "buffer B update was cancelled or routed to another daemon")

vim.api.nvim_set_current_buf(a_buf)
mp.stop()
vim.api.nvim_set_current_buf(b_buf)
mp.stop()

-- The Markdown daemon canonicalizes source paths, but users may have opened a
-- symlink alias and made unsaved edits there. A backward-sync jump must treat
-- the daemon's canonical target as the current alias buffer: opening a second
-- buffer for the real path loses the authoritative in-memory view.
mp.setup({
  sync = true,
  raise_on_jump = false,
  cursor_debounce_ms = 30,
  jump_wait_ms = 1000,
  jump_retry_ms = 10,
})
local alias_buf, alias_port = start(alias_md_path)
local alias_buf_name = vim.api.nvim_buf_get_name(alias_buf)
local alias_lines = {
  "# Unsaved alias buffer",
  "",
  "alias-buffer-target",
}
vim.api.nvim_buf_set_lines(alias_buf, 0, -1, false, alias_lines)
vim.api.nvim_exec_autocmds("TextChanged", { buffer = alias_buf })
wait_for(function()
  local code, page = curl(alias_port, "/")
  return code == 0 and page:find("alias%-buffer%-target") ~= nil
end, "symlinked Markdown buffer was not pushed before the jump")
assert(vim.bo[alias_buf].modified, "symlink alias should still be unsaved before the jump")

local jumps_before = mp.status().jumps
local jump_code, _, jump_err = post_json(alias_port, "/jump", {
  file = vim.fn.resolve(canonical_md_path),
  line = 3,
  col = 7,
})
assert(jump_code == 0, "could not queue canonical Markdown jump: " .. jump_err)
wait_for(function() return mp.status().jumps > jumps_before end,
  "plugin did not consume the canonical Markdown jump")
assert(vim.api.nvim_get_current_buf() == alias_buf,
  "canonical Markdown jump replaced the symlink alias buffer")
assert(vim.api.nvim_buf_get_name(alias_buf) == alias_buf_name,
  "canonical Markdown jump renamed the symlink alias buffer")
assert(vim.deep_equal(vim.api.nvim_buf_get_lines(alias_buf, 0, -1, false), alias_lines),
  "canonical Markdown jump discarded unsaved alias contents")
assert(vim.bo[alias_buf].modified,
  "canonical Markdown jump cleared the alias buffer's modified state")
assert(vim.deep_equal(vim.api.nvim_win_get_cursor(0), { 3, 6 }),
  "canonical Markdown jump did not land at the requested source position")

-- The alias may be loaded but not focused when the browser requests a jump.
-- Reuse it in that case too instead of opening a second canonical-path buffer.
local unrelated_buf = vim.api.nvim_create_buf(true, false)
vim.api.nvim_buf_set_lines(unrelated_buf, 0, -1, false, { "unrelated buffer" })
vim.api.nvim_set_current_buf(unrelated_buf)
assert(vim.api.nvim_get_current_buf() == unrelated_buf,
  "could not leave the alias buffer before the loaded-alias jump")
jumps_before = mp.status().jumps
jump_code, _, jump_err = post_json(alias_port, "/jump", {
  file = vim.fn.resolve(canonical_md_path),
  line = 3,
  col = 8,
})
assert(jump_code == 0, "could not queue loaded-alias Markdown jump: " .. jump_err)
wait_for(function() return mp.status().jumps > jumps_before end,
  "plugin did not consume the loaded-alias Markdown jump")
assert(vim.api.nvim_get_current_buf() == alias_buf,
  "canonical Markdown jump ignored the loaded symlink alias buffer")
assert(vim.deep_equal(vim.api.nvim_buf_get_lines(alias_buf, 0, -1, false), alias_lines),
  "loaded-alias Markdown jump discarded unsaved contents")
assert(vim.bo[alias_buf].modified,
  "loaded-alias Markdown jump cleared the alias buffer's modified state")
assert(vim.deep_equal(vim.api.nvim_win_get_cursor(0), { 3, 7 }),
  "loaded-alias Markdown jump did not land at the requested source position")

-- If both names are loaded, an unmodified canonical buffer must not outrank
-- the modified alias that contains the preview's authoritative contents.
-- Neovim normally deduplicates symlink identities while adding a buffer, so
-- briefly remove and restore this disposable test symlink to reproduce the
-- state that can arise when a symlink is created or retargeted after loading.
local unlink_ok, unlink_err = (vim.uv or vim.loop).fs_unlink(alias_md_path)
assert(unlink_ok, "could not remove the Markdown test symlink: " .. tostring(unlink_err))
local canonical_buf = vim.fn.bufadd(canonical_md_path)
vim.fn.bufload(canonical_buf)
assert(not vim.bo[canonical_buf].modified,
  "canonical duplicate should start unmodified")
local relink_ok, relink_err = (vim.uv or vim.loop).fs_symlink(
  canonical_md_path, alias_md_path)
assert(relink_ok, "could not restore the Markdown test symlink: " .. tostring(relink_err))
vim.api.nvim_set_current_buf(canonical_buf)
assert(vim.api.nvim_get_current_buf() == canonical_buf,
  "could not focus the canonical duplicate before the alias-preference jump")
jumps_before = mp.status().jumps
jump_code, _, jump_err = post_json(alias_port, "/jump", {
  file = vim.fn.resolve(canonical_md_path),
  line = 3,
  col = 9,
})
assert(jump_code == 0, "could not queue alias-preference Markdown jump: " .. jump_err)
wait_for(function() return mp.status().jumps > jumps_before end,
  "plugin did not consume the alias-preference Markdown jump")
assert(vim.api.nvim_get_current_buf() == alias_buf,
  "canonical Markdown jump preferred a stale duplicate over the modified alias")
assert(vim.deep_equal(vim.api.nvim_buf_get_lines(alias_buf, 0, -1, false), alias_lines),
  "alias-preference Markdown jump discarded unsaved contents")
assert(vim.deep_equal(vim.api.nvim_win_get_cursor(0), { 3, 8 }),
  "alias-preference Markdown jump did not land at the requested source position")

-- A uv timer can fire (queueing its schedule_wrap callback) immediately before
-- a newer CursorMoved replaces it. Replaying that precise ordering verifies
-- that the stale callback neither posts nor closes the replacement timer. The
-- cursor and visual-selection debouncers are separate, so exercise both.
local function assert_stale_debounce_is_ignored(label, route, trigger)
  local original_schedule_wrap = vim.schedule_wrap
  local original_system = vim.system
  local queued = {}
  local posts = 0

  vim.schedule_wrap = function(callback)
    return function(...)
      queued[#queued + 1] = { callback = callback, args = { ... } }
    end
  end
  vim.system = function(args, opts, on_exit)
    local url = args[#args]
    if type(url) == "string" and url:sub(-#route) == route then
      posts = posts + 1
      return nil
    end
    return original_system(args, opts, on_exit)
  end

  local ok, err = xpcall(function()
    trigger()
    assert(vim.wait(1000, function() return #queued >= 1 end, 5),
      label .. " timer did not fire")
    trigger() -- replace the fired timer before its scheduled callback runs
    queued[1].callback(unpack(queued[1].args))
    assert(posts == 0, label .. " stale callback posted an obsolete update")
    assert(vim.wait(1000, function() return #queued >= 2 end, 5),
      label .. " stale callback closed the replacement timer")
    queued[2].callback(unpack(queued[2].args))
    assert(posts == 1, label .. " replacement callback did not post exactly once")
  end, debug.traceback)

  vim.schedule_wrap = original_schedule_wrap
  vim.system = original_system
  if not ok then error(err, 0) end
end

local function fire_cursor_moved()
  vim.api.nvim_exec_autocmds("CursorMoved", { buffer = alias_buf })
end

assert_stale_debounce_is_ignored("cursor debounce", "/cursor", fire_cursor_moved)

vim.cmd("normal! v")
assert(vim.fn.mode():sub(1, 1) == "v", "could not enter visual mode for debounce test")
vim.wait(100, function() return false end, 10) -- settle the ModeChanged seed post
assert_stale_debounce_is_ignored("selection debounce", "/selection", fire_cursor_moved)
local escape = vim.api.nvim_replace_termcodes("<Esc>", true, false, true)
vim.api.nvim_feedkeys(escape, "nx", false)
wait_for(function() return vim.fn.mode() == "n" end,
  "could not leave visual mode after debounce test")

mp.stop()
vim.fn.delete(scratch, "rf")
print("mathpreview buffer-sync regression: ok")
