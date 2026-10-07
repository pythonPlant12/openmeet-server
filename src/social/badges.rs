//! Badge counts for the workspace sidebar: conversations with unread messages or pending requests,
//! incoming friend requests, and calls the user missed since last looking at their calls.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::Utc;
use diesel::{
    prelude::*,
    sql_types::{BigInt, Uuid as SqlUuid},
};
use diesel_async::RunQueryDsl;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    schema::missed_call_reads,
    social::{SocialResource, call_sessions::CALL_ROOM_ENDED_SQL},
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

#[derive(Debug, Serialize, QueryableByName, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BadgeCounts {
    #[diesel(sql_type = BigInt)]
    messages: i64,
    #[diesel(sql_type = BigInt)]
    friends: i64,
    #[diesel(sql_type = BigInt)]
    calls: i64,
}

pub fn badge_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(get_badges))
        .route("/calls/seen", post(mark_calls_seen))
}

async fn get_badges(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<BadgeCounts> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    Ok(Json(load_badges(&mut conn, user_id).await?))
}

async fn mark_calls_seen(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<BadgeCounts> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let now = Utc::now();
    diesel::insert_into(missed_call_reads::table)
        .values((
            missed_call_reads::user_id.eq(user_id),
            missed_call_reads::seen_at.eq(now),
        ))
        .on_conflict(missed_call_reads::user_id)
        .do_update()
        .set(missed_call_reads::seen_at.eq(now))
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;
    let badges = load_badges(&mut conn, user_id).await?;
    // Other open tabs clear the Calls badge too.
    state
        .social_events
        .publish([user_id], SocialResource::Calls);
    Ok(Json(badges))
}

async fn load_badges(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
) -> Result<BadgeCounts, (StatusCode, String)> {
    diesel::sql_query(badges_sql())
        .bind::<SqlUuid, _>(user_id)
        .get_result(conn)
        .await
        .map_err(internal_error)
}

/// The counts mirror what the sidebar lists: conversations come from group memberships and from direct
/// conversations the user has not hidden, and a call is missed when the user never answered it and it is
/// over (its meeting ended or the call expired). Declined calls are not missed.
fn badges_sql() -> String {
    format!(
        "SELECT
            (
                (SELECT COUNT(*) FROM conversations AS c
                 WHERE (
                        (c.kind = 'group' AND EXISTS (
                            SELECT 1 FROM conversation_members AS cm
                            WHERE cm.conversation_id = c.id AND cm.user_id = $1))
                     OR (c.kind = 'direct'
                         AND (c.direct_user_low_id = $1 OR c.direct_user_high_id = $1)
                         AND NOT EXISTS (
                            SELECT 1 FROM conversation_hidden_states AS h
                            WHERE h.conversation_id = c.id AND h.user_id = $1))
                   )
                   AND (
                        EXISTS (
                            SELECT 1 FROM conversation_messages AS m
                            LEFT JOIN conversation_read_states AS r
                              ON r.conversation_id = m.conversation_id AND r.user_id = $1
                            WHERE m.conversation_id = c.id
                              AND m.sender_id <> $1
                              AND m.sequence > COALESCE(r.last_read_sequence, 0))
                     OR EXISTS (
                            SELECT 1 FROM conversation_read_states AS r
                            WHERE r.conversation_id = c.id AND r.user_id = $1 AND r.marked_unread)
                   ))
              + (SELECT COUNT(*) FROM direct_message_requests
                 WHERE recipient_id = $1 AND status = 'pending')
              + (SELECT COUNT(*) FROM group_invitations
                 WHERE invitee_id = $1 AND status = 'pending')
            )::BIGINT AS messages,
            (SELECT COUNT(*) FROM friendships
             WHERE addressee_id = $1 AND status = 'pending')::BIGINT AS friends,
            (
                (SELECT COUNT(*) FROM call_session_members AS member
                 JOIN call_sessions AS s ON s.id = member.call_session_id
                 WHERE member.user_id = $1
                   AND member.status = 'pending'
                   AND s.initiator_id <> $1
                   AND s.created_at > COALESCE(
                        (SELECT seen_at FROM missed_call_reads WHERE user_id = $1), '-infinity')
                   AND (s.status <> 'active' OR s.expires_at <= NOW() OR {ended}))
              + (SELECT COUNT(*) FROM call_invitations AS i
                 WHERE i.callee_id = $1
                   AND i.created_at > COALESCE(
                        (SELECT seen_at FROM missed_call_reads WHERE user_id = $1), '-infinity')
                   AND (i.status = 'expired' OR (i.status = 'pending' AND i.expires_at <= NOW())))
            )::BIGINT AS calls",
        ended = CALL_ROOM_ENDED_SQL.replace("{room}", "s.sfu_room_id"),
    )
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!(%error, "Badge request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Internal server error".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::badges_sql;

    #[test]
    fn missed_calls_use_the_call_room_end_rule() {
        let sql = badges_sql();
        assert!(sql.contains("meeting_sessions"));
        assert!(sql.contains("s.sfu_room_id"));
        assert!(!sql.contains("{room}"));
    }
}
