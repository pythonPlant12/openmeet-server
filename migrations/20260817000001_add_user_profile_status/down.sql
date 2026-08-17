ALTER TABLE users
    DROP CONSTRAINT IF EXISTS users_valid_status,
    DROP COLUMN IF EXISTS status_message,
    DROP COLUMN IF EXISTS status;
