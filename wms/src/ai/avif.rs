//! Pure-Rust AVIF decode for the on-demand OCR / vision ladder (no C deps).
//!
//! In this build the `image` crate's `avif` feature is ENCODE-only — no C
//! `dav1d` is linked — so `image::load_from_memory` fails on every `.avif`.
//! That blinds OCR and vision on the 548 files our own AVIF optimizer produces
//! (re-encoded with `ravif`) plus any native `.avif` attachment. This module
//! fills the gap with a container-parse + AV1-decode pipeline that pulls in no C:
//!
//!   avif-parse  → strip the ISOBMFF/AVIF container down to the primary item's
//!                 AV1 OBU payload (the alpha item is parsed but ignored)
//!   rav1d-safe  → decode that OBU to YUV planes (safe Rust, forbid(unsafe_code))
//!   (here)      → YUV → RGB8 → `image::DynamicImage`
//!
//! The target is OCR / vision INPUT, not pixel-exact display, so the cheap paths
//! are taken deliberately: chroma is upsampled nearest-neighbour (4:2:0 / 4:2:2 /
//! 4:4:4 all handled), 10/12-bit samples are right-shifted down to 8-bit rather
//! than erroring, and the alpha plane is dropped.
//!
//! ## Colorimetry
//! The YUV→RGB matrix and range come from the decoded frame's `color_info()`
//! (i.e. the AV1 sequence header). BT.709 is used only when the header signals
//! it; **BT.601 is the default for everything else, including an unspecified
//! matrix.** The luma/chroma RANGE is taken from the signalled `color_range`
//! (our optimizer's `ravif` emits full-range BT.601 for YCbCr, or Identity/GBR
//! for its RGB model); only when the header carries no range at all does the AV1
//! bitstream default of *limited* range apply — matching the documented
//! "BT.601 limited-range" fallback.

use anyhow::{anyhow, Result};
use image::{DynamicImage, RgbImage};
use rav1d_safe::{ColorRange, Decoder, Frame, MatrixCoefficients, PixelLayout, PlaneView16, PlaneView8, Planes};

/// ISOBMFF sniff for AVIF: bytes 4..8 == `ftyp` and either the major brand
/// (8..12) or one of the compatible brands (from offset 16, 4 bytes each, within
/// the `ftyp` box) is `avif` / `avis`. Many CAS rows carry mime
/// `application/octet-stream`, so this magic-byte check — not the mime — is what
/// actually routes those files into AVIF decode.
pub fn looks_like_avif(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    if matches!(&bytes[8..12], b"avif" | b"avis") {
        return true;
    }
    // Compatible-brands list: bounded by the ftyp box size, defensively clamped.
    let box_size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = box_size.min(bytes.len());
    let mut i = 16; // skip major_brand (8..12) + minor_version (12..16)
    while i + 4 <= end {
        if matches!(&bytes[i..i + 4], b"avif" | b"avis") {
            return true;
        }
        i += 4;
    }
    false
}

/// Decode an AVIF still image to an RGB8 `DynamicImage`. Returns `Err` on a
/// container/decode failure or an empty frame; never intended to be fed a PDF or
/// a non-AVIF raster (route those through `image::load_from_memory`).
pub fn decode_avif(bytes: &[u8]) -> Result<DynamicImage> {
    // 1. Container → primary item AV1 OBU payload (alpha item ignored).
    let mut cursor = std::io::Cursor::new(bytes);
    let avif = avif_parse::read_avif(&mut cursor)
        .map_err(|e| anyhow!("avif-parse container parse failed: {e:?}"))?;
    let obu: &[u8] = &avif.primary_item;

    // 2. Decode the single temporal unit. threads=1 (Settings::default) makes
    //    decode() synchronous: the frame is returned inline. get_frame / flush
    //    are belt-and-braces for a decoder that buffered it instead.
    let mut decoder = Decoder::new().map_err(|e| anyhow!("rav1d init failed: {e}"))?;
    let mut frame = decoder
        .decode(obu)
        .map_err(|e| anyhow!("rav1d decode failed: {e}"))?;
    if frame.is_none() {
        frame = decoder.get_frame().map_err(|e| anyhow!("rav1d get_frame failed: {e}"))?;
    }
    if frame.is_none() {
        frame = decoder
            .flush()
            .map_err(|e| anyhow!("rav1d flush failed: {e}"))?
            .into_iter()
            .next();
    }
    let frame = frame.ok_or_else(|| anyhow!("rav1d produced no frame from AVIF primary item"))?;

    // 3. YUV → RGB8.
    frame_to_rgb(&frame)
}

/// A tightly-packed 8-bit plane (`w * h`, no stride padding).
struct Plane {
    data: Vec<u8>,
    w: usize,
    h: usize,
}

fn frame_to_rgb(frame: &Frame) -> Result<DynamicImage> {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    if w == 0 || h == 0 {
        return Err(anyhow!("AVIF frame has a zero dimension ({w}x{h})"));
    }
    let layout = frame.pixel_layout();
    let color = frame.color_info();
    let full_range = matches!(color.color_range, ColorRange::Full);
    let matrix = color.matrix_coefficients;

    // Pull Y/U/V into packed 8-bit planes (10/12-bit downshifted to 8-bit).
    let (yp, up, vp) = collect_planes(frame);

    let mut rgb = vec![0u8; w * h * 3];
    for j in 0..h {
        for i in 0..w {
            let yi = j.min(yp.h.saturating_sub(1)) * yp.w + i.min(yp.w.saturating_sub(1));
            let y = yp.data[yi] as f32;
            let (cb, cr) = match (&up, &vp) {
                (Some(u), Some(v)) => {
                    let (cx, cy) = chroma_xy(i, j, layout, u.w, u.h);
                    (u.data[cy * u.w + cx] as f32, v.data[cy * v.w + cx] as f32)
                }
                // I400 grayscale (or a missing chroma plane): neutral chroma.
                _ => (128.0, 128.0),
            };
            let (r, g, b) = yuv_to_rgb8(y, cb, cr, matrix, full_range);
            let o = (j * w + i) * 3;
            rgb[o] = r;
            rgb[o + 1] = g;
            rgb[o + 2] = b;
        }
    }

    RgbImage::from_raw(w as u32, h as u32, rgb)
        .map(DynamicImage::ImageRgb8)
        .ok_or_else(|| anyhow!("RGB buffer size mismatch"))
}

fn collect_planes(frame: &Frame) -> (Plane, Option<Plane>, Option<Plane>) {
    match frame.planes() {
        Planes::Depth8(p) => {
            let y = copy8(&p.y());
            let u = p.u().map(|pv| copy8(&pv));
            let v = p.v().map(|pv| copy8(&pv));
            (y, u, v)
        }
        Planes::Depth16(p) => {
            // 10/12-bit → 8-bit: drop the low bits. Coarse but fine for OCR.
            let shift = frame.bit_depth().saturating_sub(8);
            let y = copy16(&p.y(), shift);
            let u = p.u().map(|pv| copy16(&pv, shift));
            let v = p.v().map(|pv| copy16(&pv, shift));
            (y, u, v)
        }
    }
}

fn copy8(pv: &PlaneView8) -> Plane {
    let (w, h) = (pv.width(), pv.height());
    let mut data = Vec::with_capacity(w * h);
    for j in 0..h {
        data.extend_from_slice(pv.row(j)); // row() is exactly `w` samples, no stride
    }
    Plane { data, w, h }
}

fn copy16(pv: &PlaneView16, shift: u8) -> Plane {
    let (w, h) = (pv.width(), pv.height());
    let mut data = Vec::with_capacity(w * h);
    for j in 0..h {
        for &s in pv.row(j) {
            data.push((s >> shift).min(255) as u8);
        }
    }
    Plane { data, w, h }
}

/// Nearest-neighbour chroma sample position for luma pixel `(i, j)`, clamped to
/// the chroma plane's dimensions `(cw, ch)`.
fn chroma_xy(i: usize, j: usize, layout: PixelLayout, cw: usize, ch: usize) -> (usize, usize) {
    let (cx, cy) = match layout {
        PixelLayout::I420 => (i / 2, j / 2),
        PixelLayout::I422 => (i / 2, j),
        PixelLayout::I444 => (i, j),
        PixelLayout::I400 => (0, 0),
    };
    (cx.min(cw.saturating_sub(1)), cy.min(ch.saturating_sub(1)))
}

/// Convert one YUV sample (8-bit) to RGB8. See the module "Colorimetry" note for
/// the matrix/range selection and the BT.601 default.
fn yuv_to_rgb8(y: f32, cb: f32, cr: f32, matrix: MatrixCoefficients, full_range: bool) -> (u8, u8, u8) {
    // Identity (MC=0): the three planes are already G, B, R at full range — no
    // matrix, just a channel reorder (Y=G, U=B, V=R).
    if matches!(matrix, MatrixCoefficients::Identity) {
        return (clamp8(cr), clamp8(y), clamp8(cb));
    }
    let cb = cb - 128.0;
    let cr = cr - 128.0;
    let is709 = matches!(matrix, MatrixCoefficients::BT709);
    let (r, g, b) = if full_range {
        if is709 {
            (y + 1.5748 * cr, y - 0.187_324 * cb - 0.468_124 * cr, y + 1.8556 * cb)
        } else {
            (y + 1.402 * cr, y - 0.344_136 * cb - 0.714_136 * cr, y + 1.772 * cb)
        }
    } else {
        let y = 1.164_383 * (y - 16.0);
        if is709 {
            (y + 1.792_741 * cr, y - 0.213_249 * cb - 0.532_909 * cr, y + 2.112_402 * cb)
        } else {
            (y + 1.596_027 * cr, y - 0.391_762 * cb - 0.812_968 * cr, y + 2.017_232 * cb)
        }
    };
    (clamp8(r), clamp8(g), clamp8(b))
}

fn clamp8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ExtendedColorType, ImageEncoder, Rgb};

    /// Encode a synthetic high-contrast (text-like) RGB image to AVIF using the
    /// `image` crate's own encoder — encode support IS enabled in this build —
    /// then decode it back through `decode_avif`. Asserts dimensions round-trip
    /// and the mean per-channel error stays within a loose tolerance (AV1 is
    /// lossy). Also proves the encoded bytes sniff as AVIF.
    fn synthetic_blocks() -> RgbImage {
        // Four solid colour quadrants over an 8x8 high-contrast checkerboard
        // tint. Solid colour regions exercise the full YUV→RGB matrix (distinct
        // R/G/B), while the checkerboard keeps it text-like; flat areas dominate
        // so 4:2:0 chroma subsampling stays near-lossless away from the seams.
        let mut img = RgbImage::new(64, 64);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let base = match (x < 32, y < 32) {
                (true, true) => [200u8, 40, 40],   // red
                (false, true) => [40, 180, 40],    // green
                (true, false) => [40, 40, 200],    // blue
                (false, false) => [210, 210, 210], // near-white
            };
            let dark = ((x / 8) + (y / 8)) % 2 == 0;
            *px = if dark {
                Rgb([base[0] / 4, base[1] / 4, base[2] / 4])
            } else {
                Rgb(base)
            };
        }
        img
    }

    fn encode_avif(img: &RgbImage) -> Vec<u8> {
        let mut buf = Vec::new();
        let enc = image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut buf, 10, 90);
        enc.write_image(img.as_raw(), img.width(), img.height(), ExtendedColorType::Rgb8)
            .expect("AVIF encode");
        buf
    }

    #[test]
    fn avif_round_trip_8bit() {
        let img = synthetic_blocks();
        let encoded = encode_avif(&img);
        assert!(looks_like_avif(&encoded), "encoded bytes must sniff as AVIF");

        let decoded = decode_avif(&encoded).expect("decode_avif").to_rgb8();
        assert_eq!(decoded.dimensions(), (64, 64));

        let mut err = 0u64;
        for (a, b) in img.pixels().zip(decoded.pixels()) {
            for c in 0..3 {
                err += (a[c] as i32 - b[c] as i32).unsigned_abs() as u64;
            }
        }
        let mean = err as f64 / (64.0 * 64.0 * 3.0);
        // Observed ~1.1 on x86_64 (quality 90). A generous ceiling absorbs
        // encoder/speed variation across platforms while still catching a wrong
        // colour matrix or range (those blow the mean well past 20).
        assert!(mean < 10.0, "mean per-channel error too high: {mean}");
    }

    #[test]
    fn looks_like_avif_sniff() {
        let encoded = encode_avif(&synthetic_blocks());
        assert!(looks_like_avif(&encoded));
        // Negatives: JPEG magic and a too-short buffer must not match.
        assert!(!looks_like_avif(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!looks_like_avif(b"%PDF-1.4"));
    }
}
