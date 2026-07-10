#!/bin/sh
# Install the Locus desktop entry + icon on Linux — the equivalent of the
# macOS `Locus.app` bundle. Wayland has no per-window icons: the compositor
# shows the icon of the desktop entry whose name matches the window's app_id
# (`io.github.sonv.locus`, pinned in view.rs). This also makes Locus appear
# in app launchers and pinnable to docks/taskbars.
#
#   scripts/install-locus-desktop.sh [path-to-locus-binary]
#
# The binary defaults to `locus` on $PATH, falling back to this checkout's
# target/release/locus. Installs per-user (no sudo): icons into
# ~/.local/share/icons/hicolor, the entry into ~/.local/share/applications.
set -eu

cd "$(dirname "$0")/.."

case "$(uname -s)" in
  Linux) ;;
  *) echo "install-locus-desktop.sh: Linux only (macOS uses make-locus-app.sh)" >&2; exit 1 ;;
esac

APP_ID=io.github.sonv.locus
BIN="${1:-}"
if [ -z "$BIN" ]; then
  BIN=$(command -v locus 2>/dev/null || true)
fi
if [ -z "$BIN" ] && [ -x target/release/locus ]; then
  BIN="$(pwd)/target/release/locus"
fi
[ -n "$BIN" ] && [ -x "$BIN" ] || {
  echo "locus binary not found — install it first:" >&2
  echo "  cargo install --path crates/cli --features gui --force" >&2
  exit 1
}

DATA="${XDG_DATA_HOME:-$HOME/.local/share}"

# Icon: the scalable SVG covers every size on modern desktops; add a 512px
# PNG when a rasterizer is available (some docks prefer bitmaps).
mkdir -p "$DATA/icons/hicolor/scalable/apps"
cp crates/cli/assets/locus-icon.svg "$DATA/icons/hicolor/scalable/apps/$APP_ID.svg"
if command -v rsvg-convert >/dev/null 2>&1; then
  mkdir -p "$DATA/icons/hicolor/512x512/apps"
  rsvg-convert -w 512 -h 512 crates/cli/assets/locus-icon.svg \
    -o "$DATA/icons/hicolor/512x512/apps/$APP_ID.png"
fi

mkdir -p "$DATA/applications"
cat > "$DATA/applications/$APP_ID.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Locus
Comment=Live LaTeX preview in a native window
Exec=$BIN %f
Icon=$APP_ID
Terminal=false
Categories=Office;Viewer;
MimeType=text/x-tex;
StartupWMClass=locus
DESKTOP

# Refresh caches; harmless if the tools are absent.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA/applications" || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -f -t "$DATA/icons/hicolor" 2>/dev/null || true

echo "installed $DATA/applications/$APP_ID.desktop (icon: $APP_ID)"
echo "log out/in (or restart the shell/compositor) if the icon doesn't appear immediately"
