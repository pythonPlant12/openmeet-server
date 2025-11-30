mod signaling;
mod sfu;

use axum::{routing::get, Router};
use axum_server::tls_rustls::RustlsConfig;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;
use tracing_subscriber;

use crate::signaling::handler::websocket_handler;
use crate::sfu::repository::{InMemoryRoomRepository, RoomRepository};

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("Starting OpenMeet SFU server...");

    let room_repo: Arc<dyn RoomRepository> = Arc::new(InMemoryRoomRepository::new());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/ws", get(websocket_handler))
        .layer(cors)
        .with_state(room_repo);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8081));
    let use_tls = std::env::var("USE_TLS").unwrap_or_default() == "true";

    if use_tls {
        let cert_path = PathBuf::from(
            std::env::var("SSL_CERT_PATH").unwrap_or_else(|_| "../certs/localhost+3.pem".to_string()),
        );
        let key_path = PathBuf::from(
            std::env::var("SSL_KEY_PATH").unwrap_or_else(|_| "../certs/localhost+3-key.pem".to_string()),
        );
        let tls_config = RustlsConfig::from_pem_file(&cert_path, &key_path)
            .await
            .expect("Failed to load TLS certificates");

        info!("Server listening on https://0.0.0.0:8081 (TLS enabled)");
        info!("WebSocket endpoint: wss://localhost:8081/ws");

        axum_server::bind_rustls(addr, tls_config)
            .serve(app.into_make_service())
            .await
            .expect("Server failed to start");
    } else {
        info!("Server listening on http://0.0.0.0:8081 (TLS disabled, use reverse proxy)");
        info!("WebSocket endpoint: ws://localhost:8081/ws");

        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app)
            .await
            .expect("Server failed to start");
    }
}

async fn health_check() -> &'static str {
    "OK"
}
