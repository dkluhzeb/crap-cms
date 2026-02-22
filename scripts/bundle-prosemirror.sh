#!/usr/bin/env bash
#
# Bundle ProseMirror into a single IIFE file for the admin UI.
#
# Run once, commit the output. Re-run only when upgrading ProseMirror.
#
#   bash scripts/bundle-prosemirror.sh
#
# Produces: static/prosemirror.js
# Requires: node, npm (used only in a temp directory — no node_modules in repo)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT="$PROJECT_DIR/static/prosemirror.js"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "==> Setting up temp build directory..."

cat > "$TMPDIR/package.json" <<'PKGJSON'
{
  "private": true,
  "dependencies": {
    "prosemirror-model": "^1.24.1",
    "prosemirror-state": "^1.4.3",
    "prosemirror-view": "^1.37.1",
    "prosemirror-schema-basic": "^1.2.4",
    "prosemirror-schema-list": "^1.5.1",
    "prosemirror-history": "^1.4.1",
    "prosemirror-keymap": "^1.2.2",
    "prosemirror-commands": "^1.7.0",
    "prosemirror-dropcursor": "^1.8.2",
    "prosemirror-gapcursor": "^1.3.2",
    "prosemirror-inputrules": "^1.4.0",
    "esbuild": "^0.24.2"
  }
}
PKGJSON

echo "==> Installing ProseMirror packages..."
(cd "$TMPDIR" && npm install --no-audit --no-fund --loglevel=error)

echo "==> Copying entry point..."
cp "$SCRIPT_DIR/prosemirror-entry.js" "$TMPDIR/entry.js"

echo "==> Bundling with esbuild..."
(cd "$TMPDIR" && npx esbuild entry.js \
  --bundle \
  --format=iife \
  --minify \
  --outfile=prosemirror.js \
  --target=es2020)

cp "$TMPDIR/prosemirror.js" "$OUTPUT"

SIZE=$(wc -c < "$OUTPUT")
echo "==> Done! static/prosemirror.js ($SIZE bytes)"
