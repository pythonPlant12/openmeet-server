use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::signaling::message::SignalingMessage;
use crate::sfu::{
    participant::{Participant, ParticipantConnection},
    repository::RoomRepository,
};

/// WebSocket handler - upgrades HTTP to WebSocket
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(room_repo): State<Arc<dyn RoomRepository>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, room_repo))
}

/// Handle an individual WebSocket connection
async fn handle_socket(socket: WebSocket, room_repo: Arc<dyn RoomRepository>) {
    let (mut sender, mut receiver) = socket.split();

    // Channel for sending messages to this client
    let (tx, mut rx) = mpsc::unbounded_channel::<SignalingMessage>();

    let participant_id = Uuid::new_v4().to_string();
    info!("New WebSocket connection: {}", participant_id);

    // Task to send messages from the channel to the WebSocket
    let mut send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            // Serialize message to JSON
            match serde_json::to_string(&message) {
                Ok(json) => {
                    if sender.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    error!("Failed to serialize message: {}", e);
                }
            }
        }
    });

    // Variables to track this participant's state
    let mut current_room_id: Option<String> = None;
    let mut participant_name: Option<String> = None;

    // Clone for use in recv_task (Arc clone is cheap, String clone is needed)
    let room_repo_clone = Arc::clone(&room_repo);
    let participant_id_clone = participant_id.clone();

    // Task to receive messages from WebSocket and handle them
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(message)) = receiver.next().await {
            match message {
                Message::Text(text) => {
                    // Parse the JSON message
                    match serde_json::from_str::<SignalingMessage>(&text) {
                        Ok(msg) => {
                            handle_message(
                                msg,
                                &participant_id_clone,
                                &mut current_room_id,
                                &mut participant_name,
                                &tx,
                                &room_repo_clone,
                            )
                            .await;
                        }
                        Err(e) => {
                            warn!("Failed to parse message: {}", e);
                            let _ = tx.send(SignalingMessage::Error {
                                message: format!("Invalid message format: {}", e),
                            });
                        }
                    }
                }
                Message::Close(_) => {
                    info!("WebSocket closing for participant: {}", participant_id_clone);
                    break;
                }
                _ => {}
            }
        }

        (current_room_id, participant_id_clone)
    });

    // Wait for either task to finish
    tokio::select! {
        _ = &mut send_task => {
            recv_task.abort();
        }
        result = &mut recv_task => {
            send_task.abort();

            // Clean up: remove participant from room
            if let Ok((Some(room_id), participant_id)) = result {
                if let Some(room_lock) = room_repo.get_room(&room_id).await {
                    let mut room = room_lock.write().await;
                    room.remove_participant(&participant_id);

                    // Delete room if empty
                    if room.is_empty() {
                        drop(room); // Release lock before deleting
                        let _ = room_repo.delete_room(&room_id).await;
                        info!("Deleted empty room: {}", room_id);
                    }
                }
            }
        }
    }

    info!("WebSocket connection closed: {}", participant_id);
}

/// Handle individual signaling messages
async fn handle_message(
    message: SignalingMessage,
    participant_id: &str,
    current_room_id: &mut Option<String>,
    participant_name: &mut Option<String>,
    tx: &mpsc::UnboundedSender<SignalingMessage>,
    room_repo: &Arc<dyn RoomRepository>,
) {
    match message {
        SignalingMessage::Join {
            room_id,
            participant_name: name,
        } => {
            info!(
                "Participant {} ({}) joining room {}",
                name, participant_id, room_id
            );

            // Create room if it doesn't exist
            if !room_repo.room_exists(&room_id).await {
                if let Err(e) = room_repo.create_room(room_id.clone()).await {
                    error!("Failed to create room: {}", e);
                    let _ = tx.send(SignalingMessage::Error {
                        message: format!("Failed to create room: {}", e),
                    });
                    return;
                }
            }

            // Get the room
            if let Some(room_lock) = room_repo.get_room(&room_id).await {
                let mut room = room_lock.write().await;

                // Get list of existing participants before adding new one
                let existing_participants = room.get_participants_info();

                // Create participant and add to room
                let participant = Participant::new(participant_id.to_string(), name.clone());
                let participant_conn = ParticipantConnection::new(participant, tx.clone());

                room.add_participant(participant_conn);

                // Update state
                *current_room_id = Some(room_id.clone());
                *participant_name = Some(name.clone());

                // Send confirmation to the joining participant
                let _ = tx.send(SignalingMessage::Joined {
                    participant_id: participant_id.to_string(),
                    participant_name: name,
                });

                // Send list of existing participants to the new joiner
                for (id, name) in existing_participants {
                    let _ = tx.send(SignalingMessage::ParticipantJoined {
                        participant_id: id,
                        participant_name: name,
                    });
                }

                info!(
                    "Participant {} successfully joined room {} ({} participants)",
                    participant_id,
                    room_id,
                    room.participant_count()
                );
            } else {
                error!("Room {} not found after creation", room_id);
                let _ = tx.send(SignalingMessage::Error {
                    message: "Room not found".to_string(),
                });
            }
        }

        SignalingMessage::Offer { target_id, sdp } => {
            if let Some(room_id) = current_room_id {
                if let Some(room_lock) = room_repo.get_room(room_id).await {
                    let room = room_lock.read().await;
                    let _ = room.send_to(
                        &target_id,
                        SignalingMessage::Offer {
                            target_id: participant_id.to_string(),
                            sdp,
                        },
                    );
                }
            }
        }

        SignalingMessage::Answer { target_id, sdp } => {
            if let Some(room_id) = current_room_id {
                if let Some(room_lock) = room_repo.get_room(room_id).await {
                    let room = room_lock.read().await;
                    let _ = room.send_to(
                        &target_id,
                        SignalingMessage::Answer {
                            target_id: participant_id.to_string(),
                            sdp,
                        },
                    );
                }
            }
        }

        SignalingMessage::IceCandidate {
            target_id,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => {
            if let Some(room_id) = current_room_id {
                if let Some(room_lock) = room_repo.get_room(room_id).await {
                    let room = room_lock.read().await;
                    let _ = room.send_to(
                        &target_id,
                        SignalingMessage::IceCandidate {
                            target_id: participant_id.to_string(),
                            candidate,
                            sdp_mid,
                            sdp_m_line_index,
                        },
                    );
                }
            }
        }

        SignalingMessage::MediaStateChanged {
            participant_id: _,
            audio_enabled,
            video_enabled,
        } => {
            if let Some(room_id) = current_room_id {
                if let Some(room_lock) = room_repo.get_room(room_id).await {
                    let room = room_lock.read().await;
                    room.broadcast(SignalingMessage::MediaStateChanged {
                        participant_id: participant_id.to_string(),
                        audio_enabled,
                        video_enabled,
                    });
                }
            }
        }

        _ => {
            warn!("Unhandled message type from {}", participant_id);
        }
    }
}
