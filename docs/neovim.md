# Neovim setup

[Back to the README](../README.md)

Run `:MathPreview` in a TeX or Markdown buffer to start the browser preview.
The defaults work without a `setup()` call. See [Installation](installation.md)
for lazy.nvim, packer.nvim, vim-plug, and manual installation instructions.

## Commands

The plugin registers six commands:

| Command | Action |
| --- | --- |
| `:MathPreview` | Starts the current document's daemon on a free port in `23636..23651`, opens the browser, and attaches buffer and source-sync handlers. Reopens or refocuses the tab if already running. |
| `:MathPreviewStop` | Stops the daemon and its sync handlers. By default, quitting Neovim also stops the preview. |
| `:MathPreviewRestart` | Stops and starts the daemon after a 200 ms grace period. Use after a binary update or configuration change. |
| `:MathPreviewClean` | Scans preview ports for abandoned daemons and offers to stop them. |
| `:MathPreviewStatus` | Prints PID/port, root file, push and cursor counts, errors, installation method, resolved binary path, and versions. |
| `:MathPreviewDebug` | Prints the resolved viewer settings, source-jump command, and consulted configuration/macro paths. A `*` marks an existing file. |

`:MathPreviewClean` looks for daemons with no editor attached and no viewer tab
connected. The first `:MathPreview` of a session performs the same check unless
`stale_check = false`. It asks before stopping anything. Older daemons that
cannot report editor state are listed as “state unknown” when no tab is open.

`MathPreviewDebug` reads the daemon's `/debug` endpoint. Open
`http://127.0.0.1:<port>/debug` for the full JSON, including its log.

## Buffer updates and source sync

The daemon selects its frontend from the document extension. A `.md` or
`.markdown` buffer is its own root. For TeX, it finds the project root and can
apply unsaved changes from an `\input` child as an in-memory override. Editing
a child file therefore updates the root preview without writing to disk. A new
named buffer can preview before its first save. Neovim 0.10+ uses `vim.system`
and `vim.uv`, with `jobstart` and `vim.loop` fallbacks on older versions.

Cursor movement scrolls to and highlights the nearest rendered word, math, or
reference. Movement caused by recent typing follows without flashing the
highlight. The viewer does not scroll while the target is between 25% and 75%
of the viewport. Outside that band, it places the target at the 25% line.
Blank paragraph separators inside theorem and proof environments also have
source anchors, so an empty source line can sync to its corresponding gap.

In the other direction, Cmd/Ctrl-click rendered content to jump to its source
location by default. Alternative gestures, including double-click, are
configurable in `[viewer.source-jump]`. The plugin keeps a long-poll request
on `/jump`, so an idle preview does not repeatedly spawn short polling requests. Source
jumps record the prior editor position in the window's jump list. Press
Ctrl-O to return. A jump leaves Insert, Replace, Visual, or Terminal mode,
reveals folds, and centers the destination before running `on_jump`.

Visual selections highlight the corresponding rendered region. Linewise `V`
covers full source rows, while blockwise Ctrl-V uses the selection's bounding
rectangle. Leaving Visual mode clears the highlight. These updates use the
same `sync` and `cursor_debounce_ms` settings as cursor sync.

With `sync_search = true`, the preview mirrors Neovim's active `/` or `?`
search, including `\<...\>` word boundaries and the `ignorecase`, `smartcase`,
`\c`, and `\C` rules. Search mirroring requires `sync` and `hlsearch`, with
`incsearch` for live updates while typing the query. `:nohlsearch` clears the
preview highlights too.

## Setup options

Override only the options you need. This example records the defaults and
commonly used optional values:

```lua
require("mathpreview").setup({
  -- "cargo" compiles locally. "github" downloads the matching verified release.
  install_method = "cargo",
  -- An explicit path overrides installation and is never overwritten.
  -- cmd = "/usr/local/bin/mathpreview-cli",
  -- Optional Cargo prefix. Use the same --root in any Cargo build hook.
  -- install_root = "~/.local",

  filetypes = { "tex", "plaintex", "latex", "markdown" },
  auto_open_browser = true,
  viewer_host = "mathpreview.localhost",
  debounce_ms = 40,
  cursor_debounce_ms = 80,
  sync = true,
  sync_search = true,
  close_on_exit = true,
  stale_check = true,

  -- One parked browser-to-editor request, with a backoff on empty/error replies.
  jump_wait_ms = 25000,
  jump_retry_ms = 1000,

  -- nil uses the embedded MathJax bundle and works offline.
  -- mathjax_url = "https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js",
  -- Optional external editor command. Empty string disables process spawning.
  -- editor = "code -g {file}:{line}:{col}",

  raise_on_jump = true,
  jump_window = "nvim",
  -- Extra hook after the source jump and built-in window focus.
  -- on_jump = function(jump) ... end, -- jump = { file, line, col }
})
```

`install_root` affects Cargo only. The default is Cargo's own prefix, usually
`~/.cargo`. GitHub binaries always live in versioned directories under
`stdpath("data")/mathpreview`. Both are run by absolute path.

`debounce_ms` sets the delay before a buffer push. `cursor_debounce_ms`
throttles forward source sync and visual-selection updates. Set `sync = false`
to disable bidirectional cursor sync. Set `auto_open_browser = false` to start
the daemon without opening a viewer.

`viewer_host` controls the browser-facing hostname. A shared
`mathpreview.localhost` origin lets browser extensions and per-site settings
apply across previews. Use `"{stem}.localhost"` for a separate hostname per
document, such as `my-paper-v2.localhost` for `My Paper_v2.tex`. `localhost`,
`127.0.0.1`, and `*.localhost` names are accepted. The plugin's internal
requests still use `127.0.0.1`.

`close_on_exit = true` stops the daemon when Neovim quits and asks its tab to
close. A browser may refuse to close a tab with navigation history, leaving a
“preview ended” message instead. Set it to `false` to keep reading after
quitting Neovim. `:MathPreviewStop` still stops the preview explicitly.

With `editor = nil` and sync enabled, source jumps navigate in the existing
Neovim instance instead of spawning an editor process. When sync is disabled,
the plugin supplies a command targeting that Neovim server. An explicit
command overrides this behavior and can use `{file}`, `{line}`, and `{col}`.
Set `editor = ""` to disable process spawning.

## Source-jump focus

`raise_on_jump = true` brings the editor forward after a browser source jump,
where the platform permits it. Cursor navigation itself works independently
of window focus.

On Linux and BSD, the plugin walks from Neovim's PID through its shell and
terminal ancestors to find the owning window. This lets two terminal windows
running separate previews focus the appropriate editor. `jump_window` is a
class/app-ID fallback when the PID search finds no window.

| Environment | Focus method | Requirement |
| --- | --- | --- |
| macOS | `osascript` activates the detected terminal or GUI app | Built in, with `$LC_TERMINAL` as a tmux fallback |
| Hyprland | `hyprctl dispatch focuswindow pid:<ancestor>` | `hyprctl` |
| Sway / wlroots | `swaymsg [pid=<ancestor>] focus` | `swaymsg` |
| KDE/KWin Wayland | `kdotool search --pid <ancestor>` and window activation | `kdotool` |
| X11 | Activates `$WINDOWID`, or searches by ancestor PID | `xdotool` |
| GNOME/Mutter | Use an `on_jump` hook with a shell-extension bridge | No general activation API |

On macOS, supported terminals include Terminal, iTerm, WezTerm, Ghostty, kitty,
and Alacritty, with Neovide and nvim-qt among the GUI options. The first focus
attempt may request an Automation permission. macOS activates the application,
not a specific terminal tab.

For terminal Neovim on Linux, set `jump_window` to a fallback such as `kitty`,
`foot`, `Alacritty`, or `org.wezfurlong.wezterm`. Keep `"nvim"` for a GUI when
appropriate. To find the active window's class or app ID, use
`kdotool getactivewindow getwindowclassname`, `hyprctl activewindow`, or
`swaymsg -t get_tree` with the terminal focused.

Set `raise_on_jump = false` to keep focus in the browser. For other window
managers, `on_jump = function(jump) ... end` runs after cursor movement and the
built-in focus attempt. Its argument contains `file`, `line`, and `col`. Hook
errors are caught and logged.

## Troubleshooting

If `:MathPreviewStatus` shows `daemon_running = false`, inspect `:messages` for
the spawn error. A missing compiler or unavailable release may require changing
`install_method`, or setting `cmd` to a usable binary's absolute path.

If `last_error` is set, it reports the plugin's request failure, such as a
crashed daemon or mismatched port. Try `:MathPreviewRestart` and inspect
`:MathPreviewDebug` to confirm which settings and files were loaded.

After updating, compare `plugin_version` and `binary_version` in
`:MathPreviewStatus`. Restart the preview so an already running daemon picks
up the new executable. See [Updating the binary](installation.md#update-the-binary)
for manager hooks and managed downloads.
