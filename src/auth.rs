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
    #[allow(dead_code)]
    pub user_id: String,
    pub dam_access: DamAccess,
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
        let dam_access = if claims.roles.iter().any(|r| r == "admin") {
            DamAccess::Full
        } else {
            // Determine DAM access: check comad subscription first, fall back to clann.
            let comad_access = claims.subscriptions.get("comad")
                .filter(|s| s.status == "active" || s.status == "trialing")
                .map(|s| match s.tier.as_str() {
                    "team" | "enterprise" => DamAccess::Full,
                    "individual" => DamAccess::ImagesOnly,
                    _ => DamAccess::None,
                });

            let clann_access = claims.subscriptions.get("clann")
                .filter(|s| s.status == "active" || s.status == "trialing")
                .map(|s| match s.tier.as_str() {
                    "professional" | "enterprise" => DamAccess::Full,
                    "family" => DamAccess::ImagesOnly,
                    _ => DamAccess::None,
                });

            // Use the highest access level from either subscription.
            match (comad_access, clann_access) {
                (Some(DamAccess::Full), _) | (_, Some(DamAccess::Full)) => DamAccess::Full,
                (Some(DamAccess::ImagesOnly), _) | (_, Some(DamAccess::ImagesOnly)) => DamAccess::ImagesOnly,
                _ => DamAccess::None,
            }
        };

        Ok(AuthUser {
            user_id: claims.sub,
            dam_access,
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
}
