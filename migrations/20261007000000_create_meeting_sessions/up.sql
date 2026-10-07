-- One row per live period of an SFU room: it starts with the first participant and ends with the last.
CREATE TABLE meeting_sessions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    sfu_room_id VARCHAR(128) NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ,
    CONSTRAINT meeting_sessions_valid_period CHECK (ended_at IS NULL OR ended_at >= started_at)
);

CREATE UNIQUE INDEX meeting_sessions_one_live_room_idx
    ON meeting_sessions (sfu_room_id)
    WHERE ended_at IS NULL;
CREATE INDEX meeting_sessions_room_started_idx ON meeting_sessions (sfu_room_id, started_at DESC);

-- One row per signaling connection that joined the room. Guests have no user ID.
CREATE TABLE meeting_participants (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    meeting_session_id UUID NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    participant_id VARCHAR(64) NOT NULL,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    display_name VARCHAR(80) NOT NULL,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    left_at TIMESTAMPTZ,
    CONSTRAINT meeting_participants_valid_period CHECK (left_at IS NULL OR left_at >= joined_at)
);

CREATE UNIQUE INDEX meeting_participants_connection_idx
    ON meeting_participants (meeting_session_id, participant_id);
CREATE INDEX meeting_participants_user_idx
    ON meeting_participants (user_id, joined_at DESC)
    WHERE user_id IS NOT NULL;
