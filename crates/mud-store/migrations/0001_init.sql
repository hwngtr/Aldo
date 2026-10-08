-- MuD schema. Types are narrow on purpose: ids are INTEGER, byte counts are
-- INTEGER (never REAL), and every column a remote source can influence is
-- constrained so a malformed value is rejected at the boundary.

CREATE TABLE release (
    release_id              INTEGER PRIMARY KEY AUTOINCREMENT,
    discogs_release_id      INTEGER NOT NULL UNIQUE,
    discogs_master_id       INTEGER,
    musicbrainz_release_id  TEXT CHECK (musicbrainz_release_id IS NULL OR length(musicbrainz_release_id) = 36),
    title                   TEXT    NOT NULL,
    release_types           TEXT    NOT NULL DEFAULT '',
    media                   TEXT,
    status                  TEXT    NOT NULL,
    year                    INTEGER CHECK (year IS NULL OR year BETWEEN 1800 AND 2100),
    country                 TEXT,
    disc_count              INTEGER NOT NULL DEFAULT 1 CHECK (disc_count BETWEEN 1 AND 99),
    duration_ms             INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    cover_art_url           TEXT,
    popularity              INTEGER CHECK (popularity IS NULL OR popularity >= 0),
    genres                  TEXT    NOT NULL DEFAULT '',
    styles                  TEXT    NOT NULL DEFAULT '',
    fetched_at              INTEGER NOT NULL
);

CREATE INDEX release_by_master ON release (discogs_master_id);

CREATE TABLE master (
    master_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    discogs_master_id  INTEGER NOT NULL UNIQUE,
    title              TEXT    NOT NULL,
    year               INTEGER,
    track_count        INTEGER
);

CREATE TABLE artist (
    artist_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    discogs_artist_id  INTEGER,
    canonical_name     TEXT    NOT NULL,
    realname           TEXT,
    anv                TEXT,
    UNIQUE (discogs_artist_id)
);

CREATE TABLE label (
    label_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    discogs_label_id  INTEGER,
    name              TEXT NOT NULL,
    UNIQUE (discogs_label_id)
);

CREATE TABLE release_artist (
    release_id     INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    artist_id      INTEGER NOT NULL REFERENCES artist (artist_id) ON DELETE CASCADE,
    position       INTEGER NOT NULL CHECK (position >= 0),
    join_phrase    TEXT,
    anv            TEXT,
    is_album_artist INTEGER NOT NULL DEFAULT 0 CHECK (is_album_artist IN (0, 1)),
    PRIMARY KEY (release_id, artist_id)
);

CREATE TABLE release_track (
    track_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    release_id    INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    position_raw  TEXT    NOT NULL,
    disc_number   INTEGER NOT NULL CHECK (disc_number > 0),
    track_number  INTEGER,
    title         TEXT    NOT NULL,
    kind          TEXT    NOT NULL CHECK (kind IN ('track', 'heading', 'index')),
    duration_ms   INTEGER,
    parent_track_id INTEGER REFERENCES release_track (track_id) ON DELETE CASCADE,
    UNIQUE (release_id, position_raw)
);

CREATE INDEX release_track_by_disc ON release_track (release_id, disc_number, track_number);

CREATE TABLE release_credit (
    credit_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    release_id         INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    release_track_id   INTEGER REFERENCES release_track (track_id) ON DELETE CASCADE,
    artist_id          INTEGER NOT NULL REFERENCES artist (artist_id) ON DELETE CASCADE,
    role               TEXT    NOT NULL,
    anv                TEXT,
    tracks_scope_raw   TEXT
);

CREATE TABLE release_identifier (
    release_id      INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    label_id        INTEGER REFERENCES label (label_id) ON DELETE SET NULL,
    discogs_label_id INTEGER,
    label_name      TEXT    NOT NULL,
    catalog_number  TEXT,
    barcode         TEXT,
    PRIMARY KEY (release_id, label_name, catalog_number, barcode)
);

CREATE TABLE album_art (
    art_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    release_id  INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    url         TEXT    NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN ('front', 'back', 'booklet', 'other')),
    is_primary  INTEGER NOT NULL DEFAULT 0 CHECK (is_primary IN (0, 1))
);

CREATE TABLE app_user (
    user_id            INTEGER PRIMARY KEY CHECK (user_id = 1),
    slsk_username      TEXT,
    slsk_password_md5  TEXT,
    discogs_token      TEXT,
    proxy_url          TEXT,
    library_root       TEXT    NOT NULL,
    created_at         INTEGER NOT NULL
);

CREATE TABLE search_session (
    session_id        INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id           INTEGER NOT NULL REFERENCES app_user (user_id) ON DELETE CASCADE,
    raw_query         TEXT    NOT NULL,
    normalized_query  TEXT    NOT NULL,
    parsed_artist     TEXT,
    parsed_album      TEXT,
    parsed_year       INTEGER CHECK (parsed_year IS NULL OR parsed_year BETWEEN 0 AND 65535),
    filters_json      TEXT    NOT NULL,
    created_at        INTEGER NOT NULL,
    result_count      INTEGER NOT NULL DEFAULT 0 CHECK (result_count >= 0)
);

CREATE INDEX search_session_by_user ON search_session (user_id, created_at DESC);

CREATE TABLE provider_fanout (
    fanout_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id         INTEGER NOT NULL REFERENCES search_session (session_id) ON DELETE CASCADE,
    parent_fanout_id   INTEGER REFERENCES provider_fanout (fanout_id) ON DELETE CASCADE,
    provider           TEXT    NOT NULL CHECK (provider IN ('soulseek', 'tracker', 'torrent-meta', 'discogs', 'musicbrainz')),
    query_sent         TEXT    NOT NULL,
    status             TEXT    NOT NULL CHECK (status IN ('pending', 'running', 'done', 'error', 'budget_denied')),
    result_count       INTEGER NOT NULL DEFAULT 0 CHECK (result_count >= 0),
    latency_ms         INTEGER CHECK (latency_ms IS NULL OR latency_ms >= 0),
    error              TEXT,
    created_at         INTEGER NOT NULL
);

CREATE INDEX provider_fanout_by_session ON provider_fanout (session_id);

CREATE TABLE candidate (
    candidate_id  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    INTEGER NOT NULL REFERENCES search_session (session_id) ON DELETE CASCADE,
    dedupe_key    TEXT    NOT NULL CHECK (length(dedupe_key) = 64),
    display_artist TEXT   NOT NULL,
    display_album  TEXT   NOT NULL,
    display_year   INTEGER,
    track_count    INTEGER NOT NULL DEFAULT 0 CHECK (track_count >= 0),
    disc_count     INTEGER NOT NULL DEFAULT 1 CHECK (disc_count BETWEEN 0 AND 255),
    total_bytes    INTEGER NOT NULL DEFAULT 0 CHECK (total_bytes >= 0),
    created_at     INTEGER NOT NULL
);

CREATE UNIQUE INDEX candidate_dedupe_per_session ON candidate (session_id, dedupe_key);

CREATE TABLE candidate_locator (
    locator_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    candidate_id     INTEGER NOT NULL REFERENCES candidate (candidate_id) ON DELETE CASCADE,
    kind             TEXT    NOT NULL CHECK (kind IN ('soulseek', 'torrent')),
    is_primary       INTEGER NOT NULL DEFAULT 0 CHECK (is_primary IN (0, 1)),
    slsk_peer        TEXT,
    slsk_remote_path TEXT,
    slsk_size        INTEGER CHECK (slsk_size IS NULL OR slsk_size >= 0),
    slsk_slot_free   INTEGER CHECK (slsk_slot_free IS NULL OR slsk_slot_free IN (0, 1)),
    slsk_queue_len   INTEGER CHECK (slsk_queue_len IS NULL OR slsk_queue_len >= 0),
    slsk_speed_bps   INTEGER CHECK (slsk_speed_bps IS NULL OR slsk_speed_bps >= 0),
    sample_rate_hz   INTEGER CHECK (sample_rate_hz IS NULL OR sample_rate_hz >= 0),
    bit_depth        INTEGER CHECK (bit_depth IS NULL OR bit_depth BETWEEN 0 AND 255),
    duration_ms      INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    infohash         TEXT CHECK (infohash IS NULL OR length(infohash) = 40),
    file_index       INTEGER CHECK (file_index IS NULL OR file_index >= 0),
    tracker          TEXT,
    seeders          INTEGER CHECK (seeders IS NULL OR seeders >= 0),
    leechers         INTEGER CHECK (leechers IS NULL OR leechers >= 0)
);

CREATE INDEX candidate_locator_by_candidate ON candidate_locator (candidate_id);
CREATE INDEX candidate_locator_by_infohash ON candidate_locator (infohash);

CREATE TABLE candidate_source (
    source_id     INTEGER PRIMARY KEY AUTOINCREMENT,
    candidate_id  INTEGER NOT NULL REFERENCES candidate (candidate_id) ON DELETE CASCADE,
    fanout_id     INTEGER NOT NULL REFERENCES provider_fanout (fanout_id) ON DELETE CASCADE,
    locator_id    INTEGER NOT NULL REFERENCES candidate_locator (locator_id) ON DELETE CASCADE,
    UNIQUE (fanout_id, locator_id)
);

CREATE TABLE candidate_file (
    candidate_id  INTEGER NOT NULL REFERENCES candidate (candidate_id) ON DELETE CASCADE,
    ordinal       INTEGER NOT NULL CHECK (ordinal >= 0),
    path          TEXT    NOT NULL,
    bytes         INTEGER NOT NULL CHECK (bytes >= 0),
    PRIMARY KEY (candidate_id, ordinal)
);

CREATE TABLE match_proposal (
    proposal_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    candidate_id  INTEGER NOT NULL REFERENCES candidate (candidate_id) ON DELETE CASCADE,
    release_id    INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    score         REAL    NOT NULL CHECK (score >= 0.0 AND score <= 1.0),
    strategy      TEXT    NOT NULL CHECK (strategy IN ('title', 'tracklist', 'fingerprint', 'manual')),
    decision      TEXT    NOT NULL CHECK (decision IN ('auto', 'accepted', 'rejected')),
    reject_reason TEXT,
    decided_at    INTEGER NOT NULL,
    UNIQUE (candidate_id, release_id, strategy)
);

CREATE TABLE acquisition (
    acquisition_id INTEGER PRIMARY KEY AUTOINCREMENT,
    candidate_id   INTEGER NOT NULL REFERENCES candidate (candidate_id) ON DELETE CASCADE,
    user_id        INTEGER NOT NULL REFERENCES app_user (user_id) ON DELETE CASCADE,
    engine         TEXT    NOT NULL CHECK (engine IN ('soulseek', 'qbittorrent')),
    status         TEXT    NOT NULL CHECK (status IN ('queued', 'downloading', 'partial', 'downloaded', 'analyzed', 'matched', 'tagged', 'failed', 'cancelled')),
    qb_hash        TEXT CHECK (qb_hash IS NULL OR length(qb_hash) = 40),
    qb_savepath    TEXT,
    qb_ratio       REAL,
    bytes_done     INTEGER NOT NULL DEFAULT 0 CHECK (bytes_done >= 0),
    bytes_total    INTEGER NOT NULL DEFAULT 0 CHECK (bytes_total >= 0),
    speed_bps      INTEGER NOT NULL DEFAULT 0 CHECK (speed_bps >= 0),
    started_at     INTEGER,
    finished_at    INTEGER,
    created_at     INTEGER NOT NULL,
    UNIQUE (candidate_id, engine)
);

CREATE TABLE qbittorrent_event (
    event_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    acquisition_id INTEGER NOT NULL REFERENCES acquisition (acquisition_id) ON DELETE CASCADE,
    kind          TEXT NOT NULL,
    qb_state      TEXT NOT NULL,
    progress      REAL NOT NULL CHECK (progress >= 0.0 AND progress <= 1.0),
    received_at   INTEGER NOT NULL
);

CREATE INDEX qbittorrent_event_by_acquisition ON qbittorrent_event (acquisition_id, event_id);

CREATE TABLE asset (
    asset_id         INTEGER PRIMARY KEY AUTOINCREMENT,
    acquisition_id   INTEGER NOT NULL REFERENCES acquisition (acquisition_id) ON DELETE CASCADE,
    release_track_id INTEGER REFERENCES release_track (track_id) ON DELETE SET NULL,
    ordinal          INTEGER NOT NULL CHECK (ordinal >= 0),
    rel_path         TEXT    NOT NULL,
    bytes            INTEGER NOT NULL CHECK (bytes >= 0),
    state            TEXT    NOT NULL CHECK (state IN ('expected', 'partial', 'complete', 'verified')),
    sha256_audio     TEXT CHECK (sha256_audio IS NULL OR length(sha256_audio) = 64),
    UNIQUE (acquisition_id, ordinal)
);

CREATE TABLE asset_probe (
    asset_id      INTEGER PRIMARY KEY REFERENCES asset (asset_id) ON DELETE CASCADE,
    duration_ms   INTEGER NOT NULL CHECK (duration_ms >= 0),
    sample_rate_hz INTEGER NOT NULL CHECK (sample_rate_hz > 0),
    bit_depth     INTEGER NOT NULL CHECK (bit_depth IN (8, 16, 24, 32)),
    channels      INTEGER NOT NULL CHECK (channels BETWEEN 1 AND 8),
    codec         TEXT    NOT NULL,
    bitrate_kbps  REAL CHECK (bitrate_kbps IS NULL OR bitrate_kbps >= 0)
);

CREATE TABLE asset_fingerprint (
    asset_id          INTEGER PRIMARY KEY REFERENCES asset (asset_id) ON DELETE CASCADE,
    chromaprint       TEXT    NOT NULL,
    duration_seconds  INTEGER NOT NULL CHECK (duration_seconds >= 0),
    acoustid_id       TEXT,
    recording_mbid    TEXT,
    score             REAL    CHECK (score IS NULL OR (score >= 0.0 AND score <= 1.0))
);

CREATE TABLE cue_sheet (
    cue_id         INTEGER PRIMARY KEY AUTOINCREMENT,
    acquisition_id INTEGER NOT NULL UNIQUE REFERENCES acquisition (acquisition_id) ON DELETE CASCADE,
    title          TEXT NOT NULL,
    performer      TEXT,
    catalog_number TEXT,
    year           INTEGER CHECK (year IS NULL OR year BETWEEN 0 AND 65535),
    raw            TEXT NOT NULL
);

CREATE TABLE cue_track (
    cue_track_id INTEGER PRIMARY KEY AUTOINCREMENT,
    cue_id       INTEGER NOT NULL REFERENCES cue_sheet (cue_id) ON DELETE CASCADE,
    disc_number  INTEGER NOT NULL CHECK (disc_number > 0),
    track_number INTEGER NOT NULL CHECK (track_number > 0),
    title        TEXT    NOT NULL,
    performer    TEXT,
    file_ref     TEXT
);

CREATE TABLE tag_application (
    tag_app_id     INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_id       INTEGER NOT NULL REFERENCES asset (asset_id) ON DELETE CASCADE,
    release_id     INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    mb_release_id  TEXT,
    mb_recording_id TEXT,
    tag_hash       TEXT    NOT NULL,
    applied_at     INTEGER NOT NULL,
    reverted       INTEGER NOT NULL DEFAULT 0 CHECK (reverted IN (0, 1))
);

CREATE INDEX tag_application_by_asset ON tag_application (asset_id, tag_app_id DESC);

CREATE TABLE tag_field (
    tag_field_id INTEGER PRIMARY KEY AUTOINCREMENT,
    tag_app_id   INTEGER NOT NULL REFERENCES tag_application (tag_app_id) ON DELETE CASCADE,
    key          TEXT    NOT NULL,
    value        TEXT    NOT NULL,
    ordinal      INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX tag_field_by_application ON tag_field (tag_app_id);

-- Rate limiter state survives restarts: a SoulSeek ban costs 30 minutes, so a
-- budget must never reset just because the process did.
-- Discogs response cache. /database/search needs a token and only allows 60
-- requests per minute, so every release we have seen is kept here forever and
-- never re-fetched. 60/min is 55 hours for 200k releases.
CREATE TABLE discogs_lookup (
    discogs_release_id  INTEGER PRIMARY KEY,
    payload_json        TEXT    NOT NULL,
    fetched_at          INTEGER NOT NULL
);

CREATE INDEX discogs_lookup_by_age ON discogs_lookup (fetched_at);

-- Torrent file listings resolved from magnets, keyed by info hash. Resolving a
-- magnet costs peer connections, so the listing is cached permanently.
CREATE TABLE torrent_metainfo (
    infohash      TEXT PRIMARY KEY CHECK (length(infohash) = 40),
    payload_b64   BLOB    NOT NULL,
    name          TEXT    NOT NULL,
    total_bytes   INTEGER NOT NULL CHECK (total_bytes >= 0),
    flac_count    INTEGER NOT NULL CHECK (flac_count >= 0),
    resolved_at   INTEGER NOT NULL
);

CREATE TABLE rate_budget (
    provider          TEXT PRIMARY KEY,
    capacity          INTEGER NOT NULL CHECK (capacity > 0),
    window_ms         INTEGER NOT NULL CHECK (window_ms > 0),
    available         INTEGER NOT NULL CHECK (available >= 0),
    refilled_at       INTEGER NOT NULL,
    min_interval_ms   INTEGER NOT NULL DEFAULT 0,
    last_used_at      INTEGER
);