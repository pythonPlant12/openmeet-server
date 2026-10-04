//! Avatar storage with thumbnails. Lists render avatars at about 48 CSS pixels, so every stored
//! avatar has a small square thumbnail next to the original, and full images load only on demand.
//! The thumbnail key is derived from the original key, so no extra database column is needed.

use std::io::Cursor;

use axum::{
    body::Body,
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use image::{ImageFormat, ImageReader, Limits, imageops::FilterType};
use serde::Deserialize;

use crate::{
    social::{handlers::storage_error, has_matching_if_none_match},
    storage::AvatarStorage,
};

/// Covers 48px list avatars on high-density screens and the 112px account tile.
const THUMBNAIL_SIZE: u32 = 128;
const MAX_SOURCE_DIMENSION: u32 = 8_192;
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const THUMBNAIL_JPEG_QUALITY: u8 = 85;
const CACHE_CONTROL: &str = "private, no-cache, max-age=0, must-revalidate";

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AvatarSize {
    Thumb,
    #[default]
    Full,
}

#[derive(Debug, Default, Deserialize)]
pub struct AvatarQuery {
    #[serde(default)]
    pub size: AvatarSize,
}

pub struct Thumbnail {
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

/// Builds a user avatar URL whose revision changes with every upload, because each upload gets a
/// new random file name. Clients can then cache avatars by URL without showing stale pictures.
pub fn user_avatar_url(user_id: uuid::Uuid, avatar_key: &str) -> String {
    let revision = avatar_key
        .rsplit('/')
        .next()
        .and_then(|file_name| file_name.split('.').next())
        .unwrap_or(avatar_key);
    format!("/social/users/{user_id}/avatar?v={revision}")
}

pub fn thumbnail_key(avatar_key: &str) -> String {
    format!("{avatar_key}.thumb")
}

/// Decodes the upload and renders a center-cropped square thumbnail. Opaque images become JPEG;
/// images with transparency become PNG so the transparent areas survive.
pub fn render_thumbnail(bytes: &[u8]) -> anyhow::Result<Thumbnail> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);

    let thumbnail =
        reader
            .decode()?
            .resize_to_fill(THUMBNAIL_SIZE, THUMBNAIL_SIZE, FilterType::Lanczos3);
    let mut output = Cursor::new(Vec::new());
    let content_type = if thumbnail.color().has_alpha() {
        thumbnail
            .to_rgba8()
            .write_to(&mut output, ImageFormat::Png)?;
        "image/png"
    } else {
        let encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, THUMBNAIL_JPEG_QUALITY);
        thumbnail.to_rgb8().write_with_encoder(encoder)?;
        "image/jpeg"
    };
    Ok(Thumbnail {
        content_type,
        bytes: output.into_inner(),
    })
}

/// Stores an uploaded avatar and its thumbnail. Undecodable images are rejected before anything
/// is written, and a failed thumbnail write removes the original so no half-stored avatar remains.
pub async fn store_avatar(
    storage: &dyn AvatarStorage,
    key: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> Result<(), (StatusCode, String)> {
    let source = bytes.clone();
    let thumbnail = tokio::task::spawn_blocking(move || render_thumbnail(&source))
        .await
        .map_err(|error| storage_error(error.into()))?
        .map_err(|error| {
            tracing::warn!(%error, "Rejected undecodable avatar upload");
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Avatar image could not be read".to_string(),
            )
        })?;

    storage
        .upload(key, content_type, bytes)
        .await
        .map_err(storage_error)?;
    if let Err(error) = storage
        .upload(&thumbnail_key(key), thumbnail.content_type, thumbnail.bytes)
        .await
    {
        if let Err(delete_error) = storage.delete(key).await {
            tracing::error!(%delete_error, avatar_key = %key, "Failed to remove avatar without thumbnail");
        }
        return Err(storage_error(error));
    }
    Ok(())
}

/// Removes an avatar and its thumbnail. Both deletes run so one failure does not leak the other.
pub async fn delete_avatar(storage: &dyn AvatarStorage, key: &str) -> anyhow::Result<()> {
    let original = storage.delete(key).await;
    let thumbnail = storage.delete(&thumbnail_key(key)).await;
    original.and(thumbnail)
}

/// Serves an avatar in the requested size with an ETag. Avatars stored before thumbnails existed
/// get their thumbnail rendered and saved on first request.
pub async fn avatar_response(
    storage: &dyn AvatarStorage,
    headers: &HeaderMap,
    avatar_key: &str,
    size: AvatarSize,
) -> Result<Response, (StatusCode, String)> {
    let etag = match size {
        AvatarSize::Full => format!("\"{avatar_key}\""),
        AvatarSize::Thumb => format!("\"{}\"", thumbnail_key(avatar_key)),
    };
    if has_matching_if_none_match(headers, &etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, CACHE_CONTROL)
            .body(Body::empty())
            .map_err(internal_error);
    }

    let (content_type, bytes) = match size {
        AvatarSize::Full => {
            let avatar = storage.download(avatar_key).await.map_err(storage_error)?;
            (avatar.content_type, avatar.bytes)
        }
        AvatarSize::Thumb => match storage.download(&thumbnail_key(avatar_key)).await {
            Ok(thumbnail) => (thumbnail.content_type, thumbnail.bytes),
            Err(_) => backfill_thumbnail(storage, avatar_key).await?,
        },
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, CACHE_CONTROL)
        .body(Body::from(bytes))
        .map_err(internal_error)
}

async fn backfill_thumbnail(
    storage: &dyn AvatarStorage,
    avatar_key: &str,
) -> Result<(String, Vec<u8>), (StatusCode, String)> {
    let original = storage.download(avatar_key).await.map_err(storage_error)?;
    let source = original.bytes.clone();
    let rendered = tokio::task::spawn_blocking(move || render_thumbnail(&source))
        .await
        .map_err(|error| storage_error(error.into()))?;
    let thumbnail = match rendered {
        Ok(thumbnail) => thumbnail,
        Err(error) => {
            // An unreadable legacy image still has its original, which the browser may display.
            tracing::warn!(%error, %avatar_key, "Serving original avatar because its thumbnail could not be rendered");
            return Ok((original.content_type, original.bytes));
        }
    };
    if let Err(error) = storage
        .upload(
            &thumbnail_key(avatar_key),
            thumbnail.content_type,
            thumbnail.bytes.clone(),
        )
        .await
    {
        tracing::warn!(%error, %avatar_key, "Failed to save backfilled avatar thumbnail");
    }
    Ok((thumbnail.content_type.to_string(), thumbnail.bytes))
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!("Avatar response error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Request failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    use super::{THUMBNAIL_SIZE, render_thumbnail, thumbnail_key};

    fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn renders_square_jpeg_thumbnails_for_opaque_images() {
        let source = encode(
            DynamicImage::ImageRgb8(RgbImage::from_pixel(640, 320, Rgb([11, 122, 117]))),
            ImageFormat::Png,
        );

        let thumbnail = render_thumbnail(&source).unwrap();
        let decoded = image::load_from_memory(&thumbnail.bytes).unwrap();

        assert_eq!(thumbnail.content_type, "image/jpeg");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (THUMBNAIL_SIZE, THUMBNAIL_SIZE)
        );
    }

    #[test]
    fn keeps_transparency_as_png() {
        let source = encode(
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(200, 200, Rgba([0, 0, 0, 0]))),
            ImageFormat::Png,
        );

        assert_eq!(render_thumbnail(&source).unwrap().content_type, "image/png");
    }

    #[test]
    fn rejects_bytes_that_are_not_an_image() {
        assert!(render_thumbnail(b"\x89PNG\r\n\x1a\nnot really a png").is_err());
    }

    #[test]
    fn user_avatar_urls_change_with_each_upload() {
        let user_id = uuid::Uuid::nil();
        assert_eq!(
            super::user_avatar_url(user_id, "avatars/user/0a1b.png"),
            format!("/social/users/{user_id}/avatar?v=0a1b")
        );
        assert_ne!(
            super::user_avatar_url(user_id, "avatars/user/first.png"),
            super::user_avatar_url(user_id, "avatars/user/second.png")
        );
    }

    #[test]
    fn derives_thumbnail_keys_from_avatar_keys() {
        assert_eq!(
            thumbnail_key("avatars/user/one.png"),
            "avatars/user/one.png.thumb"
        );
    }
}
