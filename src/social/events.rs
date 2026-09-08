use std::sync::Arc;

use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::broadcast,
    time::{Duration, timeout},
};
use uuid::Uuid;

use crate::AppState;

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

#[derive(Clone, Debug)]
struct SocialEvent {
    recipients: Arc<[Uuid]>,
    resource: SocialResource,
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
enum ServerMessage {
    Authenticated,
    Error { message: &'static str },
    Refresh { resource: SocialResource },
}

impl SocialEventHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CAPACITY);
        Self { sender }
    }

    pub fn publish(&self, recipients: impl IntoIterator<Item = Uuid>, resource: SocialResource) {
        let recipients = recipients.into_iter().collect::<Vec<_>>();
        if recipients.is_empty() {
            return;
        }
        let _ = self.sender.send(SocialEvent {
            recipients: recipients.into(),
            resource,
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
                    let Ok(payload) = serde_json::to_string(&ServerMessage::Refresh { resource: event.resource }) else {
                        continue;
                    };
                    if sender.send(Message::Text(payload.into())).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
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

async fn send_message(socket: &mut WebSocket, message: &ServerMessage) -> Result<(), ()> {
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

    use super::{ClientMessage, SocialEventHub, SocialResource, is_allowed_origin};

    #[tokio::test]
    async fn publishes_targeted_resource_events() {
        let hub = SocialEventHub::new();
        let recipient = Uuid::new_v4();
        let mut receiver = hub.subscribe();

        hub.publish([recipient], SocialResource::Friends);

        let event = receiver.recv().await.unwrap();
        assert_eq!(event.recipients.as_ref(), &[recipient]);
        assert_eq!(event.resource, SocialResource::Friends);
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
