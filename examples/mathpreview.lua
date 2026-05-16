-- mathpreview.lua — drop in init.lua via:
--   require("mathpreview").setup({})
-- or source directly:
--   :luafile /path/to/mathpreview.lua
--
-- On TextChanged / TextChangedI it debounces the current buffer's contents
-- and POSTs them to the mathpreview daemon's /buffer endpoint. The daemon
-- re-renders and broadcasts the result over its WebSocket to every
-- connected browser tab. No disk writes; no git pollution.

local M = {}

local config = {
  url           = "http://127.0.0.1:23636/buffer",
  debounce_ms   = 40,
  -- Buffer filetypes that should trigger pushes. `tex` covers vimtex /
  -- LaTeX-Suite filetype detection; `*.tex` pattern matching is also done
  -- as a fallback for when filetype isn't set yet.
  filetypes     = { "tex", "plaintex", "latex" },
  enabled       = true,
}

local uv = vim.uv or vim.loop  -- nvim 0.10+ uses vim.uv; older uses vim.loop
local timer = nil

local last_status = {
  pushes = 0,
  last_push_ms = 0,
  last_error = nil,
}

local function curl_supports_systen()
  return vim.system ~= nil
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
  if curl_supports_systen() then
    vim.system(args, { stdin = body }, function(res) vim.schedule(function() on_done(res) end) end)
  else
    -- nvim < 0.10 path: jobstart + chansend.
    local job = vim.fn.jobstart(args, {
      on_stderr = function(_, data)
        if data and #data > 0 then
          local err = table.concat(data, "\n"):gsub("%s+$", "")
          if err ~= "" then last_status.last_error = err end
        end
      end,
      on_exit = function(_, code)
        if code ~= 0 then last_status.last_error = ("curl exit %d"):format(code)
        else last_status.last_error = nil end
      end,
    })
    if job <= 0 then
      last_status.last_error = "could not start curl"
      return
    end
    vim.fn.chansend(job, body)
    vim.fn.chanclose(job, "stdin")
  end
  last_status.pushes = last_status.pushes + 1
  last_status.last_push_ms = uv.hrtime() / 1e6
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
    debounce_ms    = config.debounce_ms,
    filetypes      = config.filetypes,
    current_ft     = vim.bo[buf].filetype,
    current_path   = vim.api.nvim_buf_get_name(buf),
    pushes         = last_status.pushes,
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

  vim.api.nvim_create_user_command("MathpreviewPush",   function() M.push()    end, {})
  vim.api.nvim_create_user_command("MathpreviewEnable", function() M.enable()  end, {})
  vim.api.nvim_create_user_command("MathpreviewDisable",function() M.disable() end, {})
  vim.api.nvim_create_user_command("MathpreviewStatus", function()
    print(vim.inspect(M.status()))
  end, {})
end

-- Auto-setup when sourced directly (`:luafile`).
M.setup({})

return M
