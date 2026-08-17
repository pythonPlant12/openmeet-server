CREATE TABLE conversation_messages (
    sequence BIGSERIAL PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    sender_id UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    sender_name VARCHAR(255) NOT NULL,
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT conversation_messages_content_length
        CHECK (char_length(content) BETWEEN 1 AND 2000)
);

CREATE INDEX conversation_messages_conversation_sequence_idx
    ON conversation_messages (conversation_id, sequence DESC);
