mod avatars;
mod call_sessions;
mod conversations;
mod events;
mod group_invitations;
mod handlers;
mod messages;
mod models;
mod notifications;
mod presence;

use axum::http::{HeaderMap, header};

pub use call_sessions::{
    SfuRoomAuthorization, authorize_sfu_room, call_session_routes, start_call_session,
};
pub use conversations::conversation_routes;
pub use events::{SocialEventHub, SocialResource, social_events_handler};
pub use handlers::social_routes;
pub(crate) use notifications::{create_notification, notification_routes};

pub(crate) fn has_matching_if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|value| {
            let value = value.trim();
            value == "*"
                || value
                    .strip_prefix("W/")
                    .map(str::trim_start)
                    .unwrap_or(value)
                    == etag
        })
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::has_matching_if_none_match;

    #[test]
    fn matches_weak_wildcard_and_multiple_etags() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"old\", W/\"current\""),
        );
        assert!(has_matching_if_none_match(&headers, "\"current\""));

        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
        assert!(has_matching_if_none_match(&headers, "\"current\""));
    }
}
