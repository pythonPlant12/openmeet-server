//! Account deletion. Direct conversations and groups with no other member go with the account. Groups it owns pass
//! to the next member, as when an owner leaves. Its messages and calls in the remaining groups move to the
//! placeholder account, so other members keep their history.

use std::collections::HashSet;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{
    AsyncConnection, AsyncPgConnection, RunQueryDsl, scoped_futures::ScopedFutureExt,
};
use uuid::Uuid;

use crate::{
    AppState,
    auth::DELETED_USER_ID,
    schema::{
        call_session_members, call_sessions, conversation_members,
        conversation_message_attachments, conversation_messages, conversations, friendships,
        revoked_sfu_rooms, users,
    },
    social::{
        SocialResource,
        avatars::delete_avatar,
        conversations::{
            ConversationError, disconnect_user_from_sfu_rooms, group_successor, internal_error,
            revoke_and_close_sfu_rooms,
        },
        models::NewRevokedSfuRoom,
    },
};

const DELETED_USER_NAME: &str = "Deleted user";

/// What is left to clean up outside the database once the account is gone.
#[derive(Default)]
struct DeletedAccount {
    avatar_keys: Vec<String>,
    attachment_keys: Vec<String>,
    /// Calls of deleted conversations: every peer leaves.
    revoked_room_ids: Vec<String>,
    /// Calls of remaining groups: only the deleted user leaves.
    left_room_ids: Vec<String>,
    conversation_recipients: HashSet<Uuid>,
    call_recipients: HashSet<Uuid>,
    friend_ids: HashSet<Uuid>,
}

/// Deletes the account and everything that belongs only to it. Fails with 404 for an unknown account or the
/// placeholder account.
pub async fn delete_account(state: &AppState, user_id: Uuid) -> Result<(), (StatusCode, String)> {
    if user_id == DELETED_USER_ID {
        return Err(user_not_found());
    }
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let deleted = conn
        .transaction(|conn| delete_account_rows(conn, user_id).scope_boxed())
        .await
        .map_err(ConversationError::into_api_error)?;

    revoke_and_close_sfu_rooms(state, &deleted.revoked_room_ids).await;
    disconnect_user_from_sfu_rooms(state, &deleted.left_room_ids, user_id).await;
    state.social_events.publish(
        deleted.conversation_recipients,
        SocialResource::Conversations,
    );
    state
        .social_events
        .publish(deleted.call_recipients, SocialResource::Calls);
    state
        .social_events
        .publish(deleted.friend_ids, SocialResource::Friends);
    for avatar_key in deleted.avatar_keys {
        if let Err(error) = delete_avatar(state.avatar_storage.as_ref(), &avatar_key).await {
            tracing::warn!(%error, %user_id, %avatar_key, "Failed to remove deleted account avatar");
        }
    }
    for attachment_key in deleted.attachment_keys {
        if let Err(error) = state.avatar_storage.delete(&attachment_key).await {
            tracing::warn!(%error, %user_id, %attachment_key, "Failed to remove deleted account attachment");
        }
    }
    Ok(())
}

async fn delete_account_rows(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<DeletedAccount, ConversationError> {
    let avatar_key: Option<String> = users::table
        .find(user_id)
        .select(users::avatar_key)
        .for_update()
        .first(conn)
        .await
        .optional()?
        .ok_or_else(user_not_found)?;
    let mut deleted = DeletedAccount {
        avatar_keys: avatar_key.into_iter().collect(),
        ..DeletedAccount::default()
    };

    deleted.friend_ids = friendships::table
        .filter(
            friendships::requester_id
                .eq(user_id)
                .or(friendships::addressee_id.eq(user_id)),
        )
        .select((friendships::requester_id, friendships::addressee_id))
        .load::<(Uuid, Uuid)>(conn)
        .await?
        .into_iter()
        .map(|(requester, addressee)| {
            if requester == user_id {
                addressee
            } else {
                requester
            }
        })
        .collect();

    let mut doomed: Vec<Uuid> = conversations::table
        .filter(conversations::kind.eq("direct"))
        .filter(
            conversations::direct_user_low_id
                .eq(user_id)
                .or(conversations::direct_user_high_id.eq(user_id)),
        )
        .select(conversations::id)
        .load(conn)
        .await?;
    let groups: Vec<(Uuid, Uuid)> = conversations::table
        .inner_join(conversation_members::table)
        .filter(conversations::kind.eq("group"))
        .filter(conversation_members::user_id.eq(user_id))
        .select((conversations::id, conversations::creator_id))
        .for_update()
        .load(conn)
        .await?;
    let mut kept = Vec::new();
    for (group_id, creator_id) in groups {
        let members: Vec<(Uuid, String, DateTime<Utc>)> = conversation_members::table
            .filter(conversation_members::conversation_id.eq(group_id))
            .select((
                conversation_members::user_id,
                conversation_members::role,
                conversation_members::joined_at,
            ))
            .load(conn)
            .await?;
        let Some(successor) = group_successor(&members, user_id) else {
            doomed.push(group_id);
            continue;
        };
        if creator_id == user_id {
            diesel::update(conversations::table.find(group_id))
                .set(conversations::creator_id.eq(successor))
                .execute(conn)
                .await?;
            diesel::update(
                conversation_members::table
                    .filter(conversation_members::conversation_id.eq(group_id))
                    .filter(conversation_members::user_id.eq(successor)),
            )
            .set(conversation_members::role.eq("creator"))
            .execute(conn)
            .await?;
        }
        deleted.conversation_recipients.extend(
            members
                .iter()
                .map(|(id, _, _)| *id)
                .filter(|id| *id != user_id),
        );
        kept.push(group_id);
    }

    delete_conversations(conn, &doomed, &mut deleted).await?;

    // Expired calls can still hold SFU peers until every participant leaves.
    let (left_call_ids, left_room_ids): (Vec<Uuid>, Vec<String>) = call_sessions::table
        .inner_join(call_session_members::table)
        .filter(call_sessions::conversation_id.eq_any(&kept))
        .filter(call_sessions::status.eq_any(["active", "expired"]))
        .filter(call_session_members::user_id.eq(user_id))
        .select((call_sessions::id, call_sessions::sfu_room_id))
        .load::<(Uuid, String)>(conn)
        .await?
        .into_iter()
        .unzip();
    deleted.left_room_ids = left_room_ids;
    deleted.call_recipients.extend(
        call_session_members::table
            .filter(call_session_members::call_session_id.eq_any(&left_call_ids))
            .select(call_session_members::user_id)
            .load::<Uuid>(conn)
            .await?,
    );

    diesel::update(
        conversation_messages::table.filter(conversation_messages::sender_id.eq(user_id)),
    )
    .set((
        conversation_messages::sender_id.eq(DELETED_USER_ID),
        conversation_messages::sender_name.eq(DELETED_USER_NAME),
    ))
    .execute(conn)
    .await?;
    diesel::update(call_sessions::table.filter(call_sessions::initiator_id.eq(user_id)))
        .set(call_sessions::initiator_id.eq(DELETED_USER_ID))
        .execute(conn)
        .await?;
    // A group can still name the user as creator after they left it; the group itself stays.
    diesel::update(conversations::table.filter(conversations::creator_id.eq(user_id)))
        .set(conversations::creator_id.eq(DELETED_USER_ID))
        .execute(conn)
        .await?;
    diesel::delete(users::table.find(user_id))
        .execute(conn)
        .await?;

    deleted.conversation_recipients.remove(&user_id);
    deleted.call_recipients.remove(&user_id);
    Ok(deleted)
}

/// Deletes conversations that end with the account, and records their calls and stored files.
async fn delete_conversations(
    conn: &mut AsyncPgConnection,
    conversation_ids: &[Uuid],
    deleted: &mut DeletedAccount,
) -> Result<(), ConversationError> {
    if conversation_ids.is_empty() {
        return Ok(());
    }
    deleted.conversation_recipients.extend(
        conversation_members::table
            .filter(conversation_members::conversation_id.eq_any(conversation_ids))
            .select(conversation_members::user_id)
            .load::<Uuid>(conn)
            .await?,
    );
    deleted.call_recipients.extend(
        call_session_members::table
            .inner_join(call_sessions::table)
            .filter(call_sessions::conversation_id.eq_any(conversation_ids))
            .select(call_session_members::user_id)
            .load::<Uuid>(conn)
            .await?,
    );
    deleted.revoked_room_ids = call_sessions::table
        .filter(call_sessions::conversation_id.eq_any(conversation_ids))
        .select(call_sessions::sfu_room_id)
        .load(conn)
        .await?;
    deleted.attachment_keys = conversation_message_attachments::table
        .inner_join(conversation_messages::table.on(
            conversation_messages::sequence.eq(conversation_message_attachments::message_sequence),
        ))
        .filter(conversation_messages::conversation_id.eq_any(conversation_ids))
        .select(conversation_message_attachments::storage_key)
        .load(conn)
        .await?;
    deleted.avatar_keys.extend(
        conversations::table
            .filter(conversations::id.eq_any(conversation_ids))
            .select(conversations::avatar_key)
            .load::<Option<String>>(conn)
            .await?
            .into_iter()
            .flatten(),
    );

    let tombstones = deleted
        .revoked_room_ids
        .iter()
        .map(|sfu_room_id| NewRevokedSfuRoom { sfu_room_id })
        .collect::<Vec<_>>();
    if !tombstones.is_empty() {
        diesel::insert_into(revoked_sfu_rooms::table)
            .values(&tombstones)
            .on_conflict_do_nothing()
            .execute(conn)
            .await?;
    }
    diesel::delete(conversations::table.filter(conversations::id.eq_any(conversation_ids)))
        .execute(conn)
        .await?;
    Ok(())
}

fn user_not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "User not found".to_string())
}
