pub mod auth;
pub mod db;
pub mod schema;
pub mod sfu;
pub mod signaling;
pub mod social;
pub mod storage;

use std::sync::Arc;

use axum::{
    Router,
    http::{HeaderValue, Method, header},
    routing::get,
};
use metrics_exporter_prometheus::PrometheusHandle;
use tower_http::cors::{AllowOrigin, CorsLayer};

use auth::{JwtConfig, auth_routes};
use db::DbPool;
use sfu::repository::RoomRepository;
use signaling::handler::websocket_handler;
use social::{SocialEventHub, social_routes};
use storage::AvatarStorage;

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub jwt: JwtConfig,
    pub room_repo: Arc<dyn RoomRepository>,
    pub metrics_handle: PrometheusHandle,
    pub enforce_room_access: bool,
    pub avatar_storage: Arc<dyn AvatarStorage>,
    pub social_events: SocialEventHub,
}

impl axum::extract::FromRef<AppState> for Arc<dyn RoomRepository> {
    fn from_ref(state: &AppState) -> Self {
        Arc::clone(&state.room_repo)
    }
}

pub fn build_router(state: AppState) -> Router {
    let cors = cors_layer();

    Router::new()
        .route("/health", get(health_check))
        .route("/ws", get(websocket_handler))
        .route("/metrics", get(metrics_handler))
        .nest("/auth", auth_routes())
        .nest("/social", social_routes())
        .layer(cors)
        .with_state(state)
}

fn cors_layer() -> CorsLayer {
    let origins = std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_else(|_| {
        "http://localhost:5173,http://127.0.0.1:5173,http://localhost:5174,http://127.0.0.1:5174"
            .to_string()
    });
    let origins = parse_allowed_origins(&origins);

    assert!(
        !origins.is_empty(),
        "CORS_ALLOWED_ORIGINS must not be empty"
    );

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

fn parse_allowed_origins(origins: &str) -> Vec<HeaderValue> {
    origins
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| HeaderValue::from_str(origin).expect("invalid CORS_ALLOWED_ORIGINS value"))
        .collect()
}

async fn health_check() -> &'static str {
    "OK"
}

async fn metrics_handler(axum::extract::State(state): axum::extract::State<AppState>) -> String {
    state.metrics_handle.render()
}

#[cfg(test)]
mod tests {
    use super::parse_allowed_origins;

    #[test]
    fn parses_only_explicit_origins() {
        let origins = parse_allowed_origins("https://openmeets.eu, http://localhost:5174");

        assert_eq!(origins.len(), 2);
        assert_eq!(origins[0], "https://openmeets.eu");
        assert_eq!(origins[1], "http://localhost:5174");
    }
}
