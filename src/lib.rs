pub mod auth;
pub mod db;
pub mod schema;
pub mod sfu;
pub mod signaling;
pub mod social;

use std::sync::Arc;

use axum::{Router, routing::get};
use metrics_exporter_prometheus::PrometheusHandle;
use tower_http::cors::{Any, CorsLayer};

use auth::{JwtConfig, auth_routes};
use db::DbPool;
use sfu::repository::RoomRepository;
use signaling::handler::websocket_handler;
use social::social_routes;

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub jwt: JwtConfig,
    pub room_repo: Arc<dyn RoomRepository>,
    pub metrics_handle: PrometheusHandle,
    pub enforce_room_access: bool,
}

impl axum::extract::FromRef<AppState> for Arc<dyn RoomRepository> {
    fn from_ref(state: &AppState) -> Self {
        Arc::clone(&state.room_repo)
    }
}

pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/health", get(health_check))
        .route("/ws", get(websocket_handler))
        .route("/metrics", get(metrics_handler))
        .nest("/auth", auth_routes())
        .nest("/social", social_routes())
        .layer(cors)
        .with_state(state)
}

async fn health_check() -> &'static str {
    "OK"
}

async fn metrics_handler(axum::extract::State(state): axum::extract::State<AppState>) -> String {
    state.metrics_handle.render()
}
