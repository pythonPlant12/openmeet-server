ALTER TABLE conversations
    DROP CONSTRAINT conversations_valid_group_metadata;

DROP INDEX conversations_avatar_key_idx;
DROP INDEX conversations_group_code_idx;

ALTER TABLE conversations
    DROP COLUMN avatar_key,
    DROP COLUMN group_code;
