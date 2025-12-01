use argon2::{password_hash::SaltString, Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::{
    extract::State,
    http::{header, StatusCode},
    Json,
};
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use rand::rngs::OsRng;

use crate::auth::{
    jwt::JwtConfig,
    models::{
        AuthResponse, LoginRequest, NewRefreshToken, NewUser, RefreshRequest, RegisterRequest,
        TokenResponse, User, UserResponse,
    },
};
use crate::schema::{refresh_tokens, users};
use crate::AppState;

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

// POST /auth/register
pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<AuthResponse> {
    let mut conn = state
        .pool
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Hash password with Argon2
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .to_string();

    // Insert user
    let new_user = NewUser {
        email: req.email.clone(),
        name: req.name,
        password_hash,
        role: "user".to_string(),
    };

    let user: User = diesel::insert_into(users::table)
        .values(&new_user)
        .returning(User::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|e| match e {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => (StatusCode::CONFLICT, "Email already exists".to_string()),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        })?;

    // Create tokens
    let (access_token, refresh_token) = create_tokens(&state.jwt, &user, &mut conn).await?;

    Ok(Json(AuthResponse {
        user: user.into(),
        access_token,
        refresh_token,
    }))
}

// POST /auth/login
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<AuthResponse> {
    let mut conn = state
        .pool
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Find user by email
    let user: User = users::table
        .filter(users::email.eq(&req.email))
        .select(User::as_select())
        .first(&mut conn)
        .await
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid credentials".to_string()))?;

    // Verify password
    let parsed_hash = PasswordHash::new(&user.password_hash)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed_hash)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid credentials".to_string()))?;

    // Create tokens
    let (access_token, refresh_token) = create_tokens(&state.jwt, &user, &mut conn).await?;

    Ok(Json(AuthResponse {
        user: user.into(),
        access_token,
        refresh_token,
    }))
}

// POST /auth/refresh
pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> ApiResult<TokenResponse> {
    let mut conn = state
        .pool
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let token_hash = JwtConfig::hash_refresh_token(&req.refresh_token);

    // Find and validate refresh token
    let (token_id, user_id, expires_at): (uuid::Uuid, uuid::Uuid, chrono::NaiveDateTime) =
        refresh_tokens::table
            .filter(refresh_tokens::token_hash.eq(&token_hash))
            .select((
                refresh_tokens::id,
                refresh_tokens::user_id,
                refresh_tokens::expires_at,
            ))
            .first(&mut conn)
            .await
            .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid refresh token".to_string()))?;

    // Check expiration
    if expires_at < Utc::now().naive_utc() {
        // Delete expired token
        diesel::delete(refresh_tokens::table.filter(refresh_tokens::id.eq(token_id)))
            .execute(&mut conn)
            .await
            .ok();
        return Err((StatusCode::UNAUTHORIZED, "Refresh token expired".to_string()));
    }

    // Create new access token
    let access_token = state
        .jwt
        .create_access_token(user_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(TokenResponse { access_token }))
}

// POST /auth/logout
pub async fn logout(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut conn = state
        .pool
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let token_hash = JwtConfig::hash_refresh_token(&req.refresh_token);

    // Delete refresh token (invalidates the session)
    diesel::delete(refresh_tokens::table.filter(refresh_tokens::token_hash.eq(&token_hash)))
        .execute(&mut conn)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

// GET /auth/me - requires Authorization header
pub async fn me(
    State(state): State<AppState>,
    headers: header::HeaderMap,
) -> ApiResult<UserResponse> {
    let user_id = extract_user_id(&state.jwt, &headers)?;

    let mut conn = state
        .pool
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let user: User = users::table
        .filter(users::id.eq(user_id))
        .select(User::as_select())
        .first(&mut conn)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "User not found".to_string()))?;

    Ok(Json(user.into()))
}

// Helper: create both tokens and store refresh token in DB
async fn create_tokens(
    jwt: &JwtConfig,
    user: &User,
    conn: &mut diesel_async::AsyncPgConnection,
) -> Result<(String, String), (StatusCode, String)> {
    let access_token = jwt
        .create_access_token(user.id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let refresh_token = JwtConfig::create_refresh_token();
    let token_hash = JwtConfig::hash_refresh_token(&refresh_token);

    let new_refresh_token = NewRefreshToken {
        user_id: user.id,
        token_hash,
        expires_at: jwt.refresh_token_expires_at(),
    };

    diesel::insert_into(refresh_tokens::table)
        .values(&new_refresh_token)
        .execute(conn)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok((access_token, refresh_token))
}

// Helper: extract user ID from Authorization header
fn extract_user_id(
    jwt: &JwtConfig,
    headers: &header::HeaderMap,
) -> Result<uuid::Uuid, (StatusCode, String)> {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "Missing authorization header".to_string()))?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or((StatusCode::UNAUTHORIZED, "Invalid authorization format".to_string()))?;

    jwt.validate_access_token(token)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid or expired token".to_string()))
}
