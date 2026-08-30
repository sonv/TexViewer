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
write(startup_path, { "# disk startup" })
write(a_path, { "# A disk" })
write(b_path, { "# B disk" })

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

local function wait_for(predicate, message)
  assert(vim.wait(5000, predicate, 50), message)
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
vim.fn.delete(scratch, "rf")
print("mathpreview buffer-sync regression: ok")
