//! Best-effort re-render of an attachment page to a bounded JPEG, used ONLY by
//! the brain tool loop's optional image-passthrough (`ECK_BRAIN_IMAGE_PASSTHROUGH`
//! / the bench `image_passthrough` flag). It is a thin WRAPPER over the PUBLIC
//! decode/encode helpers in `attachments.rs` (`resolve_file`,
//! `decode_image_bytes`, `downscale_jpeg`) — it does NOT duplicate their logic.
//!
//! Scope note: page rasterization is `attachments::decode_page` (raster images
//! incl. AVIF, and scan-PDF pages); files it cannot render return `Ok(None)`
//! here and the caller emits a skip note.

use anyhow::Result;
use eck_core::db::SurrealDb;

/// A page image re-rendered for passthrough: JPEG bytes + display file name.
pub(crate) struct RenderedPage {
    pub jpeg: Vec<u8>,
    pub name: String,
}

/// Resolve a CAS attachment and re-render it as a JPEG no larger than `max_px`
/// on the long edge (quality 80, via the shared `downscale_jpeg`). Returns
/// `Ok(None)` when the attachment has no renderable page (not an image, not a
/// scan PDF with an image XObject) so the caller can skip it.
///
/// `page` selects the PDF page (0-based); ignored for single-image files.
pub(crate) async fn render_attachment_page_jpeg(
    db: &SurrealDb,
    cas_uuid: &str,
    page: u32,
    max_px: u32,
) -> Result<Option<RenderedPage>> {
    let (abs_path, mime, name) = match crate::ai::attachments::resolve_file(db, cas_uuid).await {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let bytes = match tokio::fs::read(&abs_path).await {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    // Any page the attachments decoder can render: raster images (incl. AVIF)
    // and scan-PDF pages (image XObjects / our own one-JPEG-per-page wrap).
    // Decode + downscale on the blocking pool.
    let jpeg = tokio::task::spawn_blocking(move || -> Result<Option<Vec<u8>>> {
        match crate::ai::attachments::decode_page(&bytes, &mime, page)? {
            Some(img) => Ok(Some(crate::ai::attachments::downscale_jpeg(&img, max_px)?)),
            None => Ok(None),
        }
    })
    .await??;
    let Some(jpeg) = jpeg else { return Ok(None) };
    Ok(Some(RenderedPage { jpeg, name }))
}
