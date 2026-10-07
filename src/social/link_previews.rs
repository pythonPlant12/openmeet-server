//! Link previews for chat messages. The server fetches the page, so users never contact third-party sites
//! from the chat and the page's Open Graph image is served through `/image`.
//!
//! Fetching user-supplied URLs is a server-side request forgery risk. Only http(s) URLs on default ports
//! are fetched, every hostname must resolve to public addresses only (checked in the resolver that the
//! connection uses, so DNS rebinding cannot swap in a private address), and redirects are checked again.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use reqwest::{
    Url,
    dns::{Addrs, Name, Resolve, Resolving},
    redirect,
};
use serde::{Deserialize, Serialize};

use crate::{AppState, auth::extract_user_id};

const MAX_URL_BYTES: usize = 2_048;
const FETCH_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_REDIRECTS: usize = 3;
/// Previews only need the document head, so longer pages are cut off rather than rejected.
const MAX_HTML_BYTES: usize = 512 * 1024;
const MAX_IMAGE_BYTES: usize = 3 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const CACHE_CAPACITY: usize = 512;
const MAX_TITLE_CHARACTERS: usize = 200;
const MAX_DESCRIPTION_CHARACTERS: usize = 300;

pub fn link_preview_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(get_link_preview))
        .route("/image", get(get_link_preview_image))
}

#[derive(Debug, Deserialize)]
struct LinkPreviewQuery {
    url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct LinkPreview {
    url: String,
    title: Option<String>,
    description: Option<String>,
    site_name: Option<String>,
    /// Whether `/image?url=` serves an image for this page.
    has_image: bool,
    #[serde(skip)]
    image_url: Option<Url>,
}

async fn get_link_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LinkPreviewQuery>,
) -> Result<Json<LinkPreview>, (StatusCode, String)> {
    extract_user_id(&state.jwt, &headers)?;
    let url = parse_public_url(&query.url)
        .ok_or((StatusCode::BAD_REQUEST, "Unsupported link".to_string()))?;
    preview_for(url)
        .await
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, "No preview available".to_string()))
}

/// Serves the Open Graph image of a previewed page. It takes the page URL, not an image URL, so it
/// cannot be used to fetch arbitrary files.
async fn get_link_preview_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LinkPreviewQuery>,
) -> Result<Response, (StatusCode, String)> {
    extract_user_id(&state.jwt, &headers)?;
    let not_found = || (StatusCode::NOT_FOUND, "No preview image".to_string());
    let url = parse_public_url(&query.url)
        .ok_or((StatusCode::BAD_REQUEST, "Unsupported link".to_string()))?;
    let image_url = preview_for(url)
        .await
        .and_then(|preview| preview.image_url)
        .ok_or_else(not_found)?;
    let (bytes, content_type) = fetch_image(image_url).await.ok_or_else(not_found)?;

    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=3600"),
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

type PreviewCache = Mutex<HashMap<String, (Instant, Option<LinkPreview>)>>;

fn cache() -> &'static PreviewCache {
    static CACHE: OnceLock<PreviewCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Looks the page up in the cache, or fetches it. Failures are cached too, so a broken link in a busy
/// chat is not fetched again for every viewer.
async fn preview_for(url: Url) -> Option<LinkPreview> {
    let key = url.to_string();
    if let Some((fetched_at, preview)) = cache().lock().ok()?.get(&key) {
        if fetched_at.elapsed() < CACHE_TTL {
            return preview.clone();
        }
    }

    let preview = fetch_preview(url).await;
    if let Ok(mut cache) = cache().lock() {
        if cache.len() >= CACHE_CAPACITY {
            cache.retain(|_, (fetched_at, _)| fetched_at.elapsed() < CACHE_TTL);
        }
        if cache.len() >= CACHE_CAPACITY {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (fetched_at, _))| *fetched_at)
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (Instant::now(), preview.clone()));
    }
    preview
}

async fn fetch_preview(url: Url) -> Option<LinkPreview> {
    let response = http_client()
        .get(url)
        .header(header::ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase())
        .is_some_and(|value| value.contains("text/html") || value.contains("xhtml"));
    if !is_html {
        return None;
    }
    // The final URL after redirects is the base for relative image paths.
    let page_url = response.url().clone();
    let body = read_limited(response, MAX_HTML_BYTES, true).await?;
    let preview = parse_preview(&page_url, &String::from_utf8_lossy(&body));
    (preview.title.is_some() || preview.description.is_some()).then_some(preview)
}

async fn fetch_image(url: Url) -> Option<(Vec<u8>, &'static str)> {
    let response = http_client()
        .get(url)
        .header(header::ACCEPT, "image/*")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let bytes = read_limited(response, MAX_IMAGE_BYTES, false).await?;
    // The bytes decide the type, never the remote header, so nothing but these images is served.
    let content_type = match image::guess_format(&bytes).ok()? {
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::WebP => "image/webp",
        image::ImageFormat::Gif => "image/gif",
        _ => return None,
    };
    Some((bytes, content_type))
}

/// Reads at most `limit` bytes. Truncating is fine for HTML heads; images over the limit are rejected.
async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
    truncate: bool,
) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > limit {
            if !truncate {
                return None;
            }
            body.extend_from_slice(&chunk[..limit - body.len()]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .dns_resolver(Arc::new(PublicResolver))
            // A proxy would resolve hostnames itself and bypass the public-address check.
            .no_proxy()
            .redirect(redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if parse_public_url(attempt.url().as_str()).is_none() {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .connect_timeout(FETCH_TIMEOUT)
            .timeout(FETCH_TIMEOUT)
            .user_agent("OpenMeetLinkPreview/1.0 (+https://openmeets.eu)")
            .build()
            .expect("link preview HTTP client configuration is valid")
    })
}

/// Resolves hostnames and refuses any answer that contains a non-public address.
struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses: Vec<SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if addresses.is_empty() || !addresses.iter().all(|address| is_public_ip(address.ip())) {
                return Err("link host does not resolve to a public address".into());
            }
            let addresses: Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

/// Accepts http(s) URLs on default ports whose host is a public name or a public IP address.
fn parse_public_url(raw: &str) -> Option<Url> {
    if raw.len() > MAX_URL_BYTES {
        return None;
    }
    let mut url = Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() {
        return None;
    }
    if url.password().is_some() || url.port().is_some() {
        return None;
    }
    match url.host()? {
        // IP literals skip the resolver, so they are checked here.
        url::Host::Ipv4(ip) => is_public_ip(IpAddr::V4(ip)).then_some(())?,
        url::Host::Ipv6(ip) => is_public_ip(IpAddr::V6(ip)).then_some(())?,
        url::Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            let is_internal_name = !domain.contains('.')
                || ["localhost", "local", "internal", "home.arpa"]
                    .iter()
                    .any(|suffix| domain == *suffix || domain.ends_with(&format!(".{suffix}")));
            if is_internal_name {
                return None;
            }
        }
    }
    url.set_fragment(None);
    Some(url)
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_ipv4(mapped);
            }
            let segments = ip.segments();
            // NAT64 addresses embed an IPv4 address in the last 32 bits.
            if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [high, low] = [segments[6], segments[7]];
                return is_public_ipv4(Ipv4Addr::new(
                    (high >> 8) as u8,
                    high as u8,
                    (low >> 8) as u8,
                    low as u8,
                ));
            }
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00 // unique local
                || (segments[0] & 0xffc0) == 0xfe80 // link local
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)) // documentation
        }
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [first, second, third, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || first == 0
        || (first == 100 && (64..=127).contains(&second)) // carrier-grade NAT
        || (first == 192 && second == 0 && third == 0) // IETF protocol assignments
        || (first == 198 && (18..=19).contains(&second)) // benchmarking
        || first >= 240)
}

/// Reads Open Graph, Twitter card, and standard metadata from the document head.
fn parse_preview(page_url: &Url, html: &str) -> LinkPreview {
    let lower = html.to_ascii_lowercase();
    let head_end = lower.find("</head").unwrap_or(lower.len());
    let (head, lower_head) = (&html[..head_end], &lower[..head_end]);

    let mut meta: HashMap<String, String> = HashMap::new();
    let mut search_from = 0;
    while let Some(offset) = lower_head[search_from..].find("<meta") {
        let start = search_from + offset;
        let Some(end) = lower_head[start..].find('>').map(|end| start + end) else {
            break;
        };
        let attributes = parse_attributes(&head[start + "<meta".len()..end]);
        let key = attributes
            .get("property")
            .or_else(|| attributes.get("name"))
            .map(|key| key.to_ascii_lowercase());
        if let (Some(key), Some(content)) = (key, attributes.get("content")) {
            meta.entry(key).or_insert_with(|| content.clone());
        }
        search_from = end;
    }

    let first = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| meta.get(*key))
            .map(|value| clean_text(value))
            .filter(|value| !value.is_empty())
    };
    let title = first(&["og:title", "twitter:title"]).or_else(|| {
        let start = lower_head.find("<title")?;
        let start = start + lower_head[start..].find('>')? + 1;
        let end = start + lower_head[start..].find("</title")?;
        Some(clean_text(&head[start..end])).filter(|title| !title.is_empty())
    });
    let image_url = first(&[
        "og:image:secure_url",
        "og:image:url",
        "og:image",
        "twitter:image",
        "twitter:image:src",
    ])
    .and_then(|image| page_url.join(&image).ok())
    .and_then(|image| parse_public_url(image.as_str()));

    LinkPreview {
        url: page_url.to_string(),
        title: title.map(|title| truncate(&title, MAX_TITLE_CHARACTERS)),
        description: first(&["og:description", "twitter:description", "description"])
            .map(|description| truncate(&description, MAX_DESCRIPTION_CHARACTERS)),
        site_name: first(&["og:site_name", "application-name"]).or_else(|| {
            page_url
                .host_str()
                .map(|host| host.trim_start_matches("www.").to_string())
        }),
        has_image: image_url.is_some(),
        image_url,
    }
}

fn parse_attributes(tag: &str) -> HashMap<String, String> {
    let mut attributes = HashMap::new();
    let mut rest = tag.trim_start_matches('/');
    loop {
        rest = rest
            .trim_start_matches(|character: char| character.is_whitespace() || character == '/');
        if rest.is_empty() {
            break;
        }
        let name_end = rest
            .find(|character: char| {
                character == '=' || character.is_whitespace() || character == '/'
            })
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_ascii_lowercase();
        rest = rest[name_end..].trim_start();
        let Some(after_equals) = rest.strip_prefix('=') else {
            if !name.is_empty() {
                attributes.insert(name, String::new());
            }
            if name_end == 0 {
                // Skip a character that cannot start a name, so malformed tags cannot loop forever.
                rest = &rest[rest.chars().next().map_or(0, char::len_utf8)..];
            }
            continue;
        };
        let after_equals = after_equals.trim_start();
        let (value, remaining) = match after_equals.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let value_start = &after_equals[1..];
                match value_start.find(quote) {
                    Some(end) => (&value_start[..end], &value_start[end + 1..]),
                    None => (value_start, ""),
                }
            }
            _ => {
                let end = after_equals
                    .find(char::is_whitespace)
                    .unwrap_or(after_equals.len());
                (&after_equals[..end], &after_equals[end..])
            }
        };
        if !name.is_empty() {
            attributes.insert(name, value.to_string());
        }
        rest = remaining;
    }
    attributes
}

/// Decodes the common HTML entities and collapses whitespace.
fn clean_text(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        decoded.push_str(&rest[..start]);
        rest = &rest[start..];
        let entity_end = rest.find(';').filter(|end| *end <= 12);
        let replacement = entity_end.and_then(|end| {
            let entity = &rest[1..end];
            let character = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| {
                        entity
                            .strip_prefix('#')
                            .and_then(|decimal| decimal.parse().ok())
                    })
                    .and_then(char::from_u32),
            }?;
            Some((character, end + 1))
        });
        match replacement {
            Some((character, length)) => {
                decoded.push(character);
                rest = &rest[length..];
            }
            None => {
                decoded.push('&');
                rest = &rest[1..];
            }
        }
    }
    decoded.push_str(rest);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(value: &str, limit: usize) -> String {
    let mut characters = value.chars();
    let mut truncated: String = characters.by_ref().take(limit).collect();
    if characters.next().is_some() {
        truncated.push('…');
    }
    truncated
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use reqwest::Url;

    use super::{clean_text, is_public_ip, parse_preview, parse_public_url};

    #[test]
    fn accepts_only_public_web_urls_on_default_ports() {
        assert!(parse_public_url("https://example.com/page#section").is_some());
        assert!(parse_public_url("http://example.com").is_some());
        for url in [
            "ftp://example.com",
            "https://user:pass@example.com",
            "https://example.com:8443",
            "http://localhost/admin",
            "http://service.internal",
            "http://printer.local",
            "http://intranet/",
            "http://127.0.0.1",
            "http://169.254.169.254/latest/meta-data",
            "http://10.0.0.5",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:192.168.0.1]/",
        ] {
            assert!(parse_public_url(url).is_none(), "{url} must be rejected");
        }
        assert_eq!(
            parse_public_url("https://example.com/a#b")
                .unwrap()
                .as_str(),
            "https://example.com/a"
        );
    }

    #[test]
    fn classifies_private_and_public_addresses() {
        for address in [
            "100.64.0.1",
            "192.0.0.8",
            "198.18.0.1",
            "0.1.2.3",
            "240.0.0.1",
            "fe80::1",
            "64:ff9b::a00:1",
        ] {
            assert!(
                !is_public_ip(address.parse::<IpAddr>().unwrap()),
                "{address}"
            );
        }
        for address in ["8.8.8.8", "2606:4700:4700::1111", "64:ff9b::808:808"] {
            assert!(
                is_public_ip(address.parse::<IpAddr>().unwrap()),
                "{address}"
            );
        }
    }

    #[test]
    fn reads_open_graph_metadata_before_fallbacks() {
        let page = Url::parse("https://www.example.com/articles/1").unwrap();
        let preview = parse_preview(
            &page,
            r#"<html><HEAD><title>Fallback</title>
            <meta property="og:title" content="Tom &amp; Jerry &#x2014; Live">
            <meta name='description' content='Plain description'>
            <meta property=og:image content="/images/cover.png" />
            </head><body><meta property="og:title" content="Body"></body></html>"#,
        );

        assert_eq!(preview.title.as_deref(), Some("Tom & Jerry — Live"));
        assert_eq!(preview.description.as_deref(), Some("Plain description"));
        assert_eq!(preview.site_name.as_deref(), Some("example.com"));
        assert_eq!(
            preview.image_url.as_ref().map(Url::as_str),
            Some("https://www.example.com/images/cover.png")
        );
        assert!(preview.has_image);
    }

    #[test]
    fn falls_back_to_the_title_tag_and_drops_private_images() {
        let page = Url::parse("https://example.com/").unwrap();
        let preview = parse_preview(
            &page,
            "<head><title>\n  Hello   world </title><meta property=\"og:image\" content=\"http://127.0.0.1/x.png\"></head>",
        );

        assert_eq!(preview.title.as_deref(), Some("Hello world"));
        assert!(!preview.has_image);
    }

    #[test]
    fn decodes_entities_without_panicking_on_malformed_input() {
        assert_eq!(
            clean_text("a &lt;b&gt; &#39;c&#39; &unknown; & d"),
            "a <b> 'c' &unknown; & d"
        );
        assert_eq!(clean_text("&#99999999999;"), "&#99999999999;");
    }
}
