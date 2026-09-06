CREATE TABLE conversations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    kind VARCHAR(10) NOT NULL,
    creator_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    title VARCHAR(128),
    access_policy VARCHAR(20),
    password_hash VARCHAR(255),
    direct_user_low_id UUID REFERENCES users(id) ON DELETE CASCADE,
    direct_user_high_id UUID REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT conversations_valid_kind CHECK (kind IN ('group', 'direct')),
    CONSTRAINT conversations_valid_group CHECK ((
        (kind = 'group'
            AND title IS NOT NULL
            AND access_policy IN ('open', 'password', 'friends_only')
            AND direct_user_low_id IS NULL
            AND direct_user_high_id IS NULL
            AND ((access_policy = 'password' AND password_hash IS NOT NULL)
                OR (access_policy <> 'password' AND password_hash IS NULL)))
        OR
        (kind = 'direct'
            AND title IS NULL
            AND access_policy IS NULL
            AND password_hash IS NULL
            AND direct_user_low_id IS NOT NULL
            AND direct_user_high_id IS NOT NULL
            AND direct_user_low_id < direct_user_high_id)
    ) IS TRUE)
);

CREATE UNIQUE INDEX conversations_direct_pair_idx
    ON conversations (direct_user_low_id, direct_user_high_id);
CREATE INDEX conversations_creator_id_idx ON conversations (creator_id);

CREATE TABLE conversation_members (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role VARCHAR(10) NOT NULL DEFAULT 'member',
    joined_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (conversation_id, user_id),
    CONSTRAINT conversation_members_valid_role CHECK (role IN ('creator', 'admin', 'member'))
);

CREATE INDEX conversation_members_user_id_idx ON conversation_members (user_id);

CREATE TABLE direct_message_requests (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    requester_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recipient_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status VARCHAR(10) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT direct_message_requests_different_users CHECK (requester_id <> recipient_id),
    CONSTRAINT direct_message_requests_valid_status CHECK (status IN ('pending', 'accepted', 'declined'))
);

CREATE UNIQUE INDEX direct_message_requests_unique_pair_idx
    ON direct_message_requests (LEAST(requester_id, recipient_id), GREATEST(requester_id, recipient_id));
CREATE INDEX direct_message_requests_recipient_status_idx
    ON direct_message_requests (recipient_id, status, created_at DESC);
