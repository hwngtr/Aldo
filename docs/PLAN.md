# MuD implementation plan

Phase plan, schema rationale, and the decisions that are not obvious from the
code. See `README.md` for how to build and run.

## Constraint that shapes everything

SoulSeek has no search index. The server is a rendezvous and mailbox: a
`FileSearch` is relayed into the distributed D-network and results come only from
peers online at that instant. A file on an offline machine is invisible forever.

Consequence: **the first half of the goal is a latency problem, the second half
is not a latency problem at all.** Search, match and tag-ready can land in
400–1500 ms by streaming results. Moving the bytes cannot, and no amount of
architecture changes that.

Therefore the API streams candidate rows over SSE as each provider finishes,
rather than blocking a request until all providers are done.

## Budgets

| Upstream | Limit | Consequence of exceeding |
|---|---|---|
| SoulSeek search | 34 per 220 s, 400 ms apart | 30-minute ban |
| Discogs | 60/min with a token, 25 without | HTTP 429 |
| MusicBrainz | 1/s | IP ban |
| AcoustID | 3/s per key | rejected |

Only the SoulSeek one is dangerous and silent: a search that returns instantly
and empty is the signature of an exhausted budget. `GET /api/v1/budgets` exposes
the remaining allowance so the UI can show it.

## Schema decisions worth knowing

**`candidate` ↔ `candidate_locator` is one-to-many; `candidate` ↔ `candidate_source`
is one-to-one.** That asymmetry is the whole cross-source dedup mechanism. Three
peers and one torrent for the same album collapse to one candidate row with
`provider_count = 4`, enforced by a unique index on `(session_id, dedupe_key)`
rather than by application logic.

**`discogs_lookup` is not optional.** 60 requests per minute is 55 hours for
200,000 releases. Every release body is cached permanently. Phase 8 replaces live
search with a local index built from the CC0 monthly dumps.

**`torrent_metainfo` is keyed by info hash and never overwritten.** Resolving a
magnet costs peer connections, and two torrents with the same hash are one
torrent.

**`rate_budget` lives in the database.** A restart must not hand the client a
fresh SoulSeek allowance.

**`release_track` keeps `position_raw` next to the normalised `disc_number` and
`track_number`.** Discogs positions are `"A"`, `"1-3"`, `"2C"`. Dropping the raw
string loses vinyl sides on a re-tag.

**`release_credit.tracks_scope_raw` is not decoration.** Discogs returns one
`extraartists` array where `tracks: ""` means the whole release and
`tracks: "3, 7-9"` means those tracks. Ignore it and the producer of track 3 gets
tagged onto all twelve files.

**`tag_application` + `tag_field` is an audit log.** FLAC tags are destructive
overwrites. `tag_hash` makes re-tagging idempotent and detects drift.

## Phase plan

| Phase | Scope | Status |
|---|---|---|
| P0 | Workspace, schema, provider trait, rate registry, bearer auth | done |
| P1 | Discogs client, persistent release cache, mapping, artist and credit persistence | done |
| P1.5 | Discogs search wired end to end, with per-provider status | done |
| P1.6 | CLI-first reshape; the HTTP daemon became optional | done |
| P2a | SoulSeek search: FLAC filter, grouping, budget | done |
| P2b | Nyaa and RutTracker providers | next |
| P2c | Download the files a SoulSeek result points at | done |
| P3 | Magnet resolution via BEP-9, FLAC filtering, CUE parsing | |
| P4 | qbittorrent-nox transfer engine, staging directory, dedupe | |
| P5 | Match scoring, chromaprint and AcoustID tiebreak | |
| P6 | `lofty` tag writing, cover art, idempotent re-tag | |
| P7 | SoulSeek sharing, so downloads are not queued last | |
| P8 | Local Discogs index from the CC0 dumps | |

P7 is not optional. SoulSeek treats users who share nothing as leechers and
queues them last.

## What a search does today

`POST /api/v1/search` runs one fan-out, against Discogs:

1. Parse the query and record a `search_session`.
2. Record a `provider_fanout` row *before* the attempt, so a search that could
   not be made is still visible afterwards.
3. Spend one rate-budget token, then call `/database/search`.
4. For each hit, read the release body from `discogs_lookup` if it is there, and
   otherwise spend another token and fetch it. Bodies are cached permanently.
5. Map each body and upsert it, which writes the release, its tracklist, its
   artists, its credits and its identifiers.
6. Return the matches plus one status per provider.

A release is fetched at most once ever. A second identical search costs one
token for the query and nothing for the bodies.

The response separates three facts that would otherwise look identical:

| State | Means |
|---|---|
| `done` | The provider answered. Zero results is an answer. |
| `budget_denied` | The budget refused, so the request was never sent. |
| `not_configured` | No credentials, so it could not be attempted. |
| `failed` | It was asked and did not answer usefully. |

## What a SoulSeek search does

The network has no search index. The server relays a query into a distributed
tree and peers answer with whatever they hold, so a file on an offline machine
is invisible and the same query minutes later can answer differently. Search is
a shortlist of what is reachable now, not a catalogue.

`soulseek-rs-lib` is blocking and owns its own sockets, so it runs on a
dedicated thread and is reached over channels. It also cannot be proxied: the
distributed search needs inbound reachability that a SOCKS5 proxy cannot give.

A response is a list of *files*, not albums. They are grouped by peer and
directory, which is a heuristic: a peer who filed a record as
`OK Computer (1997)/12 Vinyl 01` produces three groups where someone else
produces one. Discogs disambiguates identity later, so a wrong guess here is
correctable rather than fatal.

FLAC is judged by extension. The `sample_rate` and `bit_depth` attributes are
read when a peer sends them and treated as absent when it does not, because a
`.flac` from a client that reports no attributes is still a FLAC.

## Three things the real network taught us

Found by running a live search and a captured Discogs body through the code
rather than trusting fixtures.

**SoulSeek has no registration step.** Logging in with an unused name creates
the account. There is nothing to sign up for separately.

**A search result carries no queue length.** The server reports a peer's upload
slots but not how many peers are queued. `SoulSeekLocator.queue_length` is
therefore `Option`, and availability follows `has_free_slot` alone.

**`ReleaseStatus` is the Discogs approval status**, not MusicBrainz's
official/promotion/bootleg. See below.

## Two things the real Discogs API taught us

Both were found by running a captured response through the mapper rather than
trusting a hand-written fixture. See
`crates/mud-api/tests/real_discogs.rs`.

**`ReleaseStatus` is the Discogs approval status.** Values are `Accepted`,
`Draft`, `Deleted`, `Rejected`. It is not the MusicBrainz
official/promotion/bootleg vocabulary, and the two must not be conflated.
Whether a pressing is a promo or a bootleg is a separate fact that Discogs keeps
in `formats[].descriptions`; it is not modelled yet.

**A mapped release has no local ids.** `ArtistRef.artist` and
`ReleaseTrack.track_id` are zero until the release is stored, so a credit
references its track by *position*, not by id. Persisting a placeholder id
violates the foreign key and aborts the whole upsert.


## Tag mapping

Per Picard's tag map. The three rules that cause bugs:

1. `COMPILATION` is **not** a release-type indicator. Derive it from
   `RELEASETYPE`'s secondary type.
2. A remix is a different recording, with its own MusicBrainz recording MBID.
   Tag the recording's artist and title, not the work's.
3. `originaldate` and `albumsort` come from the earliest release in the master,
   not from the release being tagged.

## Proxy scope

| Path | Proxied how |
|---|---|
| RutTracker, Nyaa, Discogs, MusicBrainz | `mud-net` `ProxyConfig`, SOCKS5 or HTTP |
| SoulSeek | **Not possible.** `soulseek-rs-lib` owns its socket. Its D-network also needs inbound reachability, which a proxy cannot give. |
| qbittorrent | Separate `Proxy\Host` setting in `qBittorrent.conf` |

Two independent proxy configurations, not one.