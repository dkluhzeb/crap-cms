#!/usr/bin/env bash
#
# Vendor htmx into the admin UI's static directory.
#
# htmx ships as a single self-contained minified file, so no bundler is
# required — we just download the upstream artifact, verify its SHA-384,
# and prepend a banner with provenance. Re-run when upgrading htmx.
#
#   bash scripts/bundle-htmx.sh
#
# Produces: static/htmx.js
# Requires: curl, openssl
#
# Verifying the hash here (rather than via SRI in the HTML) means the
# vendored bytes are pinned in git: any tampering changes the file
# blob in PR diffs, no external SRI lookup needed at page load.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT="$PROJECT_DIR/static/htmx.js"

# Bump VERSION when upgrading. EXPECTED_SHA384 must be the upstream
# hash for that release — get it from
#   https://www.npmjs.com/package/htmx.org or compute via:
#     curl -fsSL "https://unpkg.com/htmx.org@${VERSION}/dist/htmx.min.js" \
#       | openssl dgst -sha384 -binary | openssl base64 -A
# Compare to a published hash (npm provenance, GitHub release) before
# committing a change.
VERSION="2.0.9"
EXPECTED_SHA384="ESlCao+z/oasnu2Uc/5K1LQTI7YCF2KKO4xakCPQCFuiHhCh8Oa/R5NwHY6guZ3m"

URL="https://unpkg.com/htmx.org@${VERSION}/dist/htmx.min.js"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "==> Downloading htmx ${VERSION} from ${URL} ..."
curl -fsSL "$URL" -o "$TMPDIR/htmx.min.js"

ACTUAL_SHA384=$(openssl dgst -sha384 -binary "$TMPDIR/htmx.min.js" | openssl base64 -A)
echo "==> SHA-384: ${ACTUAL_SHA384}"

if [[ "$ACTUAL_SHA384" != "$EXPECTED_SHA384" ]]; then
  echo ""
  echo "FATAL: hash mismatch — the upstream artifact does not match the"
  echo "expected hash recorded in this script."
  echo ""
  echo "  expected: ${EXPECTED_SHA384}"
  echo "  actual:   ${ACTUAL_SHA384}"
  echo ""
  echo "If this is an intentional upgrade, update EXPECTED_SHA384 in"
  echo "scripts/bundle-htmx.sh after independently verifying the new"
  echo "release (npm provenance, GitHub release notes)."
  exit 1
fi

echo "==> Hash verified. Writing ${OUTPUT} ..."

# Prepend a one-line banner so future readers can trace provenance
# from the file alone. The banner is a normal `/* ... */` comment so
# it doesn't break IIFE evaluation.
{
  echo "/*! htmx v${VERSION} — vendored from ${URL}"
  echo " *  sha384: ${EXPECTED_SHA384}"
  echo " *  produced by scripts/bundle-htmx.sh */"
  cat "$TMPDIR/htmx.min.js"
} > "$OUTPUT"

SIZE=$(wc -c < "$OUTPUT")
echo "==> Done! static/htmx.js (${SIZE} bytes)"
