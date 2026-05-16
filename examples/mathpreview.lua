-- mathpreview.lua — drop in init.lua via:
--   require("mathpreview").setup({})
-- or source directly:
--   :luafile /path/to/mathpreview.lua
--
-- On TextChanged / TextChangedI it debounces the current buffer's contents
-- and POSTs them to the mathpreview daemon's /buffer endpoint. The daemon
-- re-renders and broadcasts the result over its WebSocket to every
-- connected browser tab. No disk writes; no git pollution.
--
-- CursorMoved / CursorMovedI also POST the current file/line/column to
-- /cursor so the browser can highlight the matching rendered block.
-- Double-click or Alt/Cmd-click in the browser queues a /jump entry that
-- this plugin polls and applies back in nvim.

local M = {}

local config = {
  url           = "http://127.0.0.1:23636/buffer",
  cursor_url    = "http://127.0.0.1:23636/cursor",
  jump_url      = "http://127.0.0.1:23636/jump",
  debounce_ms   = 40,
  cursor_debounce_ms = 80,
  jump_poll_ms  = 120,
  -- Buffer filetypes that should trigger pushes. `tex` covers vimtex /
  -- LaTeX-Suite filetype detection; `*.tex` pattern matching is also done
  -- as a fallback for when filetype isn't set yet.
  filetypes     = { "tex", "plaintex", "latex" },
  enabled       = true,
  sync          = true,
}

local uv = vim.uv or vim.loop  -- nvim 0.10+ uses vim.uv; older uses vim.loop
local timer = nil
local cursor_timer = nil
local jump_timer = nil
local last_jump_seq = 0

local last_status = {
  pushes = 0,
  last_push_ms = 0,
  last_error = nil,
  cursor_posts = 0,
  jumps = 0,
}

local function curl_supports_systen()
  return vim.system ~= nil
end

local function json_encode(value)
  if vim.json and vim.json.encode then return vim.json.encode(value) end
  return vim.fn.json_encode(value)
end

local function json_decode(value)
  if vim.json and vim.json.decode then return vim.json.decode(value) end
  return vim.fn.json_decode(value)
end

local function run_system(args, opts, on_done)
  opts = opts or {}
  if curl_supports_systen() then
    vim.system(args, opts, function(res)
      if on_done then vim.schedule(function() on_done(res) end) end
    end)
    return
  end

  local stdout = {}
  local stderr = {}
  local job = vim.fn.jobstart(args, {
    on_stdout = function(_, data)
      if data then vim.list_extend(stdout, data) end
    end,
    on_stderr = function(_, data)
      if data then vim.list_extend(stderr, data) end
    end,
    on_exit = function(_, code)
      if on_done then
        on_done({
          code = code,
          stdout = table.concat(stdout, "\n"),
          stderr = table.concat(stderr, "\n"),
        })
      end
    end,
  })
  if job <= 0 then
    if on_done then on_done({ code = -1, stderr = "could not start curl" }) end
    return
  end
  if opts.stdin then
    vim.fn.chansend(job, opts.stdin)
    vim.fn.chanclose(job, "stdin")
  end
end

local function push_buffer()
  local buf = vim.api.nvim_get_current_buf()
  local path = vim.api.nvim_buf_get_name(buf)
  if path == "" then
    last_status.last_error = "current buffer has no name"
    return
  end
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  local body = table.concat(lines, "\n")
  local args = {
    "curl",
    "--silent",
    "--show-error",
    "--max-time", "5",
    "--header", "X-Mathpreview-Path: " .. path,
    "--data-binary", "@-",
    "-X", "POST",
    config.url,
  }
  local on_done = function(res)
    if res and res.code ~= 0 then
      last_status.last_error = ("curl exit %d: %s"):format(res.code or -1, (res.stderr or ""):gsub("%s+$", ""))
    else
      last_status.last_error = nil
    end
  end
  run_system(args, { stdin = body }, on_done)
  last_status.pushes = last_status.pushes + 1
  last_status.last_push_ms = uv.hrtime() / 1e6
end

local function post_cursor()
  if not config.enabled or not config.sync then return end
  local buf = vim.api.nvim_get_current_buf()
  local path = vim.api.nvim_buf_get_name(buf)
  if path == "" then return end
  local cursor = vim.api.nvim_win_get_cursor(0)
  local payload = json_encode({
    file = path,
    line = cursor[1],
    col = cursor[2] + 1,
  })
  local args = {
    "curl",
    "--silent",
    "--show-error",
    "--max-time", "2",
    "--header", "content-type: application/json",
    "--data-binary", "@-",
    "-X", "POST",
    config.cursor_url,
  }
  run_system(args, { stdin = payload }, function(res)
    if res and res.code ~= 0 then
      last_status.last_error = ("cursor curl exit %d: %s"):format(res.code or -1, (res.stderr or ""):gsub("%s+$", ""))
    else
      last_status.last_error = nil
    end
  end)
  last_status.cursor_posts = last_status.cursor_posts + 1
end

local function debounced_cursor()
  if not config.enabled or not config.sync then return end
  if cursor_timer then cursor_timer:stop(); cursor_timer:close() end
  cursor_timer = uv.new_timer()
  cursor_timer:start(config.cursor_debounce_ms, 0, vim.schedule_wrap(function()
    post_cursor()
    if cursor_timer then cursor_timer:close(); cursor_timer = nil end
  end))
end

local function jump_to_source(jump)
  if type(jump) ~= "table" or not jump.file or not jump.line then return end
  local seq = tonumber(jump.seq) or 0
  if seq <= last_jump_seq then return end
  last_jump_seq = seq
  local file = tostring(jump.file)
  local line = math.max(1, tonumber(jump.line) or 1)
  local col = math.max(0, (tonumber(jump.col) or 1) - 1)
  if vim.api.nvim_buf_get_name(0) ~= file then
    vim.cmd("edit " .. vim.fn.fnameescape(file))
  end
  local line_count = vim.api.nvim_buf_line_count(0)
  line = math.min(line, math.max(1, line_count))
  local line_text = vim.api.nvim_buf_get_lines(0, line - 1, line, false)[1] or ""
  col = math.min(col, #line_text)
  vim.api.nvim_win_set_cursor(0, { line, col })
  vim.cmd("normal! zz")
  last_status.jumps = last_status.jumps + 1
end

local function poll_jump()
  if not config.enabled or not config.sync then return end
  local args = {
    "curl",
    "--silent",
    "--show-error",
    "--max-time", "2",
    config.jump_url .. "?after=" .. tostring(last_jump_seq),
  }
  run_system(args, {}, function(res)
    if not res or res.code ~= 0 then return end
    local body = (res.stdout or ""):gsub("^%s+", ""):gsub("%s+$", "")
    if body == "" then return end
    local ok, decoded = pcall(json_decode, body)
    if ok then jump_to_source(decoded) end
  end)
end

local function start_jump_poll()
  if jump_timer then jump_timer:stop(); jump_timer:close(); jump_timer = nil end
  if not config.sync then return end
  jump_timer = uv.new_timer()
  jump_timer:start(config.jump_poll_ms, config.jump_poll_ms, vim.schedule_wrap(poll_jump))
end

local function debounced_push()
  if not config.enabled then return end
  if timer then timer:stop(); timer:close() end
  timer = uv.new_timer()
  timer:start(config.debounce_ms, 0, vim.schedule_wrap(function()
    push_buffer()
    if timer then timer:close(); timer = nil end
  end))
end

function M.push()        push_buffer()        end
function M.sync_cursor() post_cursor()        end
function M.enable()      config.enabled = true  end
function M.disable()     config.enabled = false end

function M.status()
  local age = uv.hrtime() / 1e6 - last_status.last_push_ms
  local autocmds = {}
  pcall(function()
    autocmds = vim.api.nvim_get_autocmds({ group = "mathpreview" }) or {}
  end)
  local buf = vim.api.nvim_get_current_buf()
  return {
    enabled        = config.enabled,
    url            = config.url,
    cursor_url     = config.cursor_url,
    jump_url       = config.jump_url,
    debounce_ms    = config.debounce_ms,
    cursor_debounce_ms = config.cursor_debounce_ms,
    jump_poll_ms   = config.jump_poll_ms,
    sync           = config.sync,
    filetypes      = config.filetypes,
    current_ft     = vim.bo[buf].filetype,
    current_path   = vim.api.nvim_buf_get_name(buf),
    pushes         = last_status.pushes,
    cursor_posts   = last_status.cursor_posts,
    jumps          = last_status.jumps,
    last_jump_seq  = last_jump_seq,
    last_push_ago_ms = (last_status.last_push_ms > 0) and math.floor(age) or nil,
    last_error     = last_status.last_error,
    autocmds_count = #autocmds,
    nvim_version   = vim.version() and (vim.version().major .. "." .. vim.version().minor) or "?",
  }
end

function M.setup(opts)
  opts = opts or {}
  config = vim.tbl_extend("force", config, opts)

  vim.api.nvim_create_augroup("mathpreview", { clear = true })
  vim.api.nvim_create_autocmd({ "TextChanged", "TextChangedI" }, {
    group = "mathpreview",
    pattern = "*",
    callback = function(args)
      if vim.tbl_contains(config.filetypes, vim.bo[args.buf].filetype) then
        debounced_push()
      end
    end,
  })
  vim.api.nvim_create_autocmd({ "CursorMoved", "CursorMovedI" }, {
    group = "mathpreview",
    pattern = "*",
    callback = function(args)
      if vim.tbl_contains(config.filetypes, vim.bo[args.buf].filetype) then
        debounced_cursor()
      end
    end,
  })

  vim.api.nvim_create_user_command("MathpreviewPush",   function() M.push()    end, {})
  vim.api.nvim_create_user_command("MathpreviewSync",   function() M.sync_cursor() end, {})
  vim.api.nvim_create_user_command("MathpreviewEnable", function() M.enable()  end, {})
  vim.api.nvim_create_user_command("MathpreviewDisable",function() M.disable() end, {})
  vim.api.nvim_create_user_command("MathpreviewStatus", function()
    print(vim.inspect(M.status()))
  end, {})

  start_jump_poll()
end

-- Auto-setup when sourced directly (`:luafile`).
M.setup({})

return M
