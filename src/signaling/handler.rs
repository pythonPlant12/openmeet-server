use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use metrics::{counter, gauge};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::sfu::{
    participant::{Participant, ParticipantConnection},
    peer_connection::{PeerConnectionConfig, SfuPeerConnection},
    repository::RoomRepository,
    room::Room,
};
use crate::signaling::message::SignalingMessage;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_remote::TrackRemote;

#[derive(Default)]
struct CandidateTypeCounts {
    host: usize,
    srflx: usize,
    relay: usize,
    other: usize,
}

impl CandidateTypeCounts {
    fn record(&mut self, candidate: &str) {
        match ice_candidate_type(candidate) {
            "host" => self.host += 1,
            "srflx" => self.srflx += 1,
            "relay" => self.relay += 1,
            _ => self.other += 1,
        }
    }
}

fn ice_candidate_type(candidate: &str) -> &str {
    let parts: Vec<&str> = candidate.split_whitespace().collect();
    parts
        .iter()
        .position(|part| *part == "typ")
        .and_then(|index| parts.get(index + 1))
        .copied()
        .unwrap_or("unknown")
}

fn ice_candidate_protocol(candidate: &str) -> &str {
    candidate.split_whitespace().nth(2).unwrap_or("unknown")
}

fn ice_candidate_address(candidate: &str) -> Option<&str> {
    candidate.split_whitespace().nth(4)
}

fn is_loopback_candidate(candidate: &str) -> bool {
    ice_candidate_address(candidate)
        .map(|address| address == "127.0.0.1" || address == "::1" || address == "localhost")
        .unwrap_or(false)
}

fn is_docker_private_candidate(candidate: &str) -> bool {
    ice_candidate_address(candidate)
        .map(|address| address.starts_with("172.") || address.starts_with("10."))
        .unwrap_or(false)
}

fn turn_url_host(url: &str) -> &str {
    url.strip_prefix("turn:")
        .or_else(|| url.strip_prefix("turns:"))
        .unwrap_or(url)
        .split([':', '?', '/'])
        .next()
        .unwrap_or(url)
}

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

    // Metrics: track new connection
    counter!("sfu_websocket_connections_total").increment(1);
    gauge!("sfu_active_connections").increment(1.0);

    // Task to send messages from the channel to the WebSocket
    let participant_id_for_logging = participant_id.clone();
    let mut send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            match serde_json::to_string(&message) {
                Ok(json) => {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        sender.send(Message::Text(json.into())),
                    )
                    .await
                    {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            error!("✗ Send failed to {}: {}", participant_id_for_logging, e);
                            break;
                        }
                        Err(_) => {
                            error!("✗ Send timed out to {}", participant_id_for_logging);
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "✗ Serialize failed for {}: {}",
                        participant_id_for_logging, e
                    );
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
                    info!(
                        "WebSocket closing for participant: {}",
                        participant_id_clone
                    );
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
            cleanup_participant_connection(&room_repo, None, &participant_id).await;
        }
        result = &mut recv_task => {
            send_task.abort();

            // Clean up: remove participant from room
            if let Ok((Some(room_id), participant_id)) = result {
                cleanup_participant_connection(&room_repo, Some(room_id.as_str()), &participant_id).await;
            }
        }
    }

    info!("WebSocket connection closed: {}", participant_id);

    // Metrics: track disconnection
    counter!("sfu_websocket_disconnections_total").increment(1);
    gauge!("sfu_active_connections").decrement(1.0);
}

async fn cleanup_participant_connection(
    room_repo: &Arc<dyn RoomRepository>,
    known_room_id: Option<&str>,
    participant_id: &str,
) {
    if let Some(room_id) = known_room_id {
        cleanup_participant_from_room(room_repo, room_id, participant_id).await;
        return;
    }

    for room_id in room_repo.list_rooms().await {
        let removed = cleanup_participant_from_room(room_repo, &room_id, participant_id).await;
        if removed {
            return;
        }
    }
}

async fn cleanup_participant_from_room(
    room_repo: &Arc<dyn RoomRepository>,
    room_id: &str,
    participant_id: &str,
) -> bool {
    let Some(room_lock) = room_repo.get_room(room_id).await else {
        return false;
    };

    let mut room = room_lock.write().await;
    if !room.participants.contains_key(participant_id) {
        return false;
    }

    room.remove_participant(participant_id).await;

    // Delete room if empty
    if room.is_empty() {
        drop(room); // Release lock before deleting
        let _ = room_repo.delete_room(room_id).await;
        info!("Deleted empty room: {}", room_id);
        // Metrics: room deleted
        counter!("sfu_rooms_deleted_total").increment(1);
        gauge!("sfu_active_rooms").decrement(1.0);
    }

    true
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
                // Metrics: new room created
                counter!("sfu_rooms_created_total").increment(1);
                gauge!("sfu_active_rooms").increment(1.0);
            }

            // Get the room
            if let Some(room_lock) = room_repo.get_room(&room_id).await {
                let mut room = room_lock.write().await;

                // Get list of existing participants before adding new one (including media states)
                let existing_participants = room.get_participants_with_media_state();

                // Create participant and add to room
                let participant = Participant::new(participant_id.to_string(), name.clone());
                let mut participant_conn = ParticipantConnection::new(participant, tx.clone());

                // Create WebRTC peer connection for this participant
                // Use PUBLIC_IP env var for NAT traversal (production), or None (localhost dev)
                let mut config = PeerConnectionConfig::default();

                // Configure NAT1to1 IP if PUBLIC_IP is set
                if let Ok(ip) = std::env::var("PUBLIC_IP") {
                    if !ip.is_empty() {
                        config = config.with_public_ip(ip);
                    }
                }

                // Configure UDP port range if both UDP_PORT_MIN and UDP_PORT_MAX are set
                if let (Ok(min_str), Ok(max_str)) =
                    (std::env::var("UDP_PORT_MIN"), std::env::var("UDP_PORT_MAX"))
                {
                    if let (Ok(min), Ok(max)) = (min_str.parse::<u16>(), max_str.parse::<u16>()) {
                        config = config.with_udp_port_range(min, max);
                    }
                }

                // Configure TURN server for users behind symmetric NAT/restrictive firewalls
                if let (Ok(turn_url), Ok(turn_user), Ok(turn_password)) = (
                    std::env::var("TURN_URL"),
                    std::env::var("TURN_USER"),
                    std::env::var("TURN_PASSWORD"),
                ) {
                    if !turn_url.is_empty() && !turn_user.is_empty() {
                        info!("Configuring TURN server: {}", turn_url);
                        config = config.with_turn_server(turn_url, turn_user, turn_password);
                    }
                }

                // Configure custom STUN server if provided (in addition to default Google STUN)
                if let Ok(stun_url) = std::env::var("STUN_URL") {
                    if !stun_url.is_empty() {
                        config = config.with_stun_server(stun_url);
                    }
                }

                let turn_hosts: Vec<String> = config
                    .ice_servers
                    .iter()
                    .flat_map(|server| server.urls.iter())
                    .filter(|url| url.starts_with("turn:") || url.starts_with("turns:"))
                    .map(|url| turn_url_host(url).to_string())
                    .collect();

                info!(
                    "ICE config for {}: public_ip={}, udp_range={}, turn_hosts={:?}",
                    participant_id,
                    config.public_ip.as_deref().unwrap_or("<none>"),
                    config
                        .udp_port_range
                        .map(|(min, max)| format!("{}-{}", min, max))
                        .unwrap_or_else(|| "<ephemeral>".to_string()),
                    turn_hosts
                );

                if config.public_ip.is_none() {
                    warn!(
                        "PUBLIC_IP is not configured for {}. Remote browsers may receive Docker/private ICE candidates and fail to connect media.",
                        participant_id
                    );
                }

                if turn_hosts
                    .iter()
                    .any(|host| host == "localhost" || host == "127.0.0.1")
                {
                    warn!(
                        "TURN host for {} resolves to loopback. This only works for same-machine browser tests.",
                        participant_id
                    );
                }

                match SfuPeerConnection::new(participant_id.to_string(), config).await {
                    Ok(peer_conn) => {
                        // Metrics: peer connection created successfully
                        counter!("sfu_peer_connections_created_total").increment(1);
                        // Set up ICE candidate handler to send SFU candidates to client
                        {
                            let pc = peer_conn.lock().await;
                            let raw_pc = pc.get_peer_connection();
                            let tx_ice = tx.clone();
                            let participant_id_ice = participant_id.to_string();
                            let room_id_ice = room_id.clone();
                            let candidate_counts =
                                Arc::new(StdMutex::new(CandidateTypeCounts::default()));
                            let candidate_counts_for_handler = Arc::clone(&candidate_counts);

                            raw_pc.on_ice_candidate(Box::new(move |candidate| {
                                let tx = tx_ice.clone();
                                let participant_id = participant_id_ice.clone();
                                let candidate_counts = Arc::clone(&candidate_counts_for_handler);
                                Box::pin(async move {
                                    if let Some(candidate) = candidate {
                                        match candidate.to_json() {
                                            Ok(init) => {
                                                if let Ok(mut counts) = candidate_counts.lock() {
                                                    counts.record(&init.candidate);
                                                }

                                                if is_loopback_candidate(&init.candidate) {
                                                    warn!(
                                                        "SFU generated loopback ICE candidate for {}: {}. Remote browsers cannot use this candidate.",
                                                        participant_id, init.candidate
                                                    );
                                                } else if is_docker_private_candidate(&init.candidate) {
                                                    warn!(
                                                        "SFU generated private/container ICE candidate for {}: {}. Ensure PUBLIC_IP/NAT mapping is configured for remote browsers.",
                                                        participant_id, init.candidate
                                                    );
                                                }

                                                info!(
                                                    "SFU generated ICE candidate for {}: {}",
                                                    participant_id, init.candidate
                                                );
                                                let _ = tx.send(SignalingMessage::IceCandidate {
                                                    target_id: participant_id,
                                                    candidate: init.candidate,
                                                    sdp_mid: init.sdp_mid,
                                                    sdp_m_line_index: init.sdp_mline_index,
                                                });
                                            }
                                            Err(e) => {
                                                warn!("Failed to serialize ICE candidate: {}", e);
                                            }
                                        }
                                    } else {
                                        if let Ok(counts) = candidate_counts.lock() {
                                            info!(
                                                "ICE gathering complete for {}: host={}, srflx={}, relay={}, other={}",
                                                participant_id,
                                                counts.host,
                                                counts.srflx,
                                                counts.relay,
                                                counts.other
                                            );
                                        } else {
                                            info!("ICE gathering complete for {}", participant_id);
                                        }
                                    }
                                })
                            }));

                            let raw_pc_for_check = Arc::clone(&raw_pc);
                            let participant_id_for_check = participant_id.to_string();
                            tokio::spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                                let ice_state = raw_pc_for_check.ice_connection_state();
                                let peer_state = raw_pc_for_check.connection_state();

                                if !matches!(
                                    ice_state,
                                    webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Connected
                                        | webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Completed
                                        | webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Closed
                                ) {
                                    warn!(
                                        "Participant {} in room {} negotiated but ICE is still {:?} after 8s (peer={:?}). Check PUBLIC_IP, TURN external-ip, and firewall/UDP reachability.",
                                        participant_id_for_check,
                                        room_id_ice,
                                        ice_state,
                                        peer_state
                                    );
                                }
                            });
                        }

                        // Set up track handler to forward media to other participants
                        let room_lock_clone = Arc::clone(&room_lock);
                        let participant_id_clone = participant_id.to_string();

                        {
                            let pc = peer_conn.lock().await;
                            // Get the raw peer connection for PLI forwarding
                            let sender_peer_connection = pc.get_peer_connection();

                            pc.on_track(move |track, receiver| {
                                let room_lock = Arc::clone(&room_lock_clone);
                                let participant_id = participant_id_clone.clone();
                                let sender_pc = Arc::clone(&sender_peer_connection);

                                tokio::spawn(async move {
                                    if let Some(room_lock) = Some(room_lock) {
                                        let mut room = room_lock.write().await;
                                        room.handle_incoming_track(
                                            &participant_id,
                                            track,
                                            receiver,
                                            sender_pc,
                                        )
                                        .await;
                                    }
                                });
                            });
                        }

                        // Store peer connection in participant
                        participant_conn.set_peer_connection(peer_conn);
                    }
                    Err(e) => {
                        error!(
                            "Failed to create peer connection for {}: {}",
                            participant_id, e
                        );
                        // Metrics: peer connection failed
                        counter!("sfu_peer_connection_failures_total").increment(1);
                        let _ = tx.send(SignalingMessage::Error {
                            message: format!("Failed to create peer connection: {}", e),
                        });
                        return;
                    }
                }

                room.add_participant(participant_conn);
                // Metrics: participant joined
                counter!("sfu_participants_joined_total").increment(1);

                // Update state
                *current_room_id = Some(room_id.clone());
                *participant_name = Some(name.clone());

                // Send confirmation to the joining participant
                let _ = tx.send(SignalingMessage::Joined {
                    participant_id: participant_id.to_string(),
                    participant_name: name,
                });

                // Send list of existing participants to the new joiner (with media states)
                for (id, name, audio_enabled, video_enabled) in existing_participants {
                    // Send ParticipantJoined first
                    let _ = tx.send(SignalingMessage::ParticipantJoined {
                        participant_id: id.clone(),
                        participant_name: name,
                    });

                    // If media state is not default (both enabled), send MediaStateChanged
                    if !audio_enabled || !video_enabled {
                        let _ = tx.send(SignalingMessage::MediaStateChanged {
                            participant_id: id,
                            audio_enabled,
                            video_enabled,
                        });
                    }
                }

                info!(
                    "✓ {} joined room {} ({} total)",
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

        SignalingMessage::Offer { target_id: _, sdp } => {
            // In SFU mode, client sends offer to server (not to other participants)
            // Server processes offer and sends back an answer
            if let Some(room_id) = current_room_id {
                if let Some(room_lock) = room_repo.get_room(room_id).await {
                    let room = room_lock.read().await;

                    // Get this participant's peer connection
                    if let Some(participant_conn) = room.participants.get(participant_id) {
                        if let Some(peer_conn) = participant_conn.get_peer_connection() {
                            let peer_conn_lock = peer_conn.lock().await;

                            // Set remote description (client's offer)
                            match peer_conn_lock.set_remote_description(sdp, "offer").await {
                                Ok(_) => {
                                    // Create answer WITHOUT adding existing tracks
                                    // We'll send existing tracks via a separate renegotiation offer
                                    match peer_conn_lock.create_answer().await {
                                        Ok(answer) => {
                                            // Send answer back to client
                                            let _ = tx.send(SignalingMessage::Answer {
                                                target_id: participant_id.to_string(),
                                                sdp: answer.sdp,
                                            });

                                            // After initial connection, send existing tracks via renegotiation
                                            // This is more reliable than adding tracks in the initial answer
                                            // Clone the track info for use in async task (sender_id, track, sender_peer_connection, sender_ssrc, packet_broadcaster)
                                            let existing_tracks: Vec<(
                                                String,
                                                Vec<(
                                                    Arc<TrackRemote>,
                                                    Arc<webrtc::peer_connection::RTCPeerConnection>,
                                                    u32,
                                                    tokio::sync::broadcast::Sender<
                                                        webrtc::rtp::packet::Packet,
                                                    >,
                                                )>,
                                            )> =
                                                room.participant_tracks
                                                    .iter()
                                                    .filter(|(id, _)| *id != participant_id)
                                                    .map(|(id, tracks)| {
                                                        (
                                                            id.clone(),
                                                            tracks
                                                                .iter()
                                                                .map(|info| {
                                                                    (
                                                        Arc::clone(&info.track),
                                                        Arc::clone(&info.sender_peer_connection),
                                                        info.sender_ssrc,
                                                        info.packet_broadcaster.clone(),
                                                    )
                                                                })
                                                                .collect(),
                                                        )
                                                    })
                                                    .collect();

                                            if !existing_tracks.is_empty() {
                                                let peer_conn_for_renego = Arc::clone(&peer_conn);
                                                let tx_for_renego = tx.clone();
                                                let participant_id_for_renego =
                                                    participant_id.to_string();
                                                let pending_negotiated_tracks_ref =
                                                    Arc::clone(&room.pending_negotiated_tracks);

                                                // Get the late joiner's shutdown receiver - used to stop writer tasks when they disconnect
                                                let shutdown_rx_for_late_joiner =
                                                    participant_conn.get_shutdown_receiver();

                                                // Collect sender info for StreamOwner messages
                                                let sender_info: std::collections::HashMap<
                                                    String,
                                                    String,
                                                > = room
                                                    .participants
                                                    .iter()
                                                    .map(|(id, conn)| {
                                                        (id.clone(), conn.participant.name.clone())
                                                    })
                                                    .collect();

                                                info!(
                                                    "Will send {} existing tracks to {} via renegotiation",
                                                    existing_tracks
                                                        .iter()
                                                        .map(|(_, tracks)| tracks.len())
                                                        .sum::<usize>(),
                                                    participant_id
                                                );

                                                tokio::spawn(async move {
                                                    let peer_conn_lock =
                                                        peer_conn_for_renego.lock().await;
                                                    let pc = peer_conn_lock.get_peer_connection();

                                                    use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
                                                    use webrtc::rtp_transceiver::rtp_sender::RTCRtpSender;
                                                    use webrtc::peer_connection::RTCPeerConnection;

                                                    // Include sender_peer_connection, sender_ssrc, and packet_broadcaster for late joiner subscription
                                                    let mut pending_forwards: Vec<(
                                                        Arc<TrackRemote>,
                                                        Arc<TrackLocalStaticRTP>,
                                                        Arc<RTCRtpSender>,
                                                        String,
                                                        String,
                                                        Arc<RTCPeerConnection>,
                                                        u32,
                                                        tokio::sync::broadcast::Sender<
                                                            webrtc::rtp::packet::Packet,
                                                        >,
                                                    )> = Vec::new();

                                                    // Add all existing tracks to peer connection
                                                    for (sender_id, track_tuples) in existing_tracks
                                                    {
                                                        // Send StreamOwner for each sender's stream
                                                        if let Some((first_track, _, _, _)) =
                                                            track_tuples.first()
                                                        {
                                                            let sender_name = sender_info
                                                                .get(&sender_id)
                                                                .cloned()
                                                                .unwrap_or_else(|| {
                                                                    "Unknown".to_string()
                                                                });
                                                            let _ = tx_for_renego.send(
                                                                SignalingMessage::StreamOwner {
                                                                    stream_id: first_track
                                                                        .stream_id(),
                                                                    participant_id: sender_id
                                                                        .clone(),
                                                                    participant_name: sender_name,
                                                                },
                                                            );
                                                        }

                                                        for (
                                                            track,
                                                            sender_pc,
                                                            sender_ssrc,
                                                            packet_broadcaster,
                                                        ) in track_tuples
                                                        {
                                                            match Room::create_forwarding_track(
                                                                &track,
                                                            )
                                                            .await
                                                            {
                                                                Ok(local_track) => {
                                                                    match pc
                                                                        .add_track(Arc::clone(
                                                                            &local_track,
                                                                        )
                                                                            as Arc<
                                                                                dyn TrackLocal
                                                                                    + Send
                                                                                    + Sync,
                                                                            >)
                                                                        .await
                                                                    {
                                                                        Ok(rtp_sender) => {
                                                                            info!(
                                                                                "Added existing {} track from {} to {}",
                                                                                track.kind(),
                                                                                sender_id,
                                                                                participant_id_for_renego
                                                                            );
                                                                            pending_forwards
                                                                                .push((
                                                                                Arc::clone(&track),
                                                                                local_track,
                                                                                rtp_sender,
                                                                                sender_id
                                                                                    .to_string(),
                                                                                track
                                                                                    .id()
                                                                                    .to_string(),
                                                                                Arc::clone(
                                                                                    &sender_pc,
                                                                                ),
                                                                                sender_ssrc,
                                                                                packet_broadcaster,
                                                                            ));
                                                                        }
                                                                        Err(e) => {
                                                                            error!(
                                                                                "Failed to add track: {}",
                                                                                e
                                                                            );
                                                                        }
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    error!(
                                                                        "Failed to create forwarding track: {}",
                                                                        e
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }

                                                    drop(peer_conn_lock);

                                                    // Send renegotiation offer and mark tracks as pending only on success.
                                                    // A non-stable signaling state is glare/collision; retry briefly because otherwise
                                                    // the added tracks can remain on the PeerConnection without a matching offer.
                                                    let mut offer_result = None;
                                                    for attempt in 1..=5 {
                                                        let peer_conn_lock =
                                                            peer_conn_for_renego.lock().await;
                                                        let result = peer_conn_lock
                                                            .create_offer_if_stable()
                                                            .await;
                                                        drop(peer_conn_lock);

                                                        match result {
                                                            Ok(Some(offer)) => {
                                                                offer_result = Some(Ok(offer));
                                                                break;
                                                            }
                                                            Ok(None) => {
                                                                info!(
                                                                    "Skipping renegotiation for {} - collision, retry attempt {}",
                                                                    participant_id_for_renego,
                                                                    attempt
                                                                );
                                                                tokio::time::sleep(std::time::Duration::from_millis(250 * attempt)).await;
                                                            }
                                                            Err(e) => {
                                                                offer_result = Some(Err(e));
                                                                break;
                                                            }
                                                        }
                                                    }

                                                    match offer_result {
                                                        Some(Ok(offer)) => {
                                                            info!(
                                                                "Sending renegotiation offer to {} with {} tracks",
                                                                participant_id_for_renego,
                                                                pending_forwards.len()
                                                            );
                                                            if let Err(e) =
                                                                tx_for_renego
                                                                    .send(SignalingMessage::Offer {
                                                                    target_id:
                                                                        participant_id_for_renego
                                                                            .clone(),
                                                                    sdp: offer.sdp,
                                                                })
                                                            {
                                                                warn!(
                                                                    "Failed to queue renegotiation offer to {}: {}",
                                                                    participant_id_for_renego, e
                                                                );
                                                                return;
                                                            }

                                                            // Mark tracks as pending and start RTP forwarding via broadcast subscription.
                                                            // They become negotiated only after the client answers this server offer.
                                                            let mut pending =
                                                                pending_negotiated_tracks_ref
                                                                    .write()
                                                                    .await;
                                                            for (
                                                                track_remote,
                                                                local_track,
                                                                rtp_sender,
                                                                from_id,
                                                                track_id,
                                                                sender_pc,
                                                                sender_ssrc,
                                                                packet_broadcaster,
                                                            ) in pending_forwards
                                                            {
                                                                pending
                                                                    .entry(participant_id_for_renego.clone())
                                                                    .or_insert_with(std::collections::HashSet::new)
                                                                    .insert(track_id.clone());

                                                                let track_kind =
                                                                    track_remote.kind();
                                                                let to_id =
                                                                    participant_id_for_renego
                                                                        .clone();
                                                                let from_id_clone = from_id.clone();

                                                                // Get local SSRC for this sender
                                                                let local_ssrc = match rtp_sender
                                                                    .get_parameters()
                                                                    .await
                                                                    .encodings
                                                                    .first()
                                                                {
                                                                    Some(encoding) => encoding.ssrc,
                                                                    None => {
                                                                        error!(
                                                                            "No SSRC found for RTP sender to {}",
                                                                            to_id
                                                                        );
                                                                        continue;
                                                                    }
                                                                };

                                                                Room::subscribe_new_participant_to_track(
                                                                    local_track,
                                                                    rtp_sender,
                                                                    &packet_broadcaster,
                                                                    Arc::clone(&sender_pc),
                                                                    sender_ssrc,
                                                                    local_ssrc,
                                                                    from_id.clone(),
                                                                    to_id.clone(),
                                                                    track_kind,
                                                                    shutdown_rx_for_late_joiner.clone(),
                                                                );

                                                                // CRITICAL: Send PLI to original sender to request immediate keyframe
                                                                // This ensures the new participant gets a fresh keyframe right away
                                                                if track_kind == webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video {
                                                                    let pli = PictureLossIndication {
                                                                        sender_ssrc: 0,  // Will be filled by the stack
                                                                        media_ssrc: sender_ssrc,
                                                                    };
                                                                    if let Err(e) = sender_pc.write_rtcp(&[Box::new(pli)]).await {
                                                                        error!("Failed to send PLI to {} for new participant: {}", from_id_clone, e);
                                                                    } else {
                                                                        info!("📹 Sent PLI to {} requesting keyframe for new participant {}", from_id_clone, participant_id_for_renego);
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        None => {
                                                            warn!(
                                                                "Failed to send renegotiation offer to {} after collision retries",
                                                                participant_id_for_renego
                                                            );
                                                        }
                                                        Some(Err(e)) => {
                                                            error!(
                                                                "Failed to create renegotiation offer for {}: {}",
                                                                participant_id_for_renego, e
                                                            );
                                                        }
                                                    }
                                                });
                                            }
                                        }
                                        Err(e) => {
                                            error!(
                                                "Failed to create answer for {}: {}",
                                                participant_id, e
                                            );
                                            let _ = tx.send(SignalingMessage::Error {
                                                message: format!("Failed to create answer: {}", e),
                                            });
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!(
                                        "Failed to set remote description for {}: {}",
                                        participant_id, e
                                    );
                                    let _ = tx.send(SignalingMessage::Error {
                                        message: format!("Failed to process offer: {}", e),
                                    });
                                }
                            }
                        } else {
                            warn!("No peer connection for participant {}", participant_id);
                        }
                    }
                }
            }
        }

        SignalingMessage::Answer { target_id: _, sdp } => {
            // Client sends answer in response to server-initiated renegotiation offer
            if let Some(room_id) = current_room_id {
                if let Some(room_lock) = room_repo.get_room(room_id).await {
                    let room = room_lock.read().await;

                    if let Some(participant_conn) = room.participants.get(participant_id) {
                        if let Some(peer_conn) = participant_conn.get_peer_connection() {
                            let peer_conn_lock = peer_conn.lock().await;

                            match peer_conn_lock.set_remote_description(sdp, "answer").await {
                                Ok(_) => {
                                    info!("Set remote description (answer) for {}", participant_id);

                                    // Retry renegotiation for any tracks that weren't negotiated due to collision
                                    drop(peer_conn_lock);
                                    let participant_id_for_retry = participant_id.to_string();
                                    let peer_conn_for_retry = Arc::clone(&peer_conn);
                                    let tx_for_retry = tx.clone();
                                    let room_lock_for_retry = Arc::clone(&room_lock);

                                    tokio::spawn(async move {
                                        let room = room_lock_for_retry.read().await;
                                        let peer_conn_lock = peer_conn_for_retry.lock().await;

                                        // Promote tracks from the answered server offer into the negotiated set.
                                        let pending_track_ids = {
                                            let mut pending =
                                                room.pending_negotiated_tracks.write().await;
                                            pending
                                                .remove(&participant_id_for_retry)
                                                .unwrap_or_default()
                                        };

                                        if !pending_track_ids.is_empty() {
                                            let mut negotiated =
                                                room.negotiated_tracks.write().await;
                                            negotiated
                                                .entry(participant_id_for_retry.clone())
                                                .or_insert_with(std::collections::HashSet::new)
                                                .extend(pending_track_ids);
                                        }

                                        // Get track IDs already negotiated to this participant.
                                        let negotiated = room.negotiated_tracks.read().await;
                                        let already_negotiated = negotiated
                                            .get(&participant_id_for_retry)
                                            .cloned()
                                            .unwrap_or_default();
                                        drop(negotiated);

                                        // Check for tracks not yet negotiated (uses track_id, not stream_id)
                                        let all_track_ids: Vec<String> = room
                                            .participant_tracks
                                            .iter()
                                            .filter(|(id, _)| *id != &participant_id_for_retry)
                                            .flat_map(|(_, tracks)| tracks)
                                            .map(|track_info| track_info.track.id().to_string())
                                            .collect();

                                        let missing_track_ids: Vec<String> = all_track_ids
                                            .iter()
                                            .filter(|track_id| {
                                                !already_negotiated.contains(*track_id)
                                            })
                                            .cloned()
                                            .collect();

                                        if !missing_track_ids.is_empty() {
                                            info!(
                                                "🔄 Retrying for {} - {} tracks pending: {:?}",
                                                participant_id_for_retry,
                                                missing_track_ids.len(),
                                                missing_track_ids
                                            );

                                            // Send new offer including pending tracks
                                            match peer_conn_lock.create_offer_if_stable().await {
                                                Ok(Some(offer)) => {
                                                    info!(
                                                        "✓ Retry offer sent for {}",
                                                        participant_id_for_retry
                                                    );
                                                    if let Err(e) =
                                                        tx_for_retry.send(SignalingMessage::Offer {
                                                            target_id: participant_id_for_retry
                                                                .clone(),
                                                            sdp: offer.sdp,
                                                        })
                                                    {
                                                        warn!(
                                                            "Failed to queue retry offer to {}: {}",
                                                            participant_id_for_retry, e
                                                        );
                                                        return;
                                                    }

                                                    // Mark these tracks pending until the client answers this retry offer.
                                                    let mut pending = room
                                                        .pending_negotiated_tracks
                                                        .write()
                                                        .await;
                                                    for track_id in missing_track_ids {
                                                        pending
                                                            .entry(participant_id_for_retry.clone())
                                                            .or_insert_with(
                                                                std::collections::HashSet::new,
                                                            )
                                                            .insert(track_id);
                                                    }
                                                }
                                                Ok(None) => {
                                                    info!(
                                                        "⚠ Still in collision for {}, will retry on next answer",
                                                        participant_id_for_retry
                                                    );
                                                }
                                                Err(e) => {
                                                    error!(
                                                        "✗ Retry failed for {}: {}",
                                                        participant_id_for_retry, e
                                                    );
                                                }
                                            }
                                        } else {
                                            info!(
                                                "✓ All tracks negotiated to {}",
                                                participant_id_for_retry
                                            );
                                        }
                                    });
                                }
                                Err(e) => {
                                    error!("Failed to set answer for {}: {}", participant_id, e);
                                }
                            }
                        }
                    }
                }
            }
        }

        SignalingMessage::IceCandidate {
            target_id: _,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => {
            // Client sends ICE candidate to server
            if let Some(room_id) = current_room_id {
                if let Some(room_lock) = room_repo.get_room(room_id).await {
                    let room = room_lock.read().await;

                    // Get this participant's peer connection
                    if let Some(participant_conn) = room.participants.get(participant_id) {
                        if let Some(peer_conn) = participant_conn.get_peer_connection() {
                            let peer_conn_lock = peer_conn.lock().await;

                            // Add ICE candidate to peer connection
                            match peer_conn_lock
                                .add_ice_candidate(
                                    candidate.clone(),
                                    sdp_mid.clone(),
                                    sdp_m_line_index,
                                )
                                .await
                            {
                                Ok(_) => {
                                    info!(
                                        "Added ICE candidate for participant {}: typ={}, protocol={}",
                                        participant_id,
                                        ice_candidate_type(&candidate),
                                        ice_candidate_protocol(&candidate)
                                    );
                                    // Metrics: ICE candidate received
                                    counter!("sfu_ice_candidates_received_total").increment(1);
                                }
                                Err(e) => {
                                    error!(
                                        "Failed to add ICE candidate for {}: {}",
                                        participant_id, e
                                    );
                                }
                            }
                        } else {
                            warn!("No peer connection for participant {}", participant_id);
                        }
                    }
                }
            }
        }

        SignalingMessage::MediaStateChanged {
            participant_id: _,
            audio_enabled,
            video_enabled,
        } => {
            // Update participant's media state and broadcast to others
            if let Some(room_id) = current_room_id {
                if let Some(room_lock) = room_repo.get_room(room_id).await {
                    let mut room = room_lock.write().await;

                    // Update the participant's stored media state
                    if let Some(participant_conn) = room.participants.get_mut(participant_id) {
                        participant_conn.participant.audio_enabled = audio_enabled;
                        participant_conn.participant.video_enabled = video_enabled;
                    }

                    info!(
                        "📢 {} toggled media: audio={}, video={}",
                        participant_id, audio_enabled, video_enabled
                    );
                    room.broadcast_except(
                        participant_id,
                        SignalingMessage::MediaStateChanged {
                            participant_id: participant_id.to_string(),
                            audio_enabled,
                            video_enabled,
                        },
                    );
                }
            }
        }

        SignalingMessage::ChatMessage {
            participant_id: _,
            participant_name: _,
            message,
            timestamp,
        } => {
            // Broadcast chat message to all participants in the room (including sender)
            if let Some(room_id) = current_room_id {
                if let Some(room_lock) = room_repo.get_room(room_id).await {
                    let room = room_lock.read().await;
                    let sender_name = participant_name
                        .clone()
                        .unwrap_or_else(|| "Unknown".to_string());

                    info!("💬 {} sent message in room {}", sender_name, room_id);
                    room.broadcast(SignalingMessage::ChatMessage {
                        participant_id: participant_id.to_string(),
                        participant_name: sender_name,
                        message,
                        timestamp,
                    });
                }
            }
        }

        _ => {
            warn!("Unhandled message type from {}", participant_id);
        }
    }
}
