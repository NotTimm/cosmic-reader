#!/usr/bin/env bash
# Builds every package format that this machine has tooling for, skipping
# (with a note) the ones it can't. Output lands in ./dist.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

HERE="$(dirname "${BASH_SOURCE[0]}")"
VERSION="$(version)"
mkdir -p "$DIST_DIR"

echo "=== Building cosmic-comic $VERSION packages ==="
built=()
skipped=()

try() {
    local label="$1" script="$2" probe="$3"
    if ! command -v "$probe" >/dev/null 2>&1; then
        skipped+=("$label (missing '$probe')")
        return
    fi
    echo
    echo "=== $label ==="
    if bash "$HERE/$script"; then
        built+=("$label")
    else
        skipped+=("$label (build failed)")
    fi
}

try "deb"      build-deb.sh      dpkg-deb
try "rpm"      build-rpm.sh      rpmbuild
try "AppImage" build-appimage.sh curl
try "Flatpak"  build-flatpak.sh  flatpak-builder

echo
echo "=== Summary ==="
for b in "${built[@]:-}";   do [ -n "$b" ] && echo "  built:   $b"; done
for s in "${skipped[@]:-}"; do [ -n "$s" ] && echo "  skipped: $s"; done
echo
ls -lh "$DIST_DIR" 2>/dev/null | grep -vE '^total|appimagetool' || true
