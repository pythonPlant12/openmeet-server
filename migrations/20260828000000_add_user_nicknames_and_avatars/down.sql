DROP INDEX users_nickname_unique_idx;
ALTER TABLE users DROP CONSTRAINT users_nickname_format;
ALTER TABLE users DROP COLUMN avatar_key;
ALTER TABLE users DROP COLUMN nickname;
