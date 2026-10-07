use std::collections::HashSet;

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{delete, get, post},
};
use chrono::Utc;
use diesel::{
    deserialize::QueryableByName,
    dsl::{exists, not},
    prelude::*,
    sql_query,
    sql_types::{Array, BigInt, Bool, Text, Uuid as SqlUuid},
};
use diesel_async::{AsyncConnection, RunQueryDsl, scoped_futures::ScopedFutureExt};
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    schema::{
        call_session_members, call_sessions, conversation_hidden_states, conversation_members,
        conversations, direct_message_requests, friendships, revoked_sfu_rooms, users,
    },
    social::{
        SocialResource,
        avatars::{AvatarQuery, avatar_response, delete_avatar, store_avatar, user_avatar_url},
        group_invitations,
        handlers::{
            MAX_AVATAR_UPLOAD_BYTES, parse_avatar_upload, user_search_pattern,
            validated_user_search_query,
        },
        messages::{
            create_message, list_messages, mark_conversation_read, mark_conversation_unread,
            toggle_message_reaction,
        },
        models::{
            AddConversationMemberRequest, Conversation, ConversationKind, ConversationMember,
            ConversationResponse, CreateGroupRequest, DirectMessageRequest,
            DirectMessageRequestResponse, GroupAccessPolicy, GroupCandidate, GroupCandidatesPage,
            GroupCandidatesQuery, GroupCodeInfoRequest, GroupInfoResponse, GroupMemberResponse,
            GroupMembersPage, JoinGroupByCodeRequest, JoinGroupRequest, NewConversation,
            NewConversationMember, NewDirectMessageRequest, NewRevokedSfuRoom,
            OpenDirectConversationResponse, PageQuery, RespondToDirectMessageRequest,
            UpdateConversationMemberRoleRequest, UpdateGroupPolicyRequest, UpdateGroupRequest,
            UserStatus,
        },
        presence,
    },
};

const GROUP_PAGE_SIZE: i64 = 50;

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;
type EmptyResult = Result<StatusCode, (StatusCode, String)>;

#[derive(Debug)]
pub(super) struct ConversationError {
    status: StatusCode,
    message: String,
}

impl ConversationError {
    pub(super) fn into_api_error(self) -> (StatusCode, String) {
        (self.status, self.message)
    }
}

impl From<(StatusCode, String)> for ConversationError {
    fn from((status, message): (StatusCode, String)) -> Self {
        Self { status, message }
    }
}

impl From<diesel::result::Error> for ConversationError {
    fn from(error: diesel::result::Error) -> Self {
        let (status, message) = internal_error(error);
        Self { status, message }
    }
}

pub fn conversation_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_conversations))
        .route("/groups", get(list_groups).post(create_group))
        .route("/groups/code/info", post(get_group_info_by_code))
        .route("/groups/code/join", post(join_group_by_code))
        .route("/groups/{id}/info", get(get_group_info))
        .route("/groups/{id}/join", post(join_group))
        .route("/groups/{id}/leave", post(leave_group))
        .route("/groups/{id}/policy", post(update_group_policy))
        .route(
            "/groups/{id}/avatar",
            get(get_group_avatar)
                .post(upload_group_avatar)
                .delete(remove_group_avatar)
                .layer(DefaultBodyLimit::max(MAX_AVATAR_UPLOAD_BYTES)),
        )
        .route(
            "/groups/{id}/members",
            get(list_group_members).post(add_group_member),
        )
        .route("/groups/{id}/candidates", get(list_group_candidates))
        .route(
            "/groups/{id}/invitations",
            post(group_invitations::create_group_invitation),
        )
        .route(
            "/groups/invitations",
            get(group_invitations::list_group_invitations),
        )
        .route(
            "/groups/invitations/{id}/accept",
            post(group_invitations::accept_group_invitation),
        )
        .route(
            "/groups/invitations/{id}/decline",
            post(group_invitations::decline_group_invitation),
        )
        .route(
            "/groups/{id}/members/{user_id}",
            delete(remove_group_member),
        )
        .route(
            "/groups/{id}/members/{user_id}/role",
            post(update_group_member_role),
        )
        .route("/groups/{id}", delete(delete_group).patch(update_group))
        .route("/direct/requests", get(list_direct_message_requests))
        .route("/direct/{user_id}", post(open_direct_conversation))
        .route(
            "/direct/requests/{id}/respond",
            post(respond_to_direct_message_request),
        )
        .route("/{id}/messages", get(list_messages).post(create_message))
        .route(
            "/{id}/messages/{sequence}/reactions",
            post(toggle_message_reaction),
        )
        .route("/{id}/read", post(mark_conversation_read))
        .route("/{id}/unread", post(mark_conversation_unread))
        .route("/{id}", delete(hide_direct_conversation))
}

async fn list_conversations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<ConversationResponse>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    let memberships: Vec<ConversationMember> = conversation_members::table
        .filter(conversation_members::user_id.eq(user_id))
        .select(ConversationMember::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let member_roles = memberships
        .iter()
        .map(|member| (member.conversation_id, member.role.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let group_ids = memberships
        .iter()
        .map(|member| member.conversation_id)
        .collect::<Vec<_>>();

    let mut groups: Vec<Conversation> = conversations::table
        .filter(conversations::kind.eq("group"))
        .filter(conversations::id.eq_any(group_ids))
        .select(Conversation::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let mut direct: Vec<Conversation> = conversations::table
        .filter(conversations::kind.eq("direct"))
        .filter(not(exists(
            conversation_hidden_states::table
                .filter(conversation_hidden_states::conversation_id.eq(conversations::id))
                .filter(conversation_hidden_states::user_id.eq(user_id)),
        )))
        .filter(
            conversations::direct_user_low_id
                .eq(user_id)
                .or(conversations::direct_user_high_id.eq(user_id)),
        )
        .select(Conversation::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    groups.append(&mut direct);
    let mut response = groups
        .iter()
        .map(|conversation| {
            conversation_response(
                conversation,
                user_id,
                member_roles.get(&conversation.id).cloned(),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let message_counts = message_counts_for_conversations(
        &mut conn,
        user_id,
        &response
            .iter()
            .map(|conversation| conversation.id)
            .collect::<Vec<_>>(),
    )
    .await?;
    for conversation in &mut response {
        let counts = message_counts.get(&conversation.id);
        conversation.message_count = counts.map(|counts| counts.message_count).unwrap_or(0);
        conversation.unread_count = counts.map(|counts| counts.unread_count).unwrap_or(0);
        conversation.marked_unread = counts.is_some_and(|counts| counts.marked_unread);
    }
    response.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

    Ok(Json(response))
}

async fn list_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<ConversationResponse>> {
    let Json(mut conversations) = list_conversations(State(state), headers).await?;
    conversations.retain(|conversation| matches!(conversation.kind, ConversationKind::Group));
    Ok(Json(conversations))
}

async fn create_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateGroupRequest>,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let title = validate_group_title(&request.title)?;
    validate_password_for_policy(request.access_policy, request.password.as_deref())?;
    let member_ids = validate_initial_member_ids(user_id, request.member_ids)?;
    let password_hash = hash_group_password(request.password).await?;
    let group_id = Uuid::new_v4();
    let group_code = group_code_from_uuid(group_id);
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let initial_member_ids = member_ids.clone();
    let access_policy = request.access_policy;
    let conversation: Conversation = conn
        .transaction(|conn| {
            async move {
                validate_initial_member_friendships(conn, user_id, &member_ids, access_policy)
                    .await?;

                let conversation: Conversation = diesel::insert_into(conversations::table)
                    .values(NewConversation {
                        id: group_id,
                        kind: "group".to_string(),
                        creator_id: user_id,
                        title: Some(title),
                        access_policy: Some(access_policy.as_db_value().to_string()),
                        password_hash,
                        direct_user_low_id: None,
                        direct_user_high_id: None,
                        group_code: Some(group_code),
                        legacy_group_code: None,
                        avatar_key: None,
                    })
                    .returning(Conversation::as_returning())
                    .get_result(conn)
                    .await?;
                let mut members = Vec::with_capacity(member_ids.len() + 1);
                members.push(NewConversationMember {
                    conversation_id: conversation.id,
                    user_id,
                    role: "creator".to_string(),
                });
                members.extend(
                    member_ids
                        .into_iter()
                        .map(|member_id| NewConversationMember {
                            conversation_id: conversation.id,
                            user_id: member_id,
                            role: "member".to_string(),
                        }),
                );
                diesel::insert_into(conversation_members::table)
                    .values(&members)
                    .execute(conn)
                    .await?;

                Ok(conversation)
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;

    let mut recipients = initial_member_ids;
    recipients.push(user_id);
    state
        .social_events
        .publish(recipients, SocialResource::Conversations);

    Ok(Json(conversation_response(
        &conversation,
        user_id,
        Some("creator".to_string()),
    )?))
}

async fn get_group_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
) -> ApiResult<GroupInfoResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let group = load_group(&mut conn, group_id).await?;
    Ok(Json(group_info_response(&mut conn, group, user_id).await?))
}

async fn get_group_info_by_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<GroupCodeInfoRequest>,
) -> ApiResult<GroupInfoResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let group = load_group_by_code(&mut conn, &request.group_code).await?;
    Ok(Json(group_info_response(&mut conn, group, user_id).await?))
}

async fn group_info_response(
    conn: &mut diesel_async::AsyncPgConnection,
    group: Conversation,
    user_id: Uuid,
) -> Result<GroupInfoResponse, (StatusCode, String)> {
    let role = group_member_role(conn, group.id, user_id).await?;
    let policy = group_policy(&group)?;

    if !can_view_group_info(policy, role.is_some()) {
        return Err((StatusCode::NOT_FOUND, "Group not found".to_string()));
    }

    let member_count = conversation_members::table
        .filter(conversation_members::conversation_id.eq(group.id))
        .count()
        .get_result(conn)
        .await
        .map_err(internal_error)?;
    let avatar_url = group_avatar_url(&group);
    let title = group
        .title
        .ok_or_else(|| internal_error("group is missing a title"))?;
    let group_code = group
        .group_code
        .ok_or_else(|| internal_error("group is missing a code"))?;
    let is_member = role.is_some();

    Ok(GroupInfoResponse {
        id: group.id,
        title,
        group_code,
        avatar_url,
        access_policy: policy,
        member_count,
        is_member,
        role,
        can_join: can_join_group(is_member),
        created_at: group.created_at,
    })
}

async fn list_group_members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Query(page): Query<PageQuery>,
) -> ApiResult<GroupMembersPage> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let offset = validated_page_offset(page.offset)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    load_group(&mut conn, group_id).await?;

    if group_member_role(&mut conn, group_id, user_id)
        .await?
        .is_none()
    {
        return Err((StatusCode::NOT_FOUND, "Group not found".to_string()));
    }

    let mut members: Vec<(
        Uuid,
        String,
        String,
        Option<String>,
        String,
        String,
        chrono::DateTime<Utc>,
    )> = conversation_members::table
        .inner_join(users::table.on(users::id.eq(conversation_members::user_id)))
        .filter(conversation_members::conversation_id.eq(group_id))
        .order((conversation_members::joined_at.asc(), users::id.asc()))
        .select((
            users::id,
            users::name,
            users::nickname,
            users::avatar_key,
            users::status,
            conversation_members::role,
            conversation_members::joined_at,
        ))
        .offset(offset)
        .limit(GROUP_PAGE_SIZE + 1)
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let next_offset = next_page_offset(&mut members, offset);
    let member_statuses = members
        .iter()
        .filter_map(|member| UserStatus::from_db_value(&member.4).map(|status| (member.0, status)))
        .collect::<Vec<_>>();
    let presence_by_member =
        presence::visible_group_member_presence(&mut conn, user_id, &member_statuses)
            .await
            .map_err(internal_error)?;

    Ok(Json(GroupMembersPage {
        members: members
            .into_iter()
            .map(
                |(id, name, nickname, avatar_key, _, role, joined_at)| GroupMemberResponse {
                    id,
                    name,
                    nickname,
                    avatar_url: avatar_key.map(|key| user_avatar_url(id, &key)),
                    is_online: presence_by_member
                        .get(&id)
                        .is_some_and(|presence| presence.is_online),
                    status: presence_by_member.get(&id).map(|presence| presence.status),
                    role,
                    joined_at,
                },
            )
            .collect(),
        next_offset,
    }))
}

/// Lists people an admin may add to the group, filtered by the group's access policy.
async fn list_group_candidates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Query(request): Query<GroupCandidatesQuery>,
) -> ApiResult<GroupCandidatesPage> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let offset = validated_page_offset(request.offset)?;
    let query = validated_user_search_query(&request.query).ok_or((
        StatusCode::BAD_REQUEST,
        "Query must be 2 to 80 characters".to_string(),
    ))?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let group = require_group_admin(&mut conn, group_id, user_id).await?;
    let policy = group_policy(&group)?;

    let mut candidates: Vec<GroupCandidateRow> = sql_query(
        "SELECT candidate.id, candidate.name, candidate.nickname
         FROM users AS candidate
         WHERE candidate.id <> $1
           AND NOT EXISTS (
               SELECT 1 FROM conversation_members AS member
               WHERE member.conversation_id = $2 AND member.user_id = candidate.id
           )
           AND (candidate.name ILIKE $3 OR candidate.email ILIKE $3 OR candidate.nickname ILIKE $3)
           AND CASE $4
               WHEN 'friends_of_friends' THEN EXISTS (
                   SELECT 1
                   FROM conversation_members AS member
                   JOIN friendships AS friendship
                     ON friendship.status = 'accepted'
                    AND ((friendship.requester_id = member.user_id AND friendship.addressee_id = candidate.id)
                      OR (friendship.addressee_id = member.user_id AND friendship.requester_id = candidate.id))
                   WHERE member.conversation_id = $2
               )
               WHEN 'friends_only' THEN NOT EXISTS (
                   SELECT 1
                   FROM conversation_members AS member
                   WHERE member.conversation_id = $2
                     AND NOT EXISTS (
                         SELECT 1 FROM friendships AS friendship
                         WHERE friendship.status = 'accepted'
                           AND ((friendship.requester_id = member.user_id AND friendship.addressee_id = candidate.id)
                             OR (friendship.addressee_id = member.user_id AND friendship.requester_id = candidate.id))
                     )
               )
               ELSE TRUE
           END
         ORDER BY candidate.name, candidate.email, candidate.id
         OFFSET $5
         LIMIT $6",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<SqlUuid, _>(group_id)
    .bind::<Text, _>(user_search_pattern(&query))
    .bind::<Text, _>(policy.as_db_value())
    .bind::<BigInt, _>(offset)
    .bind::<BigInt, _>(GROUP_PAGE_SIZE + 1)
    .load(&mut conn)
    .await
    .map_err(internal_error)?;
    let next_offset = next_page_offset(&mut candidates, offset);

    Ok(Json(GroupCandidatesPage {
        results: candidates
            .into_iter()
            .map(|candidate| GroupCandidate {
                id: candidate.id,
                name: candidate.name,
                nickname: candidate.nickname,
            })
            .collect(),
        next_offset,
    }))
}

#[derive(QueryableByName)]
struct GroupCandidateRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Text)]
    nickname: String,
}

fn validated_page_offset(offset: Option<i64>) -> Result<i64, (StatusCode, String)> {
    match offset.unwrap_or(0) {
        offset if offset >= 0 => Ok(offset),
        _ => Err((
            StatusCode::BAD_REQUEST,
            "Offset must not be negative".to_string(),
        )),
    }
}

/// Trims the extra look-ahead row and returns the offset of the next page, if one exists.
fn next_page_offset<T>(rows: &mut Vec<T>, offset: i64) -> Option<i64> {
    let has_more = rows.len() as i64 > GROUP_PAGE_SIZE;
    rows.truncate(GROUP_PAGE_SIZE as usize);
    has_more.then_some(offset + GROUP_PAGE_SIZE)
}

async fn join_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Json(request): Json<JoinGroupRequest>,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    join_group_impl(&state, user_id, group_id, request).await
}

async fn join_group_by_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<JoinGroupByCodeRequest>,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let group_id = load_group_by_code(&mut conn, &request.group_code).await?.id;
    drop(conn);
    join_group_impl(
        &state,
        user_id,
        group_id,
        JoinGroupRequest {
            password: request.password,
        },
    )
    .await
}

async fn join_group_impl(
    state: &AppState,
    user_id: Uuid,
    group_id: Uuid,
    request: JoinGroupRequest,
) -> ApiResult<ConversationResponse> {
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let (group, role): (Conversation, String) = conn
        .transaction(|conn| {
            async move {
                let group = load_group_for_update(conn, group_id).await?;

                if let Some(role) = group_member_role(conn, group_id, user_id).await? {
                    return Ok((group, role));
                }
                authorize_group_admission(conn, &group, user_id, request.password).await?;
                add_member(conn, group_id, user_id).await?;

                Ok((group, "member".to_string()))
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;

    publish_group_update(state, &mut conn, group_id, &[]).await;

    Ok(Json(conversation_response(&group, user_id, Some(role))?))
}

async fn update_group_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Json(request): Json<UpdateGroupPolicyRequest>,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    update_group_impl(
        &state,
        user_id,
        group_id,
        UpdateGroupRequest {
            title: None,
            access_policy: Some(request.access_policy),
            password: request.password,
        },
    )
    .await
}

async fn update_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Json(request): Json<UpdateGroupRequest>,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    update_group_impl(&state, user_id, group_id, request).await
}

async fn update_group_impl(
    state: &AppState,
    user_id: Uuid,
    group_id: Uuid,
    request: UpdateGroupRequest,
) -> ApiResult<ConversationResponse> {
    if request.title.is_none() && request.access_policy.is_none() && request.password.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "At least one group field must be provided".to_string(),
        ));
    }
    let title = request
        .title
        .as_deref()
        .map(validate_group_title)
        .transpose()?;
    if request.password.is_some() {
        validate_password_for_policy(GroupAccessPolicy::Password, request.password.as_deref())?;
    }
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let group = require_group_admin(&mut conn, group_id, user_id).await?;
    validate_group_update_request(
        group_policy(&group)?,
        request.access_policy,
        request.password.is_some(),
    )?;
    drop(conn);

    let password_hash = hash_group_password(request.password.clone()).await?;
    let requested_policy = request.access_policy;
    let password_supplied = request.password.is_some();
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let (updated, role): (Conversation, String) = conn
        .transaction(|conn| {
            async move {
                let (group, role) = require_group_admin_for_update(conn, group_id, user_id).await?;
                let current_policy = group_policy(&group)?;
                let access_policy = validate_group_update_request(
                    current_policy,
                    requested_policy,
                    password_supplied,
                )?;
                let title = title.unwrap_or_else(|| {
                    group
                        .title
                        .expect("persisted groups are constrained to have titles")
                });
                let password_hash = if access_policy == GroupAccessPolicy::Password {
                    password_hash.or(group.password_hash)
                } else {
                    None
                };
                let updated: Conversation = diesel::update(conversations::table.find(group_id))
                    .set((
                        conversations::title.eq(title),
                        conversations::access_policy.eq(access_policy.as_db_value()),
                        conversations::password_hash.eq(password_hash),
                        conversations::updated_at.eq(Utc::now()),
                    ))
                    .returning(Conversation::as_returning())
                    .get_result(conn)
                    .await?;

                Ok((updated, role))
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;

    publish_group_update(state, &mut conn, group_id, &[]).await;

    Ok(Json(conversation_response(&updated, user_id, Some(role))?))
}

async fn add_group_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Json(request): Json<AddConversationMemberRequest>,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let (group, role): (Conversation, String) = conn
        .transaction(|conn| {
            async move {
                let (group, role) = require_group_admin_for_update(conn, group_id, user_id).await?;
                ensure_user_exists(conn, request.user_id).await?;

                if group_member_role(conn, group_id, request.user_id)
                    .await?
                    .is_none()
                {
                    authorize_admin_addition(conn, &group, request.user_id).await?;
                    add_member(conn, group_id, request.user_id).await?;
                }

                Ok((group, role))
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;

    publish_group_update(&state, &mut conn, group_id, &[]).await;

    Ok(Json(conversation_response(&group, user_id, Some(role))?))
}

async fn remove_group_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((group_id, member_id)): Path<(Uuid, Uuid)>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let room_ids = conn
        .transaction(|conn| {
            async move {
                let (group, _) = require_group_admin_for_update(conn, group_id, user_id).await?;
                if member_id == group.creator_id {
                    return Err((
                        StatusCode::FORBIDDEN,
                        "The group creator cannot be removed".to_string(),
                    )
                        .into());
                }
                let deleted = diesel::delete(
                    conversation_members::table
                        .filter(conversation_members::conversation_id.eq(group_id))
                        .filter(conversation_members::user_id.eq(member_id)),
                )
                .execute(conn)
                .await?;
                if deleted == 0 {
                    return Err(
                        (StatusCode::NOT_FOUND, "Group member not found".to_string()).into(),
                    );
                }
                let room_ids = revoke_group_call_memberships(conn, group_id, member_id).await?;
                diesel::update(conversations::table.find(group_id))
                    .set(conversations::updated_at.eq(Utc::now()))
                    .execute(conn)
                    .await?;
                Ok(room_ids)
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;
    publish_group_update(&state, &mut conn, group_id, &[member_id]).await;
    state
        .social_events
        .publish([member_id], SocialResource::Calls);
    disconnect_user_from_sfu_rooms(&state, &room_ids, member_id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn leave_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let room_ids = conn
        .transaction(|conn| {
            async move {
                let group = load_group_for_update(conn, group_id).await?;
                if group.creator_id == user_id {
                    return Err((
                        StatusCode::CONFLICT,
                        "Transfer group ownership before leaving".to_string(),
                    )
                        .into());
                }
                let deleted = diesel::delete(
                    conversation_members::table
                        .filter(conversation_members::conversation_id.eq(group_id))
                        .filter(conversation_members::user_id.eq(user_id)),
                )
                .execute(conn)
                .await?;
                if deleted == 0 {
                    return Err((
                        StatusCode::NOT_FOUND,
                        "Group membership not found".to_string(),
                    )
                        .into());
                }
                let room_ids = revoke_group_call_memberships(conn, group_id, user_id).await?;
                touch_group(conn, group_id).await?;
                Ok(room_ids)
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;
    publish_group_update(&state, &mut conn, group_id, &[user_id]).await;
    state
        .social_events
        .publish([user_id], SocialResource::Calls);
    disconnect_user_from_sfu_rooms(&state, &room_ids, user_id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let deletion: Result<(Vec<Uuid>, Vec<Uuid>, Vec<String>, Option<String>), ConversationError> =
        conn.transaction(|conn| {
            async move {
                let (group, role) = require_group_admin_for_update(conn, group_id, user_id).await?;
                if !can_delete_group(&role) {
                    return Err((
                        StatusCode::FORBIDDEN,
                        "Group admin access is required".to_string(),
                    )
                        .into());
                }
                let member_ids = group_member_ids(conn, group_id).await?;
                let call_recipients = call_session_members::table
                    .inner_join(call_sessions::table)
                    .filter(call_sessions::conversation_id.eq(group_id))
                    .select(call_session_members::user_id)
                    .distinct()
                    .load::<Uuid>(conn)
                    .await?;
                let sfu_room_ids = call_sessions::table
                    .filter(call_sessions::conversation_id.eq(group_id))
                    .select(call_sessions::sfu_room_id)
                    .load::<String>(conn)
                    .await?;
                let tombstones = sfu_room_ids
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
                diesel::delete(conversations::table.find(group_id))
                    .execute(conn)
                    .await?;
                Ok((member_ids, call_recipients, sfu_room_ids, group.avatar_key))
            }
            .scope_boxed()
        })
        .await;
    let (member_ids, call_recipients, sfu_room_ids, avatar_key) =
        deletion.map_err(ConversationError::into_api_error)?;

    revoke_and_close_sfu_rooms(&state, &sfu_room_ids).await;
    state
        .social_events
        .publish(member_ids, SocialResource::Conversations);
    state
        .social_events
        .publish(call_recipients, SocialResource::Calls);
    if let Some(avatar_key) = avatar_key
        && let Err(error) = delete_avatar(state.avatar_storage.as_ref(), &avatar_key).await
    {
        tracing::warn!(%error, %group_id, %avatar_key, "Failed to remove deleted group avatar");
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn get_group_avatar(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Query(query): Query<AvatarQuery>,
) -> Result<Response, (StatusCode, String)> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let group = load_group(&mut conn, group_id).await?;
    let is_member = group_member_role(&mut conn, group_id, user_id)
        .await?
        .is_some();
    if !can_view_group_info(group_policy(&group)?, is_member) {
        return Err((StatusCode::NOT_FOUND, "Group not found".to_string()));
    }
    let avatar_key = group
        .avatar_key
        .ok_or((StatusCode::NOT_FOUND, "Avatar not found".to_string()))?;
    avatar_response(
        state.avatar_storage.as_ref(),
        &headers,
        &avatar_key,
        query.size,
    )
    .await
}

async fn remove_group_avatar(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let avatar_key = conn
        .transaction(|conn| {
            async move {
                let (group, _) = require_group_admin_for_update(conn, group_id, user_id).await?;
                let Some(avatar_key) = group.avatar_key else {
                    return Ok(None);
                };
                diesel::update(conversations::table.find(group_id))
                    .set((
                        conversations::avatar_key.eq(None::<String>),
                        conversations::updated_at.eq(Utc::now()),
                    ))
                    .execute(conn)
                    .await?;
                Ok(Some(avatar_key))
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;
    if let Some(avatar_key) = avatar_key {
        if let Err(error) = delete_avatar(state.avatar_storage.as_ref(), &avatar_key).await {
            tracing::warn!(%error, %group_id, %avatar_key, "Failed to remove deleted group avatar");
        }
        publish_group_update(&state, &mut conn, group_id, &[]).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn upload_group_avatar(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    multipart: Multipart,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    require_group_admin(&mut conn, group_id, user_id).await?;
    drop(conn);

    let avatar = parse_avatar_upload(multipart).await?;
    let key = format!(
        "group-avatars/{group_id}/{}.{}",
        Uuid::new_v4(),
        avatar.extension
    );
    store_avatar(
        state.avatar_storage.as_ref(),
        &key,
        &avatar.content_type,
        avatar.bytes,
    )
    .await?;

    let mut conn = match state.pool.get().await {
        Ok(conn) => conn,
        Err(error) => {
            if let Err(delete_error) = delete_avatar(state.avatar_storage.as_ref(), &key).await {
                tracing::error!(%delete_error, avatar_key = %key, "Failed to remove orphaned group avatar");
            }
            return Err(internal_error(error));
        }
    };
    let stored_key = key.clone();
    let update_result: Result<(Conversation, String, Option<String>), ConversationError> = conn
        .transaction(|conn| {
            async move {
                let (group, role) = require_group_admin_for_update(conn, group_id, user_id).await?;
                let previous_avatar_key = group.avatar_key;
                let updated: Conversation = diesel::update(conversations::table.find(group_id))
                    .set((
                        conversations::avatar_key.eq(Some(&stored_key)),
                        conversations::updated_at.eq(Utc::now()),
                    ))
                    .returning(Conversation::as_returning())
                    .get_result(conn)
                    .await?;
                Ok((updated, role, previous_avatar_key))
            }
            .scope_boxed()
        })
        .await;
    let (updated, role, previous_avatar_key) = match update_result {
        Ok(updated) => updated,
        Err(error) => {
            if let Err(delete_error) = delete_avatar(state.avatar_storage.as_ref(), &key).await {
                tracing::error!(%delete_error, avatar_key = %key, "Failed to remove orphaned group avatar");
            }
            return Err(error.into_api_error());
        }
    };

    if let Some(previous_key) = previous_avatar_key
        .as_deref()
        .filter(|previous| *previous != key.as_str())
        && let Err(error) = delete_avatar(state.avatar_storage.as_ref(), previous_key).await
    {
        tracing::warn!(%error, avatar_key = %previous_key, "Failed to remove replaced group avatar");
    }
    publish_group_update(&state, &mut conn, group_id, &[]).await;

    Ok(Json(conversation_response(&updated, user_id, Some(role))?))
}

async fn hide_direct_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let conversation: Conversation = conversations::table
        .find(conversation_id)
        .select(Conversation::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Conversation not found".to_string()))?;

    if conversation.kind != "direct"
        || (conversation.direct_user_low_id != Some(user_id)
            && conversation.direct_user_high_id != Some(user_id))
    {
        return Err((StatusCode::NOT_FOUND, "Conversation not found".to_string()));
    }

    diesel::insert_into(conversation_hidden_states::table)
        .values((
            conversation_hidden_states::conversation_id.eq(conversation_id),
            conversation_hidden_states::user_id.eq(user_id),
        ))
        .on_conflict((
            conversation_hidden_states::conversation_id,
            conversation_hidden_states::user_id,
        ))
        .do_nothing()
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;
    state
        .social_events
        .publish([user_id], SocialResource::Conversations);
    Ok(StatusCode::NO_CONTENT)
}

async fn update_group_member_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((group_id, member_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateConversationMemberRoleRequest>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    conn.transaction(|conn| {
        async move {
            let group = load_group_for_update(conn, group_id).await?;
            if group.creator_id != user_id {
                return Err((
                    StatusCode::FORBIDDEN,
                    "Only the group creator can change admin roles".to_string(),
                )
                    .into());
            }
            if member_id == group.creator_id {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "The group creator role cannot be changed".to_string(),
                )
                    .into());
            }
            let updated = diesel::update(
                conversation_members::table
                    .filter(conversation_members::conversation_id.eq(group_id))
                    .filter(conversation_members::user_id.eq(member_id)),
            )
            .set(conversation_members::role.eq(request.role.as_db_value()))
            .execute(conn)
            .await?;
            if updated == 0 {
                return Err((StatusCode::NOT_FOUND, "Group member not found".to_string()).into());
            }
            touch_group(conn, group_id).await?;
            Ok(())
        }
        .scope_boxed()
    })
    .await
    .map_err(ConversationError::into_api_error)?;
    publish_group_update(&state, &mut conn, group_id, &[]).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn open_direct_conversation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_id): Path<Uuid>,
) -> ApiResult<OpenDirectConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    if target_id == user_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "Cannot open a direct conversation with yourself".to_string(),
        ));
    }
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    ensure_user_exists(&mut conn, target_id).await?;

    if let Some(conversation) = find_direct_conversation(&mut conn, user_id, target_id).await? {
        diesel::delete(
            conversation_hidden_states::table
                .filter(conversation_hidden_states::conversation_id.eq(conversation.id))
                .filter(conversation_hidden_states::user_id.eq(user_id)),
        )
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;
        state
            .social_events
            .publish([user_id], SocialResource::Conversations);
        return Ok(Json(OpenDirectConversationResponse {
            state: "available".to_string(),
            conversation: Some(conversation_response(&conversation, user_id, None)?),
            request_id: None,
        }));
    }

    if are_accepted_friends(&mut conn, user_id, target_id).await? {
        let conversation = conn
            .transaction(|conn| {
                async move {
                    let conversation = ensure_direct_conversation(conn, user_id, target_id).await?;
                    resolve_pending_direct_request(conn, user_id, target_id).await?;
                    Ok(conversation)
                }
                .scope_boxed()
            })
            .await
            .map_err(ConversationError::into_api_error)?;
        diesel::delete(
            conversation_hidden_states::table
                .filter(conversation_hidden_states::conversation_id.eq(conversation.id))
                .filter(conversation_hidden_states::user_id.eq(user_id)),
        )
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;
        state
            .social_events
            .publish([user_id, target_id], SocialResource::Conversations);
        return Ok(Json(OpenDirectConversationResponse {
            state: "available".to_string(),
            conversation: Some(conversation_response(&conversation, user_id, None)?),
            request_id: None,
        }));
    }

    let existing: Option<DirectMessageRequest> = direct_message_requests::table
        .filter(
            direct_message_requests::requester_id
                .eq(user_id)
                .and(direct_message_requests::recipient_id.eq(target_id))
                .or(direct_message_requests::requester_id
                    .eq(target_id)
                    .and(direct_message_requests::recipient_id.eq(user_id))),
        )
        .select(DirectMessageRequest::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?;
    if let Some(request) = existing {
        if request.requester_id == user_id && request.status == "pending" {
            return Ok(Json(OpenDirectConversationResponse {
                state: "pending".to_string(),
                conversation: None,
                request_id: Some(request.id),
            }));
        }
        if request.status == "accepted" {
            let conversation = ensure_direct_conversation(&mut conn, user_id, target_id).await?;
            state
                .social_events
                .publish([user_id, target_id], SocialResource::Conversations);
            return Ok(Json(OpenDirectConversationResponse {
                state: "available".to_string(),
                conversation: Some(conversation_response(&conversation, user_id, None)?),
                request_id: Some(request.id),
            }));
        }
        return Err((
            StatusCode::CONFLICT,
            "A direct-message request already exists for these users".to_string(),
        ));
    }

    let request: DirectMessageRequest = diesel::insert_into(direct_message_requests::table)
        .values(NewDirectMessageRequest {
            requester_id: user_id,
            recipient_id: target_id,
        })
        .returning(DirectMessageRequest::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => (
                StatusCode::CONFLICT,
                "A direct-message request already exists for these users".to_string(),
            ),
            _ => internal_error(error),
        })?;

    state
        .social_events
        .publish([target_id], SocialResource::Conversations);

    Ok(Json(OpenDirectConversationResponse {
        state: "pending".to_string(),
        conversation: None,
        request_id: Some(request.id),
    }))
}

async fn list_direct_message_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<DirectMessageRequestResponse>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let requests: Vec<DirectMessageRequest> = direct_message_requests::table
        .filter(direct_message_requests::recipient_id.eq(user_id))
        .filter(direct_message_requests::status.eq("pending"))
        .order(direct_message_requests::created_at.desc())
        .select(DirectMessageRequest::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        requests
            .into_iter()
            .map(|request| DirectMessageRequestResponse {
                id: request.id,
                requester_id: request.requester_id,
                created_at: request.created_at,
            })
            .collect(),
    ))
}

async fn respond_to_direct_message_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
    Json(request): Json<RespondToDirectMessageRequest>,
) -> ApiResult<OpenDirectConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let (direct_request, conversation): (DirectMessageRequest, Option<Conversation>) = conn
        .transaction(|conn| {
            async move {
                let status = if request.accept {
                    "accepted"
                } else {
                    "declined"
                };
                let direct_request: DirectMessageRequest = diesel::update(
                    direct_message_requests::table
                        .filter(direct_message_requests::id.eq(request_id))
                        .filter(direct_message_requests::recipient_id.eq(user_id))
                        .filter(direct_message_requests::status.eq("pending")),
                )
                .set((
                    direct_message_requests::status.eq(status),
                    direct_message_requests::updated_at.eq(Utc::now()),
                ))
                .returning(DirectMessageRequest::as_returning())
                .get_result(conn)
                .await
                .map_err(|error| match error {
                    diesel::result::Error::NotFound => (
                        StatusCode::NOT_FOUND,
                        "Direct-message request is no longer available".to_string(),
                    ),
                    _ => internal_error(error),
                })?;

                let conversation = if request.accept {
                    Some(
                        ensure_direct_conversation(conn, user_id, direct_request.requester_id)
                            .await?,
                    )
                } else {
                    None
                };

                Ok((direct_request, conversation))
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;

    state.social_events.publish(
        [direct_request.requester_id, direct_request.recipient_id],
        SocialResource::Conversations,
    );

    if !request.accept {
        return Ok(Json(OpenDirectConversationResponse {
            state: "declined".to_string(),
            conversation: None,
            request_id: Some(direct_request.id),
        }));
    }

    let conversation =
        conversation.expect("accepted direct-message requests create a conversation");
    Ok(Json(OpenDirectConversationResponse {
        state: "available".to_string(),
        conversation: Some(conversation_response(&conversation, user_id, None)?),
        request_id: Some(direct_request.id),
    }))
}

async fn load_group(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
) -> Result<Conversation, (StatusCode, String)> {
    conversations::table
        .find(group_id)
        .filter(conversations::kind.eq("group"))
        .select(Conversation::as_select())
        .first(conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::NotFound => {
                (StatusCode::NOT_FOUND, "Group not found".to_string())
            }
            _ => internal_error(error),
        })
}

async fn load_group_by_code(
    conn: &mut diesel_async::AsyncPgConnection,
    code: &str,
) -> Result<Conversation, (StatusCode, String)> {
    let Some(code) = normalize_group_code(code) else {
        return Err((StatusCode::NOT_FOUND, "Group not found".to_string()));
    };
    conversations::table
        .filter(conversations::kind.eq("group"))
        .filter(
            conversations::group_code
                .eq(&code)
                .or(conversations::legacy_group_code.eq(&code)),
        )
        .select(Conversation::as_select())
        .first(conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::NotFound => {
                (StatusCode::NOT_FOUND, "Group not found".to_string())
            }
            _ => internal_error(error),
        })
}

pub(super) async fn load_group_for_update(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
) -> Result<Conversation, (StatusCode, String)> {
    conversations::table
        .find(group_id)
        .filter(conversations::kind.eq("group"))
        .for_update()
        .select(Conversation::as_select())
        .first(conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::NotFound => {
                (StatusCode::NOT_FOUND, "Group not found".to_string())
            }
            _ => internal_error(error),
        })
}

async fn require_group_admin(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
    user_id: Uuid,
) -> Result<Conversation, (StatusCode, String)> {
    let group = load_group(conn, group_id).await?;
    let role = group_member_role(conn, group_id, user_id).await?;
    if !matches!(role.as_deref(), Some("creator" | "admin")) {
        return Err((
            StatusCode::FORBIDDEN,
            "Group admin access is required".to_string(),
        ));
    }
    Ok(group)
}

pub(super) async fn require_group_admin_for_update(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
    user_id: Uuid,
) -> Result<(Conversation, String), (StatusCode, String)> {
    let group = load_group_for_update(conn, group_id).await?;
    let Some(role) = group_member_role(conn, group_id, user_id).await? else {
        return Err((
            StatusCode::FORBIDDEN,
            "Group admin access is required".to_string(),
        ));
    };
    if !matches!(role.as_str(), "creator" | "admin") {
        return Err((
            StatusCode::FORBIDDEN,
            "Group admin access is required".to_string(),
        ));
    }
    Ok((group, role))
}

pub(super) async fn group_member_role(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
    user_id: Uuid,
) -> Result<Option<String>, (StatusCode, String)> {
    conversation_members::table
        .filter(conversation_members::conversation_id.eq(group_id))
        .filter(conversation_members::user_id.eq(user_id))
        .select(conversation_members::role)
        .first(conn)
        .await
        .optional()
        .map_err(internal_error)
}

pub(super) async fn authorize_group_admission(
    conn: &mut diesel_async::AsyncPgConnection,
    group: &Conversation,
    candidate_id: Uuid,
    password: Option<String>,
) -> Result<(), (StatusCode, String)> {
    let policy = group_policy(group)?;
    match policy {
        GroupAccessPolicy::Open => Ok(()),
        GroupAccessPolicy::Password => {
            if verify_group_password(group.password_hash.clone(), password).await? {
                Ok(())
            } else {
                Err((StatusCode::FORBIDDEN, "Invalid group password".to_string()))
            }
        }
        GroupAccessPolicy::FriendsOnly | GroupAccessPolicy::FriendsOfFriends => {
            authorize_friendship_policy(conn, group, policy, candidate_id).await
        }
    }
}

async fn authorize_admin_addition(
    conn: &mut diesel_async::AsyncPgConnection,
    group: &Conversation,
    candidate_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    match group_policy(group)? {
        GroupAccessPolicy::Open => Ok(()),
        GroupAccessPolicy::Password => Err((
            StatusCode::CONFLICT,
            "Password-protected groups require an invitation".to_string(),
        )),
        policy => authorize_friendship_policy(conn, group, policy, candidate_id).await,
    }
}

async fn authorize_friendship_policy(
    conn: &mut diesel_async::AsyncPgConnection,
    group: &Conversation,
    policy: GroupAccessPolicy,
    candidate_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let (allowed, message) = match policy {
        GroupAccessPolicy::FriendsOnly => (
            is_friends_with_every_member(conn, group.id, candidate_id).await?,
            "Friends-only groups require friendship with every member",
        ),
        GroupAccessPolicy::FriendsOfFriends => (
            is_friends_with_any_member(conn, group.id, candidate_id).await?,
            "Friends-of-friends groups require friendship with a member",
        ),
        GroupAccessPolicy::Open | GroupAccessPolicy::Password => return Ok(()),
    };
    if allowed {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, message.to_string()))
    }
}

async fn is_friends_with_any_member(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
    candidate_id: Uuid,
) -> Result<bool, (StatusCode, String)> {
    let member_ids = conversation_members::table
        .filter(conversation_members::conversation_id.eq(group_id))
        .filter(conversation_members::user_id.ne(candidate_id))
        .select(conversation_members::user_id);
    diesel::select(exists(
        friendships::table
            .filter(friendships::status.eq("accepted"))
            .filter(
                friendships::requester_id
                    .eq(candidate_id)
                    .and(friendships::addressee_id.eq_any(member_ids.clone()))
                    .or(friendships::addressee_id
                        .eq(candidate_id)
                        .and(friendships::requester_id.eq_any(member_ids))),
            ),
    ))
    .get_result(conn)
    .await
    .map_err(internal_error)
}

async fn is_friends_with_every_member(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
    candidate_id: Uuid,
) -> Result<bool, (StatusCode, String)> {
    let member_ids: Vec<Uuid> = conversation_members::table
        .filter(conversation_members::conversation_id.eq(group_id))
        .select(conversation_members::user_id)
        .load(conn)
        .await
        .map_err(internal_error)?;
    for member_id in member_ids {
        if member_id != candidate_id && !are_accepted_friends(conn, candidate_id, member_id).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn group_member_ids(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
) -> Result<Vec<Uuid>, (StatusCode, String)> {
    conversation_members::table
        .filter(conversation_members::conversation_id.eq(group_id))
        .select(conversation_members::user_id)
        .load(conn)
        .await
        .map_err(internal_error)
}

async fn revoke_group_call_memberships(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<String>, ConversationError> {
    // Expired calls can retain active SFU peers until every participant leaves.
    let revocable_sessions = call_sessions::table
        .filter(call_sessions::conversation_id.eq(group_id))
        .filter(call_sessions::status.eq_any(["active", "expired"]));
    let room_ids = revocable_sessions
        .select(call_sessions::sfu_room_id)
        .load::<String>(conn)
        .await?;

    diesel::update(
        call_session_members::table
            .filter(call_session_members::user_id.eq(user_id))
            .filter(call_session_members::status.ne("declined"))
            .filter(
                call_session_members::call_session_id.eq_any(
                    call_sessions::table
                        .filter(call_sessions::conversation_id.eq(group_id))
                        .filter(call_sessions::status.eq_any(["active", "expired"]))
                        .select(call_sessions::id),
                ),
            ),
    )
    .set((
        call_session_members::status.eq("declined"),
        call_session_members::responded_at.eq(Some(Utc::now())),
    ))
    .execute(conn)
    .await?;

    Ok(room_ids)
}

async fn disconnect_user_from_sfu_rooms(state: &AppState, room_ids: &[String], user_id: Uuid) {
    for room_id in room_ids.iter().collect::<HashSet<_>>() {
        if let Err(error) = state.room_repo.deny_room_user(room_id, user_id).await {
            tracing::error!(%error, %room_id, %user_id, "Failed to deny SFU room user");
            continue;
        }
        let Some(room_lock) = state.room_repo.get_room(room_id).await else {
            continue;
        };
        let mut room = room_lock.write().await;
        let participant_ids = room
            .participants
            .iter()
            .filter(|(_, participant)| participant.participant.user_id == Some(user_id))
            .map(|(participant_id, _)| participant_id.clone())
            .collect::<Vec<_>>();
        for participant_id in participant_ids {
            room.remove_participant(&participant_id).await;
        }
    }
}

async fn revoke_and_close_sfu_rooms(state: &AppState, room_ids: &[String]) {
    for room_id in room_ids.iter().collect::<HashSet<_>>() {
        let room_lock = match state.room_repo.revoke_room(room_id).await {
            Ok(room_lock) => room_lock,
            Err(error) => {
                tracing::error!(%error, %room_id, "Failed to revoke in-memory SFU room");
                continue;
            }
        };
        let Some(room_lock) = room_lock else {
            continue;
        };

        let mut room = room_lock.write().await;
        let participant_ids = room.participants.keys().cloned().collect::<Vec<_>>();
        for participant_id in participant_ids {
            room.remove_participant(&participant_id).await;
        }
        drop(room);

        metrics::counter!("sfu_rooms_deleted_total").increment(1);
        metrics::gauge!("sfu_active_rooms").decrement(1.0);
    }
}

pub(super) async fn publish_group_update(
    state: &AppState,
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
    additional_recipients: &[Uuid],
) {
    match group_member_ids(conn, group_id).await {
        Ok(mut recipients) => {
            recipients.extend_from_slice(additional_recipients);
            state
                .social_events
                .publish(recipients, SocialResource::Conversations);
        }
        Err((_, error)) => {
            tracing::warn!(%error, %group_id, "Failed to publish group conversation update");
        }
    }
}

async fn are_accepted_friends(
    conn: &mut diesel_async::AsyncPgConnection,
    first_user_id: Uuid,
    second_user_id: Uuid,
) -> Result<bool, (StatusCode, String)> {
    friendships::table
        .filter(friendships::status.eq("accepted"))
        .filter(
            friendships::requester_id
                .eq(first_user_id)
                .and(friendships::addressee_id.eq(second_user_id))
                .or(friendships::requester_id
                    .eq(second_user_id)
                    .and(friendships::addressee_id.eq(first_user_id))),
        )
        .select(friendships::id)
        .first::<Uuid>(conn)
        .await
        .optional()
        .map(|friendship| friendship.is_some())
        .map_err(internal_error)
}

async fn validate_initial_member_friendships(
    conn: &mut diesel_async::AsyncPgConnection,
    creator_id: Uuid,
    member_ids: &[Uuid],
    access_policy: GroupAccessPolicy,
) -> Result<(), ConversationError> {
    let required_pairs = required_initial_friendship_pairs(creator_id, member_ids, access_policy);
    if required_pairs.is_empty() {
        return Ok(());
    }

    let mut participant_ids = member_ids.to_vec();
    participant_ids.push(creator_id);
    let accepted_pairs = friendships::table
        .filter(friendships::status.eq("accepted"))
        .filter(friendships::requester_id.eq_any(&participant_ids))
        .filter(friendships::addressee_id.eq_any(&participant_ids))
        .select((friendships::requester_id, friendships::addressee_id))
        .load::<(Uuid, Uuid)>(conn)
        .await?
        .into_iter()
        .map(|(first, second)| canonical_user_pair(first, second))
        .collect::<HashSet<_>>();

    if required_pairs
        .iter()
        .all(|pair| accepted_pairs.contains(pair))
    {
        return Ok(());
    }

    let message = if access_policy == GroupAccessPolicy::FriendsOnly {
        "Friends-only groups require friendship with every member"
    } else {
        "Initial group members must be accepted friends"
    };
    Err((StatusCode::FORBIDDEN, message.to_string()).into())
}

fn required_initial_friendship_pairs(
    creator_id: Uuid,
    member_ids: &[Uuid],
    access_policy: GroupAccessPolicy,
) -> HashSet<(Uuid, Uuid)> {
    let mut pairs = member_ids
        .iter()
        .map(|member_id| canonical_user_pair(creator_id, *member_id))
        .collect::<HashSet<_>>();
    if access_policy == GroupAccessPolicy::FriendsOnly {
        for (index, member_id) in member_ids.iter().enumerate() {
            pairs.extend(
                member_ids[index + 1..]
                    .iter()
                    .map(|other_id| canonical_user_pair(*member_id, *other_id)),
            );
        }
    }
    pairs
}

fn canonical_user_pair(first: Uuid, second: Uuid) -> (Uuid, Uuid) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

pub(super) async fn add_member(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    diesel::insert_into(conversation_members::table)
        .values(NewConversationMember {
            conversation_id: group_id,
            user_id,
            role: "member".to_string(),
        })
        .on_conflict((
            conversation_members::conversation_id,
            conversation_members::user_id,
        ))
        .do_nothing()
        .execute(conn)
        .await
        .map_err(internal_error)?;
    touch_group(conn, group_id).await
}

async fn touch_group(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    diesel::update(conversations::table.find(group_id))
        .set(conversations::updated_at.eq(Utc::now()))
        .execute(conn)
        .await
        .map_err(internal_error)?;
    Ok(())
}

pub(super) async fn ensure_user_exists(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    users::table
        .find(user_id)
        .select(users::id)
        .first::<Uuid>(conn)
        .await
        .map(|_| ())
        .map_err(|error| match error {
            diesel::result::Error::NotFound => {
                (StatusCode::NOT_FOUND, "User not found".to_string())
            }
            _ => internal_error(error),
        })
}

async fn ensure_direct_conversation(
    conn: &mut diesel_async::AsyncPgConnection,
    first_user_id: Uuid,
    second_user_id: Uuid,
) -> Result<Conversation, (StatusCode, String)> {
    let (low_id, high_id) = if first_user_id < second_user_id {
        (first_user_id, second_user_id)
    } else {
        (second_user_id, first_user_id)
    };

    diesel::insert_into(conversations::table)
        .values(NewConversation {
            id: Uuid::new_v4(),
            kind: "direct".to_string(),
            creator_id: first_user_id,
            title: None,
            access_policy: None,
            password_hash: None,
            direct_user_low_id: Some(low_id),
            direct_user_high_id: Some(high_id),
            group_code: None,
            legacy_group_code: None,
            avatar_key: None,
        })
        .on_conflict((
            conversations::direct_user_low_id,
            conversations::direct_user_high_id,
        ))
        .do_nothing()
        .execute(conn)
        .await
        .map_err(internal_error)?;

    // Direct access is derived from immutable pair columns, never membership rows.
    find_direct_conversation(conn, first_user_id, second_user_id)
        .await?
        .ok_or_else(|| internal_error("direct conversation was not created"))
}

async fn resolve_pending_direct_request(
    conn: &mut diesel_async::AsyncPgConnection,
    first_user_id: Uuid,
    second_user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    diesel::update(
        direct_message_requests::table
            .filter(direct_message_requests::status.eq("pending"))
            .filter(
                direct_message_requests::requester_id
                    .eq(first_user_id)
                    .and(direct_message_requests::recipient_id.eq(second_user_id))
                    .or(direct_message_requests::requester_id
                        .eq(second_user_id)
                        .and(direct_message_requests::recipient_id.eq(first_user_id))),
            ),
    )
    .set((
        direct_message_requests::status.eq("accepted"),
        direct_message_requests::updated_at.eq(Utc::now()),
    ))
    .execute(conn)
    .await
    .map_err(internal_error)?;
    Ok(())
}

async fn find_direct_conversation(
    conn: &mut diesel_async::AsyncPgConnection,
    first_user_id: Uuid,
    second_user_id: Uuid,
) -> Result<Option<Conversation>, (StatusCode, String)> {
    let (low_id, high_id) = if first_user_id < second_user_id {
        (first_user_id, second_user_id)
    } else {
        (second_user_id, first_user_id)
    };
    conversations::table
        .filter(conversations::kind.eq("direct"))
        .filter(conversations::direct_user_low_id.eq(low_id))
        .filter(conversations::direct_user_high_id.eq(high_id))
        .select(Conversation::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(internal_error)
}

pub(super) fn conversation_response(
    conversation: &Conversation,
    current_user_id: Uuid,
    role: Option<String>,
) -> Result<ConversationResponse, (StatusCode, String)> {
    let kind = ConversationKind::from_db_value(&conversation.kind)
        .ok_or_else(|| internal_error("invalid conversation kind"))?;
    let access_policy = match conversation.access_policy.as_deref() {
        Some(value) => Some(
            GroupAccessPolicy::from_db_value(value)
                .ok_or_else(|| internal_error("invalid group access policy"))?,
        ),
        None => None,
    };
    let other_user_id = match kind {
        ConversationKind::Group => None,
        ConversationKind::Direct => match (
            conversation.direct_user_low_id,
            conversation.direct_user_high_id,
        ) {
            (Some(low_id), Some(high_id)) if low_id == current_user_id => Some(high_id),
            (Some(low_id), Some(high_id)) if high_id == current_user_id => Some(low_id),
            _ => {
                return Err((
                    StatusCode::FORBIDDEN,
                    "Conversation access denied".to_string(),
                ));
            }
        },
    };
    Ok(ConversationResponse {
        id: conversation.id,
        kind,
        title: conversation.title.clone(),
        access_policy,
        group_code: conversation.group_code.clone(),
        avatar_url: group_avatar_url(conversation),
        role,
        other_user_id,
        message_count: 0,
        unread_count: 0,
        marked_unread: false,
        created_at: conversation.created_at,
        updated_at: conversation.updated_at,
    })
}

fn group_avatar_url(conversation: &Conversation) -> Option<String> {
    conversation
        .avatar_key
        .as_ref()
        .map(|_| group_avatar_url_with_revision(conversation.id, conversation.updated_at))
}

fn group_avatar_url_with_revision(group_id: Uuid, updated_at: chrono::DateTime<Utc>) -> String {
    format!(
        "/social/conversations/groups/{group_id}/avatar?v={}",
        updated_at.timestamp_micros()
    )
}

#[derive(QueryableByName)]
struct ConversationMessageCounts {
    #[diesel(sql_type = SqlUuid)]
    conversation_id: Uuid,
    #[diesel(sql_type = BigInt)]
    message_count: i64,
    #[diesel(sql_type = BigInt)]
    unread_count: i64,
    #[diesel(sql_type = Bool)]
    marked_unread: bool,
}

async fn message_counts_for_conversations(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    conversation_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, ConversationMessageCounts>, (StatusCode, String)> {
    if conversation_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let counts: Vec<ConversationMessageCounts> = sql_query(
        "SELECT messages.conversation_id,
                COUNT(*)::BIGINT AS message_count,
                COUNT(*) FILTER (
                    WHERE messages.sender_id <> $1
                      AND messages.sequence > COALESCE(reads.last_read_sequence, 0)
                )::BIGINT AS unread_count,
                BOOL_OR(COALESCE(reads.marked_unread, FALSE)) AS marked_unread
         FROM conversation_messages AS messages
         LEFT JOIN conversation_read_states AS reads
           ON reads.conversation_id = messages.conversation_id AND reads.user_id = $1
         WHERE messages.conversation_id = ANY($2)
         GROUP BY messages.conversation_id",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<Array<SqlUuid>, _>(conversation_ids.to_vec())
    .load(conn)
    .await
    .map_err(internal_error)?;

    Ok(counts
        .into_iter()
        .map(|count| (count.conversation_id, count))
        .collect())
}

pub(super) fn group_policy(
    group: &Conversation,
) -> Result<GroupAccessPolicy, (StatusCode, String)> {
    group
        .access_policy
        .as_deref()
        .and_then(GroupAccessPolicy::from_db_value)
        .ok_or_else(|| internal_error("invalid group access policy"))
}

fn group_code_from_uuid(uuid: Uuid) -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    const LENGTH: usize = 22;
    let mut remaining = *uuid.as_bytes();
    let mut code = [b'0'; LENGTH];

    for character in code.iter_mut().rev() {
        let mut remainder = 0u16;
        for byte in &mut remaining {
            let value = remainder * 256 + u16::from(*byte);
            *byte = (value / 62) as u8;
            remainder = value % 62;
        }
        *character = ALPHABET[remainder as usize];
    }

    String::from_utf8(code.to_vec()).expect("Base62 alphabet is valid UTF-8")
}

fn normalize_group_code(code: &str) -> Option<String> {
    let code = code.trim();
    if code.len() == 22 && code.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Some(code.to_string());
    }

    let legacy_code = code.to_ascii_lowercase();
    (legacy_code.len() <= 80
        && legacy_code
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphanumeric())))
    .then_some(legacy_code)
}

fn validate_initial_member_ids(
    creator_id: Uuid,
    member_ids: Vec<Uuid>,
) -> Result<Vec<Uuid>, (StatusCode, String)> {
    if member_ids.len() > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Groups may include at most 100 initial members".to_string(),
        ));
    }
    let mut unique_ids = HashSet::with_capacity(member_ids.len());
    for member_id in &member_ids {
        if *member_id == creator_id {
            return Err((
                StatusCode::BAD_REQUEST,
                "The group creator cannot be an initial member".to_string(),
            ));
        }
        if !unique_ids.insert(*member_id) {
            return Err((
                StatusCode::BAD_REQUEST,
                "Initial group members must be unique".to_string(),
            ));
        }
    }
    Ok(member_ids)
}

fn validate_group_update_request(
    current_policy: GroupAccessPolicy,
    requested_policy: Option<GroupAccessPolicy>,
    password_supplied: bool,
) -> Result<GroupAccessPolicy, (StatusCode, String)> {
    let policy = requested_policy.unwrap_or(current_policy);
    if policy == GroupAccessPolicy::Password {
        if current_policy != GroupAccessPolicy::Password && !password_supplied {
            return Err((
                StatusCode::BAD_REQUEST,
                "Group passwords must contain 8 to 256 characters".to_string(),
            ));
        }
    } else if password_supplied {
        return Err((
            StatusCode::BAD_REQUEST,
            "Only password groups may include a password".to_string(),
        ));
    }
    Ok(policy)
}

fn can_delete_group(role: &str) -> bool {
    matches!(role, "creator" | "admin")
}

fn can_view_group_info(policy: GroupAccessPolicy, is_member: bool) -> bool {
    is_member
        || matches!(
            policy,
            GroupAccessPolicy::Open | GroupAccessPolicy::Password
        )
}

fn can_join_group(is_member: bool) -> bool {
    !is_member
}

fn validate_group_title(title: &str) -> Result<String, (StatusCode, String)> {
    let title = title.trim();
    if title.is_empty() || title.len() > 128 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Group title must contain 1 to 128 characters".to_string(),
        ));
    }
    Ok(title.to_string())
}

fn validate_password_for_policy(
    policy: GroupAccessPolicy,
    password: Option<&str>,
) -> Result<(), (StatusCode, String)> {
    match (policy, password) {
        (GroupAccessPolicy::Password, Some(password)) if (8..=256).contains(&password.len()) => {
            Ok(())
        }
        (GroupAccessPolicy::Password, _) => Err((
            StatusCode::BAD_REQUEST,
            "Group passwords must contain 8 to 256 characters".to_string(),
        )),
        (_, None) => Ok(()),
        _ => Err((
            StatusCode::BAD_REQUEST,
            "Only password groups may include a password".to_string(),
        )),
    }
}

async fn hash_group_password(
    password: Option<String>,
) -> Result<Option<String>, (StatusCode, String)> {
    let Some(password) = password else {
        return Ok(None);
    };
    let password_hash = tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(internal_error)
    })
    .await
    .map_err(internal_error)??;
    Ok(Some(password_hash))
}

async fn verify_group_password(
    password_hash: Option<String>,
    password: Option<String>,
) -> Result<bool, (StatusCode, String)> {
    let (Some(password_hash), Some(password)) = (password_hash, password) else {
        return Ok(false);
    };
    tokio::task::spawn_blocking(move || {
        let parsed_hash = PasswordHash::new(&password_hash).map_err(internal_error)?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    })
    .await
    .map_err(internal_error)?
}

pub(super) fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!("Conversation API error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Request failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        GroupAccessPolicy, can_delete_group, can_join_group, can_view_group_info,
        group_avatar_url_with_revision, group_code_from_uuid, normalize_group_code,
        required_initial_friendship_pairs, validate_group_title, validate_group_update_request,
        validate_initial_member_ids, validate_password_for_policy,
    };
    use chrono::{Duration, TimeZone, Utc};
    use uuid::Uuid;

    #[test]
    fn generates_22_character_base62_group_ids() {
        let code = group_code_from_uuid(Uuid::new_v4());

        assert_eq!(code.len(), 22);
        assert!(code.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }

    #[test]
    fn encodes_every_uuid_bit_into_the_group_id() {
        let first = Uuid::parse_str("20000000-0000-4000-8000-000000000001").unwrap();
        let second = Uuid::parse_str("20000000-0000-4000-8000-000000000002").unwrap();

        assert_ne!(group_code_from_uuid(first), group_code_from_uuid(second));
    }

    #[test]
    fn group_ids_are_the_exact_base62_value_of_the_uuid() {
        // Exact 128-bit arithmetic is the reference the database function must also match.
        const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        for uuid in [
            Uuid::nil(),
            Uuid::max(),
            Uuid::parse_str("418874e5-a2f5-4b14-a177-2353e0e9061d").unwrap(),
        ] {
            let mut value = uuid.as_u128();
            let mut expected = [b'0'; 22];
            for character in expected.iter_mut().rev() {
                *character = ALPHABET[(value % 62) as usize];
                value /= 62;
            }
            assert_eq!(group_code_from_uuid(uuid).as_bytes(), expected);
        }
        assert_eq!(
            group_code_from_uuid(Uuid::parse_str("418874e5-a2f5-4b14-a177-2353e0e9061d").unwrap()),
            "1zerRwRYnyht7gsK8shFaX"
        );
    }

    #[test]
    fn normalizes_group_codes_for_lookup() {
        assert_eq!(
            normalize_group_code("  2aIFX0J6L4w7Y9KzQp8VrN  "),
            Some("2aIFX0J6L4w7Y9KzQp8VrN".to_string())
        );
        assert_eq!(
            normalize_group_code("  Calm-Harbor-123ABC  "),
            Some("calm-harbor-123abc".to_string())
        );
        assert_eq!(normalize_group_code("  "), None);
        assert_eq!(normalize_group_code(&"a".repeat(81)), None);
        assert_eq!(normalize_group_code("2aIFX0J6L4w7Y9KzQp8Vr-"), None);
    }

    #[test]
    fn group_avatar_url_changes_revision_without_changing_route() {
        let group_id = Uuid::new_v4();
        let first_update = Utc.timestamp_micros(1_700_000_000_000_000).unwrap();
        let second_update = first_update + Duration::microseconds(1);

        let first = group_avatar_url_with_revision(group_id, first_update);
        let second = group_avatar_url_with_revision(group_id, second_update);

        assert_ne!(first, second);
        assert!(first.starts_with(&format!(
            "/social/conversations/groups/{group_id}/avatar?v="
        )));
        assert!(second.starts_with(&format!(
            "/social/conversations/groups/{group_id}/avatar?v="
        )));
    }

    #[test]
    fn validates_initial_group_members() {
        let creator_id = Uuid::new_v4();
        let member_id = Uuid::new_v4();

        assert_eq!(
            validate_initial_member_ids(creator_id, vec![member_id]).unwrap(),
            vec![member_id]
        );
        assert!(validate_initial_member_ids(creator_id, vec![creator_id]).is_err());
        assert!(validate_initial_member_ids(creator_id, vec![member_id, member_id]).is_err());
        assert!(
            validate_initial_member_ids(creator_id, (0..101).map(|_| Uuid::new_v4()).collect())
                .is_err()
        );
    }

    #[test]
    fn friends_only_initial_members_require_every_friendship_pair() {
        let creator_id = Uuid::new_v4();
        let first_member_id = Uuid::new_v4();
        let second_member_id = Uuid::new_v4();
        let member_ids = [first_member_id, second_member_id];

        let friends_only_pairs = required_initial_friendship_pairs(
            creator_id,
            &member_ids,
            GroupAccessPolicy::FriendsOnly,
        );
        assert_eq!(friends_only_pairs.len(), 3);
        assert!(
            friends_only_pairs.contains(&super::canonical_user_pair(creator_id, first_member_id,))
        );
        assert!(
            friends_only_pairs.contains(&super::canonical_user_pair(creator_id, second_member_id,))
        );
        assert!(friends_only_pairs.contains(&super::canonical_user_pair(
            first_member_id,
            second_member_id,
        )));

        for policy in [GroupAccessPolicy::Open, GroupAccessPolicy::Password] {
            let invitation_pairs =
                required_initial_friendship_pairs(creator_id, &member_ids, policy);
            assert_eq!(invitation_pairs.len(), 2);
            assert!(!invitation_pairs.contains(&super::canonical_user_pair(
                first_member_id,
                second_member_id,
            )));
        }
    }

    #[test]
    fn validates_partial_group_updates() {
        assert_eq!(
            validate_group_update_request(GroupAccessPolicy::Open, None, false).unwrap(),
            GroupAccessPolicy::Open
        );
        assert_eq!(
            validate_group_update_request(GroupAccessPolicy::Password, None, false).unwrap(),
            GroupAccessPolicy::Password
        );
        assert_eq!(
            validate_group_update_request(
                GroupAccessPolicy::Password,
                Some(GroupAccessPolicy::Password),
                false
            )
            .unwrap(),
            GroupAccessPolicy::Password
        );
        assert!(
            validate_group_update_request(
                GroupAccessPolicy::Open,
                Some(GroupAccessPolicy::Password),
                false
            )
            .is_err()
        );
        assert!(validate_group_update_request(GroupAccessPolicy::Open, None, true).is_err());
    }

    #[test]
    fn allows_creator_and_admin_to_delete_groups() {
        assert!(can_delete_group("creator"));
        assert!(can_delete_group("admin"));
        assert!(!can_delete_group("member"));
    }

    #[test]
    fn validates_group_titles() {
        assert_eq!(validate_group_title("  Team chat  ").unwrap(), "Team chat");
        assert!(validate_group_title(" ").is_err());
        assert!(validate_group_title(&"a".repeat(129)).is_err());
    }

    #[test]
    fn requires_password_only_for_password_groups() {
        assert!(
            validate_password_for_policy(GroupAccessPolicy::Password, Some("password1")).is_ok()
        );
        assert!(validate_password_for_policy(GroupAccessPolicy::Password, Some("short")).is_err());
        assert!(
            validate_password_for_policy(GroupAccessPolicy::Password, Some(&"a".repeat(257)))
                .is_err()
        );
        assert!(validate_password_for_policy(GroupAccessPolicy::Password, None).is_err());
        assert!(validate_password_for_policy(GroupAccessPolicy::Open, Some("password1")).is_err());
        assert!(validate_password_for_policy(GroupAccessPolicy::FriendsOnly, None).is_ok());
    }

    #[test]
    fn limits_group_info_previews_by_access_policy() {
        assert!(can_view_group_info(GroupAccessPolicy::Open, false));
        assert!(can_view_group_info(GroupAccessPolicy::Password, false));
        assert!(!can_view_group_info(GroupAccessPolicy::FriendsOnly, false));
        assert!(can_view_group_info(GroupAccessPolicy::FriendsOnly, true));
        assert!(!can_view_group_info(
            GroupAccessPolicy::FriendsOfFriends,
            false
        ));
        assert!(can_view_group_info(
            GroupAccessPolicy::FriendsOfFriends,
            true
        ));
    }

    #[test]
    fn only_non_members_can_join_a_group() {
        assert!(can_join_group(false));
        assert!(!can_join_group(true));
    }
}
