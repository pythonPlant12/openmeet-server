mod signaling;
mod sfu;

use axum::{routing::get, Router};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;
use tracing_subscriber;

use crate::signaling::handler::websocket_handler;
use crate::sfu::repository::{InMemoryRoomRepository, RoomRepository};

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("Starting SFU backend server...");

    // Create the room repository (in-memory for now)
    let room_repo: Arc<dyn RoomRepository> = Arc::new(InMemoryRoomRepository::new());

    // Configure CORS to allow frontend connections
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Build the application with routes
    let app = Router::new()
        .route("/health", get(health_check))
        .route("/ws", get(websocket_handler))
        .layer(cors)
        .with_state(room_repo);

    // Start server on port 8080
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8081")
        .await
        .expect("Failed to bind to port 8080");

    info!("Server listening on http://0.0.0.0:8081");
    info!("Health check: http://localhost:8081/health");
    info!("WebSocket endpoint: ws://localhost:8081/ws");

    axum::serve(listener, app)
        .await
        .expect("Server failed to start");
}

// Simple health check handler
async fn health_check() -> &'static str {
    "OK"
}
