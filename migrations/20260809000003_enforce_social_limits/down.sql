DROP TRIGGER IF EXISTS call_invitations_active_limit ON call_invitations;
DROP FUNCTION IF EXISTS enforce_active_call_limit();
DROP TRIGGER IF EXISTS friendships_pending_limit ON friendships;
DROP FUNCTION IF EXISTS enforce_friend_request_limits();
