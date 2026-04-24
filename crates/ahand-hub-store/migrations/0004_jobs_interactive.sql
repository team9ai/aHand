-- jobs.interactive was added to the Rust Job struct + INSERT/SELECT in
-- job_store.rs but the corresponding DDL never landed. Production hub
-- in dev failed every WS device accept path with:
--   internal: error returned from database: column "interactive" does not exist
-- because the control-plane's `POST /jobs` insert references it.
--
-- Default false matches the behavior for existing jobs (pre-interactive rows)
-- and for any control-plane client that doesn't set the field.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS interactive BOOLEAN NOT NULL DEFAULT false;
