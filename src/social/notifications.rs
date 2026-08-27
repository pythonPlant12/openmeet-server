use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    schema::{notifications, users},
    social::models::{NewNotification, Notification, NotificationResponse},
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;
type EmptyResult = Result<StatusCode, (StatusCode, String)>;

pub(crate) fn notification_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_notifications))
        .route("/{id}/read", post(mark_notification_read))
}

pub(crate) async fn create_notification(
    conn: &mut AsyncPgConnection,
    recipient_id: Uuid,
    actor_id: Uuid,
    kind: &str,
    data: Value,
) -> Result<(), diesel::result::Error> {
    diesel::insert_into(notifications::table)
        .values(NewNotification {
            recipient_id,
            actor_id,
            kind,
            data,
        })
        .execute(conn)
        .await?;
    Ok(())
}

async fn list_notifications(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<NotificationResponse>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let notifications_with_actors: Vec<(Notification, String)> = notifications::table
        .inner_join(users::table.on(users::id.eq(notifications::actor_id)))
        .filter(notifications::recipient_id.eq(user_id))
        .filter(notifications::read_at.is_null())
        .order(notifications::created_at.desc())
        .limit(100)
        .select((Notification::as_select(), users::name))
        .load(&mut conn)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        notifications_with_actors
            .into_iter()
            .map(|(notification, actor_name)| notification_response(notification, actor_name))
            .collect(),
    ))
}

async fn mark_notification_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(notification_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    diesel::update(
        notifications::table
            .filter(notifications::id.eq(notification_id))
            .filter(notifications::recipient_id.eq(user_id))
            .filter(notifications::read_at.is_null()),
    )
    .set(notifications::read_at.eq(Utc::now()))
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;

    Ok(StatusCode::NO_CONTENT)
}

fn notification_response(notification: Notification, actor_name: String) -> NotificationResponse {
    NotificationResponse {
        id: notification.id,
        kind: notification.kind,
        actor_id: notification.actor_id,
        actor_name,
        data: notification.data,
        created_at: notification.created_at,
    }
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!(error = %error, "Social notification database operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Notification unavailable".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::{Notification, notification_response};

    #[test]
    fn serializes_generic_notification_payload() {
        let notification = Notification {
            id: Uuid::nil(),
            recipient_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            kind: "friendRequest".to_string(),
            data: json!({ "friendshipId": "friendship-id" }),
            created_at: Utc::now(),
            read_at: None,
        };

        let response = notification_response(notification, "Alice".to_string());

        assert_eq!(response.kind, "friendRequest");
        assert_eq!(response.actor_name, "Alice");
        assert_eq!(response.data["friendshipId"], "friendship-id");
    }
}
