ALTER TABLE conversations
    ADD COLUMN group_code VARCHAR(80),
    ADD COLUMN avatar_key VARCHAR(512);

UPDATE conversations
SET group_code = 'legacy-group-' || REPLACE(id::text, '-', '')
WHERE kind = 'group';

CREATE UNIQUE INDEX conversations_group_code_idx
    ON conversations (group_code)
    WHERE group_code IS NOT NULL;

CREATE UNIQUE INDEX conversations_avatar_key_idx
    ON conversations (avatar_key)
    WHERE avatar_key IS NOT NULL;

ALTER TABLE conversations
    ADD CONSTRAINT conversations_valid_group_metadata CHECK ((
        (kind = 'group'
            AND group_code IS NOT NULL
            AND group_code ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
            AND (avatar_key IS NULL
                OR avatar_key LIKE 'group-avatars/' || id::text || '/%'))
        OR
        (kind = 'direct'
            AND group_code IS NULL
            AND avatar_key IS NULL)
    ) IS TRUE);
