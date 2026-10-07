-- Meetings started by signed-in users have an owner who decides who may join. Rooms without a row
-- (guest meetings and meetings created before this table) stay open to anyone with the link.
CREATE TABLE meeting_rooms (
    room_id VARCHAR(128) PRIMARY KEY,
    owner_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    access_policy VARCHAR(20) NOT NULL DEFAULT 'open',
    password_hash TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT meeting_rooms_valid_access_policy CHECK (
        access_policy IN ('open', 'password', 'friends_only', 'friends_of_friends')
    ),
    CONSTRAINT meeting_rooms_password_matches_policy CHECK (
        (access_policy = 'password') = (password_hash IS NOT NULL)
    )
);

CREATE INDEX meeting_rooms_owner_idx ON meeting_rooms (owner_id, created_at DESC);

-- An invitation admits one person whatever the room's access policy is.
CREATE TABLE meeting_room_invitations (
    room_id VARCHAR(128) NOT NULL REFERENCES meeting_rooms(room_id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    invited_by UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (room_id, user_id)
);
