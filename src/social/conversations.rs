use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
};
use chrono::Utc;
use diesel::{
    deserialize::QueryableByName,
    prelude::*,
    sql_query,
    sql_types::{Array, BigInt, Uuid as SqlUuid},
};
use diesel_async::{AsyncConnection, RunQueryDsl, scoped_futures::ScopedFutureExt};
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    schema::{conversation_members, conversations, direct_message_requests, friendships, users},
    social::{
        messages::{create_message, list_messages},
        models::{
            AddConversationMemberRequest, Conversation, ConversationKind, ConversationMember,
            ConversationResponse, CreateGroupRequest, DirectMessageRequest,
            DirectMessageRequestResponse, GroupAccessPolicy, GroupInfoResponse,
            GroupMemberResponse, NewConversation, NewConversationMember, NewDirectMessageRequest,
            OpenDirectConversationResponse, RespondToDirectMessageRequest,
            UpdateConversationMemberRoleRequest, UpdateGroupPolicyRequest,
        },
    },
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;
type EmptyResult = Result<StatusCode, (StatusCode, String)>;

#[derive(Debug)]
struct ConversationError {
    status: StatusCode,
    message: String,
}

impl ConversationError {
    fn into_api_error(self) -> (StatusCode, String) {
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
        .route("/groups/{id}/info", get(get_group_info))
        .route("/groups/{id}/join", post(join_group))
        .route("/groups/{id}/policy", post(update_group_policy))
        .route(
            "/groups/{id}/members",
            get(list_group_members).post(add_group_member),
        )
        .route(
            "/groups/{id}/members/{user_id}",
            delete(remove_group_member),
        )
        .route(
            "/groups/{id}/members/{user_id}/role",
            post(update_group_member_role),
        )
        .route("/direct/requests", get(list_direct_message_requests))
        .route("/direct/{user_id}", post(open_direct_conversation))
        .route(
            "/direct/requests/{id}/respond",
            post(respond_to_direct_message_request),
        )
        .route("/{id}/messages", get(list_messages).post(create_message))
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
    let password_hash = hash_group_password(request.password).await?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    let conversation: Conversation = diesel::insert_into(conversations::table)
        .values(NewConversation {
            kind: "group".to_string(),
            creator_id: user_id,
            title: Some(title),
            access_policy: Some(request.access_policy.as_db_value().to_string()),
            password_hash,
            direct_user_low_id: None,
            direct_user_high_id: None,
        })
        .returning(Conversation::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;

    diesel::insert_into(conversation_members::table)
        .values(NewConversationMember {
            conversation_id: conversation.id,
            user_id,
            role: "creator".to_string(),
        })
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;

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
    let role = group_member_role(&mut conn, group_id, user_id).await?;
    let policy = group_policy(&group)?;

    if !can_view_group_info(policy, role.is_some()) {
        return Err((StatusCode::NOT_FOUND, "Group not found".to_string()));
    }

    let member_count = conversation_members::table
        .filter(conversation_members::conversation_id.eq(group_id))
        .count()
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    let title = group
        .title
        .ok_or_else(|| internal_error("group is missing a title"))?;
    let is_member = role.is_some();

    Ok(Json(GroupInfoResponse {
        id: group.id,
        title,
        access_policy: policy,
        member_count,
        is_member,
        role,
        can_join: can_join_group(is_member),
    }))
}

async fn list_group_members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
) -> ApiResult<Vec<GroupMemberResponse>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    load_group(&mut conn, group_id).await?;

    if group_member_role(&mut conn, group_id, user_id)
        .await?
        .is_none()
    {
        return Err((StatusCode::NOT_FOUND, "Group not found".to_string()));
    }

    let members: Vec<(Uuid, String, String, chrono::DateTime<Utc>)> = conversation_members::table
        .inner_join(users::table.on(users::id.eq(conversation_members::user_id)))
        .filter(conversation_members::conversation_id.eq(group_id))
        .order((conversation_members::joined_at.asc(), users::id.asc()))
        .select((
            users::id,
            users::name,
            conversation_members::role,
            conversation_members::joined_at,
        ))
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        members
            .into_iter()
            .map(|(id, name, role, joined_at)| GroupMemberResponse {
                id,
                name,
                role,
                joined_at,
            })
            .collect(),
    ))
}

async fn join_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Json(request): Json<crate::social::models::JoinGroupRequest>,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
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

    Ok(Json(conversation_response(&group, user_id, Some(role))?))
}

async fn update_group_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Json(request): Json<UpdateGroupPolicyRequest>,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    validate_password_for_policy(request.access_policy, request.password.as_deref())?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    conn.transaction(|conn| {
        async move {
            require_group_admin_for_update(conn, group_id, user_id).await?;
            Ok(())
        }
        .scope_boxed()
    })
    .await
    .map_err(ConversationError::into_api_error)?;

    let password_hash = hash_group_password(request.password).await?;
    let access_policy = request.access_policy;
    let (updated, role): (Conversation, String) = conn
        .transaction(|conn| {
            async move {
                let (_, role) = require_group_admin_for_update(conn, group_id, user_id).await?;
                let updated: Conversation = diesel::update(conversations::table.find(group_id))
                    .set((
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

    Ok(Json(conversation_response(&group, user_id, Some(role))?))
}

async fn remove_group_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((group_id, member_id)): Path<(Uuid, Uuid)>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let group = require_group_admin(&mut conn, group_id, user_id).await?;

    if member_id == group.creator_id {
        return Err((
            StatusCode::FORBIDDEN,
            "The group creator cannot be removed".to_string(),
        ));
    }
    let deleted = diesel::delete(
        conversation_members::table
            .filter(conversation_members::conversation_id.eq(group_id))
            .filter(conversation_members::user_id.eq(member_id)),
    )
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;
    if deleted == 0 {
        return Err((StatusCode::NOT_FOUND, "Group member not found".to_string()));
    }
    touch_group(&mut conn, group_id).await?;
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
    let group = load_group(&mut conn, group_id).await?;
    if group.creator_id != user_id {
        return Err((
            StatusCode::FORBIDDEN,
            "Only the group creator can change admin roles".to_string(),
        ));
    }
    if member_id == group.creator_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "The group creator role cannot be changed".to_string(),
        ));
    }
    let updated = diesel::update(
        conversation_members::table
            .filter(conversation_members::conversation_id.eq(group_id))
            .filter(conversation_members::user_id.eq(member_id)),
    )
    .set(conversation_members::role.eq(request.role.as_db_value()))
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;
    if updated == 0 {
        return Err((StatusCode::NOT_FOUND, "Group member not found".to_string()));
    }
    touch_group(&mut conn, group_id).await?;
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

async fn load_group_for_update(
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

async fn require_group_admin_for_update(
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

async fn group_member_role(
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

async fn authorize_group_admission(
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
        GroupAccessPolicy::FriendsOnly => {
            if is_friends_with_every_member(conn, group.id, candidate_id).await? {
                Ok(())
            } else {
                Err((
                    StatusCode::FORBIDDEN,
                    "Friends-only groups require friendship with every member".to_string(),
                ))
            }
        }
    }
}

async fn authorize_admin_addition(
    conn: &mut diesel_async::AsyncPgConnection,
    group: &Conversation,
    candidate_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    if group_policy(group)? == GroupAccessPolicy::FriendsOnly
        && !is_friends_with_every_member(conn, group.id, candidate_id).await?
    {
        return Err((
            StatusCode::FORBIDDEN,
            "Friends-only groups require friendship with every member".to_string(),
        ));
    }
    Ok(())
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

async fn add_member(
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

async fn ensure_user_exists(
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
            kind: "direct".to_string(),
            creator_id: first_user_id,
            title: None,
            access_policy: None,
            password_hash: None,
            direct_user_low_id: Some(low_id),
            direct_user_high_id: Some(high_id),
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

fn conversation_response(
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
        role,
        other_user_id,
        message_count: 0,
        unread_count: 0,
        created_at: conversation.created_at,
        updated_at: conversation.updated_at,
    })
}

#[derive(QueryableByName)]
struct ConversationMessageCounts {
    #[diesel(sql_type = SqlUuid)]
    conversation_id: Uuid,
    #[diesel(sql_type = BigInt)]
    message_count: i64,
    #[diesel(sql_type = BigInt)]
    unread_count: i64,
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
                )::BIGINT AS unread_count
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

fn group_policy(group: &Conversation) -> Result<GroupAccessPolicy, (StatusCode, String)> {
    group
        .access_policy
        .as_deref()
        .and_then(GroupAccessPolicy::from_db_value)
        .ok_or_else(|| internal_error("invalid group access policy"))
}

fn can_view_group_info(policy: GroupAccessPolicy, is_member: bool) -> bool {
    is_member || policy != GroupAccessPolicy::FriendsOnly
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

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!("Conversation API error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Request failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        GroupAccessPolicy, can_join_group, can_view_group_info, validate_group_title,
        validate_password_for_policy,
    };

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
    }

    #[test]
    fn only_non_members_can_join_a_group() {
        assert!(can_join_group(false));
        assert!(!can_join_group(true));
    }
}
