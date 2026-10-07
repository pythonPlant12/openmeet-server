-- When each user last looked at their calls. Calls they missed after this time count toward the
-- Calls badge.
CREATE TABLE missed_call_reads (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
