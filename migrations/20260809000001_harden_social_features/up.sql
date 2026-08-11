ALTER TABLE friendships
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN updated_at TYPE TIMESTAMPTZ USING updated_at AT TIME ZONE 'UTC';

ALTER TABLE call_invitations
    ALTER COLUMN expires_at TYPE TIMESTAMPTZ USING expires_at AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC',
    ALTER COLUMN responded_at TYPE TIMESTAMPTZ USING responded_at AT TIME ZONE 'UTC';

ALTER TABLE meeting_history
    ALTER COLUMN last_joined_at TYPE TIMESTAMPTZ USING last_joined_at AT TIME ZONE 'UTC',
    ALTER COLUMN created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC';

ALTER TABLE user_presence
    ALTER COLUMN last_seen_at TYPE TIMESTAMPTZ USING last_seen_at AT TIME ZONE 'UTC';

ALTER TABLE call_invitations DROP CONSTRAINT call_invitations_valid_status;
ALTER TABLE call_invitations ADD CONSTRAINT call_invitations_valid_status
    CHECK (status IN ('pending', 'accepted', 'declined', 'expired'));
ALTER TABLE call_invitations ADD CONSTRAINT call_invitations_valid_room_id
    CHECK (room_id ~ '^[A-Za-z0-9_-]{1,128}$');
ALTER TABLE call_invitations ADD CONSTRAINT call_invitations_valid_expiration
    CHECK (expires_at > created_at);

ALTER TABLE meeting_history ADD CONSTRAINT meeting_history_valid_room_id
    CHECK (room_id ~ '^[A-Za-z0-9_-]{1,128}$');

CREATE INDEX call_invitations_caller_id_idx ON call_invitations(caller_id);
CREATE UNIQUE INDEX call_invitations_one_pending_pair_idx
    ON call_invitations(caller_id, callee_id)
    WHERE status = 'pending';
