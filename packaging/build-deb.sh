#!/usr/bin/env bash
# Builds a .deb. Runtime dependencies are discovered from the binary's
# actual ELF links via dpkg-shlibdeps, so the package depends on real
# system packages (libarchive13, libxkbcommon0, libssl3, ...) rather than
# a hand-maintained list that drifts.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

need dpkg-deb "sudo apt install dpkg-dev  # or: sudo dnf install dpkg"

# Debian package names for the libraries the binary links against. Used
# when dpkg-shlibdeps isn't available (i.e. when cross-building a .deb
# from a non-Debian host, where the local shlibs database can't map
# sonames to packages anyway).
FALLBACK_DEPS="libc6, libgcc-s1, libarchive13, libxkbcommon0, libssl3, \
zlib1g, libzstd1, liblzma5, libbz2-1.0, liblz4-1, libacl1, libxml2"

VERSION="$(version_native)"
if command -v dpkg >/dev/null 2>&1; then
    ARCH="$(dpkg --print-architecture)"
else
    case "$(uname -m)" in
        x86_64)  ARCH=amd64 ;;
        aarch64) ARCH=arm64 ;;
        *)       ARCH="$(uname -m)" ;;
    esac
fi
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

build_release
stage_tree "$STAGE"

mkdir -p "$STAGE/DEBIAN" "$STAGE/usr/share/doc/$APP_NAME"
install -Dm0644 "$REPO_DIR/README.md" "$STAGE/usr/share/doc/$APP_NAME/README.md"

if command -v dpkg-shlibdeps >/dev/null 2>&1; then
    # dpkg-shlibdeps insists on running from a package root with a
    # debian/ directory present.
    mkdir -p "$STAGE/debian"
    echo 9 > "$STAGE/debian/compat"
    : > "$STAGE/debian/control"
    DEPS="$(cd "$STAGE" && dpkg-shlibdeps -O --ignore-missing-info "usr/bin/$APP_NAME" 2>/dev/null \
        | sed -E 's/^shlibs:Depends=//')"
    rm -rf "$STAGE/debian"
fi
if [ -z "${DEPS:-}" ]; then
    echo "==> dpkg-shlibdeps unavailable; using curated dependency list"
    DEPS="$FALLBACK_DEPS"
fi

cat > "$STAGE/DEBIAN/control" <<EOF
Package: $APP_NAME
Version: $VERSION
Section: graphics
Priority: optional
Architecture: $ARCH
Depends: $DEPS
Maintainer: Tim Singel <gamer.tdogtim@gmail.com>
Homepage: https://github.com/NotTimm/cosmic-reader
Description: Comic book and ebook reader for the COSMIC desktop
 A native COSMIC desktop reader for comic book archives (.cbz/.cbr),
 manga and comic series folders, and .epub books. Includes a scanned
 library with cover art, reading progress, and online metadata lookup.
EOF

mkdir -p "$DIST_DIR"
OUT="$DIST_DIR/${APP_NAME}_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$STAGE" "$OUT"
echo "==> $OUT"
