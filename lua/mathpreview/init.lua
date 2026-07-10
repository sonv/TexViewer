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
-- on TextChanged. By default VimLeavePre kills the spawned daemon (and closes
-- the native window) so quitting nvim doesn't leave a stray server bound to
-- the port; set `close_on_exit = false` to deliberately let the preview
-- outlive nvim instead (see the config comment).

local M = {}

local DEFAULT_PORT = 23636
local PORT_SCAN_RANGE = 16  -- try 23636..23651 before giving up

-- Version this plugin checkout expects the `mathpreview-cli` binary to be.
-- On :MathPreview we compare it against the binary's `--version`; if this is
-- a source checkout with cargo, a stale/missing binary is (re)installed to
-- match (see ensure_binary). Otherwise we warn once on mismatch — the signal
-- that a fix you "released" isn't actually the binary you're running.
-- RELEASE: bump this in lockstep with Cargo.toml / Cargo.lock / CHANGELOG.
local PLUGIN_VERSION = "1.0.0"

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
  -- Mirror the editor's `/` search into the preview (hlsearch-style highlight
  -- of every match), including live search-as-you-type while the `/` cmdline is
  -- open (mirrors 'incsearch'). Requires `sync` and the 'hlsearch' option
  -- ('incsearch' for the as-you-type part; both are nvim defaults). Set false
  -- to keep the preview search independent of the editor's.
  sync_search = true,
  auto_open_browser = true,
  -- Which viewer to open the preview in:
  --   "browser" (default) — your default web browser, in a new tab.
  --   "window"            — a standalone native window (the OS webview:
  --                         WebKit on macOS, WebKitGTK on Linux). No browser
  --                         tab. It shows the exact same preview, so every
  --                         feature works identically.
  -- "window" needs a binary built with the `gui` cargo feature. The plugin's
  -- own auto-install handles this: when viewer="window" it adds `--features
  -- gui`, and on `:MathPreview` it detects a binary that lacks the feature and
  -- reinstalls once (so switching browser→window just works on a source
  -- checkout). The gui build pulls in webview deps — on Linux you need
  -- `libwebkit2gtk-4.1-dev` installed first. `auto_open_browser = false` opens
  -- neither.
  viewer = "browser",
  -- What happens to the preview when you quit nvim.
  --   true  (default) — tear it down: stop the daemon and CLOSE the native
  --                     window. This is what peek.nvim does — but it only works
  --                     for the native `viewer = "window"`. A *browser tab*
  --                     can't be closed by the plugin (browsers forbid scripts
  --                     from closing tabs they didn't open), so the tab just
  --                     goes inert when the daemon stops; close it yourself.
  --   false           — leave the preview RUNNING so it outlives nvim: the
  --                     daemon and window/tab stay live and fully usable until
  --                     you close them or run `:MathPreviewStop`. Use this to
  --                     keep reading the rendered doc after quitting the editor
  --                     (the one thing you can do with a browser tab that you
  --                     can't do by closing it). Implemented by spawning the
  --                     daemon and window detached so nvim's exit doesn't kill
  --                     them; `:MathPreviewStop` still stops them explicitly.
  close_on_exit = true,
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
  --       "kdotool search --all --class nvim | head -1 | xargs kdotool windowactivate" })
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
  search_url = nil, -- http://127.0.0.1:<port>/search
  jump_url = nil,   -- http://127.0.0.1:<port>/jump
  debug_url = nil,  -- http://127.0.0.1:<port>/debug
}

local uv = vim.uv or vim.loop

-- One daemon (and browser tab) PER root .tex file, so `:e other.tex` +
-- :MathPreview opens its own viewer instead of reusing the first file's.
-- `daemons` is the registry, keyed by absolute root path; each entry is
-- `{ job, port, root, opened, jump_seq }` (`opened` = we've opened/reused a tab
-- this session; `jump_seq` = that daemon's last source-jump sequence).
local daemons = {}
-- The globals below MIRROR whichever entry is "active" — the daemon for the
-- buffer you're currently in. A BufEnter autocmd keeps them in sync, so the
-- existing push / cursor / selection / jump code (which reads these + config.*
-- URLs) routes to the right daemon without per-call lookups. nil = the current
-- buffer has no daemon.
local daemon_job = nil   -- active daemon's jobid, or nil
local daemon_port = nil  -- active daemon's port
local daemon_root = nil  -- active daemon's root .tex path
-- Roots we're deliberately stopping (M.stop/M.restart), so the daemon's on_exit
-- doesn't mistake the SIGTERM for a port-bind race and auto-restart it.
local stopping = {}

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
  -- The native window needs the `gui` feature (wry/tao). Only pull it in when
  -- the user actually asked for the window, so a default "browser" install
  -- stays webview-free (no WebKitGTK build deps on Linux).
  if config.viewer == "window" then
    table.insert(args, "--features")
    table.insert(args, "gui")
  end
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
-- Roots whose first-time start is mid-resolve (binary/port), so a duplicate
-- :MathPreview for the SAME file is dropped while a DIFFERENT file can still
-- start concurrently. Keyed by root path.
local starting = {}
-- True only while a source-jump's `:edit` is running, so the BufEnter handler
-- doesn't churn the active daemon mid-jump.
local in_jump = false
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

-- Callbacks waiting on the single in-flight install. Multiple files can request
-- a start (and thus an install) concurrently now that there's a daemon per
-- root; every one's on_done must fire when the shared build finishes, or the
-- later callers' `starting[root]` guard would stay stuck true forever.
local install_waiters = {}

local function auto_install(on_done)
  if installing then
    -- One build is already running; ride its result instead of dropping ours.
    if on_done then install_waiters[#install_waiters + 1] = on_done end
    return
  end
  installing = true
  install_waiters = on_done and { on_done } or {}
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
      -- Fan out to every caller that requested this build (the first plus any
      -- that arrived while it ran), then clear the queue.
      local waiters = install_waiters
      install_waiters = {}
      for _, cb in ipairs(waiters) do cb(ok) end
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
  config.search_url = base .. "/search"
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

-- Launch the standalone native window (`mathpreview-cli view --attach <url>`),
-- which just opens a webview against this entry's already-running daemon — no
-- second daemon. Tracked in `entry.window_job` and killed on stop/restart, so
-- the window is tied to the preview session (it also closes when nvim exits).
-- On a binary without the `gui` feature the `view` subcommand is missing; we
-- detect the quick non-zero exit and point the user at the fix.
local function open_window(entry)
  -- Idempotent: a live tracked job means a window is already open (or opening,
  -- before its WebSocket has connected), so a second reuse-or-open landing in
  -- that gap is a no-op instead of launching — and orphaning — a duplicate.
  if entry.window_job then return end
  local cmd = entry.cmd or resolve_cmd()
  local url = "http://127.0.0.1:" .. tostring(entry.port) .. "/"
  local title = vim.fn.fnamemodify(entry.root, ":t")
  -- On macOS, prefer the installed Locus.app bundle's binary: a process run
  -- from inside a bundle carries the bundle identity, so the window gets the
  -- Locus dock icon and app name from the .icns — no runtime branding needed.
  -- The attach window is a pure webview client (the daemon serves the whole
  -- page), so version skew between the .app and the daemon is harmless. Build
  -- it with scripts/make-locus-app.sh --install.
  local argv = { cmd, "view", "--attach", url, "--title", title }
  if vim.fn.has("mac") == 1 then
    for _, app_bin in ipairs({
      "/Applications/Locus.app/Contents/MacOS/locus",
      vim.fn.expand("~/Applications/Locus.app/Contents/MacOS/locus"),
    }) do
      if vim.fn.executable(app_bin) == 1 then
        argv = { app_bin, "--attach", url, "--title", title }
        break
      end
    end
  end
  local stderr_lines = {}
  local jid
  jid = vim.fn.jobstart(
    argv,
    {
      -- Match the daemon: detached only when the preview should outlive nvim
      -- (close_on_exit=off), so quitting nvim leaves the window up. With
      -- close_on_exit on, it stays a child and stop_all closes it on exit.
      detach = not config.close_on_exit,
      on_stderr = function(_, data)
        if data then vim.list_extend(stderr_lines, data) end
      end,
      on_exit = function(_, code)
        -- Only clear the tracker if it still points at THIS job (a later exit
        -- from a job we've already replaced must not nil out the new one).
        if entry.window_job == jid then entry.window_job = nil end
        -- Deliberate kill (:MathStop / restart / daemon-crash teardown): the
        -- SIGTERM exit code is non-zero but expected — don't cry crash. Mirrors
        -- the daemon's `stopping` guard.
        if entry.window_stopping then
          entry.window_stopping = nil
          return
        end
        if code ~= 0 then
          local tail = vim.trim(table.concat(stderr_lines, "\n"))
          vim.schedule(function()
            -- clap prints exactly this for a missing `view` (non-gui binary).
            if tail:match("unrecognized subcommand") then
              vim.notify(
                "mathpreview: this binary lacks the native window (built without the "
                  .. "`gui` feature). Reinstall with it, or set viewer='browser'.\n"
                  .. "  cargo install --path crates/cli --features gui --force",
                vim.log.levels.ERROR)
            elseif tail ~= "" then
              vim.notify("mathpreview: native window exited (code " .. code .. ")\n" .. tail,
                vim.log.levels.WARN)
            end
          end)
        end
      end,
    })
  if jid <= 0 then
    vim.notify("mathpreview: failed to launch native window", vim.log.levels.ERROR)
    return
  end
  entry.window_job = jid
end

-- Open the configured viewer (browser tab or native window) for this daemon.
local function open_viewer(entry)
  entry.opened = true
  if config.viewer == "window" then
    open_window(entry)
  else
    open_browser("http://127.0.0.1:" .. tostring(entry.port) .. "/")
  end
end

-- The daemon is already running and the user ran :MathPreview again. Don't
-- stack up duplicate tabs (the "every run opens another tab" complaint): ask
-- the daemon how many browser tabs are connected (`/debug` → `clients`, the
-- live WebSocket count). If one is already open, reuse it — the tab live-
-- reloads, so it's already showing the current document — and just say so.
-- Only open a fresh tab when none is connected (you closed it). Falls back to
-- opening if the count can't be determined. The /debug curl runs async; its
-- callback is already main-loop-scheduled by run_system.
local function reuse_or_open_browser(entry)
  local url = "http://127.0.0.1:" .. tostring(entry.port) .. "/"
  local debug_url = "http://127.0.0.1:" .. tostring(entry.port) .. "/debug"
  local noun = config.viewer == "window" and "window" or "tab"
  local function do_open() open_viewer(entry) end
  local function say_reuse()
    entry.opened = true
    vim.notify("mathpreview: preview already open — reusing " .. noun .. " (" .. url .. ")",
      vim.log.levels.INFO)
  end
  run_system(
    { "curl", "--silent", "--max-time", "2", debug_url }, {},
    function(res)
      -- The daemon may have been stopped/replaced during the async curl.
      if daemons[entry.root] ~= entry then return end
      local clients
      if res and res.code == 0 and res.stdout and res.stdout ~= "" then
        local ok, data = pcall(vim.json.decode, res.stdout)
        if ok and type(data) == "table" then clients = data.clients end
      end
      if type(clients) == "number" then
        -- Authoritative: the daemon knows how many tabs are connected. Reuse if
        -- one is open; open a fresh tab if it was closed (clients == 0).
        if clients > 0 then say_reuse() else do_open() end
      else
        -- Daemon too old to report `clients` — fall back to this daemon's
        -- session flag so a repeat :MathPreview still doesn't stack a duplicate.
        if entry.opened then say_reuse() else do_open() end
      end
    end)
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

-- Strip the word-boundary / magic atoms that `*` and friends add, so a plain
-- word still matches the rendered text (the browser does a literal search).
-- Escape-aware: `\\v` in a vim pattern is a LITERAL backslash followed by
-- "v", not the very-magic atom — protect escaped pairs before stripping,
-- then restore each as the single literal backslash it matches.
local function normalize_search_pattern(pattern)
  local s = pattern or ""
  s = s:gsub("\\\\", "\1")
  s = s:gsub("\\[<>vVcC]", "")
  s = s:gsub("\\[zZ][se]", "")
  s = s:gsub("\1", "\\")
  return s
end

-- True when search mirroring is enabled and there's a daemon to send to.
local function search_sync_enabled()
  return daemon_job and config.sync and config.sync_search ~= false
    and config.search_url ~= nil
end

-- Mirror the editor's `/` search into the preview. Sends the search register
-- (@/) while search highlighting is active (`v:hlsearch`, which needs the
-- 'hlsearch' option), and an empty string to clear on `:nohlsearch`. Deduped so
-- it only posts on a change. Reset to nil on (de)activate so a fresh daemon
-- re-syncs. Called from CursorMoved (n / N / *) and CmdlineLeave (/ ? :noh).
local last_search_sent = nil
local function post_search()
  if not search_sync_enabled() then return end
  local active = vim.o.hlsearch and vim.v.hlsearch == 1
  local pattern = ""
  if active then
    pattern = normalize_search_pattern(vim.fn.getreg("/"))
  end
  if pattern == last_search_sent then return end
  last_search_sent = pattern
  post_sync_json(config.search_url, json_encode({ query = pattern }), "search")
end

-- Incremental search-as-you-type (mirrors 'incsearch'): while the `/` or `?`
-- cmdline is open, stream the partial pattern so the preview highlights live
-- with each keystroke. CmdlineLeave then settles the final state from
-- v:hlsearch / @/ — which self-corrects both the commit AND the abort (Esc)
-- case, since it reads the truth rather than this stream. Backspacing to an
-- empty pattern reverts the preview to the committed hlsearch state, matching
-- nvim's own behavior.
local function post_search_preview()
  if not search_sync_enabled() or not vim.o.incsearch then return end
  local t = vim.fn.getcmdtype()
  if t ~= "/" and t ~= "?" then return end -- cmdline already left; CmdlineLeave settles it
  local pattern = normalize_search_pattern(vim.fn.getcmdline())
  if pattern == "" then
    last_search_sent = nil
    post_search()
    return
  end
  if pattern == last_search_sent then return end
  last_search_sent = pattern
  post_sync_json(config.search_url, json_encode({ query = pattern }), "search")
end

-- Debounced wrapper for CmdlineChanged, which fires per keystroke. Short
-- window: live-feeling but not one curl per typed character.
local search_preview_timer = nil
local function debounced_search_preview()
  if not search_sync_enabled() then return end
  if search_preview_timer then search_preview_timer:stop(); search_preview_timer:close() end
  search_preview_timer = uv.new_timer()
  search_preview_timer:start(90, 0, vim.schedule_wrap(function()
    post_search_preview()
    if search_preview_timer then search_preview_timer:close(); search_preview_timer = nil end
  end))
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
      -- `hyprctl dispatch focuswindow` prints "ok" whenever the dispatcher RAN,
      -- even if pid:$P matched no window. Trusting "ok" would stop the walk at
      -- nvim's own PID — which owns no toplevel for terminal nvim — and never
      -- climb to the terminal (nor reach the class fallback). So gate on a real
      -- client owning $P first; a miss keeps the walk climbing to the ancestor
      -- that does. The PID can't be a substring of a bigger one ([^0-9]|$).
      walk([=[hyprctl -j clients 2>/dev/null | grep -qE "\"pid\": *$P([^0-9]|$)" && hyprctl dispatch focuswindow pid:$P >/dev/null 2>&1]=],
        [=[hyprctl dispatch focuswindow class:"$1"]=])
    elseif vim.env.SWAYSOCK and vim.fn.executable("swaymsg") == 1 then
      -- swaymsg replies {"success":true} when the command RAN, even if [pid=$P]
      -- matched no container — so checking success would stop the walk at nvim's
      -- own PID. Confirm a container actually owns $P (it's in the tree) before
      -- focusing, so a miss climbs to the terminal ancestor (then the app_id
      -- fallback). [^0-9]|$ keeps $P from matching a longer PID.
      walk([=[swaymsg -t get_tree 2>/dev/null | grep -qE "\"pid\": *$P([^0-9]|$)" && swaymsg "[pid=$P] focus" >/dev/null 2>&1]=],
        [=[swaymsg "[app_id=$1] focus"]=])
    elseif vim.fn.executable("kdotool") == 1 then
      -- KDE/KWin (Wayland). XDG_CURRENT_DESKTOP=KDE identifies the session, but
      -- kdotool (a KWin scripting bridge) is what drives the window, so its
      -- presence is the hard requirement. `--all` is essential: it searches
      -- EVERY virtual desktop/activity, so the jump finds THIS nvim even when it
      -- sits on a different desktop than the focused viewer. Without it kdotool
      -- only searches the current desktop, so the --pid match misses and the
      -- --class fallback activates the wrong nvim window. kdotool prints the id
      -- as `{uuid}`; activate only when the match looks like one, so an empty or
      -- odd result lets the process-tree walk keep climbing.
      walk([=[W=$(kdotool search --all --pid "$P" 2>/dev/null | head -1); case "$W" in \{*\}) kdotool windowactivate "$W";; *) false;; esac]=],
        [=[kdotool search --all --class "$1" | head -1 | xargs -r kdotool windowactivate]=])
    end
    return
  end
  -- X11: the host terminal exports $WINDOWID for its own window — the most
  -- precise signal, so try it first. Then the PID walk, then class. But
  -- $WINDOWID is captured at spawn and inherited verbatim under a multiplexer:
  -- a tmux/screen pane keeps the WINDOWID of whichever terminal first started
  -- the server, so after reattaching from another terminal it points at the
  -- wrong (or a closed) window. Skip it under $TMUX/$STY and let the PID walk /
  -- class fallback below find the live window instead.
  local winid = vim.env.WINDOWID
  local multiplexed = (vim.env.TMUX and vim.env.TMUX ~= "")
    or (vim.env.STY and vim.env.STY ~= "")
  if winid and winid ~= "" and not multiplexed and vim.fn.executable("xdotool") == 1 then
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
    -- Opening the target fires BufEnter synchronously; keep the jumping
    -- daemon active across it (the target is usually an \input of its project).
    in_jump = true
    pcall(vim.cmd, "edit " .. vim.fn.fnameescape(file))
    in_jump = false
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

local function daemon_count()
  local n = 0
  for _ in pairs(daemons) do n = n + 1 end
  return n
end

-- Absolute, symlink-resolved path — matches the canonical paths the daemon
-- reports in /debug `watched` (e.g. macOS /tmp → /private/tmp), so the plugin
-- can match a buffer against a daemon's watched-file set.
local function canon(p)
  if not p or p == "" then return p end
  return vim.fn.resolve(vim.fn.fnamemodify(p, ":p"))
end

-- Refresh a daemon entry's watched-file set (root + \input/\include + bib) from
-- its /debug, so routing knows which daemon owns which file. Async, best-effort;
-- ignored if the daemon was replaced/stopped meanwhile.
local function fetch_watched(entry)
  local debug_url = "http://127.0.0.1:" .. tostring(entry.port) .. "/debug"
  run_system({ "curl", "--silent", "--max-time", "2", debug_url }, {}, function(res)
    if daemons[entry.root] ~= entry then return end
    if not (res and res.code == 0 and res.stdout and res.stdout ~= "") then return end
    local ok, data = pcall(vim.json.decode, res.stdout)
    if not ok or type(data) ~= "table" then return end
    local set = {}
    if type(data.root) == "string" then set[canon(data.root)] = true end
    if type(data.watched) == "table" then
      for _, p in ipairs(data.watched) do
        if type(p) == "string" then set[canon(p)] = true end
      end
    end
    entry.watched = set
  end)
end

-- The registry entry whose daemon is currently "active" (mirrored by the
-- globals), or nil.
local function active_entry()
  return daemon_root and daemons[daemon_root] or nil
end

-- The daemon that OWNS a file: a previewed root (exact registry key), else the
-- daemon whose watched-set contains it (an \input/\include of that project).
-- This is what makes editing any project file route to the right tab even with
-- several projects open at once. nil if no running daemon owns it.
local function daemon_for_file(path)
  if not path or path == "" then return nil end
  local entry = daemons[path]
  if entry then return entry end
  local c = canon(path)
  for _, d in pairs(daemons) do
    if d.watched and d.watched[c] then return d end
  end
  return nil
end

-- Point the active globals (and the jump poll) at `entry`'s daemon, so pushes,
-- cursor/selection sync, and source-jumps route to it. Saves the outgoing
-- daemon's jump sequence first. No-op if `entry` is already active.
local function activate(entry)
  if daemon_root == entry.root then return end
  local prev = active_entry()
  if prev then prev.jump_seq = last_jump_seq end
  stop_jump_poll()
  daemon_job = entry.job
  daemon_port = entry.port
  daemon_root = entry.root
  last_jump_seq = entry.jump_seq or 0
  set_urls(entry.port)
  start_jump_poll()
  fetch_watched(entry)  -- keep this daemon's owned-file set current
  -- The dedup cache assumes the RECEIVER already shows the last-sent pattern;
  -- a different daemon's tab hasn't seen it. Invalidate and push the current
  -- search state so the newly-active tab mirrors it immediately (BufEnter
  -- doesn't guarantee a following CursorMoved).
  last_search_sent = nil
  post_search()
end

-- The current buffer has no daemon: clear the active globals so pushes no-op.
local function deactivate()
  if not daemon_root then return end  -- already inactive — skip needless churn
  local prev = active_entry()
  if prev then prev.jump_seq = last_jump_seq end
  stop_jump_poll()
  daemon_job = nil
  daemon_port = nil
  daemon_root = nil
  last_search_sent = nil
end

-- Keep the active daemon in step with the buffer you're in (called on
-- BufEnter). Switch ONLY when you enter a file that has its OWN daemon (a
-- previewed root). Otherwise keep the current active daemon: the buffer may be
-- an \input/\include of its project — which the daemon watches, so edits must
-- still push — and even an unrelated buffer is harmless (the daemon ignores
-- pushes for files it doesn't watch, as before). We never deactivate here;
-- a daemon only goes inactive when it actually stops (on_exit / stop_all).
local function activate_for_current_buffer()
  if in_jump then return end
  -- Route to the daemon that OWNS this buffer (its root, or a project whose
  -- \input/\include set contains it). If none owns it, keep the current active
  -- daemon (harmless: an unrelated file is rejected by the daemon).
  local entry = daemon_for_file(vim.api.nvim_buf_get_name(0))
  if entry then activate(entry) end
end

-- Tear down on nvim exit (VimLeavePre): stop every daemon AND close its native
-- window. With `close_on_exit = false` we do nothing — the daemon and window
-- were spawned detached, so they outlive nvim and the preview stays live
-- (`:MathPreviewStop` still stops them explicitly). A browser tab can't be
-- closed either way; `close_on_exit = false` just keeps its daemon alive so the
-- tab keeps working.
local function stop_all()
  if not config.close_on_exit then return end
  for root, d in pairs(daemons) do
    stopping[root] = true
    if d.window_job then
      -- Suppress the SIGTERM-as-crash notification (mirrors M.stop).
      d.window_stopping = true
      pcall(vim.fn.jobstop, d.window_job)
      d.window_job = nil
    end
    pcall(vim.fn.jobstop, d.job)
    daemons[root] = nil
  end
  deactivate()
  pcall(vim.api.nvim_del_augroup_by_name, "mathpreview")
end

-- Forward declaration: the BufDelete autocmd below closes over it, but its
-- body needs detach_autocmds, which is defined after attach_autocmds.
local stop_entry

-- Attach the TextChanged / CursorMoved autocmds. The push/cursor/selection
-- callbacks route to whichever daemon is active (kept in sync with the current
-- buffer by the BufEnter handler below); the per-daemon jump poll is started by
-- activate(), not here. Idempotent — clears the augroup before re-creating it.
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
        -- Mirror the `/` search hlsearch (catches n / N / * — they move the
        -- cursor); deduped, so this is cheap on every move.
        post_search()
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
  -- Search-as-you-type: stream the partial `/` / `?` pattern to the preview on
  -- each cmdline keystroke (debounced), like 'incsearch' in the buffer.
  vim.api.nvim_create_autocmd("CmdlineChanged", {
    group = "mathpreview",
    pattern = { "/", "?" },
    callback = function(args)
      if vim.tbl_contains(config.filetypes, vim.bo[args.buf].filetype) then
        debounced_search_preview()
      end
    end,
  })
  -- Promptly mirror `/` / `?` (new pattern) and `:noh` (clear) — those don't
  -- always move the cursor, so CursorMoved alone would lag. Deferred so @/ and
  -- v:hlsearch reflect the command's result. Also settles whatever the
  -- incremental preview stream left when the search was committed or aborted.
  vim.api.nvim_create_autocmd("CmdlineLeave", {
    group = "mathpreview",
    pattern = { "/", "?", ":" },
    callback = function(args)
      if vim.tbl_contains(config.filetypes, vim.bo[args.buf].filetype) then
        vim.schedule(post_search)
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
  -- Keep the active daemon in sync with the buffer you're in, so edits/cursor/
  -- jumps route to that file's daemon (and tab). Buffers with no daemon
  -- deactivate pushing until you :MathPreview them.
  vim.api.nvim_create_autocmd({ "BufEnter", "BufWinEnter" }, {
    group = "mathpreview",
    pattern = "*",
    callback = function() activate_for_current_buffer() end,
  })
  -- `:bd` / `:bw` on the previewed document closes its preview (daemon +
  -- native window) — deleting the buffer is an explicit "done with this file".
  -- ROOT buffers only: deleting an \input'd child of a multi-file project
  -- keeps the preview, which renders the root document, alive. This is
  -- deliberate regardless of `close_on_exit` (that option is about nvim
  -- exiting; this is an in-session action on the document itself).
  vim.api.nvim_create_autocmd({ "BufDelete", "BufWipeout" }, {
    group = "mathpreview",
    pattern = "*",
    callback = function(args)
      local file = vim.api.nvim_buf_get_name(args.buf)
      if file == "" then return end
      local c = canon(file)
      for root, d in pairs(daemons) do
        if canon(root) == c then
          stop_entry(d)
          return
        end
      end
    end,
  })
  vim.api.nvim_create_autocmd("VimLeavePre", {
    group = "mathpreview",
    callback = function() stop_all() end,
  })
end

local function detach_autocmds()
  pcall(vim.api.nvim_del_augroup_by_name, "mathpreview")
end

-- Stop ONE preview session — its daemon and (if any) its native window.
-- Shared by :MathPreviewStop (M.stop) and the :bd autocmd. Declared as a
-- forward local above attach_autocmds.
function stop_entry(entry)
  if not entry or not daemons[entry.root] then return end
  stopping[entry.root] = true
  daemons[entry.root] = nil
  if daemon_root == entry.root then
    stop_jump_poll()
    daemon_job = nil
    daemon_port = nil
    daemon_root = nil
  end
  pcall(vim.fn.jobstop, entry.job)  -- its on_exit is now a no-op (already deregistered)
  -- Close the native window too — it's tied to this preview session. Flag the
  -- deliberate kill so its on_exit doesn't misreport the SIGTERM exit as a
  -- crash (WebKitGTK writes stderr noise that would otherwise trip the warning
  -- on every stop/restart on Linux).
  if entry.window_job then
    entry.window_stopping = true
    pcall(vim.fn.jobstop, entry.window_job)
    entry.window_job = nil
  end
  if daemon_count() == 0 then detach_autocmds() end
end

-- Spawn the daemon for the current buffer using an already-resolved binary
-- path `cmd`. Callers reach this through ensure_binary(), which guarantees
-- the binary exists and is current first.
local function start_with(cmd, opts)
  -- Prefer the root pinned by the caller (M.start / restart) so an async binary
  -- resolve or a buffer switch in between can't re-root us onto another file.
  local root, err = opts.root, nil
  if not root then root, err = current_root() end
  -- The in-flight start for this root has resolved; clear its guard now that
  -- we're committing to (or bailing out of) the actual spawn.
  if root then starting[root] = nil end
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
      -- Detached only when the preview should outlive nvim (close_on_exit=off),
      -- so nvim's exit doesn't take the daemon with it. jobstop still reaches a
      -- detached process, so `:MathPreviewStop`/restart work regardless.
      detach = not config.close_on_exit,
      on_stderr = function(_, data)
        if data then vim.list_extend(stderr_lines, data) end
      end,
      on_exit = function(jid, code)
        -- A late exit from a job we've already replaced under the same root
        -- (restart spawned a new daemon before the old SIGTERM landed): the new
        -- registrant owns this root now, so don't tear down its state.
        local cur = daemons[root]
        if cur and cur.job ~= jid then return end
        local exited_root = root
        local was_stopping = stopping[root]
        stopping[root] = nil
        -- Daemon died on its own (crash/OOM/external kill): close its native
        -- window too, so it doesn't linger as a frozen ghost retrying a dead
        -- port and outlive the plugin's reach (M.stop already handles the
        -- deliberate path, hence the `not was_stopping` guard). Use `cur`, not
        -- the yet-to-be-declared `entry` local.
        if not was_stopping and cur and cur.window_job then
          cur.window_stopping = true
          pcall(vim.fn.jobstop, cur.window_job)
          cur.window_job = nil
        end
        daemons[root] = nil
        if daemon_root == root then
          -- The active daemon died: clear the globals (don't save its now-gone
          -- jump seq back into a removed entry).
          stop_jump_poll()
          daemon_job = nil
          daemon_port = nil
          daemon_root = nil
        end
        if daemon_count() == 0 then detach_autocmds() end
        -- A deliberate stop/restart: don't treat the SIGTERM as a crash.
        if was_stopping then return end
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
  -- Register this file's daemon and make it the active one. attach_autocmds is
  -- global (push routing) — only needed once, when the first daemon starts.
  local first = daemon_count() == 0
  local entry =
    { job = job, port = port, root = root, cmd = cmd, opened = false, jump_seq = 0, window_job = nil }
  stopping[root] = nil
  daemons[root] = entry
  if first then attach_autocmds() end
  -- Activate only if this daemon serves the buffer you're actually in;
  -- otherwise sync to the current buffer (it may have changed during the async
  -- binary resolve / restart gap).
  if vim.api.nvim_buf_get_name(0) == root then
    activate(entry)
  else
    activate_for_current_buffer()
  end
  -- Populate this daemon's watched-file set once its first render has resolved
  -- the project's \input/\include graph (activate's immediate fetch can race the
  -- initial render and come back with just the root).
  vim.defer_fn(function()
    if daemons[root] == entry then fetch_watched(entry) end
  end, 700)
  -- Skip the open on a restart that rebound the same port: the existing tab
  -- reconnects on its own (opening another would pile up duplicates). The
  -- native window, unlike a browser tab, is killed on restart (it's tied to
  -- the session), so it must be re-opened — don't take this shortcut for it.
  local reused_tab = opts.prev_port ~= nil and port == opts.prev_port
    and config.viewer ~= "window"
  if config.auto_open_browser then
    if reused_tab then
      entry.opened = true
    else
      -- A previous session's tab (still open in the browser, retrying its
      -- WebSocket every 1s) reconnects to this rebound port and hard-reloads
      -- itself. Wait for that, then reuse_or_open_browser opens a fresh tab
      -- only if none reconnected (clients == 0). Also covers the daemon's
      -- ~100-300ms bind time. Set stale_tab_wait_ms = 0 to skip the wait
      -- (opens immediately; a stale tab would then become a duplicate).
      local wait = config.stale_tab_wait_ms
      if wait == nil then wait = 1500 end
      vim.defer_fn(function()
        -- Only if this exact daemon is still the one registered for the root.
        if daemons[root] == entry then reuse_or_open_browser(entry) end
      end, wait)
    end
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
local function ensure_binary(root, on_ready)
  local cmd = resolve_cmd()
  local can_build = is_source_checkout() and vim.fn.executable("cargo") == 1

  if not cmd then
    if can_build then
      auto_install(function(ok)
        local got = resolve_cmd()
        if ok and got then
          on_ready(got)
        else
          -- on_ready won't run; release the in-flight start guard for this root.
          starting[root] = nil
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
      starting[root] = nil
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
    -- Proceed with the current binary once it's confirmed current AND (for the
    -- window viewer) confirmed to have the `gui` feature. Reinstalls when
    -- older than the plugin, or when viewer='window' but the binary was built
    -- without gui (so `view` is missing) — the reinstall picks up --features
    -- gui via cargo_install_args().
    local function proceed_if_gui_ok()
      if config.viewer ~= "window" then
        on_ready(cmd)
        return
      end
      -- `view --help` exits 0 iff the gui feature is compiled in.
      run_system({ cmd, "view", "--help" }, {}, function(vres)
        if vres and vres.code == 0 then
          on_ready(cmd)
        else
          vim.notify("mathpreview: installing the native-window (gui) build…",
            vim.log.levels.INFO)
          auto_install(function(ok) on_ready((ok and resolve_cmd()) or cmd) end)
        end
      end)
    end
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
        proceed_if_gui_ok()
      end
    end)
    return
  end

  check_binary_version(cmd)  -- async; warns once if a user/$PATH binary is stale
  on_ready(cmd)
end

function M.start(opts)
  opts = opts or {}
  -- Optional per-command viewer choice (`:MathPreview window|browser`). It sets
  -- the session default (sticky) so the reuse/open/install paths — which all
  -- read config.viewer — agree for this and later previews until changed again.
  if opts.viewer and opts.viewer ~= "" then
    if opts.viewer ~= "browser" and opts.viewer ~= "window" then
      vim.notify("mathpreview: viewer must be 'browser' or 'window' (got '"
        .. tostring(opts.viewer) .. "')", vim.log.levels.ERROR)
      return
    end
    config.viewer = opts.viewer
  end
  local root, err = opts.root, nil
  if not root then root, err = current_root() end
  if not root then
    vim.notify("mathpreview: " .. err, vim.log.levels.ERROR)
    return
  end
  -- Already serving THIS file → make it active and reuse its tab (don't open a
  -- duplicate). A different file falls through and starts its own daemon + tab.
  local entry = daemons[root]
  if entry and entry.job then
    activate(entry)
    if config.auto_open_browser then reuse_or_open_browser(entry) end
    return
  end
  -- A start for THIS file is already resolving its binary/port asynchronously;
  -- don't kick off a second one for it (would race find_free_port). A different
  -- file can still start concurrently (per-root guard).
  if starting[root] then return end
  starting[root] = true
  opts.root = root  -- pin the root across the async binary resolve / buffer change
  ensure_binary(root, function(cmd) start_with(cmd, opts) end)
end

-- The daemon for the current buffer's file, else the active one (so :MathStop /
-- :MathRestart in a non-.tex buffer still act on the preview you were last in).
local function target_entry()
  return daemon_for_file(vim.api.nvim_buf_get_name(0)) or active_entry()
end

function M.stop()
  stop_entry(target_entry())
end

function M.restart()
  local entry = target_entry()
  if not entry then M.start() return end
  -- Remember the port so the restart can rebind it and let the open tab
  -- reconnect, instead of opening a fresh one each time.
  local prev_port, root = entry.port, entry.root
  M.stop()
  -- Give the OS a moment to release the port before re-binding.
  vim.defer_fn(function() M.start({ prev_port = prev_port, root = root }) end, 200)
end

function M.status()
  local age = uv.hrtime() / 1e6 - last_status.last_push_ms
  local buf = vim.api.nvim_get_current_buf()
  return {
    daemon_running = daemon_job ~= nil,
    daemons_running = daemon_count(),  -- total across all open files
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
