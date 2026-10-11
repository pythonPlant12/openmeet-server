-- Roles group permissions. users.role names one role; the admin panel edits both tables.
CREATE TABLE roles (
    name VARCHAR(50) PRIMARY KEY,
    description VARCHAR(255) NOT NULL DEFAULT '',
    -- System roles cannot be deleted. The admin role also keeps every permission.
    is_system BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT roles_name_format CHECK (name ~ '^[a-z][a-z0-9_-]{1,49}$')
);

-- The permission catalog is fixed by the server code; the admin panel only assigns permissions to roles.
CREATE TABLE permissions (
    key VARCHAR(64) PRIMARY KEY,
    description VARCHAR(255) NOT NULL
);

CREATE TABLE role_permissions (
    role_name VARCHAR(50) NOT NULL REFERENCES roles(name) ON UPDATE CASCADE ON DELETE CASCADE,
    permission_key VARCHAR(64) NOT NULL REFERENCES permissions(key) ON DELETE CASCADE,
    PRIMARY KEY (role_name, permission_key)
);

INSERT INTO permissions (key, description) VALUES
    ('app.login', 'Sign in and keep a session in OpenMeet'),
    ('meetings.create', 'Create meeting rooms'),
    ('messages.send', 'Send messages in conversations'),
    ('admin.access', 'Open the admin panel and view rooms, users, and roles'),
    ('admin.users.manage', 'Change the role of other users'),
    ('admin.roles.manage', 'Create, edit, and delete roles');

INSERT INTO roles (name, description, is_system) VALUES
    ('admin', 'Full access, including the admin panel', TRUE),
    ('user', 'Default role for registered users', TRUE),
    ('suspended', 'Cannot sign in', TRUE);

INSERT INTO role_permissions (role_name, permission_key)
SELECT 'admin', key FROM permissions;

INSERT INTO role_permissions (role_name, permission_key) VALUES
    ('user', 'app.login'),
    ('user', 'meetings.create'),
    ('user', 'messages.send');

UPDATE users SET role = 'user' WHERE role NOT IN (SELECT name FROM roles);

ALTER TABLE users
    ADD CONSTRAINT users_role_fkey
        FOREIGN KEY (role) REFERENCES roles(name) ON UPDATE CASCADE ON DELETE RESTRICT;

CREATE INDEX idx_users_role ON users(role);
