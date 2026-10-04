ALTER TABLE conversations DROP CONSTRAINT conversations_valid_group_metadata;

ALTER TABLE conversations ALTER COLUMN group_code TYPE VARCHAR(80) COLLATE "default";

UPDATE conversations
SET group_code = COALESCE(legacy_group_code, 'legacy-group-' || REPLACE(id::TEXT, '-', ''))
WHERE kind = 'group';

DROP INDEX conversations_legacy_group_code_idx;

ALTER TABLE conversations DROP COLUMN legacy_group_code;

DROP FUNCTION uuid_to_base62_22(UUID);

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
