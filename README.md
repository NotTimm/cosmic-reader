# Cosmic Comic

A native [COSMIC desktop](https://system76.com/cosmic) reader for comic book
archives (`.cbz`/`.cbr`) and manga/comic series folders.

## Features

- Library view with covers, search, and a "Continue Reading" shelf
- Chapter-aware series folders (nested `.cbz`/`.cbr`/image-dir chapters)
- Reading progress and metadata saved to a local SQLite database
- Single/dual page layout, theater mode, fullscreen
- Zoom & pan: `Ctrl`+scroll wheel, touchscreen pinch, or `M`/`Ctrl +`/`Ctrl -`
- Drag and drop a file or folder onto the window to open it
- Metadata debug panel (the `i` button) showing what the app parsed out of
  the filename and what it matched via AniList (manga/anime) and
  ComicVine (Western comics)

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
