use std::collections::HashMap;

use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path, Query, State, multipart::Field},
    http::{HeaderMap, StatusCode, header},
    response::Response,
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
        conversation_hidden_states, conversation_members, conversation_message_attachments,
        conversation_messages, conversation_read_states, conversations, message_reactions, users,
    },
    social::{
        SocialResource,
        models::{
            Conversation, ConversationMessage, ConversationMessageAttachment,
            ConversationMessageResponse, ConversationMessagesResponse,
            CreateConversationMessageRequest, ListConversationMessagesQuery,
            MessageAttachmentResponse, MessageReactionSummary, MessageReplyPreview,
            NewConversationMessage, NewConversationMessageAttachment, ToggleMessageReactionRequest,
        },
    },
    storage::AvatarStorage,
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;
type CreateApiResult<T> = Result<(StatusCode, Json<T>), (StatusCode, String)>;
type EmptyResult = Result<StatusCode, (StatusCode, String)>;

const REPLY_PREVIEW_CHARACTERS: usize = 140;
const MAX_REACTION_BYTES: usize = 32;
const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const MAX_ATTACHMENT_TOTAL_BYTES: usize = 25 * 1024 * 1024;
const MAX_ATTACHMENTS_PER_MESSAGE: usize = 10;
const MAX_FILE_NAME_BYTES: usize = 255;
const MAX_CONTENT_TYPE_BYTES: usize = 255;
const MAX_REPLY_TO_SEQUENCE_BYTES: usize = 64;
const MAX_MESSAGE_CONTENT_BYTES: usize = 8_000;
pub(crate) const MAX_ATTACHMENT_REQUEST_BYTES: usize = MAX_ATTACHMENT_TOTAL_BYTES + 1024 * 1024;

struct PendingAttachment {
    id: Uuid,
    storage_key: String,
    file_name: String,
    content_type: String,
    bytes: Vec<u8>,
}

#[derive(Clone)]
struct UploadedAttachment {
    id: Uuid,
    storage_key: String,
    file_name: String,
    content_type: String,
    byte_size: i64,
}

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
            Vec::new(),
        )),
    ))
}

pub(crate) async fn create_message_with_attachments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
    multipart: Multipart,
) -> CreateApiResult<ConversationMessageResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    // Check membership before accepting or writing attachment data.
    {
        let mut conn = state.pool.get().await.map_err(internal_error)?;
        authorize_conversation_access(&mut conn, conversation_id, user_id).await?;
    }
    let (content, reply_to_sequence, pending_attachments) =
        parse_attachment_message(multipart, conversation_id).await?;

    let storage = state.avatar_storage.clone();
    let uploaded_attachments = upload_attachments(storage.as_ref(), pending_attachments).await?;
    let uploaded_keys = uploaded_attachments
        .iter()
        .map(|attachment| attachment.storage_key.clone())
        .collect::<Vec<_>>();
    let attachment_responses = uploaded_attachments
        .iter()
        .map(|attachment| attachment_response(conversation_id, attachment))
        .collect::<Vec<_>>();
    let mut conn = match state.pool.get().await {
        Ok(conn) => conn,
        Err(error) => {
            delete_uploaded_attachments(storage.as_ref(), &uploaded_keys).await;
            return Err(internal_error(error));
        }
    };

    let result: Result<(ConversationMessage, String, Vec<Uuid>), MessageError> = conn
        .transaction(|conn| {
            let attachments = &uploaded_attachments;
            async move {
                // Membership and reply target can change while multipart data is being received.
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
                if !attachments.is_empty() {
                    let rows = attachments
                        .iter()
                        .map(|attachment| NewConversationMessageAttachment {
                            id: attachment.id,
                            message_sequence: message.sequence,
                            storage_key: attachment.storage_key.clone(),
                            file_name: attachment.file_name.clone(),
                            content_type: attachment.content_type.clone(),
                            byte_size: attachment.byte_size,
                        })
                        .collect::<Vec<_>>();
                    diesel::insert_into(conversation_message_attachments::table)
                        .values(&rows)
                        .execute(conn)
                        .await
                        .map_err(internal_error)?;
                }
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
        .await;
    let (message, sender_nickname, recipients) = match result {
        Ok(result) => result,
        Err(error) => {
            delete_uploaded_attachments(storage.as_ref(), &uploaded_keys).await;
            return Err(error.into_api_error());
        }
    };

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
            attachment_responses,
        )),
    ))
}

pub(crate) async fn get_message_attachment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((conversation_id, attachment_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, (StatusCode, String)> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    authorize_conversation_access(&mut conn, conversation_id, user_id).await?;

    let attachment = conversation_message_attachments::table
        .inner_join(conversation_messages::table.on(
            conversation_messages::sequence.eq(conversation_message_attachments::message_sequence),
        ))
        .filter(conversation_message_attachments::id.eq(attachment_id))
        .filter(conversation_messages::conversation_id.eq(conversation_id))
        .select(ConversationMessageAttachment::as_select())
        .first(&mut conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::NotFound => {
                (StatusCode::NOT_FOUND, "Attachment not found".to_string())
            }
            _ => internal_error(error),
        })?;
    drop(conn);
    let object = state
        .avatar_storage
        .download(&attachment.storage_key)
        .await
        .map_err(attachment_storage_error)?;
    let content_type = attachment.content_type;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type.clone())
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition(&attachment.file_name, &content_type),
        )
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(Body::from(object.bytes))
        .map_err(internal_error)
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

    // Capture this before reading the latest page advances the read state. The client needs the
    // exact boundary even when the unread range spans more than one history page.
    let first_unread_sequence = if is_latest_page {
        let last_read_sequence = conversation_read_states::table
            .find((conversation_id, user_id))
            .select(conversation_read_states::last_read_sequence)
            .first::<i64>(&mut conn)
            .await
            .optional()
            .map_err(internal_error)?
            .unwrap_or(0);
        conversation_messages::table
            .filter(conversation_messages::conversation_id.eq(conversation_id))
            .filter(conversation_messages::sender_id.ne(user_id))
            .filter(conversation_messages::sequence.gt(last_read_sequence))
            .select(diesel::dsl::min(conversation_messages::sequence))
            .first::<Option<i64>>(&mut conn)
            .await
            .map_err(internal_error)?
    } else {
        None
    };

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
    let mut attachments = message_attachments(&mut conn, conversation_id, &sequences).await?;

    Ok(Json(ConversationMessagesResponse {
        messages: messages
            .into_iter()
            .map(|(message, sender_nickname)| {
                let reply_to = message
                    .reply_to_sequence
                    .and_then(|sequence| replies.get(&sequence).cloned());
                let message_reactions = reactions.remove(&message.sequence).unwrap_or_default();
                let message_attachments = attachments.remove(&message.sequence).unwrap_or_default();
                message_response(
                    message,
                    sender_nickname,
                    reply_to,
                    message_reactions,
                    message_attachments,
                )
            })
            .collect(),
        next_before,
        first_unread_sequence,
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
    attachments: Vec<MessageAttachmentResponse>,
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
        attachments,
    }
}

async fn message_attachments(
    conn: &mut diesel_async::AsyncPgConnection,
    conversation_id: Uuid,
    sequences: &[i64],
) -> Result<HashMap<i64, Vec<MessageAttachmentResponse>>, (StatusCode, String)> {
    if sequences.is_empty() {
        return Ok(HashMap::new());
    }
    let attachments = conversation_message_attachments::table
        .inner_join(conversation_messages::table.on(
            conversation_messages::sequence.eq(conversation_message_attachments::message_sequence),
        ))
        .filter(conversation_messages::conversation_id.eq(conversation_id))
        .filter(conversation_message_attachments::message_sequence.eq_any(sequences))
        .order(conversation_message_attachments::created_at.asc())
        .select(ConversationMessageAttachment::as_select())
        .load(conn)
        .await
        .map_err(internal_error)?;
    let mut responses = HashMap::new();
    for attachment in attachments {
        responses
            .entry(attachment.message_sequence)
            .or_insert_with(Vec::new)
            .push(MessageAttachmentResponse {
                id: attachment.id,
                file_name: attachment.file_name,
                content_type: attachment.content_type,
                byte_size: attachment.byte_size,
                url: attachment_url(conversation_id, attachment.id),
            });
    }
    Ok(responses)
}

fn attachment_response(
    conversation_id: Uuid,
    attachment: &UploadedAttachment,
) -> MessageAttachmentResponse {
    MessageAttachmentResponse {
        id: attachment.id,
        file_name: attachment.file_name.clone(),
        content_type: attachment.content_type.clone(),
        byte_size: attachment.byte_size,
        url: attachment_url(conversation_id, attachment.id),
    }
}

fn attachment_url(conversation_id: Uuid, attachment_id: Uuid) -> String {
    format!("/social/conversations/{conversation_id}/attachments/{attachment_id}")
}

async fn parse_attachment_message(
    mut multipart: Multipart,
    conversation_id: Uuid,
) -> Result<(String, Option<i64>, Vec<PendingAttachment>), (StatusCode, String)> {
    let mut content = None;
    let mut reply_to_sequence = None;
    let mut attachments = Vec::new();
    let mut attachment_bytes = 0;

    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        let name = field.name().map(str::to_owned).ok_or((
            StatusCode::BAD_REQUEST,
            "Multipart field name is required".to_string(),
        ))?;
        match name.as_str() {
            "content" => {
                if content.is_some() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "content may only be provided once".to_string(),
                    ));
                }
                let bytes = read_multipart_field(field, MAX_MESSAGE_CONTENT_BYTES).await?;
                content = Some(String::from_utf8(bytes).map_err(|_| {
                    (
                        StatusCode::BAD_REQUEST,
                        "content must be valid UTF-8".to_string(),
                    )
                })?);
            }
            "replyToSequence" => {
                if reply_to_sequence.is_some() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "replyToSequence may only be provided once".to_string(),
                    ));
                }
                let bytes = read_multipart_field(field, MAX_REPLY_TO_SEQUENCE_BYTES).await?;
                let value = String::from_utf8(bytes).map_err(|_| {
                    (
                        StatusCode::BAD_REQUEST,
                        "replyToSequence must be an integer".to_string(),
                    )
                })?;
                reply_to_sequence = Some(parse_reply_to_sequence(&value)?);
            }
            "file" => {
                if attachments.len() == MAX_ATTACHMENTS_PER_MESSAGE {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "A message may contain at most 10 attachments".to_string(),
                    ));
                }
                let file_name = field.file_name().map(str::to_owned);
                let content_type = field.content_type().map(ToString::to_string);
                let bytes = read_multipart_field(field, MAX_ATTACHMENT_BYTES).await?;
                attachment_bytes += bytes.len();
                if attachment_bytes > MAX_ATTACHMENT_TOTAL_BYTES {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!(
                            "Message attachments must not exceed {MAX_ATTACHMENT_TOTAL_BYTES} bytes"
                        ),
                    ));
                }
                let (file_name, content_type) = validate_attachment_metadata(
                    file_name.as_deref(),
                    content_type.as_deref(),
                    bytes.len(),
                )?;
                let id = Uuid::new_v4();
                attachments.push(PendingAttachment {
                    id,
                    storage_key: format!("message-attachments/{conversation_id}/{id}"),
                    file_name,
                    content_type,
                    bytes,
                });
            }
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Unexpected multipart field".to_string(),
                ));
            }
        }
    }

    let content = validate_attachment_content(content.as_deref().unwrap_or_default())?;
    if content.is_empty() && attachments.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Message must contain content or an attachment".to_string(),
        ));
    }
    Ok((content, reply_to_sequence, attachments))
}

async fn read_multipart_field(
    mut field: Field<'_>,
    max_bytes: usize,
) -> Result<Vec<u8>, (StatusCode, String)> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
        if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("Attachment must not exceed {MAX_ATTACHMENT_BYTES} bytes"),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn upload_attachments(
    storage: &dyn AvatarStorage,
    attachments: Vec<PendingAttachment>,
) -> Result<Vec<UploadedAttachment>, (StatusCode, String)> {
    let mut uploaded = Vec::with_capacity(attachments.len());
    let mut uploaded_keys = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        uploaded_keys.push(attachment.storage_key.clone());
        let byte_size = attachment.bytes.len() as i64;
        if let Err(error) = storage
            .upload(
                &attachment.storage_key,
                &attachment.content_type,
                attachment.bytes,
            )
            .await
        {
            delete_uploaded_attachments(storage, &uploaded_keys).await;
            return Err(attachment_storage_error(error));
        }
        uploaded.push(UploadedAttachment {
            id: attachment.id,
            storage_key: attachment.storage_key,
            file_name: attachment.file_name,
            content_type: attachment.content_type,
            byte_size,
        });
    }
    Ok(uploaded)
}

async fn delete_uploaded_attachments(storage: &dyn AvatarStorage, keys: &[String]) {
    for key in keys {
        if let Err(error) = storage.delete(key).await {
            tracing::error!(%error, %key, "Failed to remove message attachment after request failure");
        }
    }
}

fn multipart_error(error: axum::extract::multipart::MultipartError) -> (StatusCode, String) {
    tracing::warn!(%error, "Invalid message attachment upload");
    (
        StatusCode::BAD_REQUEST,
        "Invalid message attachment upload".to_string(),
    )
}

fn attachment_storage_error(error: anyhow::Error) -> (StatusCode, String) {
    tracing::error!(%error, "Message attachment storage error");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "Attachment storage is unavailable".to_string(),
    )
}

fn validate_attachment_content(content: &str) -> Result<String, (StatusCode, String)> {
    let content = content.trim();
    if content.chars().count() > 2000 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Message content must not exceed 2000 characters".to_string(),
        ));
    }
    Ok(content.to_string())
}

fn parse_reply_to_sequence(value: &str) -> Result<i64, (StatusCode, String)> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or((
            StatusCode::BAD_REQUEST,
            "replyToSequence must be a positive integer".to_string(),
        ))
}

fn validate_attachment_metadata(
    file_name: Option<&str>,
    content_type: Option<&str>,
    byte_size: usize,
) -> Result<(String, String), (StatusCode, String)> {
    if !(1..=MAX_ATTACHMENT_BYTES).contains(&byte_size) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Attachment must be between 1 and {MAX_ATTACHMENT_BYTES} bytes"),
        ));
    }
    let file_name = file_name.unwrap_or_default().trim();
    if file_name.is_empty()
        || file_name.len() > MAX_FILE_NAME_BYTES
        || file_name.chars().any(char::is_control)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Attachment file name is invalid".to_string(),
        ));
    }
    let content_type = content_type.unwrap_or("application/octet-stream");
    if content_type.is_empty()
        || content_type.len() > MAX_CONTENT_TYPE_BYTES
        || content_type.chars().any(char::is_control)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Attachment content type is invalid".to_string(),
        ));
    }
    Ok((file_name.to_string(), content_type.to_ascii_lowercase()))
}

fn content_disposition(file_name: &str, content_type: &str) -> String {
    let disposition = if matches!(
        content_type,
        "image/jpeg" | "image/png" | "image/webp" | "image/gif" | "video/mp4" | "video/webm"
    ) {
        "inline"
    } else {
        "attachment"
    };
    let safe_file_name = file_name
        .chars()
        .filter_map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | ' ' | '.' | '-' | '_' | '(' | ')' => {
                Some(character)
            }
            _ => Some('_'),
        })
        .take(180)
        .collect::<String>();
    let safe_file_name = safe_file_name.trim();
    let safe_file_name = (!safe_file_name.is_empty())
        .then_some(safe_file_name)
        .unwrap_or("attachment");
    format!("{disposition}; filename=\"{safe_file_name}\"")
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
        ListConversationMessagesQuery, content_disposition, excerpt, parse_reply_to_sequence,
        validate_attachment_content, validate_attachment_metadata, validate_message_content,
        validate_message_page, validate_reaction,
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
    fn validates_attachment_message_fields() {
        assert_eq!(validate_attachment_content("  hello  ").unwrap(), "hello");
        assert!(validate_attachment_content(&"a".repeat(2001)).is_err());
        assert_eq!(parse_reply_to_sequence(" 42 ").unwrap(), 42);
        assert!(parse_reply_to_sequence("0").is_err());
        assert!(parse_reply_to_sequence("nope").is_err());
        assert_eq!(
            validate_attachment_metadata(Some("report.pdf"), None, 1).unwrap(),
            (
                "report.pdf".to_string(),
                "application/octet-stream".to_string()
            )
        );
        assert!(validate_attachment_metadata(Some("\r\n"), Some("text/plain"), 1).is_err());
        assert!(validate_attachment_metadata(Some("file"), Some("text/plain"), 0).is_err());
    }

    #[test]
    fn uses_safe_attachment_dispositions() {
        assert_eq!(
            content_disposition("photo.png", "image/png"),
            "inline; filename=\"photo.png\""
        );
        assert_eq!(
            content_disposition("evil\"\r\n.txt", "text/html"),
            "attachment; filename=\"evil___.txt\""
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
