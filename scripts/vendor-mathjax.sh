#!/usr/bin/env bash
# scripts/vendor-mathjax.sh — refresh crates/cli/vendor/mathjax/.
#
# Pulls the latest MathJax 3 from npm, extracts the es5/ bundle, and trims
# alternates we don't ship (a11y, SRE, CHTML/MML output, AsciiMath/MML input,
# and the alternate top-level bundles such as tex-chtml*.js). The result is
# what `mathpreview-cli serve` ships from `/vendor/mathjax/*`.
#
# Run from the repo root: `bash scripts/vendor-mathjax.sh`.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR_DIR="$REPO_ROOT/crates/cli/vendor/mathjax"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

VERSION="${1:-3}"

echo "fetching mathjax@$VERSION from npm into $TMP_DIR"
(cd "$TMP_DIR" && npm pack "mathjax@$VERSION" >/dev/null)
TARBALL="$(ls "$TMP_DIR"/mathjax-*.tgz | head -1)"
echo "extracting $TARBALL"
tar -xzf "$TARBALL" -C "$TMP_DIR"

echo "trimming bundles we do not use"
PKG="$TMP_DIR/package"
rm -rf \
  "$PKG/es5/a11y" \
  "$PKG/es5/sre" \
  "$PKG/es5/output/chtml" \
  "$PKG/es5/output/mml" \
  "$PKG/es5/input/asciimath" \
  "$PKG/es5/input/mml"
rm -f \
  "$PKG/es5/mml-chtml.js" \
  "$PKG/es5/mml-svg.js" \
  "$PKG/es5/node-main.js" \
  "$PKG/es5/tex-chtml-full-speech.js" \
  "$PKG/es5/tex-chtml-full.js" \
  "$PKG/es5/tex-chtml.js" \
  "$PKG/es5/tex-mml-chtml.js" \
  "$PKG/es5/tex-mml-svg.js" \
  "$PKG/es5/tex-svg-full.js" \
  "$PKG/es5/ui/menu.js"

rm -rf "$VENDOR_DIR"
mkdir -p "$VENDOR_DIR"
mv "$PKG"/* "$VENDOR_DIR/"

echo "vendored $(du -sh "$VENDOR_DIR" | awk '{print $1}') under $VENDOR_DIR"
