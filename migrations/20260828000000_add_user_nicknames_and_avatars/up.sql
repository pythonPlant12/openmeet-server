ALTER TABLE users ADD COLUMN nickname VARCHAR(50);
ALTER TABLE users ADD COLUMN avatar_key VARCHAR(255);

UPDATE users
SET nickname = 'user_' || substring(replace(id::text, '-', '') FROM 1 FOR 12)
WHERE nickname IS NULL;

ALTER TABLE users ALTER COLUMN nickname SET NOT NULL;
ALTER TABLE users ADD CONSTRAINT users_nickname_format CHECK (nickname ~ '^[a-z0-9_]{3,50}$');
CREATE UNIQUE INDEX users_nickname_unique_idx ON users (nickname);
