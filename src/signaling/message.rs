use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessagePayload {
    /// Room-scoped, increasing ID that replies and reactions refer to.
    #[serde(default)]
    pub id: u64,
    pub participant_id: String,
    pub participant_name: String,
    pub message: String,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ChatReplyPreview>,
    #[serde(default)]
    pub reactions: Vec<ChatMessageReaction>,
}

impl ChatMessagePayload {
    pub fn into_signaling_message(self) -> SignalingMessage {
        SignalingMessage::ChatMessage {
            id: self.id,
            participant_id: self.participant_id,
            participant_name: self.participant_name,
            message: self.message,
            timestamp: self.timestamp,
            reply_to_id: None,
            reply_to: self.reply_to,
            reactions: self.reactions,
        }
    }
}

/// Short quote of the message a chat message replies to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatReplyPreview {
    pub id: u64,
    pub participant_id: String,
    pub participant_name: String,
    pub message: String,
}

/// Everyone who reacted to a chat message with one emoji, in the order the emoji was first used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageReaction {
    pub emoji: String,
    pub participant_ids: Vec<String>,
}

/// Messages sent between client and server for WebRTC signaling
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SignalingMessage {
    /// First client message. Credentials are sent in-frame, never in the URL.
    #[serde(rename_all = "camelCase")]
    Authenticate {
        protocol_version: u16,
        access_token: Option<String>,
    },

    /// Server confirms session identity decision before room operations.
    #[serde(rename_all = "camelCase")]
    Authenticated {
        protocol_version: u16,
        authenticated: bool,
    },

    /// Client wants to join a room
    #[serde(rename_all = "camelCase")]
    Join {
        room_id: String,
        participant_name: String,
        /// Only for password-protected meeting rooms.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        /// Joins as the screen share of this participant: it only sends media and receives none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen_share_of: Option<String>,
    },

    /// Server confirms participant joined
    #[serde(rename_all = "camelCase")]
    Joined {
        participant_id: String,
        participant_name: String,
    },

    /// WebRTC offer from client to server (or server to client)
    #[serde(rename_all = "camelCase")]
    Offer { target_id: String, sdp: String },

    /// WebRTC answer in response to offer
    #[serde(rename_all = "camelCase")]
    Answer { target_id: String, sdp: String },

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
        /// The presenter, when this participant is a screen share.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen_share_of: Option<String>,
    },

    /// Notify when a participant leaves the room
    #[serde(rename_all = "camelCase")]
    ParticipantLeft { participant_id: String },

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

    /// Chat message from a participant. Clients send `replyToId`; the server fills in the ID, the
    /// reply quote, and reactions.
    #[serde(rename_all = "camelCase")]
    ChatMessage {
        #[serde(default)]
        id: u64,
        participant_id: String,
        participant_name: String,
        message: String,
        timestamp: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to_id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<ChatReplyPreview>,
        #[serde(default)]
        reactions: Vec<ChatMessageReaction>,
    },

    /// Client adds its reaction to a chat message, or removes it when it already reacted with the emoji.
    #[serde(rename_all = "camelCase")]
    ChatReaction { message_id: u64, emoji: String },

    /// Server sends the full reaction list of a chat message after it changes.
    #[serde(rename_all = "camelCase")]
    ChatReactionsChanged {
        message_id: u64,
        reactions: Vec<ChatMessageReaction>,
    },

    /// Existing messages sent to a participant when they join
    #[serde(rename_all = "camelCase")]
    ChatHistory { messages: Vec<ChatMessagePayload> },

    /// Error message from server
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_join_message() {
        let msg = SignalingMessage::Join {
            room_id: "room123".to_string(),
            participant_name: "Alice".to_string(),
            password: None,
            screen_share_of: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"join\""));
        assert!(json.contains("\"roomId\":\"room123\""));
        assert!(json.contains("\"participantName\":\"Alice\""));
    }

    #[test]
    fn test_authenticate_message_uses_camel_case() {
        let msg = SignalingMessage::Authenticate {
            protocol_version: 2,
            access_token: None,
        };

        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"type":"authenticate","protocolVersion":2,"accessToken":null}"#
        );
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

    #[test]
    fn test_serialize_chat_history() {
        let msg = SignalingMessage::ChatHistory {
            messages: vec![ChatMessagePayload {
                id: 7,
                participant_id: "peer123".to_string(),
                participant_name: "Alice".to_string(),
                message: "Welcome".to_string(),
                timestamp: 1234,
                reply_to: None,
                reactions: vec![ChatMessageReaction {
                    emoji: "👍".to_string(),
                    participant_ids: vec!["peer456".to_string()],
                }],
            }],
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"chatHistory","messages":[{"id":7,"participantId":"peer123","participantName":"Alice","message":"Welcome","timestamp":1234,"reactions":[{"emoji":"👍","participantIds":["peer456"]}]}]}"#
        );
    }

    #[test]
    fn test_deserialize_legacy_and_reply_chat_messages() {
        let legacy = r#"{"type":"chatMessage","participantId":"","participantName":"","message":"Hi","timestamp":0}"#;
        let reply = r#"{"type":"chatMessage","participantId":"","participantName":"","message":"Yes","timestamp":0,"replyToId":3}"#;

        assert!(matches!(
            serde_json::from_str::<SignalingMessage>(legacy).unwrap(),
            SignalingMessage::ChatMessage {
                id: 0,
                reply_to_id: None,
                ..
            }
        ));
        assert!(matches!(
            serde_json::from_str::<SignalingMessage>(reply).unwrap(),
            SignalingMessage::ChatMessage {
                reply_to_id: Some(3),
                ..
            }
        ));
    }

    #[test]
    fn test_chat_reaction_messages_use_camel_case() {
        let reaction: SignalingMessage =
            serde_json::from_str(r#"{"type":"chatReaction","messageId":4,"emoji":"🎉"}"#).unwrap();
        assert!(matches!(
            reaction,
            SignalingMessage::ChatReaction { message_id: 4, .. }
        ));

        let changed = SignalingMessage::ChatReactionsChanged {
            message_id: 4,
            reactions: Vec::new(),
        };
        assert_eq!(
            serde_json::to_string(&changed).unwrap(),
            r#"{"type":"chatReactionsChanged","messageId":4,"reactions":[]}"#
        );
    }
}
