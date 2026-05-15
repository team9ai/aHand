-- Add resolved execution mode without breaking old hub binaries.
--
-- The legacy `interactive` boolean remains the compatibility field:
--   interactive=false -> batch
--   interactive=true  -> pty
--
-- The default lets old INSERT statements keep working after this migration.
ALTER TABLE jobs
  ADD COLUMN IF NOT EXISTS execution_mode TEXT NOT NULL DEFAULT 'batch';

UPDATE jobs
SET execution_mode = CASE
  WHEN interactive = true THEN 'pty'
  ELSE 'batch'
END
WHERE execution_mode IS NULL OR execution_mode = 'batch';
