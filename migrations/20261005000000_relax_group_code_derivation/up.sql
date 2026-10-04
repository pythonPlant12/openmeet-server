-- uuid_to_base62_22 used rounding NUMERIC division, so its codes never matched the server's Base62
-- encoding and the equality check rejected every new group. Existing codes are already shared, so
-- they stay as stored: the server generates codes, and format plus uniqueness are still enforced.
ALTER TABLE conversations DROP CONSTRAINT conversations_valid_group_metadata;
ALTER TABLE conversations
    ADD CONSTRAINT conversations_valid_group_metadata CHECK ((
        (kind = 'group'
            AND group_code IS NOT NULL
            AND group_code ~ '^[0-9A-Za-z]{22}$'
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

-- Integer division makes the function agree with the server encoding for any future use.
CREATE OR REPLACE FUNCTION uuid_to_base62_22(value UUID)
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
        number := div(number, 62);
    END LOOP;

    RETURN output;
END;
$$;
