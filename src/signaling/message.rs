use serde::{Deserialize, Serialize};

/// Messages sent between client and server for WebRTC signaling
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SignalingMessage {
    /// Client wants to join a room
    #[serde(rename_all = "camelCase")]
    Join {
        room_id: String,
        participant_name: String,
    },

    /// Server confirms participant joined
    #[serde(rename_all = "camelCase")]
    Joined {
        participant_id: String,
        participant_name: String,
    },

    /// WebRTC offer from client to server (or server to client)
    #[serde(rename_all = "camelCase")]
    Offer {
        target_id: String,
        sdp: String,
    },

    /// WebRTC answer in response to offer
    #[serde(rename_all = "camelCase")]
    Answer {
        target_id: String,
        sdp: String,
    },

    /// ICE candidate for NAT traversal
    #[serde(rename_all = "camelCase")]
    IceCandidate {
        target_id: String,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u16>,
    },

    /// Notify when a new participant joins the room
    #[serde(rename_all = "camelCase")]
    ParticipantJoined {
        participant_id: String,
        participant_name: String,
    },

    /// Notify when a participant leaves the room
    #[serde(rename_all = "camelCase")]
    ParticipantLeft {
        participant_id: String,
    },

    /// Maps a stream ID to its owner participant
    #[serde(rename_all = "camelCase")]
    StreamOwner {
        stream_id: String,
        participant_id: String,
        participant_name: String,
    },

    /// Participant toggled their audio/video
    #[serde(rename_all = "camelCase")]
    MediaStateChanged {
        participant_id: String,
        audio_enabled: bool,
        video_enabled: bool,
    },

    /// Chat message from a participant
    #[serde(rename_all = "camelCase")]
    ChatMessage {
        participant_id: String,
        participant_name: String,
        message: String,
        timestamp: u64,
    },

    /// Error message from server
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_join_message() {
        let msg = SignalingMessage::Join {
            room_id: "room123".to_string(),
            participant_name: "Alice".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"join\""));
        assert!(json.contains("\"roomId\":\"room123\""));
        assert!(json.contains("\"participantName\":\"Alice\""));
    }

    #[test]
    fn test_deserialize_offer_message() {
        let json = r#"{"type":"offer","targetId":"peer123","sdp":"v=0..."}"#;
        let msg: SignalingMessage = serde_json::from_str(json).unwrap();

        match msg {
            SignalingMessage::Offer { target_id, sdp } => {
                assert_eq!(target_id, "peer123");
                assert_eq!(sdp, "v=0...");
            }
            _ => panic!("Wrong message type"),
        }
    }
}
