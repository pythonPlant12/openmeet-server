use std::collections::HashMap;

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::{delete, get, post},
};
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel::sql_types::Uuid as SqlUuid;
use diesel_async::{
    AsyncConnection, AsyncPgConnection, RunQueryDsl, scoped_futures::ScopedFutureExt,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    AppState,
    auth::{extract_user_id, models::User},
    schema::{call_invitations, friendships, meeting_history, notifications, user_presence, users},
    social::{
        SocialResource, conversation_routes, create_notification,
        models::{
            CallInvitation, CallInvitationResponse, CreateCallRequest, CreateFriendRequest,
            FriendRequestItem, FriendSummary, FriendsResponse, Friendship, MeetingHistory,
            MeetingHistoryResponse, NewCallInvitation, NewFriendship, NewMeetingHistory,
            RecordMeetingRequest, RespondToCallRequest, SearchUsersQuery, UpdateSelfProfileRequest,
            UserDiscovery, UserPresence, UserProfile, UserStatus,
        },
        notification_routes,
    },
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;
type EmptyResult = Result<StatusCode, (StatusCode, String)>;

const MAX_AVATAR_BYTES: usize = 5 * 1024 * 1024;
const MAX_AVATAR_UPLOAD_BYTES: usize = MAX_AVATAR_BYTES + 1024 * 1024;

pub fn social_routes() -> Router<AppState> {
    Router::new()
        .route("/friends", get(list_friends).post(create_friend_request))
        .route("/friends/{id}", delete(delete_friendship))
        .route("/friends/{id}/accept", post(accept_friend_request))
        .route("/users", get(search_users))
        .route("/users/{id}/profile", get(get_user_profile))
        .route("/users/{id}/avatar", get(get_user_avatar))
        .route(
            "/me/profile",
            get(get_self_profile).patch(update_self_profile),
        )
        .route(
            "/me/profile/avatar",
            post(upload_avatar).layer(DefaultBodyLimit::max(MAX_AVATAR_UPLOAD_BYTES)),
        )
        .route("/presence", post(update_presence))
        .route("/events", get(crate::social::social_events_handler))
        .route("/calls", post(create_call_invitation))
        .route("/calls/incoming", get(list_incoming_calls))
        .route("/calls/{id}/respond", post(respond_to_call))
        .route("/meetings", get(list_meetings).post(record_meeting))
        .route("/meetings/{id}", delete(delete_meeting))
        .nest("/notifications", notification_routes())
        .route(
            "/conversations/{id}/call-sessions",
            post(crate::social::start_call_session),
        )
        .nest("/call-sessions", crate::social::call_session_routes())
        .nest("/conversations", conversation_routes())
}

pub async fn search_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchUsersQuery>,
) -> ApiResult<Vec<UserDiscovery>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let query = validated_user_search_query(&query.query).ok_or((
        StatusCode::BAD_REQUEST,
        "Query must be 2 to 80 characters".to_string(),
    ))?;
    let pattern = format!(
        "%{}%",
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    let users: Vec<UserDiscovery> = users::table
        .filter(users::id.ne(user_id))
        .filter(users::name.ilike(&pattern).or(users::email.ilike(&pattern)))
        .order((users::name.asc(), users::email.asc()))
        .limit(10)
        .select(UserDiscovery::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    Ok(Json(users))
}

pub async fn get_user_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
) -> ApiResult<UserProfile> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    authorize_profile_access(&mut conn, user_id, target_user_id).await?;

    let user: User = users::table
        .filter(users::id.eq(target_user_id))
        .select(User::as_select())
        .first(&mut conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::NotFound => {
                (StatusCode::NOT_FOUND, "User not found".to_string())
            }
            _ => internal_error(error),
        })?;
    let last_seen_at = user_presence::table
        .filter(user_presence::user_id.eq(target_user_id))
        .select(user_presence::last_seen_at)
        .first::<DateTime<Utc>>(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?;
    Ok(Json(profile_response(user, last_seen_at)?))
}

pub async fn get_user_avatar(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
) -> Result<Response, (StatusCode, String)> {
    let requester_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    authorize_profile_access(&mut conn, requester_id, user_id).await?;
    let avatar_key = users::table
        .filter(users::id.eq(user_id))
        .select(users::avatar_key)
        .first::<Option<String>>(&mut conn)
        .await
        .map_err(user_not_found)?
        .ok_or((StatusCode::NOT_FOUND, "Avatar not found".to_string()))?;
    let avatar = state
        .avatar_storage
        .download(&avatar_key)
        .await
        .map_err(storage_error)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, avatar.content_type)
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(Body::from(avatar.bytes))
        .map_err(internal_error)
}

pub async fn get_self_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<UserProfile> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let user: User = users::table
        .filter(users::id.eq(user_id))
        .select(User::as_select())
        .first(&mut conn)
        .await
        .map_err(user_not_found)?;
    let last_seen_at = user_presence::table
        .filter(user_presence::user_id.eq(user_id))
        .select(user_presence::last_seen_at)
        .first::<DateTime<Utc>>(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?;

    Ok(Json(profile_response(user, last_seen_at)?))
}

pub async fn update_self_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateSelfProfileRequest>,
) -> ApiResult<UserProfile> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let existing: User = users::table
        .filter(users::id.eq(user_id))
        .select(User::as_select())
        .first(&mut conn)
        .await
        .map_err(user_not_found)?;

    let name = match request.name {
        Some(name) => normalize_name(&name).ok_or((
            StatusCode::BAD_REQUEST,
            "Name must be 1 to 255 characters".to_string(),
        ))?,
        None => existing.name,
    };
    let nickname = match request.nickname {
        Some(nickname) => normalize_nickname(&nickname).ok_or((
            StatusCode::BAD_REQUEST,
            "Nickname must be 3 to 50 lowercase letters, numbers, or underscores".to_string(),
        ))?,
        None => existing.nickname,
    };
    let status = request
        .status
        .map(UserStatus::as_db_value)
        .unwrap_or(&existing.status)
        .to_string();
    let status_message = match request.status_message {
        Some(status_message) => normalize_status_message(&status_message).ok_or((
            StatusCode::BAD_REQUEST,
            "Status message must be at most 255 characters".to_string(),
        ))?,
        None => existing.status_message,
    };

    let user: User = diesel::update(users::table.filter(users::id.eq(user_id)))
        .set((
            users::name.eq(name),
            users::nickname.eq(nickname),
            users::status.eq(status),
            users::status_message.eq(status_message),
            users::updated_at.eq(Utc::now().naive_utc()),
        ))
        .returning(User::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => (StatusCode::CONFLICT, "Nickname already exists".to_string()),
            _ => internal_error(error),
        })?;

    Ok(Json(profile_response(user, None)?))
}

pub async fn upload_avatar(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<UserProfile> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut avatar = None;

    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        if field.name() != Some("avatar") || avatar.is_some() {
            return Err((
                StatusCode::BAD_REQUEST,
                "Submit exactly one avatar file".to_string(),
            ));
        }
        let content_type = field.content_type().map(str::to_owned).ok_or((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Avatar content type is required".to_string(),
        ))?;
        let extension = avatar_extension(&content_type).ok_or((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Avatar must be a JPEG, PNG, WebP, or GIF image".to_string(),
        ))?;
        let bytes = field.bytes().await.map_err(multipart_error)?;
        if !valid_avatar_size(bytes.len()) {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "Avatar must be between 1 byte and 5 MiB".to_string(),
            ));
        }
        if !has_image_signature(&content_type, &bytes) {
            return Err((
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Avatar contents do not match its image type".to_string(),
            ));
        }
        avatar = Some((content_type, extension, bytes.to_vec()));
    }

    let Some((content_type, extension, bytes)) = avatar else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Avatar file is required".to_string(),
        ));
    };
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let previous_avatar_key = users::table
        .filter(users::id.eq(user_id))
        .select(users::avatar_key)
        .first::<Option<String>>(&mut conn)
        .await
        .map_err(user_not_found)?;
    let key = format!("avatars/{user_id}/{}.{}", Uuid::new_v4(), extension);
    state
        .avatar_storage
        .upload(&key, &content_type, bytes)
        .await
        .map_err(storage_error)?;

    let update_result: Result<User, diesel::result::Error> =
        diesel::update(users::table.filter(users::id.eq(user_id)))
            .set((
                users::avatar_key.eq(Some(&key)),
                users::updated_at.eq(Utc::now().naive_utc()),
            ))
            .returning(User::as_returning())
            .get_result(&mut conn)
            .await;
    let user = match update_result {
        Ok(user) => user,
        Err(error) => {
            if let Err(delete_error) = state.avatar_storage.delete(&key).await {
                tracing::error!(%delete_error, avatar_key = %key, "Failed to remove orphaned avatar");
            }
            return Err(user_not_found(error));
        }
    };

    if let Some(previous_key) = previous_avatar_key
        .as_deref()
        .filter(|previous| *previous != key.as_str())
    {
        if let Err(error) = state.avatar_storage.delete(previous_key).await {
            tracing::warn!(%error, avatar_key = %previous_key, "Failed to remove replaced avatar");
        }
    }

    Ok(Json(profile_response(user, None)?))
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
            let avatar_url = user
                .avatar_key
                .as_ref()
                .map(|_| format!("/social/users/{}/avatar", user.id));
            let summary = FriendSummary {
                id: user.id,
                name: user.name,
                email: user.email,
                avatar_url,
                is_online: presence_by_user.get(&user.id).copied().unwrap_or(false),
                friendship_id: None,
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
        let Some(mut user) = users_by_id.get(&related_id).cloned() else {
            continue;
        };
        user.friendship_id = Some(relationship.id);

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

    let existing: Option<Friendship> = friendships::table
        .filter(
            friendships::requester_id
                .eq(user_id)
                .and(friendships::addressee_id.eq(target.id))
                .or(friendships::requester_id
                    .eq(target.id)
                    .and(friendships::addressee_id.eq(user_id))),
        )
        .select(Friendship::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?;
    if let Some(existing) = existing {
        let message = if existing.status == "accepted" {
            "Already friends"
        } else {
            "Friend request already pending"
        };
        return Err((StatusCode::CONFLICT, message.to_string()));
    }

    let result = conn
        .transaction(|conn| {
            async move {
                let friendship: Friendship = diesel::insert_into(friendships::table)
                    .values(NewFriendship {
                        requester_id: user_id,
                        addressee_id: target.id,
                    })
                    .returning(Friendship::as_returning())
                    .get_result(conn)
                    .await?;
                create_notification(
                    conn,
                    target.id,
                    user_id,
                    "friendRequest",
                    json!({ "friendshipId": friendship.id }),
                )
                .await?;
                Ok(())
            }
            .scope_boxed()
        })
        .await;

    match result {
        Ok(_) => {
            state
                .social_events
                .publish([target.id], SocialResource::Notifications);
            state
                .social_events
                .publish([user_id, target.id], SocialResource::Friends);
            Ok(StatusCode::NO_CONTENT)
        }
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => Err((
            StatusCode::CONFLICT,
            "Friend request already pending".to_string(),
        )),
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::CheckViolation,
            _,
        )) => Err((
            StatusCode::BAD_REQUEST,
            "Invalid friend request".to_string(),
        )),
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
    let friendship: Friendship = diesel::update(
        friendships::table
            .filter(friendships::id.eq(friendship_id))
            .filter(friendships::addressee_id.eq(user_id))
            .filter(friendships::status.eq("pending")),
    )
    .set((
        friendships::status.eq("accepted"),
        friendships::updated_at.eq(Utc::now()),
    ))
    .returning(Friendship::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|error| match error {
        diesel::result::Error::NotFound => (
            StatusCode::NOT_FOUND,
            "Friend request not found".to_string(),
        ),
        _ => internal_error(error),
    })?;
    state.social_events.publish(
        [friendship.requester_id, friendship.addressee_id],
        SocialResource::Friends,
    );
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_friendship(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(friendship_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let friendship: Friendship = conn
        .transaction(|conn| {
            async move {
                let friendship: Friendship = diesel::delete(
                    friendships::table
                        .filter(friendships::id.eq(friendship_id))
                        .filter(
                            friendships::requester_id
                                .eq(user_id)
                                .or(friendships::addressee_id.eq(user_id)),
                        ),
                )
                .returning(Friendship::as_returning())
                .get_result(conn)
                .await?;
                if friendship.status == "accepted" {
                    let recipient_id = if friendship.requester_id == user_id {
                        friendship.addressee_id
                    } else {
                        friendship.requester_id
                    };
                    create_notification(
                        conn,
                        recipient_id,
                        user_id,
                        "friendRemoved",
                        json!({ "friendshipId": friendship.id }),
                    )
                    .await?;
                } else {
                    diesel::delete(
                        notifications::table
                            .filter(notifications::recipient_id.eq(friendship.addressee_id))
                            .filter(notifications::actor_id.eq(friendship.requester_id))
                            .filter(notifications::kind.eq("friendRequest"))
                            .filter(notifications::read_at.is_null()),
                    )
                    .execute(conn)
                    .await?;
                }
                Ok(friendship)
            }
            .scope_boxed()
        })
        .await
        .map_err(|error: diesel::result::Error| match error {
            diesel::result::Error::NotFound => {
                (StatusCode::NOT_FOUND, "Friendship not found".to_string())
            }
            _ => internal_error(error),
        })?;

    tracing::debug!(friendship_id = %friendship.id, user_id = %user_id, "Friendship removed");
    state.social_events.publish(
        [friendship.requester_id, friendship.addressee_id],
        SocialResource::Friends,
    );
    if friendship.status == "accepted" {
        let recipient_id = if friendship.requester_id == user_id {
            friendship.addressee_id
        } else {
            friendship.requester_id
        };
        state
            .social_events
            .publish([recipient_id], SocialResource::Notifications);
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
        state.social_events.publish(
            [invitation.caller_id, invitation.callee_id],
            SocialResource::Calls,
        );
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

    if let Err(error) = diesel::sql_query(
        "DELETE FROM call_invitations WHERE caller_id = $1 AND status <> 'pending' AND id NOT IN (SELECT id FROM call_invitations WHERE caller_id = $1 AND status <> 'pending' ORDER BY created_at DESC LIMIT 100)",
    )
    .bind::<SqlUuid, _>(invitation.caller_id)
    .execute(&mut conn)
    .await
    {
        tracing::warn!(%error, caller_id = %invitation.caller_id, "Failed to prune old call invitations");
    }

    state.social_events.publish(
        [invitation.caller_id, invitation.callee_id],
        SocialResource::Calls,
    );
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
            let avatar_url = user
                .avatar_key
                .as_ref()
                .map(|_| format!("/social/users/{}/avatar", user.id));
            let summary = FriendSummary {
                id: user.id,
                name: user.name,
                email: user.email,
                avatar_url,
                is_online: presence_by_user.get(&user.id).copied().unwrap_or(false),
                friendship_id: None,
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

    state.social_events.publish(
        [invitation.caller_id, invitation.callee_id],
        SocialResource::Calls,
    );

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

fn profile_response(
    user: User,
    last_seen_at: Option<DateTime<Utc>>,
) -> Result<UserProfile, (StatusCode, String)> {
    let status = UserStatus::from_db_value(&user.status).ok_or_else(|| {
        tracing::error!(user_id = %user.id, status = %user.status, "Invalid persisted user status");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Profile unavailable".to_string(),
        )
    })?;
    let online_after = Utc::now() - Duration::seconds(45);

    Ok(UserProfile {
        id: user.id,
        name: user.name,
        nickname: user.nickname,
        email: user.email,
        avatar_url: user
            .avatar_key
            .as_ref()
            .map(|_| format!("/social/users/{}/avatar", user.id)),
        status,
        status_message: user.status_message,
        created_at: user.created_at,
        is_online: last_seen_at
            .as_ref()
            .is_some_and(|last_seen_at| *last_seen_at > online_after),
        last_seen_at,
    })
}

fn normalize_name(value: &str) -> Option<String> {
    let value = value.trim();
    (1..=255)
        .contains(&value.chars().count())
        .then(|| value.to_string())
}

fn normalize_nickname(value: &str) -> Option<String> {
    let nickname = value.trim().to_lowercase();
    ((3..=50).contains(&nickname.len())
        && nickname.bytes().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == b'_'
        }))
    .then_some(nickname)
}

fn normalize_status_message(value: &str) -> Option<String> {
    let value = value.trim();
    (value.chars().count() <= 255).then(|| value.to_string())
}

fn avatar_extension(content_type: &str) -> Option<&'static str> {
    match image_media_type(content_type).as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn has_image_signature(content_type: &str, bytes: &[u8]) -> bool {
    match image_media_type(content_type).as_str() {
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        _ => false,
    }
}

fn valid_avatar_size(size: usize) -> bool {
    (1..=MAX_AVATAR_BYTES).contains(&size)
}

fn image_media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

async fn authorize_profile_access(
    conn: &mut diesel_async::AsyncPgConnection,
    requester_id: Uuid,
    target_user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    if requester_id == target_user_id {
        return Ok(());
    }

    let is_friend = friendships::table
        .filter(friendships::status.eq("accepted"))
        .filter(
            friendships::requester_id
                .eq(requester_id)
                .and(friendships::addressee_id.eq(target_user_id))
                .or(friendships::requester_id
                    .eq(target_user_id)
                    .and(friendships::addressee_id.eq(requester_id))),
        )
        .select(friendships::id)
        .first::<Uuid>(conn)
        .await
        .optional()
        .map_err(internal_error)?
        .is_some();
    if is_friend {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "Profiles are limited to friends".to_string(),
        ))
    }
}

fn multipart_error(error: axum::extract::multipart::MultipartError) -> (StatusCode, String) {
    tracing::warn!(%error, "Invalid avatar upload");
    (StatusCode::BAD_REQUEST, "Invalid avatar upload".to_string())
}

fn storage_error(error: anyhow::Error) -> (StatusCode, String) {
    tracing::error!(%error, "Avatar storage error");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "Avatar storage is unavailable".to_string(),
    )
}

fn user_not_found(error: diesel::result::Error) -> (StatusCode, String) {
    match error {
        diesel::result::Error::NotFound => (StatusCode::NOT_FOUND, "User not found".to_string()),
        _ => internal_error(error),
    }
}

fn validated_user_search_query(query: &str) -> Option<&str> {
    let query = query.trim();
    (2..=80).contains(&query.chars().count()).then_some(query)
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
    use super::{
        MAX_AVATAR_BYTES, avatar_extension, has_image_signature, is_valid_room_id, normalize_name,
        normalize_nickname, normalize_status_message, valid_avatar_size,
        validated_user_search_query,
    };

    #[test]
    fn validates_room_ids() {
        assert!(is_valid_room_id("abc-123_room"));
        assert!(!is_valid_room_id(""));
        assert!(!is_valid_room_id("room with spaces"));
        assert!(!is_valid_room_id(&"a".repeat(129)));
    }

    #[test]
    fn validates_user_search_queries() {
        assert_eq!(validated_user_search_query("  Ada  "), Some("Ada"));
        assert_eq!(validated_user_search_query(" "), None);
        assert_eq!(validated_user_search_query("a"), None);
        assert_eq!(validated_user_search_query(&"a".repeat(81)), None);
    }

    #[test]
    fn validates_editable_profile_fields() {
        assert_eq!(
            normalize_name("  Ada Lovelace  "),
            Some("Ada Lovelace".to_string())
        );
        assert_eq!(normalize_name(" "), None);
        assert_eq!(normalize_nickname("Ada_42"), Some("ada_42".to_string()));
        assert_eq!(normalize_nickname("ada-name"), None);
        assert_eq!(normalize_status_message(&"a".repeat(255)).is_some(), true);
        assert_eq!(normalize_status_message(&"a".repeat(256)), None);
    }

    #[test]
    fn limits_avatar_content_types() {
        assert_eq!(avatar_extension("image/png"), Some("png"));
        assert_eq!(avatar_extension("image/svg+xml"), None);
        assert_eq!(avatar_extension("application/octet-stream"), None);
    }

    #[test]
    fn accepts_avatar_files_through_five_mebibytes() {
        assert!(valid_avatar_size(1));
        assert!(valid_avatar_size(MAX_AVATAR_BYTES));
        assert!(!valid_avatar_size(0));
        assert!(!valid_avatar_size(MAX_AVATAR_BYTES + 1));
    }

    #[test]
    fn rejects_mime_declared_images_without_matching_signatures() {
        assert!(has_image_signature("image/png", b"\x89PNG\r\n\x1a\n"));
        assert!(!has_image_signature("image/png", b"not an image"));
        assert!(has_image_signature("image/webp", b"RIFFxxxxWEBP"));
    }

    #[test]
    fn accepts_image_content_type_parameters() {
        assert_eq!(avatar_extension("image/png; charset=binary"), Some("png"));
        assert!(has_image_signature(
            "image/png; charset=binary",
            b"\x89PNG\r\n\x1a\nimage data"
        ));
    }
}
