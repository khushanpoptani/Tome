ALTER TABLE job_events RENAME TO job_events_phase_1;

CREATE TABLE job_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT REFERENCES jobs(id),
    event_type TEXT NOT NULL CHECK (event_type IN (
        'job.created', 'job.queued', 'job.started', 'job.progress',
        'job.completed', 'job.failed', 'job.cancelled', 'job.interrupted',
        'inference.context_prepared', 'inference.generation_started',
        'inference.text_delta', 'inference.output_checkpoint',
        'inference.usage_updated', 'inference.stopped',
        'inference.completed', 'inference.failed', 'inference.interrupted',
        'server.warning'
    )),
    payload_json TEXT NOT NULL DEFAULT '{}',
    occurred_at TEXT NOT NULL
);

INSERT INTO job_events (event_id, job_id, event_type, payload_json, occurred_at)
SELECT event_id, job_id, event_type, payload_json, occurred_at
FROM job_events_phase_1;

DROP TABLE job_events_phase_1;
CREATE INDEX job_events_job_event_idx ON job_events (job_id, event_id);

CREATE TABLE inference_jobs (
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    model_id TEXT NOT NULL,
    model_artifact_sha256 TEXT NOT NULL,
    request_json TEXT NOT NULL,
    settings_json TEXT NOT NULL,
    context_manifest_json TEXT,
    output_text TEXT NOT NULL DEFAULT '',
    output_sequence INTEGER NOT NULL DEFAULT 0,
    usage_json TEXT,
    completion_reason TEXT,
    warnings_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX inference_jobs_model_idx ON inference_jobs (model_id, created_at);
