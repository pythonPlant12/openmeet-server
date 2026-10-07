use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::Utc;
use diesel::{
    deserialize::QueryableByName,
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
        conversation_hidden_states, conversation_members, conversation_messages, conversations,
        message_reactions, users,
    },
    social::{
        SocialResource,
        models::{
            Conversation, ConversationMessage, ConversationMessageResponse,
            ConversationMessagesResponse, CreateConversationMessageRequest,
            ListConversationMessagesQuery, MessageReactionSummary, MessageReplyPreview,
            NewConversationMessage, ToggleMessageReactionRequest,
        },
    },
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;
type CreateApiResult<T> = Result<(StatusCode, Json<T>), (StatusCode, String)>;
type EmptyResult = Result<StatusCode, (StatusCode, String)>;

const REPLY_PREVIEW_CHARACTERS: usize = 140;
const MAX_REACTION_BYTES: usize = 32;

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
    let reply_to_sequence = request.reply_to_sequence;
    let mut conn = state.pool.get().await.map_err(internal_error)?;

    let (message, sender_nickname, recipients): (ConversationMessage, String, Vec<Uuid>) = conn
        .transaction(|conn| {
            async move {
                authorize_conversation_access(conn, conversation_id, user_id).await?;
                if let Some(sequence) = reply_to_sequence {
                    ensure_message_in_conversation(conn, conversation_id, sequence).await?;
                }
                let recipients = conversation_participants(conn, conversation_id).await?;
                let (sender_name, sender_nickname) = users::table
                    .find(user_id)
                    .select((users::name, users::nickname))
                    .first(conn)
                    .await
                    .map_err(internal_error)?;
                let message = diesel::insert_into(conversation_messages::table)
                    .values(NewConversationMessage {
                        conversation_id,
                        sender_id: user_id,
                        sender_name,
                        content,
                        reply_to_sequence,
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
                diesel::delete(
                    conversation_hidden_states::table
                        .filter(conversation_hidden_states::conversation_id.eq(conversation_id))
                        .filter(conversation_hidden_states::user_id.ne(user_id)),
                )
                .execute(conn)
                .await
                .map_err(internal_error)?;

                Ok((message, sender_nickname, recipients))
            }
            .scope_boxed()
        })
        .await
        .map_err(MessageError::into_api_error)?;

    state
        .social_events
        .publish(recipients, SocialResource::Conversations);

    let reply_to = match message.reply_to_sequence {
        Some(sequence) => reply_previews(&mut conn, conversation_id, &[sequence])
            .await?
            .remove(&sequence),
        None => None,
    };
    Ok((
        StatusCode::CREATED,
        Json(message_response(
            message,
            sender_nickname,
            reply_to,
            Vec::new(),
        )),
    ))
}

/// Adds the viewer's reaction, or removes it when the same emoji is sent again.
pub(crate) async fn toggle_message_reaction(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((conversation_id, sequence)): Path<(Uuid, i64)>,
    Json(request): Json<ToggleMessageReactionRequest>,
) -> ApiResult<Vec<MessageReactionSummary>> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let emoji = validate_reaction(&request.emoji)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let recipients = conn
        .transaction(|conn| {
            async move {
                authorize_conversation_access(conn, conversation_id, user_id).await?;
                ensure_message_in_conversation(conn, conversation_id, sequence).await?;
                let inserted = diesel::insert_into(message_reactions::table)
                    .values((
                        message_reactions::message_sequence.eq(sequence),
                        message_reactions::user_id.eq(user_id),
                        message_reactions::emoji.eq(&emoji),
                    ))
                    .on_conflict_do_nothing()
                    .execute(conn)
                    .await?;
                if inserted == 0 {
                    diesel::delete(
                        message_reactions::table
                            .filter(message_reactions::message_sequence.eq(sequence))
                            .filter(message_reactions::user_id.eq(user_id))
                            .filter(message_reactions::emoji.eq(&emoji)),
                    )
                    .execute(conn)
                    .await?;
                }
                Ok::<_, MessageError>(conversation_participants(conn, conversation_id).await?)
            }
            .scope_boxed()
        })
        .await
        .map_err(MessageError::into_api_error)?;

    state
        .social_events
        .publish(recipients, SocialResource::Conversations);

    Ok(Json(
        reaction_summaries(&mut conn, user_id, &[sequence])
            .await?
            .remove(&sequence)
            .unwrap_or_default(),
    ))
}

async fn ensure_message_in_conversation(
    conn: &mut diesel_async::AsyncPgConnection,
    conversation_id: Uuid,
    sequence: i64,
) -> Result<(), (StatusCode, String)> {
    conversation_messages::table
        .filter(conversation_messages::sequence.eq(sequence))
        .filter(conversation_messages::conversation_id.eq(conversation_id))
        .select(conversation_messages::sequence)
        .first::<i64>(conn)
        .await
        .optional()
        .map_err(internal_error)?
        .map(|_| ())
        .ok_or((StatusCode::NOT_FOUND, "Message not found".to_string()))
}

async fn reply_previews(
    conn: &mut diesel_async::AsyncPgConnection,
    conversation_id: Uuid,
    sequences: &[i64],
) -> Result<HashMap<i64, MessageReplyPreview>, (StatusCode, String)> {
    if sequences.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(i64, Uuid, String, String, String)> = conversation_messages::table
        .inner_join(users::table.on(users::id.eq(conversation_messages::sender_id)))
        .filter(conversation_messages::conversation_id.eq(conversation_id))
        .filter(conversation_messages::sequence.eq_any(sequences))
        .select((
            conversation_messages::sequence,
            conversation_messages::sender_id,
            conversation_messages::sender_name,
            users::nickname,
            conversation_messages::content,
        ))
        .load(conn)
        .await
        .map_err(internal_error)?;
    Ok(rows
        .into_iter()
        .map(
            |(sequence, sender_id, sender_name, sender_nickname, content)| {
                (
                    sequence,
                    MessageReplyPreview {
                        sequence,
                        sender_id,
                        sender_name,
                        sender_nickname,
                        content: excerpt(&content),
                    },
                )
            },
        )
        .collect())
}

#[derive(QueryableByName)]
struct ReactionRow {
    #[diesel(sql_type = BigInt)]
    message_sequence: i64,
    #[diesel(sql_type = Text)]
    emoji: String,
    #[diesel(sql_type = BigInt)]
    count: i64,
    #[diesel(sql_type = Bool)]
    reacted_by_me: bool,
}

async fn reaction_summaries(
    conn: &mut diesel_async::AsyncPgConnection,
    viewer_id: Uuid,
    sequences: &[i64],
) -> Result<HashMap<i64, Vec<MessageReactionSummary>>, (StatusCode, String)> {
    if sequences.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<ReactionRow> = sql_query(
        "SELECT message_sequence, emoji, COUNT(*)::BIGINT AS count,
                BOOL_OR(user_id = $1) AS reacted_by_me
         FROM message_reactions
         WHERE message_sequence = ANY($2)
         GROUP BY message_sequence, emoji
         ORDER BY message_sequence, MIN(created_at), emoji",
    )
    .bind::<SqlUuid, _>(viewer_id)
    .bind::<Array<BigInt>, _>(sequences.to_vec())
    .load(conn)
    .await
    .map_err(internal_error)?;

    let mut summaries: HashMap<i64, Vec<MessageReactionSummary>> = HashMap::new();
    for row in rows {
        summaries
            .entry(row.message_sequence)
            .or_default()
            .push(MessageReactionSummary {
                emoji: row.emoji,
                count: row.count,
                reacted_by_me: row.reacted_by_me,
            });
    }
    Ok(summaries)
}

pub(crate) fn excerpt(content: &str) -> String {
    let mut characters = content.chars();
    let mut preview: String = characters.by_ref().take(REPLY_PREVIEW_CHARACTERS).collect();
    if characters.next().is_some() {
        preview.push('…');
    }
    preview
}

/// Reactions are emoji: short, with no letters, spaces, or control characters.
pub(crate) fn validate_reaction(emoji: &str) -> Result<String, (StatusCode, String)> {
    let emoji = emoji.trim();
    let valid = !emoji.is_empty()
        && emoji.len() <= MAX_REACTION_BYTES
        && emoji.chars().all(|character| {
            !character.is_alphanumeric() && !character.is_whitespace() && !character.is_control()
        });
    if valid {
        Ok(emoji.to_string())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "Reaction must be a single emoji".to_string(),
        ))
    }
}

async fn conversation_participants(
    conn: &mut diesel_async::AsyncPgConnection,
    conversation_id: Uuid,
) -> Result<Vec<Uuid>, (StatusCode, String)> {
    let conversation: Conversation = conversations::table
        .find(conversation_id)
        .select(Conversation::as_select())
        .first(conn)
        .await
        .map_err(internal_error)?;

    if conversation.kind == "direct" {
        return Ok([
            conversation.direct_user_low_id,
            conversation.direct_user_high_id,
        ]
        .into_iter()
        .flatten()
        .collect());
    }

    conversation_members::table
        .filter(conversation_members::conversation_id.eq(conversation_id))
        .select(conversation_members::user_id)
        .load(conn)
        .await
        .map_err(internal_error)
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
    let mut messages: Vec<(ConversationMessage, String)> = message_query
        .inner_join(users::table.on(users::id.eq(conversation_messages::sender_id)))
        .order(conversation_messages::sequence.desc())
        .limit(limit + 1)
        .select((ConversationMessage::as_select(), users::nickname))
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let next_before = if messages.len() as i64 > limit {
        messages.truncate(limit as usize);
        messages.last().map(|(message, _)| message.sequence)
    } else {
        None
    };

    if is_latest_page {
        let latest_sequence = messages
            .as_slice()
            .first()
            .map(|(message, _)| message.sequence)
            .unwrap_or(0);
        mark_messages_read(&mut conn, conversation_id, user_id, latest_sequence).await?;
    }

    let sequences = messages
        .iter()
        .map(|(message, _)| message.sequence)
        .collect::<Vec<_>>();
    let reply_sequences = messages
        .iter()
        .filter_map(|(message, _)| message.reply_to_sequence)
        .collect::<Vec<_>>();
    let replies = reply_previews(&mut conn, conversation_id, &reply_sequences).await?;
    let mut reactions = reaction_summaries(&mut conn, user_id, &sequences).await?;

    Ok(Json(ConversationMessagesResponse {
        messages: messages
            .into_iter()
            .map(|(message, sender_nickname)| {
                let reply_to = message
                    .reply_to_sequence
                    .and_then(|sequence| replies.get(&sequence).cloned());
                let message_reactions = reactions.remove(&message.sequence).unwrap_or_default();
                message_response(message, sender_nickname, reply_to, message_reactions)
            })
            .collect(),
        next_before,
    }))
}

pub(crate) async fn mark_conversation_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    authorize_conversation_access(&mut conn, conversation_id, user_id).await?;
    let latest_sequence: Option<i64> = conversation_messages::table
        .filter(conversation_messages::conversation_id.eq(conversation_id))
        .select(diesel::dsl::max(conversation_messages::sequence))
        .first(&mut conn)
        .await
        .map_err(internal_error)?;
    mark_messages_read(
        &mut conn,
        conversation_id,
        user_id,
        latest_sequence.unwrap_or(0),
    )
    .await?;
    // Other open sessions of the same user refresh their unread badges.
    state
        .social_events
        .publish([user_id], SocialResource::Conversations);
    Ok(StatusCode::NO_CONTENT)
}

/// Flags a conversation as unread for the current user without changing message read progress.
/// Opening the conversation clears the flag.
pub(crate) async fn mark_conversation_unread(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
) -> EmptyResult {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    authorize_conversation_access(&mut conn, conversation_id, user_id).await?;
    sql_query(
        "INSERT INTO conversation_read_states (conversation_id, user_id, marked_unread)
         VALUES ($1, $2, TRUE)
         ON CONFLICT (conversation_id, user_id) DO UPDATE
         SET marked_unread = TRUE, updated_at = NOW()",
    )
    .bind::<SqlUuid, _>(conversation_id)
    .bind::<SqlUuid, _>(user_id)
    .execute(&mut conn)
    .await
    .map_err(internal_error)?;
    state
        .social_events
        .publish([user_id], SocialResource::Conversations);
    Ok(StatusCode::NO_CONTENT)
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
             marked_unread = FALSE,
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

fn message_response(
    message: ConversationMessage,
    sender_nickname: String,
    reply_to: Option<MessageReplyPreview>,
    reactions: Vec<MessageReactionSummary>,
) -> ConversationMessageResponse {
    ConversationMessageResponse {
        sequence: message.sequence,
        conversation_id: message.conversation_id,
        sender_id: message.sender_id,
        sender_name: message.sender_name,
        sender_nickname,
        content: message.content,
        created_at: message.created_at,
        reply_to,
        reactions,
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
    if !(1..=50).contains(&limit) {
        return Err((
            StatusCode::BAD_REQUEST,
            "limit must be between 1 and 50".to_string(),
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

    use super::{
        ListConversationMessagesQuery, excerpt, validate_message_content, validate_message_page,
        validate_reaction,
    };

    #[test]
    fn accepts_emoji_reactions_only() {
        for emoji in ["👍", "❤️", "👨‍👩‍👧", "🇪🇸"] {
            assert_eq!(validate_reaction(emoji).unwrap(), emoji);
        }
        for invalid in ["", "  ", "lol", "a👍", "👍 👍", &"👍".repeat(9)] {
            assert_eq!(
                validate_reaction(invalid).unwrap_err().0,
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[test]
    fn shortens_long_quotes_with_an_ellipsis() {
        assert_eq!(excerpt("short"), "short");
        let long = "a".repeat(200);
        assert_eq!(excerpt(&long).chars().count(), 141);
        assert!(excerpt(&long).ends_with('…'));
    }

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
                limit: Some(50),
            })
            .unwrap(),
            (None, 50)
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
                limit: Some(51),
            })
            .is_err()
        );
    }
}
