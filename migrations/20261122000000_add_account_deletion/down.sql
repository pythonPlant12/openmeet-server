-- This fails once an account was deleted, because its messages and calls now belong to the placeholder.
DELETE FROM users WHERE id = '00000000-0000-0000-0000-000000000000';

DELETE FROM permissions WHERE key = 'admin.users.delete';
