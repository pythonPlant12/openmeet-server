pub mod handlers;
pub mod jwt;
pub mod models;
pub mod permissions;
pub mod routes;

pub use handlers::*;
pub use jwt::JwtConfig;
pub use permissions::{Permission, require_permission};
pub use routes::auth_routes;

/// The placeholder account that keeps a deleted user's group messages and calls. It never signs in, and user
/// lists and lookups leave it out. The account deletion migration creates it.
pub const DELETED_USER_ID: uuid::Uuid = uuid::Uuid::nil();
