-- Only the admin role may delete accounts. Other roles can get the permission from an admin who holds it.
INSERT INTO permissions (key, description) VALUES
    ('admin.users.delete', 'Delete user accounts');

INSERT INTO role_permissions (role_name, permission_key) VALUES
    ('admin', 'admin.users.delete');

-- A deleted user's group messages and calls move to this placeholder account, so other members keep their
-- history. It holds the suspended role, and its password hash matches no password, so it cannot sign in.
INSERT INTO users (id, email, name, nickname, password_hash, role, status, status_message)
VALUES (
    '00000000-0000-0000-0000-000000000000',
    'deleted-user@openmeet.invalid',
    'Deleted user',
    CASE
        WHEN EXISTS (SELECT 1 FROM users WHERE nickname = 'deleted_user')
            THEN 'deleted_user_' || substring(md5(random()::text) FROM 1 FOR 8)
        ELSE 'deleted_user'
    END,
    '$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    'suspended',
    'offline',
    ''
);
