//! Admin API for the openmeet-admin panel. Every route needs the `admin.access` permission; routes that
//! change users or roles also need the matching manage permission.

mod accounts;
mod overview;
mod roles;
mod rooms;

use std::collections::HashSet;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, patch},
};
use diesel::prelude::*;
use diesel_async::{
    AsyncPgConnection, RunQueryDsl, pooled_connection::deadpool::Object as PooledConnection,
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    AppState,
    auth::{Permission, extract_user_id, permissions::user_permissions},
    schema::{role_permissions, users},
};

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/overview", get(overview::get_overview))
        .route("/rooms", get(rooms::list_live_rooms))
        .route("/users", get(accounts::list_users))
        .route("/users/{id}", patch(accounts::update_user))
        .route("/permissions", get(roles::list_permissions))
        .route("/roles", get(roles::list_roles).post(roles::create_role))
        .route(
            "/roles/{name}",
            patch(roles::update_role).delete(roles::delete_role),
        )
}

/// The signed-in admin and the permissions of their role.
struct Admin {
    id: Uuid,
    role: String,
    permissions: HashSet<String>,
}

impl Admin {
    fn can(&self, permission: Permission) -> bool {
        self.permissions.contains(permission.key())
    }

    /// An admin may only hand out permissions they hold, so a manage permission cannot escalate itself.
    fn holds_all<'a>(&self, keys: impl IntoIterator<Item = &'a String>) -> bool {
        keys.into_iter().all(|key| self.permissions.contains(key))
    }
}

async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    permission: Option<Permission>,
) -> Result<(Admin, PooledConnection<AsyncPgConnection>), (StatusCode, String)> {
    let user_id = extract_user_id(&state.jwt, headers)?;
    let mut conn = state.pool.get().await.map_err(internal_error)?;
    let role: String = users::table
        .find(user_id)
        .select(users::role)
        .first(&mut conn)
        .await
        .map_err(|_| (StatusCode::UNAUTHORIZED, "User not found".to_string()))?;
    let permissions = user_permissions(&mut conn, user_id)
        .await
        .map_err(internal_error)?
        .into_iter()
        .collect();
    let admin = Admin {
        id: user_id,
        role,
        permissions,
    };

    let required = [Some(Permission::AdminAccess), permission];
    if required.into_iter().flatten().all(|key| admin.can(key)) {
        Ok((admin, conn))
    } else {
        Err((StatusCode::FORBIDDEN, forbidden_message().to_string()))
    }
}

fn forbidden_message() -> &'static str {
    "You are not allowed to do this"
}

async fn role_permission_keys(
    conn: &mut AsyncPgConnection,
    role: &str,
) -> QueryResult<Vec<String>> {
    role_permissions::table
        .filter(role_permissions::role_name.eq(role))
        .select(role_permissions::permission_key)
        .order(role_permissions::permission_key.asc())
        .load(conn)
        .await
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!(%error, "Admin API error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Internal server error".to_string(),
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminMeResponse {
    id: Uuid,
    email: String,
    name: String,
    nickname: String,
    role: String,
    permissions: Vec<String>,
}

// GET /admin/me
async fn me(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<AdminMeResponse> {
    let (admin, mut conn) = authorize(&state, &headers, None).await?;
    let (email, name, nickname): (String, String, String) = users::table
        .find(admin.id)
        .select((users::email, users::name, users::nickname))
        .first(&mut conn)
        .await
        .map_err(internal_error)?;
    let mut permissions: Vec<String> = admin.permissions.into_iter().collect();
    permissions.sort();

    Ok(Json(AdminMeResponse {
        id: admin.id,
        email,
        name,
        nickname,
        role: admin.role,
        permissions,
    }))
}
