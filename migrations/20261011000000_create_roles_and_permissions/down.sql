DROP INDEX idx_users_role;

ALTER TABLE users DROP CONSTRAINT users_role_fkey;

-- Custom roles do not exist before this migration.
UPDATE users SET role = 'user' WHERE role NOT IN ('admin', 'user');

DROP TABLE role_permissions;
DROP TABLE permissions;
DROP TABLE roles;
