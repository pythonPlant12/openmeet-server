use axum::{
    routing::{get, post},
    Router,
};

use crate::auth::{login, logout, me, refresh, register};
use crate::AppState;

pub fn auth_routes() -> Router<AppState> {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/refresh", post(refresh))
        .route("/logout", post(logout))
        .route("/me", get(me))
}
