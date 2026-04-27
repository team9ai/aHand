ALTER TABLE devices ADD COLUMN external_user_id TEXT;
CREATE INDEX devices_external_user_id_idx ON devices(external_user_id);
