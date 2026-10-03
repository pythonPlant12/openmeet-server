CREATE TABLE revoked_sfu_rooms (
    sfu_room_id VARCHAR(128) PRIMARY KEY,
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
