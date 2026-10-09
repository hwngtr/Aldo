# Aldo Data Model

Aldo uses SQLite. The database stores search history, provider results,
catalog metadata, downloads, and rate budgets.

## Entity relationship diagram

```mermaid
erDiagram
    APP_USER ||--o{ SEARCH_SESSION : creates
    SEARCH_SESSION ||--o{ PROVIDER_FANOUT : records
    SEARCH_SESSION ||--o{ CANDIDATE : contains
    SEARCH_SESSION ||--o| SELECTED_RELEASE : selects
    CANDIDATE ||--o{ CANDIDATE_LOCATOR : offers
    CANDIDATE ||--o{ CANDIDATE_FILE : contains
    CANDIDATE ||--o{ CANDIDATE_SOURCE : reported_by
    PROVIDER_FANOUT ||--o{ CANDIDATE_SOURCE : produces
    CANDIDATE_LOCATOR ||--o{ CANDIDATE_SOURCE : identifies
    CANDIDATE ||--o{ ACQUISITION : downloads
    ACQUISITION ||--o{ ASSET : tracks
    RELEASE ||--o{ RELEASE_TRACK : has
    RELEASE ||--o{ RELEASE_ARTIST : credits
    ARTIST ||--o{ RELEASE_ARTIST : appears_on
    RELEASE ||--o{ RELEASE_CREDIT : has
    ARTIST ||--o{ RELEASE_CREDIT : receives
    RELEASE ||--o{ RELEASE_IDENTIFIER : identifies
    LABEL ||--o{ RELEASE_IDENTIFIER : names
    RELEASE ||--o{ ALBUM_ART : has
    RELEASE ||--o{ DISCOGS_LOOKUP : caches
    RELEASE ||--o{ RELEASE_TRACK : maps
    RATE_BUDGET ||--o{ PROVIDER_FANOUT : limits
```

`SELECTED_RELEASE` is shown conceptually. The current schema stores the public
Discogs release ID on `search_session` as `selected_discogs_release_id`.

## Core tables

### Search tables

```mermaid
erDiagram
    SEARCH_SESSION {
        integer session_id PK
        integer user_id FK
        text raw_query
        text normalized_query
        text parsed_artist
        text parsed_album
        integer parsed_year
        text filters_json
        integer selected_discogs_release_id
        integer result_count
        integer created_at
    }
    PROVIDER_FANOUT {
        integer fanout_id PK
        integer session_id FK
        text provider
        text query_sent
        text status
        integer result_count
        integer latency_ms
        text error
        integer created_at
    }
    CANDIDATE {
        integer candidate_id PK
        integer session_id FK
        text dedupe_key
        text display_artist
        text display_album
        integer display_year
        integer track_count
        integer disc_count
        integer total_bytes
        integer created_at
    }
    SEARCH_SESSION ||--o{ PROVIDER_FANOUT : has
    SEARCH_SESSION ||--o{ CANDIDATE : has
```

The unique `(session_id, dedupe_key)` index collapses duplicate reports inside
one search. A candidate can have several locators from different peers.

### Provider source tables

```mermaid
erDiagram
    CANDIDATE {
        integer candidate_id PK
    }
    CANDIDATE_LOCATOR {
        integer locator_id PK
        integer candidate_id FK
        text kind
        integer is_primary
        text slsk_peer
        text slsk_remote_path
        integer slsk_size
        integer slsk_slot_free
        integer slsk_queue_len
        integer slsk_speed_bps
        text infohash
        integer file_index
        integer seeders
    }
    CANDIDATE_FILE {
        integer candidate_id FK
        integer ordinal
        text path
        integer bytes
    }
    CANDIDATE_SOURCE {
        integer candidate_id FK
        integer fanout_id FK
        integer locator_id FK
    }
    CANDIDATE ||--o{ CANDIDATE_LOCATOR : has
    CANDIDATE ||--o{ CANDIDATE_FILE : has
    CANDIDATE ||--o{ CANDIDATE_SOURCE : has
```

`is_primary` selects the source used for a download. Available sources are
preferred over queued sources.

### Catalog tables

```mermaid
erDiagram
    RELEASE {
        integer release_id PK
        integer discogs_release_id UK
        integer discogs_master_id
        text musicbrainz_release_id
        text title
        text release_types
        integer year
        text country
        integer disc_count
        text cover_art_url
        integer fetched_at
    }
    RELEASE_TRACK {
        integer track_id PK
        integer release_id FK
        text position_raw
        integer disc_number
        integer track_number
        text title
        text kind
        integer duration_ms
    }
    ARTIST {
        integer artist_id PK
        integer discogs_artist_id UK
        text canonical_name
        text anv
    }
    RELEASE_ARTIST {
        integer release_id FK
        integer artist_id FK
        integer position
        text join_phrase
        integer is_album_artist
    }
    RELEASE ||--o{ RELEASE_TRACK : contains
    RELEASE ||--o{ RELEASE_ARTIST : credits
    ARTIST ||--o{ RELEASE_ARTIST : appears_on
```

`position_raw` is retained because Discogs positions can be `A`, `2C`, or
`1-3`. The parsed numeric fields support ordering, but the raw value preserves
the original pressing information.

## Acquisition state

```mermaid
stateDiagram-v2
    [*] --> Planned
    Planned --> Downloading
    Downloading --> Partial
    Downloading --> Downloaded
    Partial --> Downloading
    Partial --> Failed
    Downloaded --> [*]
    Failed --> [*]
```

An acquisition has one asset row per expected file. Asset progress is separate
from acquisition progress so interrupted downloads can be resumed or diagnosed.

## Migration rules

1. Add schema changes as a new numbered SQL migration.
2. Keep migrations forward-only.
3. Enable foreign keys on every SQLite connection.
4. Use checked narrowing helpers when reading SQLite integers into domain types.
5. Preserve raw provider values when normalization could lose information.
