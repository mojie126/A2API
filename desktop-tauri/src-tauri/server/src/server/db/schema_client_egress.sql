ALTER TABLE requests ADD COLUMN identified_client TEXT NOT NULL DEFAULT '';
ALTER TABLE requests ADD COLUMN outbound_client TEXT NOT NULL DEFAULT '';
ALTER TABLE requests ADD COLUMN outbound_user_agent TEXT NOT NULL DEFAULT '';
