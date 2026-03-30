use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use printpdf::{
    BuiltinFont, Color, Mm, PdfDocument, PdfLayerReference, Rect, Rgb,
};
use qrcode::{EcLevel, QrCode};
use serde::Deserialize;
use std::io::BufWriter;
use std::sync::Arc;
use uuid::Uuid;

use eck_core::utils::smart_tag::SmartTag;

use crate::AppState;

// ─── Request types ───────────────────────────────────────────────────────────

#[derive(Deserialize, Debug, Clone)]
pub struct ElementConfig {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ContentConfig {
    pub qr1: Option<ElementConfig>,
    pub qr2: Option<ElementConfig>,
    pub qr3: Option<ElementConfig>,
    pub checksum: Option<ElementConfig>,
    pub serial: Option<ElementConfig>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RegalConfig {
    pub index: i32,
    pub columns: i32,
    pub rows: i32,
    pub start_index: i32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct WarehouseConfig {
    pub regals: Vec<RegalConfig>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LabelRequest {
    #[serde(rename = "type")]
    pub label_type: String,
    #[serde(default)]
    pub start_number: i64,
    #[serde(default)]
    pub count: i32,
    #[serde(default = "default_cols")]
    pub cols: i32,
    #[serde(default = "default_rows")]
    pub rows: i32,
    #[serde(default = "default_margin")]
    pub margin_top: f64,
    #[serde(default = "default_margin")]
    pub margin_left: f64,
    #[serde(default = "default_margin")]
    pub margin_right: f64,
    #[serde(default = "default_margin")]
    pub margin_bottom: f64,
    #[serde(default)]
    pub gap_x: f64,
    #[serde(default)]
    pub gap_y: f64,
    #[serde(default)]
    pub is_tight_mode: bool,
    #[serde(default = "default_serial_digits")]
    pub serial_digits: i32,
    pub content_config: Option<ContentConfig>,
    pub warehouse_config: Option<WarehouseConfig>,
    pub rack_id: Option<String>,
}

fn default_cols() -> i32 { 3 }
fn default_rows() -> i32 { 7 }
fn default_margin() -> f64 { 4.0 }
fn default_serial_digits() -> i32 { 6 }

// ─── Internal label item ─────────────────────────────────────────────────────

struct LabelItem {
    uuid: Uuid,
    tag_type: char,
    display_primary: String,   // serial number
    display_secondary: String, // checksum
}

// ─── Base32 / CRC helpers ────────────────────────────────────────────────────

const BASE32_CHARS: &[u8] = b"0123456789ABCDEFGHJKLMNPQRTUVWXY";

fn to_base32_char(num: usize) -> char {
    if num >= BASE32_CHARS.len() { '?' } else { BASE32_CHARS[num] as char }
}

fn eck_crc(value: i64) -> String {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(value.to_string().as_bytes());
    let temp = hasher.finalize() & 1023;
    let c1 = BASE32_CHARS[(temp >> 5) as usize] as char;
    let c2 = BASE32_CHARS[(temp & 31) as usize] as char;
    format!("{}{}", c1, c2)
}

fn format_serial(num: i64, prefix: &str, digits: i32) -> String {
    let padded = format!("{:018}", num);
    if digits > 0 && digits < 18 {
        let start = 18 - digits as usize;
        return format!("{}{}", prefix, &padded[start..]);
    }
    format!("{}{}", prefix, padded)
}

fn calculate_warehouse_location(place_index: i64, config: &WarehouseConfig) -> Option<(i32, i32, i32)> {
    for r in &config.regals {
        let places_in_regal = r.columns * r.rows;
        let end_idx = r.start_index + places_in_regal - 1;
        if place_index >= r.start_index as i64 && place_index <= end_idx as i64 {
            let index_in_regal = (place_index as i32) - r.start_index;
            let column = index_in_regal / r.rows;
            let row = index_in_regal % r.rows;
            return Some((r.index, column + 1, row + 1));
        }
    }
    None
}

// ─── QR rendering (vector — draws filled rectangles, no raster) ──────────────

fn add_qr_to_layer(
    layer: &PdfLayerReference,
    qr_data: &str,
    pos_x: f64,
    pos_y: f64,
    size_mm: f64,
) -> Result<(), anyhow::Error> {
    let code = QrCode::with_error_correction_level(qr_data, EcLevel::L)?;
    let modules = code.to_colors();
    let module_count = code.width() as f64;
    let cell_mm = size_mm / module_count;

    let black = Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None));

    for row in 0..code.width() as usize {
        for col in 0..code.width() as usize {
            if modules[row * code.width() as usize + col] == qrcode::Color::Dark {
                let cx = pos_x + (col as f64) * cell_mm;
                // PDF Y is bottom-up, QR matrix is top-down
                let cy = pos_y + size_mm - ((row + 1) as f64) * cell_mm;

                // filled black square (PaintMode::Fill is default)
                let rect = Rect::new(
                    Mm(cx as f32),
                    Mm(cy as f32),
                    Mm((cx + cell_mm) as f32),
                    Mm((cy + cell_mm) as f32),
                );
                layer.set_fill_color(black.clone());
                layer.set_outline_color(black.clone());
                layer.set_outline_thickness(0.0);
                layer.add_rect(rect);
            }
        }
    }

    Ok(())
}

// ─── PDF generation ──────────────────────────────────────────────────────────

fn generate_labels_pdf(cfg: &LabelRequest, items: &[LabelItem]) -> Result<Vec<u8>, anyhow::Error> {
    let page_width: f64 = 210.0;
    let page_height: f64 = 297.0;

    let (doc, page1, layer1) = PdfDocument::new("Labels", Mm(210.0), Mm(297.0), "Layer 1");
    let mut current_layer = doc.get_page(page1).get_layer(layer1);

    let mut extra_x = 0.0;
    let mut extra_y = 0.0;
    if !cfg.is_tight_mode {
        extra_x = cfg.gap_x / 2.0;
        extra_y = cfg.gap_y / 2.0;
    }

    let eff_margin_left = cfg.margin_left + extra_x;
    let eff_margin_right = cfg.margin_right + extra_x;
    let eff_margin_top = cfg.margin_top + extra_y;
    let eff_margin_bottom = cfg.margin_bottom + extra_y;

    let avail_w = page_width - eff_margin_left - eff_margin_right;
    let avail_h = page_height - eff_margin_top - eff_margin_bottom;

    let total_gap_x = (cfg.cols - 1) as f64 * cfg.gap_x;
    let total_gap_y = (cfg.rows - 1) as f64 * cfg.gap_y;
    let label_w = (avail_w - total_gap_x) / cfg.cols as f64;
    let label_h = (avail_h - total_gap_y) / cfg.rows as f64;

    let labels_per_page = cfg.cols * cfg.rows;

    let font = doc.add_builtin_font(BuiltinFont::HelveticaBold)?;
    let courier_font = doc.add_builtin_font(BuiltinFont::CourierBold)?;

    for i in 0..items.len() {
        if i > 0 && (i as i32) % labels_per_page == 0 {
            let (new_page, new_layer) = doc.add_page(Mm(210.0), Mm(297.0), "Layer 1");
            current_layer = doc.get_page(new_page).get_layer(new_layer);
        }

        let index_on_page = (i as i32) % labels_per_page;
        let col_idx = index_on_page % cfg.cols;
        let row_idx = index_on_page / cfg.cols;

        let origin_x = eff_margin_left + (col_idx as f64) * (label_w + cfg.gap_x);
        let top_y = eff_margin_top + (row_idx as f64) * (label_h + cfg.gap_y);
        let origin_y = page_height - top_y - label_h;

        let item = &items[i];

        // SmartTag V2: encode to URL-safe base64 (26 chars)
        let tag = SmartTag::new(item.tag_type, item.uuid);
        let encoded = tag.encode();

        let field1 = &item.display_primary;
        let field2 = &item.display_secondary;

        let min_side = label_w.min(label_h);

        if cfg.content_config.is_none() {
            // Default layout (no content config from frontend)
            let qr1_scale = 0.85;
            let qr1_size = label_h * qr1_scale;
            let qr1_data = format!("ECK1.COM/{}", encoded);
            let qr1_x = origin_x + 2.0;
            let qr1_y = origin_y + (label_h - qr1_size) / 2.0;
            let _ = add_qr_to_layer(&current_layer, &qr1_data, qr1_x, qr1_y, qr1_size);

            let cs_scale = 0.45;
            let cs_size = label_h * cs_scale;
            let cs_x = origin_x + qr1_size + 8.0;
            let cs_y = origin_y + label_h / 2.0 - cs_size / 4.0;
            current_layer.use_text(
                field2,
                (cs_size * 2.5) as f32,
                Mm(cs_x as f32),
                Mm(cs_y as f32),
                &font,
            );

            let cs_width = field2.len() as f64 * cs_size * 0.6;

            let s_scale = 0.12;
            let s_size = label_h * s_scale;
            let serial_x = origin_x + qr1_size + 8.0;
            let serial_y = origin_y + label_h * 0.25;
            current_layer.set_fill_color(Color::Rgb(Rgb::new(0.3, 0.3, 0.3, None)));
            current_layer.use_text(
                field1,
                (s_size * 2.5) as f32,
                Mm(serial_x as f32),
                Mm(serial_y as f32),
                &courier_font,
            );
            current_layer.set_fill_color(Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)));

            let s_qr_scale = 0.32;
            let s_qr_size = label_h * s_qr_scale;
            let right_x = origin_x + qr1_size + cs_width + 16.0;

            if right_x + s_qr_size < origin_x + label_w {
                let qr2_data = format!("ECK2.COM/{}", encoded);
                let qr2_y = origin_y + label_h - s_qr_size - 3.0;
                let _ = add_qr_to_layer(&current_layer, &qr2_data, right_x, qr2_y, s_qr_size);

                let qr3_data = format!("ECK3.COM/{}", encoded);
                let qr3_y = origin_y + 3.0;
                let _ = add_qr_to_layer(&current_layer, &qr3_data, right_x, qr3_y, s_qr_size);
            }
        } else {
            // Custom layout from frontend contentConfig (percentage-based positioning)
            let cc = cfg.content_config.as_ref().unwrap();

            if let Some(ref cs_cfg) = cc.checksum {
                let size = min_side * cs_cfg.scale;
                let pos_x = origin_x + (cs_cfg.x * label_w / 100.0);
                let pos_y = origin_y + (cs_cfg.y * label_h / 100.0);
                current_layer.use_text(
                    field2,
                    (size * 2.5) as f32,
                    Mm(pos_x as f32),
                    Mm(pos_y as f32),
                    &font,
                );
            }

            if let Some(ref serial_cfg) = cc.serial {
                let size = min_side * serial_cfg.scale;
                let pos_x = origin_x + (serial_cfg.x * label_w / 100.0);
                let pos_y = origin_y + (serial_cfg.y * label_h / 100.0);
                current_layer.set_fill_color(Color::Rgb(Rgb::new(0.3, 0.3, 0.3, None)));
                current_layer.use_text(
                    field1,
                    (size * 2.5) as f32,
                    Mm(pos_x as f32),
                    Mm(pos_y as f32),
                    &courier_font,
                );
                current_layer.set_fill_color(Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)));
            }

            let draw_qr = |prefix: &str, el: &ElementConfig| {
                let qr_data = format!("{}/{}", prefix, encoded);
                let size = min_side * el.scale;
                let pos_x = origin_x + (el.x * label_w / 100.0);
                let pos_y = origin_y + (el.y * label_h / 100.0);
                let _ = add_qr_to_layer(&current_layer, &qr_data, pos_x, pos_y, size);
            };

            if let Some(ref qr1) = cc.qr1 {
                draw_qr("ECK1.COM", qr1);
            }
            if let Some(ref qr2) = cc.qr2 {
                draw_qr("ECK2.COM", qr2);
            }
            if let Some(ref qr3) = cc.qr3 {
                draw_qr("ECK3.COM", qr3);
            }
        }
    }

    let mut buf = Vec::new();
    doc.save(&mut BufWriter::new(&mut buf))?;
    Ok(buf)
}

// ─── Handler ─────────────────────────────────────────────────────────────────

/// POST /api/print/labels
pub async fn generate_labels(
    State(_state): State<Arc<AppState>>,
    Json(mut payload): Json<LabelRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // Apply defaults
    if payload.cols == 0 { payload.cols = 3; }
    if payload.rows == 0 { payload.rows = 7; }
    if payload.count == 0 { payload.count = payload.cols * payload.rows; }
    if payload.label_type.is_empty() { payload.label_type = "i".to_string(); }

    let tag_char = match payload.label_type.as_str() {
        "i" => 'i',
        "b" => 'b',
        "p" => 'p',
        "l" => 'l',
        _ => 'i',
    };

    let serial_prefix = match payload.label_type.as_str() {
        "i" => "!",
        "b" => "#",
        "l" => "*",
        "p" => "_",
        _ => "",
    };

    let mut items = Vec::with_capacity(payload.count as usize);

    if payload.label_type == "p" {
        // Places — use warehouse config for naming
        for i in 0..payload.count {
            let current_id = payload.start_number + (i as i64);
            let uuid = Uuid::new_v4();

            let (short_name, display_primary) = if let Some(ref wc) = payload.warehouse_config {
                if let Some((r_idx, c_idx, row_idx)) = calculate_warehouse_location(current_id, wc) {
                    let name = format!(
                        "{}{}{}",
                        to_base32_char(r_idx as usize),
                        to_base32_char(c_idx as usize),
                        to_base32_char(row_idx as usize)
                    );
                    let serial = format_serial(current_id, serial_prefix, payload.serial_digits);
                    (name, serial)
                } else {
                    let serial = format_serial(current_id, serial_prefix, payload.serial_digits);
                    (serial.clone(), serial)
                }
            } else {
                let serial = format_serial(current_id, serial_prefix, payload.serial_digits);
                (serial.clone(), serial)
            };

            items.push(LabelItem {
                uuid,
                tag_type: tag_char,
                display_primary,
                display_secondary: short_name,
            });
        }
    } else {
        // Items, Boxes, Labels
        for i in 0..payload.count {
            let current_id = payload.start_number + (i as i64);
            let uuid = Uuid::new_v4();

            items.push(LabelItem {
                uuid,
                tag_type: tag_char,
                display_primary: format_serial(current_id, serial_prefix, payload.serial_digits),
                display_secondary: eck_crc(current_id),
            });
        }
    }

    let pdf_bytes = generate_labels_pdf(&payload, &items).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to generate PDF: {}", e))
    })?;

    let filename = format!("labels_{}_{}.pdf", payload.label_type, payload.start_number);

    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", filename)),
            (header::CONTENT_LENGTH, pdf_bytes.len().to_string()),
        ],
        pdf_bytes,
    ))
}
