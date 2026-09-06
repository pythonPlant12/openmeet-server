use chrono::{DateTime, NaiveDateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::schema::{
    call_invitations, call_session_members, call_sessions, conversation_members,
    conversation_messages, conversations, direct_message_requests, friendships, meeting_history,
    notifications, user_presence, users,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GroupAccessPolicy {
    Open,
    Password,
    FriendsOnly,
}

impl GroupAccessPolicy {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Password => "password",
            Self::FriendsOnly => "friends_only",
        }
    }

    pub fn from_db_value(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "password" => Some(Self::Password),
            "friends_only" => Some(Self::FriendsOnly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConversationKind {
    Group,
    Direct,
}

impl ConversationKind {
    pub fn from_db_value(value: &str) -> Option<Self> {
        match value {
            "group" => Some(Self::Group),
            "direct" => Some(Self::Direct),
            _ => None,
        }
    }
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = conversations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Conversation {
    pub id: Uuid,
    pub kind: String,
    pub creator_id: Uuid,
    pub title: Option<String>,
    pub access_policy: Option<String>,
    pub password_hash: Option<String>,
    pub direct_user_low_id: Option<Uuid>,
    pub direct_user_high_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversations)]
pub struct NewConversation {
    pub kind: String,
    pub creator_id: Uuid,
    pub title: Option<String>,
    pub access_policy: Option<String>,
    pub password_hash: Option<String>,
    pub direct_user_low_id: Option<Uuid>,
    pub direct_user_high_id: Option<Uuid>,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = conversation_members)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ConversationMember {
    pub conversation_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversation_members)]
pub struct NewConversationMember {
    pub conversation_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = conversation_messages)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ConversationMessage {
    pub sequence: i64,
    pub conversation_id: Uuid,
    pub sender_id: Uuid,
    pub sender_name: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = conversation_messages)]
pub struct NewConversationMessage {
    pub conversation_id: Uuid,
    pub sender_id: Uuid,
    pub sender_name: String,
    pub content: String,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = call_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct CallSession {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub initiator_id: Uuid,
    pub sfu_room_id: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = call_sessions)]
pub struct NewCallSession {
    pub conversation_id: Uuid,
    pub initiator_id: Uuid,
    pub sfu_room_id: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = call_session_members)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct CallSessionMember {
    pub call_session_id: Uuid,
    pub user_id: Uuid,
    pub status: String,
    pub responded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = call_session_members)]
pub struct NewCallSessionMember {
    pub call_session_id: Uuid,
    pub user_id: Uuid,
    pub status: String,
    pub responded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = direct_message_requests)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct DirectMessageRequest {
    pub id: Uuid,
    pub requester_id: Uuid,
    pub recipient_id: Uuid,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = direct_message_requests)]
pub struct NewDirectMessageRequest {
    pub requester_id: Uuid,
    pub recipient_id: Uuid,
}

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
#[diesel(table_name = notifications)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Notification {
    pub id: Uuid,
    pub recipient_id: Uuid,
    pub actor_id: Uuid,
    pub kind: String,
    pub data: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = notifications)]
pub struct NewNotification<'a> {
    pub recipient_id: Uuid,
    pub actor_id: Uuid,
    pub kind: &'a str,
    pub data: serde_json::Value,
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
pub struct SearchUsersQuery {
    pub query: String,
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
pub struct RespondToCallSessionRequest {
    pub accept: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordMeetingRequest {
    pub room_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGroupRequest {
    pub title: String,
    pub access_policy: GroupAccessPolicy,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateGroupPolicyRequest {
    pub access_policy: GroupAccessPolicy,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddConversationMemberRequest {
    pub user_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinGroupRequest {
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateConversationMessageRequest {
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ListConversationMessagesQuery {
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GroupMemberRole {
    Admin,
    Member,
}

impl GroupMemberRole {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConversationMemberRoleRequest {
    pub role: GroupMemberRole,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RespondToDirectMessageRequest {
    pub accept: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendSummary {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub is_online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub friendship_id: Option<Uuid>,
}

#[derive(Debug, Queryable, Selectable, Serialize)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[serde(rename_all = "camelCase")]
pub struct UserDiscovery {
    pub id: Uuid,
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UserStatus {
    Available,
    Away,
    DoNotDisturb,
    Offline,
}

impl UserStatus {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Away => "away",
            Self::DoNotDisturb => "do_not_disturb",
            Self::Offline => "offline",
        }
    }

    pub fn from_db_value(value: &str) -> Option<Self> {
        match value {
            "available" => Some(Self::Available),
            "away" => Some(Self::Away),
            "do_not_disturb" => Some(Self::DoNotDisturb),
            "offline" => Some(Self::Offline),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub id: Uuid,
    pub name: String,
    pub nickname: String,
    pub email: String,
    pub avatar_url: Option<String>,
    pub status: UserStatus,
    pub status_message: String,
    pub created_at: NaiveDateTime,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub is_online: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSelfProfileRequest {
    pub name: Option<String>,
    pub nickname: Option<String>,
    pub status: Option<UserStatus>,
    pub status_message: Option<String>,
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
pub struct NotificationResponse {
    pub id: Uuid,
    pub kind: String,
    pub actor_id: Uuid,
    pub actor_name: String,
    pub data: serde_json::Value,
    pub created_at: DateTime<Utc>,
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
pub struct CallSessionStartResponse {
    pub id: Uuid,
    pub room_id: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingCallSessionResponse {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub initiator_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSessionJoinResponse {
    pub room_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSessionResponse {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingHistoryResponse {
    pub id: Uuid,
    pub room_id: String,
    pub last_joined_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationResponse {
    pub id: Uuid,
    pub kind: ConversationKind,
    pub title: Option<String>,
    pub access_policy: Option<GroupAccessPolicy>,
    pub role: Option<String>,
    pub other_user_id: Option<Uuid>,
    pub message_count: i64,
    pub unread_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInfoResponse {
    pub id: Uuid,
    pub title: String,
    pub access_policy: GroupAccessPolicy,
    pub member_count: i64,
    pub is_member: bool,
    pub role: Option<String>,
    pub can_join: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupMemberResponse {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessageResponse {
    pub sequence: i64,
    pub conversation_id: Uuid,
    pub sender_id: Uuid,
    pub sender_name: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessagesResponse {
    pub messages: Vec<ConversationMessageResponse>,
    pub next_before: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDirectConversationResponse {
    pub state: String,
    pub conversation: Option<ConversationResponse>,
    pub request_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectMessageRequestResponse {
    pub id: Uuid,
    pub requester_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::UserStatus;

    #[test]
    fn validates_persisted_user_statuses() {
        assert_eq!(
            UserStatus::from_db_value("available"),
            Some(UserStatus::Available)
        );
        assert_eq!(UserStatus::from_db_value("away"), Some(UserStatus::Away));
        assert_eq!(
            UserStatus::from_db_value("do_not_disturb"),
            Some(UserStatus::DoNotDisturb)
        );
        assert_eq!(
            UserStatus::from_db_value("offline"),
            Some(UserStatus::Offline)
        );
        assert_eq!(UserStatus::from_db_value("busy"), None);
        assert_eq!(UserStatus::from_db_value("doNotDisturb"), None);
    }

    #[test]
    fn serializes_user_statuses_for_profile_api() {
        assert_eq!(
            serde_json::to_value(UserStatus::DoNotDisturb).unwrap(),
            "doNotDisturb"
        );
    }
}
