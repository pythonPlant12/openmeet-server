use tokio::sync::mpsc;
use crate::signaling::message::SignalingMessage;

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

/// Connection info for a participant (WebSocket sender)
pub struct ParticipantConnection {
    pub participant: Participant,
    pub sender: mpsc::UnboundedSender<SignalingMessage>,
}

impl ParticipantConnection {
    pub fn new(participant: Participant, sender: mpsc::UnboundedSender<SignalingMessage>) -> Self {
        Self {
            participant,
            sender,
        }
    }

    /// Send a message to this participant
    pub fn send(&self, message: SignalingMessage) -> Result<(), String> {
        self.sender
            .send(message)
            .map_err(|e| format!("Failed to send message: {}", e))
    }
}
