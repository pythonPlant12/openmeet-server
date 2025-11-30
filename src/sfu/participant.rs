use tokio::sync::{mpsc, Mutex};
use std::sync::Arc;
use crate::signaling::message::SignalingMessage;
use crate::sfu::peer_connection::SfuPeerConnection;

/// Represents a participant in a video conference room
#[derive(Debug, Clone)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub audio_enabled: bool,
    pub video_enabled: bool,
}

impl Participant {
    pub fn new(id: String, name: String) -> Self {
        Self {
            id,
            name,
            audio_enabled: true,
            video_enabled: true,
        }
    }

    pub fn toggle_audio(&mut self, enabled: bool) {
        self.audio_enabled = enabled;
    }

    pub fn toggle_video(&mut self, enabled: bool) {
        self.video_enabled = enabled;
    }
}

/// Connection info for a participant (WebSocket sender + WebRTC peer connection)
pub struct ParticipantConnection {
    pub participant: Participant,
    pub sender: mpsc::UnboundedSender<SignalingMessage>,
    pub peer_connection: Option<Arc<Mutex<SfuPeerConnection>>>,
}

impl ParticipantConnection {
    pub fn new(participant: Participant, sender: mpsc::UnboundedSender<SignalingMessage>) -> Self {
        Self {
            participant,
            sender,
            peer_connection: None,
        }
    }

    /// Set the peer connection for this participant
    pub fn set_peer_connection(&mut self, peer_connection: Arc<Mutex<SfuPeerConnection>>) {
        self.peer_connection = Some(peer_connection);
    }

    /// Get the peer connection if it exists
    pub fn get_peer_connection(&self) -> Option<Arc<Mutex<SfuPeerConnection>>> {
        self.peer_connection.clone()
    }

    /// Send a signaling message to this participant
    pub fn send(&self, message: SignalingMessage) -> Result<(), String> {
        self.sender
            .send(message)
            .map_err(|e| format!("Failed to send message: {}", e))
    }
}
