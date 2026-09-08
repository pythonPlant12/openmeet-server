use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{
    AsyncConnection, AsyncPgConnection, RunQueryDsl, scoped_futures::ScopedFutureExt,
};
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    db::DbPool,
    schema::{call_session_members, call_sessions, conversation_members, conversations},
    social::{
        SocialResource,
        models::{
            CallSession, CallSessionJoinResponse, CallSessionMember, CallSessionResponse,
            CallSessionStartResponse, Conversation, IncomingCallSessionResponse, NewCallSession,
            NewCallSessionMember, RespondToCallSessionRequest,
        },
    },
};

const CALL_SESSION_TTL: Duration = Duration::hours(1);

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SfuRoomAuthorization {
    Legacy,
    Authorized,
    Denied,
}

#[derive(Debug)]
struct CallSessionError {
    status: StatusCode,
    message: String,
}

impl CallSessionError {
    fn into_api_error(self) -> (StatusCode, String) {
        (self.status, self.message)
    }
}

impl From<(StatusCode, String)> for CallSessionError {
    fn from((status, message): (StatusCode, String)) -> Self {
        Self { status, message }
    }
}

impl From<diesel::result::Error> for CallSessionError {
    fn from(error: diesel::result::Error) -> Self {
        let (status, message) = internal_error(error);
        Self { status, message }
    }
}

pub fn call_session_routes() -> Router<AppState> {
    Router::new()
        .route("/incoming", get(list_incoming_call_sessions))
        .route("/{id}", get(get_call_session))
        .route("/{id}/respond", post(respond_to_call_session))
}

pub async fn start_call_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
) -> ApiResult<CallSessionStartResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let now = Utc::now();
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let (session, recipients): (CallSession, Vec<Uuid>) = conn
        .transaction(|conn| {
            async move {
                let conversation = load_conversation_for_update(conn, conversation_id).await?;
                let participant_ids = snapshot_participants(conn, &conversation, user_id).await?;

                expire_call_sessions(conn, now).await?;
                let existing_session = call_sessions::table
                    .filter(call_sessions::conversation_id.eq(conversation_id))
                    .filter(call_sessions::status.eq("active"))
                    .select(call_sessions::id)
                    .first::<Uuid>(conn)
                    .await
                    .optional()?;
                if existing_session.is_some() {
                    return Err(CallSessionError::from((
                        StatusCode::CONFLICT,
                        "A call is already active for this conversation".to_string(),
                    )));
                }

                let session: CallSession = diesel::insert_into(call_sessions::table)
                    .values(NewCallSession {
                        conversation_id,
                        initiator_id: user_id,
                        sfu_room_id: Uuid::new_v4().to_string(),
                        expires_at: now + CALL_SESSION_TTL,
                    })
                    .returning(CallSession::as_returning())
                    .get_result(conn)
                    .await?;
                let members = participant_ids
                    .iter()
                    .copied()
                    .map(|participant_id| NewCallSessionMember {
                        call_session_id: session.id,
                        user_id: participant_id,
                        status: if participant_id == user_id {
                            "accepted".to_string()
                        } else {
                            "pending".to_string()
                        },
                        responded_at: (participant_id == user_id).then_some(now),
                    })
                    .collect::<Vec<_>>();
                diesel::insert_into(call_session_members::table)
                    .values(&members)
                    .execute(conn)
                    .await?;

                Ok((session, participant_ids))
            }
            .scope_boxed()
        })
        .await
        .map_err(CallSessionError::into_api_error)?;

    state
        .social_events
        .publish(recipients, SocialResource::Calls);

    Ok(Json(CallSessionStartResponse {
        id: session.id,
        room_id: session.sfu_room_id,
        expires_at: session.expires_at,
    }))
}

async fn list_incoming_call_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<IncomingCallSessionResponse>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let now = Utc::now();
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    expire_call_sessions(&mut conn, now)
        .await
        .map_err(internal_error)?;

    let sessions: Vec<CallSession> = call_session_members::table
        .inner_join(call_sessions::table)
        .filter(call_session_members::user_id.eq(user_id))
        .filter(call_session_members::status.eq("pending"))
        .filter(call_sessions::status.eq("active"))
        .filter(call_sessions::expires_at.gt(now))
        .order(call_sessions::created_at.desc())
        .select(CallSession::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        sessions
            .into_iter()
            .map(|session| IncomingCallSessionResponse {
                id: session.id,
                conversation_id: session.conversation_id,
                initiator_id: session.initiator_id,
                expires_at: session.expires_at,
                created_at: session.created_at,
            })
            .collect(),
    ))
}

async fn get_call_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(call_session_id): Path<Uuid>,
) -> ApiResult<CallSessionJoinResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let now = Utc::now();
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    expire_call_sessions(&mut conn, now)
        .await
        .map_err(internal_error)?;

    let room_id = call_session_members::table
        .inner_join(call_sessions::table)
        .filter(call_session_members::call_session_id.eq(call_session_id))
        .filter(call_session_members::user_id.eq(user_id))
        .filter(call_session_members::status.eq("accepted"))
        .filter(call_sessions::status.eq("active"))
        .filter(call_sessions::expires_at.gt(now))
        .select(call_sessions::sfu_room_id)
        .first::<String>(&mut conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::NotFound => (
                StatusCode::NOT_FOUND,
                "Call session is not available".to_string(),
            ),
            _ => internal_error(error),
        })?;

    Ok(Json(CallSessionJoinResponse { room_id }))
}

async fn respond_to_call_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(call_session_id): Path<Uuid>,
    Json(request): Json<RespondToCallSessionRequest>,
) -> ApiResult<CallSessionResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let now = Utc::now();
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let (room_id, recipients): (Option<String>, Vec<Uuid>) = conn
        .transaction(|conn| {
            async move {
                expire_call_sessions(conn, now).await?;
                let session: CallSession = call_sessions::table
                    .find(call_session_id)
                    .filter(call_sessions::status.eq("active"))
                    .filter(call_sessions::expires_at.gt(now))
                    .for_update()
                    .select(CallSession::as_select())
                    .first(conn)
                    .await
                    .map_err(|error| match error {
                        diesel::result::Error::NotFound => CallSessionError::from((
                            StatusCode::NOT_FOUND,
                            "Call session is not available".to_string(),
                        )),
                        _ => CallSessionError::from(error),
                    })?;
                let member: CallSessionMember = call_session_members::table
                    .filter(call_session_members::call_session_id.eq(call_session_id))
                    .filter(call_session_members::user_id.eq(user_id))
                    .filter(call_session_members::status.eq("pending"))
                    .for_update()
                    .select(CallSessionMember::as_select())
                    .first(conn)
                    .await
                    .map_err(|error| match error {
                        diesel::result::Error::NotFound => CallSessionError::from((
                            StatusCode::NOT_FOUND,
                            "Call session is not available".to_string(),
                        )),
                        _ => CallSessionError::from(error),
                    })?;
                let recipients = call_session_member_ids(conn, call_session_id).await?;
                let status = if request.accept {
                    "accepted"
                } else {
                    "declined"
                };
                diesel::update(
                    call_session_members::table
                        .filter(call_session_members::call_session_id.eq(member.call_session_id))
                        .filter(call_session_members::user_id.eq(member.user_id)),
                )
                .set((
                    call_session_members::status.eq(status),
                    call_session_members::responded_at.eq(Some(now)),
                ))
                .execute(conn)
                .await?;

                if request.accept {
                    Ok((Some(session.sfu_room_id), recipients))
                } else {
                    Ok((None, recipients))
                }
            }
            .scope_boxed()
        })
        .await
        .map_err(CallSessionError::into_api_error)?;

    state
        .social_events
        .publish(recipients, SocialResource::Calls);

    Ok(Json(call_session_response(request.accept, room_id)))
}

async fn call_session_member_ids(
    conn: &mut AsyncPgConnection,
    call_session_id: Uuid,
) -> Result<Vec<Uuid>, (StatusCode, String)> {
    call_session_members::table
        .filter(call_session_members::call_session_id.eq(call_session_id))
        .select(call_session_members::user_id)
        .load(conn)
        .await
        .map_err(internal_error)
}

/// Classifies an SFU room and authorizes accepted call-session participants.
pub async fn authorize_sfu_room(
    pool: &DbPool,
    sfu_room_id: &str,
    user_id: Option<Uuid>,
) -> Result<SfuRoomAuthorization> {
    let now = Utc::now();
    let mut conn = pool.get().await?;
    expire_call_sessions(&mut conn, now).await?;

    let is_call_session_room = call_sessions::table
        .filter(call_sessions::sfu_room_id.eq(sfu_room_id))
        .select(call_sessions::id)
        .first::<Uuid>(&mut conn)
        .await
        .optional()?
        .is_some();
    if !is_call_session_room {
        return Ok(classify_sfu_room_authorization(false, user_id, false));
    }

    let Some(user_id) = user_id else {
        return Ok(classify_sfu_room_authorization(true, None, false));
    };
    let authorized = call_session_members::table
        .inner_join(call_sessions::table)
        .filter(call_session_members::user_id.eq(user_id))
        .filter(call_session_members::status.eq("accepted"))
        .filter(call_sessions::sfu_room_id.eq(sfu_room_id))
        .filter(call_sessions::status.eq("active"))
        .filter(call_sessions::expires_at.gt(now))
        .select(call_session_members::user_id)
        .first::<Uuid>(&mut conn)
        .await
        .optional()?
        .is_some();
    Ok(classify_sfu_room_authorization(
        true,
        Some(user_id),
        authorized,
    ))
}

fn classify_sfu_room_authorization(
    is_call_session_room: bool,
    user_id: Option<Uuid>,
    has_accepted_membership: bool,
) -> SfuRoomAuthorization {
    if !is_call_session_room {
        SfuRoomAuthorization::Legacy
    } else if user_id.is_some() && has_accepted_membership {
        SfuRoomAuthorization::Authorized
    } else {
        SfuRoomAuthorization::Denied
    }
}

async fn load_conversation_for_update(
    conn: &mut AsyncPgConnection,
    conversation_id: Uuid,
) -> Result<Conversation, CallSessionError> {
    conversations::table
        .find(conversation_id)
        .for_update()
        .select(Conversation::as_select())
        .first(conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::NotFound => CallSessionError::from((
                StatusCode::NOT_FOUND,
                "Conversation not found".to_string(),
            )),
            _ => CallSessionError::from(error),
        })
}

async fn snapshot_participants(
    conn: &mut AsyncPgConnection,
    conversation: &Conversation,
    caller_id: Uuid,
) -> Result<Vec<Uuid>, CallSessionError> {
    match conversation.kind.as_str() {
        "direct" => match (
            conversation.direct_user_low_id,
            conversation.direct_user_high_id,
        ) {
            (Some(low_id), Some(high_id)) if is_direct_participant(low_id, high_id, caller_id) => {
                Ok(vec![low_id, high_id])
            }
            _ => Err(CallSessionError::from((
                StatusCode::FORBIDDEN,
                "Conversation access denied".to_string(),
            ))),
        },
        "group" => {
            let member_ids = conversation_members::table
                .filter(conversation_members::conversation_id.eq(conversation.id))
                .select(conversation_members::user_id)
                .load(conn)
                .await?;
            if member_ids.contains(&caller_id) {
                Ok(member_ids)
            } else {
                Err(CallSessionError::from((
                    StatusCode::FORBIDDEN,
                    "Conversation access denied".to_string(),
                )))
            }
        }
        _ => Err(CallSessionError::from((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Conversation unavailable".to_string(),
        ))),
    }
}

async fn expire_call_sessions(
    conn: &mut AsyncPgConnection,
    now: DateTime<Utc>,
) -> Result<(), diesel::result::Error> {
    diesel::update(
        call_sessions::table
            .filter(call_sessions::status.eq("active"))
            .filter(call_sessions::expires_at.le(now)),
    )
    .set(call_sessions::status.eq("expired"))
    .execute(conn)
    .await?;
    Ok(())
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!("Call session API error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Request failed".to_string(),
    )
}

fn call_session_response(accepted: bool, room_id: Option<String>) -> CallSessionResponse {
    CallSessionResponse {
        accepted,
        room_id: accepted.then_some(room_id).flatten(),
    }
}

fn is_direct_participant(low_id: Uuid, high_id: Uuid, user_id: Uuid) -> bool {
    user_id == low_id || user_id == high_id
}

#[cfg(test)]
mod tests {
    use super::{
        CALL_SESSION_TTL, SfuRoomAuthorization, call_session_response,
        classify_sfu_room_authorization, is_direct_participant,
    };
    use uuid::Uuid;

    #[test]
    fn call_sessions_have_a_bounded_lifetime() {
        assert_eq!(CALL_SESSION_TTL, chrono::Duration::hours(1));
        assert!(CALL_SESSION_TTL > chrono::Duration::zero());
    }

    #[test]
    fn declined_call_response_never_serializes_a_room_id() {
        let response = call_session_response(false, Some("opaque-room".to_string()));

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({ "accepted": false })
        );
    }

    #[test]
    fn accepted_call_response_contains_its_room_id() {
        let response = call_session_response(true, Some("opaque-room".to_string()));

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({ "accepted": true, "roomId": "opaque-room" })
        );
    }

    #[test]
    fn authorizes_only_users_in_the_direct_conversation_pair() {
        let low_id = Uuid::new_v4();
        let high_id = Uuid::new_v4();

        assert!(is_direct_participant(low_id, high_id, low_id));
        assert!(is_direct_participant(low_id, high_id, high_id));
        assert!(!is_direct_participant(low_id, high_id, Uuid::new_v4()));
    }

    #[test]
    fn classifies_legacy_and_call_session_room_admission() {
        let user_id = Uuid::new_v4();

        assert_eq!(
            classify_sfu_room_authorization(false, None, false),
            SfuRoomAuthorization::Legacy
        );
        assert_eq!(
            classify_sfu_room_authorization(true, None, false),
            SfuRoomAuthorization::Denied
        );
        assert_eq!(
            classify_sfu_room_authorization(true, Some(user_id), false),
            SfuRoomAuthorization::Denied
        );
        assert_eq!(
            classify_sfu_room_authorization(true, Some(user_id), true),
            SfuRoomAuthorization::Authorized
        );
    }
}
