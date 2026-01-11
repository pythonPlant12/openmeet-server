use tokio::sync::{mpsc, watch, Mutex};
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
    /// Shutdown signal - when this sender is dropped, all receivers are notified.
    /// Used to stop writer tasks that are sending TO this participant.
    _shutdown_tx: watch::Sender<()>,
    /// Receiver for shutdown signal - cloned and passed to writer tasks
    shutdown_rx: watch::Receiver<()>,
}

impl ParticipantConnection {
    pub fn new(participant: Participant, sender: mpsc::UnboundedSender<SignalingMessage>) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        Self {
            participant,
            sender,
            peer_connection: None,
            _shutdown_tx: shutdown_tx,
            shutdown_rx,
        }
    }

    /// Get a receiver for the shutdown signal.
    /// Writer tasks that send TO this participant should use this to detect disconnection.
    pub fn get_shutdown_receiver(&self) -> watch::Receiver<()> {
        self.shutdown_rx.clone()
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
