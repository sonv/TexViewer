# Installation

[Back to the README](../README.md)

MathPreview has two parts: the `mathpreview-cli` binary and its Neovim plugin.
The plugin starts the binary and sends your buffer to it as you type. Install
the plugin with your usual manager, choose a binary installation method, then
run `:MathPreview` in a TeX or Markdown buffer.

## Choose a binary installation method

The plugin can install a missing or stale managed binary in two ways:

| Method | What it does | Requires Rust? |
| --- | --- | --- |
| `install_method = "cargo"` (default) | Compiles the plugin checkout with `cargo install --path crates/cli --force` | Yes |
| `install_method = "github"` | Downloads the matching GitHub release, verifies SHA-256 and its reported version, and stores it under Neovim's data directory | No |

Both binaries include the Markdown parser, MathJax, New Computer Modern fonts,
viewer JavaScript, and CSS. The HTML/SVG preview needs no separate MathJax,
Markdown renderer, Node/npm, or TeX installation. Optional native TikZ rendering
and native PDF compilation for TeX documents need a local TeX toolchain.

Installation runs automatically on the first `:MathPreview` if a usable binary
is missing. Managed binaries are checked again after plugin updates. An
optional plugin-manager build hook installs the binary during install/update
instead, so the first preview does not have to wait.

### GitHub binary

In the plugin checkout, run:

```sh
sh scripts/install-prebuilt.sh
```

Select that managed binary in Neovim. Running the script alone does not change
the default installation method:

```lua
require("mathpreview").setup({ install_method = "github" })
```

The installer needs `sh`, `curl`, `tar`, and either `shasum` (macOS) or
`sha256sum` (Linux). Releases support macOS arm64/x86_64 and GNU/Linux
arm64/x86_64 with glibc 2.34 or newer. For Alpine/musl or stock NixOS without a
GNU compatibility loader, use Cargo. The installer checks the checksum,
archive layout, and reported version before replacing an existing binary.

For a manual installation, download the matching archive and `.sha256` from
[GitHub Releases](https://github.com/sonv/TexViewer/releases), verify it with
`shasum -a 256 -c <archive>.sha256` or `sha256sum -c <archive>.sha256`, and
extract it. Point the plugin at the extracted binary:

```lua
require("mathpreview").setup({
  cmd = "/absolute/path/to/mathpreview-cli",
})
```

An explicit `cmd` is user-managed and is never overwritten by the plugin. If
macOS Gatekeeper blocks a browser-downloaded binary you have verified, remove
that file's quarantine attribute with
`xattr -d com.apple.quarantine /absolute/path/to/mathpreview-cli`.

### Compile with Cargo

Install from the repository:

```sh
cargo install --git https://github.com/sonv/TexViewer mathpreview-cli
```

Or compile and install from an existing checkout:

```sh
cargo install --path crates/cli --force
```

`cargo install` puts the executable in `$CARGO_HOME/bin`, usually
`~/.cargo/bin`. Ensure that directory is on your shell's `$PATH` if you want
to use the CLI directly. `cargo build` alone leaves the executable under
`target/` and does not install it on `$PATH`. To use another installation
prefix, add `--root <prefix>`. The executable goes in `<prefix>/bin`, matching
the plugin's `install_root` option.

### Which binary runs?

An explicit `cmd` takes priority. Otherwise the plugin uses the binary managed
by `install_method`. Cargo mode also checks `$PATH` and a local
`target/release/` build. GitHub mode uses its managed release exclusively, so
an older Cargo or PATH installation cannot bypass the selected download.
`:MathPreviewStatus` shows the method, target, resolved path, and versions.

## Install the Neovim plugin

Any plugin manager that puts this repository's `lua/` and `plugin/` directories
on `runtimepath` works. `setup()` is optional when using the defaults. See
[Neovim setup](neovim.md) for the full options and commands.

### lazy.nvim

Compile locally with Rust:

```lua
{
  "sonv/TexViewer",
  ft = { "tex", "plaintex", "latex", "markdown" },
  build = "cargo install --path crates/cli --force",
}
```

Or use a verified GitHub binary without Rust:

```lua
{
  "sonv/TexViewer",
  ft = { "tex", "plaintex", "latex", "markdown" },
  build = "sh scripts/install-prebuilt.sh",
  opts = { install_method = "github" },
  config = function(_, opts)
    require("mathpreview").setup(opts)
  end,
}
```

Both `build` hooks are optional. Omit the hook to install on the first
`:MathPreview`. Do not combine `install_method = "github"` with the Cargo
hook. Lazy runs the hook before `setup()`, so that combination still requires
Rust. The script and first-use downloader use the same versioned location
under `stdpath("data")/mathpreview`.

To load the plugin by command as well as filetype, add:

```lua
cmd = {
  "MathPreview", "MathPreviewStop", "MathPreviewRestart",
  "MathPreviewClean", "MathPreviewStatus", "MathPreviewDebug",
},
```

Add overrides to `opts` and keep the explicit `config` function shown above.
The options are documented in [Neovim setup](neovim.md#setup-options).

### packer.nvim

```lua
use {
  "sonv/TexViewer",
  ft = { "tex", "plaintex", "latex", "markdown" },
  run = "cargo install --path crates/cli --force",
  config = function()
    require("mathpreview").setup({})
  end,
}
```

For a GitHub binary, replace `run` with
`run = "sh scripts/install-prebuilt.sh"` and set
`install_method = "github"` inside `setup()`. Omit the hook to install on first
use instead.

### vim-plug

In your `init.vim` plugin block:

```vim
Plug 'sonv/TexViewer', { 'do': 'cargo install --path crates/cli --force' }
```

For a GitHub binary, use `{ 'do': 'sh scripts/install-prebuilt.sh' }` and select
the method in `init.lua`, or a `lua << EOF` block in `init.vim`:

```lua
require("mathpreview").setup({ install_method = "github" })
```

Plain `Plug 'sonv/TexViewer'` also works. It installs the binary on first use,
with Cargo as the default unless overridden in `setup()`.

### Manual install

Neovim's [native package mechanism](https://neovim.io/doc/user/repeat.html#packages)
loads packages under `~/.config/nvim/pack/*/start/*` at startup:

```sh
mkdir -p ~/.config/nvim/pack/sonv/start
git clone https://github.com/sonv/TexViewer ~/.config/nvim/pack/sonv/start/mathpreview
cd ~/.config/nvim/pack/sonv/start/mathpreview
```

Optionally prepare the binary before first use with one of:

```sh
# Compile locally.
cargo install --path crates/cli --force

# Or download the GitHub binary. Also select install_method = "github" in setup().
sh scripts/install-prebuilt.sh
```

The commands become available on the next Neovim launch without an
`init.lua` edit. To pin a release, run `git checkout vX.Y.Z` in the checkout,
using a published tag from the Releases page.

For a checkout tracking `main`, update with `git pull`, then rerun the
installation command or let `:MathPreview` update its managed binary. Removing
the plugin checkout leaves downloaded binaries in
`stdpath("data")/mathpreview`. `:MathPreviewStatus` reports the exact
`install_dir`. Remove old version directories only when no pinned Neovim
configuration needs them. Cleanup is not automatic because separate profiles
may use different plugin versions.

## Update the binary

The plugin and binary versions move together. The selected installation method
handles both first installation and managed updates.

With a build hook, the plugin manager prepares the binary during updates:

```vim
:Lazy update TexViewer
:Lazy build TexViewer
```

The first command updates the checkout and runs the hook. The second runs the
hook without fetching a new commit. The corresponding update commands are
`:PackerSync` and `:PlugUpdate sonv/TexViewer`.

Without a hook, the next `:MathPreview` installs a missing or stale managed
binary. GitHub mode reports unpublished versions, unsupported platforms, or
incompatible binaries without falling back to Cargo or replacing a working
executable.

If Cargo mode sets `install_root`, give the build hook the same
`--root <prefix>`, or omit the hook and let the first-use installer read your
Lua settings.

Run `:MathPreviewStatus` to check that `plugin_version` and `binary_version`
match, then `:MathPreviewRestart` to replace a running daemon. A false
`install_dir_on_path` is expected for GitHub mode's private versioned directory.
The plugin runs that binary by absolute path.

## Open a preview

Open a named `.tex`, `.md`, or `.markdown` buffer and run `:MathPreview`. The
plugin starts a background server on a free port in `23636..23651`, opens
`http://mathpreview.localhost:<port>/`, and pushes edits without requiring a
save. A new named file can preview before its first write.

Use `:MathPreviewStop` to stop it and `:MathPreviewRestart` to restart it.
[Neovim setup](neovim.md#commands) explains all six commands.
[Viewer controls](viewer.md) covers reading, search, references, and source
jumps in the browser.

## Use the CLI directly

The CLI works without Neovim:

```sh
# Static HTML uses the MathJax CDN by default.
mathpreview-cli render path/to/paper.tex -o out.html
mathpreview-cli render path/to/notes.md -o notes.html

# Live previews update when files are saved to disk.
mathpreview-cli serve path/to/paper.tex
mathpreview-cli serve path/to/notes.markdown
```

Open the generated HTML file in a browser, or visit the live server's reported
URL. The server defaults to port `23636`. Without editor buffer pushes, updates
happen on save rather than on each edit.

Integrations can use `mathpreview-cli convert` for persistent newline-delimited
JSON conversion, or call the Rust `DocumentConverter` trait. See
[Converter API v1](converter-api.md) for the schema and
[Converter and viewer architecture](architecture.md) for ownership and
extension limits. The stock CLI and server currently select the bundled LaTeX
and Markdown converters. They do not load external converters from TOML or
executable commands.

## MathJax and offline setup

The live server works offline by default. The binary embeds MathJax 4
(`tex-svg.js`, TeX extensions, and New Computer Modern SVG font shards), about
14 MB in total, plus the four NCM body-text WOFF2 files. It serves them from
`/vendor/mathjax/` and uses:

```sh
--mathjax-url /vendor/mathjax/tex-svg.js
```

No separate download is needed, even after moving the executable. The New
Computer Modern body font shares its family with the math glyphs and is
[OFL-1.1 licensed](../crates/cli/vendor/newcm-text/LICENSE.txt).

To use a network-hosted MathJax 4 build instead:

```sh
mathpreview-cli serve path/to/paper.tex \
  --mathjax-url https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js
```

Or set the URL in Neovim:

```lua
require("mathpreview").setup({
  mathjax_url = "https://cdn.jsdelivr.net/npm/mathjax@4/tex-svg.js",
})
```

Static `mathpreview-cli render` output uses the jsDelivr CDN by default because
a directly opened HTML file has no daemon serving `/vendor/mathjax/`. To use
static HTML without Internet access, point `--mathjax-url` at a MathJax bundle
served by an HTTP server you control.
