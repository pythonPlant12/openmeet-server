-- Missed calls the user has read: opened their details or swiped them read. Unread missed calls show a
-- dot in the Calls list and count toward the Calls badge.
CREATE TABLE call_read_states (
    call_session_id UUID NOT NULL REFERENCES call_sessions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    read_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (call_session_id, user_id)
);
