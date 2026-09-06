DROP INDEX call_invitations_one_pending_pair_idx;

WITH ranked_calls AS (
    SELECT
        id,
        ROW_NUMBER() OVER (
            PARTITION BY LEAST(caller_id, callee_id), GREATEST(caller_id, callee_id)
            ORDER BY created_at, id
        ) AS pair_rank
    FROM call_invitations
    WHERE status = 'pending'
)
UPDATE call_invitations
SET status = 'expired', responded_at = NOW()
FROM ranked_calls
WHERE call_invitations.id = ranked_calls.id
  AND ranked_calls.pair_rank > 1;

CREATE UNIQUE INDEX call_invitations_one_pending_pair_idx
    ON call_invitations(LEAST(caller_id, callee_id), GREATEST(caller_id, callee_id))
    WHERE status = 'pending';
