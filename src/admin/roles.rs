use std::collections::{BTreeSet, HashMap};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use diesel::{dsl::count_star, prelude::*};
use diesel_async::{
    AsyncConnection, AsyncPgConnection, RunQueryDsl, scoped_futures::ScopedFutureExt,
};
use serde::{Deserialize, Serialize};

use super::{ApiResult, authorize, forbidden_message, internal_error};
use crate::{
    AppState,
    auth::Permission,
    schema::{permissions, role_permissions, roles, users},
};

/// The admin role keeps every permission, so the panel always has someone who can manage it.
const ADMIN_ROLE: &str = "admin";
const MAX_DESCRIPTION_LENGTH: usize = 255;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PermissionResponse {
    key: String,
    description: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RoleResponse {
    name: String,
    description: String,
    is_system: bool,
    /// The admin role's permissions cannot change.
    is_locked: bool,
    permissions: Vec<String>,
    user_count: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateRoleRequest {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    permissions: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateRoleRequest {
    description: Option<String>,
    permissions: Option<Vec<String>>,
}

// GET /admin/permissions
pub(super) async fn list_permissions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<PermissionResponse>> {
    let (_, mut conn) = authorize(&state, &headers, None).await?;
    let rows: Vec<(String, String)> = permissions::table
        .select((permissions::key, permissions::description))
        .load(&mut conn)
        .await
        .map_err(internal_error)?;
    let mut descriptions: HashMap<String, String> = rows.into_iter().collect();

    // Keep the catalog order from the code: app permissions first, then admin permissions.
    Ok(Json(
        Permission::ALL
            .into_iter()
            .filter_map(|permission| {
                descriptions
                    .remove(permission.key())
                    .map(|description| PermissionResponse {
                        key: permission.key().to_string(),
                        description,
                    })
            })
            .collect(),
    ))
}

// GET /admin/roles
pub(super) async fn list_roles(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<RoleResponse>> {
    let (_, mut conn) = authorize(&state, &headers, None).await?;
    load_roles(&mut conn, None).await.map(Json)
}

// POST /admin/roles
pub(super) async fn create_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateRoleRequest>,
) -> Result<(StatusCode, Json<RoleResponse>), (StatusCode, String)> {
    let (admin, mut conn) = authorize(&state, &headers, Some(Permission::AdminRolesManage)).await?;
    let name = validate_role_name(&request.name)?;
    let description = validate_description(&request.description)?;
    let permissions = validate_permissions(&request.permissions)?;
    if !admin.holds_all(&permissions) {
        return Err((StatusCode::FORBIDDEN, forbidden_message().to_string()));
    }

    let role_name = name.clone();
    conn.transaction(|conn| {
        async move {
            diesel::insert_into(roles::table)
                .values((
                    roles::name.eq(&role_name),
                    roles::description.eq(&description),
                ))
                .execute(conn)
                .await?;
            replace_permissions(conn, &role_name, &permissions).await
        }
        .scope_boxed()
    })
    .await
    .map_err(|error| match error {
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => (
            StatusCode::CONFLICT,
            "A role with this name already exists".to_string(),
        ),
        error => internal_error(error),
    })?;
    tracing::info!(admin_id = %admin.id, role = %name, "Admin created role");

    let role = load_role(&mut conn, &name).await?;
    Ok((StatusCode::CREATED, Json(role)))
}

// PATCH /admin/roles/{name}
pub(super) async fn update_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<UpdateRoleRequest>,
) -> ApiResult<RoleResponse> {
    let (admin, mut conn) = authorize(&state, &headers, Some(Permission::AdminRolesManage)).await?;
    ensure_role_exists(&mut conn, &name).await?;
    let description = request
        .description
        .as_deref()
        .map(validate_description)
        .transpose()?;
    let permissions = request
        .permissions
        .as_deref()
        .map(validate_permissions)
        .transpose()?;

    if let Some(permissions) = &permissions {
        if name == ADMIN_ROLE {
            return Err((
                StatusCode::BAD_REQUEST,
                "The admin role always has every permission".to_string(),
            ));
        }
        // Editing your own role could remove your own access to the panel.
        if name == admin.role {
            return Err((
                StatusCode::BAD_REQUEST,
                "You cannot change the permissions of your own role".to_string(),
            ));
        }
        let current = super::role_permission_keys(&mut conn, &name)
            .await
            .map_err(internal_error)?;
        if !admin.holds_all(permissions) || !admin.holds_all(&current) {
            return Err((StatusCode::FORBIDDEN, forbidden_message().to_string()));
        }
    }

    let role_name = name.clone();
    conn.transaction(|conn| {
        async move {
            if let Some(description) = &description {
                diesel::update(roles::table.find(&role_name))
                    .set(roles::description.eq(description))
                    .execute(conn)
                    .await?;
            }
            if let Some(permissions) = &permissions {
                replace_permissions(conn, &role_name, permissions).await?;
            }
            diesel::update(roles::table.find(&role_name))
                .set(roles::updated_at.eq(Utc::now()))
                .execute(conn)
                .await
                .map(|_| ())
        }
        .scope_boxed()
    })
    .await
    .map_err(internal_error)?;
    tracing::info!(admin_id = %admin.id, role = %name, "Admin updated role");

    load_role(&mut conn, &name).await.map(Json)
}

// DELETE /admin/roles/{name}
pub(super) async fn delete_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let (admin, mut conn) = authorize(&state, &headers, Some(Permission::AdminRolesManage)).await?;
    let is_system: bool = roles::table
        .find(&name)
        .select(roles::is_system)
        .first(&mut conn)
        .await
        .optional()
        .map_err(internal_error)?
        .ok_or(role_not_found())?;
    if is_system {
        return Err((
            StatusCode::BAD_REQUEST,
            "System roles cannot be deleted".to_string(),
        ));
    }
    let current = super::role_permission_keys(&mut conn, &name)
        .await
        .map_err(internal_error)?;
    if !admin.holds_all(&current) {
        return Err((StatusCode::FORBIDDEN, forbidden_message().to_string()));
    }

    let assigned: i64 = users::table
        .filter(users::role.eq(&name))
        .select(count_star())
        .get_result(&mut conn)
        .await
        .map_err(internal_error)?;
    if assigned > 0 {
        return Err((
            StatusCode::CONFLICT,
            format!("Move the {assigned} users with this role to another role first"),
        ));
    }

    // users.role restricts deletes, so a user assigned after the count above makes this fail safely.
    diesel::delete(roles::table.find(&name))
        .execute(&mut conn)
        .await
        .map_err(|error| match error {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::ForeignKeyViolation,
                _,
            ) => (
                StatusCode::CONFLICT,
                "Users still have this role".to_string(),
            ),
            error => internal_error(error),
        })?;
    tracing::info!(admin_id = %admin.id, role = %name, "Admin deleted role");

    Ok(StatusCode::NO_CONTENT)
}

async fn replace_permissions(
    conn: &mut AsyncPgConnection,
    role: &str,
    permissions: &BTreeSet<String>,
) -> QueryResult<()> {
    diesel::delete(role_permissions::table.filter(role_permissions::role_name.eq(role)))
        .execute(conn)
        .await?;
    let rows: Vec<_> = permissions
        .iter()
        .map(|key| {
            (
                role_permissions::role_name.eq(role),
                role_permissions::permission_key.eq(key),
            )
        })
        .collect();
    if !rows.is_empty() {
        diesel::insert_into(role_permissions::table)
            .values(&rows)
            .execute(conn)
            .await?;
    }
    Ok(())
}

async fn ensure_role_exists(
    conn: &mut AsyncPgConnection,
    name: &str,
) -> Result<(), (StatusCode, String)> {
    let exists: bool = diesel::select(diesel::dsl::exists(roles::table.find(name)))
        .get_result(conn)
        .await
        .map_err(internal_error)?;
    if exists {
        Ok(())
    } else {
        Err(role_not_found())
    }
}

async fn load_role(
    conn: &mut AsyncPgConnection,
    name: &str,
) -> Result<RoleResponse, (StatusCode, String)> {
    load_roles(conn, Some(name))
        .await?
        .pop()
        .ok_or(role_not_found())
}

async fn load_roles(
    conn: &mut AsyncPgConnection,
    only: Option<&str>,
) -> Result<Vec<RoleResponse>, (StatusCode, String)> {
    let mut statement = roles::table
        .select((
            roles::name,
            roles::description,
            roles::is_system,
            roles::created_at,
            roles::updated_at,
        ))
        .order((roles::is_system.desc(), roles::name.asc()))
        .into_boxed();
    if let Some(name) = only {
        statement = statement.filter(roles::name.eq(name));
    }
    let rows: Vec<(String, String, bool, DateTime<Utc>, DateTime<Utc>)> =
        statement.load(conn).await.map_err(internal_error)?;
    let names: Vec<&String> = rows.iter().map(|(name, ..)| name).collect();

    let mut permissions: HashMap<String, Vec<String>> = HashMap::new();
    let permission_rows: Vec<(String, String)> = role_permissions::table
        .filter(role_permissions::role_name.eq_any(&names))
        .select((
            role_permissions::role_name,
            role_permissions::permission_key,
        ))
        .order(role_permissions::permission_key.asc())
        .load(conn)
        .await
        .map_err(internal_error)?;
    for (role, key) in permission_rows {
        permissions.entry(role).or_default().push(key);
    }
    let user_counts: HashMap<String, i64> = users::table
        .filter(users::role.eq_any(&names))
        .group_by(users::role)
        .select((users::role, count_star()))
        .load::<(String, i64)>(conn)
        .await
        .map_err(internal_error)?
        .into_iter()
        .collect();

    Ok(rows
        .into_iter()
        .map(
            |(name, description, is_system, created_at, updated_at)| RoleResponse {
                is_locked: name == ADMIN_ROLE,
                permissions: permissions.remove(&name).unwrap_or_default(),
                user_count: user_counts.get(&name).copied().unwrap_or(0),
                name,
                description,
                is_system,
                created_at,
                updated_at,
            },
        )
        .collect())
}

fn validate_role_name(name: &str) -> Result<String, (StatusCode, String)> {
    let name = name.trim().to_lowercase();
    let mut characters = name.chars();
    let valid = (2..=50).contains(&name.len())
        && characters
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '-'
        });
    if valid {
        Ok(name)
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "Role names are 2 to 50 lowercase letters, numbers, hyphens, or underscores and start with a letter"
                .to_string(),
        ))
    }
}

fn validate_description(description: &str) -> Result<String, (StatusCode, String)> {
    let description = description.trim();
    if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Descriptions are at most {MAX_DESCRIPTION_LENGTH} characters"),
        ));
    }
    Ok(description.to_string())
}

fn validate_permissions(keys: &[String]) -> Result<BTreeSet<String>, (StatusCode, String)> {
    keys.iter()
        .map(|key| {
            Permission::from_key(key)
                .map(|permission| permission.key().to_string())
                .ok_or((
                    StatusCode::BAD_REQUEST,
                    format!("Unknown permission: {key}"),
                ))
        })
        .collect()
}

fn role_not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "Role not found".to_string())
}

#[cfg(test)]
mod tests {
    use super::{validate_permissions, validate_role_name};

    #[test]
    fn role_names_are_normalized_slugs() {
        assert_eq!(validate_role_name(" Moderator ").unwrap(), "moderator");
        assert_eq!(
            validate_role_name("support-tier_2").unwrap(),
            "support-tier_2"
        );
        assert!(validate_role_name("a").is_err());
        assert!(validate_role_name("2fast").is_err());
        assert!(validate_role_name("has space").is_err());
        assert!(validate_role_name(&"a".repeat(51)).is_err());
    }

    #[test]
    fn unknown_permissions_are_rejected_and_duplicates_collapse() {
        let keys = vec!["app.login".to_string(), "app.login".to_string()];
        assert_eq!(validate_permissions(&keys).unwrap().len(), 1);
        assert!(validate_permissions(&["admin.everything".to_string()]).is_err());
    }
}
