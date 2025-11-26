use std::collections::HashMap;
use crate::sfu::participant::ParticipantConnection;
use crate::signaling::message::SignalingMessage;
use tracing::{info, warn};

/// A video conference room containing multiple participants
pub struct Room {
    pub id: String,
    pub participants: HashMap<String, ParticipantConnection>,
}

impl Room {
    pub fn new(id: String) -> Self {
        info!("Creating new room: {}", id);
        Self {
            id,
            participants: HashMap::new(),
        }
    }

    /// Add a participant to the room
    pub fn add_participant(&mut self, participant: ParticipantConnection) {
        let participant_id = participant.participant.id.clone();
        let participant_name = participant.participant.name.clone();

        info!(
            "Adding participant {} ({}) to room {}",
            participant_name, participant_id, self.id
        );

        self.participants.insert(participant_id.clone(), participant);

        // Notify all other participants about the new joiner
        self.broadcast_except(
            &participant_id,
            SignalingMessage::ParticipantJoined {
                participant_id: participant_id.clone(),
                participant_name,
            },
        );
    }

    /// Remove a participant from the room
    pub fn remove_participant(&mut self, participant_id: &str) {
        if let Some(_) = self.participants.remove(participant_id) {
            info!("Removed participant {} from room {}", participant_id, self.id);

            // Notify all remaining participants
            self.broadcast(SignalingMessage::ParticipantLeft {
                participant_id: participant_id.to_string(),
            });
        } else {
            warn!("Tried to remove non-existent participant: {}", participant_id);
        }
    }

    /// Get participant count
    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }

    /// Check if room is empty
    pub fn is_empty(&self) -> bool {
        self.participants.is_empty()
    }

    /// Broadcast a message to all participants in the room
    pub fn broadcast(&self, message: SignalingMessage) {
        for (id, participant) in &self.participants {
            if let Err(e) = participant.send(message.clone()) {
                warn!("Failed to send message to participant {}: {}", id, e);
            }
        }
    }

    /// Broadcast a message to all participants except one
    pub fn broadcast_except(&self, exclude_id: &str, message: SignalingMessage) {
        for (id, participant) in &self.participants {
            if id != exclude_id {
                if let Err(e) = participant.send(message.clone()) {
                    warn!("Failed to send message to participant {}: {}", id, e);
                }
            }
        }
    }

    /// Send a message to a specific participant
    pub fn send_to(&self, target_id: &str, message: SignalingMessage) -> Result<(), String> {
        self.participants
            .get(target_id)
            .ok_or_else(|| format!("Participant {} not found", target_id))?
            .send(message)
    }

    /// Get list of all participant IDs and names
    pub fn get_participants_info(&self) -> Vec<(String, String)> {
        self.participants
            .iter()
            .map(|(id, conn)| (id.clone(), conn.participant.name.clone()))
            .collect()
    }
}
