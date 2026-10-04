//! Invitations into password-protected groups. Admins invite a person, and the invitee joins by
//! accepting the invitation with the group password, so the password stays the admission gate.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl, scoped_futures::ScopedFutureExt};
use serde_json::json;
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    schema::{conversations, group_invitations, users},
    social::{
        SocialResource,
        conversations::{
            ConversationError, add_member, authorize_group_admission, conversation_response,
            ensure_user_exists, group_member_role, group_policy, internal_error,
            load_group_for_update, publish_group_update, require_group_admin_for_update,
        },
        create_notification,
        models::{
            AcceptGroupInvitationRequest, Conversation, ConversationResponse,
            CreateGroupInvitationRequest, GroupAccessPolicy, GroupInvitation,
            GroupInvitationResponse, NewGroupInvitation,
        },
    },
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;
type EmptyResult = Result<StatusCode, (StatusCode, String)>;

const PENDING: &str = "pending";

pub(super) async fn create_group_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Json(request): Json<CreateGroupInvitationRequest>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let invitee_id = request.user_id;
    if invitee_id == user_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "You cannot invite yourself".to_string(),
        ));
    }

    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let notified = conn
        .transaction(|conn| {
            async move {
                // Locking the group row serializes invitations, so the pending check below is safe.
                let (group, _) = require_group_admin_for_update(conn, group_id, user_id).await?;
                if group_policy(&group)? != GroupAccessPolicy::Password {
                    return Err((
                        StatusCode::CONFLICT,
                        "Only password-protected groups use invitations".to_string(),
                    )
                        .into());
                }
                ensure_user_exists(conn, invitee_id).await?;
                if group_member_role(conn, group_id, invitee_id)
                    .await?
                    .is_some()
                {
                    return Err((
                        StatusCode::CONFLICT,
                        "That person is already a member".to_string(),
                    )
                        .into());
                }
                if pending_invitation_exists(conn, group_id, invitee_id).await? {
                    return Ok(false);
                }

                let invitation_id: Uuid = diesel::insert_into(group_invitations::table)
                    .values(NewGroupInvitation {
                        conversation_id: group_id,
                        inviter_id: user_id,
                        invitee_id,
                    })
                    .returning(group_invitations::id)
                    .get_result(conn)
                    .await?;
                create_notification(
                    conn,
                    invitee_id,
                    user_id,
                    "groupInvitation",
                    json!({
                        "invitationId": invitation_id,
                        "groupId": group_id,
                        "groupTitle": group.title,
                    }),
                )
                .await?;
                Ok::<_, ConversationError>(true)
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;

    if notified {
        state
            .social_events
            .publish([invitee_id], SocialResource::Notifications);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn list_group_invitations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<GroupInvitationResponse>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let invitations: Vec<(GroupInvitation, Conversation, String)> = group_invitations::table
        .inner_join(conversations::table)
        .inner_join(users::table.on(users::id.eq(group_invitations::inviter_id)))
        .filter(group_invitations::invitee_id.eq(user_id))
        .filter(group_invitations::status.eq(PENDING))
        .order(group_invitations::created_at.desc())
        .limit(100)
        .select((
            GroupInvitation::as_select(),
            Conversation::as_select(),
            users::name,
        ))
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    invitations
        .into_iter()
        .map(|(invitation, group, inviter_name)| {
            Ok(GroupInvitationResponse {
                id: invitation.id,
                group_id: group.id,
                access_policy: group_policy(&group)?,
                group_title: group
                    .title
                    .ok_or_else(|| internal_error("group is missing a title"))?,
                inviter_id: invitation.inviter_id,
                inviter_name,
                created_at: invitation.created_at,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}

pub(super) async fn accept_group_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(invitation_id): Path<Uuid>,
    Json(request): Json<AcceptGroupInvitationRequest>,
) -> ApiResult<ConversationResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let (group, role): (Conversation, String) = conn
        .transaction(|conn| {
            async move {
                let invitation =
                    load_pending_invitation_for_update(conn, invitation_id, user_id).await?;
                let group = load_group_for_update(conn, invitation.conversation_id).await?;
                let role = match group_member_role(conn, group.id, user_id).await? {
                    Some(role) => role,
                    None => {
                        // Admission rules apply at join time: a wrong password keeps the invitation pending.
                        authorize_group_admission(conn, &group, user_id, request.password).await?;
                        add_member(conn, group.id, user_id).await?;
                        "member".to_string()
                    }
                };
                resolve_invitation(conn, invitation.id, "accepted").await?;
                Ok::<_, ConversationError>((group, role))
            }
            .scope_boxed()
        })
        .await
        .map_err(ConversationError::into_api_error)?;

    publish_group_update(&state, &mut conn, group.id, &[]).await;

    Ok(Json(conversation_response(&group, user_id, Some(role))?))
}

pub(super) async fn decline_group_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(invitation_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let updated = diesel::update(
        group_invitations::table
            .filter(group_invitations::id.eq(invitation_id))
            .filter(group_invitations::invitee_id.eq(user_id))
            .filter(group_invitations::status.eq(PENDING)),
    )
    .set((
        group_invitations::status.eq("declined"),
        group_invitations::responded_at.eq(Utc::now()),
    ))
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;

    if updated == 0 {
        return Err(invitation_not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn pending_invitation_exists(
    conn: &mut diesel_async::AsyncPgConnection,
    group_id: Uuid,
    invitee_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    diesel::select(diesel::dsl::exists(
        group_invitations::table
            .filter(group_invitations::conversation_id.eq(group_id))
            .filter(group_invitations::invitee_id.eq(invitee_id))
            .filter(group_invitations::status.eq(PENDING)),
    ))
    .get_result(conn)
    .await
}

async fn load_pending_invitation_for_update(
    conn: &mut diesel_async::AsyncPgConnection,
    invitation_id: Uuid,
    invitee_id: Uuid,
) -> Result<GroupInvitation, (StatusCode, String)> {
    group_invitations::table
        .filter(group_invitations::id.eq(invitation_id))
        .filter(group_invitations::invitee_id.eq(invitee_id))
        .filter(group_invitations::status.eq(PENDING))
        .select(GroupInvitation::as_select())
        .for_update()
        .first(conn)
        .await
        .optional()
        .map_err(internal_error)?
        .ok_or_else(invitation_not_found)
}

async fn resolve_invitation(
    conn: &mut diesel_async::AsyncPgConnection,
    invitation_id: Uuid,
    status: &str,
) -> Result<(), diesel::result::Error> {
    diesel::update(group_invitations::table.find(invitation_id))
        .set((
            group_invitations::status.eq(status),
            group_invitations::responded_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
        .map(|_| ())
}

fn invitation_not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "Invitation not found".to_string())
}
