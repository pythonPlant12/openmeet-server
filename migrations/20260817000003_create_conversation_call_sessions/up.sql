CREATE TABLE call_sessions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    initiator_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    sfu_room_id VARCHAR(128) NOT NULL UNIQUE,
    status VARCHAR(10) NOT NULL DEFAULT 'active',
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT call_sessions_valid_status CHECK (status IN ('active', 'expired'))
);

CREATE UNIQUE INDEX call_sessions_one_active_conversation_idx
    ON call_sessions (conversation_id)
    WHERE status = 'active';
CREATE INDEX call_sessions_expiry_idx ON call_sessions (expires_at) WHERE status = 'active';

CREATE TABLE call_session_members (
    call_session_id UUID NOT NULL REFERENCES call_sessions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status VARCHAR(10) NOT NULL,
    responded_at TIMESTAMPTZ,
    PRIMARY KEY (call_session_id, user_id),
    CONSTRAINT call_session_members_valid_status CHECK (status IN ('accepted', 'pending', 'declined')),
    CONSTRAINT call_session_members_accepted_response CHECK (
        (status = 'pending' AND responded_at IS NULL)
        OR (status IN ('accepted', 'declined') AND responded_at IS NOT NULL)
    )
);

CREATE INDEX call_session_members_incoming_idx
    ON call_session_members (user_id, call_session_id)
    WHERE status = 'pending';
