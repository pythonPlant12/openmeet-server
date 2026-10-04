DROP TABLE group_invitations;

UPDATE conversations
SET access_policy = 'friends_only'
WHERE access_policy = 'friends_of_friends';

ALTER TABLE conversations DROP CONSTRAINT conversations_valid_group;
ALTER TABLE conversations ADD CONSTRAINT conversations_valid_group CHECK ((
    (kind = 'group'
        AND title IS NOT NULL
        AND access_policy IN ('open', 'password', 'friends_only')
        AND direct_user_low_id IS NULL
        AND direct_user_high_id IS NULL
        AND ((access_policy = 'password' AND password_hash IS NOT NULL)
            OR (access_policy <> 'password' AND password_hash IS NULL)))
    OR
    (kind = 'direct'
        AND title IS NULL
        AND access_policy IS NULL
        AND password_hash IS NULL
        AND direct_user_low_id IS NOT NULL
        AND direct_user_high_id IS NOT NULL
        AND direct_user_low_id < direct_user_high_id)
) IS TRUE);
