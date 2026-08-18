#!/usr/bin/env bash
# Installs (or updates) Cosmic Comic for the current user, no sudo required.
#
# Run it fresh to install, or re-run it any time to update: it pulls the
# latest changes (if this is a git checkout with nothing uncommitted),
# rebuilds, and reinstalls over the previous copy.
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_dir"

prefix="${COSMIC_COMIC_PREFIX:-$HOME/.local}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo is not installed (see https://rustup.rs)" >&2
    exit 1
fi
if ! command -v just >/dev/null 2>&1; then
    echo "error: 'just' is not installed (needed to run the install recipe)" >&2
    exit 1
fi

if [ -d .git ] && command -v git >/dev/null 2>&1; then
    if git diff --quiet && git diff --cached --quiet; then
        echo "==> Pulling latest changes"
        git pull --ff-only
    else
        echo "==> Skipping git pull (uncommitted local changes present)"
    fi
fi

echo "==> Building release binary"
just build-release

echo "==> Installing to $prefix"
just prefix="$prefix" install

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f "$prefix/share/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "==> Done. Installed to $prefix/bin/cosmic-comic"

case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *)
        echo
        echo "Note: $prefix/bin is not on your PATH."
        echo "Add this to your shell config to run 'cosmic-comic' directly:"
        echo "  export PATH=\"$prefix/bin:\$PATH\""
        ;;
esac
