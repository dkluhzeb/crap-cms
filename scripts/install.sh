#!/usr/bin/env bash
#
# crap-cms installer.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/dkluhzeb/crap-cms/main/scripts/install.sh | bash
#   curl -fsSL .../install.sh | bash -s -- v0.1.0-alpha.5   # pin version
#
# Environment:
#   XDG_DATA_HOME  Where versions live (default: $HOME/.local/share).
#   BIN_DIR        Where the shim symlink lives (default: $HOME/.local/bin).
#
# Layout this installer creates:
#   $XDG_DATA_HOME/crap-cms/versions/<version>/crap-cms   # the actual binary
#   $XDG_DATA_HOME/crap-cms/current -> versions/<version>/crap-cms
#   $BIN_DIR/crap-cms -> $XDG_DATA_HOME/crap-cms/current
#
# Self-update continues to work because the binary on PATH is a symlink the
# running `crap-cms update` command can flip atomically.

set -euo pipefail

REPO="dkluhzeb/crap-cms"
BINARY_NAME="crap-cms"
XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
STORE_DIR="$XDG_DATA_HOME/crap-cms"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { printf '%b>>>%b %s\n'  "$GREEN"  "$NC" "$*"; }
warn()  { printf '%b>>>%b %s\n'  "$YELLOW" "$NC" "$*"; }
error() { printf '%b>>>%b %s\n'  "$RED"    "$NC" "$*" >&2; }
die()   { error "$@"; exit 1; }

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="linux" ;;
        *)      die "Unsupported OS: $os (only Linux is supported by this installer)" ;;
    esac

    case "$arch" in
        x86_64|amd64)   arch="x86_64" ;;
        aarch64|arm64)  arch="aarch64" ;;
        *)              die "Unsupported architecture: $arch" ;;
    esac

    printf '%s-%s-%s' "$BINARY_NAME" "$os" "$arch"
}

# Fetch latest release tag from GitHub API. Uses /releases (not /releases/latest)
# so pre-release tags are included — important while we're still in alpha.
get_latest_version() {
    curl -fsSL "https://api.github.com/repos/${REPO}/releases" \
        | grep '"tag_name"' \
        | head -1 \
        | sed -E 's/.*"tag_name":\s*"([^"]+)".*/\1/'
}

main() {
    local version="${1:-}"
    local asset_name
    asset_name="$(detect_platform)"

    if [ -z "$version" ]; then
        info "Fetching latest release..."
        version="$(get_latest_version)"
        [ -n "$version" ] || die "Could not determine latest version"
    fi
    info "Version: $version"
    info "Asset:   $asset_name"

    local url="https://github.com/${REPO}/releases/download/${version}/${asset_name}"
    local sums_url="https://github.com/${REPO}/releases/download/${version}/SHA256SUMS"

    local tmp_dir tmp_bin tmp_sums
    tmp_dir="$(mktemp -d)"
    tmp_bin="$tmp_dir/$asset_name"
    tmp_sums="$tmp_dir/SHA256SUMS"
    # shellcheck disable=SC2064
    trap "rm -rf \"$tmp_dir\"" EXIT

    info "Downloading ${url}..."
    curl -fSL --progress-bar -o "$tmp_bin" "$url" \
        || die "Download failed. Check that version '$version' exists and has a '$asset_name' asset."

    if curl -fsSL -o "$tmp_sums" "$sums_url" 2>/dev/null; then
        info "Verifying SHA256 checksum..."
        local expected actual
        expected="$(grep "$asset_name" "$tmp_sums" | awk '{print $1}')"
        actual="$(sha256sum "$tmp_bin" | awk '{print $1}')"

        if [ -z "$expected" ]; then
            warn "No checksum found for $asset_name in SHA256SUMS, skipping verification"
        elif [ "$expected" != "$actual" ]; then
            die "Checksum mismatch!"$'\n'"  Expected: $expected"$'\n'"  Got:      $actual"
        else
            info "Checksum OK: $actual"
        fi
    else
        warn "No SHA256SUMS file in release, computing checksum for reference:"
        sha256sum "$tmp_bin" | awk '{print "    " $1}'
    fi

    # Move into the versioned store.
    local version_dir="$STORE_DIR/versions/$version"
    mkdir -p "$version_dir"
    mv "$tmp_bin" "$version_dir/$BINARY_NAME"
    chmod +x "$version_dir/$BINARY_NAME"

    # Flip the `current` symlink atomically (symlink-then-rename).
    local current_link="$STORE_DIR/current"
    local tmp_link="$STORE_DIR/.current.new"
    ln -sfn "$version_dir/$BINARY_NAME" "$tmp_link"
    mv -Tf "$tmp_link" "$current_link"

    # Ensure the PATH shim exists.
    mkdir -p "$BIN_DIR"
    ln -sfn "$current_link" "$BIN_DIR/$BINARY_NAME"

    info "Installed ${BINARY_NAME} ${version} to ${version_dir}/${BINARY_NAME}"
    info "Shim:     ${BIN_DIR}/${BINARY_NAME} -> ${current_link}"

    # PATH nudge — do not auto-edit rc files, but tell the user what to add.
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *)
            warn "${BIN_DIR} is not on your PATH."
            warn "Add this line to your shell init file (~/.bashrc, ~/.zshrc, etc.):"
            printf '\n    export PATH="%s:$PATH"\n\n' "$BIN_DIR"
            ;;
    esac

    # Smoke check (best-effort — don't fail install if --version misbehaves).
    "$version_dir/$BINARY_NAME" --version 2>/dev/null || true
}

main "$@"
