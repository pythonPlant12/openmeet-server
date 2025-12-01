use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

// Claims inside access token (JWT, short-lived, stateless)
#[derive(Debug, Serialize, Deserialize)]
pub struct AccessClaims {
    pub sub: String, // user ID
    pub exp: i64,    // expiration
    pub iat: i64,    // issued at
}

#[derive(Clone)]
pub struct JwtConfig {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    pub access_token_minutes: i64,
    pub refresh_token_days: i64,
}

impl JwtConfig {
    pub fn new(secret: &str, access_token_minutes: i64, refresh_token_days: i64) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            access_token_minutes,
            refresh_token_days,
        }
    }

    // Create short-lived JWT access token
    pub fn create_access_token(&self, user_id: Uuid) -> Result<String, jsonwebtoken::errors::Error> {
        let now = Utc::now();
        let claims = AccessClaims {
            sub: user_id.to_string(),
            iat: now.timestamp(),
            exp: (now + Duration::minutes(self.access_token_minutes)).timestamp(),
        };
        encode(&Header::default(), &claims, &self.encoding_key)
    }

    // Validate access token, return user ID
    pub fn validate_access_token(&self, token: &str) -> Result<Uuid, jsonwebtoken::errors::Error> {
        let token_data = decode::<AccessClaims>(token, &self.decoding_key, &Validation::default())?;
        Uuid::parse_str(&token_data.claims.sub)
            .map_err(|_| jsonwebtoken::errors::ErrorKind::InvalidSubject.into())
    }

    // Create random refresh token (NOT a JWT - just random UUID)
    pub fn create_refresh_token() -> String {
        Uuid::new_v4().to_string()
    }

    // Hash refresh token for DB storage (never store raw tokens)
    pub fn hash_refresh_token(token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    // Calculate refresh token expiration
    pub fn refresh_token_expires_at(&self) -> chrono::NaiveDateTime {
        (Utc::now() + Duration::days(self.refresh_token_days)).naive_utc()
    }
}
