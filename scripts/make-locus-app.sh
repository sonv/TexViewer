#!/bin/sh
# Assemble Locus.app — the macOS bundle for the native viewer. A bundle (vs the
# bare `locus` binary) gives the icon to the dock BEFORE launch, makes Locus
# pinnable, and shows it in Spotlight/Launchpad. Double-clicking it opens the
# native "choose a .tex file" panel (Finder passes no argv).
#
#   scripts/make-locus-app.sh            build into target/bundle/Locus.app
#   scripts/make-locus-app.sh --install  ... and copy to /Applications
#   LOCUS_BINARY=/path/to/locus scripts/make-locus-app.sh --install
#                                         reuse an already-built binary
#
# The binary is built with `--features gui`; the bundle is ad-hoc signed so
# Gatekeeper treats a locally built app consistently on Apple Silicon.
set -eu

cd "$(dirname "$0")/.."

case "$(uname -s)" in
  Darwin) ;;
  *) echo "make-locus-app.sh: macOS only (bundles are a Darwin concept)" >&2; exit 1 ;;
esac

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "could not read workspace version from Cargo.toml" >&2; exit 1; }

if [ -n "${LOCUS_BINARY:-}" ]; then
  [ -x "$LOCUS_BINARY" ] || {
    echo "LOCUS_BINARY is not executable: $LOCUS_BINARY" >&2
    exit 1
  }
  LOCUS_SOURCE=$LOCUS_BINARY
  LOCUS_SOURCE_VERSION=$("$LOCUS_SOURCE" --version | awk 'NR == 1 { print $2 }')
  [ "$LOCUS_SOURCE_VERSION" = "$VERSION" ] || {
    echo "LOCUS_BINARY is version $LOCUS_SOURCE_VERSION; expected $VERSION" >&2
    exit 1
  }
  echo "assembling Locus.app $VERSION from ${LOCUS_SOURCE}…"
else
  echo "building locus $VERSION (--features gui, release)…"
  cargo build --release --features gui -p mathpreview-cli
  LOCUS_SOURCE=target/release/locus
fi

APP=target/bundle/Locus.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$LOCUS_SOURCE" "$APP/Contents/MacOS/locus"
cp crates/cli/assets/Locus.icns "$APP/Contents/Resources/Locus.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>            <string>Locus</string>
  <key>CFBundleDisplayName</key>     <string>Locus</string>
  <key>CFBundleIdentifier</key>      <string>io.github.sonv.locus</string>
  <key>CFBundleExecutable</key>      <string>locus</string>
  <key>CFBundleIconFile</key>        <string>Locus</string>
  <key>CFBundlePackageType</key>     <string>APPL</string>
  <key>CFBundleShortVersionString</key> <string>$VERSION</string>
  <key>CFBundleVersion</key>         <string>$VERSION</string>
  <key>LSMinimumSystemVersion</key>  <string>11.0</string>
  <key>NSHighResolutionCapable</key> <true/>
</dict>
</plist>
PLIST

codesign --force --sign - "$APP" 2>/dev/null || echo "warning: ad-hoc codesign failed (continuing)" >&2
echo "assembled $APP"

if [ "${1:-}" = "--install" ]; then
  DEST=/Applications/Locus.app
  rm -rf "$DEST"
  cp -R "$APP" "$DEST"
  echo "installed $DEST"
fi
