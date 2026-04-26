CREATE TABLE webhook_deliveries (
    event_id TEXT PRIMARY KEY,
    payload JSONB NOT NULL,
    attempts INT NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX webhook_deliveries_next_retry_at_idx
    ON webhook_deliveries(next_retry_at);
