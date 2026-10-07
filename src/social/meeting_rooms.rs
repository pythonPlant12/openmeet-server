//! Meeting rooms owned by signed-in users, with the same access policies groups have. Signaling admits
//! people through `admit_to_meeting_room`; rooms without a row stay open, as guest meetings always were.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use chrono::Utc;
use diesel::{
    deserialize::QueryableByName,
    prelude::*,
    sql_query,
    sql_types::{Bool, Nullable, Text, Uuid as SqlUuid},
};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    db::DbPool,
    schema::{
        meeting_participants, meeting_room_invitations, meeting_rooms, meeting_sessions, users,
    },
    social::{
        SocialResource,
        avatars::user_avatar_url,
        conversations::{hash_group_password, verify_group_password},
        create_notification,
        handlers::{is_valid_room_id, user_search_pattern, validated_user_search_query},
        models::GroupAccessPolicy,
    },
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

const MAX_CANDIDATES: i64 = 20;
/// Wrong passwords allowed per room within the window, so a room password cannot be guessed quickly.
const PASSWORD_ATTEMPT_LIMIT: usize = 10;
const PASSWORD_ATTEMPT_WINDOW: Duration = Duration::from_secs(60);

pub fn meeting_room_routes() -> Router<AppState> {
    Router::new()
        .route("/", post(create_meeting_room))
        .route(
            "/{room_id}",
            get(get_meeting_room).patch(update_meeting_room),
        )
        .route("/{room_id}/password", post(check_meeting_room_password))
        .route("/{room_id}/candidates", get(list_invitation_candidates))
        .route("/{room_id}/invitations", post(invite_to_meeting_room))
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = meeting_rooms)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct MeetingRoom {
    room_id: String,
    owner_id: Uuid,
    access_policy: String,
    password_hash: Option<String>,
}

impl MeetingRoom {
    fn policy(&self) -> GroupAccessPolicy {
        // The table constraint only allows known values; anything else is treated as the strictest policy.
        GroupAccessPolicy::from_db_value(&self.access_policy)
            .unwrap_or(GroupAccessPolicy::FriendsOnly)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MeetingRoomSettingsRequest {
    access_policy: GroupAccessPolicy,
    password: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PasswordRequest {
    password: String,
}

#[derive(Debug, Deserialize)]
struct CandidatesQuery {
    #[serde(default)]
    query: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InviteRequest {
    user_id: Uuid,
}

/// What one viewer may do in a room. Guests see the policy but never the owner's identity details.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeetingRoomAccessResponse {
    room_id: String,
    /// False for rooms without an owner; they are open and have no settings.
    managed: bool,
    access_policy: GroupAccessPolicy,
    is_owner: bool,
    can_join: bool,
    requires_password: bool,
    owner_name: Option<String>,
    /// Why the viewer cannot join, when they cannot.
    denied_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InvitationCandidate {
    id: Uuid,
    name: String,
    nickname: String,
    avatar_url: Option<String>,
    is_friend: bool,
    invited: bool,
}

/// The result of checking whether someone may enter a meeting room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingRoomAdmission {
    Allowed,
    PasswordRequired,
    WrongPassword,
    Denied(&'static str),
}

impl MeetingRoomAdmission {
    /// The message clients show; it is also how they tell the cases apart.
    pub fn message(self) -> &'static str {
        match self {
            Self::Allowed => "",
            Self::PasswordRequired => "Meeting password required",
            Self::WrongPassword => "Incorrect meeting password",
            Self::Denied(reason) => reason,
        }
    }
}

/// Decides admission without the password: `PasswordRequired` means only a correct password is missing.
fn admission_without_password(
    policy: GroupAccessPolicy,
    is_owner: bool,
    is_invited: bool,
    is_eligible: bool,
    is_signed_in: bool,
) -> MeetingRoomAdmission {
    if is_owner || is_invited {
        return MeetingRoomAdmission::Allowed;
    }
    match policy {
        GroupAccessPolicy::Open => MeetingRoomAdmission::Allowed,
        GroupAccessPolicy::Password => MeetingRoomAdmission::PasswordRequired,
        GroupAccessPolicy::FriendsOnly | GroupAccessPolicy::FriendsOfFriends if !is_signed_in => {
            MeetingRoomAdmission::Denied("Sign in to join this meeting")
        }
        GroupAccessPolicy::FriendsOnly if !is_eligible => {
            MeetingRoomAdmission::Denied("This meeting is limited to the host's friends")
        }
        GroupAccessPolicy::FriendsOfFriends if !is_eligible => MeetingRoomAdmission::Denied(
            "This meeting is limited to the host's friends and their friends",
        ),
        _ => MeetingRoomAdmission::Allowed,
    }
}

/// Admission check for signaling joins. Rooms without an owner are open.
pub async fn admit_to_meeting_room(
    pool: &DbPool,
    room_id: &str,
    user_id: Option<Uuid>,
    password: Option<&str>,
) -> anyhow::Result<MeetingRoomAdmission> {
    let mut conn = pool.get().await?;
    let Some(room) = find_room(&mut conn, room_id).await? else {
        return Ok(MeetingRoomAdmission::Allowed);
    };
    let admission = admission_for(&mut conn, &room, user_id).await?;
    if admission != MeetingRoomAdmission::PasswordRequired {
        return Ok(admission);
    }
    let Some(password) = password.filter(|password| !password.is_empty()) else {
        return Ok(admission);
    };
    Ok(check_password(&room, password).await?)
}

async fn admission_for(
    conn: &mut AsyncPgConnection,
    room: &MeetingRoom,
    user_id: Option<Uuid>,
) -> Result<MeetingRoomAdmission, diesel::result::Error> {
    let policy = room.policy();
    let (is_invited, is_eligible) = match user_id {
        Some(user_id) => (
            is_invited(conn, &room.room_id, user_id).await?,
            is_eligible(conn, policy, room.owner_id, user_id).await?,
        ),
        None => (false, false),
    };
    Ok(admission_without_password(
        policy,
        user_id == Some(room.owner_id),
        is_invited,
        is_eligible,
        user_id.is_some(),
    ))
}

async fn check_password(
    room: &MeetingRoom,
    password: &str,
) -> Result<MeetingRoomAdmission, diesel::result::Error> {
    if !failed_password_attempts(&room.room_id, false) {
        return Ok(MeetingRoomAdmission::Denied(
            "Too many password attempts. Try again in a minute.",
        ));
    }
    let matches = verify_group_password(room.password_hash.clone(), Some(password.to_string()))
        .await
        .unwrap_or(false);
    if matches {
        Ok(MeetingRoomAdmission::Allowed)
    } else {
        failed_password_attempts(&room.room_id, true);
        Ok(MeetingRoomAdmission::WrongPassword)
    }
}

/// Wrong passwords per room in a sliding window. Returns whether another attempt is allowed, after
/// recording a failure when `record_failure` is set. Correct passwords are never counted, so many people
/// joining at once are not locked out.
fn failed_password_attempts(room_id: &str, record_failure: bool) -> bool {
    static FAILURES: OnceLock<Mutex<HashMap<String, VecDeque<Instant>>>> = OnceLock::new();
    let Ok(mut failures) = FAILURES.get_or_init(Default::default).lock() else {
        return false;
    };
    let now = Instant::now();
    failures.retain(|_, times| {
        while times
            .front()
            .is_some_and(|time| now.duration_since(*time) >= PASSWORD_ATTEMPT_WINDOW)
        {
            times.pop_front();
        }
        !times.is_empty()
    });
    let times = failures.entry(room_id.to_string()).or_default();
    if record_failure {
        times.push_back(now);
    }
    times.len() < PASSWORD_ATTEMPT_LIMIT
}

async fn create_meeting_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<MeetingRoomSettingsRequest>,
) -> ApiResult<MeetingRoomAccessResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let password_hash = validated_password_hash(request.access_policy, request.password).await?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let room_id = Uuid::new_v4().to_string();
    diesel::insert_into(meeting_rooms::table)
        .values((
            meeting_rooms::room_id.eq(&room_id),
            meeting_rooms::owner_id.eq(user_id),
            meeting_rooms::access_policy.eq(request.access_policy.as_db_value()),
            meeting_rooms::password_hash.eq(password_hash),
        ))
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;
    access_response(&mut conn, &room_id, Some(user_id))
        .await
        .map(Json)
}

/// Access check before joining. It works without a session, so guests learn whether they need a password.
async fn get_meeting_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<MeetingRoomAccessResponse> {
    let user_id = optional_user_id(&state, &headers)?;
    validate_room_id(&room_id)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    access_response(&mut conn, &room_id, user_id)
        .await
        .map(Json)
}

async fn check_meeting_room_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<PasswordRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let user_id = optional_user_id(&state, &headers)?;
    validate_room_id(&room_id)?;
    match admit_to_meeting_room(&state.pool, &room_id, user_id, Some(&request.password))
        .await
        .map_err(internal_error)?
    {
        MeetingRoomAdmission::Allowed => Ok(StatusCode::NO_CONTENT),
        admission => Err((StatusCode::FORBIDDEN, admission.message().to_string())),
    }
}

async fn update_meeting_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<MeetingRoomSettingsRequest>,
) -> ApiResult<MeetingRoomAccessResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    validate_room_id(&room_id)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let room = find_room(&mut conn, &room_id)
        .await
        .map_err(internal_error)?
        .filter(|room| room.owner_id == user_id)
        .ok_or((
            StatusCode::FORBIDDEN,
            "Only the host can change meeting access".to_string(),
        ))?;

    // Keeping a password policy without a new password keeps the current password.
    let password_hash = if request.access_policy == GroupAccessPolicy::Password
        && room.policy() == GroupAccessPolicy::Password
        && request.password.is_none()
    {
        room.password_hash
    } else {
        validated_password_hash(request.access_policy, request.password).await?
    };
    diesel::update(meeting_rooms::table.find(&room_id))
        .set((
            meeting_rooms::access_policy.eq(request.access_policy.as_db_value()),
            meeting_rooms::password_hash.eq(password_hash),
            meeting_rooms::updated_at.eq(Utc::now()),
        ))
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;
    access_response(&mut conn, &room_id, Some(user_id))
        .await
        .map(Json)
}

/// People the viewer can invite: their friends by default, or anyone matching the search, limited to who
/// the room's policy admits.
async fn list_invitation_candidates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<CandidatesQuery>,
) -> ApiResult<Vec<InvitationCandidate>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    validate_room_id(&room_id)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let room = find_room(&mut conn, &room_id)
        .await
        .map_err(internal_error)?;
    ensure_can_invite(&mut conn, room.as_ref(), &room_id, user_id).await?;

    let search = query.query.trim();
    let pattern = if search.is_empty() {
        None
    } else {
        Some(user_search_pattern(
            validated_user_search_query(search).ok_or((
                StatusCode::BAD_REQUEST,
                "Query must be 2 to 80 characters".to_string(),
            ))?,
        ))
    };
    let (policy, owner_id) = room
        .as_ref()
        .map_or((GroupAccessPolicy::Open, user_id), |room| {
            (room.policy(), room.owner_id)
        });

    #[derive(QueryableByName)]
    struct CandidateRow {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = Text)]
        nickname: String,
        #[diesel(sql_type = Nullable<Text>)]
        avatar_key: Option<String>,
        #[diesel(sql_type = Bool)]
        is_friend: bool,
        #[diesel(sql_type = Bool)]
        invited: bool,
    }

    // Without a search only the viewer's friends are listed; a search reaches everyone the policy allows.
    let rows: Vec<CandidateRow> = sql_query(
        "WITH viewer_friends AS (
             SELECT CASE WHEN requester_id = $1 THEN addressee_id ELSE requester_id END AS id
             FROM friendships
             WHERE status = 'accepted' AND (requester_id = $1 OR addressee_id = $1)
         ),
         owner_friends AS (
             SELECT CASE WHEN requester_id = $2 THEN addressee_id ELSE requester_id END AS id
             FROM friendships
             WHERE status = 'accepted' AND (requester_id = $2 OR addressee_id = $2)
         )
         SELECT candidate.id, candidate.name, candidate.nickname, candidate.avatar_key,
                candidate.id IN (SELECT id FROM viewer_friends) AS is_friend,
                EXISTS (
                    SELECT 1 FROM meeting_room_invitations AS invitation
                    WHERE invitation.room_id = $4 AND invitation.user_id = candidate.id
                ) AS invited
         FROM users AS candidate
         WHERE candidate.id <> $1
           AND candidate.id <> $2
           AND CASE WHEN $3::text IS NULL
               THEN candidate.id IN (SELECT id FROM viewer_friends)
               ELSE candidate.name ILIKE $3 OR candidate.nickname ILIKE $3 OR candidate.email ILIKE $3
           END
           AND CASE $5
               WHEN 'friends_only' THEN candidate.id IN (SELECT id FROM owner_friends)
               WHEN 'friends_of_friends' THEN candidate.id IN (SELECT id FROM owner_friends)
                   OR EXISTS (
                       SELECT 1 FROM friendships AS link
                       WHERE link.status = 'accepted'
                         AND ((link.requester_id = candidate.id AND link.addressee_id IN (SELECT id FROM owner_friends))
                           OR (link.addressee_id = candidate.id AND link.requester_id IN (SELECT id FROM owner_friends)))
                   )
               ELSE TRUE
           END
         ORDER BY is_friend DESC, candidate.name ASC
         LIMIT $6",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<SqlUuid, _>(owner_id)
    .bind::<Nullable<Text>, _>(pattern)
    .bind::<Text, _>(&room_id)
    .bind::<Text, _>(policy.as_db_value())
    .bind::<diesel::sql_types::BigInt, _>(MAX_CANDIDATES)
    .load(&mut conn)
    .await
    .map_err(internal_error)?;

    Ok(Json(
        rows.into_iter()
            .map(|row| InvitationCandidate {
                avatar_url: row
                    .avatar_key
                    .as_deref()
                    .map(|key| user_avatar_url(row.id, key)),
                id: row.id,
                name: row.name,
                nickname: row.nickname,
                is_friend: row.is_friend,
                invited: row.invited,
            })
            .collect(),
    ))
}

async fn invite_to_meeting_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<InviteRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    validate_room_id(&room_id)?;
    if request.user_id == user_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "You are already in this meeting".to_string(),
        ));
    }
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let room = find_room(&mut conn, &room_id)
        .await
        .map_err(internal_error)?;
    ensure_can_invite(&mut conn, room.as_ref(), &room_id, user_id).await?;

    let invitee_exists = users::table
        .find(request.user_id)
        .select(users::id)
        .first::<Uuid>(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .is_some();
    if !invitee_exists {
        return Err((StatusCode::NOT_FOUND, "User not found".to_string()));
    }
    if let Some(room) = &room {
        let policy = room.policy();
        if request.user_id != room.owner_id
            && !is_eligible(&mut conn, policy, room.owner_id, request.user_id)
                .await
                .map_err(internal_error)?
        {
            return Err((
                StatusCode::FORBIDDEN,
                "This person cannot join under the meeting's access settings".to_string(),
            ));
        }
        diesel::insert_into(meeting_room_invitations::table)
            .values((
                meeting_room_invitations::room_id.eq(&room_id),
                meeting_room_invitations::user_id.eq(request.user_id),
                meeting_room_invitations::invited_by.eq(user_id),
            ))
            .on_conflict_do_nothing()
            .execute(&mut conn)
            .await
            .map_err(internal_error)?;
    }

    create_notification(
        &mut conn,
        request.user_id,
        user_id,
        "meetingInvitation",
        json!({ "roomId": room_id }),
    )
    .await
    .map_err(internal_error)?;
    state
        .social_events
        .publish([request.user_id], SocialResource::Notifications);
    Ok(StatusCode::NO_CONTENT)
}

/// Inviting needs a session and either ownership or being in the live meeting right now.
async fn ensure_can_invite(
    conn: &mut AsyncPgConnection,
    room: Option<&MeetingRoom>,
    room_id: &str,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    if room.is_some_and(|room| room.owner_id == user_id) {
        return Ok(());
    }
    let in_meeting = meeting_participants::table
        .inner_join(meeting_sessions::table)
        .filter(meeting_sessions::sfu_room_id.eq(room_id))
        .filter(meeting_sessions::ended_at.is_null())
        .filter(meeting_participants::left_at.is_null())
        .filter(meeting_participants::user_id.eq(user_id))
        .select(meeting_participants::id)
        .first::<Uuid>(conn)
        .await
        .optional()
        .map_err(internal_error)?
        .is_some();
    if in_meeting {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "Join the meeting to invite people".to_string(),
        ))
    }
}

async fn access_response(
    conn: &mut AsyncPgConnection,
    room_id: &str,
    user_id: Option<Uuid>,
) -> Result<MeetingRoomAccessResponse, (StatusCode, String)> {
    let Some(room) = find_room(conn, room_id).await.map_err(internal_error)? else {
        return Ok(MeetingRoomAccessResponse {
            room_id: room_id.to_string(),
            managed: false,
            access_policy: GroupAccessPolicy::Open,
            is_owner: false,
            can_join: true,
            requires_password: false,
            owner_name: None,
            denied_reason: None,
        });
    };
    let admission = admission_for(conn, &room, user_id)
        .await
        .map_err(internal_error)?;
    let owner_name = if user_id.is_some() {
        users::table
            .find(room.owner_id)
            .select(users::name)
            .first::<String>(conn)
            .await
            .optional()
            .map_err(internal_error)?
    } else {
        None
    };
    Ok(MeetingRoomAccessResponse {
        room_id: room.room_id.clone(),
        managed: true,
        access_policy: room.policy(),
        is_owner: user_id == Some(room.owner_id),
        can_join: admission == MeetingRoomAdmission::Allowed,
        requires_password: admission == MeetingRoomAdmission::PasswordRequired,
        owner_name,
        denied_reason: matches!(admission, MeetingRoomAdmission::Denied(_))
            .then(|| admission.message().to_string()),
    })
}

/// Access policies of the given rooms, for history lists.
pub(super) async fn room_policies(
    conn: &mut AsyncPgConnection,
    room_ids: &[String],
) -> Result<HashMap<String, GroupAccessPolicy>, diesel::result::Error> {
    Ok(meeting_rooms::table
        .filter(meeting_rooms::room_id.eq_any(room_ids))
        .select((meeting_rooms::room_id, meeting_rooms::access_policy))
        .load::<(String, String)>(conn)
        .await?
        .into_iter()
        .filter_map(|(room_id, policy)| {
            GroupAccessPolicy::from_db_value(&policy).map(|policy| (room_id, policy))
        })
        .collect())
}

async fn find_room(
    conn: &mut AsyncPgConnection,
    room_id: &str,
) -> Result<Option<MeetingRoom>, diesel::result::Error> {
    meeting_rooms::table
        .find(room_id)
        .select(MeetingRoom::as_select())
        .first(conn)
        .await
        .optional()
}

async fn is_invited(
    conn: &mut AsyncPgConnection,
    room_id: &str,
    user_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    Ok(meeting_room_invitations::table
        .find((room_id, user_id))
        .select(meeting_room_invitations::user_id)
        .first::<Uuid>(conn)
        .await
        .optional()?
        .is_some())
}

/// Whether the policy's friendship rule admits the user. Open and password rooms admit everyone here;
/// the password is checked separately.
async fn is_eligible(
    conn: &mut AsyncPgConnection,
    policy: GroupAccessPolicy,
    owner_id: Uuid,
    user_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    #[derive(QueryableByName)]
    struct Eligibility {
        #[diesel(sql_type = Bool)]
        eligible: bool,
    }
    let query = match policy {
        GroupAccessPolicy::Open | GroupAccessPolicy::Password => return Ok(true),
        GroupAccessPolicy::FriendsOnly => {
            "SELECT EXISTS (
                SELECT 1 FROM friendships
                WHERE status = 'accepted'
                  AND ((requester_id = $1 AND addressee_id = $2) OR (requester_id = $2 AND addressee_id = $1))
            ) AS eligible"
        }
        GroupAccessPolicy::FriendsOfFriends => {
            "WITH owner_friends AS (
                SELECT CASE WHEN requester_id = $1 THEN addressee_id ELSE requester_id END AS id
                FROM friendships
                WHERE status = 'accepted' AND (requester_id = $1 OR addressee_id = $1)
            )
            SELECT $2 IN (SELECT id FROM owner_friends) OR EXISTS (
                SELECT 1 FROM friendships AS link
                WHERE link.status = 'accepted'
                  AND ((link.requester_id = $2 AND link.addressee_id IN (SELECT id FROM owner_friends))
                    OR (link.addressee_id = $2 AND link.requester_id IN (SELECT id FROM owner_friends)))
            ) AS eligible"
        }
    };
    Ok(sql_query(query)
        .bind::<SqlUuid, _>(owner_id)
        .bind::<SqlUuid, _>(user_id)
        .get_result::<Eligibility>(conn)
        .await?
        .eligible)
}

async fn validated_password_hash(
    policy: GroupAccessPolicy,
    password: Option<String>,
) -> Result<Option<String>, (StatusCode, String)> {
    match (policy, password) {
        (GroupAccessPolicy::Password, Some(password)) if (4..=256).contains(&password.len()) => {
            hash_group_password(Some(password)).await
        }
        (GroupAccessPolicy::Password, _) => Err((
            StatusCode::BAD_REQUEST,
            "Meeting passwords must contain 4 to 256 characters".to_string(),
        )),
        (_, None) => Ok(None),
        _ => Err((
            StatusCode::BAD_REQUEST,
            "Only password meetings may include a password".to_string(),
        )),
    }
}

/// The signed-in user when a token is sent; no token means a guest. An invalid token is still an error,
/// so clients refresh it rather than being treated as guests.
fn optional_user_id(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<Uuid>, (StatusCode, String)> {
    if headers.get(header::AUTHORIZATION).is_none() {
        return Ok(None);
    }
    extract_user_id(&state.jwt, headers).map(Some)
}

fn validate_room_id(room_id: &str) -> Result<(), (StatusCode, String)> {
    if is_valid_room_id(room_id) {
        Ok(())
    } else {
        Err((StatusCode::BAD_REQUEST, "Invalid room ID".to_string()))
    }
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!("Meeting room API error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Request failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::{MeetingRoomAdmission, admission_without_password, failed_password_attempts};
    use crate::social::models::GroupAccessPolicy;

    #[test]
    fn owners_and_invited_people_always_get_in() {
        for policy in [
            GroupAccessPolicy::Password,
            GroupAccessPolicy::FriendsOnly,
            GroupAccessPolicy::FriendsOfFriends,
        ] {
            assert_eq!(
                admission_without_password(policy, true, false, false, true),
                MeetingRoomAdmission::Allowed
            );
            assert_eq!(
                admission_without_password(policy, false, true, false, true),
                MeetingRoomAdmission::Allowed
            );
        }
    }

    #[test]
    fn policies_admit_the_right_people() {
        use GroupAccessPolicy::*;
        assert_eq!(
            admission_without_password(Open, false, false, false, false),
            MeetingRoomAdmission::Allowed
        );
        assert_eq!(
            admission_without_password(Password, false, false, true, false),
            MeetingRoomAdmission::PasswordRequired
        );
        assert!(matches!(
            admission_without_password(FriendsOnly, false, false, false, false),
            MeetingRoomAdmission::Denied("Sign in to join this meeting")
        ));
        assert!(matches!(
            admission_without_password(FriendsOnly, false, false, false, true),
            MeetingRoomAdmission::Denied(_)
        ));
        assert_eq!(
            admission_without_password(FriendsOfFriends, false, false, true, true),
            MeetingRoomAdmission::Allowed
        );
    }

    #[test]
    fn limits_wrong_password_attempts_per_room() {
        let room = uuid::Uuid::new_v4().to_string();
        for _ in 0..20 {
            assert!(failed_password_attempts(&room, false));
        }
        for attempt in 1..=10 {
            assert_eq!(failed_password_attempts(&room, true), attempt < 10);
        }
        assert!(!failed_password_attempts(&room, false));
        assert!(failed_password_attempts(
            &uuid::Uuid::new_v4().to_string(),
            false
        ));
    }
}
