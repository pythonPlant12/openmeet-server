ALTER TABLE conversation_messages
    DROP CONSTRAINT conversation_messages_content_length;

ALTER TABLE conversation_messages
    ADD CONSTRAINT conversation_messages_content_length
        CHECK (char_length(content) BETWEEN 0 AND 2000);

CREATE TABLE conversation_message_attachments (
    id UUID PRIMARY KEY,
    message_sequence BIGINT NOT NULL REFERENCES conversation_messages(sequence) ON DELETE CASCADE,
    storage_key TEXT NOT NULL UNIQUE,
    file_name VARCHAR(255) NOT NULL,
    content_type VARCHAR(255) NOT NULL,
    byte_size BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT conversation_message_attachments_byte_size
        CHECK (byte_size BETWEEN 1 AND 26214400)
);

CREATE INDEX conversation_message_attachments_message_sequence_idx
    ON conversation_message_attachments (message_sequence, created_at);
