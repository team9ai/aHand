CREATE TABLE devices (
    id TEXT PRIMARY KEY,
    public_key BYTEA,
    hostname TEXT NOT NULL,
    os TEXT NOT NULL,
    capabilities TEXT[] NOT NULL DEFAULT '{}',
    version TEXT,
    auth_method TEXT NOT NULL,
    registered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE jobs (
    id UUID PRIMARY KEY,
    device_id TEXT NOT NULL REFERENCES devices(id),
    tool TEXT NOT NULL,
    args TEXT[] NOT NULL DEFAULT '{}',
    cwd TEXT,
    env JSONB NOT NULL DEFAULT '{}'::jsonb,
    timeout_ms BIGINT NOT NULL,
    status TEXT NOT NULL,
    exit_code INT,
    error TEXT,
    output_summary TEXT,
    requested_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ
);

CREATE TABLE audit_logs (
    id BIGSERIAL PRIMARY KEY,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT now(),
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    detail JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_ip TEXT
);
