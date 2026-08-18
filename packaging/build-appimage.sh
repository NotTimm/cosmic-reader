#!/usr/bin/env bash
# Builds an AppImage — a single self-contained executable that runs on any
# reasonably recent glibc distro. Unlike deb/rpm this bundles the non-system
# shared libraries the binary needs, so it can't rely on distro packages.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

VERSION="$(version)"
ARCH="$(uname -m)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# appimagetool isn't packaged by most distros; fetch it on demand and cache
# it next to the dist output.
TOOL="$DIST_DIR/appimagetool-$ARCH.AppImage"
if [ ! -x "$TOOL" ]; then
    echo "==> Downloading appimagetool"
    mkdir -p "$DIST_DIR"
    curl -fL -o "$TOOL" \
        "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
    chmod +x "$TOOL"
fi

build_release

APPDIR="$WORK/AppDir"
install -Dm0755 "$REPO_DIR/target/release/$APP_NAME" "$APPDIR/usr/bin/$APP_NAME"
install -Dm0644 "$REPO_DIR/res/$APP_ID.desktop" "$APPDIR/usr/share/applications/$APP_ID.desktop"
install -Dm0644 "$REPO_DIR/res/$APP_ID.metainfo.xml" \
    "$APPDIR/usr/share/metainfo/$APP_ID.metainfo.xml"
install -Dm0644 "$REPO_DIR/res/icons/$APP_ID.svg" \
    "$APPDIR/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"

# AppImage expects the desktop file and icon at the AppDir root too.
cp "$APPDIR/usr/share/applications/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
cp "$APPDIR/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg" "$APPDIR/$APP_ID.svg"
ln -sf "$APP_ID.svg" "$APPDIR/.DirIcon"

# Bundle the shared libraries that aren't part of a base system, skipping
# the ones that must come from the host (glibc, the graphics stack).
mkdir -p "$APPDIR/usr/lib"
while read -r lib; do
    case "$(basename "$lib")" in
        libc.so.*|libm.so.*|libdl.so.*|libpthread.so.*|librt.so.*|ld-linux*) continue ;;
        libGL*|libEGL*|libGLX*|libdrm*|libX11*|libxcb*|libwayland*) continue ;;
    esac
    [ -f "$lib" ] && cp -L "$lib" "$APPDIR/usr/lib/" 2>/dev/null || true
done < <(ldd "$APPDIR/usr/bin/$APP_NAME" | awk '/=> \// {print $3}')

cat > "$APPDIR/AppRun" <<'EOF'
#!/usr/bin/env bash
HERE="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="$HERE/usr/lib:${LD_LIBRARY_PATH:-}"
export XDG_DATA_DIRS="$HERE/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
exec "$HERE/usr/bin/cosmic-comic" "$@"
EOF
chmod +x "$APPDIR/AppRun"

mkdir -p "$DIST_DIR"
OUT="$DIST_DIR/CosmicComic-$VERSION-$ARCH.AppImage"
ARCH="$ARCH" "$TOOL" --no-appstream "$APPDIR" "$OUT"
echo "==> $OUT"
