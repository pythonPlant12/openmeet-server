use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
use webrtc::track::track_remote::TrackRemote;

/// Configuration for creating peer connections
#[derive(Clone)]
pub struct PeerConnectionConfig {
    pub ice_servers: Vec<RTCIceServer>,
}

impl Default for PeerConnectionConfig {
    fn default() -> Self {
        Self {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
        }
    }
}

/// Wrapper around RTCPeerConnection with SFU-specific logic
pub struct SfuPeerConnection {
    peer_connection: Arc<RTCPeerConnection>,
    participant_id: String,
}

impl SfuPeerConnection {
    /// Create a new peer connection for a participant
    pub async fn new(
        participant_id: String,
        config: PeerConnectionConfig,
    ) -> Result<Arc<Mutex<Self>>> {
        info!("Creating peer connection for participant: {}", participant_id);

        // Create media engine with default codecs
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;

        // Create interceptor registry with default interceptors
        // This enables NACK (retransmission) and PLI (keyframe request) handling
        let mut interceptor_registry = Registry::new();
        interceptor_registry = register_default_interceptors(interceptor_registry, &mut media_engine)?;

        // Build WebRTC API with interceptors for RTCP feedback handling
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(interceptor_registry)
            .build();

        // Configure ICE servers
        let rtc_config = RTCConfiguration {
            ice_servers: config.ice_servers,
            ..Default::default()
        };

        // Create peer connection
        let peer_connection = Arc::new(api.new_peer_connection(rtc_config).await?);

        let sfu_pc = Arc::new(Mutex::new(Self {
            peer_connection: Arc::clone(&peer_connection),
            participant_id: participant_id.clone(),
        }));

        // Set up connection state handlers
        Self::setup_connection_handlers(Arc::clone(&peer_connection), &participant_id).await;

        Ok(sfu_pc)
    }

    /// Set up handlers for connection state changes
    async fn setup_connection_handlers(
        peer_connection: Arc<RTCPeerConnection>,
        participant_id: &str,
    ) {
        let participant_id_clone = participant_id.to_string();

        // Handle ICE connection state changes
        peer_connection
            .on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
                let participant_id = participant_id_clone.clone();
                Box::pin(async move {
                    info!(
                        "Participant {} ICE connection state: {:?}",
                        participant_id, state
                    );

                    match state {
                        RTCIceConnectionState::Failed | RTCIceConnectionState::Disconnected => {
                            warn!("Participant {} connection issues: {:?}", participant_id, state);
                        }
                        RTCIceConnectionState::Connected | RTCIceConnectionState::Completed => {
                            info!("Participant {} successfully connected", participant_id);
                        }
                        _ => {}
                    }
                })
            }));

        let participant_id_clone = participant_id.to_string();

        // Handle peer connection state changes
        peer_connection
            .on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
                let participant_id = participant_id_clone.clone();
                Box::pin(async move {
                    debug!(
                        "Participant {} peer connection state: {:?}",
                        participant_id, state
                    );

                    if state == RTCPeerConnectionState::Failed {
                        error!("Participant {} peer connection failed", participant_id);
                    }
                })
            }));
    }

    /// Set remote description (offer or answer from client)
    pub async fn set_remote_description(&self, sdp: String, sdp_type: &str) -> Result<()> {
        debug!(
            "Setting remote description for participant {}: type={}",
            self.participant_id, sdp_type
        );

        let session_description = match sdp_type {
            "offer" => RTCSessionDescription::offer(sdp)?,
            "answer" => RTCSessionDescription::answer(sdp)?,
            _ => return Err(anyhow::anyhow!("Invalid SDP type: {}", sdp_type)),
        };

        self.peer_connection
            .set_remote_description(session_description)
            .await?;

        Ok(())
    }

    /// Create an answer to send back to the client
    pub async fn create_answer(&self) -> Result<RTCSessionDescription> {
        debug!("Creating answer for participant {}", self.participant_id);

        // Note: Transceivers are already set to sendrecv in the Offer handler
        // before adding tracks, so we just create the answer here

        let answer = self.peer_connection.create_answer(None).await?;

        self.peer_connection
            .set_local_description(answer.clone())
            .await?;

        Ok(answer)
    }

    /// Create an offer for renegotiation (Perfect Negotiation pattern)
    /// Called when negotiationneeded fires (e.g., when add_track() is called after initial negotiation)
    /// Returns None if a negotiation is already in progress
    pub async fn create_offer_if_stable(&self) -> Result<Option<RTCSessionDescription>> {
        // Check signaling state - only create offer if in stable state
        let signaling_state = self.peer_connection.signaling_state();

        if signaling_state != webrtc::peer_connection::signaling_state::RTCSignalingState::Stable {
            info!(
                "Participant {} signaling state is {:?}, skipping renegotiation offer to avoid collision",
                self.participant_id,
                signaling_state
            );
            return Ok(None);
        }

        debug!("Creating offer for participant {}", self.participant_id);

        let offer = self.peer_connection.create_offer(None).await?;

        self.peer_connection
            .set_local_description(offer.clone())
            .await?;

        Ok(Some(offer))
    }

    /// Add an ICE candidate received from the client
    pub async fn add_ice_candidate(&self, candidate: String, sdp_mid: Option<String>, sdp_m_line_index: Option<u16>) -> Result<()> {
        debug!(
            "Adding ICE candidate for participant {}: {}",
            self.participant_id, candidate
        );

        let ice_candidate = webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
            candidate,
            sdp_mid,
            sdp_mline_index: sdp_m_line_index,
            username_fragment: None,
        };

        self.peer_connection.add_ice_candidate(ice_candidate).await?;

        Ok(())
    }

    /// Get the underlying peer connection for track manipulation
    pub fn get_peer_connection(&self) -> Arc<RTCPeerConnection> {
        Arc::clone(&self.peer_connection)
    }

    /// Set up handler for incoming tracks (media from client)
    pub fn on_track<F>(&self, handler: F)
    where
        F: Fn(Arc<TrackRemote>, Arc<RTCRtpReceiver>) + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        self.peer_connection
            .on_track(Box::new(move |track, receiver, _transceiver| {
                let handler = Arc::clone(&handler);
                Box::pin(async move {
                    handler(track, receiver);
                })
            }));
    }

    /// Close the peer connection
    pub async fn close(&self) -> Result<()> {
        info!("Closing peer connection for participant {}", self.participant_id);
        self.peer_connection.close().await?;
        Ok(())
    }

    /// Get participant ID
    pub fn participant_id(&self) -> &str {
        &self.participant_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_peer_connection() {
        let config = PeerConnectionConfig::default();
        let result = SfuPeerConnection::new("test-participant".to_string(), config).await;

        assert!(result.is_ok());
        let pc = result.unwrap();
        let pc_lock = pc.lock().await;
        assert_eq!(pc_lock.participant_id(), "test-participant");
    }

    #[tokio::test]
    async fn test_peer_connection_config_default() {
        let config = PeerConnectionConfig::default();
        assert_eq!(config.ice_servers.len(), 1);
        assert_eq!(
            config.ice_servers[0].urls[0],
            "stun:stun.l.google.com:19302"
        );
    }
}
