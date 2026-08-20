# Cosmic Comic

A native [COSMIC desktop](https://system76.com/cosmic) reader for comic book
archives (`.cbz`/`.cbr`), manga/comic series folders, and `.epub` books.

## Features

- Library view with covers, search, and a "Continue Reading" shelf
- Add library folders that get scanned and grouped automatically, either by
  browsing or by typing/pasting a path (`~` expands to home)
- Settings drawer: backdrop style & opacity, layout, metadata lookup,
  library folder list
- Chapter-aware series folders (nested `.cbz`/`.cbr`/image-dir chapters)
- Fast CBR opening: pages are extracted in one pass and cached on disk
  instead of re-streaming the archive per page (see src/comic.rs)
- Basic `.epub` support: chapter navigation, heading sizes, cover
- Reading progress and metadata saved to a local SQLite database
- Single/dual page layout, theater mode, fullscreen
- Zoom & pan: mouse wheel zooms directly (no modifier needed), trackpad/
  touchscreen pinch zooms (see the libcosmic fork note below), `Ctrl`+
  trackpad scroll also zooms, plain trackpad two-finger scroll pans a
  zoomed-in image (free diagonal movement), click-drag pans too, and
  `M`/`Ctrl 0` toggle/reset zoom, "Fit Page" resizes the window's width to
  match the current page's aspect ratio (height unchanged)
- Drag and drop a file, folder, or book onto the window to open it
- Metadata debug panel (the `i` button): embedded `ComicInfo.xml` when
  present, plus what it matched via AniList (manga/anime) and ComicVine
  (Western comics, via a series→issue lookup with a cover image)

## Building

```
just build-release
```

## Packaging a release

```
just package          # every format this machine has tooling for
just package-deb      # or one at a time
just package-rpm
just package-appimage
just package-flatpak
```

Output lands in `./dist`. The version comes from `Cargo.toml` — bump it
there and the packages follow.

The `.deb` and `.rpm` deliberately **do not** bundle libraries: their
dependencies are generated from the binary's actual ELF links
(`dpkg-shlibdeps` and rpm's automatic requires), so they pull
`libarchive`, `libxkbcommon`, `openssl` etc. from the distro. The
AppImage bundles those same libraries since it can't rely on packages,
and the Flatpak builds against the freedesktop 24.08 runtime.

Tooling needed per format (each script tells you if something's missing):

| Format   | Needs |
|----------|-------|
| deb      | `dpkg-dev` |
| rpm      | `rpm-build` |
| AppImage | `curl` (fetches `appimagetool` on first run) |
| Flatpak  | `flatpak`, `flatpak-builder` |

## Where your data lives

Following the XDG spec, so it survives reinstalls and upgrades:

| Path | Contents |
|------|----------|
| `~/.local/share/cosmic-comic/library.db` | Library, reading progress, metadata |
| `~/.config/cosmic-comic/settings.json` | Preferences |
| `~/.cache/cosmic-comic/` | Cover thumbnails, extracted archives — safe to delete |

The database is versioned with SQLite's `user_version` pragma and migrated
in place on startup, so upgrading never requires wiping your library.

## Installing / updating

```
./install.sh
```

Installs to `~/.local` (binary, `.desktop` entry, icon) with no sudo
needed. Re-run the same script any time to update: it pulls the latest
commit (if this is a clean git checkout), rebuilds, and reinstalls.

Override the install prefix with `COSMIC_COMIC_PREFIX=/usr/local ./install.sh`,
or use `just prefix=/usr install` / `just uninstall` directly.

## ComicVine metadata (optional)

AniList only knows about manga/anime, so Western comics (Marvel, DC, etc.)
won't match there. To also try [ComicVine](https://comicvine.gamespot.com/api/)
for those, get a free API key and either:

- set `COMICVINE_API_KEY` in your environment, or
- write the key (nothing else) to `~/.local/share/cosmic-comic/comicvine_api_key.txt`

Without a key, the debug panel just shows that no ComicVine key is set.

## Trackpad/touchscreen pinch-to-zoom fork

Upstream `iced` (vendored inside `pop-os/libcosmic`) silently drops winit's
`PinchGesture` event, so real trackpad/touchscreen pinch was impossible.
`Cargo.toml` points at `github.com/NotTimm/libcosmic`, a small personal fork
with the ~50 lines needed to forward it and gate mouse-wheel zoom behind
`Ctrl`. See the comment above the `libcosmic` dependency for how to re-sync
that fork against a newer upstream commit.
