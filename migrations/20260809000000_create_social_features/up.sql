CREATE TABLE friendships (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    requester_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    addressee_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW(),
    CONSTRAINT friendships_different_users CHECK (requester_id <> addressee_id),
    CONSTRAINT friendships_valid_status CHECK (status IN ('pending', 'accepted'))
);

CREATE UNIQUE INDEX friendships_unique_pair
    ON friendships (LEAST(requester_id, addressee_id), GREATEST(requester_id, addressee_id));
CREATE INDEX friendships_requester_id_idx ON friendships(requester_id);
CREATE INDEX friendships_addressee_id_idx ON friendships(addressee_id);

CREATE TABLE call_invitations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    caller_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    callee_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    room_id VARCHAR(128) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    expires_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    responded_at TIMESTAMP,
    CONSTRAINT call_invitations_different_users CHECK (caller_id <> callee_id),
    CONSTRAINT call_invitations_valid_status CHECK (status IN ('pending', 'accepted', 'declined'))
);

CREATE INDEX call_invitations_callee_status_idx ON call_invitations(callee_id, status);
CREATE INDEX call_invitations_expires_at_idx ON call_invitations(expires_at);

CREATE TABLE meeting_history (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    room_id VARCHAR(128) NOT NULL,
    last_joined_at TIMESTAMP NOT NULL DEFAULT NOW(),
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    CONSTRAINT meeting_history_user_room_unique UNIQUE (user_id, room_id)
);

CREATE INDEX meeting_history_user_last_joined_idx ON meeting_history(user_id, last_joined_at DESC);

CREATE TABLE user_presence (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    last_seen_at TIMESTAMP NOT NULL DEFAULT NOW()
);
