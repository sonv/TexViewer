-- mathpreview.nvim — companion plugin for the `mathpreview-cli` binary.
--
-- Drop into your plugin manager (lazy / packer / vim-plug) by pointing at
-- this repo. The user-facing commands are registered by plugin/mathpreview.lua;
-- this file holds the implementation that those commands lazy-require:
--
--     :MathPreview         start the daemon for the current TeX or Markdown
--                          buffer and open the viewer in a browser tab
--     :MathPreviewStop     kill the daemon
--     :MathPreviewRestart  stop + start
--     :MathPreviewClean    find + stop abandoned daemons (no editor/viewer)
--     :MathPreviewStatus   echo PID, port, push counters, version handshake
--     :MathPreviewDebug    echo resolved settings + config/macro paths
--
-- The plugin spawns the daemon as a background job, finds the first free
-- port starting at 23636, opens the viewer, then debounces buffer pushes
-- on TextChanged. By default VimLeavePre kills the spawned daemon so quitting
-- nvim doesn't leave a stray server bound to
-- the port; set `close_on_exit = false` to deliberately let the preview
-- outlive nvim instead (see the config comment).

local M = {}

local DEFAULT_PORT = 23636
local PORT_SCAN_RANGE = 16  -- try 23636..23651 before giving up

-- Version this plugin checkout expects the `mathpreview-cli` binary to be.
-- On :MathPreview we compare it against the binary's `--version`; if this is
-- managed by the selected installer, a stale/missing binary is (re)installed
-- to match (see ensure_binary). User-owned `cmd` paths are only checked and
-- warned about — the signal that a fix you "released" isn't actually the
-- binary you're running.
-- RELEASE: bump this in lockstep with Cargo.toml / Cargo.lock / CHANGELOG.
local PLUGIN_VERSION = "2.1.42"

local config = {
  cmd = nil,                              -- resolved at start; "mathpreview-cli" by default
  -- How the plugin installs a missing/stale managed binary:
  --   "cargo"  (default) — compile this checkout with the Rust toolchain
  --   "github"           — download + verify the matching release binary
  -- An explicit `cmd` bypasses both methods.
  install_method = "cargo",
  -- Neovim reports both `.md` and `.markdown` files as `markdown`.
  filetypes = { "tex", "plaintex", "latex", "markdown" },
  -- Hostname for the BROWSER-facing viewer URL. Any `*.localhost` name (or
  -- `localhost` / `127.0.0.1`) passes the daemon's Host guard; a distinct
  -- name gets its own per-site browser settings (zoom, vimium, dark mode),
  -- so e.g. `viewer_host = "thesis.localhost"` separates one project's
  -- preview from the rest. Plugin-internal requests always use 127.0.0.1.
  viewer_host = "mathpreview.localhost",
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
  -- Set false to start the daemon without opening a browser tab.
  -- What happens to the preview when you quit nvim.
  --   true  (default) — tear it down, peek.nvim-style: stop the daemon and ask
  --                     the browser tab to close too. The
  --                     dying daemon broadcasts a "bye" WS event and the page
  --                     closes itself — browsers allow window.close() for a
  --                     tab whose session history has a single entry, which a
  --                     freshly opened preview tab has. (If you navigated in
  --                     that tab, the browser refuses and the tab just shows
  --                     "preview ended".)
  --   false           — leave the preview RUNNING so it outlives nvim: the
  --                     daemon and tab stay live and fully usable until
  --                     you close them or run `:MathPreviewStop`. Use this to
  --                     keep reading the rendered doc after quitting the
  --                     editor. Implemented by spawning the daemon detached so
  --                     nvim's exit doesn't kill it;
  --                     `:MathPreviewStop` still stops them explicitly.
  close_on_exit = true,
  -- Where Cargo drops the auto-built binary. nil → cargo's default
  -- (`$CARGO_HOME`, usually `~/.cargo`); a value is passed to
  -- `cargo install --root`. GitHub binaries always use a versioned directory
  -- under `stdpath("data")/mathpreview`. Both are run by absolute path.
  install_root = nil,
  -- On the first :MathPreview of a session, scan the port range for preview
  -- daemons that look abandoned — no editor attached and no viewer tab open
  -- (e.g. leftovers from crashed sessions, or pre-1.0.2 daemons that never
  -- died with nvim) — and offer to stop them. `:MathPreviewClean` runs the
  -- same sweep on demand. Set to false to never be asked.
  stale_check = true,
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

-- One daemon (and browser tab) per root document, so opening another supported
-- file and running :MathPreview creates its own viewer instead of reusing the
-- first file's.
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
local daemon_root = nil  -- active daemon's root document path
-- Roots we're deliberately stopping (M.stop/M.restart), so the daemon's on_exit
-- doesn't mistake the SIGTERM for a port-bind race and auto-restart it.
local stopping = {}

-- Push state (carried across pushes for :MathPreviewStatus). Timers and
-- in-flight uploads are per buffer: editing document B must not cancel or
-- reroute a still-pending update for document A.
local push_timers = {}
local cursor_timer = nil
local selection_timer = nil
-- `vim.system` handles keyed by bufnr. A newer snapshot of the SAME buffer can
-- supersede its older upload, while different buffers remain independent.
-- Values are nil on the jobstart fallback path (no cancellable handle).
local push_jobs = {}
local last_jump_seq = 0
-- When each buffer last changed (ms, keyed by bufnr). Cursor posts within
-- TYPING_WINDOW_MS of an edit IN THE SAME BUFFER are tagged `typing: true`
-- so the viewer stays calm — it follows the cursor but doesn't flash the
-- under-cursor box on every keystroke. Cursor moves without a nearby edit
-- are navigation, which keeps the flash. Per-buffer so editing one split
-- doesn't mis-tag a navigation made in another buffer right after.
local last_text_change = {}
local TYPING_WINDOW_MS = 500
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
local viewer_migration_warned = false

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

-- Rust target triple used by the release workflow for this host. Keep this in
-- lockstep with `.github/workflows/release.yml` and scripts/install-prebuilt.sh.
-- nil means GitHub Releases do not currently carry a compatible binary.
local function prebuilt_target()
  local uname = uv.os_uname()
  local sysname = uname and uname.sysname or ""
  local machine = uname and uname.machine or ""
  if sysname == "Darwin" then
    if machine == "arm64" or machine == "aarch64" then return "aarch64-apple-darwin" end
    if machine == "x86_64" or machine == "amd64" then return "x86_64-apple-darwin" end
  elseif sysname == "Linux" then
    if machine == "arm64" or machine == "aarch64" then return "aarch64-unknown-linux-gnu" end
    if machine == "x86_64" or machine == "amd64" then return "x86_64-unknown-linux-gnu" end
  end
  return nil
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

-- GitHub binaries live outside the plugin checkout so plugin-manager updates
-- cannot delete them. The default is version + target scoped: two nvim
-- instances pinned to different releases (or sharing a data dir across hosts)
-- never replace each other's executable.
local function github_install_prefix()
  return vim.fn.stdpath("data") .. "/mathpreview/" .. PLUGIN_VERSION
    .. "/" .. (prebuilt_target() or "unsupported")
end

local function github_bin_dir()
  return github_install_prefix() .. "/bin"
end

-- `cargo build --release` artifact inside this checkout. Kept only as a
-- last-resort fallback for contributors who `cargo build` without installing.
local function bundled_binary_path()
  return plugin_root() .. "/target/release/" .. exe_name()
end

-- The `cargo install` command used to (re)install the browser-only daemon.
local function cargo_install_args()
  local args = { "cargo", "install", "--path", "crates/cli", "--force" }
  if config.install_root and config.install_root ~= "" then
    table.insert(args, "--root")
    table.insert(args, vim.fn.expand(config.install_root))
  end
  return args
end

local function github_install_args()
  return {
    "sh",
    plugin_root() .. "/scripts/install-prebuilt.sh",
    PLUGIN_VERSION,
    github_install_prefix(),
  }
end

-- Capture every install decision before starting async work. setup() may be
-- called again while an install is running; callbacks for that install must
-- keep using the method and destination that its caller selected.
local function install_spec()
  local method = config.install_method
  local bin_dir = method == "github" and github_bin_dir() or cargo_bin_dir()
  local binary_path = bin_dir .. "/" .. exe_name()
  local args = method == "github" and github_install_args() or cargo_install_args()
  return {
    method = method,
    bin_dir = bin_dir,
    binary_path = binary_path,
    args = args,
    -- A destination can have only one installer at a time. Distinct methods
    -- or prefixes queue independently and never inherit one another's waiters.
    key = method .. "\n" .. binary_path,
  }
end

-- True if this checkout has the Rust sources we'd need to compile the binary
-- (i.e. it's a git/source install, not a binary-only drop).
local function is_source_checkout()
  return vim.fn.isdirectory(plugin_root() .. "/crates/cli") == 1
end

local function resolve_cmd(spec)
  -- Precedence: explicit override > the selected plugin-managed binary (by
  -- absolute path) > PATH > in-checkout build. GitHub mode is deliberately
  -- exclusive: selecting it must not silently pick up a Cargo/PATH binary and
  -- skip the requested download.
  if config.cmd and config.cmd ~= "" then return config.cmd end
  spec = spec or install_spec()
  local installed = spec.binary_path
  if vim.fn.executable(installed) == 1 then return installed end
  if spec.method == "github" then return nil end
  if vim.fn.executable("mathpreview-cli") == 1 then return "mathpreview-cli" end
  local bundled = bundled_binary_path()
  if vim.fn.executable(bundled) == 1 then return bundled end
  return nil
end

-- True if `cmd` is a binary the plugin installs/builds itself (so it's safe
-- to reinstall on skew). A user-supplied `cmd` or an unrelated $PATH binary
-- is left alone.
local function is_managed(cmd, spec, explicit_cmd)
  if explicit_cmd then return false end
  return cmd == spec.binary_path
    or (spec.method == "cargo" and cmd == bundled_binary_path())
end

-- Whether the selected managed executable itself (not merely some executable
-- with the same name) is what the shell resolves from $PATH.
local function managed_binary_on_path(spec)
  spec = spec or install_spec()
  local found = vim.fn.exepath("mathpreview-cli")
  if not found or found == "" then return false end
  local actual = vim.fn.resolve(vim.fn.fnamemodify(found, ":p"))
  local expected = vim.fn.resolve(vim.fn.fnamemodify(spec.binary_path, ":p"))
  return actual == expected
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
    local function start_vim_system(system_opts, callback)
      local ok, handle = pcall(vim.system, args, system_opts, callback)
      if ok then return handle end
      if on_done then
        vim.schedule(function()
          on_done({ code = -1, stdout = "", stderr = tostring(handle) })
        end)
      end
      return nil
    end
    if not on_line then
      -- Returned so a caller can cancel a superseded request (see push_buffer).
      return start_vim_system(opts, function(res)
        if on_done then vim.schedule(function() on_done(res) end) end
      end)
    end
    -- Streaming variant: forward each stderr line live, still capturing the
    -- full stderr for the final result.
    local err, buf = {}, ""
    start_vim_system({
      cwd = opts.cwd,
      stdin = opts.stdin,
      timeout = opts.timeout,
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
  local job, timeout_timer
  local exited, timed_out = false, false
  local function close_timeout()
    if not timeout_timer then return end
    timeout_timer:stop()
    timeout_timer:close()
    timeout_timer = nil
  end
  local job_opts = {
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
      exited = true
      close_timeout()
      if timed_out then
        code = 124
        stderr[#stderr + 1] = "command timed out after " .. opts.timeout .. " ms"
      end
      if on_done then
        on_done({
          code = code,
          stdout = table.concat(stdout, "\n"),
          stderr = table.concat(stderr, "\n"),
        })
      end
    end,
  }
  local started, job_or_err = pcall(vim.fn.jobstart, args, job_opts)
  if not started then
    if on_done then
      on_done({ code = -1, stdout = "", stderr = tostring(job_or_err) })
    end
    return
  end
  job = job_or_err
  if job <= 0 then
    if on_done then on_done({ code = -1, stderr = "could not start " .. tostring(args[1]) }) end
    return
  end
  if opts.timeout and opts.timeout > 0 and not exited then
    timeout_timer = uv.new_timer()
    timeout_timer:start(opts.timeout, 0, vim.schedule_wrap(function()
      if exited then return end
      timed_out = true
      vim.fn.jobstop(job)
    end))
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
  run_system({ cmd, "--version" }, { timeout = 5000 }, function(res)
    if not res or res.code ~= 0 or not res.stdout then return end
    -- clap prints "mathpreview-cli 0.1.27"; take the last whitespace token.
    local ver = res.stdout:gsub("%s+$", ""):match("(%S+)%s*$")
    if not ver then return end
    last_status.binary_version = ver
    if version_warned or semver_cmp(ver, PLUGIN_VERSION) == 0 then return end
    version_warned = true
    local rel = semver_cmp(ver, PLUGIN_VERSION) < 0 and "older than" or "newer than"
    vim.notify(
      ("mathpreview: binary is %s (%s the plugin's expected %s). Update that "
        .. "executable, or remove `cmd` and choose `install_method = \"cargo\"` "
        .. "or `\"github\"`; then run :MathPreviewRestart.")
        :format(ver, rel, PLUGIN_VERSION),
      vim.log.levels.WARN)
  end)
end

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

-- If the managed bin dir isn't on $PATH, tell the user how to add it (the
-- plugin still runs it via its absolute path; this is only for terminal use).
local function hint_path_if_needed(spec)
  -- The default GitHub path is deliberately versioned and plugin-private; it
  -- should not be added to a shell profile. Cargo's stable bin dir remains a
  -- useful terminal command, so retain the existing hint for that mode.
  if spec.method == "github"
      or path_hinted
      or managed_binary_on_path(spec) then
    return
  end
  path_hinted = true
  local dir = spec.bin_dir
  vim.notify(
    ("mathpreview: installed to %s, which isn't on your $PATH.\n"
      .. "The plugin runs it by absolute path, but for terminal use add it to "
      .. "your shell profile:\n  export PATH=\"%s:$PATH\"\n"
      .. "(or set `install_root` / `cmd` in setup())."):format(dir, dir),
    vim.log.levels.WARN)
end

-- Install mathpreview-cli with the configured method, async. Cargo builds this
-- checkout; GitHub downloads the exact version/target release and verifies its
-- SHA-256 before replacing the managed binary. Both paths notify on start and
-- finish, then call on_done(ok) on the main thread.
local INSTALL_SPINNER = { "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏" }

-- Install requests are serialized, and callers selecting the same captured
-- method + destination share one operation. Requests for another method/path
-- queue separately instead of inheriting the active install's result.
local install_active = nil
local install_requests = {}
local install_queue = {}
local start_install

local function start_next_install()
  if install_active or #install_queue == 0 then return end
  local request = table.remove(install_queue, 1)
  start_install(request)
end

start_install = function(request)
  install_active = request
  local spec = request.spec
  local started = uv.now()
  local frame = 0
  local method = spec.method
  local step = "" -- latest step, e.g. "compiling mathpreview-core"
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
  local detail = method == "github"
      and "downloading verified GitHub release"
    or "compiling with Cargo (~30s)"
  vim.notify(
    ("mathpreview: installing mathpreview-cli to %s (%s) — please wait; "
      .. ":MathPreview will start automatically when it finishes…")
      :format(spec.bin_dir, detail),
    vim.log.levels.INFO)
  -- Live progress on the cmdline (history=false → not added to :messages) so
  -- either one-time install path remains visibly active.
  progress:start(0, 120, vim.schedule_wrap(function()
    if install_active ~= request then return end
    frame = frame + 1
    local secs = math.floor((uv.now() - started) / 1000)
    local spin = INSTALL_SPINNER[(frame % #INSTALL_SPINNER) + 1]
    local action = method == "github" and "downloading" or "building"
    local msg = ("%s %s mathpreview-cli… %ds"):format(spin, action, secs)
    if step ~= "" then msg = msg .. "  " .. step end
    vim.api.nvim_echo({ { msg, "Comment" } }, false, {})
  end))
  local on_stderr_line = function(line)
    local crate = line:match("^%s*Compiling%s+(%S+)")
    if crate then
      step = "compiling " .. crate
    elseif line:match("^%s*Finished") then
      step = "finishing…"
    elseif method == "github" and line:match("^mathpreview:") then
      step = line:gsub("^mathpreview:%s*", "")
    end
  end
  run_system(
    spec.args,
    { cwd = plugin_root(), on_stderr_line = on_stderr_line },
    function(res)
      if install_active == request then install_active = nil end
      stop_progress()
      local ok = res and res.code == 0
      if ok then
        last_status.binary_version = PLUGIN_VERSION
        vim.notify("mathpreview: install complete — starting daemon.", vim.log.levels.INFO)
        hint_path_if_needed(spec)
      else
        vim.notify(
          ("mathpreview: %s install failed (exit %s). See :messages; switch "
            .. "`install_method`, install manually (README), or set `cmd`.\n%s"):format(
            method, res and res.code or "?", (res and res.stderr or ""):gsub("%s+$", "")),
          vim.log.levels.ERROR)
      end
      install_requests[spec.key] = nil
      -- Fan out to every caller that selected this exact install. Pass the
      -- captured destination so callbacks never re-read a later setup().
      local callback_errors = {}
      for _, cb in ipairs(request.waiters) do
        local callback_ok, callback_err = pcall(cb, ok, spec.binary_path)
        if not callback_ok then callback_errors[#callback_errors + 1] = callback_err end
      end
      start_next_install()
      for _, callback_err in ipairs(callback_errors) do
        vim.notify(
          "mathpreview: install completion callback failed: " .. tostring(callback_err),
          vim.log.levels.ERROR)
      end
    end)
end

local function auto_install(spec, on_done)
  local request = install_requests[spec.key]
  if request then
    if on_done then request.waiters[#request.waiters + 1] = on_done end
    return
  end
  request = { spec = spec, waiters = on_done and { on_done } or {} }
  install_requests[spec.key] = request
  if install_active then
    install_queue[#install_queue + 1] = request
  else
    start_install(request)
  end
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
    -- luv historically returned 0 on success; Neovim 0.12's luv returns nil
    -- with no error. Accept both forms while still rejecting soft
    -- `(nil, error)` / `(false, error)` failures and raised errors.
    local bound, bind_err = sock:bind("127.0.0.1", port)
    assert(bound ~= false and bind_err == nil, bind_err)
    local listening, listen_err = sock:listen(128, function() end)
    assert(listening ~= false and listen_err == nil, listen_err)
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

-- ---------------------------------------------------------------------------
-- Stale-daemon sweep: find preview daemons in the port range that nothing is
-- using anymore — crashed sessions, pre-1.0.2 daemons that never died with
-- nvim — and offer to stop them. A daemon is a candidate when no viewer tab
-- is connected (`clients == 0`) AND it reports no recent editor contact
-- (`editor_active == false`, 1.0.6+). Older daemons can't report editor
-- state; they're listed as "state unknown" when unviewed and the user
-- decides. Daemons owned by THIS session are always skipped.

-- Collect /debug from every port in the scan range (async; cb gets a list).
local function scan_daemons(cb)
  local own = {}
  for _, e in pairs(daemons) do
    if e.port then own[e.port] = true end
  end
  local results, pending, launched = {}, 0, false
  local function finish_one()
    pending = pending - 1
    if launched and pending == 0 then cb(results) end
  end
  for port = DEFAULT_PORT, DEFAULT_PORT + PORT_SCAN_RANGE - 1 do
    if not own[port] then
      pending = pending + 1
      run_system({
        "curl", "--silent", "--max-time", "1",
        "http://127.0.0.1:" .. tostring(port) .. "/debug",
      }, {}, function(res)
        if res and res.code == 0 and res.stdout and res.stdout ~= "" then
          local ok, d = pcall(json_decode, res.stdout)
          if ok and type(d) == "table" and d.root then
            table.insert(results, {
              port = port,
              root = tostring(d.root),
              version = d.version and tostring(d.version) or nil,
              clients = tonumber(d.clients) or 0,
              editor_active = d.editor_active, -- nil on pre-1.0.6 daemons
            })
          end
        end
        finish_one()
      end)
    end
  end
  launched = true
  if pending == 0 then cb(results) end
end

-- POST /stop to a daemon (the topbar stop endpoint — honored by every
-- released version, so old strays are stoppable too).
local function stop_daemon_at(port, cb)
  run_system({
    "curl", "--silent", "--max-time", "2", "-X", "POST",
    "http://127.0.0.1:" .. tostring(port) .. "/stop",
  }, {}, function(res)
    if cb then cb(res and res.code == 0) end
  end)
end

local function sweep_stale_daemons(sweep_opts)
  sweep_opts = sweep_opts or {}
  scan_daemons(function(found)
    local cands = {}
    for _, d in ipairs(found) do
      if d.clients == 0 and d.editor_active ~= true then
        table.insert(cands, d)
      end
    end
    if #cands == 0 then
      if sweep_opts.report_empty then
        vim.notify("mathpreview: no stale preview daemons found", vim.log.levels.INFO)
      end
      return
    end
    local lines = {}
    for _, d in ipairs(cands) do
      table.insert(lines, ("  port %d · %s%s%s"):format(
        d.port,
        vim.fn.fnamemodify(d.root, ":~"),
        d.version and (" · v" .. d.version) or "",
        d.editor_active == nil and " · state unknown (old daemon)" or ""))
    end
    local prompt = (
      "mathpreview: %d preview daemon(s) look abandoned (no editor attached, no viewer open):\n%s\n" ..
      "Stop them? (make sure no other nvim session is previewing these files)")
      :format(#cands, table.concat(lines, "\n"))
    if vim.fn.confirm(prompt, "&Yes\n&No", 1) ~= 1 then return end
    local done, total = 0, #cands
    for _, d in ipairs(cands) do
      stop_daemon_at(d.port, function()
        done = done + 1
        if done == total then
          vim.notify(("mathpreview: stopped %d stale daemon(s)"):format(total),
            vim.log.levels.INFO)
        end
      end)
    end
  end)
end

-- Run at most once per session, a beat after the first successful start (so
-- the prompt never delays the preview itself).
local stale_scan_done = false
local function maybe_sweep_stale_daemons()
  if not config.stale_check or stale_scan_done then return end
  stale_scan_done = true
  vim.defer_fn(function() sweep_stale_daemons() end, 2500)
end

local function set_urls(port)
  -- Plugin-internal endpoints stay on the IP literal: they gain nothing from
  -- the pretty hostname (browser extensions never see them), must not depend
  -- on the OS resolver handling `*.localhost` (browsers resolve it
  -- themselves; getaddrinfo support varies), and an IP dial skips the
  -- try-::1-first hop resolvers add. Only the browser-facing viewer URL uses
  -- mathpreview.localhost (see viewer_url) so per-site extension settings —
  -- zoom, vimium, dark-mode — apply to every preview port at once.
  local base = "http://127.0.0.1:" .. tostring(port)
  config.url = base .. "/buffer"
  config.cursor_url = base .. "/cursor"
  config.selection_url = base .. "/selection"
  config.search_url = base .. "/search"
  config.jump_url = base .. "/jump"
  config.debug_url = base .. "/debug"
end

-- The BROWSER-facing address. `*.localhost` resolves to loopback inside every
-- modern browser (RFC 6761 — no OS resolver involved), and a distinct
-- hostname lets per-site browser settings (zoom, vimium keybindings, dark
-- mode) apply to mathpreview across ports without bleeding into other
-- localhost services. The daemon accepts it: its Host/Origin guard takes any
-- name whose last label is `localhost` — still rebinding-safe, since public
-- DNS cannot resolve the reserved `.localhost` TLD.
-- Hostname label from a root path's basename: lowercase, non-alphanumerics
-- collapsed to `-`, trimmed. "paper1_main.tex" → "paper1-main". Empty (or
-- fully non-alphanumeric) names fall back to "mathpreview".
local function host_stem(root)
  local stem = vim.fn.fnamemodify(root or "", ":t:r"):lower()
  stem = stem:gsub("[^%w]+", "-"):gsub("^%-+", ""):gsub("%-+$", "")
  if stem == "" then stem = "mathpreview" end
  return stem
end

local function viewer_url(port, root)
  local host = config.viewer_host
  if not host or host == "" then host = "mathpreview.localhost" end
  -- `{stem}` expands to the root file's sanitized basename, giving each
  -- paper its own origin (own zoom/extension settings, recognizable tabs):
  -- viewer_host = "{stem}.localhost" → http://paper1-main.localhost:<port>/
  host = host:gsub("{stem}", host_stem(root))
  return "http://" .. host .. ":" .. tostring(port) .. "/"
end

-- The daemon's Host guard only accepts loopback names: any `*.localhost`,
-- `localhost` itself, or a loopback IP. Warn once at setup for anything else
-- — the browser tab would open and immediately see 403s.
local viewer_host_warned = false
local function check_viewer_host()
  local host = config.viewer_host
  if not host or host == "" or viewer_host_warned then return end
  local lower = host:lower()
  local ok = lower == "localhost"
    or lower:match("%.localhost$") ~= nil
    or lower:match("^127%.%d+%.%d+%.%d+$") ~= nil
    or lower == "[::1]"
  if not ok then
    viewer_host_warned = true
    vim.notify(
      ("mathpreview: viewer_host %q is not a loopback name the daemon accepts "
        .. "(use something ending in `.localhost`, or `127.0.0.1`) — the "
        .. "preview tab will get 403s"):format(host),
      vim.log.levels.WARN)
  end
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

-- Open the browser viewer for this daemon.
local function open_viewer(entry)
  entry.opened = true
  open_browser(viewer_url(entry.port, entry.root))
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
  local url = viewer_url(entry.port, entry.root)
  local debug_url = "http://127.0.0.1:" .. tostring(entry.port) .. "/debug"
  local function do_open() open_viewer(entry) end
  local function say_reuse()
    entry.opened = true
    vim.notify("mathpreview: preview already open — reusing tab (" .. url .. ")",
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
        if ok and type(data) == "table" then
          clients = data.clients
          -- Stale-daemon nag. Reuse deliberately skips the reinstall path, so
          -- a long-lived daemon silently survives plugin/binary upgrades and
          -- lacks newer features (e.g. a tab that won't close on quit). The
          -- daemon reports its version in /debug since 1.0.4; an older daemon
          -- has no field at all, which is itself a skew signal.
          local dver = data.version and ("v" .. data.version) or "older than v1.0.4"
          if dver ~= ("v" .. PLUGIN_VERSION) and not entry.version_nagged then
            entry.version_nagged = true
            vim.notify(
              ("mathpreview: the RUNNING preview daemon is %s but the plugin is v%s — "
                .. "run :MathPreviewRestart to upgrade it"):format(dver, PLUGIN_VERSION),
              vim.log.levels.WARN)
          end
        end
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

-- Resolve the entry path for the current document. This is the buffer's own
-- path: the daemon applies format-specific root handling (TeX project walk or
-- single-file Markdown). Auto-spawn needs a concrete path on the command line.
local function current_root()
  local path = vim.api.nvim_buf_get_name(0)
  if path == nil or path == "" then
    return nil, "current buffer has no name; open a TeX or Markdown file first"
  end
  return path, nil
end

local function buffer_cursor(buf)
  local ok, cursor = pcall(vim.api.nvim_buf_get_mark, buf, ".")
  if ok and cursor and cursor[1] > 0 then return cursor end
  if vim.api.nvim_get_current_buf() == buf then
    return vim.api.nvim_win_get_cursor(0)
  end
  return { 1, 0 }
end

-- Push a snapshot of the explicitly captured buffer to the explicitly
-- captured daemon. Never consult the current window or active daemon here:
-- both can change while the debounce timer or async startup is pending.
local function push_buffer(buf, edit_cursor, entry)
  if not entry or daemons[entry.root] ~= entry then return end
  if not buf or not vim.api.nvim_buf_is_valid(buf) then return end
  local path = vim.api.nvim_buf_get_name(buf)
  if path == "" then
    last_status.last_error = "preview buffer has no name"
    return
  end
  local ok, lines = pcall(vim.api.nvim_buf_get_lines, buf, 0, -1, false)
  if not ok then return end
  local body = table.concat(lines, "\n")
  -- TextChanged captures the caret that produced this exact buffer state.
  -- Sampling only after the debounce lets a quick cursor move detach the
  -- caret from the dangerous transient token it just inserted.
  local cursor = edit_cursor or buffer_cursor(buf)
  local args = {
    "curl", "--silent", "--show-error", "--fail-with-body", "--max-time", "5",
    "--header", "X-Mathpreview-Path: " .. path,
    "--header", ("X-Mathpreview-Cursor: %d:%d"):format(cursor[1], cursor[2] + 1),
    "--data-binary", "@-",
    "-X", "POST",
    "http://127.0.0.1:" .. tostring(entry.port) .. "/buffer",
  }
  -- One in-flight push per buffer. A newer snapshot of this buffer supersedes
  -- the older one entirely (the daemon renders whole buffers, never deltas),
  -- but an update for another buffer or daemon must still arrive.
  -- Liveness guard: a job that already exited (but whose on_done hasn't been
  -- delivered yet — vim.system closes the handle in _on_exit, the callback
  -- arrives a tick later via vim.schedule) has a closed uv handle whose kill
  -- would go straight to kill(2) on a possibly-recycled PID. Never signal a
  -- closed handle.
  local previous = push_jobs[buf]
  if previous and previous.kill
      and not (previous.is_closing and previous:is_closing()) then
    pcall(previous.kill, previous, 15)
  end
  -- Declare before the closure is created: `local job = f(function() job end)`
  -- would make the inner `job` a GLOBAL (nil) because the local isn't bound
  -- until after the call, so the per-buffer slot would never clear.
  local job
  job = run_system(args, { stdin = body }, function(res)
    if job and push_jobs[buf] == job then push_jobs[buf] = nil end
    -- Killed by a newer push (vim.system reports signal=15, code=0): not an
    -- error and not this push's status to report.
    if res and res.signal and res.signal ~= 0 then return end
    if res and res.code ~= 0 then
      last_status.last_error = ("curl exit %d: %s"):format(
        res.code or -1, (res.stderr or ""):gsub("%s+$", ""))
    else
      last_status.last_error = nil
    end
  end)
  push_jobs[buf] = job
  last_status.pushes = last_status.pushes + 1
  last_status.last_push_ms = uv.hrtime() / 1e6
end

local function post_cursor()
  if not daemon_job or not config.sync then return end
  local buf = vim.api.nvim_get_current_buf()
  local path = vim.api.nvim_buf_get_name(buf)
  if path == "" then return end
  local cursor = vim.api.nvim_win_get_cursor(0)
  -- `or nil` drops the key entirely when false — older daemons never see it.
  local changed = last_text_change[buf]
  local typing = (changed and (uv.now() - changed) < TYPING_WINDOW_MS) or nil
  local payload = json_encode({
    file = path, line = cursor[1], col = cursor[2] + 1, typing = typing,
  })
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

-- Convert the editor's pattern to the literal query the rendered DOM can
-- search, while preserving the Vim semantics that affect WHICH occurrences
-- match. In particular, `*` produces `\<word\>`; dropping those boundaries
-- made the browser highlight the same letters inside larger words. Escaped
-- backslash pairs are protected before recognizing Vim atoms so `/\\<` stays
-- a literal backslash + angle bracket rather than becoming a word boundary.
local function search_pattern_spec(pattern)
  local s = pattern or ""
  s = s:gsub("\\\\", "\1")

  -- The last explicit case atom wins, matching Vim. Without one, mirror
  -- 'ignorecase' + 'smartcase' instead of forcing every browser match to
  -- case-insensitive as the old literal-search path did.
  local explicit_case = nil
  for atom in s:gmatch("\\([cC])") do explicit_case = atom end

  s = s:gsub("\\[vVcC]", "")
  local whole_start = s:sub(1, 2) == "\\<"
  if whole_start then s = s:sub(3) end
  local whole_end = s:sub(-2) == "\\>"
  if whole_end then s = s:sub(1, -3) end
  s = s:gsub("\\[zZ][se]", "")
  s = s:gsub("\1", "\\")

  local case_sensitive
  if explicit_case == "c" then
    case_sensitive = false
  elseif explicit_case == "C" then
    case_sensitive = true
  else
    case_sensitive = not vim.o.ignorecase
      or (vim.o.smartcase and s:find("%u") ~= nil)
  end

  return {
    query = s,
    whole_start = whole_start,
    whole_end = whole_end,
    case_sensitive = case_sensitive,
  }
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

local function same_search_spec(a, b)
  return a ~= nil and b ~= nil
    and a.query == b.query
    and a.whole_start == b.whole_start
    and a.whole_end == b.whole_end
    and a.case_sensitive == b.case_sensitive
end

local function send_search_spec(spec)
  if same_search_spec(spec, last_search_sent) then return end
  last_search_sent = spec
  post_sync_json(config.search_url, json_encode(spec), "search")
end

local function post_search()
  if not search_sync_enabled() then return end
  local active = vim.o.hlsearch and vim.v.hlsearch == 1
  local spec = {
    query = "",
    whole_start = false,
    whole_end = false,
    case_sensitive = false,
  }
  if active then
    spec = search_pattern_spec(vim.fn.getreg("/"))
  end
  send_search_spec(spec)
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
  local spec = search_pattern_spec(vim.fn.getcmdline())
  if spec.query == "" then
    last_search_sent = nil
    post_search()
    return
  end
  send_search_spec(spec)
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

-- Absolute, symlink-resolved path. Besides daemon routing, inverse search uses
-- this to recognize that a canonical server path and the editor's symlink path
-- name the same open buffer, so a jump never reopens it under a second name.
local function canon(p)
  if not p or p == "" then return p end
  return vim.fn.resolve(vim.fn.fnamemodify(p, ":p"))
end

local function loaded_buffer_for_path(path)
  local target = canon(path)
  if not target or target == "" then return nil end
  local current = vim.api.nvim_get_current_buf()
  local current_matches = vim.api.nvim_buf_is_loaded(current)
    and canon(vim.api.nvim_buf_get_name(current)) == target
  if current_matches and vim.bo[current].modified then return current end
  local fallback = current_matches and current or nil
  for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
    if bufnr ~= current
        and vim.api.nvim_buf_is_loaded(bufnr)
        and canon(vim.api.nvim_buf_get_name(bufnr)) == target then
      -- If aliases have already been loaded under more than one name, keep
      -- the authoritative unsaved buffer rather than whichever was listed
      -- first. Otherwise reuse the first loaded match.
      if vim.bo[bufnr].modified then return bufnr end
      fallback = fallback or bufnr
    end
  end
  return fallback
end

local function jump_to_source(jump)
  if type(jump) ~= "table" or not jump.file or not jump.line then return end
  local seq = tonumber(jump.seq) or 0
  if seq <= last_jump_seq then return end
  last_jump_seq = seq
  local file = tostring(jump.file)
  local line = math.max(1, tonumber(jump.line) or 1)
  local col = math.max(0, (tonumber(jump.col) or 1) - 1)
  local function navigate()
    -- Cursor-setting APIs do not create a jumplist entry. Record the editing
    -- position explicitly before either switching buffers or moving in place,
    -- so one Ctrl-O returns to where the user was before the preview click.
    vim.cmd([[normal! m']])
    local current = vim.api.nvim_get_current_buf()
    local loaded = loaded_buffer_for_path(file)
    local must_switch = (loaded and loaded ~= current)
      or (not loaded and canon(vim.api.nvim_buf_get_name(current)) ~= canon(file))
    if must_switch then
      -- Opening the target fires BufEnter synchronously; keep the jumping
      -- daemon active across it (the target is usually an \input of its project).
      -- `m'` above is the one deliberate jump; suppress :edit's implicit entry
      -- so Ctrl-O does not stop at an intermediate position in the target.
      in_jump = true
      local switched
      if loaded then
        -- Prefer an already loaded symlink alias. It may contain unsaved edits
        -- that would diverge from a second buffer opened by canonical name.
        switched = pcall(vim.api.nvim_set_current_buf, loaded)
      else
        switched = pcall(vim.cmd, "keepjumps edit " .. vim.fn.fnameescape(file))
      end
      in_jump = false
      if not switched then return end
    end
    local line_count = vim.api.nvim_buf_line_count(0)
    line = math.min(line, math.max(1, line_count))
    local line_text = vim.api.nvim_buf_get_lines(0, line - 1, line, false)[1] or ""
    col = math.min(col, #line_text)
    vim.api.nvim_win_set_cursor(0, { line, col })
    vim.cmd("normal! zvzz")
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

  -- Browser jumps are Normal-mode navigation. :stopinsert takes effect after
  -- the current callback returns, so defer the move by one event-loop turn;
  -- otherwise nvim shifts the already-set target one byte left when Insert or
  -- Replace mode finally ends. Other modes can leave synchronously.
  local mode = vim.api.nvim_get_mode().mode
  if mode:sub(1, 1) == "i" or mode:sub(1, 1) == "R" then
    vim.cmd("stopinsert")
    vim.defer_fn(navigate, 0)
    return
  elseif mode ~= "n" then
    local normal = vim.api.nvim_replace_termcodes("<C-\\><C-N>", true, true, true)
    vim.api.nvim_feedkeys(normal, "nx", false)
  end
  navigate()
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

local function debounced_cursor()
  if not daemon_job or not config.sync then return end
  if cursor_timer then cursor_timer:stop(); cursor_timer:close() end
  local pending = uv.new_timer()
  cursor_timer = pending
  pending:start(config.cursor_debounce_ms, 0, vim.schedule_wrap(function()
    -- The uv expiry and scheduled Lua callback are separate turns. A newer
    -- cursor event may replace this timer between them; a stale callback must
    -- never close that replacement or post the superseded position.
    if cursor_timer ~= pending then return end
    cursor_timer = nil
    if not pending:is_closing() then pending:close() end
    post_cursor()
  end))
end

-- Separate timer from cursor_timer: a visual drag fires CursorMoved rapidly,
-- and sharing the timer would cancel pending cursor posts (and vice versa).
local function debounced_selection()
  if not daemon_job or not config.sync then return end
  if selection_timer then selection_timer:stop(); selection_timer:close() end
  local pending = uv.new_timer()
  selection_timer = pending
  pending:start(config.cursor_debounce_ms, 0, vim.schedule_wrap(function()
    if selection_timer ~= pending then return end
    selection_timer = nil
    if not pending:is_closing() then pending:close() end
    post_selection()
  end))
end

local function daemon_count()
  local n = 0
  for _ in pairs(daemons) do n = n + 1 end
  return n
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

local function debounced_push(buf)
  if not buf or not vim.api.nvim_buf_is_valid(buf) then return end
  local path = vim.api.nvim_buf_get_name(buf)
  -- Prefer the daemon that owns the changed buffer. The active daemon is the
  -- compatibility fallback for a project child whose watched-file set has not
  -- arrived from /debug yet; capture it now so a later buffer switch cannot
  -- reroute this snapshot.
  local entry = daemon_for_file(path) or active_entry()
  if not entry then return end
  local edit_cursor = buffer_cursor(buf)
  local previous = push_timers[buf]
  if previous then previous:stop(); previous:close() end
  local pending = uv.new_timer()
  push_timers[buf] = pending
  pending:start(config.debounce_ms, 0, vim.schedule_wrap(function()
    -- The uv timer callback and its scheduled Lua callback are separate turns.
    -- An edit can supersede/close this timer in between; then this stale
    -- callback must neither close the replacement nor push an old snapshot.
    if push_timers[buf] ~= pending then return end
    push_timers[buf] = nil
    if not pending:is_closing() then pending:close() end
    push_buffer(buf, edit_cursor, entry)
  end))
end

-- Tear down on nvim exit (VimLeavePre). With `close_on_exit = false` the
-- detached daemon outlives nvim and the browser tab stays usable.
local function stop_all()
  if not config.close_on_exit then return end
  for root, d in pairs(daemons) do
    stopping[root] = true
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
        last_text_change[args.buf] = uv.now()
        debounced_push(args.buf)
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
  -- browser tab) — deleting the buffer is an explicit "done with this file".
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

-- Stop ONE preview session and its daemon.
-- Shared by :MathPreviewStop (M.stop) and the :bd autocmd. Declared as a
-- forward local above attach_autocmds.
function stop_entry(entry, opts)
  opts = opts or {}
  if not entry or not daemons[entry.root] then return end
  stopping[entry.root] = true
  daemons[entry.root] = nil
  if daemon_root == entry.root then
    stop_jump_poll()
    daemon_job = nil
    daemon_port = nil
    daemon_root = nil
  end
  -- How the daemon dies decides what the VIEWER does. jobstop's SIGTERM makes
  -- the daemon broadcast a goodbye — browser tabs close themselves (the
  -- teardown routes: :MathPreviewStop, :bd, quitting nvim). A RESTART must
  -- keep the viewer alive instead: SIGUSR1 exits silently, the tab sits
  -- on its 1s reconnect loop, finds the new daemon on the rebound port, and
  -- hard-reloads in place (which is why the restart path skips re-opening).
  local killed
  if opts.restart then
    local ok, pid = pcall(vim.fn.jobpid, entry.job)
    if ok and type(pid) == "number" and pid > 0 and uv.kill then
      -- luv reports failure as a soft (nil, err) return, not an error — check
      -- the result, or a failed usr1 would skip the jobstop fallback and leave
      -- the daemon running.
      local ok2, res = pcall(uv.kill, pid, "sigusr1")
      killed = ok2 and res ~= nil
    end
  end
  if not killed then
    pcall(vim.fn.jobstop, entry.job)  -- its on_exit is now a no-op (already deregistered)
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
  local start_buf, start_path = opts.buf, opts.buf_path
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
  local spawn_opts = {
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
          -- Exit code 12 is the daemon's dedicated "port bind failed" code —
          -- a lost race: another process grabbed the probed port between our
          -- free-port check and the daemon's own bind. That's the ONLY exit
          -- worth retrying on the next port; anything else (unreadable root,
          -- bad config, missing feature) would fail identically on every
          -- port in the scan range.
          if code == 12 and retries_left > 0 and (uv.now() - started_at) < 2000 then
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
    }
  local spawned, job_or_err = pcall(vim.fn.jobstart, spawn_args, spawn_opts)
  if not spawned then
    vim.notify(
      "mathpreview: failed to spawn daemon (" .. tostring(job_or_err) .. ")",
      vim.log.levels.ERROR)
    return
  end
  local job = job_or_err
  if job <= 0 then
    vim.notify("mathpreview: failed to spawn daemon", vim.log.levels.ERROR)
    return
  end
  -- Register this file's daemon and make it the active one. attach_autocmds is
  -- global (push routing) — only needed once, when the first daemon starts.
  local first = daemon_count() == 0
  local entry =
    { job = job, port = port, root = root, cmd = cmd, opened = false,
      jump_seq = 0, buf = start_buf, buf_path = start_path }
  stopping[root] = nil
  daemons[root] = entry
  if first then attach_autocmds() end
  -- One sweep for abandoned daemons per session, now that ours is registered
  -- (and therefore excluded from the scan).
  maybe_sweep_stale_daemons()
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
  -- A never-saved OR modified start buffer can differ from the bytes the new
  -- daemon read from disk. This includes edits made while ensure_binary() was
  -- still resolving, before TextChanged autocmds existed. Keep the captured
  -- buffer/path instead of consulting the window that happens to be current
  -- when the delayed startup push fires. Two attempts cover a cold bind race;
  -- a repeat identical snapshot is a cheap server-side no-op.
  local function start_buffer_needs_push()
    if not start_buf or not vim.api.nvim_buf_is_valid(start_buf) then return false end
    if vim.api.nvim_buf_get_name(start_buf) ~= start_path then return false end
    return opts.initial_sync
      or vim.fn.filereadable(start_path) == 0
      or vim.bo[start_buf].modified
  end
  if start_buffer_needs_push() then
    local function push_start_buffer()
      if daemons[root] == entry and start_buffer_needs_push() then
        push_buffer(start_buf, buffer_cursor(start_buf), entry)
      end
    end
    vim.defer_fn(push_start_buffer, 400)
    vim.defer_fn(push_start_buffer, 1600)
  end
  -- Skip the open on a restart that rebound the same port: the existing
  -- viewer survives the restart (the daemon dies silently, no goodbye) and
  -- reconnects on its own — opening another would pile up duplicates.
  local reused_tab = opts.prev_port ~= nil and port == opts.prev_port
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
    ("mathpreview: serving %s on %s"):format(vim.fn.fnamemodify(root, ":~"), viewer_url(port, root)),
    vim.log.levels.INFO)
end

-- Resolve a usable binary, installing it with the selected method as needed,
-- then call on_ready(cmd). This gives "auto-install on first use" and
-- "auto-reinstall on plugin update" with no plugin-manager build hook:
--   * cargo (default) compiles this checkout with the Rust toolchain
--   * github downloads the exact version/target release + checksum
--   * a stale/damaged managed binary → repair it with that method
--   * current binary → proceed as-is
--   * a user `cmd` / unrelated $PATH binary → proceed, warn on skew
-- Every (re)install emits auto_install's "installing… please wait" notice.
local function ensure_binary(root, on_ready)
  local explicit_cmd = config.cmd ~= nil and config.cmd ~= ""
  local spec = install_spec()
  local cmd = resolve_cmd(spec)
  local can_build = is_source_checkout() and vim.fn.executable("cargo") == 1
  local target = prebuilt_target()
  local installer = plugin_root() .. "/scripts/install-prebuilt.sh"
  local can_download = target ~= nil
    and vim.fn.filereadable(installer) == 1
    and vim.fn.executable("sh") == 1
    and vim.fn.executable("curl") == 1
    and vim.fn.executable("tar") == 1
    and (vim.fn.executable("shasum") == 1 or vim.fn.executable("sha256sum") == 1)
  local can_install = can_build
  if spec.method == "github" then can_install = can_download end

  local function install_then_ready(fallback)
    auto_install(spec, function(ok, installed)
      if ok and vim.fn.executable(installed) == 1 then
        on_ready(installed)
      elseif fallback then
        -- Preserve the historical Cargo behavior: a failed rebuild may still
        -- run a prior, parseable executable. GitHub mode is always fail-closed
        -- because selecting it promises the exact verified release.
        on_ready(fallback)
      else
        -- on_ready won't run; release the in-flight start guard for this root.
        starting[root] = nil
        if ok then
          vim.notify(
            "mathpreview: install reported success but no binary found at "
              .. installed .. " — see :messages.",
            vim.log.levels.ERROR)
        end
      end
    end)
  end

  if not cmd then
    if can_install then
      install_then_ready(nil)
    else
      starting[root] = nil
      local reason
      if spec.method == "github" then
        reason = target
            and "the GitHub installer needs `sh`, `curl`, `tar`, and `shasum`/`sha256sum`"
          or "GitHub Releases do not provide a binary for this OS/architecture"
      else
        reason = "`cargo` is unavailable to compile it"
      end
      vim.notify(("mathpreview: no usable `mathpreview-cli`; %s. Switch "
          .. "`install_method`, install manually (README), or set "
          .. "`cmd = '/path/to/mathpreview-cli'`."):format(reason),
        vim.log.levels.ERROR)
    end
    return
  end

  -- A binary exists. Probe plugin-managed files before launching them. GitHub
  -- cache entries must report this exact plugin version; malformed, damaged,
  -- older, or newer entries are all replaced. Cargo retains its historical
  -- no-downgrade behavior but repairs an unreadable binary and upgrades an old
  -- one. Explicit `cmd` always remains user-owned, even when its string happens
  -- to equal a managed destination.
  -- A user `cmd` / unrelated $PATH binary isn't ours to touch — just run the
  -- async skew check (which warns) and use it as-is.
  if is_managed(cmd, spec, explicit_cmd) then
    run_system({ cmd, "--version" }, { timeout = 5000 }, function(res)
      local output = (res and res.code == 0 and res.stdout)
        and res.stdout:gsub("%s+$", "") or nil
      local ver
      if output then
        if spec.method == "github" then
          ver = output:match("^mathpreview%-cli (%S+)$")
        else
          ver = output:match("(%S+)%s*$")
        end
      end
      if ver then last_status.binary_version = ver end
      local needs_repair = ver == nil
        or (spec.method == "github"
          and output ~= "mathpreview-cli " .. PLUGIN_VERSION)
        or (spec.method == "cargo" and ver and semver_cmp(ver, PLUGIN_VERSION) < 0)
      if needs_repair and can_install then
        local reason = ver and ("reports " .. ver) or "failed its version check"
        vim.notify(
          ("mathpreview: managed binary %s (expected %s) — reinstalling…")
            :format(reason, PLUGIN_VERSION),
          vim.log.levels.INFO)
        local fallback = spec.method == "cargo" and ver and cmd or nil
        install_then_ready(fallback)
      elseif needs_repair then
        if spec.method == "cargo" and ver then
          -- No compiler is available, but an older parseable Cargo/PATH binary
          -- can retain the plugin's historical warn-and-run behavior.
          check_binary_version(cmd)
          on_ready(cmd)
        else
          starting[root] = nil
          local requirement = spec.method == "github"
              and "the GitHub installer prerequisites are unavailable"
            or "`cargo` is unavailable"
          vim.notify(
            ("mathpreview: managed %s binary is unusable and cannot be repaired "
              .. "because %s. See :messages, switch `install_method`, or set "
              .. "an explicit `cmd`."):format(spec.method, requirement),
            vim.log.levels.ERROR)
        end
      else
        if ver ~= PLUGIN_VERSION then check_binary_version(cmd) end
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
  if opts.viewer and opts.viewer ~= "" then
    if opts.viewer ~= "browser" and not viewer_migration_warned then
      viewer_migration_warned = true
      vim.notify(
        "mathpreview: the native window viewer was removed; opening the browser instead. "
          .. "Remove `viewer = " .. string.format("%q", tostring(opts.viewer))
          .. "` from your config.",
        vim.log.levels.WARN)
    end
  end
  local root, err = opts.root, nil
  if not root then root, err = current_root() end
  if not root then
    vim.notify("mathpreview: " .. err, vim.log.levels.ERROR)
    return
  end
  -- Pin the editor buffer as well as the root across async binary resolution.
  -- The root tells the daemon what to serve; the buffer is the authoritative
  -- unsaved snapshot that may need an initial /buffer push after it binds.
  local start_buf = opts.buf
  if not start_buf then
    local current = vim.api.nvim_get_current_buf()
    local current_path = vim.api.nvim_buf_get_name(current)
    if current_path ~= "" and canon(current_path) == canon(root) then
      start_buf = current
    else
      local candidate = vim.fn.bufnr(root)
      if candidate and candidate >= 0 then start_buf = candidate end
    end
  end
  if start_buf and vim.api.nvim_buf_is_valid(start_buf) then
    opts.buf = start_buf
    opts.buf_path = opts.buf_path or vim.api.nvim_buf_get_name(start_buf)
    opts.initial_sync = opts.initial_sync
      or vim.fn.filereadable(opts.buf_path) == 0
      or vim.bo[start_buf].modified
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
-- :MathRestart in an unsupported buffer still act on the last preview).
local function target_entry()
  return daemon_for_file(vim.api.nvim_buf_get_name(0)) or active_entry()
end

function M.stop()
  stop_entry(target_entry())
end

-- Scan the port range for abandoned preview daemons and offer to stop them
-- (the on-demand form of the once-per-session startup sweep).
function M.clean()
  sweep_stale_daemons({ report_empty = true })
end

function M.restart()
  local entry = target_entry()
  if not entry then M.start() return end
  -- Remember the port so the restart can rebind it and let the open tab
  -- reconnect, instead of opening a fresh one each time.
  local prev_port, root = entry.port, entry.root
  -- If restart was requested from a watched child, preserve that modified
  -- buffer; otherwise keep the root buffer captured by the original start.
  local current_buf = vim.api.nvim_get_current_buf()
  local current_path = vim.api.nvim_buf_get_name(current_buf)
  local restart_buf = daemon_for_file(current_path) == entry and current_buf or entry.buf
  local restart_path = restart_buf and vim.api.nvim_buf_is_valid(restart_buf)
      and vim.api.nvim_buf_get_name(restart_buf) or entry.buf_path
  local initial_sync = restart_buf and vim.api.nvim_buf_is_valid(restart_buf)
      and (vim.fn.filereadable(restart_path) == 0 or vim.bo[restart_buf].modified)
  -- restart=true kills the daemon SILENTLY (no goodbye), so the tab survives
  -- and reconnects to the rebound port — M.stop() would close it.
  stop_entry(entry, { restart = true })
  -- Give the OS a moment to release the port before re-binding.
  vim.defer_fn(function()
    M.start({
      prev_port = prev_port,
      root = root,
      buf = restart_buf,
      buf_path = restart_path,
      initial_sync = initial_sync,
    })
  end, 200)
end

function M.status()
  local age = uv.hrtime() / 1e6 - last_status.last_push_ms
  local buf = vim.api.nvim_get_current_buf()
  local spec = install_spec()
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
    install_method = config.install_method,
    install_dir = spec.bin_dir,
    release_target = prebuilt_target(),
    install_dir_on_path = managed_binary_on_path(spec),
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
--     install_method = "github", -- default: "cargo"
--     cmd = "/usr/local/bin/mathpreview-cli",
--     auto_open_browser = false,
--   })
function M.setup(opts)
  opts = opts or {}
  if opts.install_method ~= nil
      and opts.install_method ~= "cargo"
      and opts.install_method ~= "github" then
    error("mathpreview: `install_method` must be \"cargo\" or \"github\"", 2)
  end
  if opts.viewer ~= nil then
    if opts.viewer ~= "browser" and not viewer_migration_warned then
      viewer_migration_warned = true
      vim.notify(
        "mathpreview: `viewer` is obsolete because previews now use the browser. "
          .. "Ignoring viewer = " .. string.format("%q", tostring(opts.viewer)) .. ".",
        vim.log.levels.WARN)
    end
    opts = vim.tbl_extend("force", {}, opts)
    opts.viewer = nil
  end
  config = vim.tbl_extend("force", config, opts)
  check_viewer_host()
end

return M
