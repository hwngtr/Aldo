# Aldo

Album Downloader (Aldo) searches for lossless albums, downloads FLAC files, and
tags them with Discogs metadata.

## Install on Arch

From the project directory:

```sh
cd packaging
makepkg -si
```

Install `metaflac` too. Aldo uses it to write tags without re-encoding the audio:

```sh
sudo pacman -S flac
```

After that, `aldo` works from any directory.

## First-time setup

Aldo uses these defaults:

- Music folder: `~/Music`
- Server username: `aldouser`
- Server password: `123`

The server account is created automatically when the credentials are accepted.
You do not need to register an account first.

Discogs metadata needs a personal access token. Create one at
<https://www.discogs.com/settings/developers>, then set it for your shell:

```sh
export ALDO_DISCOGS_TOKEN="paste-your-token-here"
```

To keep the token between shell sessions, add the export to `~/.bashrc` or
`~/.zshrc`. You can also use `~/.config/aldo/.env`.

## Search

Use the format:

```text
Artist - Album
Artist - Album - Year
```

Use the structured form for a less ambiguous automatic Discogs match. A bare
phrase such as `besame mucho` is still valid, but it can return unrelated
albums, so Aldo may reject a download whose folder does not match the selected
release.

Example:

```sh
aldo search "The Beach Boys - Surf's Up"
```

The output contains two useful parts:

```text
Discogs: 5 fetched, 0 cached, 50 reported
  Discogs #26707685  Surf’s Up - The Beach Boys (1971) | album | 10 tracks

Server: 184 folders, 2375 files
  #1   Beach Boys/2011 - The SMiLE Sessions  ...
  #2   Beach Boys/1971 - Surf's Up          ...
  #6   The Beach Boys/Surf's Up (1971)      ... 10 files ...
```

The `#6` value is the download index. It is not a Discogs ID.

The Discogs ID is the number after `Discogs #`. For an `Artist - Album` query,
Aldo normally selects the first Discogs match automatically, so you usually do
not need to copy that number. Server folders are ranked by how closely their
artist and album names match the query before speed is used as a tie-breaker.

## Download

Download the server result by its index:

```sh
aldo download 6
```

Use the index from the latest search. Indexes reset at UTC midnight, and a new
search replaces the previous daily index list.

Aldo downloads the files into:

```text
~/Music/<artist>/<album>/
```

For example:

```text
~/Music/The Beach Boys/Surf's Up (1971)/
```

When a Discogs match is available, Aldo writes album and track metadata
automatically:

- title
- album
- artist
- album artist
- track number
- track total
- disc number
- year
- Discogs release ID

Aldo matches track numbers from filenames to Discogs tracks. A file containing
`- 01 -` receives the Discogs title for track 1.

Aldo refuses to tag a folder when its file count does not match the selected
Discogs release. Search again with `Artist - Album` and choose a complete album
folder instead of downloading a one-file or unrelated result.

## Choosing a different Discogs release

Search results can contain several pressings. For example:

```text
Discogs #26707685  Surf’s Up - The Beach Boys (1971)
Discogs #21741601  Surf’s Up - The Beach Boys (1971)
Discogs #6060342   Surf's Up - The Beach Boys (1971)
```

Aldo uses the first match by default. To choose another release explicitly:

```sh
aldo download 6 --discogs-release 6060342
```

## Useful options

Use another shared folder for one search:

```sh
aldo --slsk-share "$HOME/Music" search "Radiohead - OK Computer"
```

Use another library folder:

```sh
aldo --library-root "$HOME/Downloads/music" search "Radiohead - OK Computer"
```

Show the current rate budgets:

```sh
aldo budgets
```

Show recent searches:

```sh
aldo sessions
```

## If a search returns nothing

Check the query format first:

```sh
aldo search "The Beach Boys - Surf's Up"
```

`Surf's Up The Beach Boys` is treated as one phrase because it has no ` - `
separator.

Server results only come from peers online at search time. A result can
disappear later. Sharing `~/Music` is enabled by default, and forwarding port
`2234` usually improves peer availability.

If Discogs reports `not configured`, set:

```sh
export ALDO_DISCOGS_TOKEN="paste-your-token-here"
```

If the server reports `not configured`, override the defaults with:

```sh
export ALDO_SLSK_USERNAME="aldouser"
export ALDO_SLSK_PASSWORD="123"
```

## Updating

After changing the source:

```sh
cargo build --release -p aldo
cd packaging
makepkg -si
```
