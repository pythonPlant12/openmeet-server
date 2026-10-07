ALTER TABLE users DROP CONSTRAINT users_valid_status;
ALTER TABLE users ADD CONSTRAINT users_valid_status
    CHECK (status IN ('available', 'away', 'do_not_disturb', 'sleeping', 'offline'));
