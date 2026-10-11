use axum::{Json, extract::State, http::HeaderMap};
use chrono::{Duration, Utc};
use diesel::{dsl::count_star, prelude::*};
use diesel_async::RunQueryDsl;
use serde::Serialize;

use super::{ApiResult, authorize, internal_error};
use crate::{
    AppState,
    schema::{conversation_messages, meeting_participants, meeting_sessions, user_presence, users},
    social::ONLINE_WINDOW_SECONDS,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OverviewResponse {
    users_total: i64,
    users_new_last_7_days: i64,
    users_online: i64,
    live_rooms: i64,
    live_participants: i64,
    meetings_last_24_hours: i64,
    messages_last_24_hours: i64,
    users_by_role: Vec<RoleCount>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RoleCount {
    role: String,
    users: i64,
}

// GET /admin/overview
pub(super) async fn get_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<OverviewResponse> {
    let (_, mut conn) = authorize(&state, &headers, None).await?;
    let now = Utc::now();
    let day_ago = now - Duration::hours(24);

    let users_total = users::table
        .select(count_star())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    // users.created_at is a naive UTC timestamp.
    let users_new_last_7_days = users::table
        .filter(users::created_at.gt((now - Duration::days(7)).naive_utc()))
        .select(count_star())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    let users_online = user_presence::table
        .filter(user_presence::last_seen_at.gt(now - Duration::seconds(ONLINE_WINDOW_SECONDS)))
        .select(count_star())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    let live_rooms = meeting_sessions::table
        .filter(meeting_sessions::ended_at.is_null())
        .select(count_star())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    let live_participants = meeting_participants::table
        .inner_join(meeting_sessions::table)
        .filter(meeting_sessions::ended_at.is_null())
        .filter(meeting_participants::left_at.is_null())
        .select(count_star())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    let meetings_last_24_hours = meeting_sessions::table
        .filter(meeting_sessions::started_at.gt(day_ago))
        .select(count_star())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    let messages_last_24_hours = conversation_messages::table
        .filter(conversation_messages::created_at.gt(day_ago))
        .select(count_star())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    let users_by_role = users::table
        .group_by(users::role)
        .select((users::role, count_star()))
        .order(users::role.asc())
        .load::<(String, i64)>(&mut conn)
        .await
        .map_err(internal_error)?
        .into_iter()
        .map(|(role, users)| RoleCount { role, users })
        .collect();

    Ok(Json(OverviewResponse {
        users_total,
        users_new_last_7_days,
        users_online,
        live_rooms,
        live_participants,
        meetings_last_24_hours,
        messages_last_24_hours,
        users_by_role,
    }))
}
