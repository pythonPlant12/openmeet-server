ALTER TABLE users
    ADD COLUMN status VARCHAR(20) NOT NULL DEFAULT 'available',
    ADD COLUMN status_message VARCHAR(255) NOT NULL DEFAULT 'Available for a call',
    ADD CONSTRAINT users_valid_status
        CHECK (status IN ('available', 'away', 'do_not_disturb', 'offline'));
