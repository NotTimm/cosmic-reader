#!/usr/bin/env bash
# Shared helpers for the packaging scripts. Source, don't execute.
set -euo pipefail

APP_ID="com.tsingel.CosmicComic"
APP_NAME="cosmic-comic"

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$REPO_DIR/dist"

# Version comes from Cargo.toml so there's exactly one source of truth.
version() {
    grep -m1 '^version' "$REPO_DIR/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/'
}

# Debian and RPM disallow '-' in version fields (it separates the release),
# so 0.1.0-beta.1 becomes 0.1.0~beta.1 for deb and 0.1.0~beta.1 for rpm.
# The tilde sorts *before* the plain version, which is what you want for a
# pre-release.
version_native() {
    version | tr '-' '~'
}

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: '$1' is required but not installed." >&2
        [ $# -gt 1 ] && echo "  install with: $2" >&2
        exit 1
    }
}

build_release() {
    echo "==> Building release binary"
    (cd "$REPO_DIR" && cargo build --release)
}

# Lays out a standard FHS install tree under $1, which both the .deb and
# .rpm builds package verbatim.
stage_tree() {
    local root="$1"
    install -Dm0755 "$REPO_DIR/target/release/$APP_NAME" "$root/usr/bin/$APP_NAME"
    install -Dm0644 "$REPO_DIR/res/$APP_ID.desktop" \
        "$root/usr/share/applications/$APP_ID.desktop"
    install -Dm0644 "$REPO_DIR/res/icons/$APP_ID.svg" \
        "$root/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
    install -Dm0644 "$REPO_DIR/res/$APP_ID.metainfo.xml" \
        "$root/usr/share/metainfo/$APP_ID.metainfo.xml"
}
