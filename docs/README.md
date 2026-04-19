# OpenMeet Server Documentation

The OpenMeet Server is a high-performance Selective Forwarding Unit (SFU) built in Rust that handles WebRTC media routing, WebSocket signaling, and user authentication for the OpenMeet video conferencing platform.

## Overview

The server acts as a media router, receiving audio/video streams from participants and forwarding them to other participants in the same room. This SFU architecture is more scalable than peer-to-peer mesh networks and more efficient than traditional MCU (Multipoint Control Unit) systems.

### Key Features

- **SFU Media Routing**: Efficient selective forwarding of media streams
- **WebSocket Signaling**: Real-time signaling for WebRTC negotiation
- **WebRTC Support**: Full WebRTC stack with STUN/TURN integration
- **User Authentication**: JWT-based authentication with refresh tokens
- **PostgreSQL Database**: Persistent storage for user data
- **Metrics**: Prometheus metrics for monitoring
- **Memory Efficient**: Uses jemalloc for optimized memory management
- **RTCP Feedback**: PLI (keyframe request) and NACK (packet retransmission)
- **Packet Buffering**: RTP packet buffering for retransmission

## Technology Stack

- **Rust**: Systems programming language for performance and safety
- **Axum**: Modern async web framework
- **Tokio**: Async runtime for concurrent operations
- **WebRTC**: Media streaming library (webrtc-rs)
- **Diesel**: Type-safe ORM for PostgreSQL
- **PostgreSQL**: Relational database
- **JWT**: JSON Web Tokens for authentication
- **Argon2**: Password hashing
- **Prometheus**: Metrics collection
- **Jemalloc**: Memory allocator

## Project Structure

```
openmeet-server/
├── src/
│   ├── main.rs                  # Application entry point
│   │
│   ├── auth/                    # Authentication module
│   │   ├── mod.rs               # Module exports
│   │   ├── handlers.rs          # HTTP request handlers
│   │   ├── routes.rs            # Route definitions
│   │   ├── models.rs            # Database models
│   │   └── jwt.rs               # JWT token management
│   │
│   ├── db/                      # Database configuration
│   │   └── mod.rs               # Connection pool setup
│   │
│   ├── signaling/               # WebSocket signaling
│   │   ├── mod.rs               # Module exports
│   │   ├── handler.rs           # WebSocket handler
│   │   └── message.rs           # Message types
│   │
│   ├── sfu/                     # SFU media routing
│   │   ├── mod.rs               # Module exports
│   │   ├── room.rs              # Room management
│   │   ├── participant.rs       # Participant connection
│   │   ├── peer_connection.rs   # WebRTC peer connection
│   │   ├── repository.rs        # Room repository (in-memory)
│   │   └── packet_buffer.rs     # RTP packet buffering
│   │
│   └── schema.rs                # Database schema (Diesel)
│
├── migrations/                  # Database migrations
├── Cargo.toml                   # Dependencies
└── docs/                        # Documentation
```

## Core Components

### 1. Main Application (`src/main.rs`)

The entry point sets up:
- Database connection pool
- JWT configuration
- In-memory room repository
- Prometheus metrics
- HTTP server with routes
- TLS configuration (optional)

**Key Routes:**
- `GET /health`: Health check endpoint
- `GET /ws`: WebSocket upgrade for signaling
- `GET /metrics`: Prometheus metrics endpoint
- `POST /auth/register`: User registration
- `POST /auth/login`: User login
- `POST /auth/refresh`: Token refresh

### 2. Authentication Module (`src/auth/`)

Handles user authentication and authorization:

#### `handlers.rs`
HTTP request handlers for auth endpoints:
- `register()`: Create new user account
- `login()`: Authenticate user and issue tokens
- `refresh()`: Refresh access token using refresh token

#### `jwt.rs`
JWT token management:
- Token generation and validation
- Access tokens (short-lived, 15 minutes)
- Refresh tokens (long-lived, 7 days)
- Token claims and verification

#### `models.rs`
Database models:
```rust
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}
```

**Security Features:**
- Argon2 password hashing
- JWT with HMAC-SHA256
- Secure token refresh flow
- Password validation

### 3. Signaling Module (`src/signaling/`)

WebSocket-based signaling for WebRTC:

#### `handler.rs`
WebSocket connection handler:

**Key Functions:**
- `websocket_handler()`: Upgrades HTTP to WebSocket
- `handle_socket()`: Manages individual WebSocket connections
- `handle_message()`: Routes signaling messages

**Connection Lifecycle:**
```
WebSocket connects
  ↓
Assign participant ID
  ↓
Start send/receive tasks
  ↓
Handle signaling messages
  ↓
Clean up on disconnect
```

#### `message.rs`
Signaling message types:

```rust
pub enum SignalingMessage {
    Join { room_id, participant_name },
    Joined { participant_id, participant_name },
    Offer { target_id, sdp },
    Answer { target_id, sdp },
    IceCandidate { target_id, candidate, ... },
    ParticipantJoined { participant_id, participant_name },
    ParticipantLeft { participant_id },
    StreamOwner { stream_id, participant_id, ... },
    MediaStateChanged { participant_id, audio_enabled, video_enabled },
    ChatMessage { participant_id, message, timestamp },
    Error { message },
}
```

### 4. SFU Module (`src/sfu/`)

Core media routing implementation:

#### `room.rs` (src/sfu/room.rs)
Manages video conference rooms:

**Key Structures:**
```rust
pub struct Room {
    pub id: String,
    pub participants: HashMap<String, ParticipantConnection>,
    pub participant_tracks: HashMap<String, Vec<SenderTrackInfo>>,
    pub negotiated_tracks: Arc<RwLock<HashMap<String, HashSet<String>>>>,
}
```

**Key Methods:**
- `add_participant()`: Add participant to room
- `remove_participant()`: Remove and cleanup participant
- `handle_incoming_track()`: Process new media track from participant
- `forward_track_to_others()`: Route track to other participants
- `broadcast()`: Send message to all participants
- `broadcast_except()`: Send to all except one

**Media Forwarding Pattern:**
```
Participant A sends track
  ↓
Room receives track
  ↓
Create broadcast channel
  ↓
Spawn reader task (reads from track)
  ↓
Spawn writer tasks (write to each participant)
  ↓
Packets flow: A → reader → broadcast → writers → B, C, D
```

#### `participant.rs` (src/sfu/participant.rs)
Participant connection wrapper:

```rust
pub struct ParticipantConnection {
    pub participant: Participant,
    pub sender: UnboundedSender<SignalingMessage>,
    pub peer_connection: Option<Arc<Mutex<SfuPeerConnection>>>,
    shutdown_tx: watch::Sender<()>,
}
```

**Features:**
- Signaling message sender
- Peer connection reference
- Shutdown signal for cleanup
- Media state (audio/video enabled)

#### `peer_connection.rs` (src/sfu/peer_connection.rs)
WebRTC peer connection management:

**Configuration:**
```rust
pub struct PeerConnectionConfig {
    pub ice_servers: Vec<RTCIceServer>,
    pub public_ip: Option<String>,        // NAT traversal
    pub udp_port_range: Option<(u16, u16)>, // Port allocation
}
```

**Key Features:**
- ICE server configuration (STUN/TURN)
- NAT1to1 IP mapping for public deployment
- UDP port range control
- Media engine with default codecs
- Interceptor registry for RTCP feedback

**Methods:**
- `new()`: Create peer connection with configuration
- `create_offer()`: Generate SDP offer
- `create_answer()`: Generate SDP answer
- `set_remote_description()`: Apply remote SDP
- `add_ice_candidate()`: Add ICE candidate
- `on_track()`: Handle incoming media tracks
- `close()`: Clean up connection

#### `packet_buffer.rs` (src/sfu/packet_buffer.rs)
RTP packet buffering for NACK retransmission:

```rust
pub struct RtpPacketBuffer {
    packets: Mutex<VecDeque<RtpPacket>>,
}
```

**Features:**
- Circular buffer (512 packets)
- Sequence number-based retrieval
- Supports NACK retransmission

#### `repository.rs` (src/sfu/repository.rs)
Room storage abstraction:

```rust
pub trait RoomRepository {
    async fn create_room(&self, room_id: String) -> Result<()>;
    async fn get_room(&self, room_id: &str) -> Option<Arc<RwLock<Room>>>;
    async fn delete_room(&self, room_id: &str) -> Result<()>;
    async fn room_exists(&self, room_id: &str) -> bool;
}
```

**Implementation:**
- `InMemoryRoomRepository`: HashMap-based storage
- Thread-safe with RwLock
- Easily replaceable with Redis/database

### 5. Database Module (`src/db/`)

PostgreSQL connection management:

```rust
pub type DbPool = Pool<AsyncPgConnection>;

pub fn create_pool(database_url: &str) -> DbPool {
    let config = AsyncDieselConnectionManager::new(database_url);
    Pool::builder(config).build().unwrap()
}
```

**Features:**
- Async connection pooling
- Diesel ORM for type-safe queries
- Migration support via `diesel_migrations`

## WebRTC Flow

### 1. Client Joins Room

```
Client sends JOIN message
  ↓
Server creates/gets room
  ↓
Create Participant with peer connection
  ↓
Configure ICE servers (STUN/TURN)
  ↓
Add participant to room
  ↓
Send JOINED confirmation
  ↓
Send list of existing participants
```

### 2. WebRTC Negotiation (Client → Server)

```
Client sends OFFER (SDP)
  ↓
Server receives offer
  ↓
Set remote description
  ↓
Create ANSWER (SDP)
  ↓
Send answer to client
  ↓
Exchange ICE candidates
  ↓
Connection established
```

### 3. Track Handling

```
Client sends media track
  ↓
Server on_track() callback fires
  ↓
Store track in room
  ↓
Create broadcast channel
  ↓
Spawn reader task (reads RTP packets)
  ↓
Forward to other participants:
  - Create local track for each participant
  - Add track to peer connection
  - Send renegotiation OFFER
  - Client sends ANSWER
  - Spawn writer task
  ↓
Packets flow through broadcast channel
```

### 4. Late Joiner

```
New participant joins room with existing participants
  ↓
Create peer connection
  ↓
Client sends initial OFFER
  ↓
Server sends ANSWER
  ↓
Connection established
  ↓
Server detects existing tracks in room
  ↓
Add all existing tracks to new participant's peer connection
  ↓
Send renegotiation OFFER to client
  ↓
Client processes offer and sees new tracks
  ↓
Client sends ANSWER
  ↓
Subscribe late joiner to existing broadcast channels
  ↓
Send PLI (Picture Loss Indication) to original senders
  ↓
Original senders send keyframes
  ↓
Late joiner receives video
```

### 5. RTCP Feedback

```
Receiver experiences packet loss
  ↓
Send NACK (negative acknowledgment)
  ↓
Server retrieves packet from buffer
  ↓
Retransmit packet to receiver
```

```
Receiver needs keyframe (video corruption)
  ↓
Send PLI (Picture Loss Indication)
  ↓
Server forwards PLI to original sender
  ↓
Sender generates keyframe
  ↓
Keyframe sent to receiver
```

## Development Workflow

### Prerequisites

- Rust 1.70+ (install via [rustup](https://rustup.rs/))
- PostgreSQL 14+
- Docker (for containerized development)

### Setup

1. Install dependencies:
```bash
cd openmeet-server
```

Rust dependencies are managed by Cargo (defined in `Cargo.toml`).

2. Set up database:
```bash
# Create database
createdb openmeet

# Copy environment file
cp .env.example .env
```

Edit `.env`:
```env
DATABASE_URL=postgres://user:password@localhost:5432/openmeet
JWT_SECRET=your-secret-key-here
ACCESS_TOKEN_MINUTES=15
REFRESH_TOKEN_DAYS=7
BIND_ADDRESS=0.0.0.0:8081
USE_TLS=false
STUN_URL=stun:stun.l.google.com:19302
TURN_URL=turn:localhost:3478
TURN_USER=openmeet
TURN_PASSWORD=openmeet_dev_turn
UDP_PORT_MIN=50000
UDP_PORT_MAX=50010
```

3. Run migrations:
```bash
cargo install diesel_cli --no-default-features --features postgres
diesel migration run
```

4. Run the server:
```bash
cargo run
```

Server will start on http://0.0.0.0:8081

### Available Commands

```bash
# Development
cargo run                 # Run server
cargo watch -x run        # Auto-reload on changes

# Building
cargo build              # Debug build
cargo build --release    # Optimized release build

# Testing
cargo test               # Run tests

# Database
diesel migration generate <name>  # Create new migration
diesel migration run              # Apply migrations
diesel migration revert           # Rollback migration

# Formatting and linting
cargo fmt                # Format code
cargo clippy             # Lint code
```

## Configuration

### Environment Variables

- `DATABASE_URL`: PostgreSQL connection string
- `JWT_SECRET`: Secret key for JWT signing
- `ACCESS_TOKEN_MINUTES`: Access token expiration (default: 15)
- `REFRESH_TOKEN_DAYS`: Refresh token expiration (default: 7)
- `BIND_ADDRESS`: Server bind address (default: 0.0.0.0:8081)
- `USE_TLS`: Enable TLS (default: false)
- `SSL_CERT_PATH`: TLS certificate path
- `SSL_KEY_PATH`: TLS private key path
- `STUN_URL`: STUN server URL
- `TURN_URL`: TURN server URL
- `TURN_USER`: TURN server username
- `TURN_PASSWORD`: TURN server password
- `UDP_PORT_MIN`: Minimum UDP port for WebRTC
- `UDP_PORT_MAX`: Maximum UDP port for WebRTC
- `PUBLIC_IP`: Public IP for NAT traversal (production)
- `RUST_LOG`: Logging level (default: info)

### STUN/TURN Configuration

For reliable connectivity, configure TURN server:

```rust
// In handler.rs:
if let (Ok(turn_url), Ok(turn_user), Ok(turn_password)) = (
    std::env::var("TURN_URL"),
    std::env::var("TURN_USER"),
    std::env::var("TURN_PASSWORD"),
) {
    config = config.with_turn_server(turn_url, turn_user, turn_password);
}
```

**When to use TURN:**
- Users behind symmetric NAT
- Restrictive corporate firewalls
- Mobile networks
- Production deployments

### Port Configuration

WebRTC requires UDP ports for media:

```env
UDP_PORT_MIN=50000
UDP_PORT_MAX=50010
```

Open these ports in firewall:
```bash
sudo ufw allow 50000:50010/udp
```

## Database Schema

### Users Table

```sql
CREATE TABLE users (
    id UUID PRIMARY KEY,
    email VARCHAR(255) UNIQUE NOT NULL,
    password_hash VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);
```

Managed by Diesel migrations in `migrations/`.

## Metrics

Prometheus metrics available at `/metrics`:

**Counters:**
- `sfu_websocket_connections_total`: Total WebSocket connections
- `sfu_websocket_disconnections_total`: Total disconnections
- `sfu_rooms_created_total`: Total rooms created
- `sfu_rooms_deleted_total`: Total rooms deleted
- `sfu_participants_joined_total`: Total participants joined
- `sfu_peer_connections_created_total`: Peer connections created
- `sfu_peer_connection_failures_total`: Failed peer connections
- `sfu_ice_candidates_received_total`: ICE candidates received

**Gauges:**
- `sfu_active_connections`: Current active WebSocket connections
- `sfu_active_rooms`: Current active rooms

## Memory Management

Uses jemalloc allocator with aggressive memory return:

```rust
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
```

Configuration via environment:
```env
MALLOC_CONF=background_thread:true,dirty_decay_ms:5000,muzzy_decay_ms:5000
```

**Benefits:**
- Faster allocation than system allocator
- Returns memory to OS more aggressively
- Better for long-running servers

## Performance Considerations

### Async Runtime
- Uses Tokio for async I/O
- Each WebSocket connection runs in separate tasks
- Media forwarding uses broadcast channels (O(1) fan-out)

### Locking Strategy
- `Arc<RwLock<Room>>`: Multiple readers, single writer
- Reader tasks don't block each other
- Write locks only for participant add/remove

### Broadcast Channels
- One reader, multiple writers pattern
- Constant-time message distribution
- Bounded capacity (256 packets) prevents memory growth

## Security

### Authentication
- Argon2 password hashing (memory-hard)
- JWT with HMAC-SHA256 signing
- Short-lived access tokens
- Secure refresh token flow

### WebRTC
- DTLS encryption for media (WebRTC standard)
- SRTP for encrypted media streams
- ICE consent freshness checks

### Best Practices
- Change JWT_SECRET in production
- Use TLS in production
- Configure firewall rules
- Rate limit authentication endpoints
- Validate all user inputs

## Troubleshooting

### Connection Issues

**Check ICE candidates:**
```bash
docker logs openmeet_sfu | grep "ICE candidate"
```

Should see both host and relay (TURN) candidates.

**Check TURN connectivity:**
```bash
docker logs openmeet_coturn | grep ERROR
```

### Memory Leaks

**Monitor memory:**
```bash
docker stats openmeet_sfu
```

Memory logging in code (room.rs:25-34):
```rust
fn log_memory_usage(context: &str) {
    // Logs RSS memory usage
}
```

### Database Issues

**Check connection:**
```bash
psql $DATABASE_URL -c "SELECT 1;"
```

**View migrations:**
```bash
diesel migration list
```

## Production Deployment

### Using Docker

```bash
docker build -t openmeet-server .
docker run -p 8081:8081 \
  -e DATABASE_URL=... \
  -e JWT_SECRET=... \
  openmeet-server
```

### Checklist

- [ ] Set strong JWT_SECRET
- [ ] Configure TLS certificates
- [ ] Set up PostgreSQL with backups
- [ ] Configure TURN server
- [ ] Open firewall ports (8081, 50000-50010/udp, 3478)
- [ ] Set PUBLIC_IP for NAT traversal
- [ ] Configure reverse proxy (nginx)
- [ ] Set up monitoring (Prometheus/Grafana)
- [ ] Configure log aggregation
- [ ] Set resource limits (Docker/systemd)

## Testing

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_name

# Run with output
cargo test -- --nocapture
```

## Contributing

When contributing to the server:

1. Follow Rust style guide (rustfmt)
2. Add tests for new features
3. Update documentation
4. Run clippy for linting
5. Ensure backward compatibility

## Related Documentation

- [Main Documentation](../../docs/README.md)
- [Client Documentation](../../openmeet-client/docs/README.md)
- [Rust Documentation](https://doc.rust-lang.org/)
- [Axum Documentation](https://docs.rs/axum/)
- [WebRTC-rs Documentation](https://webrtc.rs/)
- [Diesel Documentation](https://diesel.rs/)
