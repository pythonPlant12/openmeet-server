DROP INDEX call_invitations_one_pending_pair_idx;
CREATE UNIQUE INDEX call_invitations_one_pending_pair_idx
    ON call_invitations(caller_id, callee_id)
    WHERE status = 'pending';
