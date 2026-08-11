use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::schema::{call_invitations, friendships, meeting_history, user_presence};

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = friendships)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Friendship {
    pub id: Uuid,
    pub requester_id: Uuid,
    pub addressee_id: Uuid,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = friendships)]
pub struct NewFriendship {
    pub requester_id: Uuid,
    pub addressee_id: Uuid,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = call_invitations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct CallInvitation {
    pub id: Uuid,
    pub caller_id: Uuid,
    pub callee_id: Uuid,
    pub room_id: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub responded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = call_invitations)]
pub struct NewCallInvitation {
    pub caller_id: Uuid,
    pub callee_id: Uuid,
    pub room_id: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = meeting_history)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct MeetingHistory {
    pub id: Uuid,
    pub user_id: Uuid,
    pub room_id: String,
    pub last_joined_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = meeting_history)]
pub struct NewMeetingHistory {
    pub user_id: Uuid,
    pub room_id: String,
    pub last_joined_at: DateTime<Utc>,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = user_presence)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct UserPresence {
    pub user_id: Uuid,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFriendRequest {
    pub email: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCallRequest {
    pub friend_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RespondToCallRequest {
    pub accept: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordMeetingRequest {
    pub room_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendSummary {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub is_online: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendRequestItem {
    pub id: Uuid,
    pub user: FriendSummary,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendsResponse {
    pub friends: Vec<FriendSummary>,
    pub incoming_requests: Vec<FriendRequestItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallInvitationResponse {
    pub id: Uuid,
    pub room_id: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub caller: Option<FriendSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingHistoryResponse {
    pub id: Uuid,
    pub room_id: String,
    pub last_joined_at: DateTime<Utc>,
}
