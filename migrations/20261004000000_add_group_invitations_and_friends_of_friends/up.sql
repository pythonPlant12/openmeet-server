ALTER TABLE conversations DROP CONSTRAINT conversations_valid_group;
ALTER TABLE conversations ADD CONSTRAINT conversations_valid_group CHECK ((
    (kind = 'group'
        AND title IS NOT NULL
        AND access_policy IN ('open', 'password', 'friends_only', 'friends_of_friends')
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
) IS TRUE);

CREATE TABLE group_invitations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    inviter_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    invitee_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status VARCHAR(10) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    responded_at TIMESTAMPTZ,
    CONSTRAINT group_invitations_different_users CHECK (inviter_id <> invitee_id),
    CONSTRAINT group_invitations_valid_status CHECK (status IN ('pending', 'accepted', 'declined'))
);

CREATE UNIQUE INDEX group_invitations_one_pending_idx
    ON group_invitations (conversation_id, invitee_id)
    WHERE status = 'pending';
CREATE INDEX group_invitations_invitee_status_idx
    ON group_invitations (invitee_id, status, created_at DESC);
