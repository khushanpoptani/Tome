CREATE TABLE model_artifacts (
    id TEXT PRIMARY KEY NOT NULL,
    catalog_id TEXT NOT NULL UNIQUE,
    logical_model_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    catalog_version TEXT NOT NULL,
    provider TEXT NOT NULL,
    repository TEXT NOT NULL,
    revision TEXT NOT NULL,
    artifact_name TEXT NOT NULL,
    format TEXT NOT NULL,
    quantization TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    local_path TEXT NOT NULL UNIQUE,
    verification_state TEXT NOT NULL CHECK (verification_state IN ('verified', 'failed')),
    capabilities_json TEXT NOT NULL,
    compatibility_state TEXT NOT NULL CHECK (compatibility_state IN ('compatible', 'incompatible', 'runtime_unavailable', 'catalog_only')),
    compatibility_reason TEXT NOT NULL,
    context_limit INTEGER NOT NULL,
    tokenizer TEXT NOT NULL,
    chat_template TEXT,
    estimated_ram_bytes INTEGER NOT NULL,
    estimated_vram_bytes INTEGER,
    loaded INTEGER NOT NULL DEFAULT 0 CHECK (loaded IN (0, 1)),
    in_use_count INTEGER NOT NULL DEFAULT 0 CHECK (in_use_count >= 0),
    installed_at TEXT NOT NULL,
    verified_at TEXT NOT NULL,
    last_loaded_at TEXT,
    updated_at TEXT NOT NULL
);

CREATE INDEX model_artifacts_logical_idx ON model_artifacts (logical_model_id);

CREATE TABLE model_downloads (
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
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

CREATE TABLE model_settings (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    default_model_id TEXT REFERENCES model_artifacts(id) ON DELETE SET NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO model_settings (singleton, default_model_id, updated_at)
VALUES (1, NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

CREATE TABLE model_license_acceptances (
    catalog_id TEXT NOT NULL,
    revision TEXT NOT NULL,
    license TEXT NOT NULL,
    accepted_at TEXT NOT NULL,
    PRIMARY KEY (catalog_id, revision)
);
