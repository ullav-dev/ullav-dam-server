use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts},
};
use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::Deserialize;
use std::collections::HashMap;

use crate::{
    error::{AppError, AppResult},
    AppState,
};

// ── Subscription claim (matches ullav-user-management JWT structure) ──────────

#[derive(Debug, Deserialize)]
struct SubscriptionClaim {
    pub tier: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct Claims {
    pub sub: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub subscriptions: HashMap<String, SubscriptionClaim>,
}

// ── DAM access level ──────────────────────────────────────────────────────────

/// The level of DAM access granted by the user's active subscription.
///
/// Checked in order:
/// 1. `subscriptions["comad"].tier` (standalone Comad subscription)
/// 2. `subscriptions["clann"].tier` (bundled DAM access via Clann plan)
///
/// Comad mapping:
/// - `individual` → `ImagesOnly`
/// - `team` / `enterprise` → `Full`
///
/// Clann fallback mapping:
/// - `family` → `ImagesOnly`
/// - `professional` / `enterprise` → `Full`
/// - Anything else → `None`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DamAccess {
    /// No DAM access (no active subscription).
    None,
    /// Image uploads only (Comad Individual or Clann Family).
    ImagesOnly,
    /// Full unrestricted access (Comad Team/Enterprise or Clann Professional/Enterprise).
    Full,
}

// ── AuthUser extractor ────────────────────────────────────────────────────────

/// Authenticated DAM user extracted from the `Authorization: Bearer` header.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub dam_access: DamAccess,
    /// Maximum number of assets the user may own. `None` = unlimited.
    pub asset_limit: Option<i64>,
    /// Maximum total bytes the user may store. `None` = unlimited.
    pub storage_limit_bytes: Option<i64>,
    /// Maximum number of categories the user may create. `None` = unlimited.
    pub category_limit: Option<i64>,
}

#[axum::async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);

        let auth_header = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::Unauthorized("Missing Authorization header".into()))?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or_else(|| AppError::Unauthorized("Authorization must use Bearer scheme".into()))?;

        let claims = decode::<Claims>(
            token,
            &DecodingKey::from_secret(app_state.jwt_secret.as_bytes()),
            &Validation::default(),
        )
        .map(|d| d.claims)
        .map_err(|e| AppError::Unauthorized(format!("Invalid token: {e}")))?;

        // Admins bypass all subscription checks and always get full access.
        let (dam_access, asset_limit, storage_limit_bytes, category_limit) =
            if claims.roles.iter().any(|r| r == "admin") {
                (DamAccess::Full, None, None, None)
            } else {
                // Resolve the effective tier from comad first, then clann fallback.
                let comad_tier = claims.subscriptions.get("comad")
                    .filter(|s| s.status == "active" || s.status == "trialing")
                    .map(|s| s.tier.as_str());

                let clann_tier = claims.subscriptions.get("clann")
                    .filter(|s| s.status == "active" || s.status == "trialing")
                    .map(|s| s.tier.as_str());

                // Limits per tier (comad-native tiers take precedence).
                // category_limit: individual=50, family=200, professional/team=1000, enterprise=unlimited
                let (access, assets, storage, categories) = match comad_tier {
                    Some("enterprise") => (DamAccess::Full, None, None, None),
                    Some("team") => (DamAccess::Full, Some(10_000), Some(50 * 1024 * 1024 * 1024), Some(1_000)),
                    Some("individual") => (DamAccess::ImagesOnly, Some(500), Some(1024 * 1024 * 1024), Some(50)),
                    _ => match clann_tier {
                        Some("enterprise") => (DamAccess::Full, None, None, None),
                        Some("professional") => (DamAccess::Full, Some(10_000), Some(50 * 1024 * 1024 * 1024), Some(1_000)),
                        Some("family") => (DamAccess::ImagesOnly, Some(500), Some(1024 * 1024 * 1024), Some(200)),
                        _ => (DamAccess::None, Some(0), Some(0), Some(0)),
                    },
                };
                (access, assets, storage, categories)
            };

        Ok(AuthUser {
            user_id: claims.sub,
            dam_access,
            asset_limit,
            storage_limit_bytes,
            category_limit,
        })
    }
}

impl AuthUser {
    /// Returns `Err(Forbidden)` if the user has no DAM access at all.
    pub fn require_access(&self) -> AppResult<()> {
        if self.dam_access == DamAccess::None {
            return Err(AppError::Forbidden(
                "DAM access requires a Comad Individual subscription or higher.".into(),
            ));
        }
        Ok(())
    }

    /// Returns `Err(Forbidden)` if the MIME type is not allowed for this plan.
    /// ImagesOnly users may only upload image/* types.
    pub fn require_mime_allowed(&self, mime: &str) -> AppResult<()> {
        self.require_access()?;
        if self.dam_access == DamAccess::ImagesOnly && !mime.starts_with("image/") {
            return Err(AppError::Forbidden(
                "Your plan only allows image uploads. \
                 Upgrade to Comad Team for all file types."
                    .into(),
            ));
        }
        Ok(())
    }

    /// Returns `Err(Forbidden)` if `current_count` is at or above the asset limit.
    pub fn require_asset_quota(&self, current_count: i64) -> AppResult<()> {
        if let Some(limit) = self.asset_limit {
            if current_count >= limit {
                return Err(AppError::Forbidden(format!(
                    "Asset limit of {limit} reached for your plan. \
                     Upgrade to add more assets."
                )));
            }
        }
        Ok(())
    }

    /// Returns `Err(Forbidden)` if `current_count` is at or above the category limit.
    pub fn require_category_quota(&self, current_count: i64) -> AppResult<()> {
        if let Some(limit) = self.category_limit {
            if current_count >= limit {
                return Err(AppError::Forbidden(format!(
                    "Category limit of {limit} reached for your plan. \
                     Upgrade to create more categories."
                )));
            }
        }
        Ok(())
    }

    /// Returns `Err(Forbidden)` if `used_bytes + new_bytes` exceeds the storage limit.
    pub fn require_storage_quota(&self, used_bytes: i64, new_bytes: i64) -> AppResult<()> {
        if let Some(limit) = self.storage_limit_bytes {
            if used_bytes + new_bytes > limit {
                let limit_mb = limit / (1024 * 1024);
                return Err(AppError::Forbidden(format!(
                    "Storage limit of {limit_mb} MB reached for your plan. \
                     Upgrade for more storage."
                )));
            }
        }
        Ok(())
    }
}
