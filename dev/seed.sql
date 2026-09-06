-- Development-only fixtures. Safe to rerun: fixed IDs and ON CONFLICT preserve existing data.
-- Every seeded account uses password: testtest

-- Replace older fixture rows created before deterministic seed IDs were introduced.
DELETE FROM conversations
WHERE creator_id IN (
    SELECT id
    FROM users
    WHERE email IN ('test@test.com', 'alice@test.com', 'bob@test.com', 'carol@test.com', 'dave@test.com', 'eve@test.com', 'test1@test.com')
      AND (email, id) NOT IN (
          ('test@test.com', '00000000-0000-0000-0000-000000000001'),
          ('alice@test.com', '00000000-0000-0000-0000-000000000002'),
          ('bob@test.com', '00000000-0000-0000-0000-000000000003'),
          ('carol@test.com', '00000000-0000-0000-0000-000000000004'),
          ('dave@test.com', '00000000-0000-0000-0000-000000000005'),
          ('eve@test.com', '00000000-0000-0000-0000-000000000006'),
          ('test1@test.com', '00000000-0000-0000-0000-000000000007')
      )
);

DELETE FROM users
WHERE email IN ('test@test.com', 'alice@test.com', 'bob@test.com', 'carol@test.com', 'dave@test.com', 'eve@test.com', 'test1@test.com')
  AND (email, id) NOT IN (
    ('test@test.com', '00000000-0000-0000-0000-000000000001'),
    ('alice@test.com', '00000000-0000-0000-0000-000000000002'),
    ('bob@test.com', '00000000-0000-0000-0000-000000000003'),
    ('carol@test.com', '00000000-0000-0000-0000-000000000004'),
    ('dave@test.com', '00000000-0000-0000-0000-000000000005'),
    ('eve@test.com', '00000000-0000-0000-0000-000000000006'),
    ('test1@test.com', '00000000-0000-0000-0000-000000000007')
);

INSERT INTO users (id, email, name, nickname, password_hash, role)
VALUES
    ('00000000-0000-0000-0000-000000000001', 'test@test.com', 'Test User', 'test', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000002', 'alice@test.com', 'Alice Martin', 'alice', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000003', 'bob@test.com', 'Bob Chen', 'bob', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000004', 'carol@test.com', 'Carol Diaz', 'carol', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000005', 'dave@test.com', 'Dave Wilson', 'dave', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000006', 'eve@test.com', 'Eve Park', 'eve', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000007', 'test1@test.com', 'Test One', 'test1', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user')
ON CONFLICT (id) DO UPDATE SET
    email = EXCLUDED.email,
    name = EXCLUDED.name,
    nickname = EXCLUDED.nickname,
    password_hash = EXCLUDED.password_hash;

INSERT INTO friendships (id, requester_id, addressee_id, status)
VALUES
    ('10000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002', 'accepted'),
    ('10000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000003', 'accepted'),
    ('10000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000004', 'accepted'),
    ('10000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000005', 'accepted'),
    ('10000000-0000-0000-0000-000000000005', '00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000003', 'accepted'),
    ('10000000-0000-0000-0000-000000000006', '00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000004', 'accepted'),
    ('10000000-0000-0000-0000-000000000007', '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000006', 'accepted'),
    ('10000000-0000-0000-0000-000000000008', '00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000005', 'accepted'),
    ('10000000-0000-0000-0000-000000000009', '00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000006', 'accepted'),
    ('10000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000004', 'accepted'),
    ('10000000-0000-0000-0000-000000000011', '00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000005', 'accepted'),
    ('10000000-0000-0000-0000-000000000012', '00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000006', 'accepted'),
    ('10000000-0000-0000-0000-000000000013', '00000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-000000000005', 'accepted'),
    ('10000000-0000-0000-0000-000000000014', '00000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-000000000006', 'accepted'),
    ('10000000-0000-0000-0000-000000000015', '00000000-0000-0000-0000-000000000005', '00000000-0000-0000-0000-000000000006', 'accepted')
ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, updated_at = NOW();

INSERT INTO conversations (id, kind, creator_id, title, access_policy)
VALUES
    ('20000000-0000-0000-0000-000000000001', 'group', '00000000-0000-0000-0000-000000000001', 'OpenMeet testers', 'open'),
    ('20000000-0000-0000-0000-000000000002', 'group', '00000000-0000-0000-0000-000000000001', 'Friends-only lab', 'friends_only')
ON CONFLICT (id) DO NOTHING;

INSERT INTO conversation_members (conversation_id, user_id, role)
VALUES
    ('20000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', 'creator'),
    ('20000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002', 'admin'),
    ('20000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000003', 'member'),
    ('20000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001', 'creator'),
    ('20000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000002', 'member'),
    ('20000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000003', 'member')
ON CONFLICT (conversation_id, user_id) DO UPDATE SET role = EXCLUDED.role;

DELETE FROM direct_message_requests
WHERE requester_id = '00000000-0000-0000-0000-000000000006'
  AND recipient_id = '00000000-0000-0000-0000-000000000001';
