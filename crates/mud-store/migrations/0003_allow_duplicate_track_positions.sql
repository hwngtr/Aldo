-- Discogs tracklists can contain multiple entries with the same display
-- position. Track identity is the row id, so position_raw must not be unique.
PRAGMA defer_foreign_keys = ON;

ALTER TABLE release_track RENAME TO release_track_old;
CREATE TABLE release_track (
    track_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    release_id        INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    position_raw      TEXT    NOT NULL,
    disc_number       INTEGER NOT NULL CHECK (disc_number > 0),
    track_number      INTEGER,
    title             TEXT    NOT NULL,
    kind              TEXT    NOT NULL CHECK (kind IN ('track', 'heading', 'index')),
    duration_ms       INTEGER,
    parent_track_id   INTEGER REFERENCES release_track (track_id) ON DELETE CASCADE
);
INSERT INTO release_track
    (track_id, release_id, position_raw, disc_number, track_number, title, kind,
     duration_ms, parent_track_id)
SELECT track_id, release_id, position_raw, disc_number, track_number, title, kind,
       duration_ms, parent_track_id
FROM release_track_old;

ALTER TABLE release_credit RENAME TO release_credit_old;
CREATE TABLE release_credit (
    credit_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    release_id         INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    release_track_id   INTEGER REFERENCES release_track (track_id) ON DELETE CASCADE,
    artist_id          INTEGER NOT NULL REFERENCES artist (artist_id) ON DELETE CASCADE,
    role               TEXT    NOT NULL,
    anv                TEXT,
    tracks_scope_raw   TEXT
);
INSERT INTO release_credit
    (credit_id, release_id, release_track_id, artist_id, role, anv, tracks_scope_raw)
SELECT credit_id, release_id, release_track_id, artist_id, role, anv, tracks_scope_raw
FROM release_credit_old;
DROP TABLE release_credit_old;

ALTER TABLE asset RENAME TO asset_old;
CREATE TABLE asset (
    asset_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    acquisition_id    INTEGER NOT NULL REFERENCES acquisition (acquisition_id) ON DELETE CASCADE,
    release_track_id  INTEGER REFERENCES release_track (track_id) ON DELETE SET NULL,
    ordinal           INTEGER NOT NULL CHECK (ordinal >= 0),
    rel_path          TEXT    NOT NULL,
    bytes             INTEGER NOT NULL CHECK (bytes >= 0),
    state             TEXT    NOT NULL CHECK (state IN ('expected', 'partial', 'complete', 'verified')),
    sha256_audio      TEXT CHECK (sha256_audio IS NULL OR length(sha256_audio) = 64),
    UNIQUE (acquisition_id, ordinal)
);
INSERT INTO asset
    (asset_id, acquisition_id, release_track_id, ordinal, rel_path, bytes, state, sha256_audio)
SELECT asset_id, acquisition_id, release_track_id, ordinal, rel_path, bytes, state, sha256_audio
FROM asset_old;

ALTER TABLE asset_probe RENAME TO asset_probe_old;
CREATE TABLE asset_probe (
    asset_id        INTEGER PRIMARY KEY REFERENCES asset (asset_id) ON DELETE CASCADE,
    duration_ms     INTEGER NOT NULL CHECK (duration_ms >= 0),
    sample_rate_hz  INTEGER NOT NULL CHECK (sample_rate_hz > 0),
    bit_depth       INTEGER NOT NULL CHECK (bit_depth IN (8, 16, 24, 32)),
    channels        INTEGER NOT NULL CHECK (channels BETWEEN 1 AND 8),
    codec           TEXT    NOT NULL,
    bitrate_kbps    REAL CHECK (bitrate_kbps IS NULL OR bitrate_kbps >= 0)
);
INSERT INTO asset_probe
    (asset_id, duration_ms, sample_rate_hz, bit_depth, channels, codec, bitrate_kbps)
SELECT asset_id, duration_ms, sample_rate_hz, bit_depth, channels, codec, bitrate_kbps
FROM asset_probe_old;

DROP TABLE asset_probe_old;
DROP TABLE asset_old;
DROP TABLE release_track_old;
CREATE INDEX release_track_by_disc ON release_track (release_id, disc_number, track_number);
