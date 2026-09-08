// Use jemalloc with aggressive memory return to OS
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use axum_server::tls_rustls::RustlsConfig;
use diesel::{Connection, PgConnection};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use metrics_exporter_prometheus::PrometheusBuilder;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use openmeet_server::AppState;
use openmeet_server::auth::JwtConfig;
use openmeet_server::build_router;
use openmeet_server::db::create_pool;
use openmeet_server::sfu::repository::{InMemoryRoomRepository, RoomRepository};
use openmeet_server::social::SocialEventHub;
use openmeet_server::storage::S3AvatarStorage;

// Embed migrations at compile time
const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

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

    let avatar_storage = Arc::new(
        S3AvatarStorage::from_env()
            .await
            .expect("RustFS avatar storage must be configured"),
    );
    avatar_storage
        .ensure_bucket()
        .await
        .expect("RustFS avatar bucket must be available");
    info!("RustFS avatar storage initialized");

    // Initialize Prometheus metrics
    let metrics_handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("Failed to install Prometheus recorder");
    info!("Prometheus metrics initialized");

    let state = AppState {
        pool,
        jwt,
        room_repo,
        metrics_handle,
        enforce_room_access: std::env::var("ENFORCE_ROOM_ACCESS").as_deref() == Ok("true"),
        avatar_storage,
        social_events: SocialEventHub::new(),
    };

    let app = build_router(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8081));
    let use_tls = std::env::var("USE_TLS").unwrap_or_default() == "true";

    if use_tls {
        let cert_path = PathBuf::from(
            std::env::var("SSL_CERT_PATH")
                .unwrap_or_else(|_| "../certs/localhost+3.pem".to_string()),
        );
        let key_path = PathBuf::from(
            std::env::var("SSL_KEY_PATH")
                .unwrap_or_else(|_| "../certs/localhost+3-key.pem".to_string()),
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
