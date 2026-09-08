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
    call_session_members (call_session_id, user_id) {
        call_session_id -> Uuid,
        user_id -> Uuid,
        #[max_length = 10]
        status -> Varchar,
        responded_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    call_sessions (id) {
        id -> Uuid,
        conversation_id -> Uuid,
        initiator_id -> Uuid,
        #[max_length = 128]
        sfu_room_id -> Varchar,
        #[max_length = 10]
        status -> Varchar,
        expires_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    conversation_members (conversation_id, user_id) {
        conversation_id -> Uuid,
        user_id -> Uuid,
        #[max_length = 10]
        role -> Varchar,
        joined_at -> Timestamptz,
    }
}

diesel::table! {
    conversation_messages (sequence) {
        sequence -> Int8,
        conversation_id -> Uuid,
        sender_id -> Uuid,
        #[max_length = 255]
        sender_name -> Varchar,
        content -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    conversation_hidden_states (conversation_id, user_id) {
        conversation_id -> Uuid,
        user_id -> Uuid,
        hidden_at -> Timestamptz,
    }
}

diesel::table! {
    conversation_read_states (conversation_id, user_id) {
        conversation_id -> Uuid,
        user_id -> Uuid,
        last_read_sequence -> Int8,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    conversations (id) {
        id -> Uuid,
        #[max_length = 10]
        kind -> Varchar,
        creator_id -> Uuid,
        #[max_length = 128]
        title -> Nullable<Varchar>,
        #[max_length = 20]
        access_policy -> Nullable<Varchar>,
        #[max_length = 255]
        password_hash -> Nullable<Varchar>,
        direct_user_low_id -> Nullable<Uuid>,
        direct_user_high_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    direct_message_requests (id) {
        id -> Uuid,
        requester_id -> Uuid,
        recipient_id -> Uuid,
        #[max_length = 10]
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
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
    notifications (id) {
        id -> Uuid,
        recipient_id -> Uuid,
        actor_id -> Uuid,
        #[max_length = 64]
        kind -> Varchar,
        data -> Jsonb,
        created_at -> Timestamptz,
        read_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    users (id) {
        id -> Uuid,
        #[max_length = 255]
        email -> Varchar,
        #[max_length = 255]
        name -> Varchar,
        #[max_length = 50]
        nickname -> Varchar,
        #[max_length = 255]
        avatar_key -> Nullable<Varchar>,
        #[max_length = 255]
        password_hash -> Varchar,
        #[max_length = 50]
        role -> Varchar,
        #[max_length = 20]
        status -> Varchar,
        #[max_length = 255]
        status_message -> Varchar,
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
diesel::joinable!(conversation_members -> conversations (conversation_id));
diesel::joinable!(conversation_messages -> conversations (conversation_id));
diesel::joinable!(conversation_messages -> users (sender_id));
diesel::joinable!(conversation_hidden_states -> conversations (conversation_id));
diesel::joinable!(conversation_hidden_states -> users (user_id));
diesel::joinable!(conversation_read_states -> conversations (conversation_id));
diesel::joinable!(conversation_read_states -> users (user_id));
diesel::joinable!(call_session_members -> call_sessions (call_session_id));
diesel::joinable!(call_sessions -> conversations (conversation_id));

diesel::allow_tables_to_appear_in_same_query!(
    call_invitations,
    call_session_members,
    call_sessions,
    conversation_members,
    conversation_messages,
    conversation_hidden_states,
    conversation_read_states,
    conversations,
    direct_message_requests,
    friendships,
    meeting_history,
    notifications,
    refresh_tokens,
    user_presence,
    users,
);
