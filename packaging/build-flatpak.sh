#!/usr/bin/env bash
# Builds a single-file .flatpak bundle from the manifest in this directory.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

need flatpak "sudo dnf install flatpak  # or: sudo apt install flatpak"
need flatpak-builder "sudo dnf install flatpak-builder"

VERSION="$(version)"
MANIFEST="$(dirname "${BASH_SOURCE[0]}")/$APP_ID.yml"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "==> Ensuring runtimes are installed"
flatpak install --user --noninteractive --or-update flathub \
    org.freedesktop.Platform//24.08 \
    org.freedesktop.Sdk//24.08 \
    org.freedesktop.Sdk.Extension.rust-stable//24.08

echo "==> Building"
flatpak-builder --force-clean --user \
    --repo="$WORK/repo" \
    "$WORK/build" "$MANIFEST"

mkdir -p "$DIST_DIR"
OUT="$DIST_DIR/CosmicComic-$VERSION.flatpak"
flatpak build-bundle "$WORK/repo" "$OUT" "$APP_ID"
echo "==> $OUT"
echo "    install with: flatpak install --user $OUT"
