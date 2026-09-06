CREATE TABLE conversation_read_states (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_read_sequence BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (conversation_id, user_id),
    CONSTRAINT conversation_read_states_nonnegative_sequence CHECK (last_read_sequence >= 0)
);

CREATE INDEX conversation_read_states_user_id_idx
    ON conversation_read_states (user_id, conversation_id);

-- Existing history predates unread tracking, so preserve its read state at rollout.
WITH participants AS (
    SELECT conversation_id, user_id FROM conversation_members
    UNION ALL
    SELECT id, direct_user_low_id FROM conversations WHERE kind = 'direct'
    UNION ALL
    SELECT id, direct_user_high_id FROM conversations WHERE kind = 'direct'
)
INSERT INTO conversation_read_states (conversation_id, user_id, last_read_sequence)
SELECT participants.conversation_id, participants.user_id, COALESCE(MAX(messages.sequence), 0)
FROM participants
LEFT JOIN conversation_messages AS messages ON messages.conversation_id = participants.conversation_id
GROUP BY participants.conversation_id, participants.user_id;
