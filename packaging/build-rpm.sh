#!/usr/bin/env bash
# Builds an .rpm. rpmbuild's automatic dependency generator reads the
# binary's ELF links and emits Requires on the providing system packages
# (libarchive.so.13, libxkbcommon.so.0, libssl.so.3, ...), so no manual
# dependency list is needed.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

need rpmbuild "sudo dnf install rpm-build"

VERSION="$(version_native)"
ARCH="$(uname -m)"
TOP="$(mktemp -d)"
trap 'rm -rf "$TOP"' EXIT

build_release

BUILDROOT="$TOP/buildroot"
stage_tree "$BUILDROOT"

mkdir -p "$TOP/SPECS"
cat > "$TOP/SPECS/$APP_NAME.spec" <<EOF
Name:           $APP_NAME
Version:        $VERSION
Release:        1%{?dist}
Summary:        Comic book and ebook reader for the COSMIC desktop
License:        GPL-3.0-only
URL:            https://github.com/NotTimm/cosmic-reader
BuildArch:      $ARCH

# Binaries are built outside rpmbuild and staged into the buildroot, so
# turn off the steps that expect sources here.
%global debug_package %{nil}
%define _build_id_links none

%description
A native COSMIC desktop reader for comic book archives (.cbz/.cbr),
manga and comic series folders, and .epub books. Includes a scanned
library with cover art, reading progress, and online metadata lookup.

%files
/usr/bin/$APP_NAME
/usr/share/applications/$APP_ID.desktop
/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg
/usr/share/metainfo/$APP_ID.metainfo.xml

%changelog
* $(LC_ALL=C date '+%a %b %d %Y') Tim Singel <gamer.tdogtim@gmail.com> - $VERSION-1
- Initial beta release.
EOF

rpmbuild \
    --define "_topdir $TOP" \
    --buildroot "$BUILDROOT" \
    -bb "$TOP/SPECS/$APP_NAME.spec"

mkdir -p "$DIST_DIR"
find "$TOP/RPMS" -name '*.rpm' -exec cp {} "$DIST_DIR/" \;
echo "==> $(find "$DIST_DIR" -name "${APP_NAME}-${VERSION}*.rpm" | head -1)"
