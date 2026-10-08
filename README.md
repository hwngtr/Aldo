# MuD

A lossless music search and tagging daemon. Search several peer networks at once,
deduplicate what they find, download it, and write Discogs metadata into the
FLAC files without re-encoding a single audio frame.

**Status: `mud search` finds real FLACs on SoulSeek and identifies them on
Discogs; `mud download <id>` fetches SoulSeek files into the library and can
write basic Discogs tags without re-encoding the audio.**
SoulSeek results are grouped by peer and folder, filtered to FLAC, sorted by advertised
download speed, and limited to the ten fastest. Each result shows candidate ID, size,
bit depth, sample rate, advertised speed and free slot status.
Discogs results give the release identity that tagging will match against.

## Design commitments

**FLAC only, always.** Lossless filters are applied before a result reaches the
user, not after. A lossy file is never offered.

**Rate limits are a first-class service.** A SoulSeek protocol violation costs a
30-minute ban, so budgets live in the database, survive restarts, and are visible
in the API. See `crates/mud-store/src/repo/budget.rs`.

**Strong types at every boundary.** Every identifier, byte count, duration and
audio quantity is a distinct type with a validating constructor, so a swapped
argument or a seconds-for-milliseconds mix-up is a compile error. SQLite has no
unsigned or narrow integer columns, so every such read goes through a checked
narrowing helper in `crates/mud-store/src/error.rs` rather than a cast.

**Lossless metadata edits.** FLAC Vorbis comments are written by replacing the
metadata block range only; audio frames are never decoded or recompressed.

**Every upstream gets one honest `User-Agent`.** Discogs blocks default library
agents outright; MusicBrainz requires a contact address.

## Security posture

- The API binds to `127.0.0.1` and the address is not configurable.
- Every route except `/health` requires a per-install bearer token, compared in
  constant time.
- The token file is written with mode `0600`.
- Requests carrying a foreign `Origin` are refused, so a page in a browser cannot
  drive the API on the user's behalf.
- All SQL uses bound parameters. `mud-store` owns every query in the codebase, so
  a forgotten bind is impossible.
- Remote file paths from torrents and peers are treated as hostile input and are
  validated against a root before use.

## Running it

`mud` is a single-user CLI. It runs on your machine, for you, and binds no port
unless you ask for the optional daemon.

On Arch Linux, install a local package from the included
[`packaging/PKGBUILD`](./packaging/PKGBUILD):

```sh
cd packaging
makepkg -si
```

After installation, run the application from any directory:

```sh
mud search "radiohead - ok computer"
mud download 1
```

The package installs the command at `/usr/bin/mud`. Re-run `makepkg -si` after
building a newer project version.

```sh
cargo build --release

export MUD_LIBRARY_ROOT=~/Music
# Credentials can also live in the project-root `.env` file.

./target/release/mud search "radiohead - ok computer - 1997"
./target/release/mud download 1
./target/release/mud budgets
./target/release/mud sessions
```

`mud download <index>` automatically uses the first Discogs release returned by
the latest search and writes album, artist, track, year and Discogs release ID
tags. Install the `metaflac` command from the FLAC tools package before
downloading. Use `--discogs-release <id>` only when you want to override the
automatic selection.

MuD loads credentials from `~/.config/mud/.env` when installed. When running
from the source tree, MuD also loads `.env` from the project directory. Set the
file permissions to `600`. Shell environment variables override both files.

Create the installed-app configuration with:

```sh
install -dm700 ~/.config/mud
install -m600 /dev/null ~/.config/mud/.env
$EDITOR ~/.config/mud/.env
```

SoulSeek has no registration step: the network creates the account the first
time you log in with a new name. MuD uses the public `muduser` / `123`
credentials by default, so no account setup is required. Users can override
the credentials with `MUD_SLSK_USERNAME` and `MUD_SLSK_PASSWORD`.
MuD shares `~/Music` by default so peers do not queue you last:

```sh
./target/release/mud search "miles davis - kind of blue"
```

Override the default with `MUD_SLSK_SHARED` in `~/.config/mud/.env`:

```env
MUD_SLSK_SHARED=/home/username/Music
```

A search prints both providers:

```
Query: radiohead / ok computer (1997)
Discogs: 1 fetched, 0 cached, 1 reported
  OK Computer - Radiohead (1997) | album | 12 tracks

SoulSeek: 95 folders, 2586 files (1334 rejected as not FLAC)
  #1  Radiohead/OK Computer         Shrouded0147   12 files  362.2 MiB  24-bit 96 kHz  free slot
  #2  Radiohead/OK Computer (1997)  JSSpade        12 files  356.5 MiB  16-bit 44 kHz  queued
```

Download a source by its result number:

```sh
./target/release/mud download 1
```

SoulSeek result numbers run from 1 to 10 and reset at UTC midnight. A new
search replaces the day's result-number mapping. Use `mud download <number>`
before UTC midnight to download a result from the latest search.

The optional daemon holds the Discogs token and answers on loopback, for a GUI
shell. It is off by default:

```sh
cargo build --release -p mudd --features api
./target/release/mud serve
```

The daemon prints its listen address and writes a token to
`$MUD_DATA_DIR/api-token`:

```sh
TOKEN=$(cat ~/.local/share/mud/api-token)
curl -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"query":"Radiohead - OK Computer - 1997"}' \
  http://127.0.0.1:8137/api/v1/search
```

A Discogs personal access token is required before any catalog lookup: Discogs
returns 401 for `/database/search` without one, and 25 requests per minute rather
than 60. Get one at <https://www.discogs.com/settings/developers>. Without it a
search still succeeds and reports the provider as `not_configured`, which is a
different answer from "found nothing".

The response names what every provider did, so an empty result is explainable:

```json
{
  "session_id": 1,
  "matches": [{ "title": "OK Computer", "artist": "Radiohead", "year": 1997, "track_count": 12 }],
  "providers": [
    { "provider": "discogs", "state": "done", "result_count": 1,
      "detail": "1 release bodies fetched, 0 served from cache, 1 hits reported" }
  ]
}
```

`state` is one of `done`, `budget_denied`, `not_configured` or `failed`. Only
`done` means the provider answered.

`MUD_PROXY` routes the metadata sources through a SOCKS5 or HTTP proxy. It does
not affect SoulSeek, which owns its own socket.

## Layout

Dependencies run one way only; nothing points back up.

| Crate | Responsibility |
|---|---|
| `mud-core` | Domain types, provider trait. No I/O, no runtime. |
| `mud-store` | SQLite. Owns every SQL statement. |
| `mud-net` | One HTTP client, proxy config, rate-limit headers. |
| `mud-catalog` | Discogs wire types, mapper, client, search fan-out. |
| `mud-soulseek` | The SoulSeek provider, on its own thread. |
| `mud-api` | Optional loopback HTTP daemon, behind the `api` feature. |
| `mudd` | Binary. |

## Development

```sh
cargo test --workspace --all-features   # 316 tests
cargo clippy --workspace --all-targets   # zero warnings, pedantic on
```

`unsafe_code` is forbidden at the workspace level. Clippy runs with `pedantic`.

## What comes next

1. Match the downloaded files to the Discogs release, then write tags with
   `lofty`.
2. RutTracker and Nyaa RSS, routed through the SOCKS5 proxy.
3. Magnet resolution via BEP-9, and qbittorrent-nox as the transfer engine.
4. Chromaprint fingerprinting and match scoring.

SoulSeek results are a shortlist of what peers hold right now, so a search that
returns nothing usually means nobody online has it, or the client cannot be
reached on its listen port.

See `docs/PLAN.md` for the phase plan and the schema rationale.
