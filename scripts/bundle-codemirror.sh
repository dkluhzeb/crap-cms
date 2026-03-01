#!/usr/bin/env bash
#
# Bundle CodeMirror 6 into a single IIFE file for the admin UI.
#
# Run once, commit the output. Re-run only when upgrading CodeMirror.
#
#   bash scripts/bundle-codemirror.sh
#
# Produces: static/codemirror.js
# Requires: node, npm (used only in a temp directory — no node_modules in repo)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT="$PROJECT_DIR/static/codemirror.js"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "==> Setting up temp build directory..."

cat > "$TMPDIR/package.json" <<'PKGJSON'
{
  "private": true,
  "dependencies": {
    "@codemirror/view": "^6.35.0",
    "@codemirror/state": "^6.5.0",
    "@codemirror/commands": "^6.7.0",
    "@codemirror/language": "^6.10.0",
    "@codemirror/search": "^6.5.0",
    "@codemirror/autocomplete": "^6.18.0",
    "@codemirror/lang-javascript": "^6.2.0",
    "@codemirror/lang-json": "^6.0.0",
    "@codemirror/lang-html": "^6.4.0",
    "@codemirror/lang-css": "^6.3.0",
    "@codemirror/lang-python": "^6.1.0",
    "@lezer/highlight": "^1.2.0",
    "esbuild": "^0.24.2"
  }
}
PKGJSON

echo "==> Installing CodeMirror packages..."
(cd "$TMPDIR" && npm install --no-audit --no-fund --loglevel=error)

echo "==> Copying entry point..."
cp "$SCRIPT_DIR/codemirror-entry.js" "$TMPDIR/entry.js"

echo "==> Bundling with esbuild..."
(cd "$TMPDIR" && npx esbuild entry.js \
  --bundle \
  --format=iife \
  --minify \
  --outfile=codemirror.js \
  --target=es2020)

cp "$TMPDIR/codemirror.js" "$OUTPUT"

SIZE=$(wc -c < "$OUTPUT")
echo "==> Done! static/codemirror.js ($SIZE bytes)"
