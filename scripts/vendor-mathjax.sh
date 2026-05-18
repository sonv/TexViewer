#!/usr/bin/env bash
# scripts/vendor-mathjax.sh — refresh crates/cli/vendor/mathjax/.
#
# Pulls MathJax from npm and trims alternates we don't ship (a11y,
# SRE, CHTML/MML output, AsciiMath/MML input, plus the alternate
# top-level bundles like tex-chtml*.js). Defaults to MathJax 4; pass
# a different version as the first argument (e.g. `bash
# scripts/vendor-mathjax.sh 3` to roll back).
#
# MathJax 4 ships bundles at the package root; the old `es5/`
# subdirectory from v3 is gone, so the trim list is shorter and the
# serve-time vendor root in serve.rs is `vendor/mathjax/` rather
# than `vendor/mathjax/es5/`.
#
# Run from the repo root: `bash scripts/vendor-mathjax.sh`.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR_DIR="$REPO_ROOT/crates/cli/vendor/mathjax"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

VERSION="${1:-4}"

echo "fetching mathjax@$VERSION from npm into $TMP_DIR"
(cd "$TMP_DIR" && npm pack "mathjax@$VERSION" >/dev/null)
TARBALL="$(ls "$TMP_DIR"/mathjax-*.tgz | head -1)"
echo "extracting $TARBALL"
tar -xzf "$TARBALL" -C "$TMP_DIR"

PKG="$TMP_DIR/package"

# Detect package layout: MathJax 3 puts bundles under `es5/`,
# MathJax 4 puts them at the package root.
if [ -d "$PKG/es5" ]; then
  ROOT="$PKG/es5"
  echo "detected MathJax 3 layout (es5/)"
else
  ROOT="$PKG"
  echo "detected MathJax 4 layout (root)"
fi

echo "trimming bundles we do not use"
rm -rf \
  "$ROOT/a11y" \
  "$ROOT/sre" \
  "$ROOT/output/chtml" \
  "$ROOT/output/mml" \
  "$ROOT/input/asciimath" \
  "$ROOT/input/mml"
rm -f \
  "$ROOT/mml-chtml.js" \
  "$ROOT/mml-chtml-nofont.js" \
  "$ROOT/mml-svg.js" \
  "$ROOT/mml-svg-nofont.js" \
  "$ROOT/node-main.js" \
  "$ROOT/node-main.mjs" \
  "$ROOT/node-main.cjs" \
  "$ROOT/node-main-setup.cjs" \
  "$ROOT/require.mjs" \
  "$ROOT/tex-chtml-full-speech.js" \
  "$ROOT/tex-chtml-full.js" \
  "$ROOT/tex-chtml.js" \
  "$ROOT/tex-chtml-nofont.js" \
  "$ROOT/tex-mml-chtml.js" \
  "$ROOT/tex-mml-chtml-nofont.js" \
  "$ROOT/tex-mml-svg.js" \
  "$ROOT/tex-mml-svg-nofont.js" \
  "$ROOT/tex-svg-full.js" \
  "$ROOT/tex-svg-nofont.js" \
  "$ROOT/ui/menu.js"

rm -rf "$VENDOR_DIR"
mkdir -p "$VENDOR_DIR"
mv "$PKG"/* "$VENDOR_DIR/"

echo "vendored $(du -sh "$VENDOR_DIR" | awk '{print $1}') under $VENDOR_DIR"
