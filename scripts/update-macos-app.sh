#!/bin/sh
# Update every locally installed macOS entry point from this checkout:
#   - mathpreview-cli and the bare `locus` fallback in Cargo's bin directory
#   - /Applications/Locus.app, assembled from that exact installed binary
#
# Run after updating the checkout:
#   scripts/update-macos-app.sh
set -eu

cd "$(dirname "$0")/.."

case "$(uname -s)" in
  Darwin) ;;
  *) echo "update-macos-app.sh: macOS only" >&2; exit 1 ;;
esac

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "could not read workspace version from Cargo.toml" >&2; exit 1; }

if [ -n "${CARGO_INSTALL_ROOT:-}" ]; then
  INSTALL_ROOT=$CARGO_INSTALL_ROOT
elif [ -n "${CARGO_HOME:-}" ]; then
  INSTALL_ROOT=$CARGO_HOME
elif [ -n "${HOME:-}" ]; then
  INSTALL_ROOT=$HOME/.cargo
else
  echo "cannot resolve Cargo install root (HOME is unset)" >&2
  exit 1
fi
BIN_DIR=$INSTALL_ROOT/bin

echo "updating mathpreview $VERSION from $(pwd)…"
cargo install --locked --root "$INSTALL_ROOT" --path crates/cli --features gui --force

CLI_VERSION=$("$BIN_DIR/mathpreview-cli" --version | awk 'NR == 1 { print $2 }')
LOCUS_VERSION=$("$BIN_DIR/locus" --version | awk 'NR == 1 { print $2 }')
[ "$CLI_VERSION" = "$VERSION" ] || {
  echo "installed mathpreview-cli is $CLI_VERSION; expected $VERSION" >&2
  exit 1
}
[ "$LOCUS_VERSION" = "$VERSION" ] || {
  echo "installed locus is $LOCUS_VERSION; expected $VERSION" >&2
  exit 1
}

LOCUS_BINARY="$BIN_DIR/locus" scripts/make-locus-app.sh --install

APP=/Applications/Locus.app
APP_VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")
[ "$APP_VERSION" = "$VERSION" ] || {
  echo "installed Locus.app is $APP_VERSION; expected $VERSION" >&2
  exit 1
}
codesign --verify --deep --strict "$APP"

echo "updated mathpreview-cli, locus, and Locus.app to $VERSION"
echo "restart a running Neovim preview with :MathPreviewRestart"
