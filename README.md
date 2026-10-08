# MuD

Search for lossless music, download FLAC files, and tag them with Discogs metadata.

## Install on Arch

```sh
cd packaging
makepkg -si
```

You also need the FLAC tools for tagging:

```sh
sudo pacman -S flac
```

## Use

```sh
mud search "The Beach Boys - Surf's Up"
mud download 6
```

Search results are numbered. Use the number you want with `mud download`.
Discogs metadata is applied automatically when a match is available.

MuD uses `~/Music` as the default shared directory and `muduser` / `123` as
the default server credentials. Override them with environment variables:

```sh
export MUD_LIBRARY_ROOT="$HOME/Music"
export MUD_SLSK_USERNAME=muduser
export MUD_SLSK_PASSWORD=123
export MUD_DISCOGS_TOKEN=your_discogs_token
```

The Discogs token is optional, but required for metadata matching.
