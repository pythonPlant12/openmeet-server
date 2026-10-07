//! Meeting history. The SFU reports joins and leaves to `MeetingRecorder`, which stores one session per
//! live period of a room; the API reads those sessions back for history, chat link cards, and statuses.

use std::collections::{HashMap, HashSet};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use diesel::{
    prelude::*,
    sql_types::{Array, Bool, Text, Uuid as SqlUuid},
};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    db::DbPool,
    schema::{
        call_read_states, call_session_members, call_sessions, meeting_participants,
        meeting_sessions, users,
    },
    social::{
        SocialEventHub, SocialResource,
        avatars::user_avatar_url,
        call_sessions::missed_call_sql,
        handlers::is_valid_room_id,
        meeting_rooms::room_policies,
        models::{GroupAccessPolicy, UserStatus},
        presence::{self, Relationship},
    },
};

const DEFAULT_PAGE_SIZE: i64 = 20;
const MAX_PAGE_SIZE: i64 = 50;
/// History rows name a few other people; the detail view lists everyone.
const PREVIEW_PEOPLE: usize = 4;
/// Upper bound on connection rows read for one meeting, so a very busy room cannot load unbounded rows.
const MAX_MEETING_CONNECTIONS: i64 = 2_000;

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

pub fn meeting_session_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_meeting_sessions))
        .route("/{id}", get(get_meeting_session))
        .route("/{id}/read", post(mark_meeting_read))
        .route("/rooms/{room_ref}/summary", get(get_room_summary))
        .route("/rooms/{room_id}/presence", get(get_room_presence))
}

enum MeetingEvent {
    Joined {
        room_id: String,
        participant_id: String,
        user_id: Option<Uuid>,
        display_name: String,
        at: DateTime<Utc>,
    },
    Left {
        participant_id: String,
        at: DateTime<Utc>,
    },
}

/// Records meeting sessions from SFU join and leave events. One worker applies the events in order, so
/// signaling never waits for the database and never holds a room lock across a query.
#[derive(Clone)]
pub struct MeetingRecorder {
    sender: mpsc::UnboundedSender<MeetingEvent>,
}

impl MeetingRecorder {
    pub fn start(pool: DbPool, social_events: SocialEventHub) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(run_recorder(pool, social_events, receiver));
        Self { sender }
    }

    pub fn participant_joined(
        &self,
        room_id: &str,
        participant_id: &str,
        user_id: Option<Uuid>,
        display_name: &str,
    ) {
        let _ = self.sender.send(MeetingEvent::Joined {
            room_id: room_id.to_string(),
            participant_id: participant_id.to_string(),
            user_id,
            display_name: display_name.to_string(),
            at: Utc::now(),
        });
    }

    /// Safe to call for connections that never joined a room; the worker ignores unknown IDs.
    pub fn participant_left(&self, participant_id: &str) {
        let _ = self.sender.send(MeetingEvent::Left {
            participant_id: participant_id.to_string(),
            at: Utc::now(),
        });
    }
}

#[derive(Default)]
struct LiveMeetings {
    rooms: HashMap<String, LiveMeeting>,
    participant_rooms: HashMap<String, String>,
}

struct LiveMeeting {
    session_id: Uuid,
    participant_ids: HashSet<String>,
}

async fn run_recorder(
    pool: DbPool,
    social_events: SocialEventHub,
    mut events: mpsc::UnboundedReceiver<MeetingEvent>,
) {
    // Rooms live in memory, so sessions a previous process left open can never receive their leaves.
    if let Err(error) = close_abandoned_sessions(&pool).await {
        tracing::warn!(%error, "Failed to close abandoned meeting sessions");
    }

    let mut live = LiveMeetings::default();
    while let Some(event) = events.recv().await {
        let result = match event {
            MeetingEvent::Joined {
                room_id,
                participant_id,
                user_id,
                display_name,
                at,
            } => {
                record_join(
                    &pool,
                    &mut live,
                    room_id,
                    participant_id,
                    user_id,
                    display_name,
                    at,
                )
                .await
            }
            MeetingEvent::Left { participant_id, at } => {
                record_leave(&pool, &social_events, &mut live, &participant_id, at).await
            }
        };
        if let Err(error) = result {
            tracing::warn!(%error, "Failed to record meeting event");
        }
    }
}

async fn close_abandoned_sessions(pool: &DbPool) -> anyhow::Result<()> {
    let mut conn = pool.get().await?;
    diesel::sql_query(
        "UPDATE meeting_participants SET left_at = GREATEST(joined_at, NOW()) WHERE left_at IS NULL",
    )
    .execute(&mut conn)
    .await?;
    diesel::sql_query(
        "UPDATE meeting_sessions SET ended_at = GREATEST(started_at, NOW()) WHERE ended_at IS NULL",
    )
    .execute(&mut conn)
    .await?;
    Ok(())
}

async fn record_join(
    pool: &DbPool,
    live: &mut LiveMeetings,
    room_id: String,
    participant_id: String,
    user_id: Option<Uuid>,
    display_name: String,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let mut conn = pool.get().await?;
    let session_id = match live.rooms.get(&room_id) {
        Some(meeting) => meeting.session_id,
        None => {
            // A failed earlier write can leave the room's previous session open; it has ended by now.
            diesel::update(
                meeting_sessions::table
                    .filter(meeting_sessions::sfu_room_id.eq(&room_id))
                    .filter(meeting_sessions::ended_at.is_null()),
            )
            .set(meeting_sessions::ended_at.eq(at))
            .execute(&mut conn)
            .await?;
            let session_id = diesel::insert_into(meeting_sessions::table)
                .values((
                    meeting_sessions::sfu_room_id.eq(&room_id),
                    meeting_sessions::started_at.eq(at),
                ))
                .returning(meeting_sessions::id)
                .get_result::<Uuid>(&mut conn)
                .await?;
            live.rooms.insert(
                room_id.clone(),
                LiveMeeting {
                    session_id,
                    participant_ids: HashSet::new(),
                },
            );
            session_id
        }
    };

    let inserted = diesel::insert_into(meeting_participants::table)
        .values((
            meeting_participants::meeting_session_id.eq(session_id),
            meeting_participants::participant_id.eq(&participant_id),
            meeting_participants::user_id.eq(user_id),
            meeting_participants::display_name.eq(&display_name),
            meeting_participants::joined_at.eq(at),
        ))
        .on_conflict_do_nothing()
        .execute(&mut conn)
        .await;
    if let Err(error) = inserted {
        // Without a participant the session would never see a leave, so do not keep it live.
        if live
            .rooms
            .get(&room_id)
            .is_some_and(|meeting| meeting.participant_ids.is_empty())
        {
            live.rooms.remove(&room_id);
            diesel::update(meeting_sessions::table.find(session_id))
                .set(meeting_sessions::ended_at.eq(at))
                .execute(&mut conn)
                .await?;
        }
        return Err(error.into());
    }

    if let Some(meeting) = live.rooms.get_mut(&room_id) {
        meeting.participant_ids.insert(participant_id.clone());
    }
    live.participant_rooms.insert(participant_id, room_id);
    Ok(())
}

async fn record_leave(
    pool: &DbPool,
    social_events: &SocialEventHub,
    live: &mut LiveMeetings,
    participant_id: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let Some(room_id) = live.participant_rooms.remove(participant_id) else {
        return Ok(());
    };
    let Some(meeting) = live.rooms.get_mut(&room_id) else {
        return Ok(());
    };
    meeting.participant_ids.remove(participant_id);
    let session_id = meeting.session_id;
    let meeting_ended = meeting.participant_ids.is_empty();
    if meeting_ended {
        live.rooms.remove(&room_id);
    }

    let mut conn = pool.get().await?;
    diesel::update(
        meeting_participants::table
            .filter(meeting_participants::meeting_session_id.eq(session_id))
            .filter(meeting_participants::participant_id.eq(participant_id))
            .filter(meeting_participants::left_at.is_null()),
    )
    .set(meeting_participants::left_at.eq(at))
    .execute(&mut conn)
    .await?;
    if meeting_ended {
        diesel::update(
            meeting_sessions::table
                .find(session_id)
                .filter(meeting_sessions::ended_at.is_null()),
        )
        .set(meeting_sessions::ended_at.eq(at))
        .execute(&mut conn)
        .await?;
        notify_call_members(&mut conn, social_events, &room_id).await?;
    }
    Ok(())
}

/// A call stops ringing when its meeting ends, and members who never answered now have a missed call.
async fn notify_call_members(
    conn: &mut AsyncPgConnection,
    social_events: &SocialEventHub,
    room_id: &str,
) -> anyhow::Result<()> {
    let member_ids: Vec<Uuid> = call_session_members::table
        .inner_join(call_sessions::table)
        .filter(call_sessions::sfu_room_id.eq(room_id))
        .filter(call_sessions::status.eq("active"))
        .select(call_session_members::user_id)
        .load(conn)
        .await?;
    social_events.publish(member_ids, SocialResource::Calls);
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MeetingSessionsQuery {
    before: Option<DateTime<Utc>>,
    limit: Option<i64>,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = meeting_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct MeetingSession {
    id: Uuid,
    sfu_room_id: String,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

/// One person in a meeting. Reconnects create several connections, so people are grouped by account,
/// and guests by display name.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct MeetingPerson {
    user_id: Option<Uuid>,
    name: String,
    nickname: Option<String>,
    avatar_url: Option<String>,
    joined_at: DateTime<Utc>,
    /// `None` while the person is still connected.
    left_at: Option<DateTime<Utc>>,
    is_you: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeetingSessionResponse {
    id: Uuid,
    room_id: String,
    /// Set for calls started from a conversation; their meeting links use the call session ID.
    call_session_id: Option<Uuid>,
    conversation_id: Option<Uuid>,
    /// Who could join a hosted meeting; `None` for conversation calls and meetings without a host.
    access_policy: Option<GroupAccessPolicy>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    participant_count: usize,
    participants: Vec<MeetingPerson>,
    /// A call that rang for you and that you never answered.
    missed: bool,
    /// A missed call you have not opened or marked read yet.
    unread: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeetingSessionsPage {
    meetings: Vec<MeetingSessionResponse>,
    next_before: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum RoomStatus {
    Live,
    Ended,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoomSummaryResponse {
    status: RoomStatus,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    participant_count: usize,
    live_participant_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ParticipantPresenceResponse {
    participant_id: String,
    user_id: Uuid,
    /// Hidden when the person does not share their status with meeting peers.
    status: Option<UserStatus>,
    /// Avatars show beside names in the meeting, as they do in its history.
    avatar_url: Option<String>,
}

struct Connection {
    session_id: Uuid,
    user_id: Option<Uuid>,
    display_name: String,
    joined_at: DateTime<Utc>,
    left_at: Option<DateTime<Utc>>,
    account_name: Option<String>,
    nickname: Option<String>,
    avatar_key: Option<String>,
}

async fn list_meeting_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MeetingSessionsQuery>,
) -> ApiResult<MeetingSessionsPage> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    let attended = meeting_participants::table
        .filter(meeting_participants::user_id.eq(user_id))
        .select(meeting_participants::meeting_session_id);
    // Calls that rang for the user and that they missed belong in their history too.
    let missed_room = diesel::dsl::sql::<Bool>(
        "meeting_sessions.sfu_room_id IN (
            SELECT s.sfu_room_id FROM call_sessions AS s
            JOIN call_session_members AS member ON member.call_session_id = s.id
            WHERE member.user_id = ",
    )
    .bind::<SqlUuid, _>(user_id)
    .sql(&format!(" AND {})", missed_call_sql()));
    let mut sessions_query = meeting_sessions::table
        .filter(meeting_sessions::id.eq_any(attended).or(missed_room))
        .order(meeting_sessions::started_at.desc())
        .limit(limit + 1)
        .select(MeetingSession::as_select())
        .into_boxed();
    if let Some(before) = query.before {
        sessions_query = sessions_query.filter(meeting_sessions::started_at.lt(before));
    }
    let mut sessions: Vec<MeetingSession> = sessions_query
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let next_before = (sessions.len() as i64 > limit)
        .then(|| {
            sessions.truncate(limit as usize);
            sessions.last().map(|session| session.started_at)
        })
        .flatten();

    let mut meetings =
        meeting_responses(&mut conn, sessions, user_id, Some(PREVIEW_PEOPLE)).await?;
    apply_missed_flags(&mut conn, user_id, &mut meetings).await?;
    Ok(Json(MeetingSessionsPage {
        meetings,
        next_before,
    }))
}

async fn get_meeting_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
) -> ApiResult<MeetingSessionResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let attended = meeting_participants::table
        .filter(meeting_participants::meeting_session_id.eq(session_id))
        .filter(meeting_participants::user_id.eq(user_id))
        .select(meeting_participants::id)
        .first::<Uuid>(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .is_some();
    let session = meeting_sessions::table
        .find(session_id)
        .select(MeetingSession::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .ok_or_else(meeting_not_found)?;
    // Meetings someone neither attended nor missed a call for look the same as meetings that do not exist.
    if !attended
        && missed_calls(&mut conn, user_id, vec![session.sfu_room_id.clone()])
            .await?
            .is_empty()
    {
        return Err(meeting_not_found());
    }

    let mut meetings = meeting_responses(&mut conn, vec![session], user_id, None).await?;
    apply_missed_flags(&mut conn, user_id, &mut meetings).await?;
    meetings.pop().map(Json).ok_or_else(meeting_not_found)
}

/// Live state of the meeting a chat link points at. Only counts and times are returned, never names, so
/// anyone holding the link learns no more than opening it would show.
async fn get_room_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(room_ref): Path<String>,
) -> ApiResult<RoomSummaryResponse> {
    extract_user_id(&state.jwt, &headers)?;
    if !is_valid_room_id(&room_ref) {
        return Err((StatusCode::BAD_REQUEST, "Invalid room ID".to_string()));
    }
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    let mut session = latest_session(&mut conn, &room_ref).await?;
    // Conversation calls are linked by call session ID, which maps to a separate SFU room ID.
    if session.is_none() {
        if let Ok(call_session_id) = Uuid::parse_str(&room_ref) {
            let sfu_room_id = call_sessions::table
                .find(call_session_id)
                .select(call_sessions::sfu_room_id)
                .first::<String>(&mut conn)
                .await
                .optional()
                .map_err(internal_error)?;
            if let Some(sfu_room_id) = sfu_room_id {
                session = latest_session(&mut conn, &sfu_room_id).await?;
            }
        }
    }
    let session = session.ok_or_else(meeting_not_found)?;

    let connections = load_connections(&mut conn, &[session.id]).await?;
    let people = group_people(&connections, None);
    Ok(Json(RoomSummaryResponse {
        status: if session.ended_at.is_some() {
            RoomStatus::Ended
        } else {
            RoomStatus::Live
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        participant_count: people.len(),
        live_participant_count: people
            .iter()
            .filter(|person| person.left_at.is_none())
            .count(),
    }))
}

/// Statuses of the registered people connected to a live room, for the viewer's own meeting only.
async fn get_room_presence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<Vec<ParticipantPresenceResponse>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    if !is_valid_room_id(&room_id) {
        return Err((StatusCode::BAD_REQUEST, "Invalid room ID".to_string()));
    }
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let session_id = meeting_sessions::table
        .filter(meeting_sessions::sfu_room_id.eq(&room_id))
        .filter(meeting_sessions::ended_at.is_null())
        .select(meeting_sessions::id)
        .first::<Uuid>(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .ok_or_else(meeting_not_found)?;

    let connected: Vec<(String, Uuid)> = meeting_participants::table
        .filter(meeting_participants::meeting_session_id.eq(session_id))
        .filter(meeting_participants::left_at.is_null())
        .filter(meeting_participants::user_id.is_not_null())
        .select((
            meeting_participants::participant_id,
            meeting_participants::user_id.assume_not_null(),
        ))
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    if !connected
        .iter()
        .any(|(_, connected_user_id)| *connected_user_id == user_id)
    {
        return Err(meeting_not_found());
    }

    let user_ids = connected
        .iter()
        .map(|(_, connected_user_id)| *connected_user_id)
        .collect::<HashSet<_>>();
    let accounts: HashMap<Uuid, (Option<UserStatus>, Option<String>)> = users::table
        .filter(users::id.eq_any(user_ids))
        .select((users::id, users::status, users::avatar_key))
        .load::<(Uuid, String, Option<String>)>(&mut conn)
        .await
        .map_err(internal_error)?
        .into_iter()
        .map(|(id, status, avatar_key)| {
            (
                id,
                (
                    UserStatus::from_db_value(&status),
                    avatar_key.map(|key| user_avatar_url(id, &key)),
                ),
            )
        })
        .collect();

    Ok(Json(
        connected
            .into_iter()
            .filter_map(|(participant_id, connected_user_id)| {
                let relationship = if connected_user_id == user_id {
                    Relationship::SELF
                } else {
                    Relationship::MEETING_PEER
                };
                let (status, avatar_url) = accounts.get(&connected_user_id)?;
                let status = status.filter(|_| {
                    presence::can_see(presence::visibility_for(connected_user_id), relationship)
                });
                Some(ParticipantPresenceResponse {
                    participant_id,
                    user_id: connected_user_id,
                    status,
                    avatar_url: avatar_url.clone(),
                })
            })
            .collect(),
    ))
}

async fn latest_session(
    conn: &mut AsyncPgConnection,
    sfu_room_id: &str,
) -> Result<Option<MeetingSession>, (StatusCode, String)> {
    meeting_sessions::table
        .filter(meeting_sessions::sfu_room_id.eq(sfu_room_id))
        .order(meeting_sessions::started_at.desc())
        .select(MeetingSession::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(internal_error)
}

async fn load_connections(
    conn: &mut AsyncPgConnection,
    session_ids: &[Uuid],
) -> Result<Vec<Connection>, (StatusCode, String)> {
    let rows = meeting_participants::table
        .left_join(users::table)
        .filter(meeting_participants::meeting_session_id.eq_any(session_ids))
        .order(meeting_participants::joined_at.asc())
        .limit(MAX_MEETING_CONNECTIONS * session_ids.len().max(1) as i64)
        .select((
            meeting_participants::meeting_session_id,
            meeting_participants::user_id,
            meeting_participants::display_name,
            meeting_participants::joined_at,
            meeting_participants::left_at,
            users::name.nullable(),
            users::nickname.nullable(),
            users::avatar_key.nullable(),
        ))
        .load::<(
            Uuid,
            Option<Uuid>,
            String,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
            Option<String>,
            Option<String>,
            Option<String>,
        )>(conn)
        .await
        .map_err(internal_error)?;

    Ok(rows
        .into_iter()
        .map(
            |(
                session_id,
                user_id,
                display_name,
                joined_at,
                left_at,
                account_name,
                nickname,
                avatar_key,
            )| Connection {
                session_id,
                user_id,
                display_name,
                joined_at,
                left_at,
                account_name,
                nickname,
                avatar_key,
            },
        )
        .collect())
}

/// Groups one meeting's connections (ordered by join time) into people, in the order they first joined.
fn group_people(connections: &[Connection], viewer_id: Option<Uuid>) -> Vec<MeetingPerson> {
    let mut people: Vec<MeetingPerson> = Vec::new();
    let mut index_by_key: HashMap<String, usize> = HashMap::new();
    for connection in connections {
        let key = match connection.user_id {
            Some(user_id) => format!("user:{user_id}"),
            None => format!("guest:{}", connection.display_name.trim().to_lowercase()),
        };
        if let Some(&index) = index_by_key.get(&key) {
            let person = &mut people[index];
            person.left_at = match (person.left_at, connection.left_at) {
                (Some(previous), Some(current)) => Some(previous.max(current)),
                _ => None,
            };
            continue;
        }
        index_by_key.insert(key, people.len());
        people.push(MeetingPerson {
            user_id: connection.user_id,
            name: connection
                .account_name
                .clone()
                .unwrap_or_else(|| connection.display_name.clone()),
            nickname: connection.nickname.clone(),
            avatar_url: connection
                .user_id
                .zip(connection.avatar_key.as_deref())
                .map(|(user_id, key)| user_avatar_url(user_id, key)),
            joined_at: connection.joined_at,
            left_at: connection.left_at,
            is_you: viewer_id.is_some() && connection.user_id == viewer_id,
        });
    }
    people
}

async fn meeting_responses(
    conn: &mut AsyncPgConnection,
    sessions: Vec<MeetingSession>,
    viewer_id: Uuid,
    preview_people: Option<usize>,
) -> Result<Vec<MeetingSessionResponse>, (StatusCode, String)> {
    if sessions.is_empty() {
        return Ok(Vec::new());
    }
    let session_ids = sessions
        .iter()
        .map(|session| session.id)
        .collect::<Vec<_>>();
    let room_ids = sessions
        .iter()
        .map(|session| session.sfu_room_id.clone())
        .collect::<Vec<_>>();
    let calls: HashMap<String, (Uuid, Uuid)> = call_sessions::table
        .filter(call_sessions::sfu_room_id.eq_any(&room_ids))
        .select((
            call_sessions::sfu_room_id,
            call_sessions::id,
            call_sessions::conversation_id,
        ))
        .load::<(String, Uuid, Uuid)>(conn)
        .await
        .map_err(internal_error)?
        .into_iter()
        .map(|(room_id, id, conversation_id)| (room_id, (id, conversation_id)))
        .collect();

    let policies = room_policies(conn, &room_ids)
        .await
        .map_err(internal_error)?;

    let mut connections_by_session: HashMap<Uuid, Vec<Connection>> = HashMap::new();
    for connection in load_connections(conn, &session_ids).await? {
        connections_by_session
            .entry(connection.session_id)
            .or_default()
            .push(connection);
    }

    Ok(sessions
        .into_iter()
        .map(|session| {
            let people = group_people(
                connections_by_session
                    .get(&session.id)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                Some(viewer_id),
            );
            let participant_count = people.len();
            let participants = match preview_people {
                // Previews name the other people, which is what identifies a call in a list.
                Some(limit) => people
                    .into_iter()
                    .filter(|person| !person.is_you)
                    .take(limit)
                    .collect(),
                None => people,
            };
            let call = calls.get(&session.sfu_room_id);
            MeetingSessionResponse {
                id: session.id,
                call_session_id: call.map(|(id, _)| *id),
                conversation_id: call.map(|(_, conversation_id)| *conversation_id),
                access_policy: policies.get(&session.sfu_room_id).copied(),
                room_id: session.sfu_room_id,
                started_at: session.started_at,
                ended_at: session.ended_at,
                participant_count,
                participants,
                missed: false,
                unread: false,
            }
        })
        .collect())
}

#[derive(QueryableByName)]
struct MissedCall {
    #[diesel(sql_type = Text)]
    sfu_room_id: String,
    #[diesel(sql_type = SqlUuid)]
    call_session_id: Uuid,
    #[diesel(sql_type = Bool)]
    unread: bool,
}

/// Calls in these rooms that rang for the user and that they missed, keyed by room.
async fn missed_calls(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    room_ids: Vec<String>,
) -> Result<HashMap<String, MissedCall>, (StatusCode, String)> {
    if room_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<MissedCall> = diesel::sql_query(format!(
        "SELECT s.sfu_room_id, s.id AS call_session_id,
                NOT EXISTS (
                    SELECT 1 FROM call_read_states AS r
                    WHERE r.call_session_id = s.id AND r.user_id = $1) AS unread
         FROM call_sessions AS s
         JOIN call_session_members AS member ON member.call_session_id = s.id
         WHERE member.user_id = $1 AND s.sfu_room_id = ANY($2) AND {}",
        missed_call_sql()
    ))
    .bind::<SqlUuid, _>(user_id)
    .bind::<Array<Text>, _>(room_ids)
    .load(conn)
    .await
    .map_err(internal_error)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.sfu_room_id.clone(), row))
        .collect())
}

/// Marks meetings the user did not attend but whose call they missed.
async fn apply_missed_flags(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    meetings: &mut [MeetingSessionResponse],
) -> Result<(), (StatusCode, String)> {
    let attended: HashSet<Uuid> = meeting_participants::table
        .filter(meeting_participants::user_id.eq(user_id))
        .filter(
            meeting_participants::meeting_session_id.eq_any(
                meetings
                    .iter()
                    .map(|meeting| meeting.id)
                    .collect::<Vec<_>>(),
            ),
        )
        .select(meeting_participants::meeting_session_id)
        .load::<Uuid>(conn)
        .await
        .map_err(internal_error)?
        .into_iter()
        .collect();
    let missed = missed_calls(
        conn,
        user_id,
        meetings
            .iter()
            .filter(|meeting| !attended.contains(&meeting.id))
            .map(|meeting| meeting.room_id.clone())
            .collect(),
    )
    .await?;
    for meeting in meetings.iter_mut() {
        if attended.contains(&meeting.id) {
            continue;
        }
        if let Some(call) = missed.get(&meeting.room_id) {
            meeting.missed = true;
            meeting.unread = call.unread;
        }
    }
    Ok(())
}

/// Opening a missed call or swiping it read clears its dot and its place in the Calls badge.
async fn mark_meeting_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let room_id = meeting_sessions::table
        .find(session_id)
        .select(meeting_sessions::sfu_room_id)
        .first::<String>(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .ok_or_else(meeting_not_found)?;
    let Some(call) = missed_calls(&mut conn, user_id, vec![room_id])
        .await?
        .into_values()
        .next()
    else {
        // Only missed calls have a read state; anything else is already read.
        return Ok(StatusCode::NO_CONTENT);
    };
    diesel::insert_into(call_read_states::table)
        .values((
            call_read_states::call_session_id.eq(call.call_session_id),
            call_read_states::user_id.eq(user_id),
        ))
        .on_conflict_do_nothing()
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;
    // Other open tabs update their dot and badge.
    state
        .social_events
        .publish([user_id], SocialResource::Calls);
    Ok(StatusCode::NO_CONTENT)
}

fn meeting_not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "Meeting not found".to_string())
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!("Meeting API error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Request failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration};
    use uuid::Uuid;

    use super::{Connection, group_people};

    fn connection(
        user_id: Option<Uuid>,
        display_name: &str,
        joined_minutes: i64,
        left_minutes: Option<i64>,
    ) -> Connection {
        let start = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        Connection {
            session_id: Uuid::nil(),
            user_id,
            display_name: display_name.to_string(),
            joined_at: start + Duration::minutes(joined_minutes),
            left_at: left_minutes.map(|minutes| start + Duration::minutes(minutes)),
            account_name: user_id.map(|_| format!("{display_name} Account")),
            nickname: None,
            avatar_key: None,
        }
    }

    #[test]
    fn groups_reconnects_by_account_and_guests_by_name() {
        let ada = Uuid::new_v4();
        let people = group_people(
            &[
                connection(Some(ada), "Ada", 0, Some(5)),
                connection(None, "Guest", 1, Some(3)),
                connection(Some(ada), "Ada", 6, Some(20)),
                connection(None, " guest ", 4, Some(10)),
            ],
            Some(ada),
        );

        assert_eq!(people.len(), 2);
        assert_eq!(people[0].name, "Ada Account");
        assert!(people[0].is_you);
        assert_eq!(
            people[0].left_at.unwrap() - people[0].joined_at,
            Duration::minutes(20)
        );
        assert_eq!(people[1].user_id, None);
        assert!(!people[1].is_you);
    }

    #[test]
    fn a_person_with_an_open_connection_has_not_left() {
        let user = Uuid::new_v4();
        let people = group_people(
            &[
                connection(Some(user), "Ada", 0, Some(5)),
                connection(Some(user), "Ada", 6, None),
            ],
            None,
        );

        assert_eq!(people[0].left_at, None);
    }
}
