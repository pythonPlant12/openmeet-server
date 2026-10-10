use std::sync::Arc;

use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::broadcast,
    time::{Duration, timeout},
};
use uuid::Uuid;

use crate::{AppState, schema::users, social::models::UserStatus};

const EVENT_CAPACITY: usize = 1024;
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const AUTH_MAX_MESSAGE_SIZE: usize = 4 * 1024;

#[derive(Clone)]
pub struct SocialEventHub {
    sender: broadcast::Sender<SocialEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SocialResource {
    Calls,
    Conversations,
    Friends,
    Notifications,
}

/// Something the user should hear about even when OpenMeet is not in front of them. Clients show
/// it as an OS notification. Push delivery can reuse the same payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SocialAlert {
    Message {
        conversation_id: Uuid,
        sequence: i64,
        sender_id: Uuid,
        sender_name: String,
        /// Group title; `None` for direct conversations.
        conversation_title: Option<String>,
        preview: String,
        has_attachments: bool,
    },
    IncomingCall {
        call_id: Uuid,
        call_kind: AlertCallKind,
        caller_name: String,
        conversation_id: Option<Uuid>,
        conversation_title: Option<String>,
        expires_at: DateTime<Utc>,
    },
}

/// Which endpoint answers the call: `/social/calls/{id}/respond` or `/call-sessions/{id}/respond`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AlertCallKind {
    Invitation,
    Session,
}

#[derive(Clone, Debug)]
enum SocialEventPayload {
    Refresh(SocialResource),
    Alert(Arc<SocialAlert>),
}

#[derive(Clone, Debug)]
struct SocialEvent {
    recipients: Arc<[Uuid]>,
    payload: SocialEventPayload,
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum ClientMessage {
    Authenticate { access_token: String },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ServerMessage<'a> {
    Authenticated,
    Error { message: &'static str },
    Refresh { resource: SocialResource },
    Alert { alert: &'a SocialAlert },
}

impl SocialEventHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CAPACITY);
        Self { sender }
    }

    pub fn publish(&self, recipients: impl IntoIterator<Item = Uuid>, resource: SocialResource) {
        self.send(recipients, SocialEventPayload::Refresh(resource));
    }

    /// Sends `alert` to the recipients whose status is Online. Away, Do not disturb, Sleeping, and
    /// Appear offline mean the user does not want to be interrupted, so they get no alert. The
    /// alert is best effort: a failed status lookup only logs, because the change it reports is
    /// already saved and the refresh event still reaches the client.
    pub async fn alert(
        &self,
        conn: &mut AsyncPgConnection,
        recipients: impl IntoIterator<Item = Uuid>,
        alert: SocialAlert,
    ) {
        let recipients = recipients.into_iter().collect::<Vec<_>>();
        if recipients.is_empty() {
            return;
        }
        let available = users::table
            .filter(users::id.eq_any(&recipients))
            .filter(users::status.eq(UserStatus::Available.as_db_value()))
            .select(users::id)
            .load::<Uuid>(conn)
            .await;
        match available {
            Ok(available) => self.send(available, SocialEventPayload::Alert(Arc::new(alert))),
            Err(error) => tracing::warn!(%error, "Failed to load alert recipients"),
        }
    }

    fn send(&self, recipients: impl IntoIterator<Item = Uuid>, payload: SocialEventPayload) {
        let recipients = recipients.into_iter().collect::<Vec<_>>();
        if recipients.is_empty() {
            return;
        }
        let _ = self.sender.send(SocialEvent {
            recipients: recipients.into(),
            payload,
        });
    }

    fn subscribe(&self) -> broadcast::Receiver<SocialEvent> {
        self.sender.subscribe()
    }
}

pub async fn social_events_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !is_allowed_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(AUTH_MAX_MESSAGE_SIZE)
        .max_frame_size(AUTH_MAX_MESSAGE_SIZE)
        .on_upgrade(move |socket| handle_socket(state, socket))
}

async fn handle_socket(state: AppState, mut socket: WebSocket) {
    let Some((user_id, expires_at)) = authenticate(&state, &mut socket).await else {
        return;
    };
    if send_message(&mut socket, &ServerMessage::Authenticated)
        .await
        .is_err()
    {
        return;
    }

    let mut events = state.social_events.subscribe();
    let (mut sender, mut receiver) = socket.split();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    let mut awaiting_pong = false;
    let expires_in = expires_at
        .saturating_sub(chrono::Utc::now().timestamp())
        .max(0) as u64;
    let access_token_expiry = tokio::time::sleep(Duration::from_secs(expires_in));
    tokio::pin!(access_token_expiry);
    loop {
        tokio::select! {
            incoming = receiver.next() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(Message::Pong(_))) => awaiting_pong = false,
                _ => {}
            },
            event = events.recv() => match event {
                Ok(event) if event.recipients.contains(&user_id) => {
                    let message = match &event.payload {
                        SocialEventPayload::Refresh(resource) => ServerMessage::Refresh { resource: *resource },
                        SocialEventPayload::Alert(alert) => ServerMessage::Alert { alert },
                    };
                    let Ok(payload) = serde_json::to_string(&message) else {
                        continue;
                    };
                    if sender.send(Message::Text(payload.into())).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                // Missed alerts are not replayed: they are only worth showing when they are new.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    for resource in [SocialResource::Calls, SocialResource::Conversations, SocialResource::Friends, SocialResource::Notifications] {
                        let Ok(payload) = serde_json::to_string(&ServerMessage::Refresh { resource }) else {
                            continue;
                        };
                        if sender.send(Message::Text(payload.into())).await.is_err() {
                            return;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = heartbeat.tick() => {
                if awaiting_pong {
                    break;
                }
                awaiting_pong = true;
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            },
            _ = &mut access_token_expiry => {
                let payload = serde_json::to_string(&ServerMessage::Error {
                    message: "Access token expired",
                });
                if let Ok(payload) = payload {
                    let _ = sender.send(Message::Text(payload.into())).await;
                }
                break;
            }
        }
    }
}

fn is_allowed_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let origins = std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_else(|_| {
        "http://localhost:5173,http://127.0.0.1:5173,http://localhost:5174,http://127.0.0.1:5174"
            .to_string()
    });
    origins.split(',').any(|allowed| allowed.trim() == origin)
}

async fn authenticate(state: &AppState, socket: &mut WebSocket) -> Option<(Uuid, i64)> {
    let message = timeout(AUTH_TIMEOUT, socket.recv()).await.ok()??.ok()?;
    let Message::Text(payload) = message else {
        let _ = send_message(
            socket,
            &ServerMessage::Error {
                message: "Authentication required",
            },
        )
        .await;
        return None;
    };
    let Ok(ClientMessage::Authenticate { access_token }) = serde_json::from_str(payload.as_str())
    else {
        let _ = send_message(
            socket,
            &ServerMessage::Error {
                message: "Authentication required",
            },
        )
        .await;
        return None;
    };
    match state
        .jwt
        .validate_access_token_with_expiration(&access_token)
    {
        Ok(identity) => Some(identity),
        Err(_) => {
            let _ = send_message(
                socket,
                &ServerMessage::Error {
                    message: "Invalid access token",
                },
            )
            .await;
            None
        }
    }
}

async fn send_message(socket: &mut WebSocket, message: &ServerMessage<'_>) -> Result<(), ()> {
    let payload = serde_json::to_string(message).map_err(|_| ())?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};
    use uuid::Uuid;

    use super::{
        AlertCallKind, ClientMessage, ServerMessage, SocialAlert, SocialEventHub,
        SocialEventPayload, SocialResource, is_allowed_origin,
    };

    #[tokio::test]
    async fn publishes_targeted_resource_events() {
        let hub = SocialEventHub::new();
        let recipient = Uuid::new_v4();
        let mut receiver = hub.subscribe();

        hub.publish([recipient], SocialResource::Friends);

        let event = receiver.recv().await.unwrap();
        assert_eq!(event.recipients.as_ref(), &[recipient]);
        assert!(matches!(
            event.payload,
            SocialEventPayload::Refresh(SocialResource::Friends)
        ));
    }

    #[test]
    fn serializes_alerts_with_camel_case_kind_and_fields() {
        let call_id = Uuid::nil();
        let alert = SocialAlert::IncomingCall {
            call_id,
            call_kind: AlertCallKind::Session,
            caller_name: "Ada".to_string(),
            conversation_id: None,
            conversation_title: None,
            expires_at: chrono::DateTime::UNIX_EPOCH,
        };

        let json = serde_json::to_value(ServerMessage::Alert { alert: &alert }).unwrap();

        assert_eq!(json["type"], "alert");
        assert_eq!(json["alert"]["kind"], "incomingCall");
        assert_eq!(json["alert"]["callKind"], "session");
        assert_eq!(json["alert"]["callerName"], "Ada");
        assert_eq!(json["alert"]["callId"], call_id.to_string());
    }

    #[test]
    fn parses_camel_case_authentication_message() {
        let message: ClientMessage =
            serde_json::from_str(r#"{"type":"authenticate","accessToken":"signed-token"}"#)
                .unwrap();

        assert!(
            matches!(message, ClientMessage::Authenticate { access_token } if access_token == "signed-token")
        );
    }

    #[test]
    fn accepts_only_configured_websocket_origins() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:5174"),
        );
        assert!(is_allowed_origin(&headers));

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://attacker.example"),
        );
        assert!(!is_allowed_origin(&headers));
    }
}
