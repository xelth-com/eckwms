//! On-demand attachment OCR + policy-gated Gemini vision (ai::attachments).
//!
//! Support-ticket attachments (PDFs / scans / photos) land in the CAS filestore
//! and are linked to a `document` via the `has_attachment` edge. These helpers
//! turn one such file into text (embedded PDF layer → OCR ladder) or, when text
//! is not enough, into a single downscaled image fed to Gemini vision.
//!
//! PPRL boundary: the text summarization pipeline masks PII BEFORE anything
//! leaves the node, but a SCAN is raw pixels — there is no token layer to mask,
//! so a scan sent to Gemini bypasses PPRL entirely. Two consequences are wired
//! in on purpose: (1) [`vision_policy`]'s `strict` mode forbids any attachment
//! image leaving the node at all; (2) the ONLY caller of the vision path is the
//! on-demand agent tool — there is NO background worker that scans attachments,
//! and none may be added (that would silently ship every customer scan to the
//! cloud). Both tools fire exclusively when the agent invokes them.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use image::DynamicImage;
#[allow(unused_imports)]
use image::ImageEncoder;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use serde::Serialize;
use serde_json::{json, Value};
use tracing::warn;

use eck_core::db::SurrealDb;

/// Extractor pipeline revision stamped onto every persisted extract
/// (`file_resource.extractor_ver`). BUMP THIS to re-arm re-extraction fleet-wide:
/// the background sweep re-picks any row whose `extractor_ver` is below this, and
/// [`ocr_one`]'s stored-extract fast path only short-circuits on an exact match.
/// This is the whole re-examination mechanism.
///
/// v2 (2026-07-28): real PDF image-XObject decoding — CCITTFaxDecode (G3/G4)
/// office scans, DCTDecode, and Flate raw bitmaps now feed the OCR ladder where
/// the old JPEG byte-grep saw nothing. Bumping re-arms the sweep so the whole
/// corpus re-reads with the new decoder.
///
/// v3 (2026-09-10): AcroForm field extraction — a fillable PDF form (e.g. the
/// tenant's "Reparaturauftrag") stores the customer's Firma/Straße/PLZ/Ort in the
/// AcroForm field VALUES, which `pdf_extract` never reads (it only walks page
/// content streams, so we saw the blank template). We now walk the AcroForm with
/// lopdf after decryption and APPEND a `--- Formularfelder ---` block of
/// `Feldname: Wert` lines to the extracted text on both the text-layer and
/// scan/OCR paths. Bumping re-arms the sweep so every stored extract is rebuilt
/// with the form fields (no manual backfill).
pub const EXTRACTOR_VER: i64 = 3;

// ── File resolution ─────────────────────────────────────────────────────────

/// Process-wide sync-engine handle, installed once by `main`. Lets this leaf
/// module pull a CAS blob it doesn't hold locally without every caller —
/// the provider-generic orchestrator, each rig `Tool` struct, the MCP surface —
/// having to carry mesh plumbing it otherwise never touches. Unset in tests, in
/// which case the ladder is disk-only exactly as before.
static MESH: std::sync::OnceLock<std::sync::Arc<eck_core::sync::engine::SyncEngine>> =
    std::sync::OnceLock::new();

/// Install the handle used by [`resolve_file`] to hydrate missing blobs.
pub fn install_mesh_hydrator(engine: std::sync::Arc<eck_core::sync::engine::SyncEngine>) {
    let _ = MESH.set(engine);
}

/// The process-wide sync-engine instance id, when installed (see
/// [`install_mesh_hydrator`]). `None` in tests / before install — a caller that
/// needs it to author a mesh write (e.g. `ai::doc_classify::persist_class`) skips
/// the write, exactly as [`persist_extract`] does.
pub(crate) fn mesh_instance_id() -> Option<String> {
    MESH.get().map(|e| e.instance_id().to_string())
}

/// A resolved CAS file: the on-disk path plus the metadata of the row that
/// actually holds the bytes (the tail of any `replaced_by` chain), with the
/// display name carried from the row the caller asked for.
pub struct ResolvedFile {
    pub path: PathBuf,
    pub mime: String,
    pub name: String,
    /// sha256 of the bytes on disk (the final row's hash) — empty if unrecorded.
    pub hash: String,
}

/// Resolve a CAS UUID to its on-disk path, mime type and original name.
/// `storage_path` is relative to the WMS working dir (FileStore::new(".")).
/// Thin tuple view over [`resolve_file_full`]; signature kept for its callers.
pub async fn resolve_file(db: &SurrealDb, cas_uuid: &str) -> Result<(PathBuf, String, String)> {
    let r = resolve_file_full(db, cas_uuid).await?;
    Ok((r.path, r.mime, r.name))
}

/// Resolve a CAS UUID, following the AVIF optimizer's `replaced_by` chain to the
/// row that owns bytes. The optimizer clears an original's `storage_path` and
/// points `replaced_by` at the replacement row (which has a live path), so a
/// bare cas_uuid can land on a byte-less row — this walks up to 4 hops to the
/// row that resolves, and reports against ITS format (mime/hash), but keeps the
/// FIRST row's `original_name` as the display name (the replacement is an
/// internal re-encode, not a rename).
///
/// `file_resource` rows mesh-sync but the BYTES do not, so on any node that
/// didn't author the file the resolving row names a path with nothing behind it.
/// If the blob is missing locally this fetches it once (direct peer, else the
/// sealed copy staged on eckN) into the local CAS. Costs one round trip per file
/// per node the first time.
pub async fn resolve_file_full(db: &SurrealDb, cas_uuid: &str) -> Result<ResolvedFile> {
    let mut id = cas_uuid.to_string();
    let mut display_name = String::new();
    for hop in 0..4 {
        let rows: Vec<Value> = db
            .query(
                "SELECT storage_path, original_name, mime_type, hash, replaced_by \
                 FROM file_resource WHERE cas_uuid = $id LIMIT 1",
            )
            .bind(("id", id.clone()))
            .await?
            .take(0)?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("file_resource or storage_path missing"))?;

        let name = row.get("original_name").and_then(|v| v.as_str()).unwrap_or("");
        if hop == 0 && !name.is_empty() {
            display_name = name.to_string();
        }

        let storage_path = row.get("storage_path").and_then(|v| v.as_str()).unwrap_or("");
        if !storage_path.is_empty() {
            if display_name.is_empty() {
                display_name = name.to_string();
            }
            let abs_path = PathBuf::from(".").join(storage_path);
            let mime = row
                .get("mime_type")
                .and_then(|v| v.as_str())
                .unwrap_or("application/octet-stream")
                .to_string();
            let hash = row.get("hash").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            if !abs_path.exists() {
                hydrate_from_mesh(&hash, storage_path).await;
            }
            return Ok(ResolvedFile { path: abs_path, mime, name: display_name, hash });
        }

        match row
            .get("replaced_by")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(next) => id = next.to_string(),
            None => break,
        }
    }
    Err(anyhow!("file_resource or storage_path missing"))
}

/// Best-effort: pull a missing blob from the mesh into the local CAS. Silent on
/// failure — the caller's own read then fails and reports `not_found` as it did
/// before, so a node with no mesh access degrades instead of erroring.
async fn hydrate_from_mesh(hash: &str, storage_path: &str) {
    if hash.is_empty() {
        return;
    }
    let Some(engine) = MESH.get() else { return };
    let Some(bytes) = engine.fetch_file_from_peers(hash).await else {
        return;
    };
    let store = eck_core::utils::filestore::FileStore::new(".");
    if let Err(e) = store.write_verified(storage_path, &bytes, hash).await {
        warn!("[attachments] mesh hydrate of {} failed: {}", hash, e);
    }
}

// ── PDF handling ────────────────────────────────────────────────────────────

/// Extract the embedded text layer of a PDF, returning `(text, page_count)`.
/// pdf-extract can panic on some malformed font tables, so the parse runs behind
/// `catch_unwind` on the blocking pool — a panic degrades to empty text and is
/// treated as "no text layer", never taking down the calling worker.
async fn pdf_text_layer(bytes: Vec<u8>) -> (String, usize) {
    tokio::task::spawn_blocking(move || {
        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pdf_extract::extract_text_from_mem_by_pages(&bytes)
        }));
        match parsed {
            Ok(Ok(pages)) => {
                let n = pages.len();
                (pages.join("\n\n").trim().to_string(), n)
            }
            _ => (String::new(), 0),
        }
    })
    .await
    .unwrap_or_else(|_| (String::new(), 0))
}

/// Pull embedded JPEG image streams out of a (scan) PDF by walking
/// `stream…endstream` chunks and keeping those whose payload begins with the
/// JPEG SOI marker (FF D8 FF). Our scan-PDF ingest wraps one JPEG per page this
/// way; up to `max` pages are returned in document order. A pure byte scan (no
/// PDF object parsing) — enough for these files and it cannot panic.
fn extract_pdf_jpeg_streams(bytes: &[u8], max: usize) -> Vec<Vec<u8>> {
    const SOI: [u8; 3] = [0xFF, 0xD8, 0xFF];
    let kw = b"stream";
    let end_kw = b"endstream";
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut i = 0usize;
    while i + kw.len() <= bytes.len() && out.len() < max {
        // Match the `stream` keyword, but not the `stream` tail of `endstream`.
        if &bytes[i..i + kw.len()] == kw && !(i >= 3 && &bytes[i - 3..i] == b"end") {
            let mut s = i + kw.len();
            if bytes.get(s) == Some(&b'\r') {
                s += 1;
            }
            if bytes.get(s) == Some(&b'\n') {
                s += 1;
            }
            if let Some(rel) = find_sub(&bytes[s..], end_kw) {
                let chunk = &bytes[s..s + rel];
                if chunk.len() >= 3 && chunk[0..3] == SOI {
                    // Trim the EOL that precedes `endstream`.
                    let mut e = chunk.len();
                    while e > 0 && (chunk[e - 1] == b'\n' || chunk[e - 1] == b'\r') {
                        e -= 1;
                    }
                    out.push(chunk[..e].to_vec());
                }
                i = s + rel + end_kw.len();
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Naive byte substring search (small needles, one-shot scan).
fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

// ── PDF image-XObject extraction (real codec decode) ─────────────────────────
//
// Office scanners write each page as an /XObject /Subtype /Image whose bytes are
// CCITT Group 3/4 fax, JBIG2, JPX, or (less often) a raw DCTDecode JPEG. The old
// [`extract_pdf_jpeg_streams`] byte-grep only ever matched the last of those — a
// CCITT G4 stream has no `FF D8 FF`, so a scan came back `pages: 0` and a
// misleading `ocr_unavailable`. This walks the PDF object graph with lopdf and
// decodes each page's image XObject into a bitmap the OCR/vision ladder can read:
//   * DCTDecode  — the stream IS a JPEG; hand the bytes to `image` unchanged.
//   * CCITTFaxDecode — pure-Rust `fax` crate (T.4 G3 / T.6 G4), honoring the
//     DecodeParms K / Columns / Rows / BlackIs1.
//   * FlateDecode / uncompressed raw samples — DeviceGray/DeviceRGB @ 1/8 bpc.
//   * JBIG2Decode / JPXDecode — recognized but NOT decoded; reported honestly as
//     `unsupported_codec` with the codec name so the agent knows why.

/// One page's best raster, or the name of an image codec we recognized but do
/// not decode (JBIG2Decode / JPXDecode) — surfaced as `unsupported_codec`.
enum PageRaster {
    Image(DynamicImage),
    Unsupported(String),
}

/// Follow a single indirect reference; direct objects pass through untouched.
fn deref<'a>(doc: &'a Document, o: &'a Object) -> &'a Object {
    match o {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(o),
        _ => o,
    }
}

/// Walk a PDF's pages → Resources → XObject → `/Subtype /Image`, decoding up to
/// `max` pages' images (each page's LARGEST decodable image) in document order.
/// Returns `(pages, unsupported_codec)` where `unsupported_codec` is set — only
/// when NO page decoded — to the name of an image codec we found but could not
/// read. lopdf can panic on malformed files, so the whole walk is wrapped in
/// `catch_unwind`; any failure yields an empty result and the caller falls back
/// to the legacy JPEG-stream grep.
fn extract_pdf_page_images(bytes: &[u8], max: usize) -> (Vec<DynamicImage>, Option<String>) {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let doc = Document::load_mem(bytes).ok()?;
        let mut images: Vec<DynamicImage> = Vec::new();
        let mut unsupported: Option<String> = None;
        for (i, (_num, page_id)) in doc.get_pages().into_iter().enumerate() {
            if i >= max {
                break;
            }
            match best_page_image(&doc, page_id) {
                Some(PageRaster::Image(img)) => images.push(img),
                Some(PageRaster::Unsupported(codec)) => {
                    unsupported.get_or_insert(codec);
                }
                None => {}
            }
        }
        Some((images, unsupported))
    }));
    match res {
        Ok(Some((imgs, unsupported))) => {
            let unsupported = if imgs.is_empty() { unsupported } else { None };
            (imgs, unsupported)
        }
        _ => (Vec::new(), None),
    }
}

/// Decode ONE specific page's image (for the vision path, where the page INDEX
/// must be honored — a page with no decodable image yields `None`, never the
/// next page's image). `catch_unwind` for the same reason as above.
fn decode_pdf_page_image(bytes: &[u8], page: usize) -> Option<DynamicImage> {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let doc = Document::load_mem(bytes).ok()?;
        let (_num, page_id) = doc.get_pages().into_iter().nth(page)?;
        match best_page_image(&doc, page_id) {
            Some(PageRaster::Image(img)) => Some(img),
            _ => None,
        }
    }));
    res.ok().flatten()
}

/// The largest decodable image XObject on one page. When nothing decodes but an
/// unsupported codec was present, that codec name is returned instead.
fn best_page_image(doc: &Document, page_id: ObjectId) -> Option<PageRaster> {
    let mut best: Option<(u64, DynamicImage)> = None;
    let mut unsupported: Option<String> = None;
    for stream in page_image_xobjects(doc, page_id) {
        match decode_image_xobject(doc, stream) {
            Some(PageRaster::Image(img)) => {
                let area = img.width() as u64 * img.height() as u64;
                if best.as_ref().map_or(true, |(a, _)| area > *a) {
                    best = Some((area, img));
                }
            }
            Some(PageRaster::Unsupported(codec)) => {
                unsupported.get_or_insert(codec);
            }
            None => {}
        }
    }
    match best {
        Some((_, img)) => Some(PageRaster::Image(img)),
        None => unsupported.map(PageRaster::Unsupported),
    }
}

/// Gather the image-XObject streams referenced by a page's resources (both an
/// inline `/Resources` dict and any inherited/referenced ones). Stencil masks
/// (`/ImageMask true`) are skipped — they mask other content, they are not scans.
fn page_image_xobjects<'a>(doc: &'a Document, page_id: ObjectId) -> Vec<&'a Stream> {
    let mut out: Vec<&Stream> = Vec::new();
    let (inline, refs) = match doc.get_page_resources(page_id) {
        Ok(v) => v,
        Err(_) => return out,
    };
    let mut resource_dicts: Vec<&Dictionary> = Vec::new();
    if let Some(d) = inline {
        resource_dicts.push(d);
    }
    for rid in refs {
        if let Ok(d) = doc.get_dictionary(rid) {
            resource_dicts.push(d);
        }
    }
    for rd in resource_dicts {
        let xdict = match rd.get_deref(b"XObject", doc).and_then(Object::as_dict) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for (_name, val) in xdict.iter() {
            let stream = match deref(doc, val).as_stream() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let is_image = stream
                .dict
                .get_deref(b"Subtype", doc)
                .ok()
                .and_then(|o| o.as_name().ok())
                == Some(&b"Image"[..]);
            if !is_image {
                continue;
            }
            let is_mask = stream
                .dict
                .get(b"ImageMask")
                .ok()
                .and_then(|o| o.as_bool().ok())
                .unwrap_or(false);
            if is_mask {
                continue;
            }
            out.push(stream);
        }
    }
    out
}

/// Decode one image XObject. `/Filter` may be a single name or an array; the
/// image codec is the LAST filter and any preceding FlateDecode is applied first.
fn decode_image_xobject(doc: &Document, stream: &Stream) -> Option<PageRaster> {
    let filters = stream.filters().unwrap_or_default();
    match filters.last().copied() {
        Some(b"DCTDecode") => {
            // The (possibly Flate-unwrapped) payload is a JPEG.
            let data = apply_pre_filters(&stream.content, &filters)?;
            image::load_from_memory(&data).ok().map(PageRaster::Image)
        }
        Some(b"CCITTFaxDecode") => {
            let data = apply_pre_filters(&stream.content, &filters)?;
            decode_ccitt_image(doc, &stream.dict, &data, filters.len() - 1).map(PageRaster::Image)
        }
        Some(b"JBIG2Decode") => Some(PageRaster::Unsupported("JBIG2Decode".to_string())),
        Some(b"JPXDecode") => Some(PageRaster::Unsupported("JPXDecode".to_string())),
        // No image codec as the last filter → raw sample data (uncompressed or
        // Flate/LZW/ASCII85, all of which lopdf inflates for us).
        _ => decode_raw_bitmap(doc, stream).map(PageRaster::Image),
    }
}

/// Apply every filter EXCEPT the last (the image codec) — these must be
/// decompression pre-passes. Only FlateDecode is honored; a stacked stream using
/// anything else before the image codec is skipped (`None`). The overwhelmingly
/// common single-filter case (`filters.len() <= 1`) returns the bytes unchanged.
fn apply_pre_filters(content: &[u8], filters: &[&[u8]]) -> Option<Vec<u8>> {
    if filters.len() <= 1 {
        return Some(content.to_vec());
    }
    let mut data = content.to_vec();
    for f in &filters[..filters.len() - 1] {
        data = match *f {
            b"FlateDecode" => inflate_zlib(&data)?,
            _ => return None,
        };
    }
    Some(data)
}

/// Inflate a zlib (PDF FlateDecode) stream, falling back to raw DEFLATE.
fn inflate_zlib(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    if flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).is_ok() && !out.is_empty() {
        return Some(out);
    }
    out.clear();
    if flate2::read::DeflateDecoder::new(data).read_to_end(&mut out).is_ok() && !out.is_empty() {
        return Some(out);
    }
    None
}

/// Decode a CCITTFaxDecode image (T.4 Group 3 / T.6 Group 4) to 8-bit grayscale
/// via the pure-Rust `fax` crate, honoring the DecodeParms:
///   * `K`      < 0 → G4 (T.6); >= 0 → G3 (T.4, best-effort).
///   * `Columns`      image width in pixels (default 1728).
///   * `Rows`         image height; else the XObject `/Height`; else decode to EOB.
///   * `BlackIs1`     default false → decoder-white = paper = 255, black = ink = 0;
///                    true inverts (white=0, black=255).
/// `EncodedByteAlign` is read but cannot be honored through the `fax` crate's
/// whole-stream decoder — our scanner corpus does not set it. A partially decoded
/// stream keeps the lines it produced rather than failing the whole page.
fn decode_ccitt_image(
    doc: &Document,
    img_dict: &Dictionary,
    data: &[u8],
    ccitt_idx: usize,
) -> Option<DynamicImage> {
    use fax::decoder::{decode_g3, decode_g4, pels};
    use fax::Color;

    let parms = ccitt_decode_parms(doc, img_dict, ccitt_idx);
    let parm_i = |key: &[u8]| -> Option<i64> {
        parms.and_then(|p| p.get_deref(key, doc).ok()).and_then(|o| o.as_i64().ok())
    };
    let parm_b = |key: &[u8]| -> Option<bool> {
        parms.and_then(|p| p.get(key).ok()).and_then(|o| o.as_bool().ok())
    };
    let dict_i = |key: &[u8]| -> Option<i64> {
        img_dict.get_deref(key, doc).ok().and_then(|o| o.as_i64().ok())
    };

    let k = parm_i(b"K").unwrap_or(0);
    let black_is_1 = parm_b(b"BlackIs1").unwrap_or(false);
    let width_i = parm_i(b"Columns").filter(|c| *c > 0).or_else(|| dict_i(b"Width")).unwrap_or(1728);
    let height_i = parm_i(b"Rows").filter(|r| *r > 0).or_else(|| dict_i(b"Height"));
    if !(1..=65535).contains(&width_i) {
        return None;
    }
    let width = width_i as u16;
    let height = height_i.filter(|h| (1..=65535).contains(h)).map(|h| h as u16);

    // fax::Color::White = decoder background; map paper→255 / ink→0 by default,
    // inverting when BlackIs1 flips the sample sense.
    let (white_val, black_val): (u8, u8) = if black_is_1 { (0, 255) } else { (255, 0) };
    let mut rows_buf: Vec<u8> = Vec::new();
    let mut nrows: u32 = 0;
    let mut push_line = |transitions: &[u16]| {
        // DoS guard: cap the accumulated bitmap at ~96 MB regardless of Rows.
        if rows_buf.len() > 96_000_000 {
            return;
        }
        for c in pels(transitions, width) {
            rows_buf.push(match c {
                Color::White => white_val,
                Color::Black => black_val,
            });
        }
        nrows += 1;
    };

    if k < 0 {
        decode_g4(data.iter().copied(), width, height, &mut push_line);
    } else {
        // G3 (1-D K==0, mixed 2-D K>0). The `fax` G3 decoder expects EOL codes; a
        // PDF stream written without them simply yields no lines → page skipped.
        decode_g3(data.iter().copied(), &mut push_line);
    }

    if nrows == 0 || rows_buf.len() != (width as usize) * (nrows as usize) {
        return None;
    }
    image::GrayImage::from_raw(width as u32, nrows, rows_buf).map(DynamicImage::ImageLuma8)
}

/// Resolve the DecodeParms dictionary for the CCITT filter: a bare dict, or the
/// `ccitt_idx`-th entry of a per-filter array. `/DP` is the abbreviated key.
fn ccitt_decode_parms<'a>(
    doc: &'a Document,
    img_dict: &'a Dictionary,
    ccitt_idx: usize,
) -> Option<&'a Dictionary> {
    let obj = img_dict
        .get_deref(b"DecodeParms", doc)
        .or_else(|_| img_dict.get_deref(b"DP", doc))
        .ok()?;
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Array(arr) => match deref(doc, arr.get(ccitt_idx)?) {
            Object::Dictionary(d) => Some(d),
            _ => None,
        },
        _ => None,
    }
}

/// Decode a raw-sample image XObject (uncompressed or Flate/LZW/ASCII85, which
/// lopdf inflates) interpreting DeviceGray/DeviceRGB at 1 or 8 bpc. Any other
/// colorspace/depth (Indexed, CMYK, 16 bpc, …) is skipped gracefully (`None`).
fn decode_raw_bitmap(doc: &Document, stream: &Stream) -> Option<DynamicImage> {
    let dict = &stream.dict;
    let width = dict.get_deref(b"Width", doc).ok().and_then(|o| o.as_i64().ok())?;
    let height = dict.get_deref(b"Height", doc).ok().and_then(|o| o.as_i64().ok())?;
    if !(1..=20000).contains(&width) || !(1..=20000).contains(&height) {
        return None;
    }
    let (w, h) = (width as u32, height as u32);
    let bpc = dict.get_deref(b"BitsPerComponent", doc).ok().and_then(|o| o.as_i64().ok()).unwrap_or(8);
    let comps = colorspace_components(doc, dict)?;
    let data = stream.decompressed_content_with_limit(96 << 20).ok()?;

    match (comps, bpc) {
        (1, 8) => {
            let need = (w as usize).checked_mul(h as usize)?;
            if data.len() < need {
                return None;
            }
            image::GrayImage::from_raw(w, h, data[..need].to_vec()).map(DynamicImage::ImageLuma8)
        }
        (1, 1) => unpack_1bpc_gray(&data, w, h).map(DynamicImage::ImageLuma8),
        (3, 8) => {
            let need = (w as usize).checked_mul(h as usize)?.checked_mul(3)?;
            if data.len() < need {
                return None;
            }
            image::RgbImage::from_raw(w, h, data[..need].to_vec()).map(DynamicImage::ImageRgb8)
        }
        _ => None,
    }
}

/// Component count for the colorspaces we support: DeviceGray/CalGray → 1,
/// DeviceRGB/CalRGB → 3, ICCBased by its `/N`. Everything else → `None` (skip).
fn colorspace_components(doc: &Document, dict: &Dictionary) -> Option<u8> {
    let cs = dict
        .get_deref(b"ColorSpace", doc)
        .or_else(|_| dict.get_deref(b"CS", doc))
        .ok()?;
    match cs {
        Object::Name(n) => name_cs_components(n),
        Object::Array(arr) => {
            let head = arr.first().and_then(|o| deref(doc, o).as_name().ok())?;
            match head {
                b"ICCBased" => {
                    let s = deref(doc, arr.get(1)?).as_stream().ok()?;
                    match s.dict.get_deref(b"N", doc).ok().and_then(|o| o.as_i64().ok())? {
                        1 => Some(1),
                        3 => Some(3),
                        _ => None,
                    }
                }
                other => name_cs_components(other),
            }
        }
        _ => None,
    }
}

fn name_cs_components(n: &[u8]) -> Option<u8> {
    match n {
        b"DeviceGray" | b"G" | b"CalGray" => Some(1),
        b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(3),
        _ => None,
    }
}

/// Unpack a 1-bit DeviceGray sample array (MSB-first, rows byte-padded) to an
/// 8-bit GrayImage: bit 1 → white (255), bit 0 → black (0) per the default Decode.
fn unpack_1bpc_gray(data: &[u8], w: u32, h: u32) -> Option<image::GrayImage> {
    let stride = (w as usize).checked_add(7)? / 8;
    if data.len() < stride.checked_mul(h as usize)? {
        return None;
    }
    let mut out = Vec::with_capacity((w as usize).checked_mul(h as usize)?);
    for row in 0..h as usize {
        let base = row * stride;
        for col in 0..w as usize {
            let bit = (data[base + col / 8] >> (7 - (col & 7))) & 1;
            out.push(if bit == 1 { 255 } else { 0 });
        }
    }
    image::GrayImage::from_raw(w, h, out)
}

/// PNG-encode a decoded page bitmap for the OCR ladder (lossless keeps 1-bit scan
/// text crisp for the recognizer, unlike a JPEG re-compress).
fn encode_page_png(img: &DynamicImage) -> Option<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// OCR-side page extraction: decode up to `max` page images (lopdf XObject path),
/// PNG-encode them for the recognizer, and — if lopdf found nothing — fall back to
/// the legacy raw JPEG-stream grep. Returns `(encoded_pages, unsupported_codec)`;
/// `unsupported_codec` is only set when NO page yielded a bitmap by either route.
async fn ocr_pdf_page_images(bytes: &[u8], max: usize) -> (Vec<Vec<u8>>, Option<String>) {
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        let (imgs, unsupported) = extract_pdf_page_images(&bytes, max);
        let encoded: Vec<Vec<u8>> = imgs.iter().filter_map(encode_page_png).collect();
        if !encoded.is_empty() {
            return (encoded, None);
        }
        // lopdf produced nothing decodable — try the legacy one-JPEG-per-page grep
        // (covers files lopdf cannot parse but whose SOI streams the scan uses).
        let streams = extract_pdf_jpeg_streams(&bytes, max);
        if !streams.is_empty() {
            return (streams, None);
        }
        (Vec::new(), unsupported)
    })
    .await
    .unwrap_or((Vec::new(), None))
}

/// An owner-password PDF: the text layer and the JPEG streams are ciphertext, so
/// both ladders come up empty though the bytes are intact. The `/Encrypt`
/// trailer reference is the honest tell (a byte scan, same discipline as the
/// stream walk) — used to report `encrypted` instead of a misleading
/// `ocr_unavailable`.
fn pdf_is_encrypted(bytes: &[u8]) -> bool {
    find_sub(bytes, b"/Encrypt").is_some()
}

/// Turn an owner-password PDF into a plaintext one the OCR/vision ladder can read.
///
/// Most of our `/Encrypt` CAS PDFs carry only an *owner* password: the *user*
/// password is empty, so any viewer opens them, but the strings and streams are
/// RC4/AES ciphertext — the text layer yields nothing and the raw JPEG-stream
/// scan sees ciphertext, not `FF D8 FF`. This loads the PDF, decrypts every
/// string and stream in place with the EMPTY user password (lopdf drops the
/// `/Encrypt` trailer in the process), and re-serializes to a plaintext PDF the
/// rest of the ladder then handles unchanged. lopdf 0.44 covers the real corpus:
/// Standard security handler V1/V2 (RC4) AND V4/V5 crypt filters (AES-128 AESV2,
/// AES-256 AESV3) — our CAS files are `/V 4 /R 4` AESV2, which lopdf ≤0.34 could
/// not do (it rejected `/V 4` outright).
///
/// lopdf parse / decrypt / serialize can panic on some malformed files, so the
/// work runs behind `catch_unwind` on the blocking pool (same discipline as
/// [`pdf_text_layer`]). On ANY failure — not encrypted, a real user password
/// (`IncorrectPassword`), a parse error, an empty serialization, or a panic —
/// this returns the ORIGINAL bytes untouched, so the caller's existing ladder and
/// its `encrypted` fallback still apply. It never panics outward.
///
/// Returns `(bytes, decrypted)` where `decrypted` is true only when the bytes
/// were actually replaced with a plaintext re-serialization — the persistence
/// layer records that fact as the `+decrypted` suffix on `extract_source`.
async fn maybe_decrypt_pdf(bytes: Vec<u8>) -> (Vec<u8>, bool) {
    if !pdf_is_encrypted(&bytes) {
        return (bytes, false);
    }
    // Working copy for the blocking closure; the original stays here so the
    // (theoretical) JoinError path can still hand back untouched bytes.
    let input = bytes.clone();
    let decrypted: Option<Vec<u8>> = tokio::task::spawn_blocking(move || {
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // lopdf 0.44 attempts decryption WITH THE EMPTY USER PASSWORD during
            // `load_mem` itself: on success it decrypts every string/stream in
            // place and strips the `/Encrypt` trailer entry. So after a load that
            // parsed:
            //   * is_encrypted() == false → the empty password worked (owner-only
            //     protection, or the file was not truly encrypted). Re-serialize
            //     the now-plaintext document.
            //   * is_encrypted() == true  → a real *user* password blocks it.
            //     Give up and return the original bytes, so the ladder's
            //     `encrypted` status still fires.
            // (Calling doc.decrypt("") here would be wrong — the doc is already
            //  decrypted, so it errors with NotEncrypted.)
            let mut doc = lopdf::Document::load_mem(&input).ok()?;
            if doc.is_encrypted() {
                return None;
            }
            let mut out = Vec::new();
            doc.save_to(&mut out).ok()?;
            Some(out)
        }));
        match out {
            Ok(Some(out)) if !out.is_empty() => Some(out),
            _ => None,
        }
    })
    .await
    .ok()
    .flatten();
    match decrypted {
        Some(out) => (out, true),
        None => (bytes, false),
    }
}

// ── AcroForm field extraction (fillable PDF forms) ──────────────────────────
//
// A fillable form (`/AcroForm`) keeps what the customer typed in the field
// VALUES (`/V`), inside compressed object streams — NOT in the page content
// streams `pdf_extract` reads. So the text layer of a filled form is just the
// blank template ("Konstanz" never appears). We walk the field tree with lopdf
// after decryption and turn every filled field into a `Feldname: Wert` line,
// appended to the extracted text as a delimited block. Same panic discipline as
// the other lopdf walkers: the whole walk runs behind `catch_unwind`.

/// Delimiter that opens the appended AcroForm block (see [`format_acroform_block`]).
const ACROFORM_HEADER: &str = "--- Formularfelder ---";

/// Decode a PDF text string (`/V`, `/T`) to Rust text. lopdf hands us the raw
/// decoded bytes of both literal and hex strings; we only have to interpret the
/// text encoding: UTF-16 with a BOM (the common case for filled forms — values
/// arrive as `<FEFF…>` UTF-16BE hex), else PDFDocEncoding, approximated by latin1
/// (byte → code point) which is correct for the ASCII + Latin-1 range that covers
/// German field values.
fn decode_pdf_text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let u16s: Vec<u16> =
            bytes[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u16s)
    } else if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let u16s: Vec<u16> =
            bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u16s)
    } else {
        // PDFDocEncoding ≈ latin1 for the printable range.
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// The field's partial name (`/T`) decoded to text, or `None` when absent/empty.
fn acroform_partial_name(doc: &Document, dict: &Dictionary) -> Option<String> {
    match dict.get(b"T").ok().map(|o| deref(doc, o)) {
        Some(Object::String(b, _)) => {
            let s = decode_pdf_text_string(b);
            if s.is_empty() { None } else { Some(s) }
        }
        _ => None,
    }
}

/// The display value of ONE terminal field, or `None` when empty/unset:
///   * `/Btn` (checkbox/radio): the on-state name from `/V` (else `/AS`); `Off`
///     and empty are skipped (unchecked).
///   * `/Tx` / `/Ch` (text / choice): the `/V` string, or a `, `-joined array for
///     a multi-select choice.
/// `ft` is the field type (own `/FT`, else inherited from an ancestor).
fn acroform_field_value(doc: &Document, dict: &Dictionary, ft: Option<&[u8]>) -> Option<String> {
    let v = dict.get(b"V").ok().map(|o| deref(doc, o));
    let is_btn = ft == Some(&b"Btn"[..]) || matches!(v, Some(Object::Name(_)));
    if is_btn {
        let name: Vec<u8> = match v {
            Some(Object::Name(n)) => n.clone(),
            _ => match dict.get(b"AS").ok().map(|o| deref(doc, o)) {
                Some(Object::Name(n)) => n.clone(),
                _ => return None,
            },
        };
        let s = String::from_utf8_lossy(&name);
        let s = s.trim();
        if s.is_empty() || s == "Off" {
            return None;
        }
        return Some(s.to_string());
    }
    match v? {
        Object::String(b, _) => {
            let s = decode_pdf_text_string(b);
            let t = s.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        }
        Object::Array(arr) => {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|o| match deref(doc, o) {
                    Object::String(b, _) => {
                        let s = decode_pdf_text_string(b);
                        let t = s.trim();
                        if t.is_empty() { None } else { Some(t.to_string()) }
                    }
                    _ => None,
                })
                .collect();
            if parts.is_empty() { None } else { Some(parts.join(", ")) }
        }
        _ => None,
    }
}

/// Recurse the AcroForm field tree. A node with `/Kids` that carry their own `/T`
/// is a non-terminal field (fully-qualified names join with `.`, like PDF
/// viewers); kids WITHOUT a `/T` are just widget annotations of THIS terminal
/// field (radio widgets, a checkbox's appearance) and are not recursed into.
/// `/FT` is inherited down the tree. Guarded against pathological depth/width.
fn walk_acroform_field(
    doc: &Document,
    obj: &Object,
    parent_name: &str,
    inherited_ft: Option<Vec<u8>>,
    out: &mut Vec<(String, String)>,
    depth: usize,
) {
    if depth > 64 || out.len() >= 512 {
        return;
    }
    let dict = match deref(doc, obj).as_dict() {
        Ok(d) => d,
        Err(_) => return,
    };
    let partial = acroform_partial_name(doc, dict);
    let full = match &partial {
        Some(t) => {
            if parent_name.is_empty() {
                t.clone()
            } else {
                format!("{parent_name}.{t}")
            }
        }
        None => parent_name.to_string(),
    };
    let ft = dict
        .get_deref(b"FT", doc)
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|n| n.to_vec())
        .or(inherited_ft);

    let child_fields: Vec<&Object> =
        match dict.get_deref(b"Kids", doc).ok().and_then(|o| o.as_array().ok()) {
            Some(kids) => kids
                .iter()
                .filter(|k| deref(doc, k).as_dict().map(|d| d.has(b"T")).unwrap_or(false))
                .collect(),
            None => Vec::new(),
        };
    if !child_fields.is_empty() {
        for kid in child_fields {
            walk_acroform_field(doc, kid, &full, ft.clone(), out, depth + 1);
        }
        return;
    }

    if let Some(val) = acroform_field_value(doc, dict, ft.as_deref()) {
        let name = if full.is_empty() { "Feld".to_string() } else { full };
        out.push((name, val));
    }
}

/// Extract every filled AcroForm field as `(fully-qualified name, value)` pairs in
/// field order. Panic-safe (lopdf can panic on malformed files); returns an empty
/// vec for a PDF with no AcroForm or that fails to parse.
fn extract_acroform_fields(bytes: &[u8]) -> Vec<(String, String)> {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let doc = Document::load_mem(bytes).ok()?;
        let root = deref(&doc, doc.trailer.get(b"Root").ok()?).as_dict().ok()?;
        let acro = root.get_deref(b"AcroForm", &doc).ok()?.as_dict().ok()?;
        let fields = acro.get_deref(b"Fields", &doc).ok()?.as_array().ok()?;
        let mut out: Vec<(String, String)> = Vec::new();
        for f in fields {
            walk_acroform_field(&doc, f, "", None, &mut out, 0);
        }
        Some(out)
    }));
    res.ok().flatten().unwrap_or_default()
}

/// Format extracted fields into the appended block (one `Feldname: Wert` line
/// each, internal whitespace in the value collapsed so the line stays minable).
/// Empty string when there are no filled fields. Capped at ~8 000 chars so it can
/// never crowd out the 20 000-char persisted store.
fn format_acroform_block(fields: &[(String, String)]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    let mut block = String::from(ACROFORM_HEADER);
    for (name, val) in fields {
        let val_flat = val.split_whitespace().collect::<Vec<_>>().join(" ");
        if val_flat.is_empty() {
            continue;
        }
        let line = format!("\n{name}: {val_flat}");
        if block.len() + line.len() > 8000 {
            break;
        }
        block.push_str(&line);
    }
    if block == ACROFORM_HEADER {
        String::new()
    } else {
        block
    }
}

/// Build the `--- Formularfelder ---` block for a (already decrypted) PDF on the
/// blocking pool. Empty string when there are no filled fields / no AcroForm.
async fn pdf_acroform_block(bytes: Vec<u8>) -> String {
    tokio::task::spawn_blocking(move || format_acroform_block(&extract_acroform_fields(&bytes)))
        .await
        .unwrap_or_default()
}

/// Append the AcroForm block to the ladder/text-layer text with a blank-line
/// delimiter. No-op when the block is empty; returns just the block when the base
/// text is blank (a filled form whose page text was empty).
fn append_form_block(text: &str, form_block: &str) -> String {
    if form_block.is_empty() {
        return text.to_string();
    }
    if text.trim().is_empty() {
        return form_block.to_string();
    }
    format!("{}\n\n{}", text.trim_end(), form_block)
}

// ── OCR quality assessment ──────────────────────────────────────────────────

/// Heuristic quality signal for OCR output. `has_essential` is deliberately NOT
/// computed here — the brain decides that from these numbers plus the question.
#[derive(Serialize)]
pub struct OcrQuality {
    pub chars: usize,
    pub lines: usize,
    /// Fraction of tokens (≥3 chars) that look like real German/English words.
    pub dictionary_ratio: f32,
    /// Fraction of tokens (≥3 chars) that are mostly punctuation/symbol noise.
    pub garbage_ratio: f32,
}

impl OcrQuality {
    fn empty() -> Self {
        Self { chars: 0, lines: 0, dictionary_ratio: 0.0, garbage_ratio: 0.0 }
    }
}

/// A tiny DE/EN function-word set: membership marks a token as a plausible word
/// even when it is short. Anything else falls back to the letter-ratio rule.
const STOPWORDS: &[&str] = &[
    "der", "die", "das", "und", "ist", "ein", "eine", "mit", "von", "den", "dem",
    "für", "auf", "nicht", "wir", "sie", "ich", "wird", "sind", "auch", "aber",
    "the", "and", "for", "you", "are", "with", "this", "that", "have", "not",
    "your", "was", "gmbh", "serial", "modell", "model", "gerät",
];

/// Assess OCR text quality. Tokenizes on whitespace, considers tokens ≥3 chars,
/// and classifies each as "plausible word" (mostly letters or a stopword) vs.
/// "garbage" (mostly non-letters — the ragged symbol soup a bad scan produces).
pub fn assess_quality(text: &str) -> OcrQuality {
    let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    let chars = text.chars().count();
    let tokens: Vec<&str> = text
        .split(|c: char| c.is_whitespace())
        .filter(|t| t.chars().count() >= 3)
        .collect();
    if tokens.is_empty() {
        return OcrQuality { chars, lines, dictionary_ratio: 0.0, garbage_ratio: 0.0 };
    }
    let mut plausible = 0usize;
    let mut garbage = 0usize;
    for t in &tokens {
        let total = t.chars().count().max(1);
        let letters = t.chars().filter(|c| c.is_alphabetic()).count();
        let ratio = letters as f32 / total as f32;
        let lower = t.to_lowercase();
        let lower = lower.trim_matches(|c: char| !c.is_alphanumeric());
        if STOPWORDS.contains(&lower) || (letters >= 3 && ratio >= 0.6) {
            plausible += 1;
        }
        if ratio < 0.3 {
            garbage += 1;
        }
    }
    let n = tokens.len() as f32;
    OcrQuality {
        chars,
        lines,
        dictionary_ratio: plausible as f32 / n,
        garbage_ratio: garbage as f32 / n,
    }
}

// ── Recognized line boxes ───────────────────────────────────────────────────

/// One recognized text line with its bounding box normalized to 0..1 so the
/// agent can later request a region crop (e.g. only the signature zone).
#[derive(Serialize, Clone)]
pub struct LineBox {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

// ── OCR engine ladder ───────────────────────────────────────────────────────

struct OcrLadder {
    status: &'static str,
    source: &'static str,
    text: String,
    lines: Vec<LineBox>,
}

impl OcrLadder {
    fn unavailable() -> Self {
        Self { status: "ocr_unavailable", source: "", text: String::new(), lines: Vec::new() }
    }
}

/// Run the OCR ladder over one or more page images:
///   a) external `ECK_OCR_CMD` subprocess (e.g. tesseract) if configured, else
///   b) the pure-Rust `ocrs` engine (feature `ocr-local`).
/// Returns `ocr_unavailable` (never panics/hangs) when neither can produce text.
async fn ocr_images(images: &[Vec<u8>]) -> OcrLadder {
    if images.is_empty() {
        return OcrLadder::unavailable();
    }
    if let Ok(cmd) = std::env::var("ECK_OCR_CMD") {
        let cmd = cmd.trim().to_string();
        if !cmd.is_empty() {
            return match run_external_ocr_pages(&cmd, images).await {
                Ok(text) => OcrLadder { status: "ok", source: "external_cmd", text, lines: Vec::new() },
                Err(e) => {
                    warn!("[attachments] external OCR failed: {e}");
                    OcrLadder::unavailable()
                }
            };
        }
    }
    ocrs_ladder(images).await
}

/// `{input}` placeholder → a temp image path; capture stdout as the OCR text.
async fn run_external_ocr_pages(cmd_tmpl: &str, images: &[Vec<u8>]) -> Result<String> {
    let multi = images.len() > 1;
    let mut all = String::new();
    for (idx, img) in images.iter().enumerate() {
        let text = run_external_ocr_one(cmd_tmpl, img).await?;
        if multi {
            all.push_str(&format!("\n--- page {} ---\n", idx + 1));
        }
        all.push_str(text.trim());
        all.push('\n');
    }
    Ok(all.trim().to_string())
}

async fn run_external_ocr_one(cmd_tmpl: &str, img: &[u8]) -> Result<String> {
    let tmp = std::env::temp_dir().join(format!("eck_ocr_{}.{}", uuid::Uuid::new_v4(), image_ext(img)));
    tokio::fs::write(&tmp, img).await.context("write OCR temp file")?;
    let tmp_str = tmp.to_string_lossy().to_string();

    let mut parts: Vec<String> = cmd_tmpl
        .split_whitespace()
        .map(|p| p.replace("{input}", &tmp_str))
        .collect();
    if !cmd_tmpl.contains("{input}") {
        parts.push(tmp_str);
    }
    let out = if let Some((prog, rest)) = parts.split_first() {
        tokio::process::Command::new(prog).args(rest).output().await
    } else {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(anyhow!("ECK_OCR_CMD is empty"));
    };
    let _ = tokio::fs::remove_file(&tmp).await;
    let out = out.context("spawn OCR subprocess")?;
    if !out.status.success() {
        anyhow::bail!(
            "OCR command exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(feature = "ocr-local")]
async fn ocrs_ladder(images: &[Vec<u8>]) -> OcrLadder {
    match ocrs_backend::run(images).await {
        Ok((text, lines)) => OcrLadder { status: "ok", source: "ocrs", text, lines },
        Err(e) => {
            warn!("[attachments] ocrs OCR failed: {e}");
            OcrLadder::unavailable()
        }
    }
}

#[cfg(not(feature = "ocr-local"))]
async fn ocrs_ladder(_images: &[Vec<u8>]) -> OcrLadder {
    OcrLadder::unavailable()
}

// ── ocrs backend (pure-Rust OCR; feature `ocr-local`) ───────────────────────

#[cfg(feature = "ocr-local")]
mod ocrs_backend {
    use super::LineBox;
    use std::path::{Path, PathBuf};
    use anyhow::{anyhow, Context, Result};
    use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
    use rten::Model;
    use tokio::sync::OnceCell;
    use tracing::info;

    /// Detection + recognition models are ~a few MB each; load once per process.
    static OCRS: OnceCell<OcrsEngine> = OnceCell::const_new();

    /// The two ocrs model files hosted on the official release bucket. Fetched
    /// once on first use when absent from `ECK_OCRS_MODEL_DIR`.
    const DETECTION_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.rten";
    const RECOGNITION_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.rten";

    pub(super) struct OcrsEngine {
        engine: OcrEngine,
    }

    impl OcrsEngine {
        fn load(det: &Path, rec: &Path) -> Result<Self> {
            let detection_model = Model::load_file(det)
                .map_err(|e| anyhow!("load detection model {}: {e}", det.display()))?;
            let recognition_model = Model::load_file(rec)
                .map_err(|e| anyhow!("load recognition model {}: {e}", rec.display()))?;
            let engine = OcrEngine::new(OcrEngineParams {
                detection_model: Some(detection_model),
                recognition_model: Some(recognition_model),
                ..Default::default()
            })?;
            Ok(Self { engine })
        }

        /// Recognize one page: returns `(text, up-to-80 line boxes)`.
        fn recognize(&self, bytes: &[u8]) -> Result<(String, Vec<LineBox>)> {
            // Route via mime-less detection: PDF page images are JPEG, but a
            // direct image attachment may be AVIF (encode-only in `image`).
            let img = super::decode_image_bytes(bytes, "").context("decode page image")?;
            let rgb = img.to_rgb8();
            let (w, h) = rgb.dimensions();
            let source = ImageSource::from_bytes(rgb.as_raw(), (w, h))
                .map_err(|e| anyhow!("ocrs ImageSource: {e:?}"))?;
            let input = self.engine.prepare_input(source)?;
            let words = self.engine.detect_words(&input)?;
            let line_rects = self.engine.find_text_lines(&input, &words);
            let lines = self.engine.recognize_text(&input, &line_rects)?;

            let (fw, fh) = (w.max(1) as f32, h.max(1) as f32);
            let mut text = String::new();
            let mut boxes: Vec<LineBox> = Vec::new();
            for line in lines.into_iter().flatten() {
                let s = line.to_string();
                if s.trim().is_empty() {
                    continue;
                }
                if boxes.len() < 80 {
                    let r = line.bounding_rect();
                    boxes.push(LineBox {
                        text: s.clone(),
                        x: (r.left() as f32 / fw).clamp(0.0, 1.0),
                        y: (r.top() as f32 / fh).clamp(0.0, 1.0),
                        w: (r.width() as f32 / fw).clamp(0.0, 1.0),
                        h: (r.height() as f32 / fh).clamp(0.0, 1.0),
                    });
                }
                text.push_str(&s);
                text.push('\n');
            }
            Ok((text.trim().to_string(), boxes))
        }
    }

    async fn engine() -> Result<&'static OcrsEngine> {
        OCRS.get_or_try_init(|| async {
            let (det, rec) = ensure_models().await?;
            tokio::task::spawn_blocking(move || OcrsEngine::load(&det, &rec))
                .await
                .context("ocrs load task panicked")?
        })
        .await
    }

    /// Recognize every page image on the blocking pool (rten inference is
    /// CPU-bound). Line boxes come from the FIRST page only.
    pub(super) async fn run(images: &[Vec<u8>]) -> Result<(String, Vec<LineBox>)> {
        let engine = engine().await?;
        let images = images.to_vec();
        tokio::task::spawn_blocking(move || {
            let multi = images.len() > 1;
            let mut text = String::new();
            let mut boxes: Vec<LineBox> = Vec::new();
            for (idx, b) in images.iter().enumerate() {
                let (t, bx) = engine.recognize(b)?;
                if multi {
                    text.push_str(&format!("\n--- page {} ---\n", idx + 1));
                }
                text.push_str(t.trim());
                text.push('\n');
                if idx == 0 {
                    boxes = bx;
                }
            }
            Ok::<_, anyhow::Error>((text.trim().to_string(), boxes))
        })
        .await
        .context("ocrs recognize task panicked")?
    }

    /// Resolve the model dir (`ECK_OCRS_MODEL_DIR`, default `data/models/ocrs`)
    /// and, mirroring the local-NER model bootstrap, fetch either file from the
    /// official ocrs-models bucket on a cache miss (60s timeout). A download
    /// failure bubbles up as an error → the caller reports `ocr_unavailable`.
    async fn ensure_models() -> Result<(PathBuf, PathBuf)> {
        let dir = std::env::var("ECK_OCRS_MODEL_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "data/models/ocrs".to_string());
        let dir = PathBuf::from(dir.trim());
        let det = dir.join("text-detection.rten");
        let rec = dir.join("text-recognition.rten");
        for (path, url) in [(&det, DETECTION_URL), (&rec, RECOGNITION_URL)] {
            if !path.exists() {
                download_model(path, url).await.with_context(|| {
                    format!(
                        "ocrs model missing and download failed — place text-detection.rten / \
                         text-recognition.rten in {}",
                        dir.display()
                    )
                })?;
            }
        }
        Ok((det, rec))
    }

    async fn download_model(path: &Path, url: &str) -> Result<()> {
        info!("[attachments] fetching ocrs model {} -> {}", url, path.display());
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        let bytes = http.get(url).send().await?.error_for_status()?.bytes().await?;
        let tmp = path.with_extension("rten.part");
        tokio::fs::write(&tmp, &bytes).await?;
        tokio::fs::rename(&tmp, path).await?;
        info!("[attachments] ocrs model ready: {} ({} bytes)", path.display(), bytes.len());
        Ok(())
    }
}

// ── ocr_attachment: per-file local extraction (zero AI calls) ───────────────

/// One file's OCR result — the per-entry payload of the `ocr_attachment` tool.
#[derive(Serialize)]
pub struct OcrResult {
    pub file_id: String,
    pub name: String,
    /// "ok" | "not_found" | "ocr_unavailable" | "encrypted" | "unsupported_codec".
    pub status: String,
    /// "text_layer" | "ocrs" | "external_cmd" | "".
    pub source: String,
    pub text: String,
    pub quality: OcrQuality,
    pub lines: Vec<LineBox>,
    pub pages: usize,
    /// Image codec that blocked extraction, present only on `unsupported_codec`
    /// (e.g. "JBIG2Decode" / "JPXDecode") — the page images exist but this build
    /// does not decode them; export_attachment still yields the intact bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
}

impl OcrResult {
    fn bare(file_id: &str, name: &str, status: &str) -> Self {
        Self {
            file_id: file_id.to_string(),
            name: name.to_string(),
            status: status.to_string(),
            source: String::new(),
            text: String::new(),
            quality: OcrQuality::empty(),
            pages: 0,
            lines: Vec::new(),
            codec: None,
        }
    }
}

/// The fields persisted on the byte-owning `file_resource` row so the extract
/// mesh-syncs (the blob does not) and repeat OCR calls are free. `text` is capped
/// at 20 000 chars (richer than the 6 000-char tool budget) and `quality` is
/// assessed over that stored text; `source` is the persisted `extract_source`
/// (a ladder source with an optional `+decrypted` suffix, OR a failure status).
struct StoredExtract {
    text: String,
    quality: OcrQuality,
    source: String,
    pages: i64,
    /// hex sha256 of the NORMALIZED extracted text (see [`extract_fingerprint`]),
    /// or `None` when the stored text is empty. Clusters re-uploaded copies of the
    /// same form whose bytes differ (so byte-hash CAS dedup gives each a fresh
    /// uuid) but whose extracted TEXT is identical — and "signed but otherwise
    /// blank" variants whose text still equals the template's.
    fingerprint: Option<String>,
}

/// sha256 (hex) of the extracted text after normalization — Unicode-lowercased,
/// every whitespace run collapsed to a single space, leading/trailing trimmed.
/// `None` for empty/whitespace-only text (nothing to cluster on). This is an
/// operator-log clustering key (defect 50), NOT part of the tool response.
fn extract_fingerprint(text: &str) -> Option<String> {
    let mut normalized = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            // Note a gap, but only emit it once we know a non-space follows
            // (so trailing whitespace never lands in the output).
            pending_space = !normalized.is_empty();
        } else {
            if pending_space {
                normalized.push(' ');
                pending_space = false;
            }
            normalized.extend(c.to_lowercase());
        }
    }
    if normalized.is_empty() {
        return None;
    }
    use sha2::{Digest, Sha256};
    Some(hex::encode(Sha256::digest(normalized.as_bytes())))
}

/// The byte-owning `file_resource` row resolved through the `replaced_by` chain
/// WITHOUT hydrating a missing blob — the stored-extract fast path must not pay a
/// mesh round trip when the text is already on the row. Carries the final row's
/// `cas_uuid` (the persist/read key) plus its stored extract, if any.
struct ExtractTarget {
    cas_uuid: String,
    path: PathBuf,
    storage_path: String,
    mime: String,
    name: String,
    hash: String,
    stored_ver: Option<i64>,
    stored_text: String,
    stored_source: String,
    stored_pages: i64,
}

/// Walk the `replaced_by` chain to the byte-owning row, reading its stored
/// extract alongside — same 4-hop walk as [`resolve_file_full`] but disk-only (no
/// mesh hydrate), so a node that already synced the text answers without pulling
/// bytes. `None` when no resolvable row exists.
async fn resolve_extract_target(db: &SurrealDb, cas_uuid: &str) -> Option<ExtractTarget> {
    let mut id = cas_uuid.to_string();
    let mut display_name = String::new();
    for hop in 0..4 {
        let rows: Vec<Value> = db
            .query(
                "SELECT cas_uuid, storage_path, original_name, mime_type, hash, replaced_by, \
                 extracted_text, extract_source, extractor_ver, extracted_pages \
                 FROM file_resource WHERE cas_uuid = $id LIMIT 1",
            )
            .bind(("id", id.clone()))
            .await
            .ok()?
            .take(0)
            .ok()?;
        let row = rows.into_iter().next()?;

        let name = row.get("original_name").and_then(|v| v.as_str()).unwrap_or("");
        if hop == 0 && !name.is_empty() {
            display_name = name.to_string();
        }

        let storage_path = row.get("storage_path").and_then(|v| v.as_str()).unwrap_or("");
        if !storage_path.is_empty() {
            if display_name.is_empty() {
                display_name = name.to_string();
            }
            let final_cas = row.get("cas_uuid").and_then(|v| v.as_str()).unwrap_or(&id).to_string();
            let mime = row
                .get("mime_type")
                .and_then(|v| v.as_str())
                .unwrap_or("application/octet-stream")
                .to_string();
            let hash = row.get("hash").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            return Some(ExtractTarget {
                cas_uuid: final_cas,
                path: PathBuf::from(".").join(storage_path),
                storage_path: storage_path.to_string(),
                mime,
                name: display_name,
                hash,
                stored_ver: row.get("extractor_ver").and_then(|v| v.as_i64()),
                stored_text: row.get("extracted_text").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                stored_source: row.get("extract_source").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                stored_pages: row.get("extracted_pages").and_then(|v| v.as_i64()).unwrap_or(0),
            });
        }

        match row
            .get("replaced_by")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(next) => id = next.to_string(),
            None => break,
        }
    }
    None
}

/// Extract text for ONE attachment via the local ladder (text layer → OCR).
/// Purely local — never calls an AI model.
///
/// Persistence (design 2026-07-28): before computing, if the byte-owning row
/// (tail of the `replaced_by` chain) already carries `extractor_ver ==
/// EXTRACTOR_VER`, rebuild the result from the stored fields and return — a
/// remote node that synced the extract answers WITHOUT the blob, and a repeat
/// call is free. Otherwise compute, then persist the result on that row
/// (fire-and-forget), including a failure stamp for `ocr_unavailable`/`encrypted`
/// so the sweep doesn't retry hopeless files every tick.
pub async fn ocr_one(db: &SurrealDb, cas_uuid: &str) -> OcrResult {
    let target = match resolve_extract_target(db, cas_uuid).await {
        Some(t) => t,
        None => return OcrResult::bare(cas_uuid, "", "not_found"),
    };

    // Fast path: a stored extract at the current version — no byte read, no
    // hydrate. (The sweep's candidate query already excludes these, so this
    // fires for repeat agent calls and for blob-less remote nodes.)
    if target.stored_ver == Some(EXTRACTOR_VER) {
        return stored_to_result(cas_uuid, &target.name, &target.stored_text, &target.stored_source, target.stored_pages);
    }

    // Compute path: pull the blob from the mesh if it isn't on disk (agent tools
    // want the text; the background sweep pre-checks existence so it never gets
    // here for a missing blob), then run the ladder.
    if !target.path.exists() {
        hydrate_from_mesh(&target.hash, &target.storage_path).await;
    }
    let bytes = match tokio::fs::read(&target.path).await {
        Ok(b) => b,
        Err(e) => {
            warn!("[attachments] read {} failed: {e}", target.path.display());
            return OcrResult::bare(cas_uuid, &target.name, "not_found");
        }
    };

    let (result, stored) = compute_ocr(cas_uuid, &target.name, &target.mime, bytes).await;
    // Persist (fire-and-forget, log-on-error) — including the failure cache. Only
    // "not_found" (handled above) is never persisted.
    persist_extract(db, &target.cas_uuid, &stored).await;
    result
}

/// Run the local ladder over one file's bytes, producing BOTH the tool-output
/// [`OcrResult`] (6 000-char budget, unchanged behavior) and the [`StoredExtract`]
/// to persist (20 000-char store + `extract_source`). `decrypted` is threaded in
/// from [`maybe_decrypt_pdf`] and recorded as the `+decrypted` suffix.
async fn compute_ocr(cas_uuid: &str, name: &str, mime: &str, bytes: Vec<u8>) -> (OcrResult, StoredExtract) {
    let is_pdf = mime.contains("pdf") || bytes.starts_with(b"%PDF");
    let is_image = mime.starts_with("image/") || looks_like_image(&bytes);

    if is_pdf {
        // Owner-password PDFs (empty user password) decrypt transparently here;
        // the whole ladder below then runs on plaintext bytes. A real user
        // password leaves the bytes unchanged → the `encrypted` fallback fires.
        let (bytes, decrypted) = maybe_decrypt_pdf(bytes).await;
        // Fillable-form fields (`/AcroForm /V`) live in object streams the text
        // layer never touches — extract them once and fold them into whichever
        // ladder text we end up with (text layer OR scan OCR).
        let form_block = pdf_acroform_block(bytes.clone()).await;
        let (layer, page_count) = pdf_text_layer(bytes.clone()).await;
        if non_ws_len(&layer) >= 50 {
            let text = append_form_block(&layer, &form_block);
            return make_result(cas_uuid, name, "ok", "text_layer", &text, page_count.max(1), decrypted, Vec::new());
        }
        // Scan PDF: decode the embedded page images (CCITT G3/G4, DCT JPEG, or
        // raw Flate bitmaps) and OCR them. When nothing decodes: filled form
        // fields are still real content; else an `/Encrypt` trailer means an
        // owner-password PDF; an unsupported image codec (JBIG2/JPX) is named so;
        // otherwise the ladder reports `ocr_unavailable`.
        let (images, unsupported) = ocr_pdf_page_images(&bytes, 4).await;
        if images.is_empty() {
            if !form_block.is_empty() {
                return make_result(cas_uuid, name, "ok", "text_layer", &form_block, page_count.max(1), decrypted, Vec::new());
            }
            if pdf_is_encrypted(&bytes) {
                return make_result(cas_uuid, name, "encrypted", "", "", 0, decrypted, Vec::new());
            }
            if let Some(codec) = unsupported {
                return make_unsupported(cas_uuid, name, &codec);
            }
        }
        let pages = images.len();
        let ladder = ocr_images(&images).await;
        let text = append_form_block(&ladder.text, &form_block);
        // Any content — recognized page text OR filled form fields — is "ok".
        let (status, source): (&str, &str) = if ladder.status == "ok" {
            (ladder.status, ladder.source)
        } else if !form_block.is_empty() {
            ("ok", "text_layer")
        } else {
            (ladder.status, ladder.source)
        };
        return make_result(cas_uuid, name, status, source, &text, pages, decrypted, ladder.lines);
    }

    if is_image {
        let ladder = ocr_images(std::slice::from_ref(&bytes)).await;
        return make_result(cas_uuid, name, ladder.status, ladder.source, &ladder.text, 1, false, ladder.lines);
    }

    // Plain-text attachments (e.g. QC dumps) still have usable content.
    let as_text = String::from_utf8_lossy(&bytes);
    if non_ws_len(&as_text) >= 50 {
        return make_result(cas_uuid, name, "ok", "text_layer", &as_text, 1, false, Vec::new());
    }

    make_result(cas_uuid, name, "ocr_unavailable", "", "", 0, false, Vec::new())
}

/// Assemble the tool-output result + the fields to persist from one ladder pass.
/// The tool output keeps its 6 000-char budget; the stored copy holds up to
/// 20 000 chars. On a failure status (`ocr_unavailable`/`encrypted`) the persisted
/// `extract_source` is the status itself (so the sweep caches the dead end);
/// otherwise it is the ladder source plus `+decrypted` when the PDF was decrypted.
#[allow(clippy::too_many_arguments)]
fn make_result(
    cas_uuid: &str,
    name: &str,
    status: &str,
    source: &str,
    raw_text: &str,
    pages: usize,
    decrypted: bool,
    lines: Vec<LineBox>,
) -> (OcrResult, StoredExtract) {
    let text = cap_text(raw_text, 6000);
    let quality = assess_quality(&text);
    let result = OcrResult {
        file_id: cas_uuid.to_string(),
        name: name.to_string(),
        status: status.to_string(),
        source: source.to_string(),
        text,
        quality,
        lines,
        pages,
        codec: None,
    };

    let store_text = cap_text(raw_text, 20_000);
    let store_quality = assess_quality(&store_text);
    let store_source = if status == "ok" {
        if decrypted { format!("{source}+decrypted") } else { source.to_string() }
    } else {
        // Failure cache: "ocr_unavailable" | "encrypted".
        status.to_string()
    };
    let fingerprint = extract_fingerprint(&store_text);
    (
        result,
        StoredExtract {
            text: store_text,
            quality: store_quality,
            source: store_source,
            pages: pages as i64,
            fingerprint,
        },
    )
}

/// Build the tool-output + stored-extract for a scan whose only page images use
/// a codec this build does not decode (JBIG2Decode / JPXDecode). Status is
/// `unsupported_codec`, `codec` names it, and the failure cache stores
/// `unsupported_codec:<codec>` so the sweep records the dead end (and
/// [`stored_to_result`] recovers both from it).
fn make_unsupported(cas_uuid: &str, name: &str, codec: &str) -> (OcrResult, StoredExtract) {
    let result = OcrResult {
        file_id: cas_uuid.to_string(),
        name: name.to_string(),
        status: "unsupported_codec".to_string(),
        source: String::new(),
        text: String::new(),
        quality: OcrQuality::empty(),
        lines: Vec::new(),
        pages: 0,
        codec: Some(codec.to_string()),
    };
    let stored = StoredExtract {
        text: String::new(),
        quality: OcrQuality::empty(),
        source: format!("unsupported_codec:{codec}"),
        pages: 0,
        fingerprint: None,
    };
    (result, stored)
}

/// Rebuild an [`OcrResult`] from the stored extract fields (fast path). Lines and
/// boxes are not persisted → empty. The stored text may be up to 20 000 chars but
/// the tool output keeps its 6 000-char budget, so it is re-capped here; the
/// status/source are recovered from the persisted `extract_source`.
fn stored_to_result(cas_uuid: &str, name: &str, stored_text: &str, stored_source: &str, stored_pages: i64) -> OcrResult {
    let base = stored_source.strip_suffix("+decrypted").unwrap_or(stored_source);
    let (status, source, codec): (&str, &str, Option<String>) =
        if let Some(c) = base.strip_prefix("unsupported_codec:") {
            ("unsupported_codec", "", Some(c.to_string()))
        } else {
            match base {
                "ocr_unavailable" | "encrypted" | "unsupported_codec" => (base, "", None),
                other => ("ok", other, None), // "text_layer" | "ocrs" | "external_cmd" | ""
            }
        };
    let text = cap_text(stored_text, 6000);
    let quality = assess_quality(&text);
    OcrResult {
        file_id: cas_uuid.to_string(),
        name: name.to_string(),
        status: status.to_string(),
        source: source.to_string(),
        text,
        quality,
        lines: Vec::new(),
        pages: stored_pages.max(0) as usize,
        codec,
    }
}

/// Persist an extract on the byte-owning `file_resource` row (addressed by the
/// `cas_uuid` FIELD, like the AVIF optimizer — `file_resource` mixes id
/// conventions). Fire-and-forget, log-on-error.
///
/// Mesh correctness (the `ai_summary` reference pattern, commit d4c8559): the new
/// `extracted_*` fields are NOT in `merkle::IGNORED_FIELDS`, so the live-watch
/// bridge folds them into the content hash automatically — this write only has to
/// advance THIS node's `_vclock` so the extract strictly dominates a peer's
/// extract-less copy (exactly what the summarization worker does after writing
/// `ai_summary`). `instance_id` comes from the process-wide sync engine; unset in
/// tests, in which case there is nothing to sync and the write is skipped.
async fn persist_extract(db: &SurrealDb, final_cas_uuid: &str, stored: &StoredExtract) {
    let Some(engine) = MESH.get() else { return };
    let iid = engine.instance_id().to_string();
    let quality = serde_json::to_value(&stored.quality).unwrap_or(Value::Null);

    let existing_vc: Option<Value> = db
        .query("SELECT VALUE _vclock FROM file_resource WHERE cas_uuid = $cid LIMIT 1")
        .bind(("cid", final_cas_uuid.to_string()))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();
    let next_vc = eck_core::sync::conflict::next_local_vclock(existing_vc.as_ref(), &iid);

    // MERGE with literal/bound values only (no cross-field SET expression) — the
    // 2026-07-22 greedy-SET/WHERE gotcha does not apply, same shape the AVIF
    // optimizer uses. `.check()` surfaces a statement-level error. The
    // `extract_fingerprint` clause is included only when we have one (empty text →
    // leave the field unset rather than write a null); it is NOT in
    // `merkle::IGNORED_FIELDS`, so the live-watch bridge folds it into the content
    // hash and the `_vclock` bump above carries it across the mesh with the rest.
    let fp_clause = if stored.fingerprint.is_some() { "extract_fingerprint: $fp, " } else { "" };
    let sql = format!(
        "UPDATE file_resource MERGE {{ \
            extracted_text: $text, \
            extracted_quality: $q, \
            extract_source: $src, \
            extractor_ver: $ver, \
            extracted_pages: $pages, \
            {fp_clause}\
            extracted_at: time::now(), \
            _vclock: $vc, \
            updated_at: time::now() \
         }} WHERE cas_uuid = $cid"
    );
    let mut q = db
        .query(sql)
        .bind(("text", stored.text.clone()))
        .bind(("q", quality))
        .bind(("src", stored.source.clone()))
        .bind(("ver", EXTRACTOR_VER))
        .bind(("pages", stored.pages))
        .bind(("vc", next_vc))
        .bind(("cid", final_cas_uuid.to_string()));
    if let Some(fp) = &stored.fingerprint {
        q = q.bind(("fp", fp.clone()));
    }
    let res = q.await;
    match res {
        Ok(r) => {
            if let Err(e) = r.check() {
                warn!("[attachments] persist extract {final_cas_uuid}: {e}");
            }
        }
        Err(e) => warn!("[attachments] persist extract {final_cas_uuid}: {e}"),
    }
}

// ── analyze_attachment_visual: policy-gated Gemini vision ───────────────────

/// A region of the page, relative (0..1), for the smallest-crop discipline the
/// vision tool asks the agent to follow.
#[derive(serde::Deserialize, Serialize, schemars::JsonSchema, Clone, Copy)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Vision-policy gate for scans leaving the node (see the module PPRL note).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Policy {
    Strict,
    Necessary,
    Full,
}

/// `ECK_ATTACHMENT_VISION_POLICY` = "strict" | "necessary" (default) | "full".
pub fn vision_policy() -> Policy {
    match std::env::var("ECK_ATTACHMENT_VISION_POLICY")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "strict" => Policy::Strict,
        "full" => Policy::Full,
        _ => Policy::Necessary,
    }
}

/// `ECK_VISION_CACHE_COS` — cosine floor for the semantic vision-journal cache.
/// Garbage / unset → 0.92 (a near-paraphrase of the same question on the same
/// page/region reuses the already-paid answer for free).
fn vision_cache_cos() -> f64 {
    std::env::var("ECK_VISION_CACHE_COS")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(0.92)
}

/// One prior vision-journal row, prepared for cache matching. `page_ok` /
/// `region_ok` are precomputed against the CURRENT call (same page; region
/// reusable = the prior is whole-page, or names the same region rounded to 2
/// decimals) so [`vision_cache_verdict`] stays a pure question-matcher.
/// `question_embedding` is present only for rows written since the semantic
/// cache landed; older rows are matched textually only. `question` /
/// `question_norm` / `answer` are already PPRL-masked (that is what the journal
/// stores), so a cache HIT returns the masked answer.
#[derive(Clone)]
struct PriorRow {
    question: String,
    answer: String,
    model: String,
    created_at: Value,
    question_norm: String,
    question_embedding: Option<Vec<f32>>,
    page_ok: bool,
    region_ok: bool,
}

/// Normalise a question for exact-match caching: trim, lowercase, collapse
/// internal whitespace to single spaces.
fn normalize_question(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    let mut pending_space = false;
    for c in q.trim().chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Stable key for a region, rounded to 2 decimals — the journal/id granularity.
/// `None` (whole page) → `"full"`.
fn region_key(region: Option<Region>) -> String {
    match region {
        None => "full".to_string(),
        Some(r) => format!("{:.2},{:.2},{:.2},{:.2}", r.x, r.y, r.w, r.h),
    }
}

/// Can a prior analysis over region `prior` answer a new call over region
/// `current`? A whole-page prior (`None`) answers anything on the page; a cropped
/// prior only answers the same rounded region (and never the whole page).
fn region_reusable(prior: Option<Region>, current: Option<Region>) -> bool {
    match prior {
        None => true,
        Some(_) => current.is_some() && region_key(prior) == region_key(current),
    }
}

/// Deterministic `vision_analysis` row id: `sha256(file|page|region|q_norm)[..20]`,
/// so the same analysis authored on two nodes converges to ONE meshed row.
fn vision_row_id(file: &str, page: u32, region: Option<Region>, q_norm: &str) -> String {
    use sha2::{Digest, Sha256};
    let pre = format!("{file}|{page}|{}|{q_norm}", region_key(region));
    hex::encode(Sha256::digest(pre.as_bytes()))[..20].to_string()
}

/// Cosine similarity of two equal-length vectors; 0.0 on shape mismatch / zero
/// magnitude (so a degenerate vector never spuriously "matches").
fn cosine_sim(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x as f64, *y as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Decide whether a prior journal row already answers the current question,
/// returning the index of the row to reuse (or `None` → pay for a fresh call).
/// Only rows flagged page-and-region eligible are considered. Two passes:
///   1. an EXACT normalised-question hit wins first (no embedding needed);
///   2. otherwise the closest semantic neighbour at cosine ≥ `threshold`, but
///      only when the current question is embedded (`q_emb`) AND the prior row
///      carries a stored embedding.
/// `prior` is newest-first, so among equal candidates the most recent wins.
fn vision_cache_verdict(
    prior: &[PriorRow],
    q_norm: &str,
    q_emb: Option<&[f32]>,
    threshold: f64,
) -> Option<usize> {
    for (i, row) in prior.iter().enumerate() {
        if row.page_ok && row.region_ok && row.question_norm == q_norm {
            return Some(i);
        }
    }
    let q_emb = q_emb?;
    let mut best: Option<(usize, f64)> = None;
    for (i, row) in prior.iter().enumerate() {
        if !(row.page_ok && row.region_ok) {
            continue;
        }
        let Some(emb) = row.question_embedding.as_deref() else {
            continue;
        };
        let sim = cosine_sim(q_emb, emb);
        if sim >= threshold && best.map_or(true, |(_, b)| sim > b) {
            best = Some((i, sim));
        }
    }
    best.map(|(i, _)| i)
}

/// Policy-gated Gemini vision over ONE attachment page, wrapped with the vision
/// journal (design 2026-07-28): every metered answer is a paid artifact that must
/// never be lost. Before any paid call this fetches up to the 5 most recent prior
/// analyses for the file (returned as `prior_analyses` so a caller can reuse an
/// already-paid answer) and, via [`vision_cache_verdict`], serves an identical or
/// near-identical earlier answer for the same page/region FOR FREE (no Gemini
/// call, no telemetry row). On a genuine miss it pays, masks the answer through
/// the ticket-text masker, and journals it to the mesh-synced `vision_analysis`
/// table under a deterministic id.
pub async fn analyze_visual(
    db: &SurrealDb,
    cas_uuid: &str,
    question: &str,
    region: Option<Region>,
    page: u32,
) -> Value {
    let http = reqwest::Client::new();
    // Mask the question up front: it is what we compare against the (already
    // masked) stored questions and what we journal. In the common case (no PII
    // in the question) masking is a no-op, so exact-match caching still fires.
    let q_masked = crate::ai::orchestrator::mask_free_text(&http, question).await;
    let q_norm = normalize_question(&q_masked);

    let prior = fetch_prior_rows(db, cas_uuid, page, region, 5).await;
    let prior_view: Vec<Value> = prior
        .iter()
        .map(|r| {
            json!({
                "question": r.question,
                "answer": r.answer.chars().take(300).collect::<String>(),
                "created_at": r.created_at.clone(),
            })
        })
        .collect();

    let threshold = vision_cache_cos();

    // Pass 1 — exact normalised-question hit needs no embedding.
    if let Some(i) = vision_cache_verdict(&prior, &q_norm, None, threshold) {
        return cached_response(&prior[i], &prior_view, false);
    }
    // Pass 2 — semantic. Only embed when at least one eligible prior can be
    // compared (a stored embedding exists); otherwise there is nothing to hit.
    let q_emb = if prior
        .iter()
        .any(|r| r.page_ok && r.region_ok && r.question_embedding.is_some())
    {
        crate::ai::embeddings::embed_query(question).await.ok()
    } else {
        None
    };
    if let Some(qe) = q_emb.as_deref() {
        if let Some(i) = vision_cache_verdict(&prior, &q_norm, Some(qe), threshold) {
            return cached_response(&prior[i], &prior_view, true);
        }
    }

    // Genuine miss → pay for the vision call.
    let mut out = analyze_visual_inner(db, cas_uuid, question, region, page).await;
    if out.get("status").and_then(|v| v.as_str()) == Some("ok") {
        // Mask the answer (and reuse the masked question) BEFORE journaling — the
        // raw answer is returned to THIS caller, but the journal (which meshes)
        // must never carry raw PII. Embed the question if we haven't already, so
        // every new row can be a semantic-cache candidate for peers.
        let raw_answer = out.get("answer").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let masked_answer = crate::ai::orchestrator::mask_free_text(&http, &raw_answer).await;
        let model = out.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let tokens_in = out.get("tokens_in").and_then(|v| v.as_i64()).unwrap_or(0);
        let tokens_out = out.get("tokens_out").and_then(|v| v.as_i64()).unwrap_or(0);
        let q_emb = match q_emb {
            Some(e) => Some(e),
            None => crate::ai::embeddings::embed_query(question).await.ok(),
        };
        write_vision_journal(
            db,
            cas_uuid,
            &q_masked,
            &q_norm,
            page,
            region,
            &masked_answer,
            &model,
            tokens_in,
            tokens_out,
            q_emb.as_deref(),
        )
        .await;
        // The vision_analysis journal above is the node-local cost record; it
        // was never counted toward the shared `ai_telemetry` spend the budget
        // circuit-breaker reads (telemetry::evaluate_budget), so vision calls
        // were a blind spot for the breaker. Same usage shape as the other
        // metered stages (see ai::image_summary's log_telemetry call).
        let usage = json!({
            "promptTokenCount": tokens_in,
            "candidatesTokenCount": tokens_out,
            "totalTokenCount": tokens_in + tokens_out,
        });
        crate::ai::telemetry::log_telemetry(db, "attachments_vision", &model, cas_uuid, &usage).await;
    }
    if let Some(obj) = out.as_object_mut() {
        obj.insert("prior_analyses".to_string(), Value::Array(prior_view));
    }
    out
}

/// Build the tool-output JSON for a cache HIT: the stored (masked) answer, zero
/// tokens, no telemetry. `semantic` distinguishes an exact reuse (`cached:true`)
/// from a paraphrase reuse (`cached:"semantic"` + `cached_question`).
fn cached_response(row: &PriorRow, prior_view: &[Value], semantic: bool) -> Value {
    let mut resp = json!({
        "status": "ok",
        "answer": row.answer,
        "model": row.model,
        "cached": if semantic { Value::String("semantic".to_string()) } else { Value::Bool(true) },
        "cached_from": row.created_at.clone(),
        "tokens_in": 0,
        "tokens_out": 0,
        "prior_analyses": Value::Array(prior_view.to_vec()),
    });
    if semantic {
        resp["cached_question"] = json!(row.question);
    }
    resp
}

/// The most recent journal rows for one file (newest first), each tagged with
/// page/region eligibility for the CURRENT call. Best-effort — a missing table /
/// read error yields an empty list.
async fn fetch_prior_rows(
    db: &SurrealDb,
    cas_uuid: &str,
    page: u32,
    region: Option<Region>,
    limit: i64,
) -> Vec<PriorRow> {
    let rows: Vec<Value> = db
        .query(
            "SELECT question, answer, model, page, region, question_embedding, created_at \
             FROM vision_analysis WHERE file = $f ORDER BY created_at DESC LIMIT $n",
        )
        .bind(("f", cas_uuid.to_string()))
        .bind(("n", limit))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .unwrap_or_default();
    rows.into_iter()
        .map(|r| {
            let question = r.get("question").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let answer = r.get("answer").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let model = r.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let row_page = r.get("page").and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as u32;
            let row_region: Option<Region> = r
                .get("region")
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value::<Region>(v.clone()).ok());
            let question_embedding: Option<Vec<f32>> = r
                .get("question_embedding")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect());
            PriorRow {
                question_norm: normalize_question(&question),
                question,
                answer,
                model,
                created_at: r.get("created_at").cloned().unwrap_or(Value::Null),
                question_embedding,
                page_ok: row_page == page,
                region_ok: region_reusable(row_region, region),
            }
        })
        .collect()
}

/// Append one metered vision answer to the mesh-synced `vision_analysis` journal
/// under a DETERMINISTIC id (so the same analysis from two nodes converges).
/// `question` / `answer` are already PPRL-masked by the caller. Advances THIS
/// node's `_vclock` so the row dominates a peer's copy (the `ai_summary`
/// reference pattern). Best-effort, log-on-error.
#[allow(clippy::too_many_arguments)]
async fn write_vision_journal(
    db: &SurrealDb,
    cas_uuid: &str,
    question_masked: &str,
    question_norm: &str,
    page: u32,
    region: Option<Region>,
    answer_masked: &str,
    model: &str,
    tokens_in: i64,
    tokens_out: i64,
    question_embedding: Option<&[f32]>,
) {
    let region_json = region
        .map(|r| json!({ "x": r.x, "y": r.y, "w": r.w, "h": r.h }))
        .unwrap_or(Value::Null);
    let emb_json = question_embedding
        .map(|e| json!(e))
        .unwrap_or(Value::Null);
    // `instance_id` from the process-wide sync engine; a `local` fallback keeps
    // the cost record (and its vclock) coherent in tests / before mesh install.
    let iid = mesh_instance_id().unwrap_or_else(|| "local".to_string());
    let id = vision_row_id(cas_uuid, page, region, question_norm);
    let trid = format!("vision_analysis:`{}`", id);
    let existing_vc: Option<Value> = db
        .query("SELECT VALUE _vclock FROM type::record($trid) LIMIT 1")
        .bind(("trid", trid.clone()))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();
    let next_vc = eck_core::sync::conflict::next_local_vclock(existing_vc.as_ref(), &iid);
    let res = db
        .query(
            "UPSERT type::record($trid) CONTENT { \
                file: $f, question: $q, question_norm: $qn, page: $p, region: $r, \
                answer: $a, model: $m, tokens_in: $ti, tokens_out: $to, \
                question_embedding: $qe, created_at: time::now(), _vclock: $vc \
            }",
        )
        .bind(("trid", trid))
        .bind(("f", cas_uuid.to_string()))
        .bind(("q", question_masked.to_string()))
        .bind(("qn", question_norm.to_string()))
        .bind(("p", page))
        .bind(("r", region_json))
        .bind(("a", answer_masked.to_string()))
        .bind(("m", model.to_string()))
        .bind(("ti", tokens_in))
        .bind(("to", tokens_out))
        .bind(("qe", emb_json))
        .bind(("vc", next_vc))
        .await;
    if let Err(e) = res {
        warn!("[attachments] vision journal write failed for {cas_uuid}: {e}");
    }
}

/// Load the requested page, optionally crop to a region, downscale to JPEG and
/// send it with `question` to Gemini vision. Returns the tool-output JSON.
/// Policy is checked FIRST — `strict` refuses without ever touching the file.
async fn analyze_visual_inner(
    db: &SurrealDb,
    cas_uuid: &str,
    question: &str,
    region: Option<Region>,
    page: u32,
) -> Value {
    if vision_policy() == Policy::Strict {
        return json!({
            "status": "denied_by_policy",
            "hint": "policy=strict: use ask_human to have an operator read the document",
        });
    }
    // A vision call is billed like any generate hop — honor the circuit breaker.
    if crate::ai::telemetry::current_budget_level() == crate::ai::telemetry::BudgetLevel::Halt {
        return json!({ "status": "budget_halt", "hint": "AI budget exhausted — vision suspended" });
    }

    let (abs_path, mime, _name) = match resolve_file(db, cas_uuid).await {
        Ok(v) => v,
        Err(_) => return json!({ "status": "not_found" }),
    };
    let bytes = match tokio::fs::read(&abs_path).await {
        Ok(b) => b,
        Err(_) => return json!({ "status": "not_found" }),
    };

    // Owner-password PDFs (empty user password) decrypt transparently first, so
    // their pages rasterize like any plaintext PDF.
    let (bytes, _decrypted) = maybe_decrypt_pdf(bytes).await;

    // Note encryption before the bytes move into the blocking closure — an
    // owner-password PDF has no rasterizable page either, but for a different
    // reason the caller must hear. After the decrypt attempt above this is only
    // still true when a real user password blocked it (the `/Encrypt` trailer
    // survives), which is exactly what the encrypted hint should report.
    let is_encrypted_pdf =
        (mime.contains("pdf") || bytes.starts_with(b"%PDF")) && pdf_is_encrypted(&bytes);

    // Prepare the page ENTIRELY on the blocking pool: render (or fall back to the
    // embedded image), and for a PDF resolve the AcroForm block so a fillable
    // form is never analysed as if its logo were the page.
    let prep = tokio::task::spawn_blocking(move || prepare_vision_input(&bytes, &mime, page, region))
        .await;
    let (jpeg, context) = match prep {
        Ok(VisionPrep::Image { jpeg, context }) => (jpeg, context),
        Ok(VisionPrep::NoRender { context }) => {
            if is_encrypted_pdf {
                return json!({
                    "status": "no_image",
                    "hint": "PDF is encrypted (owner-password) — no text/vision extraction possible; the bytes are intact, use export_attachment",
                });
            }
            // A fillable form whose page could not be rendered: say so, and hand
            // the brain the field values instead of silently analysing the logo.
            return match context {
                Some(block) => vision_form_note(&block),
                None => json!({
                    "status": "no_image",
                    "hint": "no rasterizable page (vector/text PDF or unsupported format) — use ocr_attachment for the text layer",
                }),
            };
        }
        Ok(VisionPrep::Err(e)) => return json!({ "status": "error", "error": format!("image prep failed: {e}") }),
        Err(_) => return json!({ "status": "error", "error": "image prep task panicked" }),
    };

    let http = reqwest::Client::new();
    let auth = match eck_core::ai::AiAuth::resolve(&http).await {
        Ok(a) if a.is_configured() => a,
        _ => return json!({ "status": "ai_unavailable", "hint": "AI auth not configured on this node" }),
    };

    let prompt = format!(
        "You are reading a scanned document page image from a support ticket. Answer strictly \
         from what is visible in the image; if the answer is not visible, say so explicitly.\n\n\
         Question: {question}"
    );
    match vision_generate(&auth, &http, &prompt, &jpeg).await {
        Ok((answer, model, usage)) => {
            let mut out = json!({
                "status": "ok",
                "answer": answer,
                "model": model,
                "tokens_in": usage.get("promptTokenCount").and_then(|v| v.as_i64()),
                "tokens_out": usage.get("candidatesTokenCount").and_then(|v| v.as_i64()),
            });
            // Filled form: also surface the exact typed field values so the brain
            // can cross-check the rendered page (customer address / PLZ / Ort).
            if let Some(block) = context {
                out["context"] = Value::String(block);
            }
            out
        }
        Err(e) => json!({ "status": "error", "error": format!("vision call failed: {e}") }),
    }
}

/// Outcome of preparing one attachment page for the vision call (all the
/// CPU-bound decode/render/encode work, run on a blocking task).
enum VisionPrep {
    /// A page image ready to send, plus (for a PDF form) the `--- Formularfelder
    /// ---` block to pass as `context`.
    Image { jpeg: Vec<u8>, context: Option<String> },
    /// No page image could be produced. `context` is `Some(block)` when the PDF
    /// is a fillable form (→ note + field values), `None` otherwise (→ plain
    /// no_image hint).
    NoRender { context: Option<String> },
    Err(String),
}

/// Decode/render the requested page and JPEG-encode it, deciding how a PDF form
/// is handled: a `Rendered` page is analysed directly; an `Embedded` image on a
/// PDF that HAS AcroForm fields is the logo (not the filled page) and is refused
/// in favour of the form-field block; anything with no image degrades to
/// [`VisionPrep::NoRender`]. The AcroForm block, when present, always rides along
/// as `context`. Synchronous — call on a blocking pool.
fn prepare_vision_input(bytes: &[u8], mime: &str, page: u32, region: Option<Region>) -> VisionPrep {
    let is_pdf = mime.contains("pdf") || bytes.starts_with(b"%PDF");
    let form_block = || {
        if is_pdf {
            let b = format_acroform_block(&extract_acroform_fields(bytes));
            (!b.is_empty()).then_some(b)
        } else {
            None
        }
    };
    match decode_page_sourced(bytes, mime, page) {
        Ok(Some(PageSource::Rendered(img))) => match encode_vision_jpeg(img, region) {
            Ok(jpeg) => VisionPrep::Image { jpeg, context: form_block() },
            Err(e) => VisionPrep::Err(format!("{e}")),
        },
        Ok(Some(PageSource::Embedded(img))) => {
            // On a fillable form the only embedded image is the logo — refuse it.
            if let Some(block) = form_block() {
                return VisionPrep::NoRender { context: Some(block) };
            }
            match encode_vision_jpeg(img, region) {
                Ok(jpeg) => VisionPrep::Image { jpeg, context: None },
                Err(e) => VisionPrep::Err(format!("{e}")),
            }
        }
        Ok(None) => VisionPrep::NoRender { context: form_block() },
        Err(e) => VisionPrep::Err(format!("{e}")),
    }
}

/// Crop to the requested region (if any) and JPEG-encode at the policy max side
/// (768 full page / 1024 for a region crop).
fn encode_vision_jpeg(img: DynamicImage, region: Option<Region>) -> Result<Vec<u8>> {
    let (img, max_side) = match region {
        Some(r) => (crop_relative(&img, r.x, r.y, r.w, r.h), 1024u32),
        None => (img, 768u32),
    };
    downscale_jpeg(&img, max_side)
}

/// The `no_image` result for a fillable-form PDF whose page could not be
/// rendered: an honest `note` plus the `--- Formularfelder ---` block as
/// `context`, so the brain still gets the customer's address / PLZ / Ort even
/// though there is no page image to analyse.
fn vision_form_note(context_block: &str) -> Value {
    json!({
        "status": "no_image",
        "note": "page not rendered: vector PDF without embedded page image; form field values are available via ocr_attachment",
        "context": context_block,
    })
}

/// Decode raster bytes to a `DynamicImage`, routing AVIF (which `image`'s
/// encode-only `avif` feature cannot read in this build) through the pure-Rust
/// [`crate::ai::avif`] decoder, and everything else through `image`. Detection is
/// by mime OR magic bytes, because CAS rows routinely carry `application/octet-
/// stream` for both optimizer-produced and native `.avif` files.
pub(crate) fn decode_image_bytes(bytes: &[u8], mime: &str) -> Result<DynamicImage> {
    if mime == "image/avif" || crate::ai::avif::looks_like_avif(bytes) {
        crate::ai::avif::decode_avif(bytes)
    } else {
        image::load_from_memory(bytes).context("decode image")
    }
}

// ── PDF page rendering (full-page rasterizer) ────────────────────────────────
//
// A fillable service form (the tenant's "Reparaturauftrag") is VECTOR text plus
// AcroForm field appearances; the only embedded image XObject is usually the
// logo. So [`decode_pdf_page_image`] (the XObject decoder) returns the logo, not
// the page the customer filled in — the vision model would "see" a logo. The fix
// is to RENDER the page. Two rungs, tried in order, both optional so a minimal
// build / a locked-down node still degrades honestly:
//   1. External `ECK_PDF_RENDER_CMD` (e.g. `pdftoppm`, `mutool draw`) — same
//      spirit as the `ECK_OCR_CMD` rung. The kiosk has none of these and no root
//      to install them, so this is for nodes that DO ship a renderer.
//   2. Pure-Rust `hayro` (feature `pdf-raster`, default-on) — CPU-only, no C
//      library, embedded standard fonts. This is the rung the kiosk actually
//      uses.
// When NEITHER is available, [`pdf_render_available`] is false and the vision
// path says so instead of silently analysing the logo.

/// Render one PDF page (0-based) to a bitmap via the external renderer first,
/// then the pure-Rust rasterizer. `None` when no renderer is available or the
/// render produced nothing. Synchronous / CPU-bound — call it on a blocking pool.
fn render_pdf_page(bytes: &[u8], page: u32) -> Option<DynamicImage> {
    if let Ok(cmd) = std::env::var("ECK_PDF_RENDER_CMD") {
        let cmd = cmd.trim().to_string();
        if !cmd.is_empty() {
            match render_pdf_page_external(&cmd, bytes, page) {
                Some(img) => return Some(img),
                None => warn!("[attachments] ECK_PDF_RENDER_CMD produced no page image"),
            }
        }
    }
    render_pdf_page_rust(bytes, page)
}

/// External-renderer rung. The command template is expanded with these
/// placeholders and run to completion; the produced PNG is then read back:
///   * `{in}`        — a temp path holding the PDF bytes.
///   * `{page}`      — the 1-based page number.
///   * `{outprefix}` — an output path WITHOUT extension.
///   * `{out}`       — `{outprefix}.png` (a full output path).
/// Recommended recipes (single deterministic output file):
///   `pdftoppm -r 110 -f {page} -l {page} -png -singlefile {in} {outprefix}`
///   `mutool draw -r 110 -o {out} {in} {page}`
/// After the run we read `{out}` if present, else the first image file that
/// appears next to `{outprefix}` (pdftoppm without `-singlefile` suffixes the
/// page number). Blocking; runs inside a blocking task via [`render_pdf_page`].
fn render_pdf_page_external(cmd_tmpl: &str, bytes: &[u8], page: u32) -> Option<DynamicImage> {
    let dir = std::env::temp_dir();
    let stem = format!("eck_pdfrender_{}", uuid::Uuid::new_v4());
    let in_path = dir.join(format!("{stem}.pdf"));
    let out_prefix = dir.join(&stem);
    let out_path = dir.join(format!("{stem}.png"));
    if std::fs::write(&in_path, bytes).is_err() {
        return None;
    }
    let subst = |s: &str| -> String {
        s.replace("{in}", &in_path.to_string_lossy())
            .replace("{page}", &(page + 1).to_string())
            .replace("{outprefix}", &out_prefix.to_string_lossy())
            .replace("{out}", &out_path.to_string_lossy())
    };
    let parts: Vec<String> = cmd_tmpl.split_whitespace().map(subst).collect();
    let img = (|| {
        let (prog, rest) = parts.split_first()?;
        let status = std::process::Command::new(prog)
            .args(rest)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        // Prefer the exact {out}; else the first sibling file sharing the stem.
        let produced: Option<PathBuf> = if out_path.exists() {
            Some(out_path.clone())
        } else {
            std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(&stem) && n != format!("{stem}.pdf"))
                    .unwrap_or(false)
            })
        };
        let data = std::fs::read(produced?).ok()?;
        image::load_from_memory(&data).ok()
    })();
    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
    img
}

/// Pure-Rust rasterizer rung (`hayro`, feature `pdf-raster`). Renders at 2× the
/// native 72-dpi page (~144 dpi): enough for the 768 px vision downscale to keep
/// filled form text legible. Panic-safe (same discipline as the lopdf walkers)
/// and gated: with the feature off this is a no-op returning `None`.
#[cfg(feature = "pdf-raster")]
fn render_pdf_page_rust(bytes: &[u8], page: u32) -> Option<DynamicImage> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::Pdf;
    use hayro::vello_cpu::color::palette::css::WHITE;
    use hayro::{render, RenderCache, RenderSettings};

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let pdf = Pdf::new(bytes.to_vec()).ok()?;
        let pages = pdf.pages();
        let pg = pages.get(page as usize)?;
        let cache = RenderCache::new();
        let settings = InterpreterSettings::default();
        let render_settings =
            RenderSettings { x_scale: 2.0, y_scale: 2.0, bg_color: WHITE, ..Default::default() };
        let pixmap = render(pg, &cache, &settings, &render_settings);
        let (w, h) = (pixmap.width() as u32, pixmap.height() as u32);
        if w == 0 || h == 0 {
            return None;
        }
        // vello_cpu hands us premultiplied RGBA8; composite over white (the
        // render background) to a straight RGB bitmap the vision encoder wants.
        let data = pixmap.data_as_u8_slice();
        let mut rgb = Vec::with_capacity((w as usize) * (h as usize) * 3);
        for px in data.chunks_exact(4) {
            let inv = 255u8.saturating_sub(px[3]);
            rgb.push(px[0].saturating_add(inv));
            rgb.push(px[1].saturating_add(inv));
            rgb.push(px[2].saturating_add(inv));
        }
        image::RgbImage::from_raw(w, h, rgb).map(DynamicImage::ImageRgb8)
    }));
    res.ok().flatten()
}

#[cfg(not(feature = "pdf-raster"))]
fn render_pdf_page_rust(_bytes: &[u8], _page: u32) -> Option<DynamicImage> {
    None
}

/// Where a decoded page bitmap came from — the vision path treats these
/// differently. A `Rendered` page is the real filled page; an `Embedded` image
/// is a scan (good) OR, for a fillable form, just the logo (must not be analysed
/// as if it were the page — see [`analyze_visual_inner`]).
pub(crate) enum PageSource {
    Rendered(DynamicImage),
    Embedded(DynamicImage),
}

/// Decode one page, reporting its SOURCE. For a PDF: render the whole page first
/// (`Rendered`), else fall back to the embedded image XObject / one-JPEG-per-page
/// scan grep (`Embedded`). A non-PDF image is `Embedded`. `Ok(None)` when nothing
/// is renderable/extractable.
pub(crate) fn decode_page_sourced(
    bytes: &[u8],
    mime: &str,
    page: u32,
) -> Result<Option<PageSource>> {
    let is_pdf = mime.contains("pdf") || bytes.starts_with(b"%PDF");
    if is_pdf {
        // 1. Full page render (vector text + form field appearances visible).
        if let Some(img) = render_pdf_page(bytes, page) {
            return Ok(Some(PageSource::Rendered(img)));
        }
        // 2. Embedded image XObject (CCITT G3/G4, DCT JPEG, raw Flate bitmap).
        if let Some(img) = decode_pdf_page_image(bytes, page as usize) {
            return Ok(Some(PageSource::Embedded(img)));
        }
        // 3. Our own scan ingest wraps one raw JPEG per page.
        let streams = extract_pdf_jpeg_streams(bytes, page as usize + 1);
        match streams.get(page as usize) {
            Some(b) => Ok(Some(PageSource::Embedded(
                image::load_from_memory(b).context("decode PDF page image")?,
            ))),
            None => Ok(None),
        }
    } else if mime.starts_with("image/") || looks_like_image(bytes) {
        Ok(Some(PageSource::Embedded(decode_image_bytes(bytes, mime)?)))
    } else {
        Ok(None)
    }
}

/// Decode one page to a bitmap regardless of source (page render, else embedded
/// image). `Ok(None)` when there is no image to render. Thin flatten over
/// [`decode_page_sourced`] for callers that don't care where the pixels came
/// from (the passthrough re-render in `attachment_image`).
pub(crate) fn decode_page(bytes: &[u8], mime: &str, page: u32) -> Result<Option<DynamicImage>> {
    Ok(decode_page_sourced(bytes, mime, page)?.map(|s| match s {
        PageSource::Rendered(img) | PageSource::Embedded(img) => img,
    }))
}

// ── Image ops ───────────────────────────────────────────────────────────────

/// Downscale so the long side ≤ `max_side` and JPEG-encode at ~q80.
pub fn downscale_jpeg(img: &DynamicImage, max_side: u32) -> Result<Vec<u8>> {
    let (w, h) = (img.width(), img.height());
    let long = w.max(h);
    let scaled;
    let src: &DynamicImage = if long > max_side && long > 0 {
        let ratio = max_side as f32 / long as f32;
        let nw = ((w as f32 * ratio).round() as u32).max(1);
        let nh = ((h as f32 * ratio).round() as u32).max(1);
        scaled = img.resize_exact(nw, nh, image::imageops::FilterType::Triangle);
        &scaled
    } else {
        img
    };
    let rgb = DynamicImage::ImageRgb8(src.to_rgb8());
    let mut out = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
    rgb.write_with_encoder(encoder).context("jpeg encode")?;
    Ok(out.into_inner())
}

/// Crop to a relative (0..1) region, clamped to the image bounds.
pub fn crop_relative(img: &DynamicImage, x: f32, y: f32, w: f32, h: f32) -> DynamicImage {
    let (iw, ih) = (img.width(), img.height());
    let cx = (x.clamp(0.0, 1.0) * iw as f32).round() as u32;
    let cy = (y.clamp(0.0, 1.0) * ih as f32).round() as u32;
    let cw = ((w.clamp(0.0, 1.0) * iw as f32).round() as u32).max(1).min(iw.saturating_sub(cx));
    let ch = ((h.clamp(0.0, 1.0) * ih as f32).round() as u32).max(1).min(ih.saturating_sub(cy));
    img.crop_imm(cx, cy, cw.max(1), ch.max(1))
}

// ── Multimodal generate (one inline image part) ─────────────────────────────

/// Send ONE inline image + a text prompt to the EXISTING Gemini generate path.
/// Builds the multimodal `contents` (text part + `inline_data` image part) and
/// calls `generate_content`, so usage metering rides the identical report path
/// as every text-only generate call. Model = `GEMINI_VISION_MODEL`, defaulting
/// to `GEMINI_GENERATION_MODEL` (gemini-3.5-flash-lite). Returns `(answer,
/// model, usage)`.
pub async fn vision_generate(
    auth: &eck_core::ai::AiAuth,
    http: &reqwest::Client,
    prompt: &str,
    jpeg: &[u8],
) -> Result<(String, String, Value)> {
    let model = std::env::var("GEMINI_VISION_MODEL")
        .or_else(|_| std::env::var("GEMINI_GENERATION_MODEL"))
        .unwrap_or_else(|_| "gemini-3.5-flash-lite".to_string());
    let body = json!({
        "contents": [{
            "role": "user",
            "parts": [
                { "text": prompt },
                { "inline_data": { "mime_type": "image/jpeg", "data": B64.encode(jpeg) } }
            ]
        }],
        "generationConfig": { "temperature": 0.2, "maxOutputTokens": 1024 }
    });
    let _ = auth; // caller resolved auth only to gate on is_configured()
    let gen =
        eck_core::ai::AiAuth::generate_for_task(http, "attachments", &model, body, None).await?;
    tracing::info!("[ai] task=attachments path={} model={}", gen.path, gen.model);
    Ok((gen.text, gen.model, gen.usage))
}

// ── Small helpers ───────────────────────────────────────────────────────────

fn non_ws_len(s: &str) -> usize {
    s.chars().filter(|c| !c.is_whitespace()).count()
}

fn cap_text(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    s[..crate::ai::floor_char_boundary(s, max_bytes)].to_string()
}

/// Magic-byte sniff for the common raster formats (mime may be octet-stream).
fn looks_like_image(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF]) // JPEG
        || b.starts_with(&[0x89, 0x50, 0x4E, 0x47]) // PNG
        || b.starts_with(b"GIF8") // GIF
        || (b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP") // WebP
        || crate::ai::avif::looks_like_avif(b) // AVIF (brand-checked)
        || (b.len() > 12 && &b[4..8] == b"ftyp") // other HEIF/ISOBMFF family
}

fn image_ext(b: &[u8]) -> &'static str {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if b.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "png"
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "webp"
    } else {
        "png"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_flags_clean_vs_garbage() {
        let clean = assess_quality("Sehr geehrte Damen und Herren das Gerät startet nicht");
        assert!(clean.dictionary_ratio > 0.7, "ratio={}", clean.dictionary_ratio);
        assert!(clean.garbage_ratio < 0.2);

        let junk = assess_quality("|#~ @@@ ]{[ *^* ///");
        assert!(junk.dictionary_ratio < 0.2);
        assert!(junk.garbage_ratio > 0.6, "ratio={}", junk.garbage_ratio);
    }

    #[test]
    fn quality_empty_is_zeroed() {
        let q = assess_quality("");
        assert_eq!(q.chars, 0);
        assert_eq!(q.dictionary_ratio, 0.0);
        assert_eq!(q.garbage_ratio, 0.0);
    }

    #[test]
    fn extract_jpeg_streams_finds_soi_payloads() {
        // Two minimal `stream…endstream` chunks: one JPEG (FF D8 FF), one not.
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        pdf.extend_from_slice(b"stream\n");
        pdf.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0x11, 0x22, 0x33]);
        pdf.extend_from_slice(b"\nendstream\n");
        pdf.extend_from_slice(b"stream\n");
        pdf.extend_from_slice(b"plain text not an image");
        pdf.extend_from_slice(b"\nendstream\n");
        let streams = extract_pdf_jpeg_streams(&pdf, 4);
        assert_eq!(streams.len(), 1);
        assert_eq!(&streams[0][0..3], &[0xFF, 0xD8, 0xFF]);
    }

    #[test]
    fn vision_policy_parses_env() {
        std::env::set_var("ECK_ATTACHMENT_VISION_POLICY", "strict");
        assert_eq!(vision_policy(), Policy::Strict);
        std::env::set_var("ECK_ATTACHMENT_VISION_POLICY", "FULL");
        assert_eq!(vision_policy(), Policy::Full);
        std::env::set_var("ECK_ATTACHMENT_VISION_POLICY", "");
        assert_eq!(vision_policy(), Policy::Necessary);
        std::env::remove_var("ECK_ATTACHMENT_VISION_POLICY");
        assert_eq!(vision_policy(), Policy::Necessary);
    }

    #[test]
    fn cap_text_respects_char_boundary() {
        let s = format!("{}ö", "a".repeat(5999)); // 'ö' straddles byte 6000
        let capped = cap_text(&s, 6000);
        assert!(capped.len() <= 6000);
        let _ = capped; // must not panic on the multibyte boundary
    }

    // A valid RC4/AES owner-password fixture can't be built in-code (lopdf only
    // decrypts), so these lock the contract that matters instead: on the two
    // non-decryptable paths `maybe_decrypt_pdf` returns the ORIGINAL bytes byte
    // for byte, so the caller's ladder and its `encrypted` fallback still apply.
    #[tokio::test]
    async fn maybe_decrypt_pdf_passes_unencrypted_through() {
        let pdf = b"%PDF-1.4\n1 0 obj<< >>endobj\n%%EOF".to_vec();
        let (out, decrypted) = maybe_decrypt_pdf(pdf.clone()).await; // no /Encrypt → early return
        assert_eq!(out, pdf);
        assert!(!decrypted, "unencrypted bytes are not marked decrypted");
    }

    #[tokio::test]
    async fn maybe_decrypt_pdf_returns_original_on_parse_failure() {
        // Mentions /Encrypt (so the guard fires) but is not a parseable PDF, so
        // lopdf load/decrypt fails and we must hand back the untouched bytes.
        let junk = b"%PDF-1.4 /Encrypt garbage not a real pdf".to_vec();
        let (out, decrypted) = maybe_decrypt_pdf(junk.clone()).await;
        assert_eq!(out, junk);
        assert!(!decrypted, "an undecryptable file is not marked decrypted");
    }

    // ── Persistent extract: fast-path rebuild + extract_source composition ────

    #[test]
    fn stored_to_result_rebuilds_ok_extract() {
        // A stored "ok" extract rebuilds to status=ok, the base source, the text
        // (re-capped to the 6000 tool budget), and empty lines (never stored).
        let r = stored_to_result("cas1", "scan.pdf", "Hello Welt line one", "text_layer", 3);
        assert_eq!(r.status, "ok");
        assert_eq!(r.source, "text_layer");
        assert_eq!(r.text, "Hello Welt line one");
        assert_eq!(r.pages, 3);
        assert!(r.lines.is_empty(), "lines/boxes are not persisted");
        assert_eq!(r.file_id, "cas1");
        assert!(r.quality.chars > 0);
    }

    #[test]
    fn stored_to_result_strips_decrypted_suffix() {
        // "+decrypted" is persistence metadata, not part of the tool-output source.
        let r = stored_to_result("cas2", "enc.pdf", "text here", "text_layer+decrypted", 1);
        assert_eq!(r.status, "ok");
        assert_eq!(r.source, "text_layer");
    }

    #[test]
    fn stored_to_result_rebuilds_failure_stamp() {
        // A cached failure ("ocr_unavailable"/"encrypted") rebuilds to that status
        // with an empty source and no text — identical shape to a fresh failure.
        let enc = stored_to_result("cas3", "locked.pdf", "", "encrypted", 0);
        assert_eq!(enc.status, "encrypted");
        assert_eq!(enc.source, "");
        assert_eq!(enc.text, "");
        assert_eq!(enc.pages, 0);
        assert_eq!(enc.quality.chars, 0);

        let un = stored_to_result("cas4", "blob.bin", "", "ocr_unavailable", 0);
        assert_eq!(un.status, "ocr_unavailable");
        assert_eq!(un.source, "");
    }

    #[test]
    fn make_result_persists_full_text_and_source() {
        // The tool output keeps its 6000-char budget while the stored copy holds
        // up to 20000, and a decrypted "ok" extract records the "+decrypted" suffix.
        let raw = "x".repeat(9000);
        let (result, stored) = make_result("cas5", "big.pdf", "ok", "text_layer", &raw, 2, true, Vec::new());
        assert_eq!(result.text.len(), 6000, "tool output stays at the 6000 budget");
        assert_eq!(stored.text.len(), 9000, "full text (<=20000) is stored");
        assert_eq!(stored.source, "text_layer+decrypted");
        assert_eq!(stored.pages, 2);
        // The tool-output source never carries the +decrypted suffix.
        assert_eq!(result.source, "text_layer");
    }

    #[test]
    fn make_result_failure_stamps_status_as_source() {
        // On a failure status the persisted extract_source is the status itself,
        // so the sweep caches the dead end and doesn't retry it every tick.
        let (result, stored) = make_result("cas6", "x.bin", "ocr_unavailable", "", "", 0, false, Vec::new());
        assert_eq!(result.status, "ocr_unavailable");
        assert_eq!(result.source, "");
        assert_eq!(stored.source, "ocr_unavailable");
        assert_eq!(stored.text, "");
        assert_eq!(stored.pages, 0);
    }

    #[test]
    fn make_result_capped_stored_text_is_char_safe() {
        // A multibyte char straddling the 20000-byte store boundary must not panic.
        let raw = format!("{}ö{}", "a".repeat(19_999), "b".repeat(2000));
        let (_r, stored) = make_result("cas7", "n.txt", "ok", "text_layer", &raw, 1, false, Vec::new());
        assert!(stored.text.len() <= 20_000);
    }

    // ── CCITTFaxDecode office-scan decode (EXTRACTOR_VER 2) ───────────────────

    /// Hermetic CCITT Group 4 round-trip: encode a known 16×8 bitmap (left half
    /// white, right half black) with the `fax` encoder, wrap it in a synthetic
    /// image-XObject dictionary, and assert [`decode_ccitt_image`] reproduces the
    /// pattern with the correct grayscale orientation — the same real decode path
    /// office scans hit. Also pins the BlackIs1 inversion contract.
    #[test]
    fn ccitt_g4_roundtrip_decodes_pattern() {
        use fax::encoder::Encoder;
        use fax::{Color, VecWriter};

        let width: u16 = 16;
        let mut enc = Encoder::new(VecWriter::new());
        for _ in 0..8 {
            let line = (0..width).map(|c| if c < 8 { Color::White } else { Color::Black });
            enc.encode_line(line, width).unwrap();
        }
        let g4: Vec<u8> = enc.finish().unwrap().finish();

        let mut parms = Dictionary::new();
        parms.set("K", -1i64);
        parms.set("Columns", 16i64);
        parms.set("Rows", 8i64);
        let mut img = Dictionary::new();
        img.set("Width", 16i64);
        img.set("Height", 8i64);
        img.set("BitsPerComponent", 1i64);
        img.set("ColorSpace", "DeviceGray");
        img.set("Filter", "CCITTFaxDecode");
        img.set("DecodeParms", parms);

        let doc = Document::new();
        let out = decode_ccitt_image(&doc, &img, &g4, 0).expect("CCITT G4 decode");
        let gray = out.to_luma8();
        assert_eq!(gray.dimensions(), (16, 8));
        // Default BlackIs1=false: paper (decoder-white) → 255, ink (black) → 0.
        assert_eq!(gray.get_pixel(0, 0).0[0], 255, "left half is white paper");
        assert_eq!(gray.get_pixel(7, 3).0[0], 255);
        assert_eq!(gray.get_pixel(8, 0).0[0], 0, "right half is black ink");
        assert_eq!(gray.get_pixel(15, 7).0[0], 0);

        // BlackIs1=true inverts the sample sense.
        let mut parms_b1 = Dictionary::new();
        parms_b1.set("K", -1i64);
        parms_b1.set("Columns", 16i64);
        parms_b1.set("Rows", 8i64);
        parms_b1.set("BlackIs1", true);
        img.set("DecodeParms", parms_b1);
        let inv = decode_ccitt_image(&doc, &img, &g4, 0).expect("CCITT decode BlackIs1").to_luma8();
        assert_eq!(inv.get_pixel(0, 0).0[0], 0, "BlackIs1 flips white→0");
        assert_eq!(inv.get_pixel(8, 0).0[0], 255, "BlackIs1 flips black→255");
    }

    /// 1-bit DeviceGray sample unpack: MSB-first, row-padded, bit 1 → white.
    #[test]
    fn unpack_1bpc_gray_msb_first() {
        // 12px wide (2 bytes/row, 4 padding bits), 2 rows. Row0 = 1010... , Row1 = 0.
        let data = [0b1010_1010u8, 0b1010_0000, 0b0000_0000, 0b0000_0000];
        let img = unpack_1bpc_gray(&data, 12, 2).expect("unpack");
        assert_eq!(img.dimensions(), (12, 2));
        assert_eq!(img.get_pixel(0, 0).0[0], 255); // bit 1
        assert_eq!(img.get_pixel(1, 0).0[0], 0); // bit 0
        assert_eq!(img.get_pixel(0, 1).0[0], 0); // whole row 0
    }

    #[test]
    fn colorspace_name_components() {
        assert_eq!(name_cs_components(b"DeviceGray"), Some(1));
        assert_eq!(name_cs_components(b"DeviceRGB"), Some(3));
        assert_eq!(name_cs_components(b"G"), Some(1));
        assert_eq!(name_cs_components(b"DeviceCMYK"), None);
    }

    #[test]
    fn extract_fingerprint_ignores_case_and_whitespace() {
        // Same words, differing only in case + whitespace runs + edge padding →
        // identical fingerprint (clusters re-uploaded copies of the same form).
        let a = extract_fingerprint("Sehr   geehrte\tDamen\nund  HERREN").unwrap();
        let b = extract_fingerprint("  sehr geehrte damen und herren  ").unwrap();
        assert_eq!(a, b, "case/whitespace-only differences must fingerprint identically");
        // Different content → different fingerprint.
        let c = extract_fingerprint("sehr geehrte damen und herren!").unwrap();
        assert_ne!(a, c, "different text must fingerprint differently");
        // Empty / whitespace-only → no fingerprint.
        assert!(extract_fingerprint("").is_none());
        assert!(extract_fingerprint("   \n\t ").is_none());
    }

    /// The `unsupported_codec:<name>` failure stamp round-trips through the stored
    /// extract: `make_unsupported` writes it, `stored_to_result` recovers status
    /// and the codec name.
    #[test]
    fn unsupported_codec_stamp_roundtrips() {
        let (result, stored) = make_unsupported("cas9", "scan.pdf", "JBIG2Decode");
        assert_eq!(result.status, "unsupported_codec");
        assert_eq!(result.codec.as_deref(), Some("JBIG2Decode"));
        assert_eq!(stored.source, "unsupported_codec:JBIG2Decode");

        let rebuilt = stored_to_result("cas9", "scan.pdf", "", &stored.source, 0);
        assert_eq!(rebuilt.status, "unsupported_codec");
        assert_eq!(rebuilt.codec.as_deref(), Some("JBIG2Decode"));
        assert_eq!(rebuilt.source, "");
    }

    // ── AcroForm field extraction (EXTRACTOR_VER 3) ───────────────────────────

    /// A PDF literal string object (for `/T` names and text `/V` values — a bare
    /// `&str` would become a `Name`, which the walker reads only for buttons).
    fn lit(s: &str) -> Object {
        Object::String(s.as_bytes().to_vec(), lopdf::StringFormat::Literal)
    }

    /// A UTF-16BE-with-BOM hex string `/V` — how filled forms store values.
    fn utf16be(s: &str) -> Object {
        let mut b = vec![0xFEu8, 0xFF];
        for u in s.encode_utf16() {
            b.extend_from_slice(&u.to_be_bytes());
        }
        Object::String(b, lopdf::StringFormat::Hexadecimal)
    }

    /// Wrap the given AcroForm field objects in a minimal one-page PDF (page +
    /// /Root → /AcroForm → /Fields) and serialize to bytes.
    fn save_form_pdf(doc: &mut Document, field_ids: &[ObjectId]) -> Vec<u8> {
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let mut page = Dictionary::new();
        page.set("Type", "Page");
        page.set("Parent", pages_id);
        page.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        doc.objects.insert(page_id, Object::Dictionary(page));
        let mut pages = Dictionary::new();
        pages.set("Type", "Pages");
        pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
        pages.set("Count", 1i64);
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let mut cat = Dictionary::new();
        cat.set("Type", "Catalog");
        cat.set("Pages", pages_id);
        if !field_ids.is_empty() {
            let mut acro = Dictionary::new();
            acro.set(
                "Fields",
                Object::Array(field_ids.iter().map(|id| Object::Reference(*id)).collect()),
            );
            let acro_id = doc.add_object(Object::Dictionary(acro));
            cat.set("AcroForm", acro_id);
        }
        let cat_id = doc.add_object(Object::Dictionary(cat));
        doc.trailer.set("Root", cat_id);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).expect("serialize form PDF");
        buf
    }

    fn text_field(doc: &mut Document, name: &str, value: Object) -> ObjectId {
        let mut d = Dictionary::new();
        d.set("FT", "Tx");
        d.set("T", lit(name));
        d.set("V", value);
        doc.add_object(Object::Dictionary(d))
    }

    #[test]
    fn acroform_extracts_filled_fields() {
        let mut doc = Document::with_version("1.5");
        // "Ort" carries a UTF-16BE value; "PLZ" a literal one.
        let ort = text_field(&mut doc, "Ort", utf16be("Konstanz"));
        let plz = text_field(&mut doc, "PLZ", lit("78467"));
        // A checked checkbox (/V /Yes) and an unchecked one (/V /Off → skipped).
        let mut cb = Dictionary::new();
        cb.set("FT", "Btn");
        cb.set("T", lit("Zustimmung"));
        cb.set("V", "Yes");
        cb.set("AS", "Yes");
        let cb_id = doc.add_object(Object::Dictionary(cb));
        let mut off = Dictionary::new();
        off.set("FT", "Btn");
        off.set("T", lit("Newsletter"));
        off.set("V", "Off");
        let off_id = doc.add_object(Object::Dictionary(off));

        let bytes = save_form_pdf(&mut doc, &[ort, plz, cb_id, off_id]);
        let fields = extract_acroform_fields(&bytes);
        let block = format_acroform_block(&fields);

        assert!(block.starts_with(ACROFORM_HEADER), "block: {block:?}");
        assert!(block.contains("Ort: Konstanz"), "UTF-16 value decoded: {block:?}");
        assert!(block.contains("PLZ: 78467"), "literal value: {block:?}");
        assert!(block.contains("Zustimmung: Yes"), "checked box on-state: {block:?}");
        assert!(!block.contains("Newsletter"), "Off checkbox is skipped: {block:?}");
    }

    #[test]
    fn acroform_absent_leaves_text_unchanged() {
        let mut doc = Document::with_version("1.5");
        let bytes = save_form_pdf(&mut doc, &[]); // no /AcroForm
        let fields = extract_acroform_fields(&bytes);
        assert!(fields.is_empty(), "no AcroForm → no fields, got {fields:?}");
        let block = format_acroform_block(&fields);
        assert_eq!(block, "");
        // Appending an empty block is a no-op on the ladder text.
        assert_eq!(append_form_block("2010 chars of template", &block), "2010 chars of template");
    }

    #[test]
    fn acroform_kids_fully_qualified_names() {
        let mut doc = Document::with_version("1.5");
        // A terminal child field "Ort" under a parent field "Adresse" (/Kids).
        let mut child = Dictionary::new();
        child.set("FT", "Tx");
        child.set("T", lit("Ort"));
        child.set("V", lit("Konstanz"));
        let child_id = doc.add_object(Object::Dictionary(child));
        let mut parent = Dictionary::new();
        parent.set("T", lit("Adresse"));
        parent.set("Kids", Object::Array(vec![Object::Reference(child_id)]));
        let parent_id = doc.add_object(Object::Dictionary(parent));

        let bytes = save_form_pdf(&mut doc, &[parent_id]);
        let fields = extract_acroform_fields(&bytes);
        assert_eq!(fields.len(), 1, "one terminal field, got {fields:?}");
        assert_eq!(fields[0].0, "Adresse.Ort", "name is fully qualified with '.'");
        assert_eq!(fields[0].1, "Konstanz");
    }

    #[test]
    fn append_form_block_shapes() {
        // Empty block → unchanged; blank base → just the block; else delimited.
        assert_eq!(append_form_block("body", ""), "body");
        assert_eq!(append_form_block("   ", "--- Formularfelder ---\nOrt: X"), "--- Formularfelder ---\nOrt: X");
        let joined = append_form_block("body", "--- Formularfelder ---\nOrt: X");
        assert!(joined.starts_with("body\n\n--- Formularfelder ---"), "{joined:?}");
    }

    /// The vision path's "no page render available → note + form block" branch:
    /// a fillable form whose page cannot be rasterized must return a `no_image`
    /// status with an explanatory `note` and the `--- Formularfelder ---` field
    /// values as `context`, NOT silently analyse the logo. This exercises the
    /// JSON shape the brain receives (`vision_form_note` over the extracted
    /// block); the render-vs-embedded decision itself is covered by
    /// `prepare_vision_input` at runtime.
    #[test]
    fn vision_form_note_carries_fields() {
        let mut doc = Document::with_version("1.5");
        let ort = text_field(&mut doc, "Ort", utf16be("Konstanz"));
        let plz = text_field(&mut doc, "PLZ", lit("78467"));
        let bytes = save_form_pdf(&mut doc, &[ort, plz]);

        let block = format_acroform_block(&extract_acroform_fields(&bytes));
        assert!(!block.is_empty(), "form fields must extract for the note branch");

        let out = vision_form_note(&block);
        assert_eq!(out["status"], "no_image");
        let note = out["note"].as_str().unwrap_or_default();
        assert!(note.contains("page not rendered"), "explanatory note present: {note:?}");
        assert!(note.contains("ocr_attachment"), "points at ocr_attachment: {note:?}");
        let ctx = out["context"].as_str().unwrap_or_default();
        assert!(ctx.starts_with(ACROFORM_HEADER), "context is the form block: {ctx:?}");
        assert!(ctx.contains("Ort: Konstanz"), "field value present: {ctx:?}");
        assert!(ctx.contains("PLZ: 78467"), "field value present: {ctx:?}");
    }

    /// Manual, real-corpus check (kept `#[ignore]`). Point `ECK_TEST_CCITT_PDF` at
    /// a real office-scanner CCITTFaxDecode CAS blob and run:
    ///
    /// ```text
    /// ECK_TEST_CCITT_PDF=data/filestore/ab/cd/....pdf \
    ///   cargo test -p wms --bin wms -- --ignored ocr_real_corpus_ccitt_pdf --nocapture
    /// ```
    ///
    /// Asserts the lopdf XObject decoder finds page bitmaps where the old JPEG
    /// byte-grep saw none, then runs the full OCR ladder and prints the text
    /// quality (chars / dictionary_ratio) so a human can judge the OCR result.
    #[tokio::test]
    #[ignore = "requires ECK_TEST_CCITT_PDF pointing at a real CCITTFaxDecode CAS blob"]
    async fn ocr_real_corpus_ccitt_pdf() {
        let path = std::env::var("ECK_TEST_CCITT_PDF")
            .expect("set ECK_TEST_CCITT_PDF to a real CCITTFaxDecode PDF path");
        let bytes = std::fs::read(&path).expect("read the CCITT fixture");
        let jpeg_grep = extract_pdf_jpeg_streams(&bytes, 4).len();
        let (imgs, unsupported) = extract_pdf_page_images(&bytes, 4);
        eprintln!(
            "[ccitt_real] {path}\n  jpeg_grep_pages={jpeg_grep} lopdf_pages={} unsupported={:?}",
            imgs.len(),
            unsupported
        );
        for (i, im) in imgs.iter().enumerate() {
            eprintln!("  page {i}: {}x{}", im.width(), im.height());
        }
        assert!(!imgs.is_empty(), "no page images decoded from the CCITT PDF");

        let (res, _stored) = compute_ocr("t", "scan.pdf", "application/pdf", bytes).await;
        eprintln!(
            "  status={} source={} pages={} chars={} lines={} dict_ratio={:.2} garbage={:.2}",
            res.status,
            res.source,
            res.pages,
            res.quality.chars,
            res.quality.lines,
            res.quality.dictionary_ratio,
            res.quality.garbage_ratio
        );
        eprintln!("  text head: {:?}", res.text.chars().take(400).collect::<String>());
    }

    /// Manual, real-corpus check (kept `#[ignore]` — the fixtures are private CAS
    /// blobs, too large / sensitive to embed). Point `ECK_TEST_ENCRYPTED_PDF` at
    /// a real owner-password PDF and run:
    ///
    /// ```text
    /// ECK_TEST_ENCRYPTED_PDF=data/filestore/6d/5e/6d5e...d3429c.pdf \
    ///   cargo test -p wms --bin wms -- --ignored decrypt_real_corpus_pdf --nocapture
    /// ```
    ///
    /// Asserts the pipeline actually strips `/Encrypt` AND that pdf-extract then
    /// pulls a real text layer OR the raw JPEG-stream scan finds decrypted page
    /// images (scan PDFs have no text layer) — i.e. the bytes became readable.
    #[tokio::test]
    #[ignore = "requires ECK_TEST_ENCRYPTED_PDF pointing at a real CAS blob"]
    async fn decrypt_real_corpus_pdf() {
        let path = std::env::var("ECK_TEST_ENCRYPTED_PDF")
            .expect("set ECK_TEST_ENCRYPTED_PDF to a real owner-password PDF path");
        let original = std::fs::read(&path).expect("read the encrypted fixture");
        assert!(pdf_is_encrypted(&original), "fixture is not /Encrypt-marked: {path}");

        let (out, _decrypted) = maybe_decrypt_pdf(original.clone()).await;
        let changed = out != original;
        let still_encrypted = pdf_is_encrypted(&out);
        let (layer, pages) = pdf_text_layer(out.clone()).await;
        let text_chars = non_ws_len(&layer);
        let jpeg_streams = extract_pdf_jpeg_streams(&out, 8).len();
        eprintln!(
            "[decrypt_real_corpus_pdf] {path}\n  changed={changed} still_/Encrypt={still_encrypted} \
             pages={pages} text_chars={text_chars} jpeg_streams={jpeg_streams} \
             ({} -> {} bytes)",
            original.len(),
            out.len(),
        );

        assert!(changed, "decryption produced no change — the empty password did not decrypt");
        assert!(!still_encrypted, "output still carries /Encrypt after decryption");
        assert!(
            text_chars >= 50 || jpeg_streams > 0,
            "decrypted output yields neither a text layer ({text_chars} chars) nor JPEG streams \
             ({jpeg_streams}) — content is still unreadable",
        );
    }

    // ── vision journal cache ─────────────────────────────────────────────

    fn row(norm: &str, emb: Option<Vec<f32>>, page_ok: bool, region_ok: bool) -> PriorRow {
        PriorRow {
            question: norm.to_string(),
            answer: "a".to_string(),
            model: "m".to_string(),
            created_at: Value::Null,
            question_norm: norm.to_string(),
            question_embedding: emb,
            page_ok,
            region_ok,
        }
    }

    #[test]
    fn vision_normalize_question_collapses_ws() {
        assert_eq!(normalize_question("  What  IS\tthe\nSerial? "), "what is the serial?");
    }

    #[test]
    fn vision_region_reuse_rules() {
        let r = Some(Region { x: 0.1, y: 0.2, w: 0.3, h: 0.4 });
        // whole-page prior answers anything on the page
        assert!(region_reusable(None, None));
        assert!(region_reusable(None, r));
        // cropped prior answers the same rounded region, not the whole page
        assert!(region_reusable(r, r));
        assert!(region_reusable(r, Some(Region { x: 0.101, y: 0.201, w: 0.30, h: 0.40 })));
        assert!(!region_reusable(r, None));
        assert!(!region_reusable(r, Some(Region { x: 0.9, y: 0.2, w: 0.3, h: 0.4 })));
    }

    #[test]
    fn vision_row_id_is_deterministic_and_20_hex() {
        let a = vision_row_id("cas-1", 0, None, "what is the serial");
        let b = vision_row_id("cas-1", 0, None, "what is the serial");
        let c = vision_row_id("cas-1", 1, None, "what is the serial");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 20);
        assert!(a.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn vision_verdict_exact_hit() {
        let prior = vec![
            row("other question", None, true, true),
            row("what is the serial", None, true, true),
        ];
        assert_eq!(vision_cache_verdict(&prior, "what is the serial", None, 0.92), Some(1));
    }

    #[test]
    fn vision_verdict_semantic_hit() {
        // Not an exact norm match, but the embedding is near-identical.
        let prior = vec![row("give me the serial number", Some(vec![1.0, 0.0, 0.0]), true, true)];
        let q_emb = vec![0.99, 0.14, 0.0]; // cos ≈ 0.990 vs [1,0,0]
        assert_eq!(
            vision_cache_verdict(&prior, "what is the serial", Some(&q_emb), 0.92),
            Some(0)
        );
    }

    #[test]
    fn vision_verdict_page_mismatch_is_miss() {
        // Exact norm + identical embedding, but the prior is on another page.
        let prior = vec![row("what is the serial", Some(vec![1.0, 0.0, 0.0]), false, true)];
        let q_emb = vec![1.0, 0.0, 0.0];
        assert_eq!(vision_cache_verdict(&prior, "what is the serial", Some(&q_emb), 0.92), None);
    }

    #[test]
    fn vision_verdict_below_threshold_is_miss() {
        let prior = vec![row("give me the serial number", Some(vec![1.0, 0.0, 0.0]), true, true)];
        let q_emb = vec![0.0, 1.0, 0.0]; // cos = 0 vs [1,0,0]
        assert_eq!(
            vision_cache_verdict(&prior, "what is the serial", Some(&q_emb), 0.92),
            None
        );
    }
}
