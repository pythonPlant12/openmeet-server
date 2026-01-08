use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::rtcp::transport_feedbacks::transport_layer_nack::TransportLayerNack;
use webrtc::rtp::packet::Packet as RtpPacket;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{ TrackLocal, TrackLocalWriter };
use webrtc::track::track_remote::TrackRemote;
use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
use webrtc::rtp_transceiver::rtp_sender::RTCRtpSender;
use crate::sfu::participant::ParticipantConnection;
use crate::sfu::packet_buffer::RtpPacketBuffer;
use crate::signaling::message::SignalingMessage;
use tracing::{ info, warn, error, debug };

/// Broadcast channel capacity for RTP packets
const RTP_BROADCAST_CAPACITY: usize = 256;

/// Information about a track from a sender, including the peer connection for PLI forwarding
pub struct SenderTrackInfo {
    pub track: Arc<TrackRemote>,
    pub sender_peer_connection: Arc<RTCPeerConnection>,
    pub sender_ssrc: u32,
    /// Broadcast sender for this track - new participants subscribe to this
    pub packet_broadcaster: broadcast::Sender<RtpPacket>,
}

/// A video conference room containing multiple participants
pub struct Room {
    pub id: String,
    pub participants: HashMap<String, ParticipantConnection>,
    /// Tracks being sent by each participant (participant_id -> list of track info)
    pub participant_tracks: HashMap<String, Vec<SenderTrackInfo>>,
    /// Track IDs that have been successfully negotiated to each participant.
    /// Uses track_id (not stream_id) because audio+video share same stream_id.
    pub negotiated_tracks: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    /// Task handles for each participant - used to abort tasks on disconnect
    /// Key is participant_id, value is list of JoinHandles for that participant's tasks
    task_handles: HashMap<String, Vec<JoinHandle<()>>>,
}

impl Room {
    pub fn new(id: String) -> Self {
        info!("Creating new room: {}", id);
        Self {
            id,
            participants: HashMap::new(),
            participant_tracks: HashMap::new(),
            negotiated_tracks: Arc::new(RwLock::new(HashMap::new())),
            task_handles: HashMap::new(),
        }
    }

    /// Register a task handle for a participant (for cleanup on disconnect)
    pub fn register_task(&mut self, participant_id: &str, handle: JoinHandle<()>) {
        self.task_handles
            .entry(participant_id.to_string())
            .or_insert_with(Vec::new)
            .push(handle);
    }

    /// Abort all tasks for a participant
    fn abort_participant_tasks(&mut self, participant_id: &str) {
        if let Some(handles) = self.task_handles.remove(participant_id) {
            let count = handles.len();
            for handle in handles {
                handle.abort();
            }
            info!("Room {}: Aborted {} tasks for participant {}", self.id, count, participant_id);
        }
    }

    /// Add a participant to the room
    pub fn add_participant(&mut self, participant: ParticipantConnection) {
        let participant_id = participant.participant.id.clone();
        let participant_name = participant.participant.name.clone();

        info!("Adding participant {} ({}) to room {}", participant_name, participant_id, self.id);

        self.participants.insert(participant_id.clone(), participant);

        // Notify all other participants about the new joiner
        self.broadcast_except(&participant_id, SignalingMessage::ParticipantJoined {
            participant_id: participant_id.clone(),
            participant_name,
        });
    }

    /// Remove a participant from the room
    pub async fn remove_participant(&mut self, participant_id: &str) {
        if let Some(participant_conn) = self.participants.remove(participant_id) {
            info!("Removing participant {} from room {}", participant_id, self.id);

            // CRITICAL: Close peer connection to stop all WebRTC tasks
            // This causes read_rtp() and other blocking calls to error out
            if let Some(peer_conn) = participant_conn.get_peer_connection() {
                let pc = peer_conn.lock().await;
                if let Err(e) = pc.close().await {
                    warn!("Error closing peer connection for {}: {}", participant_id, e);
                } else {
                    info!("Closed peer connection for {}", participant_id);
                }
            }

            // CRITICAL: Abort all spawned tasks for this participant
            // This ensures tasks don't keep running and holding memory
            self.abort_participant_tasks(participant_id);

            // Remove their tracks (drops broadcast::Sender, causing receivers to stop)
            self.remove_participant_tracks(participant_id);

            // Clear negotiated tracks for this participant
            {
                let mut negotiated = self.negotiated_tracks.write().await;
                negotiated.remove(participant_id);
            }

            // Notify all remaining participants
            self.broadcast(SignalingMessage::ParticipantLeft {
                participant_id: participant_id.to_string(),
            });

            info!("Removed participant {} from room {}", participant_id, self.id);
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

    /// Get list of all participants with their media states
    pub fn get_participants_with_media_state(&self) -> Vec<(String, String, bool, bool)> {
        self.participants
            .iter()
            .map(|(id, conn)| (
                id.clone(),
                conn.participant.name.clone(),
                conn.participant.audio_enabled,
                conn.participant.video_enabled,
            ))
            .collect()
    }

    /// Register a track from a participant and forward it to all other participants
    pub async fn handle_incoming_track(
        &mut self,
        participant_id: &str,
        track: Arc<TrackRemote>,
        _receiver: Arc<RTCRtpReceiver>,
        sender_peer_connection: Arc<RTCPeerConnection>,
    ) {
        let track_kind = track.kind();
        let track_id = track.stream_id();
        let sender_ssrc = track.ssrc();

        info!(
            "Room {}: Participant {} sent {} track (stream_id: {}, ssrc: {})",
            self.id,
            participant_id,
            track_kind,
            track_id,
            sender_ssrc
        );

        // Create broadcast channel for this track - ONE reader broadcasts to ALL receivers
        let (packet_tx, _) = broadcast::channel::<RtpPacket>(RTP_BROADCAST_CAPACITY);

        // Store the track info including broadcaster for late joiners
        let track_info = SenderTrackInfo {
            track: Arc::clone(&track),
            sender_peer_connection: Arc::clone(&sender_peer_connection),
            sender_ssrc,
            packet_broadcaster: packet_tx.clone(),
        };

        self.participant_tracks
            .entry(participant_id.to_string())
            .or_insert_with(Vec::new)
            .push(track_info);

        // Forward track to all other participants (passing the broadcaster)
        self.forward_track_to_others(participant_id, track, sender_peer_connection, packet_tx).await;
    }

    /// Forward a track from one participant to all others in the room
    /// Uses broadcast channel pattern: ONE reader task broadcasts to ALL writer tasks
    /// All spawned tasks are registered to the sender's participant_id for cleanup
    async fn forward_track_to_others(
        &mut self,
        sender_id: &str,
        track: Arc<TrackRemote>,
        sender_peer_connection: Arc<RTCPeerConnection>,
        packet_tx: broadcast::Sender<RtpPacket>,
    ) {
        let track_kind = track.kind();
        let stream_id = track.stream_id();
        let sender_ssrc = track.ssrc();

        // Get sender info for StreamOwner message
        let sender_name = self.participants
            .get(sender_id)
            .map(|p| p.participant.name.clone())
            .unwrap_or_else(|| "Unknown".to_string());

        // Collect receivers info before spawning tasks
        let mut receivers: Vec<(String, Arc<TrackLocalStaticRTP>, Arc<RTCRtpSender>, u32)> = Vec::new();

        for (participant_id, participant_conn) in &self.participants {
            if participant_id == sender_id {
                continue;
            }

            // Send StreamOwner message BEFORE adding track so client knows the mapping
            let _ = participant_conn.send(SignalingMessage::StreamOwner {
                stream_id: stream_id.clone(),
                participant_id: sender_id.to_string(),
                participant_name: sender_name.clone(),
            });

            if let Some(peer_conn) = participant_conn.get_peer_connection() {
                let peer_conn_lock = peer_conn.lock().await;
                let pc = peer_conn_lock.get_peer_connection();

                match Self::create_forwarding_track(&track).await {
                    Ok(local_track) => {
                        match pc.add_track(Arc::clone(&local_track) as Arc<dyn TrackLocal + Send + Sync>).await {
                            Ok(rtp_sender) => {
                                // Get the local SSRC for this sender
                                let local_ssrc = match rtp_sender.get_parameters().await.encodings.first() {
                                    Some(encoding) => encoding.ssrc,
                                    None => {
                                        error!("No SSRC found for RTP sender to {}", participant_id);
                                        continue;
                                    }
                                };

                                receivers.push((participant_id.clone(), local_track, rtp_sender, local_ssrc));

                                // Send PLI for video tracks to ensure immediate keyframe delivery
                                if track_kind == webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video {
                                    let pli = PictureLossIndication {
                                        sender_ssrc: 0,
                                        media_ssrc: sender_ssrc,
                                    };
                                    if let Err(e) = sender_peer_connection.write_rtcp(&[Box::new(pli)]).await {
                                        error!("Failed to send PLI to {} for {}: {}", sender_id, participant_id, e);
                                    } else {
                                        info!("Sent PLI to {} for receiver {}", sender_id, participant_id);
                                    }
                                }

                                // Renegotiate
                                let peer_conn_for_renego = Arc::clone(&peer_conn);
                                let participant_conn_sender = participant_conn.sender.clone();
                                let participant_id_for_renego = participant_id.clone();
                                let track_id = track.id().to_string();
                                let negotiated_tracks = Arc::clone(&self.negotiated_tracks);

                                tokio::spawn(async move {
                                    let peer_conn_lock = peer_conn_for_renego.lock().await;
                                    match peer_conn_lock.create_offer_if_stable().await {
                                        Ok(Some(offer)) => {
                                            let _ = participant_conn_sender.send(SignalingMessage::Offer {
                                                target_id: participant_id_for_renego.clone(),
                                                sdp: offer.sdp,
                                            });
                                            let mut negotiated = negotiated_tracks.write().await;
                                            negotiated
                                                .entry(participant_id_for_renego.clone())
                                                .or_insert_with(HashSet::new)
                                                .insert(track_id.clone());
                                            info!("Negotiated {} track {} to {}", track_kind, track_id, participant_id_for_renego);
                                        }
                                        Ok(None) => {
                                            info!("Collision for {} track to {} - pending retry", track_kind, participant_id_for_renego);
                                        }
                                        Err(e) => {
                                            error!("Renegotiation failed for {}: {}", participant_id_for_renego, e);
                                        }
                                    }
                                });
                            }
                            Err(e) => {
                                error!("Failed to add track to {}: {}", participant_id, e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to create forwarding track: {}", e);
                    }
                }
            }
        }

        // ALWAYS spawn the reader task - even if there are no receivers yet
        // Late joiners will subscribe to the broadcast channel
        let from_id = sender_id.to_string();
        let packet_tx_clone = packet_tx.clone();

        // Reader task: reads from remote track and broadcasts to all receivers
        let reader_handle = tokio::spawn(async move {
            let mut packet_count = 0u64;

            loop {
                match track.read_rtp().await {
                    Ok((rtp_packet, _)) => {
                        packet_count += 1;

                        if packet_count % 500 == 0 {
                            info!("Read {} {} packets FROM {}", packet_count, track_kind, from_id);
                        }

                        // Broadcast to all receivers (including future late joiners)
                        // send() returns Err only if there are no receivers AND channel is closed
                        // With broadcast channel, this will succeed even with 0 receivers
                        let _ = packet_tx_clone.send(rtp_packet);
                    }
                    Err(_) => {
                        if packet_count == 0 {
                            warn!("✗ No packets for {} FROM {}", track_kind, from_id);
                        }
                        break;
                    }
                }
            }

            info!("Reader task ended: {} {} packets FROM {}", packet_count, track_kind, from_id);
        });

        // Register reader task for cleanup when sender disconnects
        self.register_task(sender_id, reader_handle);

        // Spawn writer tasks for each current receiver (late joiners use subscribe_new_participant_to_track)
        // Collect task handles for registration
        let mut task_handles: Vec<JoinHandle<()>> = Vec::new();

        if !receivers.is_empty() {
            for (to_id, local_track, rtp_sender, local_ssrc) in receivers {
                let mut packet_rx = packet_tx.subscribe();
                let from_id = sender_id.to_string();
                let sender_pc = Arc::clone(&sender_peer_connection);
                let kind = track_kind.clone();

                // Create packet buffer for this receiver
                let packet_buffer = Arc::new(RtpPacketBuffer::new());
                let buffer_for_rtcp = Arc::clone(&packet_buffer);
                let local_track_for_rtcp = Arc::clone(&local_track);

                // Spawn RTCP feedback handler
                let rtp_sender_clone = Arc::clone(&rtp_sender);
                let sender_pc_clone = Arc::clone(&sender_pc);
                let from_id_clone = from_id.clone();
                let to_id_clone = to_id.clone();

                let rtcp_handle = tokio::spawn(async move {
                    Self::handle_rtcp_feedback(
                        rtp_sender_clone,
                        sender_pc_clone,
                        sender_ssrc,
                        from_id_clone,
                        to_id_clone,
                        kind,
                        local_track_for_rtcp,
                        buffer_for_rtcp,
                        local_ssrc,
                    ).await;
                });
                task_handles.push(rtcp_handle);

                // Writer task: receives from broadcast and writes to local track
                let writer_handle = tokio::spawn(async move {
                    let mut packet_count = 0u64;

                    loop {
                        match packet_rx.recv().await {
                            Ok(mut rtp_packet) => {
                                packet_count += 1;

                                if packet_count % 500 == 0 {
                                    info!("Forwarded {} {} packets {} → {}", packet_count, kind, from_id, to_id);
                                }

                                // Rewrite SSRC for this receiver
                                rtp_packet.header.ssrc = local_ssrc;

                                // Store in buffer for NACK retransmission
                                packet_buffer.store(&rtp_packet).await;

                                // Write to local track
                                if let Err(e) = local_track.write_rtp(&rtp_packet).await {
                                    error!("✗ Write error {} → {}: {}", from_id, to_id, e);
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                break;
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                warn!("Receiver {} lagged {} packets from {}", to_id, n, from_id);
                                // Continue - we'll catch up
                            }
                        }
                    }

                    info!("Writer task ended: {} {} packets {} → {}", packet_count, kind, from_id, to_id);
                });
                task_handles.push(writer_handle);
            }
        }

        // Register all spawned tasks for cleanup when sender disconnects
        for handle in task_handles {
            self.register_task(sender_id, handle);
        }
    }

    /// Create a local track for forwarding a remote track
    pub async fn create_forwarding_track(
        remote_track: &Arc<TrackRemote>
    ) -> Result<Arc<TrackLocalStaticRTP>, String> {
        let codec = remote_track.codec();

        let local_track = TrackLocalStaticRTP::new(
            codec.capability,
            format!("forwarded-{}", remote_track.id()),
            remote_track.stream_id()
        );

        Ok(Arc::new(local_track))
    }

    /// Subscribe a new participant to an existing track (for late joiners)
    /// This spawns a writer task that receives from the track's broadcast channel
    /// Returns task handles for cleanup registration
    pub fn subscribe_new_participant_to_track(
        local_track: Arc<TrackLocalStaticRTP>,
        rtp_sender: Arc<RTCRtpSender>,
        packet_broadcaster: &broadcast::Sender<RtpPacket>,
        sender_peer_connection: Arc<RTCPeerConnection>,
        sender_ssrc: u32,
        local_ssrc: u32,
        from_participant: String,
        to_participant: String,
        track_kind: webrtc::rtp_transceiver::rtp_codec::RTPCodecType,
    ) -> Vec<JoinHandle<()>> {
        let mut packet_rx = packet_broadcaster.subscribe();
        let from_id = from_participant.clone();
        let to_id = to_participant.clone();
        let kind = track_kind;

        // Create packet buffer for this receiver
        let packet_buffer = Arc::new(RtpPacketBuffer::new());
        let buffer_for_rtcp = Arc::clone(&packet_buffer);
        let local_track_for_rtcp = Arc::clone(&local_track);

        // Spawn RTCP feedback handler
        let rtp_sender_clone = Arc::clone(&rtp_sender);
        let sender_pc_clone = Arc::clone(&sender_peer_connection);
        let from_id_clone = from_id.clone();
        let to_id_clone = to_id.clone();

        let rtcp_handle = tokio::spawn(async move {
            Self::handle_rtcp_feedback(
                rtp_sender_clone,
                sender_pc_clone,
                sender_ssrc,
                from_id_clone,
                to_id_clone,
                kind,
                local_track_for_rtcp,
                buffer_for_rtcp,
                local_ssrc,
            ).await;
        });

        // Writer task: receives from broadcast and writes to local track
        let writer_handle = tokio::spawn(async move {
            let mut packet_count = 0u64;

            loop {
                match packet_rx.recv().await {
                    Ok(mut rtp_packet) => {
                        packet_count += 1;

                        if packet_count % 500 == 0 {
                            info!("Late joiner: forwarded {} {} packets {} → {}", packet_count, kind, from_id, to_id);
                        }

                        // Rewrite SSRC for this receiver
                        rtp_packet.header.ssrc = local_ssrc;

                        // Store in buffer for NACK retransmission
                        packet_buffer.store(&rtp_packet).await;

                        // Write to local track
                        if let Err(e) = local_track.write_rtp(&rtp_packet).await {
                            error!("✗ Write error {} → {}: {}", from_id, to_id, e);
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Late joiner {} lagged {} packets from {}", to_id, n, from_id);
                    }
                }
            }

            info!("Late joiner writer ended: {} {} packets {} → {}", packet_count, kind, from_id, to_id);
        });

        vec![rtcp_handle, writer_handle]
    }

    /// Handle RTCP feedback from receivers and forward PLI/NACK to the original sender
    async fn handle_rtcp_feedback(
        rtp_sender: Arc<RTCRtpSender>,
        sender_peer_connection: Arc<RTCPeerConnection>,
        sender_ssrc: u32,
        from_participant: String,
        to_participant: String,
        track_kind: webrtc::rtp_transceiver::rtp_codec::RTPCodecType,
        local_track: Arc<TrackLocalStaticRTP>,
        packet_buffer: Arc<RtpPacketBuffer>,
        local_ssrc: u32,
    ) {
        let mut pli_count = 0u64;
        let mut nack_count = 0u64;
        let mut retransmit_count = 0u64;

        loop {
            // Read RTCP packets from the receiver (PLI, NACK, etc.)
            match rtp_sender.read_rtcp().await {
                Ok((rtcp_packets, _)) => {
                    for packet in rtcp_packets {
                        // Check if this is a PLI (Picture Loss Indication) request
                        if packet.as_any().downcast_ref::<PictureLossIndication>().is_some() {
                            pli_count += 1;
                            debug!(
                                "Received PLI #{} for {} track FROM {} (forwarding to sender {})",
                                pli_count, track_kind, to_participant, from_participant
                            );

                            // Forward PLI to the original sender to request a keyframe
                            let pli = PictureLossIndication {
                                sender_ssrc: 0,  // Will be filled by the stack
                                media_ssrc: sender_ssrc,
                            };

                            if let Err(e) = sender_peer_connection.write_rtcp(&[Box::new(pli)]).await {
                                error!(
                                    "Failed to forward PLI to sender {}: {}",
                                    from_participant, e
                                );
                            } else {
                                debug!("✓ Forwarded PLI to sender {} for {} track", from_participant, track_kind);
                            }
                        }
                        // Handle NACK - retransmit requested packets from our buffer
                        else if let Some(nack) = packet.as_any().downcast_ref::<TransportLayerNack>() {
                            nack_count += 1;

                            // Extract all requested sequence numbers from the NACK
                            let mut requested_seqs = Vec::new();
                            for nack_pair in &nack.nacks {
                                // The packet_id is the base sequence number
                                requested_seqs.push(nack_pair.packet_id);

                                // The lost_packets bitmask indicates which of the next 16 packets are also lost
                                let mut bitmask = nack_pair.lost_packets;
                                for i in 1..=16u16 {
                                    if bitmask & 1 == 1 {
                                        requested_seqs.push(nack_pair.packet_id.wrapping_add(i));
                                    }
                                    bitmask >>= 1;
                                }
                            }

                            debug!(
                                "Received NACK #{} for {} packets ({:?}) FROM {} - retransmitting",
                                nack_count,
                                requested_seqs.len(),
                                &requested_seqs[..requested_seqs.len().min(5)], // Log first 5 seq nums
                                to_participant
                            );

                            // Retransmit requested packets from buffer
                            for seq in requested_seqs {
                                if let Some(packet) = packet_buffer.get(seq).await {
                                    // Verify SSRC matches (should already be rewritten)
                                    if packet.header.ssrc == local_ssrc {
                                        if let Err(e) = local_track.write_rtp(&packet).await {
                                            error!("Failed to retransmit packet {}: {}", seq, e);
                                        } else {
                                            retransmit_count += 1;
                                        }
                                    }
                                } else {
                                    debug!("Packet {} not in buffer for retransmission", seq);
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // RTCP reading stopped, likely peer disconnected
                    break;
                }
            }
        }

        if pli_count > 0 || nack_count > 0 {
            info!(
                "RTCP handler ended: {} PLIs, {} NACKs, {} retransmissions {} → {}",
                pli_count, nack_count, retransmit_count, to_participant, from_participant
            );
        }
    }

    /// Remove all tracks for a participant (called when they leave)
    /// This drops the broadcast::Sender for each track, which causes all
    /// receiver tasks (writer tasks) to get a Closed error and terminate.
    pub fn remove_participant_tracks(&mut self, participant_id: &str) {
        if let Some(tracks) = self.participant_tracks.remove(participant_id) {
            let track_count = tracks.len();

            // Explicitly drop each track info to release broadcast senders
            // When the last sender is dropped, all receivers will get Closed error
            for track_info in tracks {
                // Log before dropping
                debug!(
                    "Room {}: Dropping track (ssrc: {}) for participant {}",
                    self.id, track_info.sender_ssrc, participant_id
                );
                // track_info is dropped here, including packet_broadcaster (broadcast::Sender)
                drop(track_info);
            }

            info!(
                "Room {}: Removed {} tracks for participant {} (broadcast channels closed)",
                self.id, track_count, participant_id
            );
        }
    }
}
