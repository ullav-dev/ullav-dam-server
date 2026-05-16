use bytes::Bytes;
use exif::{In, Reader, Tag, Value};
use serde_json::{Map, Value as Json};
use std::io::Cursor;
use tracing::warn;
use uuid::Uuid;

// ── Public types ──────────────────────────────────────────────────────────────

/// Normalised result of metadata extraction from a single file.
/// Fields that can map to the `assets` table are `Option` — `None` means the
/// file had no value so the existing DB value should be preserved.
#[derive(Debug, Default)]
pub struct ExtractedMetadata {
    pub exif: Option<Json>,
    pub iptc: Option<Json>,
    pub xmp: Option<Json>,
    pub creator: Option<String>,
    pub copyright: Option<String>,
    pub caption: Option<String>,
    /// Comma-joined keyword list, ready to write into `assets.keywords`.
    pub keywords: Option<String>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Extract EXIF/IPTC/XMP metadata from raw file bytes.
///
/// - EXIF: extracted from all formats via kamadak-exif.
/// - IPTC and XMP: extracted from JPEG only (APP13 and APP1 segments).
/// - Priority for override fields: IPTC > XMP > EXIF.
///
/// Always succeeds — any parse error is logged and `Default` is returned.
/// Intended to run inside `tokio::task::spawn_blocking`.
pub fn extract_from_bytes(data: Bytes) -> ExtractedMetadata {
    // EXIF — works for JPEG, TIFF, PNG, WebP, HEIF
    let ex = extract_exif(&data);
    let mut creator = ex.creator;
    let mut copyright = ex.copyright;
    let mut caption = ex.caption;
    let mut keywords: Option<String> = None;
    let mut iptc_json: Option<Json> = None;
    let mut xmp_json: Option<Json> = None;

    // JPEG-specific IPTC (APP13) and XMP (APP1)
    if data.starts_with(&[0xFF, 0xD8]) {
        match img_parts::jpeg::Jpeg::from_bytes(data.clone()) {
            Ok(jpeg) => {
                // XMP — second priority (overrides EXIF fallback tags)
                if let Some((xj, f)) = extract_xmp(&jpeg) {
                    xmp_json = Some(xj);
                    if f.creator.is_some() { creator = f.creator; }
                    if f.copyright.is_some() { copyright = f.copyright; }
                    if f.caption.is_some() { caption = f.caption; }
                    if f.keywords.is_some() { keywords = f.keywords; }
                }
                // IPTC — highest priority (overrides both EXIF and XMP)
                if let Some((ij, f)) = extract_iptc(&jpeg) {
                    iptc_json = Some(ij);
                    if f.creator.is_some() { creator = f.creator; }
                    if f.copyright.is_some() { copyright = f.copyright; }
                    if f.caption.is_some() { caption = f.caption; }
                    if f.keywords.is_some() { keywords = f.keywords; }
                }
            }
            Err(e) => warn!("JPEG segment parse failed: {e}"),
        }
    }

    ExtractedMetadata {
        exif: ex.json,
        iptc: iptc_json,
        xmp: xmp_json,
        creator,
        copyright,
        caption,
        keywords,
    }
}

// ── DB storage ────────────────────────────────────────────────────────────────

/// Upsert `asset_metadata` and apply file-derived overrides to `assets`.
///
/// Override policy: COALESCE in SQL — file value wins when present, existing
/// DB value is preserved when the file was silent on that field.
pub async fn store_metadata(
    client: &tokio_postgres::Client,
    asset_id: Uuid,
    meta: &ExtractedMetadata,
) -> Result<(), tokio_postgres::Error> {
    client
        .execute(
            "INSERT INTO asset_metadata (asset_id, exif, iptc, xmp) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (asset_id) DO UPDATE SET \
               exif = EXCLUDED.exif, \
               iptc = EXCLUDED.iptc, \
               xmp  = EXCLUDED.xmp, \
               extracted_at = NOW()",
            &[&asset_id, &meta.exif, &meta.iptc, &meta.xmp],
        )
        .await?;

    client
        .execute(
            "UPDATE assets SET \
               creator          = COALESCE($1, creator), \
               copyright_notice = COALESCE($2, copyright_notice), \
               caption          = COALESCE($3, caption), \
               keywords         = COALESCE($4, keywords) \
             WHERE id = $5",
            &[
                &meta.creator,
                &meta.copyright,
                &meta.caption,
                &meta.keywords,
                &asset_id,
            ],
        )
        .await?;

    Ok(())
}

// ── EXIF ──────────────────────────────────────────────────────────────────────

struct ExifResult {
    json: Option<Json>,
    creator: Option<String>,
    copyright: Option<String>,
    caption: Option<String>,
}

fn extract_exif(data: &[u8]) -> ExifResult {
    let reader = Reader::new();
    let exif = match reader.read_from_container(&mut Cursor::new(data)) {
        Ok(e) => e,
        Err(e) => {
            warn!("EXIF parse skipped: {e}");
            return ExifResult { json: None, creator: None, copyright: None, caption: None };
        }
    };

    let mut obj = Map::new();

    if let Some(v) = ascii_field(&exif, Tag::Make) {
        obj.insert("camera_make".into(), Json::String(v));
    }
    if let Some(v) = ascii_field(&exif, Tag::Model) {
        obj.insert("camera_model".into(), Json::String(v));
    }
    if let Some(v) = ascii_field(&exif, Tag::DateTimeOriginal)
        .or_else(|| ascii_field(&exif, Tag::DateTime))
    {
        obj.insert("datetime".into(), Json::String(v));
    }
    if let Some(v) = rational_display(&exif, Tag::FocalLength) {
        obj.insert("focal_length".into(), Json::String(v));
    }
    if let Some(v) = rational_display(&exif, Tag::FNumber) {
        obj.insert("aperture".into(), Json::String(v));
    }
    if let Some(v) = rational_display(&exif, Tag::ExposureTime) {
        obj.insert("shutter_speed".into(), Json::String(v));
    }
    if let Some(v) = short_or_long_field(&exif, Tag::PhotographicSensitivity) {
        obj.insert("iso".into(), Json::Number(v.into()));
    }
    if let Some(v) = short_or_long_field(&exif, Tag::PixelXDimension)
        .or_else(|| short_or_long_field(&exif, Tag::ImageWidth))
    {
        obj.insert("width".into(), Json::Number(v.into()));
    }
    if let Some(v) = short_or_long_field(&exif, Tag::PixelYDimension)
        .or_else(|| short_or_long_field(&exif, Tag::ImageLength))
    {
        obj.insert("height".into(), Json::Number(v.into()));
    }
    if let Some(v) = short_or_long_field(&exif, Tag::Orientation) {
        obj.insert("orientation".into(), Json::Number(v.into()));
    }

    if let Some((lat, lon)) = gps_decimal(&exif) {
        obj.insert("gps_lat".into(), serde_json::json!(lat));
        obj.insert("gps_lon".into(), serde_json::json!(lon));
        if let Some(alt) = gps_altitude(&exif) {
            obj.insert("gps_alt".into(), serde_json::json!(alt));
        }
    }

    let mut raw = Map::new();
    for field in exif.fields() {
        if field.ifd_num == In::PRIMARY || field.ifd_num == In::THUMBNAIL {
            raw.insert(
                format!("{}", field.tag),
                Json::String(field.display_value().with_unit(&exif).to_string()),
            );
        }
    }
    if !raw.is_empty() {
        obj.insert("_raw".into(), Json::Object(raw));
    }

    let creator = ascii_field(&exif, Tag::Artist);
    let copyright = ascii_field(&exif, Tag::Copyright);
    let caption = ascii_field(&exif, Tag::ImageDescription);

    if obj.is_empty() {
        return ExifResult { json: None, creator, copyright, caption };
    }

    ExifResult { json: Some(Json::Object(obj)), creator, copyright, caption }
}

// ── IPTC IIM (JPEG APP13) ─────────────────────────────────────────────────────

#[derive(Default)]
struct TextFields {
    creator: Option<String>,
    copyright: Option<String>,
    caption: Option<String>,
    keywords: Option<String>,
}

fn extract_iptc(jpeg: &img_parts::jpeg::Jpeg) -> Option<(Json, TextFields)> {
    // Find the first APP13 segment containing Photoshop IIM data
    let iim_data = jpeg.segments().iter().find_map(|seg| {
        if seg.marker() != img_parts::jpeg::markers::APP13 {
            return None;
        }
        find_iptc_iim_block(seg.contents())
    })?;

    parse_iptc_iim(iim_data)
}

/// Locate the IPTC-NAA resource (0x0404) within a Photoshop APP13 segment.
fn find_iptc_iim_block(contents: &[u8]) -> Option<Vec<u8>> {
    let photoshop_hdr = b"Photoshop 3.0\0";
    if !contents.starts_with(photoshop_hdr) {
        return None;
    }
    let mut pos = photoshop_hdr.len();

    while pos + 12 <= contents.len() {
        if &contents[pos..pos + 4] != b"8BIM" {
            break;
        }
        pos += 4;

        // Resource ID (2 bytes BE)
        let resource_id = u16::from_be_bytes([contents[pos], contents[pos + 1]]);
        pos += 2;

        // Pascal string: 1 length byte + N bytes, padded to even total
        let name_len = contents[pos] as usize;
        pos += 1 + name_len;
        if (1 + name_len) % 2 != 0 {
            pos += 1;
        }

        // Data length (4 bytes BE)
        if pos + 4 > contents.len() {
            break;
        }
        let data_len =
            u32::from_be_bytes([contents[pos], contents[pos + 1], contents[pos + 2], contents[pos + 3]])
                as usize;
        pos += 4;

        if pos + data_len > contents.len() {
            break;
        }

        if resource_id == 0x0404 {
            return Some(contents[pos..pos + data_len].to_vec());
        }

        // Advance past data (padded to even)
        pos += data_len;
        if data_len % 2 != 0 {
            pos += 1;
        }
    }

    None
}

fn parse_iptc_iim(data: Vec<u8>) -> Option<(Json, TextFields)> {
    let mut obj = Map::new();
    let mut keywords: Vec<String> = Vec::new();
    let mut raw = Map::new();
    let mut fields = TextFields::default();

    let mut pos = 0;
    while pos + 5 <= data.len() {
        // Marker must be 0x1C
        if data[pos] != 0x1C {
            pos += 1;
            continue;
        }
        let record = data[pos + 1];
        let dataset = data[pos + 2];
        let len = u16::from_be_bytes([data[pos + 3], data[pos + 4]]) as usize;
        pos += 5;

        if pos + len > data.len() {
            break;
        }

        let value_bytes = &data[pos..pos + len];
        pos += len;

        // Only care about record 2 (application record)
        if record != 2 {
            continue;
        }

        let text = String::from_utf8_lossy(value_bytes).trim().to_string();
        if text.is_empty() {
            continue;
        }

        let tag_key = format!("2:{dataset}");
        raw.insert(tag_key, Json::String(text.clone()));

        match dataset {
            5 => { obj.insert("title".into(), Json::String(text)); }
            80 => {
                obj.insert("creator".into(), Json::String(text.clone()));
                fields.creator = Some(text);
            }
            105 => { obj.insert("headline".into(), Json::String(text)); }
            116 => {
                obj.insert("copyright".into(), Json::String(text.clone()));
                fields.copyright = Some(text);
            }
            120 => {
                obj.insert("caption".into(), Json::String(text.clone()));
                fields.caption = Some(text);
            }
            25 => { keywords.push(text); }
            90 => { obj.insert("city".into(), Json::String(text)); }
            101 => { obj.insert("country".into(), Json::String(text)); }
            _ => {}
        }
    }

    if !keywords.is_empty() {
        let kw_str = keywords.join(", ");
        obj.insert(
            "keywords".into(),
            Json::Array(keywords.iter().map(|k| Json::String(k.clone())).collect()),
        );
        fields.keywords = Some(kw_str);
    }

    if obj.is_empty() {
        return None;
    }

    obj.insert("_raw".into(), Json::Object(raw));
    Some((Json::Object(obj), fields))
}

// ── XMP (JPEG APP1) ───────────────────────────────────────────────────────────

const XMP_NAMESPACE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

fn extract_xmp(jpeg: &img_parts::jpeg::Jpeg) -> Option<(Json, TextFields)> {
    let xml_bytes = jpeg.segments().iter().find_map(|seg| {
        if seg.marker() != img_parts::jpeg::markers::APP1 {
            return None;
        }
        let c = seg.contents();
        if c.starts_with(XMP_NAMESPACE) {
            Some(c[XMP_NAMESPACE.len()..].to_vec())
        } else {
            None
        }
    })?;

    let xml_str = std::str::from_utf8(&xml_bytes).ok()?;
    parse_xmp(xml_str)
}

fn parse_xmp(xml: &str) -> Option<(Json, TextFields)> {
    let doc = roxmltree::Document::parse(xml).ok()?;

    // Walk to rdf:Description
    let desc = doc.descendants().find(|n| {
        n.tag_name().name() == "Description"
            && n.tag_name().namespace() == Some("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
    })?;

    let mut obj = Map::new();
    let mut raw = Map::new();
    let mut fields = TextFields::default();

    for child in desc.children().filter(|n| n.is_element()) {
        let ns = child.tag_name().namespace().unwrap_or("");
        let local = child.tag_name().name();
        let qname = format!("{ns}:{local}");

        // Determine if it's a collection (Seq/Bag/Alt) or a simple text node
        let collection = child.children().find(|n| {
            let name = n.tag_name().name();
            name == "Seq" || name == "Bag" || name == "Alt"
        });

        let value_json: Json;
        if let Some(coll) = collection {
            let items: Vec<String> = coll
                .children()
                .filter(|n| n.tag_name().name() == "li")
                .filter_map(|li| li.text())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if items.is_empty() {
                continue;
            }
            value_json = Json::Array(items.iter().map(|s| Json::String(s.clone())).collect());
            // Also record first value as string in raw
            raw.insert(qname.clone(), value_json.clone());

            // Map known multi-value fields
            if ns == "http://purl.org/dc/elements/1.1/" {
                match local {
                    "creator" => {
                        if let Some(v) = items.first() {
                            obj.insert("creator".into(), Json::String(v.clone()));
                            fields.creator = Some(v.clone());
                        }
                    }
                    "subject" => {
                        let kw_str = items.join(", ");
                        obj.insert(
                            "keywords".into(),
                            Json::Array(items.iter().map(|s| Json::String(s.clone())).collect()),
                        );
                        fields.keywords = Some(kw_str);
                    }
                    "rights" => {
                        if let Some(v) = items.first() {
                            obj.insert("copyright".into(), Json::String(v.clone()));
                            fields.copyright = Some(v.clone());
                        }
                    }
                    "description" => {
                        if let Some(v) = items.first() {
                            obj.insert("caption".into(), Json::String(v.clone()));
                            fields.caption = Some(v.clone());
                        }
                    }
                    _ => {}
                }
            }
        } else {
            let text = child.text().unwrap_or("").trim().to_string();
            if text.is_empty() {
                continue;
            }
            value_json = Json::String(text.clone());
            raw.insert(qname.clone(), value_json);

            // Map known simple fields
            match (ns, local) {
                ("http://ns.adobe.com/photoshop/1.0/", "City") => {
                    obj.insert("city".into(), Json::String(text));
                }
                ("http://ns.adobe.com/photoshop/1.0/", "Country") => {
                    obj.insert("country".into(), Json::String(text));
                }
                ("http://ns.adobe.com/photoshop/1.0/", "Headline") => {
                    obj.insert("headline".into(), Json::String(text));
                }
                ("http://ns.adobe.com/photoshop/1.0/", "Credit") => {
                    obj.insert("credit".into(), Json::String(text));
                }
                ("http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/", "Location") => {
                    obj.insert("location".into(), Json::String(text));
                }
                _ => {}
            }
        }
    }

    if obj.is_empty() && raw.is_empty() {
        return None;
    }

    obj.insert("_raw".into(), Json::Object(raw));
    Some((Json::Object(obj), fields))
}

// ── EXIF helpers ──────────────────────────────────────────────────────────────

fn ascii_field(exif: &exif::Exif, tag: Tag) -> Option<String> {
    exif.get_field(tag, In::PRIMARY).and_then(|f| {
        if let Value::Ascii(ref v) = f.value {
            v.first()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(|s| s.trim_end_matches('\0').trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        }
    })
}

fn rational_display(exif: &exif::Exif, tag: Tag) -> Option<String> {
    exif.get_field(tag, In::PRIMARY)
        .map(|f| f.display_value().with_unit(exif).to_string())
        .filter(|s| !s.is_empty())
}

fn short_or_long_field(exif: &exif::Exif, tag: Tag) -> Option<i64> {
    exif.get_field(tag, In::PRIMARY).and_then(|f| match &f.value {
        Value::Short(v) => v.first().map(|&n| n as i64),
        Value::Long(v) => v.first().map(|&n| n as i64),
        _ => None,
    })
}

fn rational_to_f64(rat: &exif::Rational) -> f64 {
    if rat.denom == 0 { 0.0 } else { rat.num as f64 / rat.denom as f64 }
}

fn gps_decimal(exif: &exif::Exif) -> Option<(f64, f64)> {
    let lat = gps_dms_to_decimal(exif, Tag::GPSLatitude, Tag::GPSLatitudeRef)?;
    let lon = gps_dms_to_decimal(exif, Tag::GPSLongitude, Tag::GPSLongitudeRef)?;
    Some((lat, lon))
}

fn gps_dms_to_decimal(exif: &exif::Exif, dms_tag: Tag, ref_tag: Tag) -> Option<f64> {
    let dms_field = exif.get_field(dms_tag, In::PRIMARY)?;
    let ref_field = exif.get_field(ref_tag, In::PRIMARY)?;

    let dms = match &dms_field.value {
        Value::Rational(v) if v.len() >= 3 => v,
        _ => return None,
    };
    let reference = match &ref_field.value {
        Value::Ascii(v) => v
            .first()
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(|s| s.trim_end_matches('\0').to_string())?,
        _ => return None,
    };

    let degrees = rational_to_f64(&dms[0]);
    let minutes = rational_to_f64(&dms[1]);
    let seconds = rational_to_f64(&dms[2]);
    let mut decimal = degrees + minutes / 60.0 + seconds / 3600.0;
    if reference == "S" || reference == "W" {
        decimal = -decimal;
    }
    Some(decimal)
}

fn gps_altitude(exif: &exif::Exif) -> Option<f64> {
    let alt_field = exif.get_field(Tag::GPSAltitude, In::PRIMARY)?;
    let alt = match &alt_field.value {
        Value::Rational(v) => v.first().map(rational_to_f64)?,
        _ => return None,
    };
    let below_sea = exif
        .get_field(Tag::GPSAltitudeRef, In::PRIMARY)
        .and_then(|f| match &f.value {
            Value::Byte(v) => v.first().copied(),
            _ => None,
        })
        .unwrap_or(0)
        == 1;
    Some(if below_sea { -alt } else { alt })
}
