//! Shared media for a conversation, as on a contact or group profile: photos and videos, files, voice
//! messages, links, and GIFs, each newest first. Attachments are sorted by how the chat shows them: GIFs
//! and voice messages are named by the client that creates them, so they are told apart by file name.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use diesel::{
    dsl::{count_star, sql},
    prelude::*,
    sql_types::Bool,
};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AppState,
    auth::extract_user_id,
    schema::{conversation_message_attachments as attachments, conversation_messages as messages},
    social::{
        messages::{attachment_url, authorize_conversation_access},
        models::{ConversationMessageAttachment, MessageAttachmentResponse, VoiceMessageDetails},
    },
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

const DEFAULT_PAGE_SIZE: i64 = 30;
const MAX_PAGE_SIZE: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SharedMediaKind {
    Media,
    Files,
    Voice,
    Links,
    Gifs,
}

#[derive(Debug, Deserialize)]
pub struct SharedMediaQuery {
    pub kind: SharedMediaKind,
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedMediaItem {
    pub sequence: i64,
    pub sender_id: Uuid,
    pub sender_name: String,
    pub created_at: DateTime<Utc>,
    /// Set for every kind except links.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment: Option<MessageAttachmentResponse>,
    /// The message text, set for links only; the client finds the links in it as the chat does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedMediaPage {
    pub items: Vec<SharedMediaItem>,
    /// Pass as `before` for older items; absent on the last page.
    pub next_before: Option<i64>,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SharedMediaCounts {
    pub media: i64,
    pub files: i64,
    pub voice: i64,
    pub links: i64,
    pub gifs: i64,
}

// The rules are constant SQL with no user input. Every attachment falls in exactly one tab.
const IS_GIF: &str = "conversation_message_attachments.file_name LIKE 'giphy-%'";
const IS_VOICE: &str = "(conversation_message_attachments.file_name LIKE 'voice-message-%' \
    AND conversation_message_attachments.content_type LIKE 'audio/%')";
const IS_VISUAL: &str = "(conversation_message_attachments.content_type LIKE 'image/%' \
    OR conversation_message_attachments.content_type LIKE 'video/%')";
// Matches what the chat shows as a link: http(s), www., and bare openmeets.eu links.
const HAS_LINK: &str = "(conversation_messages.content ILIKE '%http://%' \
    OR conversation_messages.content ILIKE '%https://%' \
    OR conversation_messages.content ILIKE '%www.%' \
    OR conversation_messages.content ILIKE '%openmeets.eu/%')";

/// Which attachments belong to a tab; links come from message text instead.
fn attachment_condition(kind: SharedMediaKind) -> Option<String> {
    match kind {
        SharedMediaKind::Gifs => Some(IS_GIF.to_string()),
        SharedMediaKind::Voice => Some(IS_VOICE.to_string()),
        SharedMediaKind::Media => Some(format!("({IS_VISUAL} AND NOT {IS_GIF})")),
        SharedMediaKind::Files => Some(format!("NOT ({IS_VISUAL} OR {IS_GIF} OR {IS_VOICE})")),
        SharedMediaKind::Links => None,
    }
}

fn validate_page(query: &SharedMediaQuery) -> Result<i64, (StatusCode, String)> {
    if query.before.is_some_and(|sequence| sequence <= 0) {
        return Err((
            StatusCode::BAD_REQUEST,
            "before must be a positive server sequence".to_string(),
        ));
    }
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("limit must be between 1 and {MAX_PAGE_SIZE}"),
        ));
    }
    Ok(limit)
}

pub(crate) async fn list_shared_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
    Query(query): Query<SharedMediaQuery>,
) -> ApiResult<SharedMediaPage> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let limit = validate_page(&query)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    authorize_conversation_access(&mut conn, conversation_id, user_id).await?;

    let before = query.before.unwrap_or(i64::MAX);
    let page = match attachment_condition(query.kind) {
        Some(condition) => {
            attachment_page(&mut conn, conversation_id, &condition, before, limit).await?
        }
        None => link_page(&mut conn, conversation_id, before, limit).await?,
    };
    Ok(Json(page))
}

/// Pages by message, so a message with several photos is never split across pages.
async fn attachment_page(
    conn: &mut AsyncPgConnection,
    conversation_id: Uuid,
    condition: &str,
    before: i64,
    limit: i64,
) -> Result<SharedMediaPage, (StatusCode, String)> {
    let mut sequences: Vec<i64> = attachments::table
        .inner_join(messages::table)
        .filter(messages::conversation_id.eq(conversation_id))
        .filter(messages::sequence.lt(before))
        .filter(sql::<Bool>(condition))
        .select(messages::sequence)
        .distinct()
        .order(messages::sequence.desc())
        .limit(limit + 1)
        .load(conn)
        .await
        .map_err(internal_error)?;
    let next_before = page_cursor(&mut sequences, limit);
    if sequences.is_empty() {
        return Ok(SharedMediaPage {
            items: Vec::new(),
            next_before,
        });
    }

    let rows: Vec<(ConversationMessageAttachment, (Uuid, String, DateTime<Utc>))> =
        attachments::table
            .inner_join(messages::table)
            .filter(messages::conversation_id.eq(conversation_id))
            .filter(attachments::message_sequence.eq_any(&sequences))
            .filter(sql::<Bool>(condition))
            .order((
                messages::sequence.desc(),
                attachments::created_at.asc(),
                attachments::id.asc(),
            ))
            .select((
                ConversationMessageAttachment::as_select(),
                (
                    messages::sender_id,
                    messages::sender_name,
                    messages::created_at,
                ),
            ))
            .load(conn)
            .await
            .map_err(internal_error)?;
    let items = rows
        .into_iter()
        .map(
            |(attachment, (sender_id, sender_name, created_at))| SharedMediaItem {
                sequence: attachment.message_sequence,
                sender_id,
                sender_name,
                created_at,
                attachment: Some(MessageAttachmentResponse {
                    id: attachment.id,
                    url: attachment_url(conversation_id, attachment.id),
                    file_name: attachment.file_name,
                    content_type: attachment.content_type,
                    byte_size: attachment.byte_size,
                    voice: VoiceMessageDetails::from_columns(
                        attachment.voice_duration_ms,
                        attachment.voice_waveform,
                    ),
                }),
                content: None,
            },
        )
        .collect();
    Ok(SharedMediaPage { items, next_before })
}

async fn link_page(
    conn: &mut AsyncPgConnection,
    conversation_id: Uuid,
    before: i64,
    limit: i64,
) -> Result<SharedMediaPage, (StatusCode, String)> {
    let mut rows: Vec<(i64, Uuid, String, DateTime<Utc>, String)> = messages::table
        .filter(messages::conversation_id.eq(conversation_id))
        .filter(messages::sequence.lt(before))
        .filter(sql::<Bool>(HAS_LINK))
        .order(messages::sequence.desc())
        .limit(limit + 1)
        .select((
            messages::sequence,
            messages::sender_id,
            messages::sender_name,
            messages::created_at,
            messages::content,
        ))
        .load(conn)
        .await
        .map_err(internal_error)?;
    let next_before = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|row| row.0)
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(
            |(sequence, sender_id, sender_name, created_at, content)| SharedMediaItem {
                sequence,
                sender_id,
                sender_name,
                created_at,
                attachment: None,
                content: Some(content),
            },
        )
        .collect();
    Ok(SharedMediaPage { items, next_before })
}

/// Drops the extra row fetched to detect another page and returns the cursor for it.
fn page_cursor(sequences: &mut Vec<i64>, limit: i64) -> Option<i64> {
    if sequences.len() as i64 > limit {
        sequences.truncate(limit as usize);
        sequences.last().copied()
    } else {
        None
    }
}

pub(crate) async fn shared_media_counts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(conversation_id): Path<Uuid>,
) -> ApiResult<SharedMediaCounts> {
    let user_id = extract_user_id(&state.jwt, &headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    authorize_conversation_access(&mut conn, conversation_id, user_id).await?;

    let mut counts = HashMap::new();
    for kind in [
        SharedMediaKind::Media,
        SharedMediaKind::Files,
        SharedMediaKind::Voice,
        SharedMediaKind::Gifs,
    ] {
        let count: i64 = attachments::table
            .inner_join(messages::table)
            .filter(messages::conversation_id.eq(conversation_id))
            .filter(sql::<Bool>(
                &attachment_condition(kind).expect("attachment kind"),
            ))
            .select(count_star())
            .first(&mut conn)
            .await
            .map_err(internal_error)?;
        counts.insert(kind_key(kind), count);
    }
    let links: i64 = messages::table
        .filter(messages::conversation_id.eq(conversation_id))
        .filter(sql::<Bool>(HAS_LINK))
        .select(count_star())
        .first(&mut conn)
        .await
        .map_err(internal_error)?;

    Ok(Json(SharedMediaCounts {
        media: counts["media"],
        files: counts["files"],
        voice: counts["voice"],
        gifs: counts["gifs"],
        links,
    }))
}

fn kind_key(kind: SharedMediaKind) -> &'static str {
    match kind {
        SharedMediaKind::Media => "media",
        SharedMediaKind::Files => "files",
        SharedMediaKind::Voice => "voice",
        SharedMediaKind::Links => "links",
        SharedMediaKind::Gifs => "gifs",
    }
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!("Shared media API error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Internal server error".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::{SharedMediaKind, SharedMediaQuery, page_cursor, validate_page};

    #[test]
    fn validates_shared_media_pages() {
        let query = |before, limit| SharedMediaQuery {
            kind: SharedMediaKind::Media,
            before,
            limit,
        };
        assert_eq!(validate_page(&query(None, None)).unwrap(), 30);
        assert_eq!(validate_page(&query(Some(9), Some(60))).unwrap(), 60);
        assert!(validate_page(&query(Some(0), None)).is_err());
        assert!(validate_page(&query(None, Some(0))).is_err());
        assert!(validate_page(&query(None, Some(61))).is_err());
    }

    #[test]
    fn keeps_a_cursor_only_when_more_items_exist() {
        let mut full = vec![9, 7, 4];
        assert_eq!(page_cursor(&mut full, 2), Some(7));
        assert_eq!(full, vec![9, 7]);
        let mut last = vec![9, 7];
        assert_eq!(page_cursor(&mut last, 2), None);
    }

    #[test]
    fn reads_tab_names_from_the_query_string() {
        let kind: SharedMediaKind = serde_json::from_str("\"gifs\"").unwrap();
        assert_eq!(kind, SharedMediaKind::Gifs);
        assert!(serde_json::from_str::<SharedMediaKind>("\"music\"").is_err());
    }
}
