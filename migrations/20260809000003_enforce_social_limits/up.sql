CREATE FUNCTION enforce_friend_request_limits() RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(LEAST(NEW.requester_id::text, NEW.addressee_id::text), 1));
    PERFORM pg_advisory_xact_lock(hashtextextended(GREATEST(NEW.requester_id::text, NEW.addressee_id::text), 1));

    IF (SELECT COUNT(*) FROM friendships WHERE requester_id = NEW.requester_id AND status = 'pending') >= 100
       OR (SELECT COUNT(*) FROM friendships WHERE addressee_id = NEW.addressee_id AND status = 'pending') >= 100 THEN
        RAISE EXCEPTION 'too many pending friend requests' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER friendships_pending_limit
    BEFORE INSERT ON friendships
    FOR EACH ROW
    EXECUTE FUNCTION enforce_friend_request_limits();

CREATE FUNCTION enforce_active_call_limit() RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.caller_id::text, 2));

    IF (SELECT COUNT(*) FROM call_invitations WHERE caller_id = NEW.caller_id AND status = 'pending' AND expires_at > NOW()) >= 10 THEN
        RAISE EXCEPTION 'too many active calls' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER call_invitations_active_limit
    BEFORE INSERT ON call_invitations
    FOR EACH ROW
    EXECUTE FUNCTION enforce_active_call_limit();
