//! Live rooms as recorded by `MeetingRecorder`: one open meeting session per SFU room, with the
//! connections that have not left yet. The recorder applies events asynchronously, so a join or leave can
//! appear here a moment after it happens in the SFU.

use std::collections::{HashMap, HashSet};

use axum::{Json, extract::State, http::HeaderMap};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde::Serialize;
use uuid::Uuid;

use super::{ApiResult, authorize, internal_error};
use crate::{
    AppState,
    schema::{call_sessions, meeting_participants, meeting_rooms, meeting_sessions, users},
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LiveRoom {
    room_id: String,
    session_id: Uuid,
    started_at: DateTime<Utc>,
    kind: RoomKind,
    access_policy: Option<String>,
    owner: Option<UserSummary>,
    participants: Vec<LiveParticipant>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum RoomKind {
    /// A room created from the meeting-room API, with an owner and access policy.
    Meeting,
    /// A conversation call.
    Call,
    /// A room joined by link without a stored meeting room.
    Instant,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserSummary {
    id: Uuid,
    name: String,
    nickname: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveParticipant {
    participant_id: String,
    display_name: String,
    user: Option<UserSummary>,
    joined_at: DateTime<Utc>,
}

// GET /admin/rooms
pub(super) async fn list_live_rooms(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<LiveRoom>> {
    let (_, mut conn) = authorize(&state, &headers, None).await?;

    let sessions: Vec<(Uuid, String, DateTime<Utc>)> = meeting_sessions::table
        .filter(meeting_sessions::ended_at.is_null())
        .select((
            meeting_sessions::id,
            meeting_sessions::sfu_room_id,
            meeting_sessions::started_at,
        ))
        .order(meeting_sessions::started_at.desc())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let session_ids: Vec<Uuid> = sessions.iter().map(|(id, _, _)| *id).collect();
    let room_ids: Vec<&String> = sessions.iter().map(|(_, room_id, _)| room_id).collect();

    let participant_rows: Vec<(
        Uuid,
        String,
        String,
        DateTime<Utc>,
        Option<(Uuid, String, String)>,
    )> = meeting_participants::table
        .left_join(users::table)
        .filter(meeting_participants::meeting_session_id.eq_any(&session_ids))
        .filter(meeting_participants::left_at.is_null())
        .select((
            meeting_participants::meeting_session_id,
            meeting_participants::participant_id,
            meeting_participants::display_name,
            meeting_participants::joined_at,
            (users::id, users::name, users::nickname).nullable(),
        ))
        .order(meeting_participants::joined_at.asc())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let mut participants: HashMap<Uuid, Vec<LiveParticipant>> = HashMap::new();
    for (session_id, participant_id, display_name, joined_at, user) in participant_rows {
        participants
            .entry(session_id)
            .or_default()
            .push(LiveParticipant {
                participant_id,
                display_name,
                user: user.map(user_summary),
                joined_at,
            });
    }

    let meeting_rows: Vec<(String, String, (Uuid, String, String))> = meeting_rooms::table
        .inner_join(users::table)
        .filter(meeting_rooms::room_id.eq_any(&room_ids))
        .select((
            meeting_rooms::room_id,
            meeting_rooms::access_policy,
            (users::id, users::name, users::nickname),
        ))
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let meetings: HashMap<String, (String, UserSummary)> = meeting_rows
        .into_iter()
        .map(|(room_id, policy, owner)| (room_id, (policy, user_summary(owner))))
        .collect();

    let call_room_ids: HashSet<String> = call_sessions::table
        .filter(call_sessions::sfu_room_id.eq_any(&room_ids))
        .select(call_sessions::sfu_room_id)
        .load::<String>(&mut conn)
        .await
        .map_err(internal_error)?
        .into_iter()
        .collect();

    let rooms = sessions
        .into_iter()
        .map(|(session_id, room_id, started_at)| {
            let meeting = meetings.get(&room_id);
            let kind = if meeting.is_some() {
                RoomKind::Meeting
            } else if call_room_ids.contains(&room_id) {
                RoomKind::Call
            } else {
                RoomKind::Instant
            };
            LiveRoom {
                session_id,
                started_at,
                kind,
                access_policy: meeting.map(|(policy, _)| policy.clone()),
                owner: meeting.map(|(_, owner)| owner.clone()),
                participants: participants.remove(&session_id).unwrap_or_default(),
                room_id,
            }
        })
        .collect();

    Ok(Json(rooms))
}

fn user_summary((id, name, nickname): (Uuid, String, String)) -> UserSummary {
    UserSummary { id, name, nickname }
}
