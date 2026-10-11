use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, NaiveDateTime, Utc};
use diesel::{dsl::count_star, prelude::*};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ApiResult, authorize, forbidden_message, internal_error, role_permission_keys};
use crate::{
    AppState,
    auth::Permission,
    schema::{refresh_tokens, roles, user_presence, users},
};

const DEFAULT_PAGE_SIZE: i64 = 25;
const MAX_PAGE_SIZE: i64 = 100;
const MAX_SEARCH_LENGTH: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UsersQuery {
    search: Option<String>,
    role: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UsersPage {
    users: Vec<AdminUser>,
    total: i64,
    page: i64,
    page_size: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AdminUser {
    id: Uuid,
    email: String,
    name: String,
    nickname: String,
    role: String,
    status: String,
    /// `users.created_at` is a naive UTC timestamp.
    created_at: NaiveDateTime,
    last_seen_at: Option<DateTime<Utc>>,
}

type AdminUserRow = (
    Uuid,
    String,
    String,
    String,
    String,
    String,
    NaiveDateTime,
    Option<DateTime<Utc>>,
);

impl From<AdminUserRow> for AdminUser {
    fn from(row: AdminUserRow) -> Self {
        let (id, email, name, nickname, role, status, created_at, last_seen_at) = row;
        Self {
            id,
            email,
            name,
            nickname,
            role,
            status,
            created_at,
            last_seen_at,
        }
    }
}

macro_rules! admin_user_columns {
    () => {
        (
            users::id,
            users::email,
            users::name,
            users::nickname,
            users::role,
            users::status,
            users::created_at,
            user_presence::last_seen_at.nullable(),
        )
    };
}

// GET /admin/users
pub(super) async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsersQuery>,
) -> ApiResult<UsersPage> {
    let (_, mut conn) = authorize(&state, &headers, None).await?;
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query
        .page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|search| !search.is_empty())
        .map(|search| {
            let search: String = search.chars().take(MAX_SEARCH_LENGTH).collect();
            format!("%{}%", escape_like(&search))
        });
    let role = query.role.filter(|role| !role.is_empty());

    let filtered = || {
        let mut statement = users::table
            .left_join(user_presence::table.on(user_presence::user_id.eq(users::id)))
            .into_boxed();
        if let Some(pattern) = &search {
            statement = statement.filter(
                users::email
                    .ilike(pattern.clone())
                    .or(users::name.ilike(pattern.clone()))
                    .or(users::nickname.ilike(pattern.clone())),
            );
        }
        if let Some(role) = &role {
            statement = statement.filter(users::role.eq(role.clone()));
        }
        statement
    };

    let total = filtered()
        .select(count_star())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    let users = filtered()
        .select(admin_user_columns!())
        .order((users::created_at.desc(), users::id.asc()))
        .limit(page_size)
        .offset((page - 1) * page_size)
        .load::<AdminUserRow>(&mut conn)
        .await
        .map_err(internal_error)?
        .into_iter()
        .map(AdminUser::from)
        .collect();

    Ok(Json(UsersPage {
        users,
        total,
        page,
        page_size,
    }))
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateUserRequest {
    role: String,
}

// PATCH /admin/users/{id}
pub(super) async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Json(request): Json<UpdateUserRequest>,
) -> ApiResult<AdminUser> {
    let (admin, mut conn) = authorize(&state, &headers, Some(Permission::AdminUsersManage)).await?;
    if admin.id == user_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "You cannot change your own role".to_string(),
        ));
    }

    let current_role: String = users::table
        .find(user_id)
        .select(users::role)
        .first(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "User not found".to_string()))?;
    let role_exists: bool = diesel::select(diesel::dsl::exists(
        roles::table.filter(roles::name.eq(&request.role)),
    ))
    .get_result(&mut conn)
    .await
    .map_err(internal_error)?;
    if !role_exists {
        return Err((StatusCode::BAD_REQUEST, "Role does not exist".to_string()));
    }

    // Only manage users whose current and new roles hold nothing the admin lacks.
    let current_permissions = role_permission_keys(&mut conn, &current_role)
        .await
        .map_err(internal_error)?;
    let new_permissions = role_permission_keys(&mut conn, &request.role)
        .await
        .map_err(internal_error)?;
    if !admin.holds_all(&current_permissions) || !admin.holds_all(&new_permissions) {
        return Err((StatusCode::FORBIDDEN, forbidden_message().to_string()));
    }

    diesel::update(users::table.find(user_id))
        .set((
            users::role.eq(&request.role),
            users::updated_at.eq(Utc::now().naive_utc()),
        ))
        .execute(&mut conn)
        .await
        .map_err(internal_error)?;
    if !new_permissions
        .iter()
        .any(|key| key == Permission::AppLogin.key())
    {
        end_sessions(&mut conn, user_id).await?;
    }
    tracing::info!(admin_id = %admin.id, %user_id, role = %request.role, "Admin changed user role");

    load_user(&mut conn, user_id).await.map(Json)
}

/// Deletes refresh tokens so the user is signed out when the current access token expires.
async fn end_sessions(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    diesel::delete(refresh_tokens::table.filter(refresh_tokens::user_id.eq(user_id)))
        .execute(conn)
        .await
        .map(|_| ())
        .map_err(internal_error)
}

async fn load_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<AdminUser, (StatusCode, String)> {
    users::table
        .left_join(user_presence::table.on(user_presence::user_id.eq(users::id)))
        .filter(users::id.eq(user_id))
        .select(admin_user_columns!())
        .first::<AdminUserRow>(conn)
        .await
        .map(AdminUser::from)
        .map_err(internal_error)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::escape_like;

    #[test]
    fn escapes_like_wildcards() {
        assert_eq!(escape_like(r"50%_off\"), r"50\%\_off\\");
    }
}
