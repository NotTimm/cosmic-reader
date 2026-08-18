# Cosmic Comic

A native [COSMIC desktop](https://system76.com/cosmic) reader for comic book
archives (`.cbz`/`.cbr`), manga/comic series folders, and `.epub` books.

## Features

- Library view with covers, search, and a "Continue Reading" shelf
- Chapter-aware series folders (nested `.cbz`/`.cbr`/image-dir chapters)
- Fast CBR opening: pages are extracted in one pass and cached on disk
  instead of re-streaming the archive per page (see src/comic.rs)
- Basic `.epub` support: chapter navigation, heading sizes, cover
- Reading progress and metadata saved to a local SQLite database
- Single/dual page layout, theater mode, fullscreen
- Zoom & pan: `Ctrl`+scroll wheel, real trackpad/touchscreen pinch (see the
  libcosmic fork note below), or `M`/`Ctrl 0`
- Drag and drop a file, folder, or book onto the window to open it
- Metadata debug panel (the `i` button): embedded `ComicInfo.xml` when
  present, plus what it matched via AniList (manga/anime) and ComicVine
  (Western comics, via a series→issue lookup with a cover image)

## Building

```
just build-release
```

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
