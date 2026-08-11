// @generated automatically by Diesel CLI.

diesel::table! {
    call_invitations (id) {
        id -> Uuid,
        caller_id -> Uuid,
        callee_id -> Uuid,
        #[max_length = 128]
        room_id -> Varchar,
        #[max_length = 20]
        status -> Varchar,
        expires_at -> Timestamptz,
        created_at -> Timestamptz,
        responded_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    friendships (id) {
        id -> Uuid,
        requester_id -> Uuid,
        addressee_id -> Uuid,
        #[max_length = 20]
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    meeting_history (id) {
        id -> Uuid,
        user_id -> Uuid,
        #[max_length = 128]
        room_id -> Varchar,
        last_joined_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    users (id) {
        id -> Uuid,
        #[max_length = 255]
        email -> Varchar,
        #[max_length = 255]
        name -> Varchar,
        #[max_length = 255]
        password_hash -> Varchar,
        #[max_length = 50]
        role -> Varchar,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    refresh_tokens (id) {
        id -> Uuid,
        user_id -> Uuid,
        #[max_length = 255]
        token_hash -> Varchar,
        expires_at -> Timestamp,
        created_at -> Timestamp,
    }
}

diesel::table! {
    user_presence (user_id) {
        user_id -> Uuid,
        last_seen_at -> Timestamptz,
    }
}

// Foreign key relationship: refresh_tokens.user_id -> users.id
diesel::joinable!(refresh_tokens -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(
    call_invitations,
    friendships,
    meeting_history,
    refresh_tokens,
    user_presence,
    users,
);
