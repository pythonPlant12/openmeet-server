use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::Utc;
use diesel::{
    prelude::*,
    sql_query,
    sql_types::{BigInt, Uuid as SqlUuid},
};
use diesel_async::{AsyncConnection, RunQueryDsl, scoped_futures::ScopedFutureExt};
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    schema::{conversation_members, conversation_messages, conversations, users},
    social::models::{
        Conversation, ConversationMessage, ConversationMessageResponse,
        ConversationMessagesResponse, CreateConversationMessageRequest,
        ListConversationMessagesQuery, NewConversationMessage,
    },
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;
type CreateApiResult<T> = Result<(StatusCode, Json<T>), (StatusCode, String)>;

#[derive(Debug)]
struct MessageError {
    status: StatusCode,
    message: String,
}

impl MessageError {
    fn into_api_error(self) -> (StatusCode, String) {
        (self.status, self.message)
    }
}

impl From<(StatusCode, String)> for MessageError {
    fn from((status, message): (StatusCode, String)) -> Self {
        Self { status, message }
    }
}

impl From<diesel::result::Error> for MessageError {
    fn from(error: diesel::result::Error) -> Self {
        let (status, message) = internal_error(error);
        Self { status, message }
    }
}

pub(crate) async fn create_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
    Json(request): Json<CreateConversationMessageRequest>,
) -> CreateApiResult<ConversationMessageResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let content = validate_message_content(&request.content)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    let message: ConversationMessage = conn
        .transaction(|conn| {
            async move {
                authorize_conversation_access(conn, conversation_id, user_id).await?;
                let sender_name = users::table
                    .find(user_id)
                    .select(users::name)
                    .first(conn)
                    .await
                    .map_err(internal_error)?;
                let message = diesel::insert_into(conversation_messages::table)
                    .values(NewConversationMessage {
                        conversation_id,
                        sender_id: user_id,
                        sender_name,
                        content,
                    })
                    .returning(ConversationMessage::as_returning())
                    .get_result(conn)
                    .await
                    .map_err(internal_error)?;
                diesel::update(conversations::table.find(conversation_id))
                    .set(conversations::updated_at.eq(Utc::now()))
                    .execute(conn)
                    .await
                    .map_err(internal_error)?;

                Ok(message)
            }
            .scope_boxed()
        })
        .await
        .map_err(MessageError::into_api_error)?;

    Ok((StatusCode::CREATED, Json(message_response(message))))
}

pub(crate) async fn list_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
    Query(query): Query<ListConversationMessagesQuery>,
) -> ApiResult<ConversationMessagesResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let is_latest_page = query.before.is_none();
    let (before, limit) = validate_message_page(query)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    authorize_conversation_access(&mut conn, conversation_id, user_id).await?;

    let mut message_query = conversation_messages::table
        .filter(conversation_messages::conversation_id.eq(conversation_id))
        .into_boxed();
    if let Some(before) = before {
        message_query = message_query.filter(conversation_messages::sequence.lt(before));
    }
    let mut messages: Vec<ConversationMessage> = message_query
        .order(conversation_messages::sequence.desc())
        .limit(limit + 1)
        .select(ConversationMessage::as_select())
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let next_before = if messages.len() as i64 > limit {
        messages.truncate(limit as usize);
        messages.last().map(|message| message.sequence)
    } else {
        None
    };

    if is_latest_page {
        if let Some(latest_sequence) = messages.as_slice().first().map(|message| message.sequence) {
            mark_messages_read(&mut conn, conversation_id, user_id, latest_sequence).await?;
        }
    }

    Ok(Json(ConversationMessagesResponse {
        messages: messages.into_iter().map(message_response).collect(),
        next_before,
    }))
}

async fn mark_messages_read(
    conn: &mut diesel_async::AsyncPgConnection,
    conversation_id: Uuid,
    user_id: Uuid,
    latest_sequence: i64,
) -> Result<(), (StatusCode, String)> {
    sql_query(
        "INSERT INTO conversation_read_states (conversation_id, user_id, last_read_sequence)
         VALUES ($1, $2, $3)
         ON CONFLICT (conversation_id, user_id) DO UPDATE
         SET last_read_sequence = GREATEST(
                 conversation_read_states.last_read_sequence,
                 EXCLUDED.last_read_sequence
             ),
             updated_at = NOW()",
    )
    .bind::<SqlUuid, _>(conversation_id)
    .bind::<SqlUuid, _>(user_id)
    .bind::<BigInt, _>(latest_sequence)
    .execute(conn)
    .await
    .map_err(internal_error)?;
    Ok(())
}

async fn authorize_conversation_access(
    conn: &mut diesel_async::AsyncPgConnection,
    conversation_id: Uuid,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let conversation: Conversation = conversations::table
        .find(conversation_id)
        .select(Conversation::as_select())
        .first(conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::NotFound => {
                (StatusCode::NOT_FOUND, "Conversation not found".to_string())
            }
            _ => internal_error(error),
        })?;

    match conversation.kind.as_str() {
        "group" => {
            let is_member = conversation_members::table
                .filter(conversation_members::conversation_id.eq(conversation_id))
                .filter(conversation_members::user_id.eq(user_id))
                .select(conversation_members::user_id)
                .first::<Uuid>(conn)
                .await
                .optional()
                .map_err(internal_error)?
                .is_some();
            if is_member {
                Ok(())
            } else {
                Err((
                    StatusCode::FORBIDDEN,
                    "Conversation access denied".to_string(),
                ))
            }
        }
        "direct"
            if conversation.direct_user_low_id == Some(user_id)
                || conversation.direct_user_high_id == Some(user_id) =>
        {
            Ok(())
        }
        "direct" => Err((
            StatusCode::FORBIDDEN,
            "Conversation access denied".to_string(),
        )),
        _ => Err(internal_error("invalid conversation kind")),
    }
}

fn message_response(message: ConversationMessage) -> ConversationMessageResponse {
    ConversationMessageResponse {
        sequence: message.sequence,
        conversation_id: message.conversation_id,
        sender_id: message.sender_id,
        sender_name: message.sender_name,
        content: message.content,
        created_at: message.created_at,
    }
}

fn validate_message_content(content: &str) -> Result<String, (StatusCode, String)> {
    let content = content.trim();
    if !(1..=2000).contains(&content.chars().count()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Message content must contain 1 to 2000 characters".to_string(),
        ));
    }
    Ok(content.to_string())
}

fn validate_message_page(
    query: ListConversationMessagesQuery,
) -> Result<(Option<i64>, i64), (StatusCode, String)> {
    if query.before.is_some_and(|sequence| sequence <= 0) {
        return Err((
            StatusCode::BAD_REQUEST,
            "before must be a positive server sequence".to_string(),
        ));
    }
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err((
            StatusCode::BAD_REQUEST,
            "limit must be between 1 and 100".to_string(),
        ));
    }
    Ok((query.before, limit))
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!("Conversation message API error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Request failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::{ListConversationMessagesQuery, validate_message_content, validate_message_page};

    #[test]
    fn trims_message_content_without_changing_internal_newlines() {
        assert_eq!(
            validate_message_content("  first line\nsecond line  ").unwrap(),
            "first line\nsecond line"
        );
    }

    #[test]
    fn rejects_empty_and_oversized_message_content() {
        assert_eq!(validate_message_content("a").unwrap(), "a");
        assert_eq!(
            validate_message_content(&"a".repeat(2000)).unwrap().len(),
            2000
        );
        assert_eq!(
            validate_message_content(" \n\t ").unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            validate_message_content(&"a".repeat(2001)).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn validates_cursor_pagination_bounds() {
        assert_eq!(
            validate_message_page(ListConversationMessagesQuery {
                before: Some(42),
                limit: None,
            })
            .unwrap(),
            (Some(42), 50)
        );
        assert_eq!(
            validate_message_page(ListConversationMessagesQuery {
                before: None,
                limit: Some(100),
            })
            .unwrap(),
            (None, 100)
        );
        assert!(
            validate_message_page(ListConversationMessagesQuery {
                before: Some(0),
                limit: None,
            })
            .is_err()
        );
        assert!(
            validate_message_page(ListConversationMessagesQuery {
                before: None,
                limit: Some(0),
            })
            .is_err()
        );
        assert!(
            validate_message_page(ListConversationMessagesQuery {
                before: None,
                limit: Some(101),
            })
            .is_err()
        );
    }
}
