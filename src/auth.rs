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
struct TeamClaim {
    pub role: String,
}

#[derive(Debug, Deserialize)]
struct Claims {
    pub sub: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub subscriptions: HashMap<String, SubscriptionClaim>,
    /// Active team memberships keyed by team UUID string.
    #[serde(default)]
    pub teams: HashMap<String, TeamClaim>,
}

// ── DAM access level ──────────────────────────────────────────────────────────

/// The level of DAM access granted by the user's active subscription.
///
/// Checked in order:
/// 1. `subscriptions["comad"].tier` (standalone Comad subscription)
/// 2. `subscriptions["clann"].tier` (bundled DAM access via Clann plan)
///
/// Comad mapping:
/// - `individual` → `ImagesOnly` (images + PDFs)
/// - `team` / `enterprise` → `Full`
///
/// Clann fallback mapping:
/// - `family` → `ImagesOnly` (images + PDFs)
/// - `professional` / `enterprise` → `Full`
/// - Anything else → `None`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DamAccess {
    /// No DAM access (no active subscription).
    None,
    /// Images and PDF uploads only (Comad Individual or Clann Family).
    ImagesOnly,
    /// Full unrestricted access (Comad Team/Enterprise or Clann Professional/Enterprise).
    Full,
}

// ── AuthUser extractor ────────────────────────────────────────────────────────

/// Authenticated DAM user extracted from the `Authorization: Bearer` header.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub is_admin: bool,
    pub dam_access: DamAccess,
    /// Maximum number of assets the user may own. `None` = unlimited.
    pub asset_limit: Option<i64>,
    /// Maximum total bytes the user may store. `None` = unlimited.
    pub storage_limit_bytes: Option<i64>,
    /// Maximum number of categories the user may create. `None` = unlimited.
    pub category_limit: Option<i64>,
    /// Active team memberships: team UUID → role (`"owner"` | `"leader"` | `"member"`).
    pub teams: HashMap<String, String>,
}

fn resolve_dam_access(
    roles: &[String],
    subscriptions: &HashMap<String, SubscriptionClaim>,
) -> (DamAccess, Option<i64>, Option<i64>, Option<i64>) {
    if roles.iter().any(|r| r == "admin") {
        return (DamAccess::Full, None, None, None);
    }
    let comad_tier = subscriptions
        .get("comad")
        .filter(|s| s.status == "active" || s.status == "trialing")
        .map(|s| s.tier.as_str());
    let clann_tier = subscriptions
        .get("clann")
        .filter(|s| s.status == "active" || s.status == "trialing")
        .map(|s| s.tier.as_str());
    match comad_tier {
        Some("enterprise") => (DamAccess::Full, None, None, None),
        Some("team") => (
            DamAccess::Full,
            Some(10_000),
            Some(50 * 1024 * 1024 * 1024),
            Some(1_000),
        ),
        Some("individual") => (
            DamAccess::ImagesOnly,
            Some(500),
            Some(1024 * 1024 * 1024),
            Some(50),
        ),
        _ => match clann_tier {
            Some("enterprise") => (DamAccess::Full, None, None, None),
            Some("professional") => (
                DamAccess::Full,
                Some(10_000),
                Some(50 * 1024 * 1024 * 1024),
                Some(1_000),
            ),
            Some("family") => (
                DamAccess::ImagesOnly,
                Some(500),
                Some(1024 * 1024 * 1024),
                Some(200),
            ),
            _ => (DamAccess::None, Some(0), Some(0), Some(0)),
        },
    }
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

        let (dam_access, asset_limit, storage_limit_bytes, category_limit) =
            resolve_dam_access(&claims.roles, &claims.subscriptions);

        let teams = claims
            .teams
            .into_iter()
            .map(|(id, claim)| (id, claim.role))
            .collect();

        Ok(AuthUser {
            user_id: claims.sub,
            is_admin: claims.roles.iter().any(|r| r == "admin"),
            dam_access,
            asset_limit,
            storage_limit_bytes,
            category_limit,
            teams,
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
    /// ImagesOnly users may upload image/* and application/pdf.
    pub fn require_mime_allowed(&self, mime: &str) -> AppResult<()> {
        self.require_access()?;
        let allowed = mime.starts_with("image/") || mime == "application/pdf";
        if self.dam_access == DamAccess::ImagesOnly && !allowed {
            return Err(AppError::Forbidden(
                "Your plan only allows image and PDF uploads. \
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

    /// Returns `Err(Forbidden)` if the user is not a member of the given team.
    pub fn require_team_member(&self, team_id: &str) -> AppResult<()> {
        if !self.teams.contains_key(team_id) {
            return Err(AppError::Forbidden(format!(
                "Not a member of team {team_id}"
            )));
        }
        Ok(())
    }

    /// Returns `Err(Forbidden)` if the user is not an owner or leader of the given team.
    pub fn require_team_admin(&self, team_id: &str) -> AppResult<()> {
        match self.teams.get(team_id).map(|r| r.as_str()) {
            Some("owner" | "leader") => Ok(()),
            Some(_) => Err(AppError::Forbidden(
                "Team owner or leader role required".into(),
            )),
            None => Err(AppError::Forbidden(format!(
                "Not a member of team {team_id}"
            ))),
        }
    }
}

// ── OptionalAuthUser extractor ────────────────────────────────────────────────

/// Like `AuthUser` but never rejects the request. Returns `None` if the
/// `Authorization` header is absent or invalid. Used by endpoints that are
/// public by default but can also serve private resources to their owner.
pub struct OptionalAuthUser(pub Option<AuthUser>);

#[axum::async_trait]
impl<S> FromRequestParts<S> for OptionalAuthUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Ok(OptionalAuthUser(
            AuthUser::from_request_parts(parts, state).await.ok(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(tier: &str, status: &str) -> SubscriptionClaim {
        SubscriptionClaim {
            tier: tier.to_string(),
            status: status.to_string(),
        }
    }

    fn subs(pairs: &[(&str, &str, &str)]) -> HashMap<String, SubscriptionClaim> {
        pairs
            .iter()
            .map(|(app, tier, status)| (app.to_string(), sub(tier, status)))
            .collect()
    }

    fn auth_user(dam_access: DamAccess, asset_limit: Option<i64>, storage_limit_bytes: Option<i64>, category_limit: Option<i64>) -> AuthUser {
        AuthUser {
            user_id: "u1".to_string(),
            is_admin: false,
            dam_access,
            asset_limit,
            storage_limit_bytes,
            category_limit,
            teams: HashMap::new(),
        }
    }

    // ── resolve_dam_access ────────────────────────────────────────────────────

    #[test]
    fn admin_role_gives_full_unlimited_access() {
        let roles = vec!["admin".to_string()];
        let (access, assets, storage, cats) = resolve_dam_access(&roles, &HashMap::new());
        assert_eq!(access, DamAccess::Full);
        assert!(assets.is_none());
        assert!(storage.is_none());
        assert!(cats.is_none());
    }

    #[test]
    fn no_subscription_gives_no_access_with_zero_limits() {
        let (access, assets, storage, cats) = resolve_dam_access(&[], &HashMap::new());
        assert_eq!(access, DamAccess::None);
        assert_eq!(assets, Some(0));
        assert_eq!(storage, Some(0));
        assert_eq!(cats, Some(0));
    }

    #[test]
    fn comad_individual_active_gives_images_only() {
        let (access, assets, storage, cats) =
            resolve_dam_access(&[], &subs(&[("comad", "individual", "active")]));
        assert_eq!(access, DamAccess::ImagesOnly);
        assert_eq!(assets, Some(500));
        assert_eq!(storage, Some(1024 * 1024 * 1024));
        assert_eq!(cats, Some(50));
    }

    #[test]
    fn comad_individual_trialing_also_gives_access() {
        let (access, ..) = resolve_dam_access(&[], &subs(&[("comad", "individual", "trialing")]));
        assert_eq!(access, DamAccess::ImagesOnly);
    }

    #[test]
    fn comad_individual_canceled_gives_no_access() {
        let (access, ..) = resolve_dam_access(&[], &subs(&[("comad", "individual", "canceled")]));
        assert_eq!(access, DamAccess::None);
    }

    #[test]
    fn comad_team_gives_full_with_limits() {
        let (access, assets, storage, cats) =
            resolve_dam_access(&[], &subs(&[("comad", "team", "active")]));
        assert_eq!(access, DamAccess::Full);
        assert_eq!(assets, Some(10_000));
        assert_eq!(storage, Some(50 * 1024 * 1024 * 1024));
        assert_eq!(cats, Some(1_000));
    }

    #[test]
    fn comad_enterprise_gives_full_unlimited() {
        let (access, assets, storage, cats) =
            resolve_dam_access(&[], &subs(&[("comad", "enterprise", "active")]));
        assert_eq!(access, DamAccess::Full);
        assert!(assets.is_none());
        assert!(storage.is_none());
        assert!(cats.is_none());
    }

    #[test]
    fn comad_takes_precedence_over_clann() {
        // comad individual + clann professional → comad wins → ImagesOnly
        let (access, ..) = resolve_dam_access(
            &[],
            &subs(&[
                ("comad", "individual", "active"),
                ("clann", "professional", "active"),
            ]),
        );
        assert_eq!(access, DamAccess::ImagesOnly);
    }

    #[test]
    fn clann_family_fallback_gives_images_only() {
        let (access, assets, storage, cats) =
            resolve_dam_access(&[], &subs(&[("clann", "family", "active")]));
        assert_eq!(access, DamAccess::ImagesOnly);
        assert_eq!(assets, Some(500));
        assert_eq!(storage, Some(1024 * 1024 * 1024));
        assert_eq!(cats, Some(200));
    }

    #[test]
    fn clann_professional_fallback_gives_full_with_limits() {
        let (access, assets, storage, cats) =
            resolve_dam_access(&[], &subs(&[("clann", "professional", "active")]));
        assert_eq!(access, DamAccess::Full);
        assert_eq!(assets, Some(10_000));
        assert_eq!(storage, Some(50 * 1024 * 1024 * 1024));
        assert_eq!(cats, Some(1_000));
    }

    #[test]
    fn clann_enterprise_fallback_gives_full_unlimited() {
        let (access, assets, storage, cats) =
            resolve_dam_access(&[], &subs(&[("clann", "enterprise", "active")]));
        assert_eq!(access, DamAccess::Full);
        assert!(assets.is_none());
        assert!(storage.is_none());
        assert!(cats.is_none());
    }

    // ── AuthUser guard methods ────────────────────────────────────────────────

    #[test]
    fn require_access_blocks_none_tier() {
        let user = auth_user(DamAccess::None, Some(0), Some(0), Some(0));
        assert!(user.require_access().is_err());
    }

    #[test]
    fn require_access_allows_images_only_and_full() {
        assert!(auth_user(DamAccess::ImagesOnly, None, None, None).require_access().is_ok());
        assert!(auth_user(DamAccess::Full, None, None, None).require_access().is_ok());
    }

    #[test]
    fn require_mime_allowed_allows_image_and_pdf_for_images_only() {
        let user = auth_user(DamAccess::ImagesOnly, None, None, None);
        assert!(user.require_mime_allowed("application/pdf").is_ok());
        assert!(user.require_mime_allowed("video/mp4").is_err());
        assert!(user.require_mime_allowed("image/jpeg").is_ok());
        assert!(user.require_mime_allowed("image/png").is_ok());
    }

    #[test]
    fn require_mime_allowed_full_tier_allows_any_mime() {
        let user = auth_user(DamAccess::Full, None, None, None);
        assert!(user.require_mime_allowed("application/pdf").is_ok());
        assert!(user.require_mime_allowed("video/mp4").is_ok());
    }

    #[test]
    fn require_asset_quota_blocks_at_limit() {
        let user = auth_user(DamAccess::Full, Some(500), None, None);
        assert!(user.require_asset_quota(499).is_ok());
        assert!(user.require_asset_quota(500).is_err());
        assert!(user.require_asset_quota(501).is_err());
    }

    #[test]
    fn require_asset_quota_unlimited_always_passes() {
        let user = auth_user(DamAccess::Full, None, None, None);
        assert!(user.require_asset_quota(i64::MAX).is_ok());
    }

    #[test]
    fn require_storage_quota_blocks_when_exceeded() {
        let one_gb = 1024 * 1024 * 1024_i64;
        let user = auth_user(DamAccess::ImagesOnly, None, Some(one_gb), None);
        assert!(user.require_storage_quota(one_gb - 1, 1).is_ok());
        assert!(user.require_storage_quota(one_gb, 1).is_err());
    }

    #[test]
    fn require_category_quota_blocks_at_limit() {
        let user = auth_user(DamAccess::Full, None, None, Some(50));
        assert!(user.require_category_quota(49).is_ok());
        assert!(user.require_category_quota(50).is_err());
    }
}
