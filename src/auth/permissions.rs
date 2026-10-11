//! Role-based permissions. `users.role` names a row in `roles`, and `role_permissions` lists what that
//! role may do. The permission catalog is fixed here and seeded by the roles migration; the admin panel
//! only changes which roles hold which permissions.

use axum::http::StatusCode;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::schema::{role_permissions, users};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    AppLogin,
    MeetingsCreate,
    MessagesSend,
    AdminAccess,
    AdminUsersManage,
    AdminRolesManage,
}

impl Permission {
    pub const ALL: [Permission; 6] = [
        Permission::AppLogin,
        Permission::MeetingsCreate,
        Permission::MessagesSend,
        Permission::AdminAccess,
        Permission::AdminUsersManage,
        Permission::AdminRolesManage,
    ];

    pub const fn key(self) -> &'static str {
        match self {
            Permission::AppLogin => "app.login",
            Permission::MeetingsCreate => "meetings.create",
            Permission::MessagesSend => "messages.send",
            Permission::AdminAccess => "admin.access",
            Permission::AdminUsersManage => "admin.users.manage",
            Permission::AdminRolesManage => "admin.roles.manage",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|permission| permission.key() == key)
    }

    fn denied_message(self) -> &'static str {
        match self {
            Permission::AppLogin => "This account is suspended",
            Permission::MeetingsCreate => "You are not allowed to create meeting rooms",
            Permission::MessagesSend => "You are not allowed to send messages",
            Permission::AdminAccess
            | Permission::AdminUsersManage
            | Permission::AdminRolesManage => "You are not allowed to do this",
        }
    }
}

pub async fn has_permission(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    permission: Permission,
) -> QueryResult<bool> {
    diesel::select(diesel::dsl::exists(
        users::table
            .inner_join(role_permissions::table.on(role_permissions::role_name.eq(users::role)))
            .filter(users::id.eq(user_id))
            .filter(role_permissions::permission_key.eq(permission.key())),
    ))
    .get_result(conn)
    .await
}

/// Fails with 403 when the user's role does not hold `permission`.
pub async fn require_permission(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    permission: Permission,
) -> Result<(), (StatusCode, String)> {
    let allowed = has_permission(conn, user_id, permission)
        .await
        .map_err(|error| {
            tracing::error!(%error, permission = permission.key(), "Permission check failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error".to_string(),
            )
        })?;
    if allowed {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            permission.denied_message().to_string(),
        ))
    }
}

pub async fn user_permissions(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<Vec<String>> {
    users::table
        .inner_join(role_permissions::table.on(role_permissions::role_name.eq(users::role)))
        .filter(users::id.eq(user_id))
        .select(role_permissions::permission_key)
        .order(role_permissions::permission_key.asc())
        .load(conn)
        .await
}

#[cfg(test)]
mod tests {
    use super::Permission;

    #[test]
    fn permission_keys_round_trip() {
        for permission in Permission::ALL {
            assert_eq!(Permission::from_key(permission.key()), Some(permission));
        }
        assert_eq!(Permission::from_key("admin.everything"), None);
    }

    #[test]
    fn roles_migration_seeds_every_permission() {
        let migration =
            include_str!("../../migrations/20261011000000_create_roles_and_permissions/up.sql");
        for permission in Permission::ALL {
            assert!(
                migration.contains(&format!("('{}',", permission.key())),
                "{} is missing from the roles migration",
                permission.key()
            );
        }
    }
}
