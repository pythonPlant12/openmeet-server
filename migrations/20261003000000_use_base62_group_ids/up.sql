ALTER TABLE conversations DROP CONSTRAINT conversations_valid_group_metadata;

ALTER TABLE conversations ADD COLUMN legacy_group_code VARCHAR(80);

CREATE FUNCTION uuid_to_base62_22(value UUID)
RETURNS VARCHAR(22)
LANGUAGE plpgsql
IMMUTABLE
STRICT
PARALLEL SAFE
AS $$
DECLARE
    alphabet CONSTANT TEXT := '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz';
    bytes BYTEA := uuid_send(value);
    number NUMERIC := 0;
    output TEXT := '';
    index INTEGER;
    digit INTEGER;
BEGIN
    FOR index IN 0..15 LOOP
        number := number * 256 + get_byte(bytes, index);
    END LOOP;

    FOR index IN 1..22 LOOP
        digit := mod(number, 62)::INTEGER;
        output := substr(alphabet, digit + 1, 1) || output;
        number := trunc(number / 62);
    END LOOP;

    RETURN output;
END;
$$;

UPDATE conversations
SET legacy_group_code = group_code
WHERE kind = 'group';

UPDATE conversations
SET group_code = NULL
WHERE kind = 'group';

UPDATE conversations
SET group_code = uuid_to_base62_22(id)
WHERE kind = 'group';

CREATE UNIQUE INDEX conversations_legacy_group_code_idx
    ON conversations (legacy_group_code)
    WHERE legacy_group_code IS NOT NULL;

ALTER TABLE conversations
    ALTER COLUMN group_code TYPE VARCHAR(22) COLLATE "C",
    ADD CONSTRAINT conversations_valid_group_metadata CHECK ((
        (kind = 'group'
            AND group_code IS NOT NULL
            AND group_code ~ '^[0-9A-Za-z]{22}$'
            AND group_code = uuid_to_base62_22(id)
            AND (legacy_group_code IS NULL
                OR legacy_group_code ~ '^[a-z0-9]+(-[a-z0-9]+)*$')
            AND (avatar_key IS NULL
                OR avatar_key LIKE 'group-avatars/' || id::text || '/%'))
        OR
        (kind = 'direct'
            AND group_code IS NULL
            AND legacy_group_code IS NULL
            AND avatar_key IS NULL)
    ) IS TRUE);
