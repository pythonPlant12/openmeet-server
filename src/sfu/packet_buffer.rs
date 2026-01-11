use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::info;
use webrtc::rtp::packet::Packet as RtpPacket;

/// Buffer size - store last 512 packets (roughly 5-10 seconds of video at 30fps)
const BUFFER_SIZE: usize = 512;

/// A ring buffer for storing RTP packets for NACK retransmission
/// Thread-safe wrapper around the internal buffer
pub struct RtpPacketBuffer {
    inner: RwLock<PacketBufferInner>,
}

struct PacketBufferInner {
    /// Packets stored by sequence number
    packets: HashMap<u16, RtpPacket>,
    /// Circular list of sequence numbers in order they were added
    sequence_order: Vec<u16>,
    /// Current write position in sequence_order
    write_pos: usize,
}

impl RtpPacketBuffer {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(PacketBufferInner {
                packets: HashMap::with_capacity(BUFFER_SIZE),
                sequence_order: Vec::with_capacity(BUFFER_SIZE),
                write_pos: 0,
            }),
        }
    }

    /// Store a packet in the buffer
    pub async fn store(&self, packet: &RtpPacket) {
        let mut inner = self.inner.write().await;
        let seq = packet.header.sequence_number;

        // If buffer is full, remove the oldest packet
        if inner.sequence_order.len() >= BUFFER_SIZE {
            let write_pos = inner.write_pos;
            let old_seq = inner.sequence_order[write_pos];
            inner.packets.remove(&old_seq);
            inner.sequence_order[write_pos] = seq;
        } else {
            inner.sequence_order.push(seq);
        }

        // Store the new packet (clone to own the data)
        inner.packets.insert(seq, packet.clone());

        // Advance write position
        inner.write_pos = (inner.write_pos + 1) % BUFFER_SIZE;
    }

    /// Get a packet by sequence number for retransmission
    pub async fn get(&self, seq: u16) -> Option<RtpPacket> {
        let inner = self.inner.read().await;
        inner.packets.get(&seq).cloned()
    }

    /// Get multiple packets by sequence numbers
    pub async fn get_many(&self, seqs: &[u16]) -> Vec<RtpPacket> {
        let inner = self.inner.read().await;
        seqs.iter()
            .filter_map(|seq| inner.packets.get(seq).cloned())
            .collect()
    }
}

impl Default for RtpPacketBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RtpPacketBuffer {
    fn drop(&mut self) {
        // Use try_read to avoid blocking - this is in Drop so we can't await
        if let Ok(inner) = self.inner.try_read() {
            info!(
                "DROP RtpPacketBuffer: {} packets stored",
                inner.packets.len()
            );
        } else {
            info!("DROP RtpPacketBuffer: (lock held, couldn't read size)");
        }
    }
}
