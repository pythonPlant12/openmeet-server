//! GIF search for the chat `/giphy` command. The server calls Giphy, so the API key stays private, and it
//! serves the GIF images itself, so, as with link previews, users never contact a third party from the chat.
//!
//! Only Giphy's own hosts are contacted: search goes to the fixed API URL, and images are built from a GIF ID
//! that must be alphanumeric, so neither endpoint can be pointed at another address.

use std::{sync::OnceLock, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use reqwest::redirect;
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::{AppState, auth::extract_user_id};

const SEARCH_URL: &str = "https://api.giphy.com/v1/gifs/search";
const MEDIA_URL: &str = "https://media.giphy.com/media";
const FETCH_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_QUERY_CHARACTERS: usize = 100;
const MAX_RESULTS: u8 = 24;
const MAX_ID_LENGTH: usize = 64;
const MAX_SEARCH_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const MAX_CONCURRENT_GIPHY_REQUESTS: usize = 8;

pub fn gif_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(search_gifs))
        .route("/{id}/image", get(get_gif_image))
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<u8>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Gif {
    id: String,
    title: String,
    width: u32,
    height: u32,
}

async fn search_gifs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<Gif>>, (StatusCode, String)> {
    extract_user_id(&state.jwt, &headers)?;
    let api_key = std::env::var("GIPHY_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "GIF search is not configured".to_string(),
        ))?;
    let search: String = query.q.trim().chars().take(MAX_QUERY_CHARACTERS).collect();
    if search.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let limit = query
        .limit
        .unwrap_or(MAX_RESULTS)
        .clamp(1, MAX_RESULTS)
        .to_string();

    let _permit = acquire_giphy_permit()?;
    let response = http_client()
        .get(SEARCH_URL)
        .query(&[
            ("api_key", api_key.trim()),
            ("q", search.as_str()),
            ("limit", limit.as_str()),
            ("rating", "g"),
        ])
        .send()
        .await
        .map_err(|error| {
            log_request_failure("search", &error);
            bad_gateway()
        })?;
    if !response.status().is_success() {
        tracing::warn!("[gifs] search returned {}", response.status());
        return Err(upstream_status_error(
            response.status(),
            "GIF search is unavailable",
        ));
    }
    let body = read_limited(response, MAX_SEARCH_BYTES)
        .await
        .ok_or_else(bad_gateway)?;
    Ok(Json(parse_search(&body).ok_or_else(bad_gateway)?))
}

#[derive(Debug, Deserialize)]
struct ImageQuery {
    /// `preview` for the picker's small rendition; anything else serves the full GIF.
    size: Option<String>,
}

async fn get_gif_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ImageQuery>,
) -> Result<Response, (StatusCode, String)> {
    extract_user_id(&state.jwt, &headers)?;
    if !is_gif_id(&id) {
        return Err((StatusCode::BAD_REQUEST, "Unsupported GIF".to_string()));
    }
    let file = if query.size.as_deref() == Some("preview") {
        "200w.webp"
    } else {
        "giphy.webp"
    };
    let _permit = acquire_giphy_permit()?;
    let response = http_client()
        .get(format!("{MEDIA_URL}/{id}/{file}"))
        .send()
        .await
        .map_err(|error| {
            log_request_failure("image", &error);
            bad_gateway()
        })?;
    if !response.status().is_success() {
        tracing::warn!("[gifs] image returned {}", response.status());
        return Err(upstream_status_error(response.status(), "GIF not found"));
    }
    let bytes = read_limited(response, MAX_IMAGE_BYTES)
        .await
        .ok_or_else(bad_gateway)?;

    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/webp")),
            // Giphy media never changes for an ID.
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400, immutable"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        bytes,
    )
        .into_response())
}

fn bad_gateway() -> (StatusCode, String) {
    (
        StatusCode::BAD_GATEWAY,
        "GIF search is unavailable".to_string(),
    )
}

fn service_unavailable() -> (StatusCode, String) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "GIF search is unavailable".to_string(),
    )
}

fn upstream_status_error(status: StatusCode, message: &str) -> (StatusCode, String) {
    let status = match status {
        StatusCode::NOT_FOUND => StatusCode::NOT_FOUND,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::BAD_GATEWAY,
    };
    (status, message.to_string())
}

fn acquire_giphy_permit() -> Result<SemaphorePermit<'static>, (StatusCode, String)> {
    giphy_semaphore()
        .try_acquire()
        .map_err(|_| service_unavailable())
}

fn giphy_semaphore() -> &'static Semaphore {
    static SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();
    SEMAPHORE.get_or_init(|| Semaphore::new(MAX_CONCURRENT_GIPHY_REQUESTS))
}

fn log_request_failure(operation: &str, error: &reqwest::Error) {
    // reqwest's Display includes the request URL, which has Giphy credentials on search requests.
    tracing::warn!(
        "[gifs] {operation} request failed (timeout={}, connect={})",
        error.is_timeout(),
        error.is_connect(),
    );
}

/// Giphy IDs are short alphanumeric strings. Anything else is rejected before a URL is built from it.
fn is_gif_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LENGTH
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

#[derive(Debug, Deserialize)]
struct GiphySearch {
    data: Vec<GiphyGif>,
}

#[derive(Debug, Deserialize)]
struct GiphyGif {
    id: String,
    #[serde(default)]
    title: String,
    images: GiphyImages,
}

#[derive(Debug, Deserialize)]
struct GiphyImages {
    fixed_width: GiphyRendition,
}

/// Giphy sends dimensions as strings.
#[derive(Debug, Deserialize)]
struct GiphyRendition {
    width: String,
    height: String,
}

fn parse_search(body: &[u8]) -> Option<Vec<Gif>> {
    let search: GiphySearch = serde_json::from_slice(body).ok()?;
    Some(
        search
            .data
            .into_iter()
            .filter(|gif| is_gif_id(&gif.id))
            .filter_map(|gif| {
                Some(Gif {
                    width: gif.images.fixed_width.width.parse().ok()?,
                    height: gif.images.fixed_width.height.parse().ok()?,
                    title: gif.title.trim().to_string(),
                    id: gif.id,
                })
            })
            .collect(),
    )
}

async fn read_limited(mut response: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > limit {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // Giphy's CDN may redirect between its own hosts; nothing else is followed.
            .redirect(redirect::Policy::custom(|attempt| {
                let on_giphy = attempt.url().scheme() == "https"
                    && attempt
                        .url()
                        .host_str()
                        .is_some_and(|host| host == "giphy.com" || host.ends_with(".giphy.com"));
                if attempt.previous().len() >= MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if on_giphy {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .connect_timeout(FETCH_TIMEOUT)
            .timeout(FETCH_TIMEOUT)
            .user_agent("OpenMeetGifSearch/1.0 (+https://openmeets.eu)")
            .build()
            .expect("GIF HTTP client configuration is valid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_alphanumeric_gif_ids() {
        assert!(is_gif_id("xT9IgG50Fb7Mi0prBC"));
        assert!(!is_gif_id(""));
        assert!(!is_gif_id("../secret"));
        assert!(!is_gif_id("abc/def"));
        assert!(!is_gif_id("abc?x=1"));
        assert!(!is_gif_id(&"a".repeat(MAX_ID_LENGTH + 1)));
    }

    #[test]
    fn parses_search_results_and_skips_unusable_entries() {
        let body = br#"{
            "data": [
                { "id": "abc123", "title": " Happy Cat ", "images": { "fixed_width": { "width": "200", "height": "150" } } },
                { "id": "bad/id", "title": "x", "images": { "fixed_width": { "width": "200", "height": "150" } } },
                { "id": "nosize", "title": "x", "images": { "fixed_width": { "width": "", "height": "150" } } }
            ],
            "pagination": { "count": 3 }
        }"#;
        assert_eq!(
            parse_search(body),
            Some(vec![Gif {
                id: "abc123".to_string(),
                title: "Happy Cat".to_string(),
                width: 200,
                height: 150,
            }])
        );
    }

    #[test]
    fn rejects_malformed_search_responses() {
        assert_eq!(parse_search(b"not json"), None);
        assert_eq!(parse_search(br#"{ "meta": {} }"#), None);
    }

    #[test]
    fn maps_upstream_statuses_without_hiding_not_found_or_rate_limits() {
        assert_eq!(
            upstream_status_error(StatusCode::NOT_FOUND, "test").0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            upstream_status_error(StatusCode::TOO_MANY_REQUESTS, "test").0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            upstream_status_error(StatusCode::SERVICE_UNAVAILABLE, "test").0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            upstream_status_error(StatusCode::INTERNAL_SERVER_ERROR, "test").0,
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn limits_global_giphy_work() {
        let permits: Vec<_> = (0..MAX_CONCURRENT_GIPHY_REQUESTS)
            .map(|_| giphy_semaphore().try_acquire().unwrap())
            .collect();

        assert!(giphy_semaphore().try_acquire().is_err());
        drop(permits);
    }
}
