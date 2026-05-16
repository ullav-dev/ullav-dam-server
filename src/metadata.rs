use exif::{In, Reader, Tag, Value};
use serde_json::{Map, Value as Json};
use std::io::Cursor;
use tracing::warn;
use uuid::Uuid;

/// Normalised result of EXIF extraction from a single file.
/// IPTC and XMP columns are retained in the DB schema for future use
/// but are not yet populated (kamadak-exif handles EXIF only).
#[derive(Debug, Default)]
pub struct ExtractedMetadata {
    /// Structured EXIF data including normalised fields and raw tags.
    pub exif: Option<Json>,
    /// Reserved for IPTC metadata (not yet implemented).
    pub iptc: Option<Json>,
    /// Reserved for XMP metadata (not yet implemented).
    pub xmp: Option<Json>,
    // Fields that can override the `assets` table — `None` means the file
    // had no value so the existing DB value should be preserved.
    pub creator: Option<String>,
    pub copyright: Option<String>,
    pub caption: Option<String>,
    /// Keywords are not available from EXIF; reserved for IPTC support.
    pub keywords: Option<String>,
}

/// Extract EXIF metadata from raw file bytes.
///
/// Always succeeds — any parse or decode error is logged and `Default` is
/// returned so upload handlers are never failed by bad metadata.
/// Intended to run inside `tokio::task::spawn_blocking`.
pub fn extract_from_bytes(data: &[u8]) -> ExtractedMetadata {
    let exif_reader = Reader::new();
    let exif = match exif_reader.read_from_container(&mut Cursor::new(data)) {
        Ok(e) => e,
        Err(e) => {
            warn!("EXIF parse skipped: {e}");
            return ExtractedMetadata::default();
        }
    };

    let mut obj = Map::new();

    // ── Normalised top-level fields ───────────────────────────────────────────

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

    // ── GPS — top-level for functional btree index support ────────────────────

    if let Some((lat, lon)) = gps_decimal(&exif) {
        obj.insert("gps_lat".into(), serde_json::json!(lat));
        obj.insert("gps_lon".into(), serde_json::json!(lon));
        if let Some(alt) = gps_altitude(&exif) {
            obj.insert("gps_alt".into(), serde_json::json!(alt));
        }
    }

    // ── Raw lossless dump of all EXIF fields ──────────────────────────────────

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

    if obj.is_empty() {
        return ExtractedMetadata::default();
    }

    // ── Override candidates from EXIF fallback tags ───────────────────────────
    let creator = ascii_field(&exif, Tag::Artist);
    let copyright = ascii_field(&exif, Tag::Copyright);
    let caption = ascii_field(&exif, Tag::ImageDescription);

    ExtractedMetadata {
        exif: Some(Json::Object(obj)),
        iptc: None,
        xmp: None,
        creator,
        copyright,
        caption,
        keywords: None,
    }
}

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

// ── EXIF field accessors ──────────────────────────────────────────────────────

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
    if rat.denom == 0 {
        0.0
    } else {
        rat.num as f64 / rat.denom as f64
    }
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
    let ref_field = exif.get_field(Tag::GPSAltitudeRef, In::PRIMARY);
    let below_sea = ref_field
        .and_then(|f| match &f.value {
            Value::Byte(v) => v.first().copied(),
            _ => None,
        })
        .unwrap_or(0)
        == 1;
    Some(if below_sea { -alt } else { alt })
}
