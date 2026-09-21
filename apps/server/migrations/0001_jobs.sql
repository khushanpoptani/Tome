CREATE TABLE schema_metadata (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

INSERT INTO schema_metadata (key, value) VALUES ('protocol_version', '1');

CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    job_type TEXT NOT NULL CHECK (job_type IN (
        'inference',
        'model_download',
        'model_verification',
        'model_load',
        'model_unload',
        'attachment_processing',
        'ocr',
        'transcription'
    )),
    state TEXT NOT NULL CHECK (state IN (
        'queued', 'running', 'completed', 'failed', 'cancelled', 'interrupted'
    )),
    progress REAL NOT NULL DEFAULT 0.0 CHECK (progress >= 0.0 AND progress <= 1.0),
    input_json TEXT NOT NULL DEFAULT '{}',
    output_json TEXT,
    error_json TEXT,
    parent_job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL,
    retry_of_job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
);

CREATE INDEX jobs_state_created_idx ON jobs (state, created_at);
CREATE INDEX jobs_parent_idx ON jobs (parent_job_id);
CREATE INDEX jobs_retry_idx ON jobs (retry_of_job_id);

CREATE TABLE job_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT REFERENCES jobs(id),
    event_type TEXT NOT NULL CHECK (event_type IN (
        'job.created',
        'job.queued',
        'job.started',
        'job.progress',
        'job.completed',
        'job.failed',
        'job.cancelled',
        'job.interrupted',
        'server.warning'
    )),
    payload_json TEXT NOT NULL DEFAULT '{}',
    occurred_at TEXT NOT NULL
);

CREATE INDEX job_events_job_event_idx ON job_events (job_id, event_id);
