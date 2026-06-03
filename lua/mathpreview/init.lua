-- mathpreview.nvim — companion plugin for the `mathpreview-cli` binary.
--
-- Drop into your plugin manager (lazy / packer / vim-plug) by pointing at
-- this repo. The user-facing commands are registered by plugin/mathpreview.lua;
-- this file holds the implementation that those commands lazy-require:
--
--     :MathPreview         start the daemon for the current .tex buffer
--                          and open the browser tab
--     :MathPreviewStop     kill the daemon
--     :MathPreviewRestart  stop + start
--     :MathPreviewStatus   echo PID, port, push counters, version handshake
--     :MathPreviewDebug    echo resolved settings + config/macro paths
--
-- The plugin spawns the daemon as a background job, finds the first free
-- port starting at 23636, opens the browser, then debounces buffer pushes
-- on TextChanged. VimLeavePre kills the spawned daemon so quitting nvim
-- doesn't leave a stray server bound to the port.

local M = {}

local DEFAULT_PORT = 23636
local PORT_SCAN_RANGE = 16  -- try 23636..23651 before giving up

-- Version this plugin checkout expects the `mathpreview-cli` binary to be.
-- On :MathPreview we compare it against the binary's `--version`; if this is
-- a source checkout with cargo, a stale/missing binary is (re)installed to
-- match (see ensure_binary). Otherwise we warn once on mismatch — the signal
-- that a fix you "released" isn't actually the binary you're running.
-- RELEASE: bump this in lockstep with Cargo.toml / Cargo.lock / CHANGELOG.
local PLUGIN_VERSION = "0.1.34"

local config = {
  cmd = nil,                              -- resolved at start; "mathpreview-cli" by default
  filetypes = { "tex", "plaintex", "latex" },
  debounce_ms = 40,
  cursor_debounce_ms = 80,
  jump_poll_ms = 120,
  sync = true,
  auto_open_browser = true,
  -- Where `cargo install` drops the auto-built binary. nil → cargo's default
  -- (`$CARGO_HOME/bin`, usually `~/.cargo/bin`, which rustup puts on $PATH).
  -- Set to a prefix like "~/.local" to install to "~/.local/bin/mathpreview-cli"
  -- instead (passed to `cargo install --root`). If the resulting bin dir isn't
  -- on your $PATH, the plugin still runs the binary by absolute path and warns
  -- you once with the line to add.
  install_root = nil,
  -- Override the MathJax bundle URL the daemon serves to the browser.
  -- nil → use the vendored, embedded `/vendor/mathjax/tex-svg.js` (works
  -- offline, default). Set to a CDN like
  -- `https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js` to skip the
  -- embedded bundle and load MathJax over the network — useful if you
  -- want to pull a newer MathJax release without rebuilding the binary,
  -- or if you're behind a corporate proxy that caches CDN assets.
  mathjax_url = nil,
  -- Shell command the daemon runs for browser "reveal source" clicks
  -- (`POST /reveal-source`). `{file}`, `{line}`, `{col}` are substituted.
  -- nil → auto: when `sync` is on the plugin DISABLES the spawn (empty
  -- string) because the polled `/jump` already navigates in place; when
  -- `sync` is off it passes a command targeting THIS nvim via
  -- `v:servername`. Set a string to force a specific command regardless
  -- (e.g. `code -g {file}:{line}:{col}`), or `""` to always disable it.
  editor = nil,
  -- Per-session URLs, written when start_daemon() picks a port.
  url = nil,        -- http://127.0.0.1:<port>/buffer
  cursor_url = nil, -- http://127.0.0.1:<port>/cursor
  jump_url = nil,   -- http://127.0.0.1:<port>/jump
  debug_url = nil,  -- http://127.0.0.1:<port>/debug
}

local uv = vim.uv or vim.loop

local daemon_job = nil   -- jobid of the running daemon, or nil
local daemon_port = nil  -- port the daemon bound, or nil
local daemon_root = nil  -- root .tex file path the daemon serves, or nil

-- Push state (carried across pushes for :MathPreviewStatus).
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
  binary_version = nil,  -- filled by check_binary_version() on first start
}

-- Warn at most once per nvim session, even across restarts, so a stale
-- binary nags you once rather than on every :MathPreviewRestart.
local version_warned = false

-- Root of this plugin's checkout. init.lua lives at
-- <root>/lua/mathpreview/init.lua, so three `:h` heads climb back to <root>.
local function plugin_root()
  local src = debug.getinfo(1, "S").source
  if src:sub(1, 1) == "@" then src = src:sub(2) end
  return vim.fn.fnamemodify(src, ":h:h:h")
end

local function exe_name()
  return (vim.fn.has("win32") == 1) and "mathpreview-cli.exe" or "mathpreview-cli"
end

-- Directory `cargo install` drops the binary in. Honors `install_root`
-- (→ "<root>/bin"), then $CARGO_HOME/bin, then the ~/.cargo/bin default.
local function cargo_bin_dir()
  if config.install_root and config.install_root ~= "" then
    return vim.fn.expand(config.install_root) .. "/bin"
  end
  local cargo_home = vim.env.CARGO_HOME
  if cargo_home and cargo_home ~= "" then
    return cargo_home .. "/bin"
  end
  return vim.fn.expand("~/.cargo/bin")
end

-- Absolute path of the `cargo install`-ed binary (whether or not its dir is
-- on $PATH). This is the primary location the plugin runs from.
local function installed_binary_path()
  return cargo_bin_dir() .. "/" .. exe_name()
end

-- `cargo build --release` artifact inside this checkout. Kept only as a
-- last-resort fallback for contributors who `cargo build` without installing.
local function bundled_binary_path()
  return plugin_root() .. "/target/release/" .. exe_name()
end

-- The `cargo install` command used to (re)install the binary.
local function cargo_install_args()
  local args = { "cargo", "install", "--path", "crates/cli", "--force" }
  if config.install_root and config.install_root ~= "" then
    table.insert(args, "--root")
    table.insert(args, vim.fn.expand(config.install_root))
  end
  return args
end

-- True if this checkout has the Rust sources we'd need to compile the binary
-- (i.e. it's a git/source install, not a binary-only drop).
local function is_source_checkout()
  return vim.fn.isdirectory(plugin_root() .. "/crates/cli") == 1
end

local function resolve_cmd()
  -- Precedence: explicit override > cargo-installed binary (by absolute path,
  -- so it works even when its dir isn't on $PATH) > whatever `mathpreview-cli`
  -- is on $PATH > in-checkout build. The installed binary outranks $PATH/
  -- in-checkout so a fresh install can't be shadowed by a stale leftover.
  if config.cmd and config.cmd ~= "" then return config.cmd end
  local installed = installed_binary_path()
  if vim.fn.executable(installed) == 1 then return installed end
  if vim.fn.executable("mathpreview-cli") == 1 then return "mathpreview-cli" end
  local bundled = bundled_binary_path()
  if vim.fn.executable(bundled) == 1 then return bundled end
  return nil
end

-- True if `cmd` is a binary the plugin installs/builds itself (so it's safe
-- to reinstall on skew). A user-supplied `cmd` or an unrelated $PATH binary
-- is left alone.
local function is_managed(cmd)
  return cmd == installed_binary_path() or cmd == bundled_binary_path()
end

local function json_encode(value)
  if vim.json and vim.json.encode then return vim.json.encode(value) end
  return vim.fn.json_encode(value)
end

local function json_decode(s)
  if vim.json and vim.json.decode then return vim.json.decode(s) end
  return vim.fn.json_decode(s)
end

local function json_decode(value)
  if vim.json and vim.json.decode then return vim.json.decode(value) end
  return vim.fn.json_decode(value)
end

-- Run a shell command. Uses vim.system on nvim 0.10+; falls back to
-- jobstart on older builds. Always async; on_done is called on the main
-- thread with { code, stdout, stderr }.
local function run_system(args, opts, on_done)
  opts = opts or {}
  if vim.system then
    vim.system(args, opts, function(res)
      if on_done then vim.schedule(function() on_done(res) end) end
    end)
    return
  end
  local stdout, stderr = {}, {}
  local job = vim.fn.jobstart(args, {
    cwd = opts.cwd,  -- nil → inherit nvim's cwd
    on_stdout = function(_, data) if data then vim.list_extend(stdout, data) end end,
    on_stderr = function(_, data) if data then vim.list_extend(stderr, data) end end,
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

-- Parse "0.1.27" → { 0, 1, 27 }; returns nil on anything unexpected so the
-- caller falls back to a plain string compare.
local function parse_semver(s)
  local maj, min, pat = tostring(s):match("^(%d+)%.(%d+)%.(%d+)")
  if not maj then return nil end
  return { tonumber(maj), tonumber(min), tonumber(pat) }
end

-- -1 / 0 / 1 for a<b / a==b / a>b. Falls back to string compare when either
-- side doesn't parse as semver (e.g. a "-dev" suffix).
local function semver_cmp(a, b)
  local pa, pb = parse_semver(a), parse_semver(b)
  if not pa or not pb then
    if a == b then return 0 end
    return (a < b) and -1 or 1
  end
  for i = 1, 3 do
    if pa[i] ~= pb[i] then return (pa[i] < pb[i]) and -1 or 1 end
  end
  return 0
end

-- Run `cmd --version` async, record the version for :MathPreviewStatus, and
-- vim.notify once if it doesn't match PLUGIN_VERSION. Non-blocking: start
-- never waits on this. The binary and plugin install separately, so a
-- mismatch means the running binary predates (or postdates) this checkout —
-- the usual reason a "released" fix isn't the code actually executing.
local function check_binary_version(cmd)
  run_system({ cmd, "--version" }, {}, function(res)
    if not res or res.code ~= 0 or not res.stdout then return end
    -- clap prints "mathpreview-cli 0.1.27"; take the last whitespace token.
    local ver = res.stdout:gsub("%s+$", ""):match("(%S+)%s*$")
    if not ver then return end
    last_status.binary_version = ver
    if version_warned or semver_cmp(ver, PLUGIN_VERSION) == 0 then return end
    version_warned = true
    local rel = semver_cmp(ver, PLUGIN_VERSION) < 0 and "older than" or "newer than"
    vim.notify(
      ("mathpreview: binary is %s (%s the plugin's expected %s). Reinstall it "
        .. "(from this checkout: `cargo install --path crates/cli --force`) and "
        .. ":MathPreviewRestart so fixes match."):format(ver, rel, PLUGIN_VERSION),
      vim.log.levels.WARN)
  end)
end

-- Guards against overlapping installs (e.g. a second :MathPreview while the
-- first is still compiling).
local installing = false
-- Warn at most once per session about the install dir not being on $PATH.
local path_hinted = false

-- If the cargo bin dir isn't on $PATH, tell the user how to add it (the
-- binary still runs via its absolute path; this is for terminal use). Standard
-- fix is to put the dir on PATH in the shell profile.
local function hint_path_if_needed()
  if path_hinted or vim.fn.executable("mathpreview-cli") == 1 then
    return
  end
  path_hinted = true
  local dir = cargo_bin_dir()
  vim.notify(
    ("mathpreview: installed to %s, which isn't on your $PATH.\n"
      .. "The plugin runs it by absolute path, but for terminal use add it to "
      .. "your shell profile:\n  export PATH=\"%s:$PATH\"\n"
      .. "(or set `install_root` / `cmd` in setup()).") :format(dir, dir),
    vim.log.levels.WARN)
end

-- `cargo install` mathpreview-cli (builds + drops it in the cargo bin dir),
-- async. Notifies on start (so the user waits out the one-time ~30s build) and
-- on finish, then calls on_done(ok) on the main thread. Assumes the caller has
-- confirmed this is a source checkout with cargo available.
local function auto_install(on_done)
  if installing then return end  -- one already in flight; its callback will start us
  installing = true
  vim.notify(
    ("mathpreview: installing mathpreview-cli to %s (first run, ~30s) — please "
      .. "wait, :MathPreview will start automatically when it finishes…")
      :format(cargo_bin_dir()),
    vim.log.levels.INFO)
  run_system(cargo_install_args(), { cwd = plugin_root() }, function(res)
    installing = false
    local ok = res and res.code == 0
    if ok then
      vim.notify("mathpreview: install complete — starting daemon.", vim.log.levels.INFO)
      hint_path_if_needed()
    else
      vim.notify(
        ("mathpreview: install failed (cargo exit %s). See :messages; you can also "
          .. "install the binary manually (README) or set `cmd` in setup().\n%s"):format(
          res and res.code or "?", (res and res.stderr or ""):gsub("%s+$", "")),
        vim.log.levels.ERROR)
    end
    if on_done then on_done(ok) end
  end)
end

-- True if `port` is free for binding on 127.0.0.1. Closes the probe
-- socket either way so we don't leak a half-open handle.
local function port_is_free(port)
  local sock = uv.new_tcp()
  if not sock then return false end
  local ok = pcall(function() sock:bind("127.0.0.1", port) end)
  sock:close()
  return ok
end

-- First free port starting at `start_port` (inclusive), scanning up to
-- `PORT_SCAN_RANGE` further. Returns nil if every probed port was taken.
local function find_free_port(start_port)
  for port = start_port, start_port + PORT_SCAN_RANGE - 1 do
    if port_is_free(port) then return port end
  end
  return nil
end

local function set_urls(port)
  local base = "http://127.0.0.1:" .. tostring(port)
  config.url = base .. "/buffer"
  config.cursor_url = base .. "/cursor"
  config.jump_url = base .. "/jump"
  config.debug_url = base .. "/debug"
end

local function open_browser(url)
  local opener
  if vim.fn.has("mac") == 1 then
    opener = "open"
  elseif vim.fn.has("unix") == 1 then
    opener = vim.fn.executable("xdg-open") == 1 and "xdg-open" or nil
  elseif vim.fn.has("win32") == 1 then
    opener = "start"
  end
  if not opener then
    vim.notify("mathpreview: no browser opener found; visit " .. url, vim.log.levels.INFO)
    return
  end
  vim.fn.jobstart({ opener, url }, { detach = true })
end

-- Resolve the .tex root for the current buffer. For now: just the buffer's
-- own path, since the daemon does its own project-root walk. If the buffer
-- has no name we error out — auto-spawn needs a concrete file path to pass
-- on the command line.
local function current_root()
  local path = vim.api.nvim_buf_get_name(0)
  if path == nil or path == "" then
    return nil, "current buffer has no name; open a .tex file first"
  end
  return path, nil
end

local function push_buffer()
  if not daemon_job then return end
  local buf = vim.api.nvim_get_current_buf()
  local path = vim.api.nvim_buf_get_name(buf)
  if path == "" then
    last_status.last_error = "current buffer has no name"
    return
  end
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  local body = table.concat(lines, "\n")
  local args = {
    "curl", "--silent", "--show-error", "--fail-with-body", "--max-time", "5",
    "--header", "X-Mathpreview-Path: " .. path,
    "--data-binary", "@-",
    "-X", "POST",
    config.url,
  }
  run_system(args, { stdin = body }, function(res)
    if res and res.code ~= 0 then
      last_status.last_error = ("curl exit %d: %s"):format(
        res.code or -1, (res.stderr or ""):gsub("%s+$", ""))
    else
      last_status.last_error = nil
    end
  end)
  last_status.pushes = last_status.pushes + 1
  last_status.last_push_ms = uv.hrtime() / 1e6
end

local function post_cursor()
  if not daemon_job or not config.sync then return end
  local buf = vim.api.nvim_get_current_buf()
  local path = vim.api.nvim_buf_get_name(buf)
  if path == "" then return end
  local cursor = vim.api.nvim_win_get_cursor(0)
  local payload = json_encode({ file = path, line = cursor[1], col = cursor[2] + 1 })
  local args = {
    "curl", "--silent", "--show-error", "--fail-with-body", "--max-time", "2",
    "--header", "content-type: application/json",
    "--data-binary", "@-",
    "-X", "POST",
    config.cursor_url,
  }
  run_system(args, { stdin = payload }, function(res)
    if res and res.code ~= 0 then
      last_status.last_error = ("cursor curl exit %d: %s"):format(
        res.code or -1, (res.stderr or ""):gsub("%s+$", ""))
    else
      last_status.last_error = nil
    end
  end)
  last_status.cursor_posts = last_status.cursor_posts + 1
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
  if not daemon_job or not config.sync then return end
  local args = {
    "curl", "--silent", "--show-error", "--fail-with-body", "--max-time", "2",
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

local function stop_jump_poll()
  if jump_timer then jump_timer:stop(); jump_timer:close(); jump_timer = nil end
end

local function debounced_push()
  if not daemon_job then return end
  if timer then timer:stop(); timer:close() end
  timer = uv.new_timer()
  timer:start(config.debounce_ms, 0, vim.schedule_wrap(function()
    push_buffer()
    if timer then timer:close(); timer = nil end
  end))
end

local function debounced_cursor()
  if not daemon_job or not config.sync then return end
  if cursor_timer then cursor_timer:stop(); cursor_timer:close() end
  cursor_timer = uv.new_timer()
  cursor_timer:start(config.cursor_debounce_ms, 0, vim.schedule_wrap(function()
    post_cursor()
    if cursor_timer then cursor_timer:close(); cursor_timer = nil end
  end))
end

-- Attach the TextChanged / CursorMoved autocmds and start jump polling.
-- Idempotent — clearing the augroup before re-creating it.
local function attach_autocmds()
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
  vim.api.nvim_create_autocmd("VimLeavePre", {
    group = "mathpreview",
    callback = function() M.stop() end,
  })
  start_jump_poll()
end

local function detach_autocmds()
  pcall(vim.api.nvim_del_augroup_by_name, "mathpreview")
  stop_jump_poll()
end

-- Spawn the daemon for the current buffer using an already-resolved binary
-- path `cmd`. Callers reach this through ensure_binary(), which guarantees
-- the binary exists and is current first.
local function start_with(cmd, opts)
  local root, err = current_root()
  if not root then
    vim.notify("mathpreview: " .. err, vim.log.levels.ERROR)
    return
  end
  local port = find_free_port(DEFAULT_PORT)
  if not port then
    vim.notify(
      ("mathpreview: no free port in %d..%d"):format(DEFAULT_PORT, DEFAULT_PORT + PORT_SCAN_RANGE - 1),
      vim.log.levels.ERROR)
    return
  end
  local spawn_args = { cmd, "serve", root, "--port", tostring(port) }
  if config.mathjax_url and config.mathjax_url ~= "" then
    table.insert(spawn_args, "--mathjax-url")
    table.insert(spawn_args, config.mathjax_url)
  end
  -- reveal-source (browser modifier-click → POST /reveal-source) spawns
  -- an editor at the clicked location. When cursor sync is on, the plugin
  -- already applies that click IN PLACE via the polled /jump
  -- (jump_to_source uses `edit` in the current window), so an extra
  -- editor spawn is redundant — and `nvim --remote-send :e …` yanks you
  -- into a fresh buffer. So disable the spawn here by passing an empty
  -- --editor: the daemon returns 503 (no log noise) and the browser
  -- quietly relies on /jump. With sync off there's no poll, so keep the
  -- spawn and target THIS nvim via v:servername (the default template's
  -- $NVIM_LISTEN_ADDRESS is no longer exported by Neovim, which would
  -- otherwise fail with E247). An explicit config.editor always wins.
  local editor_cmd = config.editor
  if editor_cmd == nil then
    if not config.sync and vim.v.servername and vim.v.servername ~= "" then
      editor_cmd = string.format(
        [[nvim --server %s --remote-send "<C-\><C-N>:e +{line} {file}<CR>"]],
        vim.fn.shellescape(vim.v.servername))
    else
      editor_cmd = ""
    end
  end
  -- Always pass --editor (even empty) so the daemon never falls back to
  -- its $NVIM_LISTEN_ADDRESS default template.
  table.insert(spawn_args, "--editor")
  table.insert(spawn_args, editor_cmd)
  -- Capture the daemon's stderr so a failed spawn explains *why* instead of
  -- just printing an exit code. We also report which binary was launched —
  -- the resolved path can be a stale in-checkout build shadowing $PATH.
  local stderr_lines = {}
  local job = vim.fn.jobstart(
    spawn_args,
    {
      on_stderr = function(_, data)
        if data then vim.list_extend(stderr_lines, data) end
      end,
      on_exit = function(_, code)
        local exited_root = daemon_root or "<unknown>"
        daemon_job = nil
        daemon_port = nil
        daemon_root = nil
        detach_autocmds()
        if code ~= 0 then
          local tail = vim.trim(table.concat(stderr_lines, "\n"))
          vim.schedule(function()
            local msg = ("mathpreview: daemon for %s exited with code %d (binary: %s)")
              :format(exited_root, code, cmd)
            if tail ~= "" then
              msg = msg .. "\n" .. tail
            end
            vim.notify(msg, vim.log.levels.WARN)
          end)
        end
      end,
    })
  if job <= 0 then
    vim.notify("mathpreview: failed to spawn daemon", vim.log.levels.ERROR)
    return
  end
  daemon_job = job
  daemon_port = port
  daemon_root = root
  set_urls(port)
  attach_autocmds()
  -- Daemon takes ~100-300 ms to bind the port and finish initial render.
  -- Defer the browser open so the first GET / hits a ready server.
  if config.auto_open_browser then
    vim.defer_fn(function()
      open_browser("http://127.0.0.1:" .. tostring(port) .. "/")
    end, 350)
  end
  vim.notify(
    ("mathpreview: serving %s on http://127.0.0.1:%d"):format(vim.fn.fnamemodify(root, ":~"), port),
    vim.log.levels.INFO)
end

-- Resolve a usable binary — `cargo install`-ing it as needed — then call
-- on_ready(cmd). This gives "auto-install on first use" and "auto-reinstall on
-- plugin update" with no plugin-manager build hook:
--   * no binary + source checkout + cargo → install, then proceed
--   * our installed/in-checkout binary older than this plugin → reinstall
--   * current binary → proceed as-is
--   * a user `cmd` / unrelated $PATH binary → proceed, warn on skew
-- Every (re)install emits auto_install's "installing… please wait" notice.
local function ensure_binary(on_ready)
  local cmd = resolve_cmd()
  local can_build = is_source_checkout() and vim.fn.executable("cargo") == 1

  if not cmd then
    if can_build then
      auto_install(function(ok)
        local got = resolve_cmd()
        if ok and got then
          on_ready(got)
        elseif ok then
          vim.notify(
            "mathpreview: install reported success but no binary found at "
              .. installed_binary_path() .. " — see :messages.",
            vim.log.levels.ERROR)
        end
        -- on failure auto_install already notified with the cargo error.
      end)
    else
      vim.notify(
        "mathpreview: `mathpreview-cli` not found on $PATH" ..
        (is_source_checkout() and " and `cargo` is unavailable to install it. " or ". ") ..
        "Install the binary first (see README), or set `cmd = '/path/to/mathpreview-cli'` in setup().",
        vim.log.levels.ERROR)
    end
    return
  end

  -- A binary exists. If it's one we manage and we can compile, reinstall it
  -- when it's older than this plugin checkout (the reinstall-on-update path).
  -- A user `cmd` / unrelated $PATH binary isn't ours to touch — just run the
  -- async skew check (which warns) and use it as-is.
  if can_build and is_managed(cmd) then
    run_system({ cmd, "--version" }, {}, function(res)
      local ver = (res and res.code == 0 and res.stdout)
        and res.stdout:gsub("%s+$", ""):match("(%S+)%s*$") or nil
      if ver then last_status.binary_version = ver end
      if ver and semver_cmp(ver, PLUGIN_VERSION) < 0 then
        vim.notify(
          ("mathpreview: binary %s is older than plugin %s — reinstalling…")
            :format(ver, PLUGIN_VERSION),
          vim.log.levels.INFO)
        auto_install(function(ok) on_ready((ok and resolve_cmd()) or cmd) end)
      else
        on_ready(cmd)
      end
    end)
    return
  end

  check_binary_version(cmd)  -- async; warns once if a user/$PATH binary is stale
  on_ready(cmd)
end

function M.start(opts)
  opts = opts or {}
  if daemon_job then
    if config.auto_open_browser then
      open_browser("http://127.0.0.1:" .. tostring(daemon_port) .. "/")
    end
    return
  end
  ensure_binary(function(cmd) start_with(cmd, opts) end)
end

function M.stop()
  if not daemon_job then return end
  local job = daemon_job
  daemon_job = nil
  pcall(vim.fn.jobstop, job)
  detach_autocmds()
end

function M.restart()
  M.stop()
  -- Give the OS a moment to release the port before re-binding.
  vim.defer_fn(function() M.start() end, 200)
end

function M.status()
  local age = uv.hrtime() / 1e6 - last_status.last_push_ms
  local buf = vim.api.nvim_get_current_buf()
  return {
    daemon_running = daemon_job ~= nil,
    daemon_port = daemon_port,
    daemon_root = daemon_root,
    url = config.url,
    cursor_url = config.cursor_url,
    jump_url = config.jump_url,
    current_path = vim.api.nvim_buf_get_name(buf),
    current_ft = vim.bo[buf].filetype,
    pushes = last_status.pushes,
    cursor_posts = last_status.cursor_posts,
    jumps = last_status.jumps,
    last_jump_seq = last_jump_seq,
    last_push_ago_ms = (last_status.last_push_ms > 0) and math.floor(age) or nil,
    last_error = last_status.last_error,
    cmd = resolve_cmd(),
    install_dir = cargo_bin_dir(),
    install_dir_on_path = vim.fn.executable("mathpreview-cli") == 1,
    plugin_version = PLUGIN_VERSION,
    binary_version = last_status.binary_version,  -- nil until first :MathPreview
    nvim_version = vim.version() and (vim.version().major .. "." .. vim.version().minor) or "?",
  }
end

-- Fetch the daemon's `/debug` view and print the resolved settings plus
-- the config / macro paths it consulted (with a `*` marker for the files
-- that actually exist). Answers "what settings are in effect and where
-- did they come from?" without leaving the editor.
function M.debug()
  if not daemon_job or not config.debug_url then
    vim.notify("mathpreview: no daemon running (start with :MathPreview)", vim.log.levels.WARN)
    return
  end
  run_system(
    { "curl", "--silent", "--show-error", "--fail-with-body", "--max-time", "5", config.debug_url },
    {},
    function(res)
      if res.code ~= 0 then
        vim.notify(
          ("mathpreview: /debug failed (curl %d): %s"):format(res.code, (res.stderr or ""):gsub("%s+$", "")),
          vim.log.levels.ERROR)
        return
      end
      local ok, data = pcall(json_decode, res.stdout or "")
      if not ok or type(data) ~= "table" then
        print(res.stdout)  -- fall back to raw JSON if decode failed
        return
      end
      local lines = {}
      local function add(s) table.insert(lines, s) end
      local function mark(exists) return exists and "*" or " " end
      add("mathpreview /debug  (" .. tostring(config.debug_url) .. ")")
      add(("daemon: port %s, root %s"):format(tostring(daemon_port), tostring(daemon_root)))
      add("editor_cmd: " .. tostring(data.editor_cmd))
      add(("ws_protocol: %s   verbose_logging: %s")
        :format(tostring(data.ws_protocol), tostring(data.debug_logging)))
      local vc = data.viewer_config or {}
      add("viewer config:")
      add("  font_size:           " .. tostring(vc.font_size))
      add("  default_page_mode:   " .. tostring(vc.default_page_mode))
      add("  default_theme:       " .. tostring(vc.default_theme))
      add("  source_jump_trigger: " .. tostring(vc.source_jump_trigger))
      add("config paths (cascade, low→high priority; * = exists):")
      if data.config_paths and #data.config_paths > 0 then
        for _, p in ipairs(data.config_paths) do
          add(("  [%s] %s"):format(mark(p.exists), tostring(p.path)))
        end
      else
        add("  (none)")
      end
      add("macro paths (* = exists):")
      if data.macro_paths and #data.macro_paths > 0 then
        for _, p in ipairs(data.macro_paths) do
          add(("  [%s] %s  (%s)"):format(mark(p.exists), tostring(p.path), tostring(p.source)))
        end
      else
        add("  (none)")
      end
      print(table.concat(lines, "\n"))
    end)
end

-- Lightweight setup hook. Most users won't need to call this — the
-- defaults are fine and the plugin/mathpreview.lua file registers the
-- commands. Pass overrides here to change the binary path, filetype set,
-- debounce, or to disable browser auto-open:
--
--   require("mathpreview").setup({
--     cmd = "/usr/local/bin/mathpreview-cli",
--     auto_open_browser = false,
--   })
function M.setup(opts)
  opts = opts or {}
  config = vim.tbl_extend("force", config, opts)
end

return M
