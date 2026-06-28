use axum::{
    Router,
    routing::{get, post},
};

use crate::AppState;
use crate::auth::{login, logout, me, refresh, register};

pub fn auth_routes() -> Router<AppState> {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/refresh", post(refresh))
        .route("/logout", post(logout))
        .route("/me", get(me))
}
