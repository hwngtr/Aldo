-- Daily, user-facing indexes are separate from stable internal candidate ids.
-- A new search replaces that day's mapping; the next UTC day starts at 1 again.
CREATE TABLE daily_result_index (
    utc_day       INTEGER NOT NULL,
    result_index  INTEGER NOT NULL CHECK (result_index BETWEEN 1 AND 10),
    candidate_id  INTEGER NOT NULL REFERENCES candidate (candidate_id) ON DELETE CASCADE,
    PRIMARY KEY (utc_day, result_index)
);
