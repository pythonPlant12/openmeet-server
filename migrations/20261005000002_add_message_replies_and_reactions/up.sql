ALTER TABLE conversation_messages
    ADD COLUMN reply_to_sequence BIGINT REFERENCES conversation_messages(sequence) ON DELETE SET NULL;

CREATE TABLE message_reactions (
    message_sequence BIGINT NOT NULL REFERENCES conversation_messages(sequence) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    emoji VARCHAR(32) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (message_sequence, user_id, emoji),
    CONSTRAINT message_reactions_valid_emoji CHECK (octet_length(emoji) BETWEEN 1 AND 32)
);

CREATE INDEX message_reactions_message_sequence_idx ON message_reactions (message_sequence, created_at);
