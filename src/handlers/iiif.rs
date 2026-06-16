use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    Json,
};
use image::{DynamicImage, ImageFormat, imageops};
use serde_json::{json, Value};
use std::io::Cursor;
use uuid::Uuid;

use crate::{
    AppState,
    error::{AppError, AppResult},
    handlers::assets::image_dimensions,
};

/// Pixel budget: reject images whose decoded dimensions would exceed this.
const IIIF_MAX_PIXELS: u64 = 50_000_000;

// ── Response helpers ──────────────────────────────────────────────────────────

struct IiifResponse(Value);

impl IntoResponse for IiifResponse {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            r#"application/ld+json; profile="http://iiif.io/api/presentation/3/context.json""#
                .parse()
                .unwrap(),
        );
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
        (StatusCode::OK, headers, Json(self.0)).into_response()
    }
}

struct IiifImageInfoResponse(Value);

impl IntoResponse for IiifImageInfoResponse {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            r#"application/ld+json; profile="http://iiif.io/api/image/3/context.json""#
                .parse()
                .unwrap(),
        );
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
        (StatusCode::OK, headers, Json(self.0)).into_response()
    }
}

struct IiifImageResponse {
    data: Vec<u8>,
    content_type: &'static str,
}

impl IntoResponse for IiifImageResponse {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, self.content_type.parse().unwrap());
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
        headers.insert(header::CACHE_CONTROL, "public, max-age=86400".parse().unwrap());
        (StatusCode::OK, headers, self.data).into_response()
    }
}

// ── IIIF Image API parameter types ───────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum IiifRegion {
    Full,
    Square,
    Pixels { x: u32, y: u32, w: u32, h: u32 },
    Pct { x: f64, y: f64, w: f64, h: f64 },
}

impl IiifRegion {
    fn parse(s: &str) -> AppResult<Self> {
        match s {
            "full" => Ok(Self::Full),
            "square" => Ok(Self::Square),
            s if s.starts_with("pct:") => {
                let nums: Vec<f64> = s[4..]
                    .split(',')
                    .map(|n| {
                        n.parse::<f64>()
                            .map_err(|_| AppError::BadRequest(format!("Invalid region pct: {s}")))
                    })
                    .collect::<AppResult<_>>()?;
                if nums.len() != 4 {
                    return Err(AppError::BadRequest(format!("Region pct needs 4 values: {s}")));
                }
                Ok(Self::Pct { x: nums[0], y: nums[1], w: nums[2], h: nums[3] })
            }
            s => {
                let nums: Vec<u32> = s
                    .split(',')
                    .map(|n| {
                        n.parse::<u32>()
                            .map_err(|_| AppError::BadRequest(format!("Invalid region: {s}")))
                    })
                    .collect::<AppResult<_>>()?;
                if nums.len() != 4 {
                    return Err(AppError::BadRequest(format!("Region needs 4 values: {s}")));
                }
                Ok(Self::Pixels { x: nums[0], y: nums[1], w: nums[2], h: nums[3] })
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum IiifSize {
    Max,
    Width(u32),
    Height(u32),
    Wh(u32, u32),
    BestFit(u32, u32),
    Pct(f64),
}

impl IiifSize {
    fn parse(s: &str) -> AppResult<Self> {
        let s = s.strip_prefix('^').unwrap_or(s); // ^ = upscaling allowed; treat same
        match s {
            "max" | "full" => Ok(Self::Max),
            s if s.starts_with("pct:") => {
                let p: f64 = s[4..].parse().map_err(|_| {
                    AppError::BadRequest(format!("Invalid size pct: {s}"))
                })?;
                if p <= 0.0 || p > 100.0 {
                    return Err(AppError::BadRequest(format!("Size pct must be in (0,100]: {p}")));
                }
                Ok(Self::Pct(p))
            }
            s if s.starts_with('!') => {
                let (w, h) = parse_wh(&s[1..])?;
                Ok(Self::BestFit(w, h))
            }
            s if s.ends_with(',') => {
                let w: u32 = s.trim_end_matches(',').parse().map_err(|_| {
                    AppError::BadRequest(format!("Invalid width: {s}"))
                })?;
                Ok(Self::Width(w))
            }
            s if s.starts_with(',') => {
                let h: u32 = s.trim_start_matches(',').parse().map_err(|_| {
                    AppError::BadRequest(format!("Invalid height: {s}"))
                })?;
                Ok(Self::Height(h))
            }
            s if s.contains(',') => {
                let (w, h) = parse_wh(s)?;
                Ok(Self::Wh(w, h))
            }
            _ => Err(AppError::BadRequest(format!("Unrecognised size: {s}"))),
        }
    }
}

fn parse_wh(s: &str) -> AppResult<(u32, u32)> {
    let mut it = s.splitn(2, ',');
    let w = it
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| AppError::BadRequest(format!("Invalid size: {s}")))?;
    let h = it
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| AppError::BadRequest(format!("Invalid size: {s}")))?;
    Ok((w, h))
}

#[derive(Debug, PartialEq)]
enum IiifRotation {
    Degrees(u16),
    Mirror(u16),
}

impl IiifRotation {
    fn parse(s: &str) -> AppResult<Self> {
        let (mirror, rest) = match s.strip_prefix('!') {
            Some(r) => (true, r),
            None => (false, s),
        };
        let deg: u16 = rest
            .parse()
            .map_err(|_| AppError::BadRequest(format!("Invalid rotation: {s}")))?;
        if deg % 90 != 0 {
            return Err(AppError::BadRequest(format!("Rotation must be a multiple of 90: {deg}")));
        }
        let deg = deg % 360;
        Ok(if mirror { Self::Mirror(deg) } else { Self::Degrees(deg) })
    }
}

#[derive(Debug, PartialEq)]
enum IiifQuality {
    Default,
    Color,
    Gray,
    Bitonal,
}

impl IiifQuality {
    fn parse(s: &str) -> AppResult<Self> {
        match s {
            "default" => Ok(Self::Default),
            "color" | "colour" => Ok(Self::Color),
            "gray" | "grey" => Ok(Self::Gray),
            "bitonal" => Ok(Self::Bitonal),
            _ => Err(AppError::BadRequest(format!("Unrecognised quality: {s}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum IiifFormat {
    Jpg,
    Png,
    Webp,
}

impl IiifFormat {
    fn parse(s: &str) -> AppResult<Self> {
        match s {
            "jpg" | "jpeg" => Ok(Self::Jpg),
            "png" => Ok(Self::Png),
            "webp" => Ok(Self::Webp),
            _ => Err(AppError::BadRequest(format!("Unsupported format: {s}"))),
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Jpg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    fn image_format(self) -> ImageFormat {
        match self {
            Self::Jpg => ImageFormat::Jpeg,
            Self::Png => ImageFormat::Png,
            Self::Webp => ImageFormat::WebP,
        }
    }
}

// ── Image processing pipeline ─────────────────────────────────────────────────

fn apply_region(img: DynamicImage, region: &IiifRegion) -> DynamicImage {
    let iw = img.width();
    let ih = img.height();
    match region {
        IiifRegion::Full => img,
        IiifRegion::Square => {
            let size = iw.min(ih);
            img.crop_imm((iw - size) / 2, (ih - size) / 2, size, size)
        }
        IiifRegion::Pixels { x, y, w, h } => {
            let x = (*x).min(iw.saturating_sub(1));
            let y = (*y).min(ih.saturating_sub(1));
            let w = (*w).min(iw - x).max(1);
            let h = (*h).min(ih - y).max(1);
            img.crop_imm(x, y, w, h)
        }
        IiifRegion::Pct { x, y, w, h } => {
            let px = ((iw as f64 * x / 100.0) as u32).min(iw.saturating_sub(1));
            let py = ((ih as f64 * y / 100.0) as u32).min(ih.saturating_sub(1));
            let pw = ((iw as f64 * w / 100.0) as u32).min(iw - px).max(1);
            let ph = ((ih as f64 * h / 100.0) as u32).min(ih - py).max(1);
            img.crop_imm(px, py, pw, ph)
        }
    }
}

fn apply_size(img: DynamicImage, size: &IiifSize) -> DynamicImage {
    let (iw, ih) = (img.width(), img.height());
    let f = imageops::FilterType::Lanczos3;
    match size {
        IiifSize::Max => img,
        IiifSize::Width(w) => img.resize(*w, u32::MAX, f),
        IiifSize::Height(h) => img.resize(u32::MAX, *h, f),
        IiifSize::Wh(w, h) => img.resize_exact(*w, *h, f),
        IiifSize::BestFit(w, h) => img.resize(*w, *h, f),
        IiifSize::Pct(p) => {
            let nw = ((iw as f64 * p / 100.0) as u32).max(1);
            let nh = ((ih as f64 * p / 100.0) as u32).max(1);
            img.resize_exact(nw, nh, f)
        }
    }
}

fn apply_rotation(img: DynamicImage, rotation: &IiifRotation) -> DynamicImage {
    match rotation {
        IiifRotation::Degrees(0) => img,
        IiifRotation::Degrees(90) => img.rotate90(),
        IiifRotation::Degrees(180) => img.rotate180(),
        IiifRotation::Degrees(270) => img.rotate270(),
        IiifRotation::Mirror(0) => img.fliph(),
        IiifRotation::Mirror(90) => img.rotate90().fliph(),
        IiifRotation::Mirror(180) => img.rotate180().fliph(),
        IiifRotation::Mirror(270) => img.rotate270().fliph(),
        _ => img,
    }
}

fn apply_quality(img: DynamicImage, quality: &IiifQuality) -> DynamicImage {
    match quality {
        IiifQuality::Default | IiifQuality::Color => img,
        IiifQuality::Gray => DynamicImage::ImageLuma8(img.to_luma8()),
        IiifQuality::Bitonal => {
            let mut gray = img.to_luma8();
            for px in gray.pixels_mut() {
                px.0[0] = if px.0[0] >= 128 { 255 } else { 0 };
            }
            DynamicImage::ImageLuma8(gray)
        }
    }
}

fn encode_image(img: &DynamicImage, format: IiifFormat) -> AppResult<Vec<u8>> {
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), format.image_format())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Image encode error: {e}")))?;
    Ok(buf)
}

/// Compute halved sizes down to a minimum of 128px on the shorter side.
fn compute_sizes(width: u32, height: u32) -> Vec<Value> {
    let mut sizes = vec![json!({"width": width, "height": height})];
    let mut w = width;
    let mut h = height;
    loop {
        w /= 2;
        h /= 2;
        if w < 128 || h < 128 {
            break;
        }
        sizes.push(json!({"width": w, "height": h}));
    }
    sizes
}

/// Returns true for raster image MIME types the `image` crate can process.
fn is_raster_image(asset_type: &str) -> bool {
    asset_type.starts_with("image/") && asset_type != "image/svg+xml"
}

// ── Manifest ─────────────────────────────────────────────────────────────────

/// Returns a IIIF Presentation API 3.0 Manifest for a single asset.
///
/// Public assets are served without authentication so external IIIF viewers
/// (Universal Viewer, Mirador) can load them directly. Private assets return 404.
/// Raster image assets include an ImageService3 service reference in the canvas body.
#[utoipa::path(
    get,
    path = "/iiif/manifest/{id}",
    tag = "iiif",
    params(
        ("id" = Uuid, Path, description = "Asset ID"),
    ),
    responses(
        (status = 200, description = "IIIF Manifest 3.0"),
        (status = 404, description = "Asset not found or private"),
    )
)]
pub async fn get_manifest(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            "SELECT name, description, asset_type, caption, keywords, creator, \
                    copyright_notice, is_private, width, height, storage_key \
             FROM assets WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let is_private: bool = row.get("is_private");
    if is_private {
        return Err(AppError::NotFound(format!("Asset {id} not found")));
    }

    let name: String = row.get("name");
    let description: Option<String> = row.get("description");
    let asset_type: String = row.get("asset_type");
    let caption: Option<String> = row.get("caption");
    let keywords: Option<String> = row.get("keywords");
    let creator: Option<String> = row.get("creator");
    let copyright_notice: Option<String> = row.get("copyright_notice");
    let mut width: Option<i32> = row.get("width");
    let mut height: Option<i32> = row.get("height");
    let storage_key: String = row.get("storage_key");

    // Lazy dimension population for pre-existing assets uploaded before migration.
    if width.is_none() && asset_type.starts_with("image/") {
        if let Ok(data) = state.storage.download(&storage_key).await {
            if let Some((w, h)) = image_dimensions(&data) {
                width = Some(w);
                height = Some(h);
                let _ = client
                    .execute(
                        "UPDATE assets SET width = $1, height = $2 WHERE id = $3",
                        &[&w, &h, &id],
                    )
                    .await;
            }
        }
    }

    let base = &state.public_base_url;
    let manifest_id = format!("{base}/iiif/manifest/{id}");
    let canvas_id = format!("{manifest_id}/canvas/1");
    let page_id = format!("{canvas_id}/page");
    let annotation_id = format!("{canvas_id}/annotation/1");
    let thumbnail_url = format!("{base}/assets/{id}/thumbnail");

    let mut metadata_entries: Vec<Value> = Vec::new();
    if let Some(c) = &creator {
        metadata_entries.push(json!({
            "label": {"en": ["Creator"]},
            "value": {"en": [c]}
        }));
    }
    if let Some(cap) = caption.as_ref().or(description.as_ref()) {
        metadata_entries.push(json!({
            "label": {"en": ["Description"]},
            "value": {"en": [cap]}
        }));
    }
    if let Some(kw) = &keywords {
        let kw_list: Vec<&str> = kw.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        if !kw_list.is_empty() {
            metadata_entries.push(json!({
                "label": {"en": ["Keywords"]},
                "value": {"en": kw_list}
            }));
        }
    }

    // For raster images use the IIIF Image API URL so viewers get deep zoom.
    // For other asset types fall back to the download URL.
    let body = if is_raster_image(&asset_type) {
        let image_base = format!("{base}/iiif/image/{id}");
        let mut b = json!({
            "id": format!("{image_base}/full/max/0/default.jpg"),
            "type": "Image",
            "format": asset_type,
            "service": [{
                "id": image_base,
                "type": "ImageService3",
                "profile": "level2"
            }]
        });
        if let (Some(w), Some(h)) = (width, height) {
            b["width"] = json!(w);
            b["height"] = json!(h);
        }
        b
    } else {
        let mut b = json!({
            "id": format!("{base}/assets/{id}/download"),
            "type": "Image",
            "format": asset_type,
        });
        if let (Some(w), Some(h)) = (width, height) {
            b["width"] = json!(w);
            b["height"] = json!(h);
        }
        b
    };

    let mut canvas = json!({
        "id": canvas_id,
        "type": "Canvas",
        "items": [{
            "id": page_id,
            "type": "AnnotationPage",
            "items": [{
                "id": annotation_id,
                "type": "Annotation",
                "motivation": "painting",
                "body": body,
                "target": canvas_id
            }]
        }]
    });

    if let (Some(w), Some(h)) = (width, height) {
        canvas["width"] = json!(w);
        canvas["height"] = json!(h);
    }

    let mut manifest = json!({
        "@context": "http://iiif.io/api/presentation/3/context.json",
        "id": manifest_id,
        "type": "Manifest",
        "label": {"en": [name]},
        "thumbnail": [{
            "id": thumbnail_url,
            "type": "Image",
            "format": "image/png"
        }],
        "items": [canvas]
    });

    if !metadata_entries.is_empty() {
        manifest["metadata"] = json!(metadata_entries);
    }
    if let Some(cr) = &copyright_notice {
        manifest["rights"] = json!(cr);
    }

    Ok(IiifResponse(manifest).into_response())
}

// ── Collection ────────────────────────────────────────────────────────────────

/// Returns a IIIF Presentation API 3.0 Collection for a category.
///
/// Includes manifest stubs for all non-private assets in the category and
/// collection stubs for each direct sub-category. Private categories return 404.
#[utoipa::path(
    get,
    path = "/iiif/collection/{id}",
    tag = "iiif",
    params(
        ("id" = Uuid, Path, description = "Category ID"),
    ),
    responses(
        (status = 200, description = "IIIF Collection 3.0"),
        (status = 404, description = "Category not found or private"),
    )
)]
pub async fn get_collection(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let client = state.db.get().await?;

    let cat_row = client
        .query_opt(
            "SELECT name, description, access_level::text FROM categories WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Category {id} not found")))?;

    let access_level: String = cat_row.get("access_level");
    if access_level == "Private" {
        return Err(AppError::NotFound(format!("Category {id} not found")));
    }

    let cat_name: String = cat_row.get("name");
    let cat_description: Option<String> = cat_row.get("description");

    let base = &state.public_base_url;
    let collection_id = format!("{base}/iiif/collection/{id}");

    let asset_rows = client
        .query(
            "SELECT a.id, a.name \
             FROM assets a \
             JOIN asset_categories ac ON ac.asset_id = a.id \
             WHERE ac.category_id = $1 AND a.is_private = false \
             ORDER BY a.name ASC",
            &[&id],
        )
        .await?;

    let sub_rows = client
        .query(
            "SELECT id, name, access_level::text \
             FROM categories \
             WHERE parent_id = $1 AND access_level != 'Private' \
             ORDER BY name ASC",
            &[&id],
        )
        .await?;

    let mut items: Vec<Value> = Vec::new();

    for row in &sub_rows {
        let sub_id: Uuid = row.get("id");
        let sub_name: String = row.get("name");
        items.push(json!({
            "id": format!("{base}/iiif/collection/{sub_id}"),
            "type": "Collection",
            "label": {"en": [sub_name]}
        }));
    }

    for row in &asset_rows {
        let asset_id: Uuid = row.get("id");
        let asset_name: String = row.get("name");
        items.push(json!({
            "id": format!("{base}/iiif/manifest/{asset_id}"),
            "type": "Manifest",
            "label": {"en": [asset_name]},
            "thumbnail": [{
                "id": format!("{base}/assets/{asset_id}/thumbnail"),
                "type": "Image",
                "format": "image/png"
            }]
        }));
    }

    let mut collection = json!({
        "@context": "http://iiif.io/api/presentation/3/context.json",
        "id": collection_id,
        "type": "Collection",
        "label": {"en": [cat_name]},
        "items": items
    });

    if let Some(desc) = cat_description {
        collection["summary"] = json!({"en": [desc]});
    }

    Ok(IiifResponse(collection).into_response())
}

// ── Image API — info.json ─────────────────────────────────────────────────────

/// Returns a IIIF Image API 3.0 service description for a raster image asset.
///
/// Advertises Level 2 compliance with sizes (no tiles). Private assets and
/// non-raster types return 404/501 respectively.
#[utoipa::path(
    get,
    path = "/iiif/image/{id}/info.json",
    tag = "iiif",
    params(
        ("id" = Uuid, Path, description = "Asset ID"),
    ),
    responses(
        (status = 200, description = "IIIF Image API 3.0 service description (Level 2)"),
        (status = 404, description = "Asset not found or private"),
        (status = 501, description = "Asset is not a raster image"),
    )
)]
pub async fn get_image_info(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let client = state.db.get().await?;

    let row = client
        .query_opt(
            "SELECT asset_type, is_private, width, height, storage_key FROM assets WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let is_private: bool = row.get("is_private");
    if is_private {
        return Err(AppError::NotFound(format!("Asset {id} not found")));
    }

    let asset_type: String = row.get("asset_type");
    if !is_raster_image(&asset_type) {
        return Err(AppError::NotImplemented(
            "IIIF Image API is only available for raster image assets".into(),
        ));
    }

    let mut width: Option<i32> = row.get("width");
    let mut height: Option<i32> = row.get("height");
    let storage_key: String = row.get("storage_key");

    // Lazy dimension population
    if width.is_none() {
        if let Ok(data) = state.storage.download(&storage_key).await {
            if let Some((w, h)) = image_dimensions(&data) {
                width = Some(w);
                height = Some(h);
                let _ = client
                    .execute(
                        "UPDATE assets SET width = $1, height = $2 WHERE id = $3",
                        &[&w, &h, &id],
                    )
                    .await;
            }
        }
    }

    let (w, h) = match (width, height) {
        (Some(w), Some(h)) => (w as u32, h as u32),
        _ => return Err(AppError::Internal(anyhow::anyhow!("Could not determine image dimensions for {id}"))),
    };

    let base = &state.public_base_url;
    let image_base = format!("{base}/iiif/image/{id}");
    let sizes = compute_sizes(w, h);

    let info = json!({
        "@context": "http://iiif.io/api/image/3/context.json",
        "id": image_base,
        "type": "ImageService3",
        "protocol": "http://iiif.io/api/image",
        "profile": "level2",
        "width": w,
        "height": h,
        "sizes": sizes,
        "extraFormats": ["png", "webp"]
    });

    Ok(IiifImageInfoResponse(info).into_response())
}

// ── Image API — image delivery ────────────────────────────────────────────────

/// Delivers a parameterised image region from a raster asset per IIIF Image API 3.0 Level 2.
///
/// `quality_fmt` is of the form `quality.format`, e.g. `default.jpg`, `gray.png`, `color.webp`.
/// All image processing runs on a blocking thread to avoid stalling the async executor.
#[utoipa::path(
    get,
    path = "/iiif/image/{id}/{region}/{size}/{rotation}/{quality_fmt}",
    tag = "iiif",
    params(
        ("id" = Uuid, Path, description = "Asset ID"),
        ("region" = String, Path, description = "Region: full | square | x,y,w,h | pct:x,y,w,h"),
        ("size" = String, Path, description = "Size: max | w, | ,h | w,h | !w,h | pct:n"),
        ("rotation" = String, Path, description = "Rotation: 0 | 90 | 180 | 270 (prefix ! for mirror)"),
        ("quality_fmt" = String, Path, description = "Quality.format e.g. default.jpg | gray.png | color.webp"),
    ),
    responses(
        (status = 200, description = "Processed image bytes"),
        (status = 400, description = "Invalid or unsupported parameters"),
        (status = 404, description = "Asset not found or private"),
        (status = 501, description = "Asset is not a raster image"),
    )
)]
pub async fn get_image(
    State(state): State<AppState>,
    Path((id, region_str, size_str, rotation_str, quality_fmt)): Path<(Uuid, String, String, String, String)>,
) -> AppResult<Response> {
    // Parse parameters early so we can return 400 before touching storage.
    let region = IiifRegion::parse(&region_str)?;
    let size = IiifSize::parse(&size_str)?;
    let rotation = IiifRotation::parse(&rotation_str)?;

    let (quality_str, format_str) = quality_fmt.rsplit_once('.').ok_or_else(|| {
        AppError::BadRequest(format!("Expected 'quality.format', got: {quality_fmt}"))
    })?;
    let quality = IiifQuality::parse(quality_str)?;
    let format = IiifFormat::parse(format_str)?;

    // DB + visibility check
    let client = state.db.get().await?;
    let row = client
        .query_opt(
            "SELECT asset_type, is_private, storage_key FROM assets WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Asset {id} not found")))?;

    let is_private: bool = row.get("is_private");
    if is_private {
        return Err(AppError::NotFound(format!("Asset {id} not found")));
    }

    let asset_type: String = row.get("asset_type");
    if !is_raster_image(&asset_type) {
        return Err(AppError::NotImplemented(
            "IIIF Image API is only available for raster image assets".into(),
        ));
    }

    let storage_key: String = row.get("storage_key");
    let bytes = state.storage.download(&storage_key).await?;

    // Run CPU-bound decode + transform on a blocking thread.
    let data = tokio::task::spawn_blocking(move || -> AppResult<Vec<u8>> {
        let img = image::load_from_memory(&bytes)
            .map_err(|e| AppError::BadRequest(format!("Cannot decode image: {e}")))?;

        // Pixel budget check after decode (actual decoded dimensions, not DB values).
        if (img.width() as u64) * (img.height() as u64) > IIIF_MAX_PIXELS {
            return Err(AppError::BadRequest(format!(
                "Image {}×{} exceeds maximum supported size ({} MP)",
                img.width(),
                img.height(),
                IIIF_MAX_PIXELS / 1_000_000
            )));
        }

        let img = apply_region(img, &region);
        let img = apply_size(img, &size);
        let img = apply_rotation(img, &rotation);
        let img = apply_quality(img, &quality);
        encode_image(&img, format)
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("Task error: {e}")))??;

    Ok(IiifImageResponse {
        data,
        content_type: format.content_type(),
    }
    .into_response())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Manifest structure ──────────────────────────────────────────────────

    fn sample_manifest_download() -> Value {
        // Non-image asset — body stays as download URL, no service
        let base = "https://comad-tip.stage.ullav.setanta.dev";
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let manifest_id = format!("{base}/iiif/manifest/{id}");
        let canvas_id = format!("{manifest_id}/canvas/1");
        json!({
            "@context": "http://iiif.io/api/presentation/3/context.json",
            "id": manifest_id,
            "type": "Manifest",
            "label": {"en": ["Test PDF"]},
            "thumbnail": [{"id": format!("{base}/assets/{id}/thumbnail"), "type": "Image", "format": "image/png"}],
            "items": [{
                "id": canvas_id,
                "type": "Canvas",
                "items": [{
                    "id": format!("{canvas_id}/page"),
                    "type": "AnnotationPage",
                    "items": [{
                        "id": format!("{canvas_id}/annotation/1"),
                        "type": "Annotation",
                        "motivation": "painting",
                        "body": {
                            "id": format!("{base}/assets/{id}/download"),
                            "type": "Image",
                            "format": "application/pdf",
                        },
                        "target": canvas_id
                    }]
                }]
            }]
        })
    }

    fn sample_manifest_image() -> Value {
        // Raster image asset — body uses Image API URL with service
        let base = "https://comad-tip.stage.ullav.setanta.dev";
        let id = "550e8400-e29b-41d4-a716-446655440001";
        let manifest_id = format!("{base}/iiif/manifest/{id}");
        let canvas_id = format!("{manifest_id}/canvas/1");
        let image_base = format!("{base}/iiif/image/{id}");
        json!({
            "@context": "http://iiif.io/api/presentation/3/context.json",
            "id": manifest_id,
            "type": "Manifest",
            "label": {"en": ["Test Image"]},
            "thumbnail": [{"id": format!("{base}/assets/{id}/thumbnail"), "type": "Image", "format": "image/png"}],
            "items": [{
                "id": canvas_id,
                "type": "Canvas",
                "width": 1024,
                "height": 768,
                "items": [{
                    "id": format!("{canvas_id}/page"),
                    "type": "AnnotationPage",
                    "items": [{
                        "id": format!("{canvas_id}/annotation/1"),
                        "type": "Annotation",
                        "motivation": "painting",
                        "body": {
                            "id": format!("{image_base}/full/max/0/default.jpg"),
                            "type": "Image",
                            "format": "image/jpeg",
                            "width": 1024,
                            "height": 768,
                            "service": [{"id": image_base, "type": "ImageService3", "profile": "level2"}]
                        },
                        "target": canvas_id
                    }]
                }]
            }]
        })
    }

    #[test]
    fn manifest_has_required_iiif_fields() {
        let m = sample_manifest_image();
        assert_eq!(m["@context"], "http://iiif.io/api/presentation/3/context.json");
        assert_eq!(m["type"], "Manifest");
        assert!(m["id"].as_str().unwrap().contains("/iiif/manifest/"));
        assert!(m["label"]["en"].is_array());
        assert!(m["items"].is_array());
        assert!(!m["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn image_manifest_body_references_image_api() {
        let m = sample_manifest_image();
        let body = &m["items"][0]["items"][0]["items"][0]["body"];
        assert!(body["id"].as_str().unwrap().contains("/iiif/image/"));
        assert!(body["id"].as_str().unwrap().contains("/full/max/0/default.jpg"));
        let service = &body["service"][0];
        assert_eq!(service["type"], "ImageService3");
        assert_eq!(service["profile"], "level2");
        assert!(service["id"].as_str().unwrap().contains("/iiif/image/"));
    }

    #[test]
    fn non_image_manifest_body_uses_download_url() {
        let m = sample_manifest_download();
        let body = &m["items"][0]["items"][0]["items"][0]["body"];
        assert!(body["id"].as_str().unwrap().contains("/assets/"));
        assert!(body["id"].as_str().unwrap().contains("/download"));
        assert!(body["service"].is_null());
    }

    fn sample_collection() -> Value {
        let base = "https://comad-tip.stage.ullav.setanta.dev";
        let id = "660e8400-e29b-41d4-a716-446655440000";
        json!({
            "@context": "http://iiif.io/api/presentation/3/context.json",
            "id": format!("{base}/iiif/collection/{id}"),
            "type": "Collection",
            "label": {"en": ["Test Collection"]},
            "summary": {"en": ["A description"]},
            "items": [
                {"id": format!("{base}/iiif/manifest/aaa"), "type": "Manifest", "label": {"en": ["Asset A"]}},
                {"id": format!("{base}/iiif/collection/bbb"), "type": "Collection", "label": {"en": ["Sub"]}}
            ]
        })
    }

    #[test]
    fn collection_has_required_iiif_fields() {
        let c = sample_collection();
        assert_eq!(c["@context"], "http://iiif.io/api/presentation/3/context.json");
        assert_eq!(c["type"], "Collection");
        assert!(c["id"].as_str().unwrap().contains("/iiif/collection/"));
        assert!(c["label"]["en"].is_array());
        assert!(c["items"].is_array());
    }

    #[test]
    fn collection_items_have_type_and_id() {
        let c = sample_collection();
        for item in c["items"].as_array().unwrap() {
            assert!(item["id"].is_string());
            assert!(item["type"].is_string());
            assert!(item["label"]["en"].is_array());
        }
    }

    // ── info.json ───────────────────────────────────────────────────────────

    #[test]
    fn compute_sizes_halves_until_128() {
        let sizes = compute_sizes(1024, 768);
        assert_eq!(sizes[0], json!({"width": 1024, "height": 768}));
        assert_eq!(sizes[1], json!({"width": 512, "height": 384}));
        assert_eq!(sizes[2], json!({"width": 256, "height": 192}));
        // 128×96: h=96 < 128 so the loop breaks before adding this entry
        assert_eq!(sizes.len(), 3);
    }

    #[test]
    fn compute_sizes_small_image_returns_only_original() {
        let sizes = compute_sizes(200, 100);
        // halved = 100×50, 50 < 128 so only original
        assert_eq!(sizes.len(), 1);
    }

    // ── Region parsing ──────────────────────────────────────────────────────

    #[test]
    fn parse_region_full() {
        assert_eq!(IiifRegion::parse("full").unwrap(), IiifRegion::Full);
    }

    #[test]
    fn parse_region_square() {
        assert_eq!(IiifRegion::parse("square").unwrap(), IiifRegion::Square);
    }

    #[test]
    fn parse_region_pixels() {
        assert_eq!(
            IiifRegion::parse("10,20,100,200").unwrap(),
            IiifRegion::Pixels { x: 10, y: 20, w: 100, h: 200 }
        );
    }

    #[test]
    fn parse_region_pct() {
        match IiifRegion::parse("pct:10,20,50,60").unwrap() {
            IiifRegion::Pct { x, y, w, h } => {
                assert!((x - 10.0).abs() < f64::EPSILON);
                assert!((y - 20.0).abs() < f64::EPSILON);
                assert!((w - 50.0).abs() < f64::EPSILON);
                assert!((h - 60.0).abs() < f64::EPSILON);
            }
            _ => panic!("expected Pct"),
        }
    }

    #[test]
    fn parse_region_invalid_returns_400() {
        assert!(IiifRegion::parse("notvalid").is_err());
        assert!(IiifRegion::parse("1,2,3").is_err());       // only 3 values
        assert!(IiifRegion::parse("pct:1,2,3").is_err());   // only 3 pct values
    }

    // ── Size parsing ────────────────────────────────────────────────────────

    #[test]
    fn parse_size_max() {
        assert_eq!(IiifSize::parse("max").unwrap(), IiifSize::Max);
        assert_eq!(IiifSize::parse("full").unwrap(), IiifSize::Max);
        assert_eq!(IiifSize::parse("^max").unwrap(), IiifSize::Max);
    }

    #[test]
    fn parse_size_width_only() {
        assert_eq!(IiifSize::parse("400,").unwrap(), IiifSize::Width(400));
    }

    #[test]
    fn parse_size_height_only() {
        assert_eq!(IiifSize::parse(",300").unwrap(), IiifSize::Height(300));
    }

    #[test]
    fn parse_size_exact_wh() {
        assert_eq!(IiifSize::parse("400,300").unwrap(), IiifSize::Wh(400, 300));
    }

    #[test]
    fn parse_size_best_fit() {
        assert_eq!(IiifSize::parse("!400,300").unwrap(), IiifSize::BestFit(400, 300));
    }

    #[test]
    fn parse_size_pct() {
        match IiifSize::parse("pct:50").unwrap() {
            IiifSize::Pct(p) => assert!((p - 50.0).abs() < f64::EPSILON),
            _ => panic!("expected Pct"),
        }
    }

    #[test]
    fn parse_size_pct_out_of_range() {
        assert!(IiifSize::parse("pct:0").is_err());
        assert!(IiifSize::parse("pct:101").is_err());
    }

    #[test]
    fn parse_size_invalid() {
        assert!(IiifSize::parse("xyz").is_err());
    }

    // ── Rotation parsing ────────────────────────────────────────────────────

    #[test]
    fn parse_rotation_degrees() {
        assert_eq!(IiifRotation::parse("0").unwrap(), IiifRotation::Degrees(0));
        assert_eq!(IiifRotation::parse("90").unwrap(), IiifRotation::Degrees(90));
        assert_eq!(IiifRotation::parse("180").unwrap(), IiifRotation::Degrees(180));
        assert_eq!(IiifRotation::parse("270").unwrap(), IiifRotation::Degrees(270));
        // 360 wraps to 0
        assert_eq!(IiifRotation::parse("360").unwrap(), IiifRotation::Degrees(0));
    }

    #[test]
    fn parse_rotation_mirror() {
        assert_eq!(IiifRotation::parse("!0").unwrap(), IiifRotation::Mirror(0));
        assert_eq!(IiifRotation::parse("!90").unwrap(), IiifRotation::Mirror(90));
    }

    #[test]
    fn parse_rotation_non_multiple_of_90_is_error() {
        assert!(IiifRotation::parse("45").is_err());
        assert!(IiifRotation::parse("!45").is_err());
    }

    // ── Quality + format parsing ────────────────────────────────────────────

    #[test]
    fn parse_quality_variants() {
        assert_eq!(IiifQuality::parse("default").unwrap(), IiifQuality::Default);
        assert_eq!(IiifQuality::parse("color").unwrap(), IiifQuality::Color);
        assert_eq!(IiifQuality::parse("colour").unwrap(), IiifQuality::Color);
        assert_eq!(IiifQuality::parse("gray").unwrap(), IiifQuality::Gray);
        assert_eq!(IiifQuality::parse("grey").unwrap(), IiifQuality::Gray);
        assert_eq!(IiifQuality::parse("bitonal").unwrap(), IiifQuality::Bitonal);
        assert!(IiifQuality::parse("vivid").is_err());
    }

    #[test]
    fn parse_format_variants() {
        assert_eq!(IiifFormat::parse("jpg").unwrap(), IiifFormat::Jpg);
        assert_eq!(IiifFormat::parse("jpeg").unwrap(), IiifFormat::Jpg);
        assert_eq!(IiifFormat::parse("png").unwrap(), IiifFormat::Png);
        assert_eq!(IiifFormat::parse("webp").unwrap(), IiifFormat::Webp);
        assert!(IiifFormat::parse("gif").is_err());
        assert!(IiifFormat::parse("tif").is_err());
    }

    // ── Image processing ────────────────────────────────────────────────────

    fn test_image_rgb(w: u32, h: u32) -> DynamicImage {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([((x * 255 / w) as u8), ((y * 255 / h) as u8), 128])
        });
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn apply_region_full_preserves_dimensions() {
        let img = test_image_rgb(100, 80);
        let out = apply_region(img, &IiifRegion::Full);
        assert_eq!(out.width(), 100);
        assert_eq!(out.height(), 80);
    }

    #[test]
    fn apply_region_square_takes_min_side() {
        let img = test_image_rgb(100, 80);
        let out = apply_region(img, &IiifRegion::Square);
        assert_eq!(out.width(), 80);
        assert_eq!(out.height(), 80);
    }

    #[test]
    fn apply_region_pixels_crops_correctly() {
        let img = test_image_rgb(100, 80);
        let out = apply_region(img, &IiifRegion::Pixels { x: 10, y: 10, w: 50, h: 40 });
        assert_eq!(out.width(), 50);
        assert_eq!(out.height(), 40);
    }

    #[test]
    fn apply_size_max_preserves_dimensions() {
        let img = test_image_rgb(100, 80);
        let out = apply_size(img, &IiifSize::Max);
        assert_eq!(out.width(), 100);
        assert_eq!(out.height(), 80);
    }

    #[test]
    fn apply_size_exact_wh() {
        let img = test_image_rgb(100, 80);
        let out = apply_size(img, &IiifSize::Wh(60, 40));
        assert_eq!(out.width(), 60);
        assert_eq!(out.height(), 40);
    }

    #[test]
    fn apply_rotation_90_swaps_dimensions() {
        let img = test_image_rgb(100, 80);
        let out = apply_rotation(img, &IiifRotation::Degrees(90));
        assert_eq!(out.width(), 80);
        assert_eq!(out.height(), 100);
    }

    #[test]
    fn apply_quality_gray_produces_luma_image() {
        let img = test_image_rgb(10, 10);
        let out = apply_quality(img, &IiifQuality::Gray);
        // Should be a grayscale image — check it encodes to PNG without error
        let mut buf = Vec::new();
        out.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png).unwrap();
        assert!(!buf.is_empty());
    }

    #[test]
    fn apply_quality_bitonal_produces_two_values() {
        let img = test_image_rgb(4, 4);
        let out = apply_quality(img, &IiifQuality::Bitonal);
        if let DynamicImage::ImageLuma8(luma) = out {
            for px in luma.pixels() {
                assert!(px.0[0] == 0 || px.0[0] == 255);
            }
        } else {
            panic!("expected ImageLuma8");
        }
    }

    #[test]
    fn encode_image_jpg_produces_non_empty_bytes() {
        let img = test_image_rgb(64, 64);
        let bytes = encode_image(&img, IiifFormat::Jpg).unwrap();
        assert!(!bytes.is_empty());
        // JPEG starts with FF D8
        assert_eq!(bytes[0], 0xFF);
        assert_eq!(bytes[1], 0xD8);
    }

    #[test]
    fn encode_image_png_produces_non_empty_bytes() {
        let img = test_image_rgb(64, 64);
        let bytes = encode_image(&img, IiifFormat::Png).unwrap();
        assert!(!bytes.is_empty());
        // PNG starts with 89 50 4E 47
        assert_eq!(&bytes[0..4], b"\x89PNG");
    }
}
