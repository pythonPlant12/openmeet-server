DROP TABLE conversation_message_attachments;

-- Attachment-only messages are not representable under the prior content constraint.
DELETE FROM conversation_messages WHERE content = '';

ALTER TABLE conversation_messages
    DROP CONSTRAINT conversation_messages_content_length;

ALTER TABLE conversation_messages
    ADD CONSTRAINT conversation_messages_content_length
        CHECK (char_length(content) BETWEEN 1 AND 2000);
