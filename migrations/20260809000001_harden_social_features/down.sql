DROP INDEX IF EXISTS call_invitations_one_pending_pair_idx;
DROP INDEX IF EXISTS call_invitations_caller_id_idx;

ALTER TABLE meeting_history DROP CONSTRAINT IF EXISTS meeting_history_valid_room_id;
ALTER TABLE call_invitations DROP CONSTRAINT IF EXISTS call_invitations_valid_expiration;
ALTER TABLE call_invitations DROP CONSTRAINT IF EXISTS call_invitations_valid_room_id;
UPDATE call_invitations SET status = 'declined' WHERE status = 'expired';
ALTER TABLE call_invitations DROP CONSTRAINT call_invitations_valid_status;
ALTER TABLE call_invitations ADD CONSTRAINT call_invitations_valid_status
    CHECK (status IN ('pending', 'accepted', 'declined'));

ALTER TABLE user_presence
    ALTER COLUMN last_seen_at TYPE TIMESTAMP USING last_seen_at AT TIME ZONE 'UTC';

ALTER TABLE meeting_history
    ALTER COLUMN last_joined_at TYPE TIMESTAMP USING last_joined_at AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC';

ALTER TABLE call_invitations
    ALTER COLUMN expires_at TYPE TIMESTAMP USING expires_at AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN responded_at TYPE TIMESTAMP USING responded_at AT TIME ZONE 'UTC';

ALTER TABLE friendships
    ALTER COLUMN created_at TYPE TIMESTAMP USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMP USING updated_at AT TIME ZONE 'UTC';
