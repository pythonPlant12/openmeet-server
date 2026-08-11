use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
};
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel::sql_types::Uuid as SqlUuid;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::{
    AppState,
    auth::{extract_user_id, models::User},
    schema::{call_invitations, friendships, meeting_history, user_presence, users},
    social::models::{
        CallInvitation, CallInvitationResponse, CreateCallRequest, CreateFriendRequest,
        FriendRequestItem, FriendSummary, FriendsResponse, Friendship, MeetingHistory,
        MeetingHistoryResponse, NewCallInvitation, NewFriendship, NewMeetingHistory,
        RecordMeetingRequest, RespondToCallRequest, UserPresence,
    },
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;
type EmptyResult = Result<StatusCode, (StatusCode, String)>;

pub fn social_routes() -> Router<AppState> {
    Router::new()
        .route("/friends", get(list_friends).post(create_friend_request))
        .route("/friends/{id}", delete(delete_friendship))
        .route("/friends/{id}/accept", post(accept_friend_request))
        .route("/presence", post(update_presence))
        .route("/calls", post(create_call_invitation))
        .route("/calls/incoming", get(list_incoming_calls))
        .route("/calls/{id}/respond", post(respond_to_call))
        .route("/meetings", get(list_meetings).post(record_meeting))
        .route("/meetings/{id}", delete(delete_meeting))
}

pub async fn list_friends(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<FriendsResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    let relationships: Vec<Friendship> = friendships::table
        .filter(
            friendships::requester_id
                .eq(user_id)
                .or(friendships::addressee_id.eq(user_id)),
        )
        .order(friendships::updated_at.desc())
        .select(Friendship::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    let related_ids: Vec<Uuid> = relationships
        .iter()
        .map(|relationship| {
            if relationship.requester_id == user_id {
                relationship.addressee_id
            } else {
                relationship.requester_id
            }
        })
        .collect();

    let related_users: Vec<User> = users::table
        .filter(users::id.eq_any(&related_ids))
        .select(User::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    let presence: Vec<UserPresence> = user_presence::table
        .filter(user_presence::user_id.eq_any(&related_ids))
        .select(UserPresence::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let online_after = Utc::now() - Duration::seconds(45);
    let presence_by_user: HashMap<Uuid, bool> = presence
        .into_iter()
        .map(|entry| (entry.user_id, entry.last_seen_at > online_after))
        .collect();
    let users_by_id: HashMap<Uuid, FriendSummary> = related_users
        .into_iter()
        .map(|user| {
            let summary = FriendSummary {
                id: user.id,
                name: user.name,
                email: user.email,
                is_online: presence_by_user.get(&user.id).copied().unwrap_or(false),
            };
            (summary.id, summary)
        })
        .collect();

    let mut friends = Vec::new();
    let mut incoming_requests = Vec::new();
    for relationship in relationships {
        let related_id = if relationship.requester_id == user_id {
            relationship.addressee_id
        } else {
            relationship.requester_id
        };
        let Some(user) = users_by_id.get(&related_id).cloned() else {
            continue;
        };

        if relationship.status == "accepted" {
            friends.push(user);
        } else if relationship.addressee_id == user_id {
            incoming_requests.push(FriendRequestItem {
                id: relationship.id,
                user,
                created_at: relationship.created_at,
            });
        }
    }

    Ok(Json(FriendsResponse {
        friends,
        incoming_requests,
    }))
}

pub async fn create_friend_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateFriendRequest>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let email = request.email.trim();
    if email.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Email is required".to_string()));
    }

    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let target: Option<User> = users::table
        .filter(users::email.eq(email))
        .select(User::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?;

    let Some(target) = target.filter(|target| target.id != user_id) else {
        return Ok(StatusCode::NO_CONTENT);
    };

    let result = diesel::insert_into(friendships::table)
        .values(&NewFriendship {
            requester_id: user_id,
            addressee_id: target.id,
        })
        .execute(&mut conn)
        .await;

    match result {
        Ok(_)
        | Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ))
        | Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::CheckViolation,
            _,
        )) => Ok(StatusCode::NO_CONTENT),
        Err(error) => Err(internal_error(error)),
    }
}

pub async fn accept_friend_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(friendship_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let updated = diesel::update(
        friendships::table
            .filter(friendships::id.eq(friendship_id))
            .filter(friendships::addressee_id.eq(user_id))
            .filter(friendships::status.eq("pending")),
    )
    .set((
        friendships::status.eq("accepted"),
        friendships::updated_at.eq(Utc::now()),
    ))
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;

    if updated == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            "Friend request not found".to_string(),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_friendship(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(friendship_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let deleted = diesel::delete(
        friendships::table
            .filter(friendships::id.eq(friendship_id))
            .filter(
                friendships::requester_id
                    .eq(user_id)
                    .or(friendships::addressee_id.eq(user_id)),
            ),
    )
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;

    if deleted == 0 {
        return Err((StatusCode::NOT_FOUND, "Friendship not found".to_string()));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_presence(State(state): State<AppState>, headers: HeaderMap) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let now = Utc::now();

    diesel::insert_into(user_presence::table)
        .values((
            user_presence::user_id.eq(user_id),
            user_presence::last_seen_at.eq(now),
        ))
        .on_conflict(user_presence::user_id)
        .do_update()
        .set(user_presence::last_seen_at.eq(now))
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_call_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateCallRequest>,
) -> ApiResult<CallInvitationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    let friendship_exists = friendships::table
        .filter(friendships::status.eq("accepted"))
        .filter(
            friendships::requester_id
                .eq(user_id)
                .and(friendships::addressee_id.eq(request.friend_id))
                .or(friendships::requester_id
                    .eq(request.friend_id)
                    .and(friendships::addressee_id.eq(user_id))),
        )
        .select(friendships::id)
        .first::<Uuid>(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .is_some();
    if !friendship_exists {
        return Err((
            StatusCode::FORBIDDEN,
            "Calls are limited to friends".to_string(),
        ));
    }

    let now = Utc::now();
    diesel::update(
        call_invitations::table
            .filter(
                call_invitations::caller_id
                    .eq(user_id)
                    .and(call_invitations::callee_id.eq(request.friend_id))
                    .or(call_invitations::caller_id
                        .eq(request.friend_id)
                        .and(call_invitations::callee_id.eq(user_id))),
            )
            .filter(call_invitations::status.eq("pending"))
            .filter(call_invitations::expires_at.le(now)),
    )
    .set((
        call_invitations::status.eq("expired"),
        call_invitations::responded_at.eq(Some(now)),
    ))
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;

    if let Some(invitation) = reuse_pending_call(&mut conn, user_id, request.friend_id, now)
        .await
        .map_err(internal_error)?
    {
        return Ok(Json(call_response(invitation, None)));
    }

    let recently_called = call_invitations::table
        .filter(
            call_invitations::caller_id
                .eq(user_id)
                .and(call_invitations::callee_id.eq(request.friend_id))
                .or(call_invitations::caller_id
                    .eq(request.friend_id)
                    .and(call_invitations::callee_id.eq(user_id))),
        )
        .filter(call_invitations::created_at.gt(now - Duration::seconds(30)))
        .select(call_invitations::id)
        .first::<Uuid>(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .is_some();
    if recently_called {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Please wait before calling again".to_string(),
        ));
    }

    let active_outgoing_calls = call_invitations::table
        .filter(call_invitations::caller_id.eq(user_id))
        .filter(call_invitations::status.eq("pending"))
        .filter(call_invitations::expires_at.gt(now))
        .count()
        .get_result::<i64>(&mut conn)
        .await
        .map_err(internal_error)?;
    if active_outgoing_calls >= 10 {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Too many active calls".to_string(),
        ));
    }

    let invitation_result = diesel::insert_into(call_invitations::table)
        .values(&NewCallInvitation {
            caller_id: user_id,
            callee_id: request.friend_id,
            room_id: Uuid::new_v4().to_string(),
            expires_at: now + Duration::minutes(5),
        })
        .returning(CallInvitation::as_returning())
        .get_result(&mut conn)
        .await;
    let invitation = match invitation_result {
        Ok(invitation) => invitation,
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => reuse_pending_call(&mut conn, user_id, request.friend_id, now)
            .await
            .map_err(internal_error)?
            .ok_or((
                StatusCode::CONFLICT,
                "A call is already pending".to_string(),
            ))?,
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::CheckViolation,
            _,
        )) => {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "Too many active calls".to_string(),
            ));
        }
        Err(error) => return Err(internal_error(error)),
    };

    diesel::sql_query(
        "DELETE FROM call_invitations WHERE caller_id = $1 AND status <> 'pending' AND id NOT IN (SELECT id FROM call_invitations WHERE caller_id = $1 AND status <> 'pending' ORDER BY created_at DESC LIMIT 100)",
    )
    .bind::<SqlUuid, _>(invitation.caller_id)
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;

    Ok(Json(call_response(invitation, None)))
}

async fn reuse_pending_call(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    friend_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<CallInvitation>, diesel::result::Error> {
    let invitation: Option<CallInvitation> = call_invitations::table
        .filter(
            call_invitations::caller_id
                .eq(user_id)
                .and(call_invitations::callee_id.eq(friend_id))
                .or(call_invitations::caller_id
                    .eq(friend_id)
                    .and(call_invitations::callee_id.eq(user_id))),
        )
        .filter(call_invitations::status.eq("pending"))
        .filter(call_invitations::expires_at.gt(now))
        .select(CallInvitation::as_select())
        .first(conn)
        .await
        .optional()?;
    let Some(invitation) = invitation else {
        return Ok(None);
    };
    if invitation.caller_id == user_id {
        return Ok(Some(invitation));
    }

    diesel::update(
        call_invitations::table
            .filter(call_invitations::id.eq(invitation.id))
            .filter(call_invitations::status.eq("pending")),
    )
    .set((
        call_invitations::status.eq("accepted"),
        call_invitations::responded_at.eq(Some(now)),
    ))
    .returning(CallInvitation::as_returning())
    .get_result(conn)
    .await
    .optional()
}

pub async fn list_incoming_calls(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<CallInvitationResponse>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let invitations: Vec<CallInvitation> = call_invitations::table
        .filter(call_invitations::callee_id.eq(user_id))
        .filter(call_invitations::status.eq("pending"))
        .filter(call_invitations::expires_at.gt(Utc::now()))
        .order(call_invitations::created_at.desc())
        .limit(10)
        .select(CallInvitation::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    let caller_ids: Vec<Uuid> = invitations.iter().map(|call| call.caller_id).collect();
    let callers: Vec<User> = users::table
        .filter(users::id.eq_any(&caller_ids))
        .select(User::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let presence: Vec<UserPresence> = user_presence::table
        .filter(user_presence::user_id.eq_any(&caller_ids))
        .select(UserPresence::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let online_after = Utc::now() - Duration::seconds(45);
    let presence_by_user: HashMap<Uuid, bool> = presence
        .into_iter()
        .map(|entry| (entry.user_id, entry.last_seen_at > online_after))
        .collect();
    let callers_by_id: HashMap<Uuid, FriendSummary> = callers
        .into_iter()
        .map(|user| {
            let summary = FriendSummary {
                id: user.id,
                name: user.name,
                email: user.email,
                is_online: presence_by_user.get(&user.id).copied().unwrap_or(false),
            };
            (summary.id, summary)
        })
        .collect();

    Ok(Json(
        invitations
            .into_iter()
            .map(|invitation| {
                let caller = callers_by_id.get(&invitation.caller_id).cloned();
                call_response(invitation, caller)
            })
            .collect(),
    ))
}

pub async fn respond_to_call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(invitation_id): Path<Uuid>,
    Json(request): Json<RespondToCallRequest>,
) -> ApiResult<CallInvitationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let status = if request.accept {
        "accepted"
    } else {
        "declined"
    };

    let invitation: CallInvitation = diesel::update(
        call_invitations::table
            .filter(call_invitations::id.eq(invitation_id))
            .filter(call_invitations::callee_id.eq(user_id))
            .filter(call_invitations::status.eq("pending"))
            .filter(call_invitations::expires_at.gt(Utc::now())),
    )
    .set((
        call_invitations::status.eq(status),
        call_invitations::responded_at.eq(Some(Utc::now())),
    ))
    .returning(CallInvitation::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|error| match error {
        diesel::result::Error::NotFound => (
            StatusCode::NOT_FOUND,
            "Call invitation is no longer available".to_string(),
        ),
        _ => internal_error(error),
    })?;

    Ok(Json(CallInvitationResponse {
        id: invitation.id,
        room_id: invitation.room_id,
        status: status.to_string(),
        expires_at: invitation.expires_at,
        caller: None,
    }))
}

pub async fn list_meetings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<MeetingHistoryResponse>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let meetings: Vec<MeetingHistory> = meeting_history::table
        .filter(meeting_history::user_id.eq(user_id))
        .order(meeting_history::last_joined_at.desc())
        .limit(12)
        .select(MeetingHistory::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        meetings
            .into_iter()
            .map(|meeting| MeetingHistoryResponse {
                id: meeting.id,
                room_id: meeting.room_id,
                last_joined_at: meeting.last_joined_at,
            })
            .collect(),
    ))
}

pub async fn record_meeting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RecordMeetingRequest>,
) -> ApiResult<MeetingHistoryResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    if !is_valid_room_id(&request.room_id) {
        return Err((StatusCode::BAD_REQUEST, "Invalid room ID".to_string()));
    }

    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let now = Utc::now();
    let meeting: MeetingHistory = diesel::insert_into(meeting_history::table)
        .values(&NewMeetingHistory {
            user_id,
            room_id: request.room_id,
            last_joined_at: now,
        })
        .on_conflict((meeting_history::user_id, meeting_history::room_id))
        .do_update()
        .set(meeting_history::last_joined_at.eq(now))
        .returning(MeetingHistory::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;

    diesel::sql_query(
        "DELETE FROM meeting_history WHERE user_id = $1 AND id NOT IN (SELECT id FROM meeting_history WHERE user_id = $1 ORDER BY last_joined_at DESC LIMIT 12)",
    )
    .bind::<SqlUuid, _>(user_id)
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;

    Ok(Json(MeetingHistoryResponse {
        id: meeting.id,
        room_id: meeting.room_id,
        last_joined_at: meeting.last_joined_at,
    }))
}

pub async fn delete_meeting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(meeting_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let deleted = diesel::delete(
        meeting_history::table
            .filter(meeting_history::id.eq(meeting_id))
            .filter(meeting_history::user_id.eq(user_id)),
    )
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;

    if deleted == 0 {
        return Err((StatusCode::NOT_FOUND, "Meeting not found".to_string()));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn call_response(
    invitation: CallInvitation,
    caller: Option<FriendSummary>,
) -> CallInvitationResponse {
    CallInvitationResponse {
        id: invitation.id,
        room_id: invitation.room_id,
        status: invitation.status,
        expires_at: invitation.expires_at,
        caller,
    }
}

fn is_valid_room_id(room_id: &str) -> bool {
    !room_id.is_empty()
        && room_id.len() <= 128
        && room_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!("Social API error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Request failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::is_valid_room_id;

    #[test]
    fn validates_room_ids() {
        assert!(is_valid_room_id("abc-123_room"));
        assert!(!is_valid_room_id(""));
        assert!(!is_valid_room_id("room with spaces"));
        assert!(!is_valid_room_id(&"a".repeat(129)));
    }
}
