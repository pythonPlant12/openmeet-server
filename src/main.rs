mod auth;
mod db;
mod schema;
mod signaling;
mod sfu;

use axum::{routing::get, Router};
use axum_server::tls_rustls::RustlsConfig;
use diesel::{Connection, PgConnection};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use crate::auth::{auth_routes, JwtConfig};
use crate::db::{create_pool, DbPool};
use crate::signaling::handler::websocket_handler;
use crate::sfu::repository::{InMemoryRoomRepository, RoomRepository};

// Embed migrations at compile time
const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub jwt: JwtConfig,
    pub room_repo: Arc<dyn RoomRepository>,
}

// Allows WebSocket handler to extract just room_repo from AppState
// This way the handler doesn't need to know about auth-related fields
impl axum::extract::FromRef<AppState> for Arc<dyn RoomRepository> {
    fn from_ref(state: &AppState) -> Self {
        Arc::clone(&state.room_repo)
    }
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("Starting OpenMeet SFU server...");

    // Database URL
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    // Run migrations (uses sync connection, then drops it)
    {
        let mut conn = PgConnection::establish(&database_url)
            .expect("Failed to connect to database for migrations");
        conn.run_pending_migrations(MIGRATIONS)
            .expect("Failed to run migrations");
        info!("Database migrations completed");
    }

    // Create async connection pool
    let pool = create_pool(&database_url);
    info!("Database pool initialized");

    // JWT configuration
    let jwt_secret = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");
    let access_token_minutes = std::env::var("ACCESS_TOKEN_MINUTES")
        .unwrap_or_else(|_| "15".to_string())
        .parse()
        .unwrap_or(15);
    let refresh_token_days = std::env::var("REFRESH_TOKEN_DAYS")
        .unwrap_or_else(|_| "7".to_string())
        .parse()
        .unwrap_or(7);
    let jwt = JwtConfig::new(&jwt_secret, access_token_minutes, refresh_token_days);

    // Room repository (in-memory)
    let room_repo: Arc<dyn RoomRepository> = Arc::new(InMemoryRoomRepository::new());

    let state = AppState {
        pool,
        jwt,
        room_repo,
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/ws", get(websocket_handler))
        .nest("/auth", auth_routes())
        .layer(cors)
        .with_state(state);

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
