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
local PLUGIN_VERSION = "0.1.56"

local config = {
  cmd = nil,                              -- resolved at start; "mathpreview-cli" by default
  filetypes = { "tex", "plaintex", "latex" },
  debounce_ms = 40,
  cursor_debounce_ms = 80,
  -- Source-jump (browser → editor) is a long-poll, not a fixed-interval
  -- poll: the plugin keeps ONE request parked on the daemon's `/jump`
  -- endpoint, which returns the instant a jump arrives or after
  -- `jump_wait_ms` with nothing. This is why an idle preview costs ~no CPU
  -- (one hanging request) instead of spawning curl several times a second.
  -- `jump_retry_ms` is the back-off before re-parking after an empty
  -- return or a transient error, so a daemon that doesn't support
  -- long-poll (or a brief restart) can't spin.
  jump_wait_ms = 25000,
  jump_retry_ms = 1000,
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
  -- Optional hook run AFTER a browser source-jump has moved this nvim's
  -- cursor (Cmd/Ctrl-click in the preview → jump in place). Use it to do
  -- whatever your window manager needs to raise/focus the editor — the
  -- thing PDF viewers do but most HTML previewers can't. Signature:
  --   on_jump = function(jump)  -- jump = { file=, line=, col= }
  -- Example (KDE/Plasma + Wayland, nvim-qt, via kdotool):
  --   on_jump = function()
  --     vim.system({ "sh", "-c",
  --       "kdotool search --class nvim | head -1 | xargs kdotool windowactivate" })
  --   end
  -- Runs in a scheduled (main-loop) context; errors are caught and logged.
  on_jump = nil,
  -- Bring nvim's host window to the front on a source-jump — the focus a
  -- PDF viewer gives you via SyncTeX. On by default. Best-effort and
  -- platform-aware. On Linux/BSD it targets THIS nvim's own window by walking
  -- up the process tree from nvim's PID and raising the ancestor that owns a
  -- window (Hyprland `hyprctl`, Sway `swaymsg`, KDE `kdotool`, X11 `xdotool`),
  -- so two terminals each running nvim+mathpreview raise the right one. X11
  -- prefers `$WINDOWID` when set. macOS uses `osascript … activate` on the
  -- detected terminal/GUI app. If the PID walk finds nothing it falls back to
  -- focusing the `jump_window` class/app_id below. Set false to stop the plugin
  -- from stealing focus on every click. Runs before `on_jump`, so the hook can
  -- override or extend it.
  raise_on_jump = true,
  -- Linux class / app_id used only as the FALLBACK when the PID walk above
  -- can't find nvim's window. Default "nvim" matches nvim-qt / Neovide. For
  -- TERMINAL nvim, set it to your terminal's class — e.g. "kitty", "foot",
  -- "Alacritty", "org.wezfurlong.wezterm". Ignored on macOS. Find it with
  -- `kdotool getactivewindow getwindowclassname` (KDE), `hyprctl activewindow`
  -- (Hyprland), or `swaymsg -t get_tree` (Sway) with the terminal focused.
  jump_window = "nvim",
  -- Per-session URLs, written when start_daemon() picks a port.
  url = nil,        -- http://127.0.0.1:<port>/buffer
  cursor_url = nil, -- http://127.0.0.1:<port>/cursor
  selection_url = nil, -- http://127.0.0.1:<port>/selection
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
local selection_timer = nil
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

local function json_decode(value)
  if vim.json and vim.json.decode then return vim.json.decode(value) end
  return vim.fn.json_decode(value)
end

-- Run a shell command. Uses vim.system on nvim 0.10+; falls back to
-- jobstart on older builds. Always async; on_done is called on the main
-- thread with { code, stdout, stderr }.
local function run_system(args, opts, on_done)
  opts = opts or {}
  -- Optional: called with each stderr line as it arrives (for live progress).
  -- When unset, behavior is identical to before — the sync POSTs don't use it.
  local on_line = opts.on_stderr_line
  if vim.system then
    if not on_line then
      vim.system(args, opts, function(res)
        if on_done then vim.schedule(function() on_done(res) end) end
      end)
      return
    end
    -- Streaming variant: forward each stderr line live, still capturing the
    -- full stderr for the final result.
    local err, buf = {}, ""
    vim.system(args, {
      cwd = opts.cwd,
      stdin = opts.stdin,
      stderr = function(_, data)
        if not data then return end
        err[#err + 1] = data
        buf = buf .. data
        local start = 1
        while true do
          local nl = buf:find("\n", start, true)
          if not nl then break end
          local line = buf:sub(start, nl - 1)
          start = nl + 1
          vim.schedule(function() on_line(line) end)
        end
        buf = buf:sub(start)
      end,
    }, function(res)
      res = res or {}
      res.stderr = table.concat(err)
      if on_done then vim.schedule(function() on_done(res) end) end
    end)
    return
  end
  local stdout, stderr = {}, {}
  local job = vim.fn.jobstart(args, {
    cwd = opts.cwd,  -- nil → inherit nvim's cwd
    on_stdout = function(_, data) if data then vim.list_extend(stdout, data) end end,
    on_stderr = function(_, data)
      if not data then return end
      vim.list_extend(stderr, data)
      if on_line then
        for _, l in ipairs(data) do
          if l ~= "" then on_line(l) end
        end
      end
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
    if on_done then on_done({ code = -1, stderr = "could not start " .. tostring(args[1]) }) end
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
-- Guards against overlapping starts: `daemon_job` is only set inside the async
-- `start_with`, so a second `:MathPreview` during the binary/version check
-- would otherwise pass the `daemon_job` guard and spawn a duplicate daemon.
local starting = false
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
local INSTALL_SPINNER = { "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏" }

local function auto_install(on_done)
  if installing then return end  -- one already in flight; its callback will start us
  installing = true
  local started = uv.now()
  local frame = 0
  local step = "" -- latest cargo step, e.g. "compiling mathpreview-core"
  local progress = uv.new_timer()
  local function stop_progress()
    if progress then
      progress:stop()
      progress:close()
      progress = nil
    end
    -- Clear the cmdline spinner line.
    vim.api.nvim_echo({ { "" } }, false, {})
  end
  vim.notify(
    ("mathpreview: installing mathpreview-cli to %s (first run, ~30s) — please "
      .. "wait, :MathPreview will start automatically when it finishes…")
      :format(cargo_bin_dir()),
    vim.log.levels.INFO)
  -- Live progress on the cmdline (history=false → not added to :messages) so the
  -- one-time build doesn't look frozen: a spinner, elapsed seconds, and the
  -- crate cargo is currently compiling.
  progress:start(0, 120, vim.schedule_wrap(function()
    if not installing then return end
    frame = frame + 1
    local secs = math.floor((uv.now() - started) / 1000)
    local spin = INSTALL_SPINNER[(frame % #INSTALL_SPINNER) + 1]
    local msg = ("%s building mathpreview-cli… %ds"):format(spin, secs)
    if step ~= "" then msg = msg .. "  " .. step end
    vim.api.nvim_echo({ { msg, "Comment" } }, false, {})
  end))
  local on_stderr_line = function(line)
    local crate = line:match("^%s*Compiling%s+(%S+)")
    if crate then
      step = "compiling " .. crate
    elseif line:match("^%s*Finished") then
      step = "finishing…"
    end
  end
  run_system(
    cargo_install_args(),
    { cwd = plugin_root(), on_stderr_line = on_stderr_line },
    function(res)
      installing = false
      stop_progress()
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

-- True if `port` is free on 127.0.0.1. We `bind` AND `listen`: libuv sets
-- SO_REUSEADDR on the probe socket, so a bare `bind` succeeds even when
-- another process is actively listening on the port (especially on macOS) —
-- the conflict only surfaces at `listen()`. Without the listen, a second nvim
-- would think 23636 is free, spawn a daemon there, and the daemon's own bind
-- would then fail with "address already in use". Closes the socket either way.
local function port_is_free(port)
  local sock = uv.new_tcp()
  if not sock then return false end
  local ok = pcall(function()
    assert(sock:bind("127.0.0.1", port))
    assert(sock:listen(128, function() end))
  end)
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
  config.selection_url = base .. "/selection"
  config.jump_url = base .. "/jump"
  config.debug_url = base .. "/debug"
end

local function open_browser(url)
  local argv
  if vim.fn.has("mac") == 1 then
    argv = { "open", url }
  elseif vim.fn.has("unix") == 1 then
    argv = vim.fn.executable("xdg-open") == 1 and { "xdg-open", url } or nil
  elseif vim.fn.has("win32") == 1 then
    -- `start` is a cmd.exe builtin, not an executable on $PATH, so jobstart
    -- can't exec it directly — run it via `cmd /c`. The empty "" is start's
    -- window-title argument (so a quoted URL isn't mistaken for the title).
    argv = { "cmd", "/c", "start", "", url }
  end
  if not argv then
    vim.notify("mathpreview: no browser opener found; visit " .. url, vim.log.levels.INFO)
    return
  end
  vim.fn.jobstart(argv, { detach = true })
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

-- True for any visual sub-mode: charwise (v), linewise (V), or blockwise
-- (Ctrl-V, byte 0x16). Detected by byte so the source carries no control char.
local function is_visual(m)
  if not m or m == "" then return false end
  local c = m:sub(1, 1)
  return c == "v" or c == "V" or m:byte(1) == 22
end

-- Shared JSON POST used by the selection senders (mirrors post_cursor's curl).
local function post_sync_json(url, payload, label)
  local args = {
    "curl", "--silent", "--show-error", "--fail-with-body", "--max-time", "2",
    "--header", "content-type: application/json",
    "--data-binary", "@-",
    "-X", "POST",
    url,
  }
  run_system(args, { stdin = payload }, function(res)
    if res and res.code ~= 0 then
      last_status.last_error = ("%s curl exit %d: %s"):format(
        label, res.code or -1, (res.stderr or ""):gsub("%s+$", ""))
    else
      last_status.last_error = nil
    end
  end)
end

-- Send the editor's current visual selection as a source range so the preview
-- highlights the matching elements. Reads both ends live: getpos("v") is the
-- anchor (the '< / '> marks are stale until visual mode is left) and the cursor
-- is the moving end. Coordinate care: getpos col is already 1-based, but
-- nvim_win_get_cursor col is 0-based and needs +1 (the post_cursor convention).
local function post_selection()
  if not daemon_job or not config.sync then return end
  -- The debounce may fire just after the user left visual mode; bail so we
  -- don't read a stale getpos("v"). ModeChanged handles the dismiss/clear.
  local m = vim.fn.mode()
  if not is_visual(m) then return end
  local buf = vim.api.nvim_get_current_buf()
  local path = vim.api.nvim_buf_get_name(buf)
  if path == "" then return end
  local anchor = vim.fn.getpos("v")           -- [bufnum, lnum(1), col(1), off]
  local cursor = vim.api.nvim_win_get_cursor(0) -- {line(1), col(0)}
  local a_line, a_col = anchor[2], anchor[3]
  local c_line, c_col = cursor[1], cursor[2] + 1
  -- Order so start <= end by (line, col).
  local sl, sc, el, ec
  if a_line < c_line or (a_line == c_line and a_col <= c_col) then
    sl, sc, el, ec = a_line, a_col, c_line, c_col
  else
    sl, sc, el, ec = c_line, c_col, a_line, a_col
  end
  -- Linewise (V) covers whole rows; the large sentinel means "through EOL".
  if m == "V" then
    sc, ec = 1, 2147483647
  end
  local payload = json_encode({
    file = path, start_line = sl, start_col = sc, end_line = el, end_col = ec,
  })
  post_sync_json(config.selection_url, payload, "selection")
end

-- Tell the daemon to drop the selection highlight (sent on leaving visual mode).
local function post_selection_clear()
  if not daemon_job then return end
  local buf = vim.api.nvim_get_current_buf()
  local path = vim.api.nvim_buf_get_name(buf)
  if path == "" then return end
  post_sync_json(config.selection_url, json_encode({ file = path, clear = true }),
    "selection-clear")
end

-- Best-effort "bring the editor to the front" on a source-jump — the focus
-- SyncTeX gives you with a PDF viewer. Hosts differ wildly, so this covers
-- the common cases and stays silent when it can't; extend via `on_jump`.
local function raise_editor()
  local sysname = (uv.os_uname() or {}).sysname or ""
  if sysname == "Darwin" then
    local app
    if vim.g.neovide then
      app = "Neovide"
    elseif vim.fn.exists("g:GuiLoaded") == 1 then
      app = "nvim-qt"
    else
      -- LC_TERMINAL survives tmux/ssh where TERM_PROGRAM is rewritten to
      -- "tmux"; check it as a fallback (iTerm sets LC_TERMINAL=iTerm2).
      local map = {
        ["Apple_Terminal"] = "Terminal",
        ["iTerm.app"]      = "iTerm",
        ["iTerm2"]         = "iTerm",
        ["WezTerm"]        = "WezTerm",
        ["ghostty"]        = "Ghostty",
        ["vscode"]         = "Code",
      }
      app = map[vim.env.TERM_PROGRAM] or map[vim.env.LC_TERMINAL]
      if not app and vim.env.TERM == "xterm-kitty" then app = "kitty" end
      if not app and vim.env.ALACRITTY_WINDOW_ID then app = "Alacritty" end
    end
    if app then
      run_system({ "osascript", "-e", 'tell application "' .. app .. '" to activate' }, {})
    end
    return
  end
  -- Linux/BSD (best-effort). We target THIS nvim's own window so two terminals
  -- each running nvim+mathpreview raise the right one: start from nvim's PID and
  -- walk up the process tree (nvim -> shell -> terminal), raising the first
  -- ancestor that actually owns a window. If nothing matches (unusual host,
  -- missing tool) we fall back to the `jump_window` class/app_id — "anything is
  -- fair game". `jump_window` defaults to "nvim" (nvim-qt/Neovide); for terminal
  -- nvim set it to YOUR terminal's class (e.g. "kitty", "foot", "Alacritty").
  local win = config.jump_window or "nvim"
  local pid = vim.fn.getpid()
  -- Run `try` (a sh snippet using $P = current ancestor pid) at each level up
  -- the tree, stopping at the first that exits 0; otherwise run `fallback`.
  -- `win` is passed as $1 (positional, never interpolated) so it can't break
  -- the script's quoting.
  local function walk(try, fallback)
    local script = ([[
P=%d
i=0
while [ "$P" -gt 1 ] && [ "$i" -lt 12 ]; do
  if %s; then exit 0; fi
  P=$(ps -o ppid= -p "$P" 2>/dev/null | tr -d ' ')
  [ -n "$P" ] || break
  i=$((i+1))
done
%s]]):format(pid, try, fallback)
    run_system({ "sh", "-c", script, "sh", win }, {})
  end
  if vim.env.WAYLAND_DISPLAY and vim.env.WAYLAND_DISPLAY ~= "" then
    -- Wayland blocks the generic self-raise, so it's per-compositor. Detect
    -- via each compositor's env marker, then its CLI. Each selector matches by
    -- the ancestor PID ($P); the fallback matches by class/app_id ($1).
    if vim.env.HYPRLAND_INSTANCE_SIGNATURE and vim.fn.executable("hyprctl") == 1 then
      walk([=[[ "$(hyprctl dispatch focuswindow pid:$P)" = ok ]]=],
        [=[hyprctl dispatch focuswindow class:"$1"]=])
    elseif vim.env.SWAYSOCK and vim.fn.executable("swaymsg") == 1 then
      walk([=[swaymsg "[pid=$P] focus" | grep -q '"success": *true']=],
        [=[swaymsg "[app_id=$1] focus"]=])
    elseif vim.fn.executable("kdotool") == 1 then
      -- KDE/KWin: no reliable env marker, so kdotool's presence is the signal.
      walk([=[W=$(kdotool search --pid "$P" 2>/dev/null | head -1); [ -n "$W" ] && kdotool windowactivate "$W"]=],
        [=[kdotool search --class "$1" | head -1 | xargs -r kdotool windowactivate]=])
    end
    return
  end
  -- X11: the host terminal exports $WINDOWID for its own window — the most
  -- precise signal, so try it first. Then the PID walk, then class.
  local winid = vim.env.WINDOWID
  if winid and winid ~= "" and vim.fn.executable("xdotool") == 1 then
    run_system({ "xdotool", "windowactivate", winid }, {})
    return
  end
  if vim.fn.executable("xdotool") == 1 then
    walk([=[W=$(xdotool search --pid "$P" 2>/dev/null | head -1); [ -n "$W" ] && xdotool windowactivate "$W"]=],
      [=[xdotool search --class "$1" | head -1 | xargs -r xdotool windowactivate]=])
  end
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
  -- Built-in focus (SyncTeX-style): bring nvim's window forward. Runs first
  -- so a user on_jump hook can still override or extend it.
  if config.raise_on_jump then
    pcall(raise_editor)
  end
  -- User hook: raise/focus the editor window, etc. The cursor has already
  -- moved; we just hand the target to whatever the user configured.
  if type(config.on_jump) == "function" then
    local ok, err = pcall(config.on_jump, { file = file, line = line, col = col + 1 })
    if not ok then
      last_status.last_error = "on_jump error: " .. tostring(err)
    end
  end
end

-- Source-jump (browser → editor) over a single long-poll instead of a
-- fixed-interval timer: we keep exactly one curl parked on `/jump?wait=…`,
-- which the daemon holds open until a jump arrives or `jump_wait_ms` elapses.
-- On idle that's one hanging request, not several curl spawns a second — the
-- difference between ~0% and a few % CPU while the preview just sits there.
-- `jump_poll_gen` is a cancellation token: stop/restart bumps it so any
-- in-flight callback won't re-park a stale loop.
local jump_poll_gen = 0

local function long_poll_jump(gen)
  if gen ~= jump_poll_gen or not daemon_job or not config.sync then return end
  local wait_ms = config.jump_wait_ms or 25000
  -- Let curl outlive the server's hold by a few seconds so a clean timeout
  -- comes back as an empty 204, not a curl abort.
  local max_time = math.floor(wait_ms / 1000) + 5
  local args = {
    "curl", "--silent", "--show-error", "--fail-with-body",
    "--max-time", tostring(max_time),
    config.jump_url .. "?after=" .. tostring(last_jump_seq)
      .. "&wait=" .. tostring(wait_ms),
  }
  run_system(args, {}, function(res)
    if gen ~= jump_poll_gen or not daemon_job or not config.sync then return end
    local got_jump = false
    if res and res.code == 0 then
      local body = (res.stdout or ""):gsub("^%s+", ""):gsub("%s+$", "")
      if body ~= "" then
        local ok, decoded = pcall(json_decode, body)
        if ok then jump_to_source(decoded); got_jump = true end
      end
    end
    -- Re-park immediately after a real jump (latency matters); otherwise
    -- back off jump_retry_ms so an empty return, an error, or a daemon that
    -- ignores `wait` can't spin. A jump that lands during the gap isn't
    -- lost — the daemon keeps `pending_jump`, so the next park returns it at
    -- once.
    if got_jump then
      long_poll_jump(gen)
    else
      vim.defer_fn(function() long_poll_jump(gen) end, config.jump_retry_ms or 1000)
    end
  end)
end

local function start_jump_poll()
  jump_poll_gen = jump_poll_gen + 1
  if not config.sync then return end
  long_poll_jump(jump_poll_gen)
end

local function stop_jump_poll()
  -- Supersede any in-flight long-poll so its callback won't re-park.
  jump_poll_gen = jump_poll_gen + 1
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

-- Separate timer from cursor_timer: a visual drag fires CursorMoved rapidly,
-- and sharing the timer would cancel pending cursor posts (and vice versa).
local function debounced_selection()
  if not daemon_job or not config.sync then return end
  if selection_timer then selection_timer:stop(); selection_timer:close() end
  selection_timer = uv.new_timer()
  selection_timer:start(config.cursor_debounce_ms, 0, vim.schedule_wrap(function()
    post_selection()
    if selection_timer then selection_timer:close(); selection_timer = nil end
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
        -- CursorMoved also fires while extending a visual selection, so route
        -- to the range sender when a visual mode is active.
        if is_visual(vim.fn.mode()) then
          debounced_selection()
        else
          debounced_cursor()
        end
      end
    end,
  })
  -- Seed the highlight on entering visual mode (no CursorMoved yet) and clear it
  -- on leaving. `*:*` then a mode() check avoids embedding the Ctrl-V byte in a
  -- pattern; vim.v.event tells us what we came from / went to.
  vim.api.nvim_create_autocmd("ModeChanged", {
    group = "mathpreview",
    pattern = "*:*",
    callback = function(args)
      if not vim.tbl_contains(config.filetypes, vim.bo[args.buf].filetype) then return end
      local ev = vim.v.event or {}
      if is_visual(ev.new_mode or vim.fn.mode()) then
        debounced_selection()
      elseif is_visual(ev.old_mode or "") then
        if selection_timer then
          selection_timer:stop()
          selection_timer:close()
          selection_timer = nil
        end
        post_selection_clear()
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
  -- The in-flight start has resolved; clear the guard now that we're committing
  -- to (or bailing out of) the actual spawn.
  starting = false
  local root, err = current_root()
  if not root then
    vim.notify("mathpreview: " .. err, vim.log.levels.ERROR)
    return
  end
  -- On restart we prefer the port the previous daemon held: rebinding it
  -- lets the already-open browser tab's live-reload WebSocket reconnect in
  -- place (1s backoff) instead of us spawning a duplicate tab.
  local port
  if opts.prev_port and port_is_free(opts.prev_port) then
    port = opts.prev_port
  else
    -- `scan_from` is bumped past a port that lost a bind race on retry.
    port = find_free_port(opts.scan_from or DEFAULT_PORT)
  end
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
  local started_at = uv.now()
  local retries_left = opts.port_retries
  if retries_left == nil then retries_left = PORT_SCAN_RANGE end
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
          -- A near-immediate non-zero exit is almost always a port-bind race:
          -- another process grabbed the probed port between our free-port
          -- check and the daemon's own bind. Retry on the next port a bounded
          -- number of times before surfacing the error.
          if retries_left > 0 and (uv.now() - started_at) < 2000 then
            local retry_opts = vim.tbl_extend("force", opts, {
              port_retries = retries_left - 1,
              scan_from = port + 1,
            })
            retry_opts.prev_port = nil -- don't reuse the port that just lost the race
            vim.schedule(function() start_with(cmd, retry_opts) end)
            return
          end
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
  -- Skip the open on a restart that rebound the same port: the existing
  -- tab reconnects on its own, so opening another would just pile up
  -- duplicates (the bug where 10 restarts left 10 tabs). If the restart
  -- had to move to a new port, the old tab is stale, so we do open.
  local reused_tab = opts.prev_port ~= nil and port == opts.prev_port
  if config.auto_open_browser and not reused_tab then
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
        else
          -- on_ready won't run; release the in-flight start guard.
          starting = false
          if ok then
            vim.notify(
              "mathpreview: install reported success but no binary found at "
                .. installed_binary_path() .. " — see :messages.",
              vim.log.levels.ERROR)
          end
          -- on failure auto_install already notified with the cargo error.
        end
      end)
    else
      starting = false
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
  -- A start is already resolving its binary/port asynchronously; don't kick off
  -- a second one (it would race find_free_port and spawn a duplicate daemon).
  if starting then return end
  starting = true
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
  -- Remember the port so the restart can rebind it and let the open tab
  -- reconnect, instead of opening a fresh one each time.
  local prev_port = daemon_port
  M.stop()
  -- Give the OS a moment to release the port before re-binding.
  vim.defer_fn(function() M.start({ prev_port = prev_port }) end, 200)
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
