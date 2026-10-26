use std::path::{Path, PathBuf};

use hive_client::DaemonClient;
use hive_protocol::chat::ImageAttachment;

use crate::Result;

const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Infer an image media type from magic bytes, falling back to the file
/// extension. The daemon re-validates magic against the declared type.
fn infer_media_type(path: &Path, bytes: &[u8]) -> Result<&'static str> {
    if bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return Ok("image/png");
    }
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Ok("image/jpeg");
    }
    if bytes.len() >= 6 && (bytes[..6] == *b"GIF87a" || bytes[..6] == *b"GIF89a") {
        return Ok("image/gif");
    }
    if bytes.len() >= 12 && bytes[..4] == *b"RIFF" && bytes[8..12] == *b"WEBP" {
        return Ok("image/webp");
    }
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Ok("image/png"),
        Some("jpg" | "jpeg") => Ok("image/jpeg"),
        Some("gif") => Ok("image/gif"),
        Some("webp") => Ok("image/webp"),
        _ => Err("unrecognized image file; expected PNG, JPEG, GIF, or WebP".into()),
    }
}

/// Read, upload, and attach image files. Returns attachments in path order.
pub async fn load_image_attachments(
    client: &DaemonClient,
    paths: &[PathBuf],
) -> Result<Vec<ImageAttachment>> {
    if paths.len() > 16 {
        return Err("at most 16 images may be attached to one message".into());
    }
    let mut attachments = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| format!("cannot read image file {}", path.to_string_lossy()))?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "image file {} is empty or exceeds 20 MiB",
                path.to_string_lossy()
            )
            .into());
        }
        let media_type = infer_media_type(path, &bytes)?;
        let artifact = client.upload_image(&bytes, media_type).await?;
        attachments.push(ImageAttachment {
            artifact_id: artifact.id,
        });
    }
    Ok(attachments)
}

/// Render an image block for human history output.
pub fn format_image_block(media_type: &str, artifact_id: &str) -> String {
    format!(
        "[image {media_type} {}]",
        &artifact_id[..16.min(artifact_id.len())]
    )
}
