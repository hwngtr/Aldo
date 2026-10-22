PRAGMA defer_foreign_keys = ON;

ALTER TABLE asset_fingerprint RENAME TO asset_fingerprint_old;
ALTER TABLE tag_application RENAME TO tag_application_old;
ALTER TABLE tag_field RENAME TO tag_field_old;
DROP INDEX IF EXISTS tag_application_by_asset;
DROP INDEX IF EXISTS tag_field_by_application;

CREATE TABLE asset_fingerprint (
    asset_id          INTEGER PRIMARY KEY REFERENCES asset (asset_id) ON DELETE CASCADE,
    chromaprint       TEXT    NOT NULL,
    duration_seconds  INTEGER NOT NULL CHECK (duration_seconds >= 0),
    acoustid_id       TEXT,
    recording_mbid    TEXT,
    score             REAL    CHECK (score IS NULL OR (score >= 0.0 AND score <= 1.0))
);

CREATE TABLE tag_application (
    tag_app_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_id        INTEGER NOT NULL REFERENCES asset (asset_id) ON DELETE CASCADE,
    release_id      INTEGER NOT NULL REFERENCES release (release_id) ON DELETE CASCADE,
    mb_release_id   TEXT,
    mb_recording_id TEXT,
    tag_hash        TEXT    NOT NULL,
    applied_at      INTEGER NOT NULL,
    reverted        INTEGER NOT NULL DEFAULT 0 CHECK (reverted IN (0, 1))
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

INSERT INTO asset_fingerprint
    (asset_id, chromaprint, duration_seconds, acoustid_id, recording_mbid, score)
SELECT asset_id, chromaprint, duration_seconds, acoustid_id, recording_mbid, score
FROM asset_fingerprint_old;

INSERT INTO tag_application
    (tag_app_id, asset_id, release_id, mb_release_id, mb_recording_id, tag_hash,
     applied_at, reverted)
SELECT tag_app_id, asset_id, release_id, mb_release_id, mb_recording_id, tag_hash,
       applied_at, reverted
FROM tag_application_old;

INSERT INTO tag_field (tag_field_id, tag_app_id, key, value, ordinal)
SELECT tag_field_id, tag_app_id, key, value, ordinal
FROM tag_field_old;

DROP TABLE tag_field_old;
DROP TABLE tag_application_old;
DROP TABLE asset_fingerprint_old;