CREATE TABLE model_downloads_v3 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT UNIQUE REFERENCES jobs(id) ON DELETE SET NULL,
    catalog_id TEXT NOT NULL,
    part_path TEXT NOT NULL,
    final_path TEXT NOT NULL,
    bytes_downloaded INTEGER NOT NULL DEFAULT 0,
    expected_bytes INTEGER NOT NULL,
    etag TEXT,
    range_supported INTEGER,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO model_downloads_v3 (
    job_id, catalog_id, part_path, final_path, bytes_downloaded,
    expected_bytes, etag, range_supported, created_at, updated_at
)
SELECT
    job_id, catalog_id, part_path, final_path, bytes_downloaded,
    expected_bytes, etag, range_supported, created_at, updated_at
FROM model_downloads;

DROP TABLE model_downloads;
ALTER TABLE model_downloads_v3 RENAME TO model_downloads;
CREATE INDEX model_downloads_catalog_idx ON model_downloads (catalog_id);
