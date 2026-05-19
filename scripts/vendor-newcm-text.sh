#!/usr/bin/env bash
# scripts/vendor-newcm-text.sh — refresh crates/cli/vendor/newcm-text/.
#
# Pulls the `web-computer-modern` package from npm and keeps only the
# Serif 10 woff2 files used for body prose plus the OFL license file.
# The same New Computer Modern family (10pt optical size, by Antonis
# Tsolomitis) is what MathJax 4's SVG glyphs are generated from, so the
# surrounding text reads as a continuation of the math rather than a
# clashing fallback.
#
# Run from the repo root: `bash scripts/vendor-newcm-text.sh`.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR_DIR="$REPO_ROOT/crates/cli/vendor/newcm-text"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "fetching web-computer-modern from npm into $TMP_DIR"
(cd "$TMP_DIR" && npm pack web-computer-modern >/dev/null)
TARBALL="$(ls "$TMP_DIR"/web-computer-modern-*.tgz | head -1)"
echo "extracting $TARBALL"
tar -xzf "$TARBALL" -C "$TMP_DIR"

PKG="$TMP_DIR/package"

rm -rf "$VENDOR_DIR"
mkdir -p "$VENDOR_DIR/woff2"

# Body text only needs the Serif 10pt optical size in four standard
# combinations. The package ships sans, mono, math, devanagari, uncial,
# and 08pt sizes too — none of which we use; dropping them keeps the
# vendored footprint to ~800 KB instead of ~9 MB.
for w in "Regular" "Italic" "Bold" "BoldItalic"; do
  cp "$PKG/woff2/WebCM Serif 10 $w.woff2" "$VENDOR_DIR/woff2/"
done

cp "$PKG/LICENSE.txt" "$VENDOR_DIR/LICENSE.txt"

echo "vendored $(du -sh "$VENDOR_DIR" | awk '{print $1}') under $VENDOR_DIR"
