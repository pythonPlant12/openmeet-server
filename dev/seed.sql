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
    ('00000000-0000-0000-0000-000000000007', 'test1@test.com', 'Test 1', 'test1', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000008', 'test2@test.com', 'Test 2', 'test2', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000009', 'test3@test.com', 'Test 3', 'test3', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000010', 'test4@test.com', 'Test 4', 'test4', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000011', 'test5@test.com', 'Test 5', 'test5', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000012', 'test6@test.com', 'Test 6', 'test6', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000013', 'test7@test.com', 'Test 7', 'test7', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000014', 'test8@test.com', 'Test 8', 'test8', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000015', 'test9@test.com', 'Test 9', 'test9', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000016', 'test10@test.com', 'Test 10', 'test10', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000017', 'test11@test.com', 'Test 11', 'test11', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000018', 'test12@test.com', 'Test 12', 'test12', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000019', 'test13@test.com', 'Test 13', 'test13', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000020', 'test14@test.com', 'Test 14', 'test14', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000021', 'test15@test.com', 'Test 15', 'test15', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000022', 'test16@test.com', 'Test 16', 'test16', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000023', 'test17@test.com', 'Test 17', 'test17', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000024', 'test18@test.com', 'Test 18', 'test18', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000025', 'test19@test.com', 'Test 19', 'test19', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user'),
    ('00000000-0000-0000-0000-000000000026', 'test20@test.com', 'Test 20', 'test20', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk', 'user')
ON CONFLICT (id) DO UPDATE SET
    email = EXCLUDED.email,
    name = EXCLUDED.name,
    nickname = EXCLUDED.nickname,
    password_hash = EXCLUDED.password_hash;

UPDATE users
SET avatar_key = 'dev/avatars/' || nickname || '.svg'
WHERE id BETWEEN '00000000-0000-0000-0000-000000000007'::uuid
    AND '00000000-0000-0000-0000-000000000016'::uuid;

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

-- test1 through test10 form a complete accepted-friend mesh (45 pairs).
WITH numbered_users AS (
    SELECT id
    FROM users
    WHERE id BETWEEN '00000000-0000-0000-0000-000000000007'::uuid
        AND '00000000-0000-0000-0000-000000000016'::uuid
)
INSERT INTO friendships (id, requester_id, addressee_id, status)
SELECT
    uuid_generate_v5('6ba7b810-9dad-11d1-80b4-00c04fd430c8', requester.id::text || ':' || addressee.id::text),
    requester.id,
    addressee.id,
    'accepted'
FROM numbered_users requester
CROSS JOIN numbered_users addressee
WHERE requester.id < addressee.id
ON CONFLICT DO NOTHING;

UPDATE friendships
SET status = 'accepted', updated_at = NOW()
WHERE requester_id BETWEEN '00000000-0000-0000-0000-000000000007'::uuid
        AND '00000000-0000-0000-0000-000000000016'::uuid
    AND addressee_id BETWEEN '00000000-0000-0000-0000-000000000007'::uuid
        AND '00000000-0000-0000-0000-000000000016'::uuid;

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

INSERT INTO conversations (id, kind, creator_id, direct_user_low_id, direct_user_high_id)
VALUES
    ('22000000-0000-0000-0000-000000000001', 'direct', '00000000-0000-0000-0000-000000000007', '00000000-0000-0000-0000-000000000007', '00000000-0000-0000-0000-000000000008'),
    ('22000000-0000-0000-0000-000000000002', 'direct', '00000000-0000-0000-0000-000000000007', '00000000-0000-0000-0000-000000000007', '00000000-0000-0000-0000-000000000012'),
    ('22000000-0000-0000-0000-000000000003', 'direct', '00000000-0000-0000-0000-000000000008', '00000000-0000-0000-0000-000000000008', '00000000-0000-0000-0000-000000000013'),
    ('22000000-0000-0000-0000-000000000004', 'direct', '00000000-0000-0000-0000-000000000009', '00000000-0000-0000-0000-000000000009', '00000000-0000-0000-0000-000000000014'),
    ('22000000-0000-0000-0000-000000000005', 'direct', '00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000015'),
    ('22000000-0000-0000-0000-000000000006', 'direct', '00000000-0000-0000-0000-000000000011', '00000000-0000-0000-0000-000000000011', '00000000-0000-0000-0000-000000000016'),
    ('22000000-0000-0000-0000-000000000007', 'direct', '00000000-0000-0000-0000-000000000012', '00000000-0000-0000-0000-000000000012', '00000000-0000-0000-0000-000000000016')
ON CONFLICT DO NOTHING;

INSERT INTO conversations (id, kind, creator_id, title, access_policy, password_hash)
VALUES
    ('23000000-0000-0000-0000-000000000001', 'group', '00000000-0000-0000-0000-000000000007', 'Numbered crew', 'open', NULL),
    ('23000000-0000-0000-0000-000000000002', 'group', '00000000-0000-0000-0000-000000000009', 'Design sync', 'friends_only', NULL),
    ('23000000-0000-0000-0000-000000000003', 'group', '00000000-0000-0000-0000-000000000011', 'Private test room', 'password', '$argon2id$v=19$m=19456,t=2,p=1$LyWo1S0LPwugujOGwk+CLA$rf5jmTmcRETEKncXib2wTvU72phjkmJZEt6vku5wXlk'),
    ('23000000-0000-0000-0000-000000000004', 'group', '00000000-0000-0000-0000-000000000016', 'Pair lab', 'open', NULL)
ON CONFLICT (id) DO NOTHING;

INSERT INTO conversation_members (conversation_id, user_id, role)
VALUES
    ('23000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000007', 'creator'),
    ('23000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000008', 'admin'),
    ('23000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000009', 'member'),
    ('23000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000010', 'member'),
    ('23000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000011', 'member'),
    ('23000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000009', 'creator'),
    ('23000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000007', 'member'),
    ('23000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000012', 'admin'),
    ('23000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000013', 'member'),
    ('23000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000015', 'member'),
    ('23000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000011', 'creator'),
    ('23000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000008', 'member'),
    ('23000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000014', 'member'),
    ('23000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-000000000016', 'member'),
    ('23000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-000000000016', 'creator'),
    ('23000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-000000000010', 'member')
ON CONFLICT (conversation_id, user_id) DO UPDATE SET role = EXCLUDED.role;

WITH fixture_messages (low_id, high_id, sender_id, sender_name, content, created_at) AS (
    VALUES
        ('00000000-0000-0000-0000-000000000007'::uuid, '00000000-0000-0000-0000-000000000008'::uuid, '00000000-0000-0000-0000-000000000007'::uuid, 'Test 1', 'Ready for the first local call?', NOW() - INTERVAL '45 minutes'),
        ('00000000-0000-0000-0000-000000000007'::uuid, '00000000-0000-0000-0000-000000000008'::uuid, '00000000-0000-0000-0000-000000000008'::uuid, 'Test 2', 'Yes, camera and microphone are ready.', NOW() - INTERVAL '42 minutes'),
        ('00000000-0000-0000-0000-000000000007'::uuid, '00000000-0000-0000-0000-000000000012'::uuid, '00000000-0000-0000-0000-000000000012'::uuid, 'Test 6', 'Testing a conversation with one unread message.', NOW() - INTERVAL '30 minutes'),
        ('00000000-0000-0000-0000-000000000008'::uuid, '00000000-0000-0000-0000-000000000013'::uuid, '00000000-0000-0000-0000-000000000008'::uuid, 'Test 2', 'Can you verify tablet layout?', NOW() - INTERVAL '25 minutes'),
        ('00000000-0000-0000-0000-000000000009'::uuid, '00000000-0000-0000-0000-000000000014'::uuid, '00000000-0000-0000-0000-000000000014'::uuid, 'Test 8', 'Profile menu looks good here.', NOW() - INTERVAL '20 minutes'),
        ('00000000-0000-0000-0000-000000000010'::uuid, '00000000-0000-0000-0000-000000000015'::uuid, '00000000-0000-0000-0000-000000000010'::uuid, 'Test 4', 'Checking message ordering.', NOW() - INTERVAL '15 minutes'),
        ('00000000-0000-0000-0000-000000000011'::uuid, '00000000-0000-0000-0000-000000000016'::uuid, '00000000-0000-0000-0000-000000000016'::uuid, 'Test 10', 'This chat exercises the tenth avatar.', NOW() - INTERVAL '10 minutes'),
        ('00000000-0000-0000-0000-000000000012'::uuid, '00000000-0000-0000-0000-000000000016'::uuid, '00000000-0000-0000-0000-000000000012'::uuid, 'Test 6', 'Last direct fixture message.', NOW() - INTERVAL '5 minutes')
)
INSERT INTO conversation_messages (conversation_id, sender_id, sender_name, content, created_at)
SELECT conversation.id, fixture.sender_id, fixture.sender_name, fixture.content, fixture.created_at
FROM fixture_messages fixture
JOIN conversations conversation
    ON conversation.kind = 'direct'
    AND conversation.direct_user_low_id = fixture.low_id
    AND conversation.direct_user_high_id = fixture.high_id
WHERE NOT EXISTS (
    SELECT 1
    FROM conversation_messages existing
    WHERE existing.conversation_id = conversation.id AND existing.content = fixture.content
);

WITH fixture_messages (conversation_id, sender_id, sender_name, content, created_at) AS (
    VALUES
        ('23000000-0000-0000-0000-000000000001'::uuid, '00000000-0000-0000-0000-000000000007'::uuid, 'Test 1', 'Welcome to the numbered crew.', NOW() - INTERVAL '1 hour'),
        ('23000000-0000-0000-0000-000000000001'::uuid, '00000000-0000-0000-0000-000000000009'::uuid, 'Test 3', 'Five participants are represented here.', NOW() - INTERVAL '55 minutes'),
        ('23000000-0000-0000-0000-000000000002'::uuid, '00000000-0000-0000-0000-000000000013'::uuid, 'Test 7', 'Reviewing responsive dashboard details.', NOW() - INTERVAL '35 minutes'),
        ('23000000-0000-0000-0000-000000000003'::uuid, '00000000-0000-0000-0000-000000000011'::uuid, 'Test 5', 'Password-protected fixture group.', NOW() - INTERVAL '18 minutes'),
        ('23000000-0000-0000-0000-000000000004'::uuid, '00000000-0000-0000-0000-000000000016'::uuid, 'Test 10', 'Small two-person group fixture.', NOW() - INTERVAL '8 minutes')
)
INSERT INTO conversation_messages (conversation_id, sender_id, sender_name, content, created_at)
SELECT fixture.conversation_id, fixture.sender_id, fixture.sender_name, fixture.content, fixture.created_at
FROM fixture_messages fixture
WHERE NOT EXISTS (
    SELECT 1
    FROM conversation_messages existing
    WHERE existing.conversation_id = fixture.conversation_id AND existing.content = fixture.content
);

UPDATE conversations conversation
SET updated_at = latest.created_at
FROM (
    SELECT conversation_id, MAX(created_at) AS created_at
    FROM conversation_messages
    WHERE conversation_id IN (
        SELECT id FROM conversations
        WHERE creator_id BETWEEN '00000000-0000-0000-0000-000000000007'::uuid
            AND '00000000-0000-0000-0000-000000000016'::uuid
    )
    GROUP BY conversation_id
) latest
WHERE conversation.id = latest.conversation_id;

DELETE FROM direct_message_requests
WHERE requester_id = '00000000-0000-0000-0000-000000000006'
  AND recipient_id = '00000000-0000-0000-0000-000000000001';
