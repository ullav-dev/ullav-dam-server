use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;

use crate::AppState;

/// `POST /auth/login` — Proxy to ullav-user-management.
///
/// The DAM server does not issue JWTs; it forwards the request to the
/// internal auth service and returns its response unchanged. This keeps
/// ullav-user-management off the public internet while allowing mobile
/// and other external clients to authenticate through a single base URL.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let url = format!("{}/auth/login", state.auth_service_url);

    // Forward the scheme seen by the edge proxy so that ullav-user-management's
    // HttpsOnly middleware treats the request as HTTPS even though the internal
    // hop is plain HTTP.
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("https");

    let upstream = match state.auth_client
        .post(&url)
        .header("X-Forwarded-Proto", proto)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Auth service unreachable at {url}: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "Auth service unavailable"})),
            )
                .into_response();
        }
    };

    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    match upstream.json::<Value>().await {
        Ok(json) => (status, Json(json)).into_response(),
        Err(_) => (status, Json(Value::Null)).into_response(),
    }
}
