use std::collections::{BTreeSet, HashMap};
use std::io::{Cursor, Read};

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;
use uuid::Uuid;
use zip::ZipArchive;

use crate::{
    AppState,
    error::{AppError, AppResult},
    models::{asset::Asset, category::Category},
};

const CATEGORY_COLUMNS: &str = "id, name, description, parent_id, created_at, updated_at";

const ASSET_COLUMNS: &str =
    "id, name, description, asset_type, size, storage_key, bucket, \
     caption, keywords, creator, copyright_notice, available, available_until, \
     is_locked, created_at, updated_at";

#[derive(Serialize)]
pub struct ZipUploadResult {
    /// All categories created (root + sub-directories).
    pub categories: Vec<Category>,
    /// All assets successfully uploaded from the ZIP.
    pub assets: Vec<Asset>,
    /// asset_id → [category_id] for every uploaded asset.
    pub asset_category_ids: HashMap<Uuid, Vec<Uuid>>,
    /// Per-entry errors (non-fatal; other entries still processed).
    pub errors: Vec<String>,
}

/// Returns `true` for macOS-generated artifact entries that should be skipped.
fn is_artifact(path: &str) -> bool {
    if path.starts_with("__MACOSX/") {
        return true;
    }
    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    filename == ".DS_Store" || filename.starts_with("._")
}

/// POST /zip/upload
///
/// Accepts a multipart form with a `file` field containing a ZIP archive.
/// Creates a category tree mirroring the ZIP's directory structure (root
/// category named `<stem>-<YYYYMMDD-HHMMSS>`), uploads every file as an
/// asset linked to its directory's category, and returns the full result.
/// The ZIP itself is never stored as an asset.
pub async fn upload_zip(
    State(state): State<AppState>,
    mut multipart: axum::extract::Multipart,
) -> AppResult<(StatusCode, Json<ZipUploadResult>)> {
    // ── 1. Parse multipart ───────────────────────────────────────────────────
    let mut file_bytes: Option<bytes::Bytes> = None;
    let mut file_name = "archive".to_string();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        if matches!(field.name(), Some("file") | None) {
            if let Some(fname) = field.file_name() {
                file_name = fname.to_string();
            }
            file_bytes = Some(
                field.bytes().await.map_err(|e| AppError::BadRequest(e.to_string()))?,
            );
        } else {
            let _ = field.bytes().await;
        }
    }

    let data = file_bytes.ok_or_else(|| AppError::BadRequest("Missing file field".into()))?;

    // ── 2. Build root category name: <stem>-<YYYYMMDD-HHMMSS> ───────────────
    let stem = std::path::Path::new(&file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("archive")
        .to_string();
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let root_cat_name = format!("{stem}-{ts}");

    // ── 3. Open ZIP ──────────────────────────────────────────────────────────
    let mut archive = ZipArchive::new(Cursor::new(data.to_vec()))
        .map_err(|e| AppError::BadRequest(format!("Not a valid ZIP file: {e}")))?;

    // ── 4. Scan entries: collect dir paths + file indices ────────────────────
    // We need two passes over the archive; collect lightweight metadata first.
    let entry_meta: Vec<(String, bool)> = (0..archive.len())
        .filter_map(|i| {
            let entry = archive.by_index(i).ok()?;
            Some((entry.name().to_string(), entry.is_dir()))
        })
        .collect();

    let mut dir_paths: BTreeSet<String> = BTreeSet::new();
    let mut file_indices: Vec<usize> = Vec::new();

    for (i, (path, is_dir)) in entry_meta.iter().enumerate() {
        if is_artifact(path) {
            continue;
        }
        if *is_dir {
            let dir = path.trim_end_matches('/').to_string();
            if !dir.is_empty() {
                dir_paths.insert(dir);
            }
        } else {
            // Collect all implicit parent directories from file paths.
            let mut p = std::path::Path::new(path.as_str());
            while let Some(parent) = p.parent() {
                let ps = parent.to_str().unwrap_or("");
                if ps.is_empty() {
                    break;
                }
                dir_paths.insert(ps.to_string());
                p = parent;
            }
            file_indices.push(i);
        }
    }

    // Sort by depth so parent categories are always created before children.
    let mut sorted_dirs: Vec<String> = dir_paths.into_iter().collect();
    sorted_dirs.sort_by_key(|s| s.matches('/').count());

    // ── 5. Create root category ──────────────────────────────────────────────
    let client = state.db.get().await?;
    let root_id = Uuid::new_v4();

    let root_row = client
        .query_one(
            &format!(
                "INSERT INTO categories (id, name) VALUES ($1, $2) \
                 RETURNING {CATEGORY_COLUMNS}"
            ),
            &[&root_id, &root_cat_name],
        )
        .await?;

    let mut categories: Vec<Category> = vec![Category::from(&root_row)];
    // Maps normalised dir path → category UUID; "" = root.
    let mut dir_to_cat: HashMap<String, Uuid> = HashMap::new();
    dir_to_cat.insert(String::new(), root_id);

    // ── 6. Create sub-categories ─────────────────────────────────────────────
    for dir in &sorted_dirs {
        let dir_path = std::path::Path::new(dir.as_str());
        let parent_str = dir_path
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("");
        let cat_name = dir_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(dir.as_str());

        let parent_id = dir_to_cat.get(parent_str).copied().unwrap_or(root_id);
        let cat_id = Uuid::new_v4();

        let row = client
            .query_one(
                &format!(
                    "INSERT INTO categories (id, name, parent_id) VALUES ($1, $2, $3) \
                     RETURNING {CATEGORY_COLUMNS}"
                ),
                &[&cat_id, &cat_name, &parent_id],
            )
            .await?;

        categories.push(Category::from(&row));
        dir_to_cat.insert(dir.clone(), cat_id);
    }

    // ── 7. Upload each file as an asset ──────────────────────────────────────
    let mut assets: Vec<Asset> = Vec::new();
    let mut asset_category_ids: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    let mut errors: Vec<String> = Vec::new();

    for idx in file_indices {
        let path_str = entry_meta[idx].0.clone();

        // Read entry bytes.
        let entry_bytes: Vec<u8> = match archive.by_index(idx) {
            Ok(mut entry) => {
                let mut buf = Vec::with_capacity(entry.size() as usize);
                if let Err(e) = entry.read_to_end(&mut buf) {
                    errors.push(format!("{path_str}: read: {e}"));
                    continue;
                }
                buf
            }
            Err(e) => {
                errors.push(format!("{path_str}: open: {e}"));
                continue;
            }
        };

        let p = std::path::Path::new(path_str.as_str());
        let filename = p.file_name().and_then(|s| s.to_str()).unwrap_or("file");
        // Asset name = filename without extension.
        let asset_name = p.file_stem().and_then(|s| s.to_str()).unwrap_or(filename);
        let asset_type = mime_guess::from_path(filename)
            .first_or_octet_stream()
            .to_string();

        // Resolve the category for this file's parent directory.
        let parent_dir = p.parent().and_then(|p| p.to_str()).unwrap_or("");
        let cat_id = dir_to_cat.get(parent_dir).copied().unwrap_or(root_id);

        // Upload to object storage.
        let asset_id = Uuid::new_v4();
        let storage_key = format!("assets/{}/{}", asset_id, filename);
        let size = entry_bytes.len() as i64;

        if let Err(e) = state
            .storage
            .upload(&storage_key, bytes::Bytes::from(entry_bytes), &asset_type)
            .await
        {
            errors.push(format!("{path_str}: upload: {e}"));
            continue;
        }

        // Insert asset record.
        let row = match client
            .query_one(
                &format!(
                    "INSERT INTO assets \
                     (id, name, asset_type, size, storage_key, bucket, available, is_locked) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                     RETURNING {ASSET_COLUMNS}"
                ),
                &[
                    &asset_id,
                    &asset_name,
                    &asset_type,
                    &size,
                    &storage_key,
                    &state.storage.bucket,
                    &true,  // available
                    &false, // is_locked
                ],
            )
            .await
        {
            Ok(row) => row,
            Err(e) => {
                errors.push(format!("{path_str}: db: {e}"));
                continue;
            }
        };

        // Link asset to its category.
        if let Err(e) = client
            .execute(
                "INSERT INTO asset_categories (asset_id, category_id) VALUES ($1, $2) \
                 ON CONFLICT DO NOTHING",
                &[&asset_id, &cat_id],
            )
            .await
        {
            errors.push(format!("{path_str}: category link: {e}"));
        }

        asset_category_ids.insert(asset_id, vec![cat_id]);
        assets.push(Asset::from(&row));
    }

    Ok((
        StatusCode::CREATED,
        Json(ZipUploadResult {
            categories,
            assets,
            asset_category_ids,
            errors,
        }),
    ))
}
